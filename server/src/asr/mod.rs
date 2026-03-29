//! ASR语音识别模块 (WhisperLive-only 模式)
use std::sync::Arc;
use std::time::SystemTime;
use tracing::{error, info};

use crate::rpc::session_data_integration::{create_audio_metadata_from_pcm_s16le, save_user_audio_globally};
use bytes::Bytes;

use crate::{
    asr::backend::AsrBackend,
    vad::{VADConfig, VADIterator, VadState, engine::VADEngine},
};

pub mod backend;
pub mod punctuation;
pub mod stabilizer;
pub mod types;
pub mod whisperlive;

/// SmartTurn 支持的语言码 (O(1) 查找)
static SMART_TURN_LANGS: std::sync::OnceLock<std::collections::HashSet<&'static str>> = std::sync::OnceLock::new();

fn smart_turn_langs() -> &'static std::collections::HashSet<&'static str> {
    SMART_TURN_LANGS.get_or_init(|| {
        [
            // Arabic
            "ar",
            "ara",
            "arabic",
            // Bengali
            "bn",
            "ben",
            "bengali",
            // Chinese
            "zh",
            "zho",
            "cmn",
            "yue",
            "chinese",
            "cantonese",
            "mandarin",
            // Danish
            "da",
            "dan",
            "danish",
            // Dutch
            "nl",
            "nld",
            "dut",
            "dutch",
            // German
            "de",
            "deu",
            "ger",
            "german",
            // English
            "en",
            "eng",
            "english",
            // Finnish
            "fi",
            "fin",
            "finnish",
            // French
            "fr",
            "fra",
            "fre",
            "french",
            // Hindi
            "hi",
            "hin",
            "hindi",
            // Indonesian
            "id",
            "ind",
            "indonesian",
            // Italian
            "it",
            "ita",
            "italian",
            // Japanese
            "ja",
            "jpn",
            "japanese",
            // Korean
            "ko",
            "kor",
            "korean",
            // Marathi
            "mr",
            "mar",
            "marathi",
            // Norwegian
            "no",
            "nb",
            "nn",
            "nor",
            "nob",
            "nno",
            "norwegian",
            // Polish
            "pl",
            "pol",
            "polish",
            // Portuguese
            "pt",
            "por",
            "portuguese",
            // Russian
            "ru",
            "rus",
            "russian",
            // Spanish
            "es",
            "spa",
            "spanish",
            // Turkish
            "tr",
            "tur",
            "turkish",
            // Ukrainian
            "uk",
            "ukr",
            "ukrainian",
            // Vietnamese
            "vi",
            "vie",
            "vietnamese",
        ]
        .into_iter()
        .collect()
    })
}

/// 根据语言决定是否启用语义VAD
fn should_enable_semantic_vad(lang: Option<&str>) -> bool {
    let code = lang.unwrap_or("auto").to_lowercase();
    if code.is_empty() || code == "auto" {
        return true;
    }
    let base = code.split(['-', '_']).next().unwrap_or("");
    smart_turn_langs().contains(base)
}

// 语言配置直接使用字符串，无需枚举包装

// VAD 推理帧大小 (512 样本, 对应 32ms @ 16kHz)
const VAD_FRAME_SIZE: usize = 512;
// 采样率要求固定 16kHz
const TARGET_SAMPLE_RATE: u32 = 16_000;
// VAD超时时间（毫秒）
const TIMEOUT_DURATION_MS: u64 = 500;

// 🔧 已删除AudioSegmentData，使用新的会话数据持久化系统
/// 语音分段模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechMode {
    /// 内建VAD自动检测（默认）
    Vad,
    /// Push-To-Talk，由外部事件控制开始/结束
    PushToTalk,
    /// VAD模式但延迟到StopInput后再发送到LLM
    /// 设计目的：在云端实现VAD，但交互上符合PTT的"按下-松开后再发送"体验
    /// 原理：服务端仍进行VAD分段与转录事件推送，但延迟触发LLM请求直到StopInput
    VadDeferred,
}

/// 热词配置结构
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HotwordConfig {
    /// 热词文本
    pub text: String,
    /// 热词权重/分数
    pub weight: f64,
    /// 是否启用
    pub enabled: bool,
}

impl Default for HotwordConfig {
    fn default() -> Self {
        Self { text: String::new(), weight: 0.01, enabled: true }
    }
}

/// 热词组配置
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HotwordsConfig {
    /// 全局热词权重
    pub global_weight: f64,
    /// 热词列表
    pub hotwords: Vec<HotwordConfig>,
}

impl Default for HotwordsConfig {
    fn default() -> Self {
        Self {
            // 热词权重说明（基于 CTC log 概率尺度）：
            // - 轻微偏向: 1.0 - 2.0
            // - 中等偏向: 2.0 - 3.0
            // - 强烈偏向: 3.0 - 5.0
            // 注意：0.01 这样的值太小，完全无效！
            global_weight: 2.5, // 中等偏向
            hotwords: vec![
                HotwordConfig { text: "squady".to_string(), weight: 3.0, enabled: true },
                HotwordConfig { text: "hey squady".to_string(), weight: 3.0, enabled: true },
                HotwordConfig { text: "squady".to_string(), weight: 2.5, enabled: true },
                HotwordConfig { text: "asr".to_string(), weight: 2.0, enabled: true },
                // Jackson Wang 热词（覆盖常见大小写变体）
                HotwordConfig { text: "Jackson Wang".to_string(), weight: 3.0, enabled: true },
                // Jackson 单独
                HotwordConfig { text: "Jackson".to_string(), weight: 2.5, enabled: true },
                HotwordConfig { text: "jackson".to_string(), weight: 2.5, enabled: true },
                // Jacky
                HotwordConfig { text: "Jacky".to_string(), weight: 2.5, enabled: true },
                HotwordConfig { text: "jacky".to_string(), weight: 2.5, enabled: true },
                // Team Wang 热词
                HotwordConfig { text: "Team Wang".to_string(), weight: 3.0, enabled: true },
                // WHL
                HotwordConfig { text: "WHL".to_string(), weight: 3.0, enabled: true },
                // Magic AI
                HotwordConfig { text: "Magic AI".to_string(), weight: 3.0, enabled: true },
                // Magic AI Glasses
                HotwordConfig { text: "Magic AI Glasses".to_string(), weight: 3.5, enabled: true },
            ],
        }
    }
}

/// ASR引擎主接口 (WhisperLive-only)
pub struct AsrEngine {
    config: ASRModuleConfig,
    /// VAD引擎，用于创建独立的VAD迭代器（SmartTurn 仍需要）
    vad_engine: Arc<VADEngine>,
}

/// 用户会话处理器 - 每个用户独立的处理实例，共享Session池
pub struct AsrSession {
    /// 会话ID
    session_id: String,
    /// 会话独立后端，实现了 AsrBackend
    backend: Box<dyn AsrBackend>,
    /// 会话的语言标记（来自 asr_language），用于结果标注
    asr_language: Option<String>,
    /// 有状态的VAD迭代器
    vad: VADIterator,
    /// rechunk缓冲区 - 用于将不定长输入分割成512样本块
    rechunk_buffer: Vec<f32>,
    /// 语音段累积缓冲区 - 用于累积整个语音段的音频数据
    /// 当VAD检测到语音段时，我们会收到多个VAD事件，每个事件包含一部分音频数据
    /// 我们需要将这些音频数据累积起来，形成完整的语音段音频数据，然后保存
    speech_audio_buffer: Vec<f32>,
    /// 发送语音片段的sink
    // 🔧 已删除speech_segment_sink，使用新的会话数据持久化系统
    /// 超时监控任务句柄（不再在AsrSession内部控制，由Task层控制生命周期）
    timeout_monitor_handle: Option<tokio::task::JoinHandle<()>>,
    /// 超时事件接收器（不再在AsrSession内部控制，由Task层控制）
    timeout_receiver: Option<tokio::sync::mpsc::UnboundedReceiver<crate::vad::iterator::VadEvent>>,
    /// 语音分段模式
    mode: SpeechMode,
    /// 🔧 Pipeline级别的当前轮次ID引用，用于与Pipeline同步response_id
    current_turn_response_id: Arc<crate::rpc::LockfreeResponseId>,
    /// PTT激活状态
    ptt_active: bool,
    /// VAD配置（用于传递中间处理参数）
    #[allow(dead_code)]
    vad_config: VADConfig,
    /// 🔧 修复：跟踪是否已发送过超时事件，用于控制重新启动监控（废弃：Task层管理）
    timeout_event_sent: bool,
    /// 🆕 是否禁用VAD接收超时监控（废弃：Task层管理）
    timeout_monitor_disabled: bool,

    // === SmartTurn否决暂存机制 ===
    /// 🆕 SmartTurn否决时暂存的ASR结果
    /// 如果在超时前收到新的语音开始事件，会被清空；超时后会被发送给LLM
    pending_asr_result: Option<AsrResult>,
    /// 🆕 SmartTurn否决的时间戳，用于计算超时
    smart_turn_veto_time: Option<std::time::Instant>,
    /// 🆕 SmartTurn否决超时时间（毫秒）
    smart_turn_veto_timeout_ms: u32,
}

impl AsrEngine {
    /// 创建新的ASR引擎实例
    ///
    /// 语言路由配置通过 `WHISPERLIVE_ROUTING` 环境变量 (JSON格式)
    pub async fn new(config: ASRModuleConfig) -> Result<Self, AsrError> {
        info!("🚀 创建ASR引擎 (WhisperLive-only)");
        info!("   → 所有 ASR 请求通过 WhisperLive + 语言路由处理");
        info!("   → 配置: WHISPERLIVE_ROUTING 环境变量 (JSON)");

        // 创建VAD引擎（SmartTurn 仍然可用）
        let vad_config = &config.asr.vad_config;
        let vad_engine = Arc::new(VADEngine::with_pool_size(vad_config.pool.smart_turn_pool_size).map_err(|e| AsrError::RecognitionError(e.to_string()))?);

        info!("✅ ASR引擎初始化完成 (WhisperLive-only, 无需池化)");

        Ok(Self { config, vad_engine })
    }

    /// 创建新的ASR会话，可指定语音分段模式
    pub async fn create_session(&self, session_id: String, speech_mode: SpeechMode, current_turn_response_id: Arc<crate::rpc::LockfreeResponseId>) -> Result<AsrSession, AsrError> {
        self.create_session_with_language(session_id, speech_mode, None, current_turn_response_id)
            .await
    }

    /// 创建新的ASR会话，支持指定语言偏好
    /// 语义VAD 根据语言自动启用
    pub async fn create_session_with_language(
        &self,
        session_id: String,
        speech_mode: SpeechMode,
        language: Option<String>,
        current_turn_response_id: Arc<crate::rpc::LockfreeResponseId>,
    ) -> Result<AsrSession, AsrError> {
        // 确定语言设置（优先用户传入，其次配置，都没有则为空）
        let language_str = language.as_deref().or(self.config.asr.language.as_deref());
        let lang_code = language_str.unwrap_or("").to_string(); // 空字符串表示未指定，让后端自动检测

        // 创建 WhisperLive 后端（基于语言路由）
        let backend = self.create_whisperlive_backend(&lang_code)?;

        // 根据语言决定是否启用语义VAD
        let semantic_vad_enabled = should_enable_semantic_vad(language_str);

        // 创建会话
        AsrSession::new_with_backend(
            session_id,
            backend,
            self.vad_engine.clone(),
            &self.config.asr.vad_config,
            speech_mode,
            if lang_code.is_empty() { None } else { Some(lang_code) },
            semantic_vad_enabled,
            current_turn_response_id,
        )
        .await
    }

    /// 创建新的ASR会话，根据语言自动选择后端（兼容旧API）
    pub async fn create_session_with_auto_model_selection(
        &self,
        session_id: String,
        speech_mode: SpeechMode,
        language: Option<String>,
        current_turn_response_id: Arc<crate::rpc::LockfreeResponseId>,
    ) -> Result<AsrSession, AsrError> {
        // 直接委托给 create_session_with_language，WhisperLive 路由会自动处理
        self.create_session_with_language(session_id, speech_mode, language, current_turn_response_id)
            .await
    }

    /// 创建新的ASR会话，使用指定的模型类型（兼容旧API，忽略 preferred_model_type）
    pub async fn create_session_with_preferred_model(
        &self,
        session_id: String,
        speech_mode: SpeechMode,
        language: Option<String>,
        _preferred_model_type: Option<String>, // 忽略，统一使用 WhisperLive
        current_turn_response_id: Arc<crate::rpc::LockfreeResponseId>,
    ) -> Result<AsrSession, AsrError> {
        // 直接委托给 create_session_with_language
        self.create_session_with_language(session_id, speech_mode, language, current_turn_response_id)
            .await
    }

    /// 创建 WhisperLive 后端（基于语言路由，支持热词）
    fn create_whisperlive_backend(&self, lang_code: &str) -> Result<Box<dyn AsrBackend>, AsrError> {
        let backend_config = crate::asr::whisperlive::select_backend_by_language(lang_code);

        // 从配置获取热词（如果有）
        let hotwords = self
            .config
            .asr
            .hotwords
            .as_ref()
            .map(|hw| {
                hw.hotwords
                    .iter()
                    .filter(|h| h.enabled)
                    .map(|h| h.text.clone())
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty());

        if let Some(ref hw) = hotwords {
            info!(
                "🔌 创建 WhisperLive 后端\n   ├─ 语言: {}\n   ├─ 路由到: {}\n   ├─ 热词支持: {}\n   └─ 热词: {:?}",
                lang_code, backend_config.url, backend_config.supports_hotwords, hw
            );
        } else {
            info!(
                "🔌 创建 WhisperLive 后端\n   ├─ 语言: {}\n   ├─ 路由到: {}\n   └─ 热词支持: {} (无热词)",
                lang_code, backend_config.url, backend_config.supports_hotwords
            );
        }

        Ok(Box::new(crate::asr::whisperlive::WhisperLiveAsrBackend::new_with_config(
            backend_config,
            lang_code.to_string(),
            hotwords,
            None,
        )))
    }

    /// 获取支持的ASR模型列表（仅 WhisperLive）
    pub fn get_supported_models(&self) -> Vec<SupportedAsrModel> {
        self.config.asr.supported_models.clone().unwrap_or_else(|| {
            vec![SupportedAsrModel {
                name: "whisperlive-asr".to_string(),
                model_type: "whisperlive".to_string(),
                path: crate::env_utils::env_string_or_default("WHISPERLIVE_PATH", "ws://localhost:9090"),
                enabled: true,
                description: Some("WhisperLive ASR 服务，多语言支持".to_string()),
                config: None,
            }]
        })
    }

    /// 动态切换ASR模型
    pub async fn switch_model(&mut self, model_name: String) -> Result<(), AsrError> {
        info!("🔄 请求切换ASR模型: {} -> {}", self.config.asr.model_name, model_name);

        // 检查模型是否在支持列表中
        let supported_models = self.get_supported_models();
        let target_model = supported_models
            .iter()
            .find(|m| m.name == model_name && m.enabled)
            .ok_or_else(|| AsrError::ConfigError(format!("不支持的模型: {}", model_name)))?;

        // 如果已经是目标模型，直接返回
        if self.config.asr.model_name == model_name {
            info!("✅ 模型已经是 {}，无需切换", model_name);
            return Ok(());
        }

        // 更新配置
        let old_model = self.config.asr.model_name.clone();
        self.config.asr.model_name = model_name.clone();

        // 根据模型类型更新模型路径
        if target_model.model_type != "whisperlive" {
            // WebSocket模型不需要本地路径
            if let Some(custom_path) = target_model.path.strip_prefix(&self.config.asr.model_path) {
                // 如果是相对路径，更新model_name
                self.config.asr.model_name = custom_path.trim_start_matches('/').to_string();
            }
        }

        info!(
            "✅ ASR模型配置已更新: {} -> {} (类型: {})",
            old_model, model_name, target_model.model_type
        );

        // 注意：这里不重新创建Session池，而是在下次创建会话时使用新配置
        // 这样可以实现热插拔，无需重启整个系统

        Ok(())
    }

    /// 获取当前使用的模型信息
    pub fn get_current_model_info(&self) -> SupportedAsrModel {
        let supported_models = self.get_supported_models();
        supported_models
            .into_iter()
            .find(|m| m.name == self.config.asr.model_name)
            .unwrap_or_else(|| {
                // 如果找不到，返回当前配置信息
                let lname = self.config.asr.model_name.to_lowercase();
                let model_type = if lname.contains("whisperlive") { "whisperlive" } else { "whisper" };

                SupportedAsrModel {
                    name: self.config.asr.model_name.clone(),
                    model_type: model_type.to_string(),
                    path: format!("{}/{}", self.config.asr.model_path, self.config.asr.model_name),
                    enabled: true,
                    description: Some(format!("当前使用的{}模型", model_type)),
                    config: None,
                }
            })
    }

    /// 热重载配置
    pub async fn reload_config(&mut self, new_config: ASRModuleConfig) -> Result<(), AsrError> {
        info!("🔄 热重载ASR配置");

        // 备份旧配置
        let old_config = self.config.clone();

        // 应用新配置
        self.config = new_config;

        // 如果模型发生变化，记录日志
        if old_config.asr.model_name != self.config.asr.model_name {
            info!(
                "🔄 模型配置变更: {} -> {}",
                old_config.asr.model_name, self.config.asr.model_name
            );
        }

        info!("✅ ASR配置热重载完成");
        Ok(())
    }
}

impl AsrSession {
    /// 开启一次PTT语音段
    pub fn begin_speech(&mut self) {
        // 与参考Python实现对齐：新段开始执行完整重置，避免边界回退引起的首音丢失
        self.backend.reset_streaming();
        self.ptt_active = true;
        // 如有需要，这里可以启动10秒超时任务
    }

    /// 结束一次PTT语音段，flush最终结果
    pub async fn end_speech(&mut self, callback: &mut (dyn FnMut(AsrResult) + Send)) -> Result<(), AsrError> {
        match self.mode {
            // PTT：仅在激活状态下 flush
            SpeechMode::PushToTalk => {
                if self.ptt_active {
                    self.ptt_active = false;
                    info!("🔍 [PTT] end_speech: ptt_active=true，调用 streaming_recognition(is_last=true)");

                    if let Some(res) = self.backend.streaming_recognition(&[], true, true).await? {
                        let txt = res.content.trim();
                        info!("🔍 [PTT] end_speech: WhisperLive 返回文本: '{}'", txt);
                        if !txt.is_empty() {
                            let result = AsrResult {
                                text: txt.to_string(),
                                is_partial: false,
                                timestamp: SystemTime::now(),
                                language: self.asr_language.clone(),
                                vad_state: VadState::Silence,
                            };
                            callback(result);
                        }
                    } else {
                        info!("🔍 [PTT] end_speech: WhisperLive 返回 None");
                    }
                } else {
                    info!("🔍 [PTT] end_speech: ptt_active=false，跳过（用户未说话或已处理）");
                }
            },
            SpeechMode::Vad => {
                // VAD模式不会收到end_speech事件，所以不需要处理
            },
            SpeechMode::VadDeferred => {
                // VadDeferred模式与Vad模式在ASR层面行为相同，只是LLM请求时机不同
            },
        }
        Ok(())
    }

    /// 保存用户音频数据
    #[allow(dead_code)]
    async fn save_user_audio(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // 🔧 使用 Pipeline 级别的 response_id，而不是生成自己的
        let response_id = self.current_turn_response_id.load().unwrap_or_else(|| {
            format!(
                "resp_{}",
                chrono::Utc::now()
                    .timestamp_nanos_opt()
                    .unwrap_or_else(|| chrono::Utc::now().timestamp())
            )
        });

        // 检查是否有音频数据
        if self.speech_audio_buffer.is_empty() {
            info!("⚠️ 没有音频数据，跳过用户音频保存");
            return Ok(());
        }

        // 🚀 异步保存，不阻塞调用方
        let session_id = self.session_id.clone();
        let response_id_clone = response_id;
        let speech_audio_buffer = self.speech_audio_buffer.clone();
        tokio::spawn(async move {
            if speech_audio_buffer.is_empty() {
                info!("⚠️ 没有音频数据，跳过用户音频保存");
                return;
            }

            // 将f32音频数据转换为PCM S16LE格式
            let audio_bytes: Vec<u8> = speech_audio_buffer
                .iter()
                .flat_map(|&sample| {
                    let sample_i16 = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
                    sample_i16.to_le_bytes().to_vec()
                })
                .collect();

            // 创建音频元数据
            let audio_metadata = create_audio_metadata_from_pcm_s16le(audio_bytes.len(), 16000, 1);

            if let Err(e) = save_user_audio_globally(&session_id, &response_id_clone, Bytes::from(audio_bytes), audio_metadata).await {
                error!(
                    "❌ 保存用户音频数据失败: session_id={}, response_id={}, error={}",
                    session_id, response_id_clone, e
                );
            } else {
                info!(
                    "✅ 用户音频数据保存成功: session_id={}, response_id={}",
                    session_id, response_id_clone
                );
            }
        });

        Ok(())
    }

    /// 重置会话状态 (VAD超时后的软重置，不停止监控任务)
    pub async fn reset(&mut self) {
        self.rechunk_buffer.clear();
        self.backend.reset_streaming();
        self.vad.reset().await;

        // 🔧 修复：软重置时清理已完成的监控任务句柄，但不主动停止运行中的任务
        if let Some(handle) = &self.timeout_monitor_handle
            && handle.is_finished()
        {
            self.timeout_monitor_handle.take();
            info!("🧹 软重置时清理已完成的VAD监控任务句柄");
        }

        // 🔧 清理超时接收器，下次音频输入时会重新创建
        self.timeout_receiver = None;

        // 🔧 重置超时事件发送标志，允许重新启动监控
        self.timeout_event_sent = false;

        // 🆕 清理SmartTurn否决暂存状态
        self.pending_asr_result = None;
        self.smart_turn_veto_time = None;
    }

    /// 🆕 完全清理会话资源 (会话销毁时的硬清理，停止所有监控)
    pub async fn cleanup(&mut self) {
        // 执行常规重置
        self.rechunk_buffer.clear();
        // 🔧 修复：使用会话级别重置，包括降噪器上下文
        self.backend.session_reset();
        self.vad.reset().await;

        // 🆕 清理SmartTurn否决暂存状态
        self.pending_asr_result = None;
        self.smart_turn_veto_time = None;

        // 🔧 修复：只在会话彻底销毁时才停止VAD超时监控任务
        self.vad.stop_timeout_monitor().await;

        // 停止超时监控任务（保留兼容性）
        if let Some(handle) = self.timeout_monitor_handle.take() {
            handle.abort();
        }
        self.timeout_receiver = None;

        // 🔧 修复：重置超时事件发送标志
        self.timeout_event_sent = false;
    }

    /// 强制结束当前语音识别，处理所有缓冲的音频并返回最终结果。
    /// 这个方法用于处理流中断的超时情况。
    pub async fn finalize(&mut self, callback: &mut (dyn FnMut(AsrResult) + Send)) -> Result<(), AsrError> {
        info!("🔧 开始执行VAD超时finalize: session={}", self.session_id);

        // 🔧 修复：检查是否有SmartTurn否决暂存的ASR结果
        // 如果有，优先发送暂存的结果（用户已经停止说话超过veto_timeout+VAD超时时间）
        if let Some(pending_result) = self.pending_asr_result.take() {
            info!(
                "🔧 VAD超时finalize：发现SmartTurn暂存的ASR结果，发送: '{}' (session: {})",
                pending_result.text, self.session_id
            );
            self.smart_turn_veto_time = None;
            crate::monitoring::record_asr_success();
            callback(pending_result);
            // 🔧 Bug修复：清空speech_audio_buffer，避免音频混入下一段
            self.speech_audio_buffer.clear();
            self.reset().await;
            return Ok(());
        }

        // 1. 获取中间累积文本
        let mut accumulated_text = self.backend.get_intermediate_accumulated_text().unwrap_or_default();

        if let Some(res) = self.backend.streaming_recognition(&[], true, true).await? {
            let txt = res.content.trim();
            if !txt.is_empty() {
                // 优先使用最终完整识别结果，避免与中间结果拼接导致重复
                accumulated_text = txt.to_string();
                info!("🔧 VAD超时finalize：后端返回非空文本: '{}'", accumulated_text);
            } else {
                info!("🔧 VAD超时finalize：后端返回空文本，使用累积文本: '{}'", accumulated_text);
            }
        } else {
            info!("🔧 VAD超时finalize：后端返回None，使用累积文本: '{}'", accumulated_text);
        }
        // 4. 如果有文本就callback
        if !accumulated_text.is_empty() {
            info!("🔧 VAD超时finalize：输出完整文本: '{}'", accumulated_text);
            let result = AsrResult {
                text: accumulated_text,
                is_partial: false,
                timestamp: SystemTime::now(),
                language: None,
                vad_state: VadState::Silence,
            };
            callback(result);
        }
        // 🔧 Bug修复：清空speech_audio_buffer，避免音频混入下一段
        self.speech_audio_buffer.clear();
        self.reset().await;
        Ok(())
    }

    /// 启动VAD超时监控 - 完全事务驱动版本
    pub async fn start_timeout_monitor(&mut self) {
        self.start_timeout_monitor_with(TIMEOUT_DURATION_MS).await;
    }

    /// 启动VAD超时监控，允许自定义超时时长（毫秒）
    pub async fn start_timeout_monitor_with(&mut self, timeout_duration_ms: u64) {
        if self.timeout_monitor_disabled {
            info!("⏭️ 已禁用VAD超时监控: session={}", self.session_id);
            return;
        }
        // 🔧 修复：检查是否有旧的监控任务，如果有但已完成则清理
        if let Some(handle) = &self.timeout_monitor_handle {
            if handle.is_finished() {
                // 旧任务已完成，清理句柄
                self.timeout_monitor_handle.take();
            } else {
                // 旧任务还在运行，先停止它
                handle.abort();
                self.timeout_monitor_handle.take();
            }
        }

        // 🔧 修复：重置超时事件发送标志，允许重新启动监控
        self.timeout_event_sent = false;

        // 设置VAD监控状态并获取接收器（使用自定义时长）
        let timeout_receiver = self.vad.start_timeout_monitor(timeout_duration_ms).await;
        self.timeout_receiver = Some(timeout_receiver);

        // 🔧 修复：创建新的监控任务（每次音频输入时重新创建）
        let timeout_state = self.vad.get_timeout_state();
        let (cancel_tx, cancel_rx) = tokio::sync::mpsc::unbounded_channel();

        // 保存取消发送器到VAD状态
        self.vad.set_cancel_sender(cancel_tx).await;

        let handle = tokio::spawn(async move {
            crate::vad::iterator::run_timeout_monitor(timeout_state, timeout_duration_ms, cancel_rx).await;
        });
        self.timeout_monitor_handle = Some(handle);
    }

    /// 移交超时事件接收器的所有权
    pub fn take_timeout_receiver(&mut self) -> Option<tokio::sync::mpsc::UnboundedReceiver<crate::vad::iterator::VadEvent>> {
        self.timeout_receiver.take()
    }

    /// 🆕 检查是否有超时接收器可用
    pub fn has_timeout_receiver(&self) -> bool {
        self.timeout_receiver.is_some()
    }

    /// 🆕 获取超时接收器的引用（不消费）
    pub fn get_timeout_receiver_mut(&mut self) -> Option<&mut tokio::sync::mpsc::UnboundedReceiver<crate::vad::iterator::VadEvent>> {
        self.timeout_receiver.as_mut()
    }

    /// 🆕 禁用VAD接收超时监控（运行时），用于VAD延迟到StopInput再发LLM的模式
    pub fn disable_timeout_monitor(&mut self) {
        self.timeout_monitor_disabled = true;
        if let Some(handle) = self.timeout_monitor_handle.take() {
            handle.abort();
        }
        self.timeout_receiver = None;
        self.timeout_event_sent = false;
        info!("🛑 已禁用VAD接收超时监控: session={}", self.session_id);
    }

    /// 🆕 停止当前VAD超时监控实例（一次性退出监控线程，等待下次音频重新拉起）
    pub async fn stop_timeout_monitor(&mut self) {
        // 停止VAD内部监控状态并发送取消信号
        self.vad.stop_timeout_monitor().await;

        // 终止当前监控任务
        if let Some(handle) = self.timeout_monitor_handle.take() {
            handle.abort();
            info!("🛑 已终止VAD超时监控任务: session={}", self.session_id);
        }

        // 清理接收器与标志位
        self.timeout_receiver = None;
        self.timeout_event_sent = false;
    }

    /// 🆕 运行时更新 VAD 阈值/时长参数（增量应用）
    pub fn update_vad_params(&mut self, threshold: Option<f32>, min_silence_ms: Option<u32>, min_speech_ms: Option<u32>) {
        if let Some(t) = threshold {
            self.vad.set_threshold(t);
            info!("🔄 VAD threshold 更新: {}", t);
        }
        if let Some(ms) = min_silence_ms {
            self.vad.set_min_silence_duration_ms(ms);
            info!("🔄 VAD min_silence_duration_ms 更新: {}ms", ms);
        }
        if let Some(ms) = min_speech_ms {
            self.vad.set_min_speech_duration_ms(ms);
            info!("🔄 VAD min_speech_duration_ms 更新: {}ms", ms);
        }
    }

    /// 获取语音状态
    pub fn get_speech_state(&self) -> SpeechState {
        SpeechState {
            buffer_samples: self.rechunk_buffer.len(),
            buffer_duration_ms: (self.rechunk_buffer.len() as f32 / TARGET_SAMPLE_RATE as f32 * 1000.0) as u64,
            accumulated_text_length: 0, // 移除累积文本长度
        }
    }

    /// 使用已构建的后端创建新的 ASR 会话（通用）
    ///
    /// # Arguments
    /// * `semantic_vad_enabled` - 是否启用语义VAD（由管线侧根据语言决定）
    #[allow(clippy::too_many_arguments)]
    async fn new_with_backend(
        session_id: String,
        backend: Box<dyn AsrBackend>,
        vad_engine: Arc<VADEngine>,
        vad_config: &VADConfig,
        speech_mode: SpeechMode,
        // 🔧 已删除speech_segment_sink，使用新的会话数据持久化系统
        asr_language: Option<String>,
        semantic_vad_enabled: bool,
        // 🔧 Pipeline级别的当前轮次ID引用，用于与Pipeline同步response_id
        current_turn_response_id: Arc<crate::rpc::LockfreeResponseId>,
    ) -> Result<Self, AsrError> {
        // 🔧 VAD配置诊断信息
        info!("🔧 VAD配置 (session: {}):", session_id);
        info!("   threshold: {}", vad_config.pool.threshold);
        info!("   min_silence_duration_ms: {}", vad_config.pool.min_silence_duration_ms);
        info!("   min_speech_duration_ms: {}", vad_config.pool.min_speech_duration_ms);
        info!("   speech_pad_samples: {}", vad_config.pool.speech_pad_samples);
        info!(
            "   enable_final_inference: {} (是否启用完整推理)",
            vad_config.pool.enable_final_inference
        );
        info!(
            "   semantic_vad: {} (threshold: {})",
            if semantic_vad_enabled { "enabled" } else { "disabled" },
            vad_config.pool.semantic_threshold
        );

        // 创建 VAD 实例（语义VAD 由管线侧决定）
        let vad = vad_engine
            .create_vad_iterator_with_semantic(
                vad_config.pool.threshold,
                vad_config.pool.min_silence_duration_ms,
                vad_config.pool.min_speech_duration_ms,
                vad_config.pool.speech_pad_samples,
                semantic_vad_enabled,
                vad_config.pool.semantic_threshold,
            )
            .map_err(|e| AsrError::RecognitionError(e.to_string()))?;

        let session = Self {
            session_id,
            backend,
            asr_language,
            vad,
            rechunk_buffer: Vec::with_capacity(VAD_FRAME_SIZE * 10),
            speech_audio_buffer: Vec::new(),
            timeout_monitor_handle: None,
            timeout_receiver: None,
            mode: speech_mode,
            current_turn_response_id,
            ptt_active: false,
            vad_config: vad_config.clone(),
            timeout_event_sent: false,
            timeout_monitor_disabled: false,
            // SmartTurn否决暂存机制
            pending_asr_result: None,
            smart_turn_veto_time: None,
            smart_turn_veto_timeout_ms: vad_config.pool.smart_turn_veto_timeout_ms,
        };

        // 🔧 修复：不在会话创建时启动VAD超时监控，而是在第一个音频分片时启动
        // timeout_receiver初始为None，在首次音频输入时创建

        Ok(session)
    }

    /// 处理单个音频块 - 调用者负责在适当的线程上下文中调用此方法
    pub async fn process_audio_chunk(&mut self, chunk: Vec<f32>, callback: &mut (dyn FnMut(AsrResult) + Send)) -> Result<(), AsrError> {
        use ndarray::ArrayView1;

        // let buffer_len_before = self.rechunk_buffer.len();
        self.rechunk_buffer.extend_from_slice(&chunk);
        // info!(
        //     "🎧 process_audio_chunk: session_id={}, mode={:?}, chunk_samples={}, buffer_before={}, buffer_after={}",
        //     self.session_id,
        //     self.mode,
        //     chunk.len(),
        //     buffer_len_before,
        //     self.rechunk_buffer.len()
        // );

        match self.mode {
            SpeechMode::Vad => {
                // 🆕 SmartTurn否决超时检查：在处理新音频前，检查是否有暂存的ASR结果已超时
                if let (Some(pending_result), Some(veto_time)) = (self.pending_asr_result.take(), self.smart_turn_veto_time.take()) {
                    let elapsed_ms = veto_time.elapsed().as_millis() as u32;
                    if elapsed_ms >= self.smart_turn_veto_timeout_ms {
                        // 超时：发送暂存的ASR结果
                        info!(
                            "⏰ SmartTurn否决超时 ({}ms >= {}ms)，发送暂存的ASR结果: '{}' (session: {})",
                            elapsed_ms, self.smart_turn_veto_timeout_ms, pending_result.text, self.session_id
                        );
                        crate::monitoring::record_asr_success();
                        callback(pending_result);

                        // 清理状态
                        self.vad.stop_timeout_monitor().await;
                        if let Some(handle) = self.timeout_monitor_handle.take() {
                            handle.abort();
                        }
                        self.timeout_receiver = None;
                        self.timeout_event_sent = false;

                        // 🔧 修复：reset()会清空rechunk_buffer，但当前音频块已经添加进去了
                        // 保存当前缓冲区内容，reset后恢复，避免丢失音频（可能是新语音的开始）
                        let saved_buffer = std::mem::take(&mut self.rechunk_buffer);
                        self.reset().await;
                        self.rechunk_buffer = saved_buffer;

                        // 🔧 Bug修复：清空speech_audio_buffer，避免被否决的语音段音频混入下一段
                        self.speech_audio_buffer.clear();
                    } else {
                        // 未超时：放回暂存（后续会继续检查或被新语音开始事件清空）
                        self.pending_asr_result = Some(pending_result);
                        self.smart_turn_veto_time = Some(veto_time);
                    }
                }

                // 由上层Task控制是否/何时启动VAD超时监控（此处不自动开启）
                while self.rechunk_buffer.len() >= VAD_FRAME_SIZE {
                    let vad_chunk: Vec<f32> = self.rechunk_buffer.drain(0..VAD_FRAME_SIZE).collect();

                    if let Some(vad_event) = self.vad.process_chunk(&ArrayView1::from(&vad_chunk)).await? {
                        // 🔧 已删除原有音频段发送逻辑，使用新的会话数据持久化系统
                        // 🔧 新增：累积用户音频数据
                        self.speech_audio_buffer.extend_from_slice(&vad_event.audio);

                        // 语音开始时发送开始事件，不进行识别
                        if vad_event.is_first {
                            info!(
                                "🎤 VAD检测到语音开始 (音频长度: {}ms)",
                                (vad_event.audio.len() as f32 / TARGET_SAMPLE_RATE as f32 * 1000.0) as u32
                            );

                            // 🆕 用户又开始说话了，清空暂存的ASR结果（SmartTurn判断正确，用户只是停顿）
                            if self.pending_asr_result.is_some() {
                                info!("🔄 用户继续说话，丢弃暂存的ASR结果 (session: {})", self.session_id);
                                self.pending_asr_result = None;
                                self.smart_turn_veto_time = None;
                            }

                            // 与参考Python实现对齐：在段首执行完整重置，清空前端/解码器状态
                            // 避免软重置导致的 processed_frame_idx 回退不当，从而引发首音丢失
                            self.backend.reset_streaming();

                            if !self.has_timeout_receiver() {
                                self.start_timeout_monitor().await;
                            }

                            // 🔧 发送语音开始事件
                            let start_result = AsrResult {
                                text: String::new(), // 空文本，表示刚开始说话
                                is_partial: true,
                                timestamp: SystemTime::now(),
                                language: None,
                                vad_state: VadState::Speaking,
                            };
                            callback(start_result);
                        }

                        // 进行特征累积（开始和中间都是is_last=false，只有结束时is_last=true）
                        if !vad_event.is_last {
                            // 段内只累积，不做输出
                            let _ = self.backend.streaming_recognition(&vad_event.audio, false, false).await?;
                            continue;
                        } else {
                            // 🆕 SmartTurn否决处理：先记录时间，再进行ASR推理
                            // 这样超时计时从SmartTurn否决时开始，而不是ASR完成后
                            let veto_start_time = if vad_event.smart_turn_vetoed { Some(std::time::Instant::now()) } else { None };

                            // 仅在段落结束时进行一次完整推理；段内只做特征累积
                            let result = self.backend.streaming_recognition(&vad_event.audio, true, true).await?;

                            // 🆕 SmartTurn否决处理：暂存结果或直接发送（如果ASR期间已超时）
                            if let Some(veto_time) = veto_start_time {
                                // SmartTurn认为用户只是停顿
                                if let Some(voice_text) = result {
                                    let txt = voice_text.content.trim();
                                    if !txt.is_empty() {
                                        let pending = AsrResult {
                                            text: txt.to_string(),
                                            is_partial: false,
                                            timestamp: SystemTime::now(),
                                            language: self.asr_language.clone(),
                                            vad_state: VadState::Silence,
                                        };

                                        // 🆕 检查ASR推理期间是否已经超时
                                        let elapsed_ms = veto_time.elapsed().as_millis() as u32;
                                        if elapsed_ms >= self.smart_turn_veto_timeout_ms {
                                            // ASR期间已超时，直接发送结果
                                            info!(
                                                "⏰ SmartTurn否决+ASR期间已超时 ({}ms >= {}ms)，直接发送: '{}' (session: {})",
                                                elapsed_ms, self.smart_turn_veto_timeout_ms, txt, self.session_id
                                            );
                                            crate::monitoring::record_asr_success();
                                            callback(pending);

                                            // 清理状态
                                            self.vad.stop_timeout_monitor().await;
                                            if let Some(handle) = self.timeout_monitor_handle.take() {
                                                handle.abort();
                                            }
                                            self.timeout_receiver = None;
                                            self.timeout_event_sent = false;

                                            // 🔧 修复：reset()会清空rechunk_buffer，保存并恢复以避免丢失音频
                                            let saved_buffer = std::mem::take(&mut self.rechunk_buffer);
                                            self.reset().await;
                                            self.rechunk_buffer = saved_buffer;

                                            // 🔧 Bug修复：清空speech_audio_buffer，避免被否决的语音段音频混入下一段
                                            self.speech_audio_buffer.clear();
                                        } else {
                                            // 未超时，暂存等待
                                            info!(
                                                "🔄 SmartTurn否决，暂存ASR结果: '{}' (session: {}, elapsed={}ms, timeout={}ms)",
                                                txt, self.session_id, elapsed_ms, self.smart_turn_veto_timeout_ms
                                            );
                                            self.pending_asr_result = Some(pending);
                                            self.smart_turn_veto_time = Some(veto_time);
                                        }
                                    } else {
                                        info!("🔄 SmartTurn否决但ASR结果为空，不暂存 (session: {})", self.session_id);
                                    }
                                }
                                // SmartTurn否决时不清理状态，等待后续事件
                                continue;
                            }

                            // 🔧 关键修复：记录是否发送了有效文本
                            let has_sent_text = if let Some(voice_text) = result {
                                // 🔧 新增：在语音段结束时保存用户音频数据
                                info!("💾 保存用户音频数据，长度: {} 样本", self.speech_audio_buffer.len());

                                // 🔧 新增：异步保存用户音频数据，避免阻塞音频处理流程
                                let session_id = self.session_id.clone();
                                let speech_audio_buffer = self.speech_audio_buffer.clone();
                                // 🔧 在 spawn 之前获取 Pipeline 级别的 response_id
                                let response_id = self.current_turn_response_id.load().unwrap_or_else(|| {
                                    format!(
                                        "resp_{}",
                                        chrono::Utc::now()
                                            .timestamp_nanos_opt()
                                            .unwrap_or_else(|| chrono::Utc::now().timestamp())
                                    )
                                });

                                // 在单独的任务中保存音频数据
                                tokio::spawn(async move {
                                    // 检查是否有音频数据
                                    if speech_audio_buffer.is_empty() {
                                        info!("⚠️ 没有音频数据，跳过用户音频保存");
                                        return;
                                    }

                                    // 将f32音频数据转换为PCM S16LE格式
                                    let audio_bytes: Vec<u8> = speech_audio_buffer
                                        .iter()
                                        .flat_map(|&sample| {
                                            // 将f32 [-1.0, 1.0] 转换为 i16 [-32768, 32767]
                                            let sample_i16 = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
                                            sample_i16.to_le_bytes().to_vec()
                                        })
                                        .collect();

                                    // 创建音频元数据
                                    let audio_metadata = create_audio_metadata_from_pcm_s16le(
                                        audio_bytes.len(),
                                        16000, // 采样率
                                        1,     // 声道数
                                    );

                                    // 调用全局保存函数
                                    if let Err(e) = save_user_audio_globally(&session_id, &response_id, Bytes::from(audio_bytes), audio_metadata).await {
                                        error!(
                                            "❌ 保存用户音频数据失败: session_id={}, response_id={}, error={}",
                                            session_id, response_id, e
                                        );
                                    } else {
                                        info!(
                                            "✅ 用户音频数据保存成功: session_id={}, response_id={}",
                                            session_id, response_id
                                        );
                                    }
                                });

                                // 清空音频缓冲区，为下一个语音段做准备
                                self.speech_audio_buffer.clear();

                                let txt = voice_text.content.trim();
                                info!("✅ VAD语音段正常结束，最终识别结果: '{}' (session: {})", txt, self.session_id);

                                if !txt.is_empty() {
                                    let result = AsrResult {
                                        text: txt.to_string(),
                                        is_partial: false, // 最终结果
                                        timestamp: SystemTime::now(),
                                        language: self.asr_language.clone(),
                                        vad_state: VadState::Silence,
                                    };

                                    // 🔧 添加ASR成功监控指标
                                    crate::monitoring::record_asr_success();

                                    callback(result);
                                    true // 标记已发送文本
                                } else {
                                    false // 文本为空
                                }
                            } else {
                                false // ASR返回None
                            };

                            // 🔧 关键修复：无论是否有文本，都必须发送VAD Silence事件
                            // 这确保上层Task能正确更新VAD状态并触发speech_stopped事件
                            // 修复西班牙语/意大利语等语言一直录音的bug
                            if !has_sent_text {
                                info!(
                                    "⚠️ VAD语音段结束但无有效文本，发送空Silence事件确保VAD状态更新 (session: {})",
                                    self.session_id
                                );
                                let silence_event = AsrResult {
                                    text: String::new(),
                                    is_partial: false,
                                    timestamp: SystemTime::now(),
                                    language: self.asr_language.clone(),
                                    vad_state: VadState::Silence,
                                };
                                callback(silence_event);
                            }

                            // 🔧 关键修复：VAD语音段正常结束时，精确停止当前超时监控，避免重复finalize
                            // 只停止当前监控实例，为下次VAD周期做准备，不影响多轮对话

                            // 1. 停止VAD内部的超时监控状态（停止当前超时检测）
                            self.vad.stop_timeout_monitor().await;

                            // 2. 终止当前监控任务（避免延迟的超时事件）
                            if let Some(handle) = self.timeout_monitor_handle.take() {
                                handle.abort();
                                info!("🛑 VAD语音段正常结束，终止当前超时监控实例");
                            }

                            // 3. 清理当前超时接收器（避免处理过期事件）
                            self.timeout_receiver = None;

                            // 4. 重置超时事件标志（允许下次音频输入重新启动监控）
                            self.timeout_event_sent = false;

                            // 🔧 VAD模式：语音段结束后主动软重置ASR会话，避免后续VAD超时路径再次finalize产生重复文本
                            // 使用软重置保持多轮对话能力，清空流式累积状态但保留会话基础状态
                            self.reset().await;
                            info!("🔄 VAD语音段结束后已主动软重置ASR会话，为下次VAD周期做准备");
                        }
                    }
                }
            },
            SpeechMode::PushToTalk => {
                while self.rechunk_buffer.len() >= VAD_FRAME_SIZE {
                    let frame = self.rechunk_buffer.drain(0..VAD_FRAME_SIZE).collect::<Vec<_>>();

                    if self.ptt_active {
                        // 🔧 添加调试日志：确认音频数据正在处理
                        if let Some(voice_text) = self.backend.streaming_recognition(&frame, false, true).await? {
                            let txt = voice_text.content.trim();
                            if !txt.is_empty() {
                                let result = AsrResult {
                                    text: txt.to_string(),
                                    is_partial: true,
                                    timestamp: SystemTime::now(),
                                    language: self.asr_language.clone(),
                                    vad_state: VadState::Speaking,
                                };
                                callback(result);
                            }
                        }
                    }
                }
            },
            SpeechMode::VadDeferred => {
                // 与Vad处理一致，但段末不做最终识别；最终识别推迟到StopInput
                while self.rechunk_buffer.len() >= VAD_FRAME_SIZE {
                    let vad_chunk: Vec<f32> = self.rechunk_buffer.drain(0..VAD_FRAME_SIZE).collect();

                    if let Some(vad_event) = self.vad.process_chunk(&ArrayView1::from(&vad_chunk)).await? {
                        // 累积原始音频以便StopInput最终识别使用
                        self.speech_audio_buffer.extend_from_slice(&vad_event.audio);

                        if vad_event.is_first {
                            info!(
                                "🎤 VAD检测到语音开始 (音频长度: {}ms)",
                                (vad_event.audio.len() as f32 / TARGET_SAMPLE_RATE as f32 * 1000.0) as u32
                            );
                            self.backend.reset_streaming();

                            if !self.has_timeout_receiver() {
                                self.start_timeout_monitor().await;
                            }

                            let start_result = AsrResult {
                                text: String::new(),
                                is_partial: true,
                                timestamp: SystemTime::now(),
                                language: self.asr_language.clone(),
                                vad_state: VadState::Speaking,
                            };
                            callback(start_result);
                        }

                        if !vad_event.is_last {
                            // 语音进行中：仅累积特征
                            let _ = self.backend.streaming_recognition(&vad_event.audio, false, false).await?;
                            continue;
                        } else {
                            // vad_event.is_last：先发送Silence状态，然后进行推理，推理完成后退出循环不再接受新音频
                            info!("🔇 VAD检测到语音结束 (deferred模式，进行最终推理)");
                            let end_result = AsrResult {
                                text: String::new(),
                                is_partial: false,
                                timestamp: SystemTime::now(),
                                language: self.asr_language.clone(),
                                vad_state: VadState::Silence,
                            };
                            callback(end_result);

                            // 进行最终推理
                            info!(
                                "🎯 [VadDeferred] 开始段末推理，累积特征数: {} (session: {})",
                                self.speech_audio_buffer.len(),
                                self.session_id
                            );
                            // 段末推理（使用最后一块特征）
                            let result = self.backend.streaming_recognition(&vad_event.audio, true, true).await?;

                            if let Some(voice_text) = result {
                                let txt = voice_text.content.trim();
                                if !txt.is_empty() {
                                    info!(
                                        "✅ [VadDeferred] 段末推理完成，识别结果: '{}' (session: {})",
                                        txt, self.session_id
                                    );
                                    let result = AsrResult {
                                        text: txt.to_string(),
                                        is_partial: false,
                                        timestamp: SystemTime::now(),
                                        language: self.asr_language.clone(),
                                        vad_state: VadState::Silence,
                                    };
                                    crate::monitoring::record_asr_success();
                                    callback(result);
                                } else {
                                    info!("⚠️ [VadDeferred] 段末推理完成，但识别结果为空 (session: {})", self.session_id);
                                }
                            } else {
                                info!("⚠️ [VadDeferred] 段末推理完成，但未返回结果 (session: {})", self.session_id);
                            }

                            // 推理完成后清理状态，为下一轮对话做准备
                            info!("🧹 [VadDeferred] 开始清理状态，准备下一轮对话 (session: {})", self.session_id);
                            // 清空rechunk_buffer，下一轮对话不需要处理本轮的剩余音频块
                            let rechunk_buffer_len = self.rechunk_buffer.len();
                            self.rechunk_buffer.clear();
                            self.backend.reset_streaming();
                            self.vad.reset().await;

                            // 清理超时监控相关状态（如果存在）
                            if let Some(handle) = &self.timeout_monitor_handle
                                && handle.is_finished()
                            {
                                self.timeout_monitor_handle.take();
                            }
                            self.timeout_receiver = None;
                            self.timeout_event_sent = false;

                            // 清空语音音频缓冲区，为下一轮做准备
                            let speech_buffer_len = self.speech_audio_buffer.len();
                            self.speech_audio_buffer.clear();

                            info!(
                                "✅ [VadDeferred] 状态清理完成，已清空rechunk_buffer({}样本)和speech_audio_buffer({}样本)，退出循环 (session: {})",
                                rechunk_buffer_len, speech_buffer_len, self.session_id
                            );

                            // 退出循环，不再处理本次调用中的后续音频块
                            // 下次process_audio_chunk调用时会重新进入循环，处理新的音频数据
                            break;
                        }
                    }
                }
            },
        }

        Ok(())
    }
}

/// 语音段状态信息
#[derive(Debug, Clone)]
pub struct SpeechState {
    pub buffer_samples: usize,
    pub buffer_duration_ms: u64,
    pub accumulated_text_length: usize,
}

/// 历史缓冲区统计信息
#[derive(Debug, Clone)]
pub struct HistoryBufferStats {
    pub chunk_count: usize,
    pub total_samples: usize,
    pub total_duration_ms: u64,
    pub max_chunks: usize,
}

/// ASR模块配置
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ASRModuleConfig {
    pub asr: ASRConfig,
}

/// ASR核心配置
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ASRConfig {
    pub model_path: String,
    pub model_name: String,
    pub vad_config: VADConfig,
    /// 热词配置 (可选)
    pub hotwords: Option<HotwordsConfig>,
    /// 语言偏好设置 (可选): "zh" | "en" | "yue" | "ja" | "ko" | "auto"
    pub language: Option<String>,
    /// 支持的ASR模型列表 (用于动态切换)
    pub supported_models: Option<Vec<SupportedAsrModel>>,
    /// 是否启用ITN逆文本标准化 (默认: false - 保持中文数字形式)
    /// true: 将"一二三"转换为"123"
    /// false: 保持"一二三"原样输出
    pub enable_itn: Option<bool>,
    /// CTC Beam Search 配置 (默认: 4)
    /// beam_size > 1: 使用 beam search，提高解码精度
    /// beam_size = 1: 使用 greedy 解码，最快速度
    pub beam_size: Option<usize>,
}

/// 支持的ASR模型配置
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SupportedAsrModel {
    /// 模型名称
    pub name: String,
    /// 模型类型 ("whisperlive" | "whisper" 等)
    pub model_type: String,
    /// 模型路径
    pub path: String,
    /// 是否启用
    pub enabled: bool,
    /// 模型描述
    pub description: Option<String>,
    /// 模型特定配置
    pub config: Option<serde_json::Value>,
}

/// ASR（自动语音识别）结果
#[derive(Debug, Clone)]
pub struct AsrResult {
    /// 识别出的文本
    pub text: String,
    /// 是否为部分（中间）结果
    pub is_partial: bool,
    /// 结果生成时间戳
    pub timestamp: SystemTime,
    /// 检测到的语言
    pub language: Option<String>,
    /// VAD状态
    pub vad_state: VadState,
}

/// ASR错误类型
#[derive(Debug, thiserror::Error)]
pub enum AsrError {
    #[error("Recognition error: {0}")]
    RecognitionError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Session not started: {0}")]
    SessionNotStarted(String),
}

impl From<crate::vad::VADError> for AsrError {
    fn from(e: crate::vad::VADError) -> Self {
        AsrError::RecognitionError(e.to_string())
    }
}

impl From<Box<dyn std::error::Error + Send + Sync>> for AsrError {
    fn from(e: Box<dyn std::error::Error + Send + Sync>) -> Self {
        AsrError::RecognitionError(e.to_string())
    }
}

use crate::env_utils::env_string_or_default;

impl Default for ASRModuleConfig {
    fn default() -> Self {
        Self {
            asr: ASRConfig {
                model_path: String::new(),
                model_name: "whisperlive-asr".to_string(),
                vad_config: VADConfig::default(),
                hotwords: Some(HotwordsConfig::default()),
                language: Some(env_string_or_default("ASR_LANGUAGE", "auto")),
                supported_models: Some(vec![SupportedAsrModel {
                    name: "whisperlive-asr".to_string(),
                    model_type: "whisperlive".to_string(),
                    path: env_string_or_default("WHISPERLIVE_PATH", "ws://localhost:9090"),
                    enabled: true,
                    description: Some("WhisperLive ASR 服务，多语言支持".to_string()),
                    config: None,
                }]),
                enable_itn: Some(false), // 🔧 默认关闭ITN，保持中文数字形式
                beam_size: Some(4),      // 🔧 默认beam search大小为4
            },
        }
    }
}
