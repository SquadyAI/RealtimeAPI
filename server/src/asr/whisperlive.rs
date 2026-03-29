use async_trait::async_trait;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{RwLock, oneshot};
use tokio::time::{Duration, Instant};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

use crate::asr::backend::AsrBackend;
use crate::asr::types::VoiceText;
use crate::asr::types::voice_text_from_text;

/// 🔧 兜底去重：检测并折叠相邻重复的文本（模型幻觉导致的重复输出）
fn dedupe_adjacent_repeat(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // 按字符计算长度（支持中文等多字节字符）
    let chars: Vec<char> = trimmed.chars().collect();
    let len = chars.len();
    if len < 2 {
        return trimmed.to_string();
    }
    // 尝试从一半位置切分，看前后是否相同
    if len.is_multiple_of(2) {
        let half = len / 2;
        let first: String = chars[..half].iter().collect();
        let second: String = chars[half..].iter().collect();
        if first == second {
            info!("🔧 [WhisperLive] 检测到相邻重复，折叠: '{}' -> '{}'", trimmed, first);
            return first;
        }
    }
    trimmed.to_string()
}
use bytes::Bytes;
use once_cell::sync::Lazy;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::task::JoinHandle;

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Debug)]
struct PendingConnectResult {
    write: SplitSink<Ws, Message>,
    read_task: JoinHandle<()>,
    stop_read_tx: oneshot::Sender<()>,
    url_index: usize,
}

#[derive(Debug, Default)]
struct RecvState {
    server_ready: bool,
    stable_segments: Vec<String>,
    unstable_segment: String,
    last_text: String,
    last_update_at: Option<Instant>,
    disconnect_received: bool,
}

// ISO-639 三字母 -> 两字母映射（及少数例外）
static THREE_TO_TWO: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert("eng", "en");
    m.insert("zho", "zh");
    m.insert("chi", "zh");
    m.insert("deu", "de");
    m.insert("ger", "de");
    m.insert("spa", "es");
    m.insert("rus", "ru");
    m.insert("kor", "ko");
    m.insert("fra", "fr");
    m.insert("fre", "fr");
    m.insert("jpn", "ja");
    m.insert("por", "pt");
    m.insert("tur", "tr");
    m.insert("pol", "pl");
    m.insert("cat", "ca");
    m.insert("nld", "nl");
    m.insert("ara", "ar");
    m.insert("swe", "sv");
    m.insert("ita", "it");
    m.insert("ind", "id");
    m.insert("hin", "hi");
    m.insert("fin", "fi");
    m.insert("vie", "vi");
    m.insert("heb", "he");
    m.insert("ukr", "uk");
    m.insert("ell", "el");
    m.insert("gre", "el");
    m.insert("msa", "ms");
    m.insert("ces", "cs");
    m.insert("cze", "cs");
    m.insert("ron", "ro");
    m.insert("rum", "ro");
    m.insert("dan", "da");
    m.insert("hun", "hu");
    m.insert("tam", "ta");
    m.insert("nor", "no");
    m.insert("tha", "th");
    m.insert("urd", "ur");
    m.insert("hrv", "hr");
    m.insert("bul", "bg");
    m.insert("lit", "lt");
    m.insert("lat", "la");
    m.insert("mri", "mi");
    m.insert("mao", "mi");
    m.insert("mal", "ml");
    m.insert("cym", "cy");
    m.insert("wel", "cy");
    m.insert("slk", "sk");
    m.insert("slo", "sk");
    m.insert("tel", "te");
    m.insert("fas", "fa");
    m.insert("per", "fa");
    m.insert("lav", "lv");
    m.insert("ben", "bn");
    m.insert("srp", "sr");
    m.insert("aze", "az");
    m.insert("slv", "sl");
    m.insert("kan", "kn");
    m.insert("est", "et");
    m.insert("mkd", "mk");
    m.insert("mac", "mk");
    m.insert("bre", "br");
    m.insert("eus", "eu");
    m.insert("baq", "eu");
    m.insert("isl", "is");
    m.insert("ice", "is");
    m.insert("hye", "hy");
    m.insert("arm", "hy");
    m.insert("nep", "ne");
    m.insert("mon", "mn");
    m.insert("bos", "bs");
    m.insert("kaz", "kk");
    m.insert("sqi", "sq");
    m.insert("alb", "sq");
    m.insert("swa", "sw");
    m.insert("glg", "gl");
    m.insert("mar", "mr");
    m.insert("pan", "pa");
    m.insert("pun", "pa");
    m.insert("sin", "si");
    m.insert("khm", "km");
    m.insert("sna", "sn");
    m.insert("yor", "yo");
    m.insert("som", "so");
    m.insert("afr", "af");
    m.insert("oci", "oc");
    m.insert("kat", "ka");
    m.insert("geo", "ka");
    m.insert("bel", "be");
    m.insert("tgk", "tg");
    m.insert("snd", "sd");
    m.insert("guj", "gu");
    m.insert("amh", "am");
    m.insert("yid", "yi");
    m.insert("lao", "lo");
    m.insert("uzb", "uz");
    m.insert("fao", "fo");
    m.insert("hat", "ht");
    m.insert("pus", "ps");
    m.insert("tuk", "tk");
    m.insert("nno", "nn");
    m.insert("mlt", "mt");
    m.insert("san", "sa");
    m.insert("ltz", "lb");
    m.insert("mya", "my");
    m.insert("bur", "my");
    m.insert("bod", "bo");
    m.insert("tib", "bo");
    m.insert("tgl", "tl");
    m.insert("mlg", "mg");
    m.insert("asm", "as");
    m.insert("tat", "tt");
    m.insert("lin", "ln");
    m.insert("hau", "ha");
    m.insert("bak", "ba");
    m.insert("jav", "jv");
    m.insert("sun", "su");
    m.insert("cmn", "zh"); // Mandarin -> zh
    // 允许的三字母直返
    m.insert("yue", "yue"); // 粤语，允许三字母（特例）
    // 历史/兼容
    m.insert("jw", "jv");
    m
});

/// 语言路由配置（从 WHISPERLIVE_ROUTING JSON 解析）
/// 格式示例：
/// ```json
/// {
///   "zh,en,ja,ko,yue": "ws://cjk-server:9090",
///   "es,it,de,fr,pt": "ws://european-server:9090",
///   "*": "ws://default-server:9090"
/// }
/// ```
/// - key: 逗号分隔的语言代码列表（标准化后的 ISO-639-1/3 代码）
/// - value: URL字符串 或 {"url": "...", "hotwords": true}
/// - "*": 默认/兜底后端
static LANGUAGE_ROUTING: Lazy<LanguageRoutingConfig> = Lazy::new(LanguageRoutingConfig::from_env);

/// 后端配置（URL + 能力标记）
#[derive(Debug, Clone)]
pub struct BackendConfig {
    pub url: String,
    pub supports_hotwords: bool,
}

#[derive(Debug, Clone)]
struct LanguageRoutingConfig {
    /// 语言代码 -> 后端配置
    routes: HashMap<String, BackendConfig>,
    /// 默认后端（匹配 "*" 或未配置时使用）
    default_backend: BackendConfig,
}

impl LanguageRoutingConfig {
    fn from_env() -> Self {
        // 尝试从 WHISPERLIVE_ROUTING 环境变量解析 JSON
        if let Ok(json_str) = std::env::var("WHISPERLIVE_ROUTING") {
            if let Ok(parsed) = serde_json::from_str::<HashMap<String, serde_json::Value>>(&json_str) {
                let mut routes = HashMap::new();
                let mut default_backend: Option<BackendConfig> = None;

                for (key, value) in parsed {
                    let backend = Self::parse_backend_value(&value);

                    if key == "*" {
                        default_backend = Some(backend);
                    } else {
                        // 展开语言列表，每个语言代码单独映射
                        for lang in key.split(',') {
                            let lang = lang.trim().to_lowercase();
                            if !lang.is_empty() {
                                routes.insert(lang, backend.clone());
                            }
                        }
                    }
                }

                // 如果没有配置默认后端，使用 WHISPERLIVE_PATH
                let default_backend = default_backend.unwrap_or_else(|| BackendConfig {
                    url: std::env::var("WHISPERLIVE_PATH").unwrap_or_else(|_| "ws://localhost:9090".to_string()),
                    supports_hotwords: false,
                });

                info!("✅ 语言路由配置加载成功: {} 条规则", routes.len());
                info!("   默认: {} (热词: {})", default_backend.url, default_backend.supports_hotwords);
                for (lang, cfg) in &routes {
                    debug!("   {} -> {} (热词: {})", lang, cfg.url, cfg.supports_hotwords);
                }

                return Self { routes, default_backend };
            } else {
                warn!("⚠️ WHISPERLIVE_ROUTING JSON 解析失败，使用兜底配置");
            }
        }

        // 兜底：使用 WHISPERLIVE_PATH
        let default_backend = BackendConfig {
            url: std::env::var("WHISPERLIVE_PATH").unwrap_or_else(|_| "ws://localhost:9090".to_string()),
            supports_hotwords: false,
        };

        info!("ℹ️ 未配置 WHISPERLIVE_ROUTING，所有语言使用: {}", default_backend.url);

        Self { routes: HashMap::new(), default_backend }
    }

    /// 解析后端配置值
    /// 支持两种格式：
    /// - 字符串: "ws://..." (不支持热词)
    /// - 对象: {"url": "ws://...", "hotwords": true}
    fn parse_backend_value(value: &serde_json::Value) -> BackendConfig {
        match value {
            serde_json::Value::String(url) => BackendConfig { url: url.clone(), supports_hotwords: false },
            serde_json::Value::Object(obj) => {
                let url = obj
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("ws://localhost:9090")
                    .to_string();
                let supports_hotwords = obj.get("hotwords").and_then(|v| v.as_bool()).unwrap_or(false);
                BackendConfig { url, supports_hotwords }
            },
            _ => BackendConfig { url: "ws://localhost:9090".to_string(), supports_hotwords: false },
        }
    }

    fn get_backend_for_language(&self, lang_code: &str) -> &BackendConfig {
        // 先尝试精确匹配
        if let Some(cfg) = self.routes.get(lang_code) {
            return cfg;
        }

        // 尝试基础语言码（去掉地区后缀）
        let base_lang = lang_code.split('-').next().unwrap_or(lang_code);
        if base_lang != lang_code {
            if let Some(cfg) = self.routes.get(base_lang) {
                return cfg;
            }
        }

        // 返回默认
        &self.default_backend
    }
}

/// 根据语言代码选择 WhisperLive 后端配置
///
/// 路由规则通过环境变量 WHISPERLIVE_ROUTING (JSON) 配置：
/// ```json
/// {
///   "zh,yue": {"url": "ws://cjk-server:9092", "hotwords": true},
///   "en,ja,ko": "ws://whisper-server:9090",
///   "*": {"url": "ws://default-server:9090", "hotwords": false}
/// }
/// ```
///
/// 值支持两种格式：
/// - 字符串: "ws://..." (不支持热词)
/// - 对象: {"url": "ws://...", "hotwords": true}
pub fn select_backend_by_language(lang_code: &str) -> BackendConfig {
    let normalized = WhisperLiveAsrBackend::normalize_language_code(lang_code);
    let backend = LANGUAGE_ROUTING.get_backend_for_language(&normalized);

    info!(
        "🌐 语言路由: '{}' (标准化='{}') -> {} (热词: {})",
        lang_code, normalized, backend.url, backend.supports_hotwords
    );

    backend.clone()
}

/// 根据语言代码选择 WhisperLive 后端 URL（兼容旧API）
pub fn select_whisperlive_urls_by_language(lang_code: &str) -> String {
    select_backend_by_language(lang_code).url
}

#[derive(Debug)]
pub struct WhisperLiveAsrBackend {
    /// WebSocket URL 列表，支持多地址轮询，如:
    /// "ws://a:9090, ws://b:9090;ws://c:9090"
    ws_urls: Vec<String>,
    /// 当前选择的 URL 索引（下次失败会尝试下一个）
    current_url_index: usize,
    /// 首选语言字符串（传递给 Python 端）
    language: String,
    /// 后端是否支持热词
    supports_hotwords: bool,
    /// 热词列表（仅当后端支持时发送）
    hotwords: Option<Vec<String>>,
    /// 写端（发送）
    ws_write: Option<SplitSink<Ws, Message>>,
    /// 读端后台任务
    read_task: Option<tokio::task::JoinHandle<()>>,
    /// 终止读端任务
    stop_read_tx: Option<oneshot::Sender<()>>,
    /// 读端共享状态
    recv_state: Arc<RwLock<RecvState>>,
    /// 每次会话的唯一标识
    uid: String,
    /// 运行时累积文本（用于调试/兜底）
    last_text: String,
    /// 稳定的已完成片段（来自 server 的 segments.completed 或基于重复阈值策略）
    stable_segments: Vec<String>,
    /// 最近的不稳定片段（尚未完成）
    unstable_segment: String,
    /// 最近一次发给上层的文本（用于抑制重复）
    last_emitted_text: String,
    /// 中断标志：被置位时，收尾等待会立刻退出
    interrupt_flag: AtomicBool,
    /// 本次会话/连接期间累计发送到服务端的样本数（用于避免空段落发送）
    total_samples_sent: usize,

    /// 冷启动连接任务：避免在音频链路里 await connect + SERVER_READY
    connect_task: Option<JoinHandle<Result<PendingConnectResult, String>>>,
    /// 冷启动期间的音频缓存（已编码为 float32 little-endian bytes）
    pending_audio: VecDeque<Vec<u8>>,
    pending_audio_bytes: usize,
    /// 音频合并缓冲：累积多个小帧后一次性编码发送，减少WebSocket消息开销
    send_merge_buffer: Vec<f32>,
}

impl WhisperLiveAsrBackend {
    /// 构建并记录 WhisperLive 初始化消息（统一入口，避免重复代码）
    ///
    /// - `context`: 日志上下文标识，如 "" 或 "(后台)"
    ///
    /// 返回: json_string
    fn build_init_message(uid: &str, language: &str, supports_hotwords: bool, hotwords: &Option<Vec<String>>, context: &str) -> String {
        let mut init_msg = json!({
            "uid": uid,
            "task": "transcribe",
            "use_vad": true,
            "send_last_n_segments": 10,
            "no_speech_thresh": 0.45,
            "clip_audio": false,
            "same_output_threshold": 10,
            "enable_translation": false
        });

        // 只有当明确指定语言且不是 "auto" 时才发送 language 字段
        let lang_for_log: String;
        if !language.is_empty() && language.to_lowercase() != "auto" {
            let lang_norm = Self::normalize_language_code(language);
            if lang_norm != "auto" {
                init_msg["language"] = json!(lang_norm);
                lang_for_log = lang_norm;
            } else {
                lang_for_log = "(自动检测)".to_string();
            }
        } else {
            lang_for_log = "(自动检测)".to_string();
        }

        // 只有当后端支持热词且有热词配置时才发送
        let hotword_count = if supports_hotwords {
            if let Some(hw) = hotwords {
                if !hw.is_empty() {
                    init_msg["hotwords"] = json!(hw);
                    hw.len()
                } else {
                    0
                }
            } else {
                0
            }
        } else {
            0
        };

        let init_msg_str = init_msg.to_string();

        // 统一日志输出
        info!(
            "WhisperLive 初始化{}：uid={}, 语言='{}', 热词={}个",
            context, uid, lang_for_log, hotword_count
        );
        debug!("WhisperLive 发送初始化消息{}: {}", context, init_msg_str);

        init_msg_str
    }

    /// 创建后端（使用 BackendConfig）
    /// - `uid_override`: 若为 `Some(s)` 且非空，则用作 WhisperLive 连接 uid（例如 session_id），便于与业务会话关联
    pub fn new_with_config(config: BackendConfig, language: String, hotwords: Option<Vec<String>>, uid_override: Option<String>) -> Self {
        Self::new_internal(config.url, language, config.supports_hotwords, hotwords, uid_override)
    }

    /// 创建后端（兼容旧API，不支持热词）
    pub fn new(ws_urls_raw: String, language: String) -> Self {
        Self::new_internal(ws_urls_raw, language, false, None, None)
    }

    fn new_internal(ws_urls_raw: String, language: String, supports_hotwords: bool, hotwords: Option<Vec<String>>, uid_override: Option<String>) -> Self {
        let uid = uid_override.filter(|s| !s.is_empty()).unwrap_or_else(|| {
            format!(
                "rust-{}",
                chrono::Utc::now()
                    .timestamp_nanos_opt()
                    .unwrap_or_else(|| chrono::Utc::now().timestamp())
            )
        });
        // 解析地址列表，支持 ',' ';' 及空白分隔
        let mut urls: Vec<String> = ws_urls_raw
            .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if urls.is_empty() {
            urls.push("ws://localhost:9090".to_string());
        }
        // 纯 fallback 模式：始终从第一个 URL 开始尝试
        let start_idx = 0;

        // 🎯 打印后端路由信息
        info!(
            "🎯 WhisperLive 后端初始化: 语言='{}', 服务器={:?}, 热词支持={}, 热词数={}",
            language,
            urls,
            supports_hotwords,
            hotwords.as_ref().map(|h| h.len()).unwrap_or(0)
        );

        Self {
            ws_urls: urls,
            current_url_index: start_idx,
            language,
            supports_hotwords,
            hotwords,
            ws_write: None,
            read_task: None,
            stop_read_tx: None,
            recv_state: Arc::new(RwLock::new(RecvState::default())),
            uid,
            last_text: String::new(),
            stable_segments: Vec::new(),
            unstable_segment: String::new(),
            last_emitted_text: String::new(),
            interrupt_flag: AtomicBool::new(false),
            total_samples_sent: 0,
            connect_task: None,
            pending_audio: VecDeque::new(),
            pending_audio_bytes: 0,
            send_merge_buffer: Vec::with_capacity(1024),
        }
    }

    const MAX_PENDING_AUDIO_BYTES: usize = 2 * 1024 * 1024; // 冷启动缓存上限（KISS）
    const MERGE_TARGET_SAMPLES: usize = 1024; // 合并2个VAD帧(2×512=1024, 64ms@16kHz)再发送

    fn encode_audio_f32_le(audio: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::<u8>::with_capacity(audio.len() * 4);
        for &s in audio {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        bytes
    }

    fn buffer_pending_audio(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        // 防止极端情况下无限增长；超过上限则丢弃最旧的数据并报警
        let mut dropped = 0usize;
        while self.pending_audio_bytes.saturating_add(bytes.len()) > Self::MAX_PENDING_AUDIO_BYTES {
            if let Some(front) = self.pending_audio.pop_front() {
                self.pending_audio_bytes = self.pending_audio_bytes.saturating_sub(front.len());
                dropped += front.len();
            } else {
                break;
            }
        }
        if dropped > 0 {
            warn!(
                "⚠️ WhisperLive pending audio overflow, dropped {} bytes (uid={})",
                dropped, self.uid
            );
        }
        self.pending_audio_bytes = self.pending_audio_bytes.saturating_add(bytes.len());
        self.pending_audio.push_back(bytes);
    }

    fn start_connect_task(&mut self) {
        if self.connect_task.is_some() || self.ws_write.is_some() {
            return;
        }

        let ws_urls = self.ws_urls.clone();
        let current_url_index = self.current_url_index;
        let recv_state = self.recv_state.clone();
        let uid = self.uid.clone();
        let language = self.language.clone();
        let supports_hotwords = self.supports_hotwords;
        let hotwords = self.hotwords.clone();

        self.connect_task = Some(tokio::spawn(async move {
            let len = ws_urls.len();
            for attempt in 0..len {
                let idx = (current_url_index + attempt) % len;
                let url = ws_urls[idx].clone();
                // 最后一个候选给更长超时（fallback 服务器可能延迟高）
                let is_last = attempt == len - 1;
                let timeout_secs = if is_last { 10 } else { 1 };
                info!(
                    "🔌 WhisperLive 连接中(后台): {} (attempt {}/{}, timeout={}s)",
                    url,
                    attempt + 1,
                    len,
                    timeout_secs
                );

                let connect_result = tokio::time::timeout(Duration::from_secs(timeout_secs), connect_async(&url)).await;

                match connect_result {
                    Ok(Ok((ws, _))) => {
                        let (mut write, mut read): (SplitSink<Ws, Message>, SplitStream<Ws>) = ws.split();

                        let recv_state_for_read = recv_state.clone();
                        let uid_for_read = uid.clone();
                        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
                        let read_task = tokio::spawn(async move {
                            loop {
                                tokio::select! {
                                    _ = &mut stop_rx => { break; }
                                    msg = read.next() => {
                                        match msg {
                                            Some(Ok(Message::Text(txt))) => {
                                                if let Ok(v) = serde_json::from_str::<Value>(&txt)
                                                    && v.get("uid").and_then(|u| u.as_str()) == Some(uid_for_read.as_str())
                                                {
                                                    if let Some(lang) = v.get("language").and_then(|x| x.as_str()) {
                                                        let prob = v.get("language_prob").and_then(|p| p.as_f64()).unwrap_or(-1.0);
                                                        info!("WhisperLive 语言检测: {} (prob={:.3})", lang, prob);
                                                    }
                                                    if v.get("message").and_then(|m| m.as_str()) == Some("SERVER_READY") {
                                                        let mut s = recv_state_for_read.write().await;
                                                        s.server_ready = true;
                                                        info!("WhisperLive 收到 SERVER_READY (uid={})", uid_for_read);
                                                        continue;
                                                    }
                                                    if v.get("message").and_then(|m| m.as_str()) == Some("DISCONNECT") {
                                                        let mut s = recv_state_for_read.write().await;
                                                        s.disconnect_received = true;
                                                        continue;
                                                    }
                                                    if let Some(segs) = v.get("segments").and_then(|s| s.as_array()) {
                                                        let mut new_stable: Vec<String> = Vec::new();
                                                        let mut new_unstable: String = String::new();
                                                        for seg in segs {
                                                            if let Some(t) = seg.get("text").and_then(|t| t.as_str()) {
                                                                let t = t.trim();
                                                                if t.is_empty() { continue; }
                                                                let completed = seg.get("completed").and_then(|c| c.as_bool()).unwrap_or(false);
                                                                if completed { new_stable.push(t.to_string()); } else { new_unstable = t.to_string(); }
                                                            }
                                                        }
                                                        let mut s = recv_state_for_read.write().await;
                                                        if new_stable.is_empty() && new_unstable.is_empty() {
                                                            let mut parts: Vec<String> = Vec::new();
                                                            for seg in segs {
                                                                if let Some(t) = seg.get("text").and_then(|t| t.as_str()) {
                                                                    let t = t.trim();
                                                                    if !t.is_empty() { parts.push(t.to_string()); }
                                                                }
                                                            }
                                                            if !parts.is_empty() {
                                                                s.last_text = parts.join(" ").trim().to_string();
                                                                s.last_update_at = Some(Instant::now());
                                                                debug!("WhisperLive 段落更新（退化合并）: '{}'", s.last_text);
                                                            }
                                                        } else {
                                                            s.stable_segments = new_stable;
                                                            s.unstable_segment = new_unstable;
                                                            let mut parts: Vec<String> = s.stable_segments.clone();
                                                            if !s.unstable_segment.is_empty() { parts.push(s.unstable_segment.clone()); }
                                                            s.last_text = parts.join(" ").trim().to_string();
                                                            s.last_update_at = Some(Instant::now());
                                                            debug!("WhisperLive 段落更新: stable={} unstable_len={} last_text='{}'",
                                                                s.stable_segments.len(),
                                                                s.unstable_segment.len(),
                                                                s.last_text
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                            Some(Ok(Message::Close(_))) => {
                                                let mut s = recv_state_for_read.write().await;
                                                s.disconnect_received = true;
                                                break;
                                            }
                                            Some(Ok(_)) => {}
                                            Some(Err(_)) => { break; }
                                            None => {
                                                let mut s = recv_state_for_read.write().await;
                                                s.disconnect_received = true;
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        });

                        // 发送初始配置（使用统一的构建函数）
                        let init_msg_str = WhisperLiveAsrBackend::build_init_message(&uid, &language, supports_hotwords, &hotwords, "(后台)");
                        use tokio_tungstenite::tungstenite::Utf8Bytes;
                        let utf8: Utf8Bytes = Utf8Bytes::from(init_msg_str);
                        if let Err(e) = write.send(Message::Text(utf8)).await {
                            return Err(format!("WhisperLive init send failed: {}", e));
                        }

                        // 等待 SERVER_READY
                        let ready_deadline = Instant::now() + Duration::from_secs(10);
                        loop {
                            if Instant::now() >= ready_deadline {
                                return Err("WhisperLive 未在超时内就绪(后台)".to_string());
                            }
                            {
                                let s = recv_state.read().await;
                                if s.server_ready {
                                    info!("✅ WhisperLive 服务器就绪(后台)");
                                    break;
                                }
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }

                        info!("✅ WhisperLive 已连接(后台): {}", url);
                        return Ok(PendingConnectResult { write, read_task, stop_read_tx: stop_tx, url_index: idx });
                    },
                    Ok(Err(e)) => {
                        warn!("⚠️ WhisperLive 连接失败(后台): {} -> {}", url, e);
                    },
                    Err(_) => {
                        warn!("⚠️ WhisperLive 连接超时(后台): {} ({}s)", url, timeout_secs);
                    },
                }
            }
            Err("WhisperLive 无法连接任何可用地址(后台)".to_string())
        }));
    }

    /// 检查连接状态，如未连接则启动后台连接任务（非阻塞）。
    /// 返回 true 表示已连接可发送，false 表示仍在连接中。
    async fn poll_connected(&mut self) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        if self.ws_write.is_some() {
            return Ok(true);
        }

        if self.connect_task.is_none() {
            self.start_connect_task();
            return Ok(false);
        }

        if let Some(h) = &self.connect_task
            && !h.is_finished()
        {
            return Ok(false);
        }

        // 连接任务已完成：把连接状态迁移回 self
        let h = self.connect_task.take().unwrap();
        let result = h.await.map_err(|e| format!("connect task join error: {}", e))?;
        let connected = result?;
        self.ws_write = Some(connected.write);
        self.read_task = Some(connected.read_task);
        self.stop_read_tx = Some(connected.stop_read_tx);
        self.current_url_index = connected.url_index;
        self.interrupt_flag.store(false, Ordering::Relaxed);
        Ok(true)
    }

    fn normalize_language_code(input: &str) -> String {
        let raw = input.trim();
        debug!("normalize_language_code: input='{}'", input);

        if raw.is_empty() {
            debug!("normalize_language_code: empty input, returning 'auto'");
            return "auto".to_string();
        }
        let mut t = raw.to_lowercase().replace('_', "-");

        // remove trailing/duplicate hyphens
        while t.ends_with('-') {
            t.pop();
        }
        while t.contains("--") {
            t = t.replace("--", "-");
        }
        let compact = t.replace('-', "");

        // Special cases for Cantonese
        if t.starts_with("yue") || compact == "yue" || t.contains("yue") {
            debug!("normalize_language_code: '{}' -> 'yue' (Cantonese)", input);
            return "yue".to_string();
        }
        if t.starts_with("zh-") {
            if t.starts_with("zh-hk") || t.starts_with("zh-mo") {
                debug!("normalize_language_code: '{}' -> 'yue' (zh-HK/zh-MO)", input);
                return "yue".to_string();
            }
            debug!("normalize_language_code: '{}' -> 'zh' (Chinese)", input);
            return "zh".to_string();
        }
        if compact.starts_with("zhhk") || compact.starts_with("zhmo") {
            debug!("normalize_language_code: '{}' -> 'yue' (zhHK/zhMO)", input);
            return "yue".to_string();
        }
        if compact.starts_with("zh") {
            debug!("normalize_language_code: '{}' -> 'zh' (Chinese)", input);
            return "zh".to_string();
        }

        // Generic: try primary subtag (before '-')
        let primary = t.split('-').next().unwrap_or("");
        if primary.len() == 2 && primary.chars().all(|c| c.is_ascii_alphabetic()) {
            debug!(
                "normalize_language_code: '{}' -> '{}' (2-letter primary subtag)",
                input, primary
            );
            return primary.to_string();
        }
        if primary.len() == 3 && primary.chars().all(|c| c.is_ascii_alphabetic()) {
            if let Some(&code) = THREE_TO_TWO.get(primary) {
                debug!("normalize_language_code: '{}' -> '{}' (3-to-2 letter mapping)", input, code);
                return code.to_string();
            } else {
                debug!(
                    "normalize_language_code: '{}' -> 'auto' (3-letter code '{}' not found in mapping)",
                    input, primary
                );
                return "auto".to_string();
            }
        }
        // If no hyphen and whole token looks like code
        if t.len() == 2 && t.chars().all(|c| c.is_ascii_alphabetic()) {
            debug!("normalize_language_code: '{}' -> '{}' (2-letter code)", input, t);
            return t;
        }
        if t.len() == 3 && t.chars().all(|c| c.is_ascii_alphabetic()) {
            if let Some(&code) = THREE_TO_TWO.get(t.as_str()) {
                debug!("normalize_language_code: '{}' -> '{}' (3-to-2 letter mapping)", input, code);
                return code.to_string();
            } else {
                debug!(
                    "normalize_language_code: '{}' -> 'auto' (3-letter code '{}' not found in mapping)",
                    input, t
                );
                return "auto".to_string();
            }
        }
        debug!("normalize_language_code: '{}' -> 'auto' (no match)", input);
        "auto".to_string()
    }

    #[allow(dead_code)]
    async fn ensure_connected(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.ws_write.is_some() {
            return Ok(());
        }
        // 尝试使用当前索引开始的轮询，最多尝试 urls.len() 次
        let len = self.ws_urls.len();
        for attempt in 0..len {
            let idx = (self.current_url_index + attempt) % len;
            let url = self.ws_urls[idx].clone();
            // 最后一个候选给更长超时（fallback 服务器可能延迟高）
            let is_last = attempt == len - 1;
            let timeout_secs = if is_last { 10 } else { 1 };
            info!(
                "🔌 WhisperLive 连接中: {} (attempt {}/{}, timeout={}s)",
                url,
                attempt + 1,
                len,
                timeout_secs
            );
            let connect_result = tokio::time::timeout(Duration::from_secs(timeout_secs), connect_async(&url)).await;
            match connect_result {
                Ok(Ok((ws, _))) => {
                    let (write, mut read): (SplitSink<Ws, Message>, SplitStream<Ws>) = ws.split();
                    self.ws_write = Some(write);
                    self.current_url_index = idx; // 记录成功的索引
                    info!("✅ WhisperLive 已连接: {}", url);
                    // 启动读任务
                    let recv_state = self.recv_state.clone();
                    let uid = self.uid.clone();
                    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
                    self.stop_read_tx = Some(stop_tx);
                    let handle = tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                _ = &mut stop_rx => {
                                    break;
                                }
                                msg = read.next() => {
                                    match msg {
                                        Some(Ok(Message::Text(txt))) => {
                                            if let Ok(v) = serde_json::from_str::<Value>(&txt)
                                                && v.get("uid").and_then(|u| u.as_str()) == Some(uid.as_str()) {
                                                    if let Some(lang) = v.get("language").and_then(|x| x.as_str()) {
                                                        let prob = v.get("language_prob").and_then(|p| p.as_f64()).unwrap_or(-1.0);
                                                        info!("WhisperLive 语言检测: {} (prob={:.3})", lang, prob);
                                                    }
                                                    if v.get("message").and_then(|m| m.as_str()) == Some("SERVER_READY") {
                                                        let mut s = recv_state.write().await;
                                                        s.server_ready = true;
                                                        info!("WhisperLive 收到 SERVER_READY (uid={})", uid);
                                                        continue;
                                                    }
                                                    if v.get("message").and_then(|m| m.as_str()) == Some("DISCONNECT") {
                                                        let mut s = recv_state.write().await;
                                                        s.disconnect_received = true;
                                                        continue;
                                                    }
                                                    if let Some(segs) = v.get("segments").and_then(|s| s.as_array()) {
                                                        let mut new_stable: Vec<String> = Vec::new();
                                                        let mut new_unstable: String = String::new();
                                                        for seg in segs {
                                                            if let Some(t) = seg.get("text").and_then(|t| t.as_str()) {
                                                                let t = t.trim();
                                                                if t.is_empty() { continue; }
                                                                let completed = seg.get("completed").and_then(|c| c.as_bool()).unwrap_or(false);
                                                                if completed { new_stable.push(t.to_string()); } else { new_unstable = t.to_string(); }
                                                            }
                                                        }
                                                        let mut s = recv_state.write().await;
                                                        if new_stable.is_empty() && new_unstable.is_empty() {
                                                            let mut parts: Vec<String> = Vec::new();
                                                            for seg in segs {
                                                                if let Some(t) = seg.get("text").and_then(|t| t.as_str()) {
                                                                    let t = t.trim();
                                                                    if !t.is_empty() { parts.push(t.to_string()); }
                                                                }
                                                            }
                                                            if !parts.is_empty() {
                                                                s.last_text = parts.join(" ").trim().to_string();
                                                                s.last_update_at = Some(Instant::now());
                                                                debug!("WhisperLive 段落更新（退化合并）: '{}'", s.last_text);
                                                            }
                                                        } else {
                                                            s.stable_segments = new_stable;
                                                            s.unstable_segment = new_unstable;
                                                            let mut parts: Vec<String> = s.stable_segments.clone();
                                                            if !s.unstable_segment.is_empty() { parts.push(s.unstable_segment.clone()); }
                                                            s.last_text = parts.join(" ").trim().to_string();
                                                            s.last_update_at = Some(Instant::now());
                                                            debug!("WhisperLive 段落更新: stable={} unstable_len={} last_text='{}'",
                                                                s.stable_segments.len(),
                                                                s.unstable_segment.len(),
                                                                s.last_text
                                                            );
                                                        }
                                                    }
                                                }
                                        }
                                        Some(Ok(Message::Close(_))) => {
                                            let mut s = recv_state.write().await;
                                            s.disconnect_received = true;
                                            break;
                                        }
                                        Some(Ok(_)) => {}
                                        Some(Err(_)) => { break; }
                                        None => {
                                            let mut s = recv_state.write().await;
                                            s.disconnect_received = true;
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    });
                    self.read_task = Some(handle);
                    break;
                },
                Ok(Err(e)) => {
                    warn!("⚠️ WhisperLive 连接失败: {} -> {}", url, e);
                },
                Err(_) => {
                    warn!("⚠️ WhisperLive 连接超时: {} ({}s)", url, timeout_secs);
                },
            }
        }
        if self.ws_write.is_none() {
            return Err("WhisperLive 无法连接任何可用地址".into());
        }

        // 发送初始配置（使用统一的构建函数）
        let init_msg_str = Self::build_init_message(&self.uid, &self.language, self.supports_hotwords, &self.hotwords, "");
        self.send_text(init_msg_str).await?;

        // 等待 SERVER_READY（通过读任务更新的共享状态）
        let ready_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if Instant::now() >= ready_deadline {
                if let Some(tx) = self.stop_read_tx.take() {
                    let _ = tx.send(());
                }
                if let Some(h) = self.read_task.take() {
                    h.abort();
                }
                self.ws_write = None;
                return Err("WhisperLive 未在超时内就绪".into());
            }
            {
                let s = self.recv_state.read().await;
                if s.server_ready {
                    info!("✅ WhisperLive 服务器就绪");
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // 新会话就绪，清除中断标记
        self.interrupt_flag.store(false, Ordering::Relaxed);
        Ok(())
    }

    #[allow(dead_code)]
    async fn send_text(&mut self, text: String) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(ws) = &mut self.ws_write {
            use tokio_tungstenite::tungstenite::Utf8Bytes;
            let utf8: Utf8Bytes = Utf8Bytes::from(text);
            ws.send(Message::Text(utf8)).await?;
            Ok(())
        } else {
            Err("WebSocket 未连接".into())
        }
    }

    async fn send_binary(&mut self, data: Vec<u8>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(ws) = &mut self.ws_write {
            if !data.is_empty() && data != b"END_OF_AUDIO" {
                let samples = data.len() / 4;
                self.total_samples_sent = self.total_samples_sent.saturating_add(samples);
                debug!("WhisperLive 发送音频: samples={}, bytes={}", samples, data.len());
            }
            ws.send(Message::Binary(Bytes::from(data))).await?;
            Ok(())
        } else {
            Err("WebSocket 未连接".into())
        }
    }

    /// 发送二进制数据，失败时返回数据以便重新缓冲（不丢失音频）。
    /// 失败会同时将连接标记为断开，下次调用 poll_connected 时触发重连。
    async fn send_binary_safe(&mut self, data: Vec<u8>) -> Result<(), Vec<u8>> {
        if let Some(ws) = &mut self.ws_write {
            let is_audio = !data.is_empty() && data != b"END_OF_AUDIO";
            let bytes = Bytes::from(data);
            let recovery = bytes.clone(); // O(1) Arc clone
            match ws.send(Message::Binary(bytes)).await {
                Ok(()) => {
                    if is_audio {
                        let samples = recovery.len() / 4;
                        self.total_samples_sent = self.total_samples_sent.saturating_add(samples);
                    }
                    Ok(())
                },
                Err(e) => {
                    warn!("⚠️ WhisperLive WebSocket 发送失败: {}, 数据已重新缓冲 (uid={})", e, self.uid);
                    self.ws_write = None;
                    Err(recovery.to_vec())
                },
            }
        } else {
            Err(data)
        }
    }

    /// 按顺序发送所有 pending 音频 + 当前音频。
    /// 任何一步失败时，未发送的数据（含当前块）全部按序回到 pending_audio，保证不丢不乱。
    async fn flush_pending_and_send(&mut self, current_bytes: Vec<u8>) {
        while let Some(pending) = self.pending_audio.pop_front() {
            self.pending_audio_bytes = self.pending_audio_bytes.saturating_sub(pending.len());
            if let Err(failed) = self.send_binary_safe(pending).await {
                self.pending_audio_bytes = self.pending_audio_bytes.saturating_add(failed.len());
                self.pending_audio.push_front(failed);
                self.buffer_pending_audio(current_bytes);
                return;
            }
        }
        if let Err(failed) = self.send_binary_safe(current_bytes).await {
            self.buffer_pending_audio(failed);
        }
    }

    /// 仅发送 pending 音频队列（不含当前块），失败时保序回退。
    async fn flush_pending_only(&mut self) {
        while let Some(pending) = self.pending_audio.pop_front() {
            self.pending_audio_bytes = self.pending_audio_bytes.saturating_sub(pending.len());
            if let Err(failed) = self.send_binary_safe(pending).await {
                self.pending_audio_bytes = self.pending_audio_bytes.saturating_add(failed.len());
                self.pending_audio.push_front(failed);
                return;
            }
        }
    }

    async fn finalize_and_collect(&mut self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        // 发送 END_OF_AUDIO
        info!("WhisperLive 发送 END_OF_AUDIO (uid={})", self.uid);
        self.send_binary(b"END_OF_AUDIO".to_vec()).await?;

        // 等待直到空闲或收到 DISCONNECT（由读任务更新共享状态）
        let idle_timeout = Duration::from_millis(1500);
        let max_total = Duration::from_secs(8);
        let start = Instant::now();
        let mut last_seen_update: Option<Instant> = None;
        loop {
            // 外部中断：立即退出
            if self.interrupt_flag.load(Ordering::Relaxed) {
                break;
            }
            if start.elapsed() >= max_total {
                break;
            }
            let s = self.recv_state.read().await;
            if s.disconnect_received {
                break;
            }
            if let Some(t) = s.last_update_at {
                last_seen_update = Some(t);
            }
            drop(s);
            if let Some(t) = last_seen_update
                && t.elapsed() >= idle_timeout
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // 关闭写端并终止读任务
        if let Some(w) = &mut self.ws_write {
            let _ = w.close().await;
        }
        self.ws_write = None;
        if let Some(tx) = self.stop_read_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.read_task.take() {
            h.abort();
        }

        let s = self.recv_state.read().await;
        Ok(s.last_text.clone())
    }
}

#[async_trait]
impl AsrBackend for WhisperLiveAsrBackend {
    async fn streaming_recognition(&mut self, audio: &[f32], is_last: bool, _enable_final_inference: bool) -> Result<Option<VoiceText>, Box<dyn std::error::Error + Send + Sync>> {
        info!(
            "🔍 [WhisperLive] streaming_recognition: audio_len={}, is_last={}, sent={}, merge_buf={}, pending={}",
            audio.len(),
            is_last,
            self.total_samples_sent,
            self.send_merge_buffer.len(),
            self.pending_audio.len()
        );

        // Step 1: 将新音频累积到合并缓冲区（减少WebSocket消息数量）
        if !audio.is_empty() {
            self.interrupt_flag.store(false, Ordering::Relaxed);
            self.send_merge_buffer.extend_from_slice(audio);
        }

        // Step 2: 合并缓冲区达到阈值(2帧=64ms)或段末 → 编码并发送
        let should_flush_merge = is_last || self.send_merge_buffer.len() >= Self::MERGE_TARGET_SAMPLES;

        if should_flush_merge && !self.send_merge_buffer.is_empty() {
            let merged = std::mem::take(&mut self.send_merge_buffer);
            let bytes = Self::encode_audio_f32_le(&merged);

            if !self.poll_connected().await? {
                self.buffer_pending_audio(bytes);
            } else {
                self.flush_pending_and_send(bytes).await;
            }
        } else {
            // 合并缓冲区未满：仅推进连接并尽快清空已缓存数据
            if self.poll_connected().await? {
                self.flush_pending_only().await;
            }
        }

        // Step 3: 非最后一帧 → 返回中间识别结果
        if !is_last {
            let current_text = {
                let s = self.recv_state.read().await;
                s.last_text.clone()
            };
            if !current_text.is_empty() && current_text != self.last_emitted_text {
                self.last_emitted_text = current_text.clone();
                return Ok(Some(voice_text_from_text(current_text)));
            } else {
                return Ok(None);
            }
        }

        // Step 4: is_last — 段末收尾
        if self.total_samples_sent == 0 && self.pending_audio.is_empty() && self.send_merge_buffer.is_empty() {
            info!("🔍 [WhisperLive] is_last=true 但无音频数据，返回 None");
            return Ok(None);
        }

        info!(
            "🔍 [WhisperLive] is_last=true，finalize (pending={} chunks)",
            self.pending_audio.len()
        );

        // 保证已连接并把 pending 音频全部发完（允许阻塞等待连接）
        if self.ws_write.is_none() {
            if let Some(h) = self.connect_task.take() {
                info!("🔍 [WhisperLive] 冷启动：等待后台连接任务完成...");
                let connected = h
                    .await
                    .map_err(|e| format!("后台连接任务被取消: {}", e))?
                    .map_err(|e| format!("后台连接任务失败: {}", e))?;
                self.ws_write = Some(connected.write);
                self.read_task = Some(connected.read_task);
                self.stop_read_tx = Some(connected.stop_read_tx);
                self.current_url_index = connected.url_index;
                self.interrupt_flag.store(false, Ordering::Relaxed);
            } else {
                return Err("WhisperLive 无后台连接任务且未连接".into());
            }
        }
        self.flush_pending_only().await;
        info!(
            "🔍 [WhisperLive] pending 已清空，total_samples_sent={}",
            self.total_samples_sent
        );

        let text = self.finalize_and_collect().await.unwrap_or_else(|e| {
            warn!("⚠️ WhisperLive 收尾失败: {}", e);
            self.last_text.clone()
        });
        let text = text.trim().to_string();
        if text.is_empty() {
            return Ok(None);
        }

        let text = dedupe_adjacent_repeat(&text);
        if text.is_empty() {
            return Ok(None);
        }

        info!("🔍 [WhisperLive] is_last=true，最终文本: '{}'", text);
        self.last_emitted_text = text.clone();

        Ok(Some(voice_text_from_text(text)))
    }

    fn reset_streaming(&mut self) {
        info!(
            "🔍 [WhisperLive] reset_streaming 调用: last_emitted_text='{}', total_samples_sent={}",
            self.last_emitted_text, self.total_samples_sent
        );
        // 对于 WhisperLive，我们选择在新段开始时关闭上一次的连接以避免状态污染
        // 无法在同步方法中执行异步关闭；交由 drop/服务端超时回收
        self.ws_write = None;
        if let Some(h) = self.connect_task.take() {
            h.abort();
        }
        if let Some(tx) = self.stop_read_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.read_task.take() {
            h.abort();
        }
        // 置位中断标志，通知任何等待逻辑立即退出
        self.interrupt_flag.store(true, Ordering::Relaxed);
        self.last_text.clear();
        self.last_emitted_text.clear();
        self.stable_segments.clear();
        self.unstable_segment.clear();
        // 🔧 重置样本计数，避免下一轮对话误判
        self.total_samples_sent = 0;
        // 清空冷启动期间的缓存音频
        self.pending_audio.clear();
        self.pending_audio_bytes = 0;
        // 清空合并缓冲区
        self.send_merge_buffer.clear();
        // 清空读端共享状态
        if let Ok(mut s) = self.recv_state.try_write() {
            s.disconnect_received = true; // 促使等待路径尽快退出
            *s = RecvState::default();
        }
        // 刷新 uid 以隔离会话：动态去掉历史追加的 "-r<timestamp>" 后缀，避免越变越长
        let ts = chrono::Utc::now()
            .timestamp_nanos_opt()
            .unwrap_or_else(|| chrono::Utc::now().timestamp());
        let mut uid_base = self.uid.as_str();
        while let Some((prefix, suffix)) = uid_base.rsplit_once("-r") {
            // 仅剥离明显的时间戳后缀，避免误伤业务 id 中的普通 "-rxxx"
            if suffix.len() >= 10 && suffix.chars().all(|c| c.is_ascii_digit()) {
                uid_base = prefix;
            } else {
                break;
            }
        }
        self.uid = if uid_base.starts_with("rust-") {
            format!("rust-{}", ts)
        } else {
            format!("{}-r{}", uid_base, ts)
        };
        info!("🔍 [WhisperLive] reset_streaming 完成，状态已清空");
    }

    fn get_intermediate_accumulated_text(&self) -> Option<String> {
        // 使用读端共享状态
        // 注意：无法在此处 await，保留原接口语义，尽力返回最近一次快照
        if let Ok(s) = self.recv_state.try_read() {
            let result = if s.last_text.is_empty() { None } else { Some(s.last_text.clone()) };
            info!(
                "🔍 [WhisperLive] get_intermediate_accumulated_text: last_text='{}', returning={:?}",
                s.last_text, result
            );
            result
        } else {
            None
        }
    }
}
