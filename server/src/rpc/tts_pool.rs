use crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::SimpleInterruptManager;
use crate::tts::azure::{AzureTtsClient, get_voice_for_language as get_azure_voice};
use crate::tts::baidu::{BaiduHttpTtsClient, BaiduHttpTtsRequest, baidu_payload_override_for_voice_id, baidu_per_for_voice_id, baidu_speed_factor_for_voice_id, pcm_speed_adjust};
use crate::tts::edge::{EdgeTtsClient, get_voice_for_language as get_edge_voice};
use crate::tts::minimax::lang::{LINGUA_MIN_CONFIDENCE, LINGUA_MIN_MARGIN, detect_language_boost, get_voice_for_language};
use crate::tts::minimax::{AudioChunk, AudioSetting, MiniMaxConfig, MiniMaxHttpOptions, MiniMaxHttpTtsClient, VoiceSetting, normalize_minimax_lang};
use crate::tts::volc_engine::{VolcEngineRequest, VolcEngineTtsClient};
use anyhow::{Result, anyhow};
use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, broadcast, oneshot};
use tracing::{debug, error, info, warn};

/// 可用的TTS引擎
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsEngineKind {
    MiniMax,
    VolcEngine,
    Baidu,
    /// Edge TTS: 微软免费 TTS，支持 100+ 语言
    EdgeTts,
    /// Azure TTS: 微软 Azure 认知服务，支持 140+ 语言，600+ 声音
    AzureTts,
}

/// 将语言代码标准化为 Edge TTS 使用的格式
fn normalize_to_edge_lang(lang: &str) -> &'static str {
    let lower = lang.to_lowercase();
    if lower.starts_with("zh") || lower == "chinese" || lower == "mandarin" {
        "zh"
    } else if lower.starts_with("en") || lower == "english" {
        "en"
    } else if lower.starts_with("ja") || lower == "jp" || lower == "japanese" {
        "ja"
    } else if lower.starts_with("ko") || lower == "kr" || lower == "korean" {
        "ko"
    } else if lower.starts_with("fr") || lower == "french" {
        "fr"
    } else if lower.starts_with("de") || lower == "german" {
        "de"
    } else if lower.starts_with("es") || lower == "spanish" {
        "es"
    } else if lower.starts_with("ru") || lower == "russian" {
        "ru"
    } else if lower.starts_with("pt") || lower == "portuguese" {
        "pt"
    } else if lower.starts_with("it") || lower == "italian" {
        "it"
    } else if lower.starts_with("ar") || lower == "arabic" {
        "ar"
    } else if lower.starts_with("hi") || lower == "hindi" {
        "hi"
    } else if lower.starts_with("th") || lower == "thai" {
        "th"
    } else if lower.starts_with("vi") || lower == "vietnamese" {
        "vi"
    } else if lower.starts_with("id") || lower == "indonesian" {
        "id"
    } else {
        // 默认返回中文
        "zh"
    }
}

/// 使用字符集检测语言（基于 Unicode 范围）
/// 返回检测到的语言代码，用于在 from/to 语言对中匹配
fn detect_language_by_charset(text: &str) -> Option<&'static str> {
    // 统计各种字符集的字符数
    let mut cjk_count = 0; // 汉字
    let mut hiragana_count = 0; // 日文平假名
    let mut katakana_count = 0; // 日文片假名
    let mut hangul_count = 0; // 韩文
    let mut arabic_count = 0; // 阿拉伯文
    let mut thai_count = 0; // 泰文
    let mut cyrillic_count = 0; // 西里尔文（俄文等）
    let mut hebrew_count = 0; // 希伯来文
    let mut greek_count = 0; // 希腊文
    let mut latin_count = 0; // 拉丁字母
    let mut devanagari_count = 0; // 天城文（印地语等）
    let mut tamil_count = 0; // 泰米尔文
    let mut bengali_count = 0; // 孟加拉文
    let mut myanmar_count = 0; // 缅甸文
    let mut khmer_count = 0; // 高棉文
    let mut lao_count = 0; // 老挝文

    for c in text.chars() {
        let u = c as u32;
        match u {
            // 汉字
            0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x20000..=0x2A6DF => cjk_count += 1,
            // 日文平假名
            0x3040..=0x309F => hiragana_count += 1,
            // 日文片假名
            0x30A0..=0x30FF => katakana_count += 1,
            // 韩文
            0xAC00..=0xD7AF | 0x1100..=0x11FF => hangul_count += 1,
            // 阿拉伯文
            0x0600..=0x06FF | 0x0750..=0x077F => arabic_count += 1,
            // 泰文
            0x0E00..=0x0E7F => thai_count += 1,
            // 西里尔文
            0x0400..=0x052F => cyrillic_count += 1,
            // 希伯来文
            0x0590..=0x05FF => hebrew_count += 1,
            // 希腊文
            0x0370..=0x03FF => greek_count += 1,
            // 天城文（印地语）
            0x0900..=0x097F => devanagari_count += 1,
            // 泰米尔文
            0x0B80..=0x0BFF => tamil_count += 1,
            // 孟加拉文
            0x0980..=0x09FF => bengali_count += 1,
            // 缅甸文
            0x1000..=0x109F => myanmar_count += 1,
            // 高棉文
            0x1780..=0x17FF => khmer_count += 1,
            // 老挝文
            0x0E80..=0x0EFF => lao_count += 1,
            // 拉丁字母（包括扩展）
            0x0041..=0x005A | 0x0061..=0x007A | 0x00C0..=0x00FF | 0x0100..=0x017F => latin_count += 1,
            _ => {},
        }
    }

    // 日文 = 假名 + 可能混合的汉字
    let japanese_count = hiragana_count + katakana_count;

    // 按字符数量判断主要语言
    let counts = [
        (cjk_count, "zh"),
        (japanese_count, "ja"),
        (hangul_count, "ko"),
        (arabic_count, "ar"),
        (thai_count, "th"),
        (cyrillic_count, "ru"),
        (hebrew_count, "he"),
        (greek_count, "el"),
        (devanagari_count, "hi"),
        (tamil_count, "ta"),
        (bengali_count, "bn"),
        (myanmar_count, "my"),
        (khmer_count, "km"),
        (lao_count, "lo"),
        (latin_count, "en"), // 拉丁字母默认为英语
    ];

    // 找出数量最多的字符集
    let max_count = counts.iter().max_by_key(|(count, _)| *count);

    match max_count {
        Some((count, lang)) if *count > 0 => Some(*lang),
        _ => None,
    }
}

/// 准备好的合成计划，包含执行所需的全部上下文
pub struct SynthesisPlan {
    pub engine: TtsEngineKind,
    pub text: String,
    pub client_id: String,
    pub broadcast_tx: broadcast::Sender<AudioChunk>,
    pub abort_rx: oneshot::Receiver<()>,
    prepared: PreparedEngine,
}

enum PreparedEngine {
    MiniMax {
        client: MiniMaxHttpTtsClient,
        virtual_voice_id: String,
        voice_setting: Option<VoiceSetting>,
        audio_setting: Option<AudioSetting>,
        fallback_language_boost: Option<String>,
        detected_language_boost: Option<String>,
        options: MiniMaxHttpOptions,
    },
    Volc {
        client: VolcEngineTtsClient,
        request: VolcEngineRequest,
    },
    Baidu {
        client: BaiduHttpTtsClient,
        request: BaiduHttpTtsRequest,
        speed_factor: Option<f64>,
    },
    /// Edge TTS: 免费的微软 TTS
    Edge {
        client: EdgeTtsClient,
        voice: String,
        text: String,
    },
    /// Azure TTS: 微软 Azure 认知服务
    Azure {
        client: AzureTtsClient,
        voice: String,
        text: String,
    },
}

/// TTS客户端状态
#[derive(Debug, Clone, PartialEq)]
pub enum TtsClientState {
    Idle,
    InUse,
    Cleaning,
    Failed,
    Closed,
}

/// HTTP 流式 TTS 客户端包装器
pub struct TtsClient {
    minimax_http: Option<MiniMaxHttpTtsClient>,
    volc_client: Option<VolcEngineTtsClient>,
    baidu_client: Option<BaiduHttpTtsClient>,
    edge_client: Option<EdgeTtsClient>,
    azure_client: Option<AzureTtsClient>,
    broadcast_tx: broadcast::Sender<AudioChunk>,
    state: TtsClientState,
    last_used: Instant,
    use_count: u64,
    config: MiniMaxConfig,
    voice_setting: Option<VoiceSetting>,
    client_id: String,
    interrupt_manager: Option<Arc<SimpleInterruptManager>>,
    language: Option<String>,
    /// 语言对 (from_language, to_language)，用于同传场景的 TTS 语言选择
    language_pair: Option<(String, String)>,
    virtual_voice_id: String,
    audio_setting: AudioSetting,
    http_options: MiniMaxHttpOptions,
    active_stream_abort: Option<oneshot::Sender<()>>,
}

impl TtsClient {
    pub fn new(client_id: String, config: MiniMaxConfig, voice_setting: Option<VoiceSetting>) -> Self {
        let (broadcast_tx, _) = broadcast::channel(1000);
        let virtual_voice_id = voice_setting
            .as_ref()
            .and_then(|v| v.voice_id.clone())
            .filter(|vid| !vid.is_empty())
            .or_else(|| config.default_voice_id.clone())
            .unwrap_or_else(|| "zh_female_wanwanxiaohe_moon_bigtts".to_string());

        Self {
            minimax_http: None,
            volc_client: None,
            baidu_client: None,
            edge_client: None,
            azure_client: None,
            broadcast_tx,
            state: TtsClientState::Idle,
            last_used: Instant::now(),
            use_count: 0,
            config,
            voice_setting,
            client_id,
            interrupt_manager: None,
            language: None,
            language_pair: None,
            virtual_voice_id,
            audio_setting: AudioSetting::default(),
            http_options: MiniMaxHttpOptions::default(),
            active_stream_abort: None,
        }
    }

    /// 设置语言（从 start session 的 asr_language 传入）
    pub fn set_language(&mut self, language: Option<String>) {
        self.language = language;
    }

    /// 设置语言对（用于同传场景的 TTS 语言选择）
    pub fn set_language_pair(&mut self, language_pair: Option<(String, String)>) {
        self.language_pair = language_pair;
    }

    /// 设置打断管理器（当前仅存储引用以备后续扩展）
    pub fn set_interrupt_manager(&mut self, interrupt_manager: Arc<SimpleInterruptManager>) {
        self.interrupt_manager = Some(interrupt_manager);
    }

    /// 获取语音设置的可变引用
    pub fn get_voice_setting_mut(&mut self) -> Option<&mut Option<VoiceSetting>> {
        Some(&mut self.voice_setting)
    }

    /// 获取语音设置的只读引用
    pub fn get_voice_setting(&self) -> &Option<VoiceSetting> {
        &self.voice_setting
    }

    /// HTTP 模式无需归还连接即可更新语音，直接返回 false
    pub async fn should_return_client_for_voice_update(&mut self) -> bool {
        info!("🔄 HTTP TTS 客户端语音设置已变更: {}", self.client_id);
        false
    }

    /// 初始化 HTTP 客户端（预热在后台执行，不阻塞初始化）
    pub async fn initialize(&mut self) -> Result<()> {
        info!("🚀 [TtsClient::initialize] 开始初始化 HTTP 引擎, client_id: {}", self.client_id);

        // 判断是否需要 VolcEngine：只有默认音色才可能用到
        // ttv-voice-* 等自定义/克隆音色只用 MiniMax
        const DEFAULT_VOICE_ID: &str = "zh_female_wanwanxiaohe_moon_bigtts";
        let needs_volc = self.virtual_voice_id == DEFAULT_VOICE_ID;

        // 1. 创建 MiniMax 客户端（必须）
        if self.minimax_http.is_none() {
            self.minimax_http = Some(MiniMaxHttpTtsClient::new(self.config.clone()));
            info!("✅ MiniMax HTTP 客户端创建完成: {}", self.client_id);
        }

        // 2. 按需创建 VolcEngine 客户端
        if needs_volc && self.volc_client.is_none() {
            match VolcEngineTtsClient::from_env() {
                Ok(client) => {
                    info!("✅ VolcEngine HTTP 客户端创建完成: {}", self.client_id);
                    self.volc_client = Some(client);
                },
                Err(err) => {
                    warn!(
                        "⚠️ 加载 VolcEngine 配置失败: {}，只有 MiniMax 将可用 (client_id={})",
                        err, self.client_id
                    );
                },
            }
        } else if !needs_volc {
            debug!(
                "🎙️ 使用自定义音色 {}，跳过 VolcEngine 初始化 (client_id={})",
                self.virtual_voice_id, self.client_id
            );
        }

        // 3. 尝试创建 Baidu TTS HTTP 客户端（可选）
        if self.baidu_client.is_none() {
            match BaiduHttpTtsClient::from_env() {
                Ok(client) => {
                    info!("✅ Baidu TTS HTTP 客户端创建完成: {}", self.client_id);
                    self.baidu_client = Some(client);
                },
                Err(err) => {
                    debug!(
                        "⚠️ 加载 Baidu TTS 配置失败: {}，Baidu TTS 将不可用 (client_id={})",
                        err, self.client_id
                    );
                },
            }
        }

        // 4. 创建 Edge TTS 客户端（免费，无需配置）
        if self.edge_client.is_none() {
            self.edge_client = Some(EdgeTtsClient::with_defaults());
            info!("✅ Edge TTS 客户端创建完成: {}", self.client_id);
        }

        // 5. 尝试创建 Azure TTS 客户端（需要 AZURE_SPEECH_KEY 环境变量）
        if self.azure_client.is_none() {
            match AzureTtsClient::from_env() {
                Ok(client) => {
                    info!("✅ Azure TTS 客户端创建完成: {}", self.client_id);
                    self.azure_client = Some(client);
                },
                Err(err) => {
                    debug!(
                        "⚠️ 加载 Azure TTS 配置失败: {}，Azure TTS 将不可用 (client_id={})",
                        err, self.client_id
                    );
                },
            }
        }

        self.state = TtsClientState::Idle;

        // 6. 后台预热连接（不阻塞初始化）
        let minimax_client = self.minimax_http.clone();
        let volc_client = self.volc_client.clone();
        let baidu_client = self.baidu_client.clone();
        let edge_client = self.edge_client.clone();
        let azure_client = self.azure_client.clone();
        let client_id = self.client_id.clone();
        let minimax_prewarm_n = std::env::var("MINIMAX_PREWARM_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1);
        let volc_prewarm_n = std::env::var("VOLC_PREWARM_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1);
        let baidu_prewarm_n = std::env::var("BAIDU_PREWARM_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1);
        let edge_prewarm_n = std::env::var("EDGE_PREWARM_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(2); // Edge TTS 默认预热 2 个连接
        let azure_prewarm_n = std::env::var("AZURE_PREWARM_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1);

        tokio::spawn(async move {
            // MiniMax 预热
            if let Some(ref c) = minimax_client {
                if let Err(e) = c.prewarm_connections(minimax_prewarm_n).await {
                    warn!("⚠️ MiniMax 连接预热失败 (client_id={}): {}", client_id, e);
                } else {
                    info!("🔥 MiniMax 连接已预热: {} (count={})", client_id, minimax_prewarm_n);
                }
            }
            // VolcEngine 预热（如果存在）
            if let Some(ref c) = volc_client {
                if let Err(e) = c.prewarm_connections(volc_prewarm_n).await {
                    warn!("⚠️ VolcEngine 连接预热失败 (client_id={}): {}", client_id, e);
                } else {
                    info!("🔥 VolcEngine 连接已预热: {} (count={})", client_id, volc_prewarm_n);
                }
            }
            // Baidu TTS 预热（如果存在）
            if let Some(ref c) = baidu_client {
                if let Err(e) = c.prewarm_connections(baidu_prewarm_n).await {
                    warn!("⚠️ Baidu TTS 连接预热失败 (client_id={}): {}", client_id, e);
                } else {
                    info!("🔥 Baidu TTS 连接已预热: {} (count={})", client_id, baidu_prewarm_n);
                }
            }
            // Edge TTS 预热
            if let Some(ref c) = edge_client {
                if let Err(e) = c.prewarm(edge_prewarm_n).await {
                    warn!("⚠️ Edge TTS 连接预热失败 (client_id={}): {}", client_id, e);
                } else {
                    info!("🔥 Edge TTS 连接已预热: {} (count={})", client_id, edge_prewarm_n);
                }
            }
            // Azure TTS 预热（如果存在）
            if let Some(ref c) = azure_client {
                if let Err(e) = c.prewarm(azure_prewarm_n).await {
                    warn!("⚠️ Azure TTS 连接预热失败 (client_id={}): {}", client_id, e);
                } else {
                    info!("🔥 Azure TTS 连接已预热: {} (count={})", client_id, azure_prewarm_n);
                }
            }
        });

        info!("✅ TTS客户端初始化完成（预热在后台进行）: {}", self.client_id);
        Ok(())
    }

    /// 准备一次合成任务，返回可用于执行的计划
    /// turn_detected_language: 外部传入的轮次语言缓存（来自 TtsController），检测成功时会更新它
    /// turn_detected_voice_id: 外部传入的轮次音色缓存（来自 TtsController），首次检测到语言时会更新它
    pub fn prepare_synthesis(&mut self, engine: TtsEngineKind, text: &str, turn_detected_language: &mut Option<String>, turn_detected_voice_id: &mut Option<String>) -> Result<SynthesisPlan> {
        if text.trim().is_empty() {
            return Err(anyhow!("TTS 文本不能为空"));
        }

        let prepared = match engine {
            TtsEngineKind::MiniMax => {
                let client = self
                    .minimax_http
                    .clone()
                    .ok_or_else(|| anyhow!("MiniMax HTTP 客户端尚未初始化"))?;

                // Session 级别的 fallback（从 asr_language 派生）
                let session_language_boost = Some(normalize_minimax_lang(self.language.as_deref()));
                // Lingua 检测当前句子的语言
                let detected_language_boost = detect_language_boost(text, LINGUA_MIN_CONFIDENCE, LINGUA_MIN_MARGIN);

                // 检测成功时更新轮次缓存（保持轮次内语言一致性）
                if let Some(ref lang) = detected_language_boost {
                    *turn_detected_language = Some(lang.clone());

                    // 🎙️ 只在音色缓存为空时更新（保持首句音色）
                    // ⚠️ 注意：只有使用默认音色 wanwanxiaohe 时才自动根据语言切换音色
                    let is_default_voice = self.virtual_voice_id == "zh_female_wanwanxiaohe_moon_bigtts";
                    if turn_detected_voice_id.is_none() && is_default_voice {
                        if let Some(voice) = get_voice_for_language(lang) {
                            *turn_detected_voice_id = Some(voice.to_string());
                            info!(
                                "🎙️ 检测到语言并设置音色: lang={}, voice={}, client_id={}, text_preview='{}'",
                                lang,
                                voice,
                                self.client_id,
                                text.chars().take(20).collect::<String>()
                            );
                        } else {
                            // 检测到语言但无映射，锁定fallback以防止后续切换
                            let fallback = self.virtual_voice_id.clone();
                            *turn_detected_voice_id = Some(fallback.clone());
                            info!(
                                "🎙️ 检测到语言但无映射，锁定fallback: lang={}, voice={}, client_id={}, text_preview='{}'",
                                lang,
                                fallback,
                                self.client_id,
                                text.chars().take(20).collect::<String>()
                            );
                        }
                    } else if !is_default_voice {
                        info!(
                            "🎙️ 用户设置了自定义音色，跳过自动音色切换: voice={}, lang={}, client_id={}",
                            self.virtual_voice_id, lang, self.client_id
                        );
                    } else {
                        info!(
                            "🌐 Lingua 检测成功，保持首句音色: lang={}, cached_voice={:?}, client_id={}",
                            lang, turn_detected_voice_id, self.client_id
                        );
                    }
                }

                // 构建最终的 language_boost，优先级：检测结果 > 轮次缓存 > 字符集检测 > session语言
                let final_language_boost = detected_language_boost
                    .clone()
                    .or_else(|| turn_detected_language.clone())
                    .or_else(|| detect_language_by_charset(text).map(String::from))
                    .or_else(|| session_language_boost.clone());

                // 构建最终的 voice_id，优先级：轮次缓存音色 > 客户端配置音色
                let final_voice_id = turn_detected_voice_id.clone().unwrap_or_else(|| self.virtual_voice_id.clone());

                // 详细日志记录回退情况
                if detected_language_boost.is_none() {
                    if let Some(turn_lang) = turn_detected_language.as_ref() {
                        info!(
                            "🌐 Lingua 检测失败，使用轮次缓存: lang={}, voice={:?}, client_id={}, text_preview='{}'",
                            turn_lang,
                            turn_detected_voice_id,
                            self.client_id,
                            text.chars().take(20).collect::<String>()
                        );
                    } else if let Some(ref session_lang) = session_language_boost {
                        if session_lang != "auto" {
                            debug!(
                                "🌐 Lingua 检测失败且无轮次缓存，使用 session language_boost={} (client_id={})",
                                session_lang, self.client_id
                            );
                        }
                    }
                }

                PreparedEngine::MiniMax {
                    client,
                    virtual_voice_id: final_voice_id,
                    voice_setting: self.voice_setting.clone(),
                    audio_setting: Some(self.audio_setting.clone()),
                    fallback_language_boost: final_language_boost.clone(),
                    detected_language_boost: final_language_boost,
                    options: self.http_options.clone(),
                }
            },
            TtsEngineKind::VolcEngine => {
                let client = self
                    .volc_client
                    .clone()
                    .ok_or_else(|| anyhow!("VolcEngine 客户端不可用 (缺少环境配置)"))?;

                // 检测语言并转换为火山引擎格式（cn/en）
                let detected_lang = detect_language_boost(text, LINGUA_MIN_CONFIDENCE, LINGUA_MIN_MARGIN);

                // 将检测结果转换为火山引擎语言代码
                let detected_volc_lang = detected_lang.as_deref().and_then(|lang| {
                    match lang {
                        "English" => Some("en"),
                        "Chinese" => Some("cn"),
                        _ => None, // 其他语言不设置，让火山引擎自动判断
                    }
                });

                // Fallback: 如果检测失败，根据 session 的 asr_language 推断
                let fallback_volc_lang = self.language.as_deref().and_then(|asr_lang| {
                    let lower = asr_lang.to_lowercase();
                    if lower.starts_with("en") {
                        Some("en")
                    } else if lower.starts_with("zh") || lower == "cn" || lower == "auto" {
                        Some("cn")
                    } else {
                        None
                    }
                });

                // 字符集 fallback: 当 Lingua 检测失败时，根据字符集判断
                let charset_volc_lang = if detected_volc_lang.is_none() {
                    detect_language_by_charset(text).and_then(|lang| match lang {
                        "English" => Some("en"),
                        "Chinese" => Some("cn"),
                        _ => None,
                    })
                } else {
                    None
                };

                // 最终语言：优先使用检测结果 > 字符集检测 > session fallback
                let volc_language = detected_volc_lang
                    .or(charset_volc_lang)
                    .or(fallback_volc_lang)
                    .map(|s| s.to_string());

                info!(
                    "🌐 VolcEngine 语言: detected={:?}, charset={:?}, fallback={:?}, final={:?}, text='{}'",
                    detected_volc_lang,
                    charset_volc_lang,
                    fallback_volc_lang,
                    volc_language,
                    text.chars().take(30).collect::<String>()
                );

                let config = client.config().clone();
                let mut request = VolcEngineRequest::from_text(text.to_string());
                request.speaker = config.default_speaker.clone();
                request.model = config.default_model.clone();
                request.namespace = config.default_namespace.clone();
                request.audio_format = Some(config.default_audio_format.clone());
                request.sample_rate = Some(config.default_sample_rate);
                request.emotion = Some("energetic".to_string());
                request.language = volc_language;

                PreparedEngine::Volc { client, request }
            },
            TtsEngineKind::Baidu => {
                let client = self
                    .baidu_client
                    .clone()
                    .ok_or_else(|| anyhow!("Baidu TTS 客户端不可用 (缺少环境配置: BAIDU_TTS_API_KEY / BAIDU_TTS_SECRET_KEY)"))?;

                info!(
                    "🔊 Baidu TTS HTTP: 准备合成, text='{}', client_id={}",
                    text.chars().take(30).collect::<String>(),
                    self.client_id
                );

                let mut request = BaiduHttpTtsRequest::new(text.to_string());
                // 设置发音人
                if let Some(per) = baidu_per_for_voice_id(&self.virtual_voice_id) {
                    request = request.with_per(per);
                }
                // 设置 prosody 参数
                if let Some(payload) = baidu_payload_override_for_voice_id(&self.virtual_voice_id, client.config().build_start_payload()) {
                    if let Some(spd) = payload.spd {
                        request = request.with_spd(spd);
                    }
                    if let Some(pit) = payload.pit {
                        request = request.with_pit(pit);
                    }
                    if let Some(vol) = payload.vol {
                        request = request.with_vol(vol);
                    }
                }

                let speed_factor = baidu_speed_factor_for_voice_id(&self.virtual_voice_id);
                PreparedEngine::Baidu { client, request, speed_factor }
            },
            TtsEngineKind::EdgeTts => {
                let client = self.edge_client.clone().ok_or_else(|| anyhow!("Edge TTS 客户端不可用"))?;

                // 根据文本特征选择语言
                let edge_lang = if let Some((from_lang, to_lang)) = &self.language_pair {
                    // 同传场景：在 from_language 和 to_language 中选择
                    // 使用字符集检测判断文本更接近哪个语言
                    let charset_detected = detect_language_by_charset(text);

                    match charset_detected {
                        Some(detected_code) => {
                            // 检查 from_lang 或 to_lang 是否匹配检测到的语言
                            let from_lower = from_lang.to_lowercase();
                            let to_lower = to_lang.to_lowercase();

                            if from_lower.starts_with(detected_code) {
                                normalize_to_edge_lang(from_lang)
                            } else if to_lower.starts_with(detected_code) {
                                normalize_to_edge_lang(to_lang)
                            } else {
                                // from/to 都不匹配，直接使用检测到的语言
                                detected_code
                            }
                        },
                        None => {
                            // 无法通过字符集判断（如纯数字/标点），默认使用 to_language
                            normalize_to_edge_lang(to_lang)
                        },
                    }
                } else {
                    // 非同传场景：使用 Lingua 检测 > 字符集检测 > session 语言 > 默认中文
                    let detected_lang = detect_language_boost(text, LINGUA_MIN_CONFIDENCE, LINGUA_MIN_MARGIN);
                    let charset_lang = if detected_lang.is_none() { detect_language_by_charset(text) } else { None };

                    let lang_code = detected_lang
                        .as_deref()
                        .or(charset_lang)
                        .or(self.language.as_deref())
                        .unwrap_or("zh");

                    normalize_to_edge_lang(lang_code)
                };

                // 获取对应语言的声音
                let voice = get_edge_voice(edge_lang).unwrap_or("zh-CN-XiaoxiaoNeural").to_string();

                info!(
                    "🔊 Edge TTS: 准备合成, lang={}, voice={}, text='{}', client_id={}, language_pair={:?}",
                    edge_lang,
                    voice,
                    text.chars().take(30).collect::<String>(),
                    self.client_id,
                    self.language_pair
                );

                PreparedEngine::Edge { client, voice, text: text.to_string() }
            },
            TtsEngineKind::AzureTts => {
                let client = self
                    .azure_client
                    .clone()
                    .ok_or_else(|| anyhow!("Azure TTS 客户端不可用 (缺少环境配置: AZURE_SPEECH_KEY)"))?;

                // 根据文本特征选择语言（与 Edge TTS 逻辑类似）
                let azure_lang = if let Some((from_lang, to_lang)) = &self.language_pair {
                    // 同传场景：在 from_language 和 to_language 中选择
                    let charset_detected = detect_language_by_charset(text);

                    match charset_detected {
                        Some(detected_code) => {
                            let from_lower = from_lang.to_lowercase();
                            let to_lower = to_lang.to_lowercase();

                            if from_lower.starts_with(detected_code) {
                                from_lang.as_str()
                            } else if to_lower.starts_with(detected_code) {
                                to_lang.as_str()
                            } else {
                                // 直接使用检测到的语言
                                detected_code
                            }
                        },
                        None => to_lang.as_str(),
                    }
                } else {
                    // 非同传场景：使用 session 语言或检测
                    let charset_lang = detect_language_by_charset(text);
                    charset_lang.or(self.language.as_deref()).unwrap_or("zh-CN")
                };

                // 获取对应语言的 Azure 声音
                let voice = get_azure_voice(azure_lang).unwrap_or("zh-CN-XiaoxiaoNeural").to_string();

                info!(
                    "🔊 Azure TTS: 准备合成, lang={}, voice={}, text='{}', client_id={}, language_pair={:?}",
                    azure_lang,
                    voice,
                    text.chars().take(30).collect::<String>(),
                    self.client_id,
                    self.language_pair
                );

                PreparedEngine::Azure { client, voice, text: text.to_string() }
            },
        };

        if let Some(abort) = self.active_stream_abort.take() {
            let _ = abort.send(());
        }

        let (abort_tx, abort_rx) = oneshot::channel();
        self.active_stream_abort = Some(abort_tx);
        self.state = TtsClientState::InUse;
        self.last_used = Instant::now();
        self.use_count = self.use_count.saturating_add(1);

        Ok(SynthesisPlan {
            engine,
            text: text.to_string(),
            client_id: self.client_id.clone(),
            broadcast_tx: self.broadcast_tx.clone(),
            abort_rx,
            prepared,
        })
    }

    /// 订阅音频输出
    pub fn subscribe_audio(&self) -> Option<broadcast::Receiver<AudioChunk>> {
        Some(self.broadcast_tx.subscribe())
    }

    /// 清理客户端资源
    pub async fn cleanup(&mut self) {
        info!("🧹 开始清理 TTS 客户端: {}", self.client_id);
        self.abort_active_stream();
        self.minimax_http = None;
        self.volc_client = None;
        self.baidu_client = None;
        self.edge_client = None;
        self.azure_client = None;
        self.state = TtsClientState::Closed;
    }

    /// 标记为空闲
    pub fn mark_idle(&mut self) {
        self.state = TtsClientState::Idle;
        self.last_used = Instant::now();
    }

    /// 检查是否可用
    pub fn is_available(&self) -> bool {
        self.state == TtsClientState::Idle
    }

    /// TTS 客户端视为随时可用（只要至少有一个引擎已初始化）
    pub fn is_connected(&self) -> bool {
        self.minimax_http.is_some() || self.volc_client.is_some() || self.baidu_client.is_some() || self.edge_client.is_some() || self.azure_client.is_some()
    }

    /// 主动终止正在进行的流式任务
    pub fn abort_active_stream(&mut self) {
        if let Some(abort) = self.active_stream_abort.take() {
            let _ = abort.send(());
        }
        self.state = TtsClientState::Idle;
    }

    /// 获取客户端ID（用于日志）
    pub fn get_client_id(&self) -> &str {
        &self.client_id
    }
}

impl Drop for TtsClient {
    fn drop(&mut self) {
        self.abort_active_stream();
    }
}

/// 启动实际的流式任务，将音频发送到广播频道
pub fn launch_synthesis(client_arc: Arc<Mutex<TtsClient>>, plan: SynthesisPlan) {
    tokio::spawn(async move {
        let client_id = plan.client_id.clone();
        let engine = plan.engine;
        let broadcast_tx = plan.broadcast_tx.clone();
        let mut abort_rx = plan.abort_rx;

        // 🆕 追踪是否已发送文字空块（用于兜底）
        let mut text_marker_sent = false;

        let run_result: Result<bool> = match plan.prepared {
            PreparedEngine::MiniMax {
                client,
                virtual_voice_id,
                voice_setting,
                audio_setting,
                fallback_language_boost,
                detected_language_boost,
                options,
            } => {
                async {
                    let request_language_boost = detected_language_boost.clone().or(fallback_language_boost.clone());
                    if let Some(ref lang) = detected_language_boost {
                        debug!(
                            "🌐 Lingua 检测语言: {} (client_id={}, text_sample={})",
                            lang,
                            client_id,
                            plan.text.chars().take(16).collect::<String>()
                        );
                    } else if let Some(ref fallback) = fallback_language_boost {
                        debug!(
                            "🌐 Lingua 检测信心不足，使用 fallback language_boost={} (client_id={})",
                            fallback, client_id
                        );
                    } else {
                        debug!("🌐 Lingua 未提供 language_boost，保持默认 (client_id={})", client_id);
                    }

                    let stream = client
                        .synthesize_text(
                            &virtual_voice_id,
                            &plan.text,
                            voice_setting,
                            audio_setting,
                            None,
                            None,
                            request_language_boost,
                            options,
                        )
                        .await
                        .map_err(|err| anyhow!(err))?;

                    tokio::pin!(stream);
                    let mut final_seen = false;
                    let mut first_chunk = true;

                    loop {
                        tokio::select! {
                            _ = &mut abort_rx => {
                                info!("🛑 MiniMax 合成被中止: client_id={}", client_id);
                                return Err(anyhow!("MiniMax synthesis aborted"));
                            }
                            chunk_result = stream.next() => {
                                match chunk_result {
                                    Some(Ok(mut chunk)) => {
                                        // 🆕 在第一个音频帧上携带文本，不再发送单独的文字空块
                                        if first_chunk {
                                            first_chunk = false;
                                            text_marker_sent = true;
                                            chunk.sentence_text = Some(plan.text.clone());
                                            debug!("📨 MiniMax 首帧携带文本: text='{}'", plan.text.chars().take(30).collect::<String>());
                                        }

                                        final_seen |= chunk.is_final;
                                        if let Err(err) = broadcast_tx.send(chunk.clone()) {
                                            debug!("MiniMax 音频广播失败 (无订阅者): {}", err);
                                        }
                                        if chunk.is_final {
                                            break;
                                        }
                                    }
                                    Some(Err(err)) => {
                                        return Err(anyhow!(err));
                                    }
                                    None => break,
                                }
                            }
                        }
                    }

                    Ok(final_seen)
                }
                .await
            },
            PreparedEngine::Volc { client, request } => {
                async {
                    let stream = client.stream_sentence(request)?;
                    tokio::pin!(stream);
                    let mut final_seen = false;
                    let mut first_chunk = true;

                    loop {
                        tokio::select! {
                            _ = &mut abort_rx => {
                                info!("🛑 VolcEngine 合成被中止: client_id={}", client_id);
                                return Err(anyhow!("Volc synthesis aborted"));
                            }
                            chunk_result = stream.next() => {
                                match chunk_result {
                                    Some(Ok(mut chunk)) => {
                                        // 🆕 在第一个音频帧上携带文本，不再发送单独的文字空块
                                        if first_chunk {
                                            first_chunk = false;
                                            text_marker_sent = true;
                                            chunk.sentence_text = Some(plan.text.clone());
                                            debug!("📨 VolcEngine 首帧携带文本: text='{}'", plan.text.chars().take(30).collect::<String>());
                                        }

                                        final_seen |= chunk.is_final;
                                        if let Err(err) = broadcast_tx.send(chunk.clone()) {
                                            debug!("VolcEngine 音频广播失败 (无订阅者): {}", err);
                                        }
                                        if chunk.is_final {
                                            break;
                                        }
                                    }
                                    Some(Err(err)) => {
                                        return Err(anyhow!(err));
                                    }
                                    None => break,
                                }
                            }
                        }
                    }

                    Ok(final_seen)
                }
                .await
            },
            PreparedEngine::Baidu { client, request, speed_factor } => {
                async {
                    let stream = client.synthesize(request)?;
                    tokio::pin!(stream);
                    let mut final_seen = false;
                    let mut first_chunk = true;

                    loop {
                        tokio::select! {
                            _ = &mut abort_rx => {
                                info!("🛑 Baidu TTS 合成被中止: client_id={}", client_id);
                                return Err(anyhow!("Baidu synthesis aborted"));
                            }
                            chunk_result = stream.next() => {
                                match chunk_result {
                                    Some(Ok(mut chunk)) => {
                                        // PCM 无级变速：对非空音频帧应用 speed_factor
                                        if let Some(factor) = speed_factor {
                                            if !chunk.data.is_empty() && !chunk.is_final {
                                                chunk.data = pcm_speed_adjust(&chunk.data, factor);
                                            }
                                        }

                                        // 在第一个音频帧上携带文本
                                        if first_chunk {
                                            first_chunk = false;
                                            text_marker_sent = true;
                                            chunk.sentence_text = Some(plan.text.clone());
                                            debug!("📨 Baidu TTS 首帧携带文本: text='{}'", plan.text.chars().take(30).collect::<String>());
                                        }

                                        final_seen |= chunk.is_final;
                                        if let Err(err) = broadcast_tx.send(chunk.clone()) {
                                            debug!("Baidu TTS 音频广播失败 (无订阅者): {}", err);
                                        }
                                        if chunk.is_final {
                                            break;
                                        }
                                    }
                                    Some(Err(err)) => {
                                        return Err(anyhow!(err));
                                    }
                                    None => break,
                                }
                            }
                        }
                    }

                    Ok(final_seen)
                }
                .await
            },
            PreparedEngine::Edge { client, voice, text } => {
                async {
                    let stream = client
                        .synthesize(&text, Some(&voice))
                        .await
                        .map_err(|e| anyhow!("Edge TTS 合成失败: {}", e))?;

                    tokio::pin!(stream);
                    let mut final_seen = false;
                    let mut first_chunk = true;

                    loop {
                        tokio::select! {
                            _ = &mut abort_rx => {
                                info!("🛑 Edge TTS 合成被中止: client_id={}", client_id);
                                return Err(anyhow!("Edge TTS synthesis aborted"));
                            }
                            chunk_result = stream.next() => {
                                match chunk_result {
                                    Some(Ok(mut chunk)) => {
                                        // 在第一个音频帧上携带文本
                                        if first_chunk {
                                            first_chunk = false;
                                            text_marker_sent = true;
                                            chunk.sentence_text = Some(plan.text.clone());
                                            debug!("📨 Edge TTS 首帧携带文本: text='{}'", plan.text.chars().take(30).collect::<String>());
                                        }

                                        final_seen |= chunk.is_final;
                                        if let Err(err) = broadcast_tx.send(chunk.clone()) {
                                            debug!("Edge TTS 音频广播失败 (无订阅者): {}", err);
                                        }
                                        if chunk.is_final {
                                            break;
                                        }
                                    }
                                    Some(Err(err)) => {
                                        error!("Edge TTS 流错误: {}", err);
                                        return Err(anyhow!("Edge TTS stream error: {}", err));
                                    }
                                    None => break,
                                }
                            }
                        }
                    }

                    Ok(final_seen)
                }
                .await
            },
            PreparedEngine::Azure { client, voice, text } => {
                async {
                    let stream = client
                        .synthesize(&text, Some(&voice))
                        .await
                        .map_err(|e| anyhow!("Azure TTS 合成失败: {}", e))?;

                    tokio::pin!(stream);
                    let mut final_seen = false;
                    let mut first_chunk = true;

                    loop {
                        tokio::select! {
                            _ = &mut abort_rx => {
                                info!("🛑 Azure TTS 合成被中止: client_id={}", client_id);
                                return Err(anyhow!("Azure TTS synthesis aborted"));
                            }
                            chunk_result = stream.next() => {
                                match chunk_result {
                                    Some(Ok(mut chunk)) => {
                                        // 在第一个音频帧上携带文本
                                        if first_chunk {
                                            first_chunk = false;
                                            text_marker_sent = true;
                                            chunk.sentence_text = Some(plan.text.clone());
                                            debug!("📨 Azure TTS 首帧携带文本: text='{}'", plan.text.chars().take(30).collect::<String>());
                                        }

                                        final_seen |= chunk.is_final;
                                        if let Err(err) = broadcast_tx.send(chunk.clone()) {
                                            debug!("Azure TTS 音频广播失败 (无订阅者): {}", err);
                                        }
                                        if chunk.is_final {
                                            break;
                                        }
                                    }
                                    Some(Err(err)) => {
                                        error!("Azure TTS 流错误: {}", err);
                                        return Err(anyhow!("Azure TTS stream error: {}", err));
                                    }
                                    None => break,
                                }
                            }
                        }
                    }

                    Ok(final_seen)
                }
                .await
            },
        };

        // 🆕 兜底保障：如果整个流程结束后仍未发送文字空块，立即补发
        // 覆盖三种场景：1) 请求失败 2) 音频全空 3) 流结束但无音频帧
        if !text_marker_sent {
            warn!(
                "🔧 {:?} 合成结束但未发送文字空块（失败/空音频/无帧），立即补发: client_id={}, text='{}'",
                engine,
                client_id,
                plan.text.chars().take(30).collect::<String>()
            );
            let text_marker = AudioChunk::new_text_marker(plan.text.clone(), 0);
            if let Err(err) = broadcast_tx.send(text_marker) {
                debug!("兜底文字空块广播失败 (无订阅者): {}", err);
            } else {
                info!(
                    "📨 [兜底] 已补发文字空块: text='{}'",
                    plan.text.chars().take(30).collect::<String>()
                );
            }
        }

        let mut need_fallback_final = true;
        if let Ok(seen_final) = &run_result {
            need_fallback_final = !*seen_final;
        }

        if need_fallback_final {
            let fallback = AudioChunk::new(Vec::new(), u64::MAX, true);
            if let Err(err) = broadcast_tx.send(fallback) {
                debug!("发送补偿 final 块失败 ({}): {}", client_id, err);
            }
        }

        if let Err(err) = &run_result {
            error!("❌ {:?} 合成失败 (client_id={}): {}", engine, client_id, err);
        } else {
            info!("✅ {:?} 合成完成 (client_id={})", engine, client_id);
        }

        let mut client = client_arc.lock().await;
        client.active_stream_abort = None;
        match run_result {
            Ok(_) => client.mark_idle(),
            Err(_) => client.state = TtsClientState::Failed,
        }
    });
}
