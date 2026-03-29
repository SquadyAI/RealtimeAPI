mod config;
mod state;

use crate::tts::minimax::{MiniMaxConfig, VoiceSetting};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use parking_lot::RwLock as PlRwLock;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, Notify, broadcast, mpsc, watch};

/// 🔧 通道槽位（Slot）：允许原子替换的通道持有器
/// 设计目的：支持 pipeline 在生命周期内被多个 WebSocket 绑定，但同时只有一个活跃
/// - 读取（热路径）：无锁获取 sender clone
/// - 写入（冷路径）：原子替换 sender
#[derive(Debug)]
pub struct ChannelSlot<T> {
    inner: PlRwLock<Option<T>>,
}

impl<T> Default for ChannelSlot<T> {
    fn default() -> Self {
        Self { inner: PlRwLock::new(None) }
    }
}

impl<T: Clone> ChannelSlot<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// 原子替换槽位内容，返回旧值（如果有）
    pub fn swap(&self, new_value: T) -> Option<T> {
        let mut guard = self.inner.write();
        guard.replace(new_value)
    }

    /// 获取当前值的 clone（热路径，无锁竞争时非常快）
    pub fn get(&self) -> Option<T> {
        self.inner.read().clone()
    }

    /// 检查是否已设置
    pub fn is_set(&self) -> bool {
        self.inner.read().is_some()
    }
}
use tracing::{debug, error, info, warn};

use crate::mcp::client::McpClientWrapper;
use crate::rpc::ProtocolId;
use crate::{
    asr::{AsrEngine, SpeechMode},
    llm::{
        LlmTaskV2,
        llm::{ChatCompletionParams, LlmClient},
    },
    rpc::{
        pipeline::{CleanupGuard, StreamingPipeline},
        protocol::BinaryMessage,
        session_router::SessionRouter,
    },
};
// 🔧 移除：不再需要TtsEngineGuard类型

use super::{
    asr_task_core::AsrInputMessage,
    asr_task_vad_deferred::AsrTaskVadDeferred,
    audio_blocking_service::AudioBlockingService,
    event_emitter::EventEmitter,
    simple_interrupt_manager::{InterruptReason as SimpleInterruptReason, SimpleInterruptHandler, SimpleInterruptManager},
    types::{SharedFlags, TaskCompletion, TurnContext},
};
use crate::rpc::pipeline::SessionTaskScope;

// 🆕 全局单调时钟基线：用于节流计算，避免使用 Instant::now().elapsed() 得到 ~0 的错误时间基准
static APP_START: OnceLock<std::time::Instant> = OnceLock::new();

/// 连接状态概览
#[derive(Debug, Clone)]
pub struct ConnectionStatus {
    /// TTS是否已预连接
    pub tts_preconnected: bool,
    /// 会话ID
    pub session_id: String,
}

/// 模块化 ASR+LLM+TTS Pipeline
#[allow(clippy::type_complexity)]
pub struct ModularPipeline {
    session_id: String,
    router: Arc<SessionRouter>,
    asr_engine: Arc<AsrEngine>,
    llm_client: Arc<LlmClient>,
    /// MiniMax TTS 配置（用于按需创建客户端）
    #[allow(dead_code)]
    tts_config: Option<MiniMaxConfig>,

    speech_mode: SpeechMode,
    llm_params: Option<ChatCompletionParams>,
    /// 🆕 多个 MCP 客户端配置（支持同时连接多个 MCP server）
    pub mcp_clients: Vec<McpClientWrapper>,

    // 🔧 通道槽位：支持 pipeline 在生命周期内被多个 WS 绑定（原子替换）
    input_tx_slot: ChannelSlot<mpsc::Sender<AsrInputMessage>>,

    // 共享状态管理
    shared_flags: Arc<SharedFlags>,
    tts_session_created: Arc<AtomicBool>,

    // 🆕 ASR任务状态跟踪
    asr_task_running: Arc<AtomicBool>,

    // 🆕 简化的打断管理器
    simple_interrupt_manager: Arc<SimpleInterruptManager>,

    // 🆕 TTS预连接状态管理
    tts_preconnection_result: Arc<Mutex<Option<Result<(), String>>>>,

    // 🆕 音色设置
    #[allow(dead_code)]
    voice_setting: Option<VoiceSetting>,

    // 🆕 搜索配置
    _search_config: Option<serde_json::Value>,

    // 🔧 新增：启用搜索标志（可热更新）
    enable_search: Arc<AtomicBool>,

    // 🔧 新增：系统提示词
    system_prompt: Option<String>,

    // 🔧 移除VAD相关字段，改用会话数据持久化系统

    // 🔧 新增：初始爆发配置
    initial_burst_count: usize,
    initial_burst_delay_ms: u64,
    // 🔧 新增：发送速率
    send_rate_multiplier: f64,
    // 🆕 新增：控制除了语音和工具调用之外的所有事件的发送
    signal_only: Arc<AtomicBool>,

    // 🆕 TTS 控制器（管线级别）
    tts_controller: Arc<super::tts_task::TtsController>,

    // 🆕 ASR语言设置
    asr_language: Option<String>,

    // 🆕 音频阻断服务：ASR输出发送到LLM时激活，阻止音频包处理和VAD事件
    audio_blocking_service: AudioBlockingService,

    // 🆕 会话任务作用域：统一管理异步任务生命周期，防止任务泄漏
    task_scope: Arc<SessionTaskScope>,

    // 🔧 新增：MCP 提示词注册表
    #[allow(dead_code)]
    mcp_prompt_registry: Arc<crate::llm::McpPromptRegistry>,

    // 🆕 动态配置更新通道
    config_update_tx: Arc<Mutex<Option<mpsc::UnboundedSender<ConfigUpdateEvent>>>>,

    // 🆕 音频输入处理器 - 用于解码和归一化音频
    input_processor: Arc<Mutex<crate::audio::input_processor::AudioInputProcessor>>,

    // 🆕 音频输出配置（完整配置）
    audio_output_config: Arc<Mutex<crate::audio::OutputAudioConfig>>,

    // 🆕 时区和位置信息已移除 - 现在动态从IP地理位置获取
    /// 当为 true 时，response.text.done 仅发送信令（可热更新）
    text_done_signal_only: Arc<AtomicBool>,

    // 🔧 时序修复：保存当前轮次的response_id，用于打断信号（Pipeline级别）
    current_turn_response_id: Arc<super::lockfree_response_id::LockfreeResponseId>,

    idle_return_timeout: std::time::Duration,
    /// 🆕 一次性空闲归还标记：在再次检测到上游活动前只触发一次归还
    idle_returned_once: Arc<AtomicBool>,
    /// 🆕 空闲监控重置通知器（hot path直发，无锁）
    idle_reset_notify: Arc<Notify>,
    /// 🆕 热点优化：上次重置时间戳（秒级），用于节流控制
    last_idle_reset_time: Arc<std::sync::atomic::AtomicU64>,

    // 🔧 音频入口通道槽位：支持 pipeline 在生命周期内被多个 WS 绑定（原子替换）
    audio_ingress_tx_slot: ChannelSlot<mpsc::Sender<Vec<u8>>>,
    /// 🆕 ASR 语言热更新通道（向 ASR 任务广播语言变更）
    asr_language_tx: watch::Sender<Option<String>>,
    /// 🆕 VAD 运行时参数更新通道（threshold, min_silence_ms, min_speech_ms）
    asr_vad_tx: watch::Sender<Option<(Option<f32>, Option<u32>, Option<u32>)>>,

    /// 🆕 工具端点异步加载事件接收器（用于将工具注入到 LLM 任务）
    tools_loaded_rx: Option<broadcast::Receiver<crate::mcp::ToolsLoadedEvent>>,
    // 🔧 移除 defer_llm_until_stop_input 字段，现在通过 SpeechMode::VadDeferred 来处理
    /// 🆕 直接文本输入通道（用于文本-LLM-TTS混合模式，protocol_id=100+文本输入）
    direct_text_tx: Arc<Mutex<Option<mpsc::UnboundedSender<(TurnContext, String)>>>>,
}

/// 🆕 配置更新事件
#[derive(Debug, Clone)]
pub enum ConfigUpdateEvent {
    /// 更新MCP配置
    UpdateMcpClients {
        mcp_configs: Vec<crate::mcp::McpServerConfig>,
        mcp_manager: Arc<crate::mcp::McpManager>,
    },
}

impl ModularPipeline {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: &str,
        router: Arc<SessionRouter>,
        asr_engine: Arc<AsrEngine>,
        llm_client: Arc<LlmClient>,
        tts_config: Option<MiniMaxConfig>,
        speech_mode: SpeechMode,
        voice_setting: Option<VoiceSetting>,
        search_config: Option<serde_json::Value>,
        enable_search: bool,
        system_prompt: Option<String>,
        initial_burst_count: usize,
        initial_burst_delay_ms: u64,
        send_rate_multiplier: f64,
        asr_language: Option<String>,
        mcp_prompt_registry: Arc<crate::llm::McpPromptRegistry>,
        input_audio_config: crate::audio::input_processor::AudioInputConfig,
        output_audio_config: crate::audio::OutputAudioConfig,
        // timezone和location参数已移除 - 现在动态从IP地理位置获取
        text_done_signal_only: bool,
        signal_only: bool,
        asr_chinese_convert: Option<String>,
        tts_chinese_convert: Option<String>,
        emoji_prompt: Option<String>,
    ) -> Self {
        info!("🚀 创建模块化 Pipeline: {}", session_id);
        info!("🔍 ModularPipeline::new: asr_language={:?}", asr_language);

        // 记录音色设置信息
        if let Some(ref vs) = voice_setting {
            info!(
                "🎙️ 使用自定义TTS语音设置: voice_id={:?}, speed={:?}, vol={:?}, pitch={:?}, emotion={:?}",
                vs.voice_id, vs.speed, vs.vol, vs.pitch, vs.emotion
            );
        } else {
            info!("🎙️ 使用默认TTS语音设置");
        }

        // 🔧 简化：移除VAD音频保存器，使用会话数据持久化系统

        // 🚀 先创建打断管理器
        let simple_interrupt_manager = Arc::new(SimpleInterruptManager::new());

        // 🚀 创建TTS控制器并设置打断管理器
        let mut tts_controller = super::tts_task::TtsController::new(tts_config.clone(), voice_setting.clone());
        tts_controller.set_interrupt_manager(simple_interrupt_manager.clone());
        info!("✅ TTS控制器已配置打断管理器支持");
        let tts_controller = Arc::new(tts_controller);

        // 将 asr_language 传递给 TTS 控制器（用于 MiniMax start_task 的 language_boost）
        {
            let ctrl = tts_controller.clone();
            let lang = asr_language.clone();
            tokio::spawn(async move {
                ctrl.set_language(lang).await;
            });
        }

        // 🆕 初始化音频输入处理器（使用 startSession 传入的配置或默认配置）
        // 🆕 初始化音频输入处理器（使用 startSession 传入的配置或默认配置）
        let input_processor = match crate::audio::AudioInputProcessor::new(input_audio_config.clone()) {
            Ok(processor) => {
                info!(
                    "✅ 音频输入处理器初始化成功: format={:?}, sample_rate={}",
                    input_audio_config.format, input_audio_config.sample_rate
                );
                Arc::new(Mutex::new(processor))
            },
            Err(e) => {
                error!("❌ 音频输入处理器初始化失败: {}", e);
                panic!("音频输入处理器初始化失败: {}", e);
            },
        };

        // 初始化可热更新的配置
        let enable_search_flag = Arc::new(AtomicBool::new(enable_search));

        // 🆕 初始化音频阻断服务 - 从环境变量读取配置，默认100ms
        let audio_blocking_enabled = crate::env_utils::env_bool_or_default("AUDIO_BLOCKING_ENABLED", true);
        let audio_blocking_duration_ms = crate::env_utils::env_or_default("AUDIO_BLOCKING_DURATION_MS", 100);

        info!(
            "🔒 音频阻断锁配置: enabled={}, duration={}ms (AUDIO_BLOCKING_ENABLED={:?}, AUDIO_BLOCKING_DURATION_MS={:?})",
            audio_blocking_enabled,
            audio_blocking_duration_ms,
            std::env::var("AUDIO_BLOCKING_ENABLED"),
            std::env::var("AUDIO_BLOCKING_DURATION_MS")
        );

        // 🆕 创建会话任务作用域
        let task_scope = Arc::new(SessionTaskScope::new(session_id));

        // 🆕 创建音频阻断服务（使用 task_scope 管理定时器任务）
        let audio_blocking_service = AudioBlockingService::new(
            session_id.to_string(),
            audio_blocking_enabled,
            audio_blocking_duration_ms,
            task_scope.clone(),
        );
        let text_done_signal_only_flag = Arc::new(AtomicBool::new(text_done_signal_only));
        let signal_only_flag = Arc::new(AtomicBool::new(signal_only));
        let (asr_language_tx, _asr_language_rx_init) = watch::channel(asr_language.clone());
        // 🆕 VAD 参数热更新通道（初始为 None 表示无更新）
        let (asr_vad_tx, _asr_vad_rx_init) = watch::channel::<Option<(Option<f32>, Option<u32>, Option<u32>)>>(None);
        // 🆕 订阅 asr_language 变化并热更新到 TTS 控制器（用于下一次 start_task 的 language_boost）
        {
            let ctrl = tts_controller.clone();
            let mut rx = asr_language_tx.subscribe();
            tokio::spawn(async move {
                loop {
                    if rx.changed().await.is_err() {
                        break;
                    }
                    let lang = rx.borrow().clone();
                    ctrl.set_language(lang).await;
                }
            });
        }

        let session_id_owned = session_id.to_string();
        // 预先创建idle reset通道，避免热点path加锁
        let idle_reset_notify = Arc::new(Notify::new());
        let idle_base = *APP_START.get_or_init(std::time::Instant::now);
        let initial_idle_secs = idle_base.elapsed().as_secs();
        // 🆕 热点优化：初始化节流控制时间戳（秒级）
        let last_idle_reset_time = Arc::new(std::sync::atomic::AtomicU64::new(initial_idle_secs));

        let pipeline = Self {
            session_id: session_id_owned.clone(),
            router,
            asr_engine,
            llm_client: llm_client.clone(),
            tts_config: tts_config.clone(), // Keep clone here as tts_controller uses it
            speech_mode,
            llm_params: None,        // llm_params is set via with_llm_params, not in new
            mcp_clients: Vec::new(), // 🆕 初始化为空的 MCP 客户端列表
            input_tx_slot: ChannelSlot::new(),
            shared_flags: {
                let flags = SharedFlags::new();
                // 🆕 解析 ASR 繁简转换模式
                let asr_mode = asr_chinese_convert
                    .as_deref()
                    .map(crate::text_filters::ConvertMode::from)
                    .unwrap_or(crate::text_filters::ConvertMode::None);
                {
                    if let Ok(mut guard) = flags.asr_chinese_convert_mode.write() {
                        *guard = asr_mode;
                    }
                }
                // 🆕 解析 TTS 繁简转换模式
                let tts_mode = tts_chinese_convert
                    .as_deref()
                    .map(crate::text_filters::ConvertMode::from)
                    .unwrap_or(crate::text_filters::ConvertMode::None);
                {
                    if let Ok(mut guard) = flags.tts_chinese_convert_mode.write() {
                        *guard = tts_mode;
                    }
                }
                // 🆕 注入表情选择提示词
                if let Some(prompt) = emoji_prompt.clone() {
                    let mut g = flags.emoji_prompt.lock().unwrap();
                    *g = Some(prompt);
                }
                Arc::new(flags)
            },
            tts_session_created: Arc::new(AtomicBool::new(false)),
            asr_task_running: Arc::new(AtomicBool::new(false)),
            simple_interrupt_manager,
            tts_preconnection_result: Arc::new(Mutex::new(None)),
            voice_setting: voice_setting.clone(),
            _search_config: search_config.clone(),
            enable_search: enable_search_flag,
            system_prompt: system_prompt.clone(),
            initial_burst_count,
            initial_burst_delay_ms,
            send_rate_multiplier,
            tts_controller,
            asr_language,

            // 🆕 音频阻断服务 + 任务作用域
            audio_blocking_service,
            task_scope,

            mcp_prompt_registry,
            config_update_tx: Arc::new(Mutex::new(None)),                   // 🆕 初始化为None，在start时设置
            input_processor,                                                // 🆕 添加音频输入处理器
            audio_output_config: Arc::new(Mutex::new(output_audio_config)), // 🆕 添加音频输出配置（现在总是有效的配置）
            // timezone和location字段已移除
            text_done_signal_only: text_done_signal_only_flag,
            signal_only: signal_only_flag,

            // 🔧 时序修复：初始化当前轮次ID（Pipeline级别）
            current_turn_response_id: Arc::new(super::lockfree_response_id::LockfreeResponseId::new()),
            // 🆕 初始化会话级别空闲监控
            idle_return_timeout: {
                let secs = std::env::var("TTS_IDLE_RETURN_SECS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(120);
                std::time::Duration::from_secs(secs)
            },
            idle_returned_once: Arc::new(AtomicBool::new(false)),
            idle_reset_notify,
            last_idle_reset_time,
            asr_language_tx,
            asr_vad_tx,
            audio_ingress_tx_slot: ChannelSlot::new(),

            tools_loaded_rx: None,
            direct_text_tx: Arc::new(Mutex::new(None)),
        };

        // 🌡️ 启动LLM连接预热检查（异步，不阻塞创建过程）
        info!("🌡️ 检查 LLM 连接预热状态: {}", session_id);
        let llm_client_clone = llm_client.clone();
        let session_id_for_llm = session_id_owned.clone();

        tokio::spawn(async move {
            let start_time = std::time::Instant::now();

            // 等待LLM连接预热完成（最多等待3秒）
            let warmed = llm_client_clone.wait_for_connection_warm(3000).await;

            if warmed {
                info!(
                    "✅ LLM 连接预热已完成: {} (检查耗时: {:?})",
                    session_id_for_llm,
                    start_time.elapsed()
                );
            } else {
                warn!(
                    "⚠️ LLM 连接预热超时，将使用冷连接: {} (耗时: {:?})",
                    session_id_for_llm,
                    start_time.elapsed()
                );
            }
        });

        info!("✅ 模块化 Pipeline 创建完成，TTS预连接正在后台进行: {}", session_id);
        pipeline
    }

    // 🔧 移除：不再需要Pipeline级别的TtsEngineGuard设置
    // ModularPipeline使用轮次级别的会话管理，引擎在TTS任务级别动态管理

    /// 设置LLM参数（包括Function Call配置）
    pub fn with_llm_params(mut self, params: ChatCompletionParams) -> Self {
        self.llm_params = Some(params);
        self
    }

    // 🔧 移除 with_defer_llm_until_stop_input 方法，现在通过 SpeechMode::VadDeferred 来处理

    /// 启用Function Call功能的便捷方法
    pub fn with_function_calls(mut self, tools: Vec<crate::llm::llm::Tool>, tool_choice: Option<crate::llm::llm::ToolChoice>) -> Self {
        let mut params = self.llm_params.unwrap_or_default();
        params.tools = Some(tools.clone());

        // 🔧 修复：当有工具但tool_choice为None时，默认设置为Auto
        params.tool_choice = if tool_choice.is_none() && !tools.is_empty() {
            Some(crate::llm::llm::ToolChoice::auto())
        } else {
            tool_choice
        };

        self.llm_params = Some(params);
        self
    }

    /// 添加多个 MCP 服务器配置
    pub fn with_mcp_servers(mut self, mcp_configs: Vec<crate::mcp::McpServerConfig>, mcp_manager: Arc<crate::mcp::McpManager>) -> Self {
        for config in mcp_configs {
            let wrapper = if config.is_http_protocol() {
                // HTTP MCP 客户端
                if let Some(http_config) = config.to_http_config() {
                    let client = Arc::new(crate::mcp::HttpMcpClient::new(http_config));
                    McpClientWrapper::Http { client, config }
                } else {
                    warn!("⚠️ HTTP MCP 配置转换失败: {}", config.endpoint);
                    continue;
                }
            } else if config.is_websocket_protocol() || config.is_stdio_protocol() {
                // WebSocket/stdio MCP 客户端
                McpClientWrapper::WebSocket { manager: mcp_manager.clone(), config }
            } else {
                warn!("⚠️ 未知的MCP协议: {}, 跳过该配置", config.endpoint);
                continue;
            };

            info!(
                "✅ 已添加 MCP 客户端: {} (协议: {})",
                wrapper.endpoint(),
                if wrapper.is_http() { "HTTP" } else { "WebSocket/stdio" }
            );
            self.mcp_clients.push(wrapper);
        }
        self
    }

    /// 🆕 添加单个 HTTP MCP 客户端（向后兼容）
    pub fn with_http_mcp_client(mut self, http_mcp_client: Arc<crate::mcp::HttpMcpClient>, config: crate::mcp::McpServerConfig) -> Self {
        let wrapper = McpClientWrapper::Http { client: http_mcp_client, config };
        info!("✅ 已添加单个 HTTP MCP 客户端: {}", wrapper.endpoint());
        self.mcp_clients.push(wrapper);
        self
    }

    /// Set tools loaded event receiver (builder pattern).
    pub fn with_tools_loaded_receiver(mut self, rx: broadcast::Receiver<crate::mcp::ToolsLoadedEvent>) -> Self {
        self.tools_loaded_rx = Some(rx);
        self
    }
}

#[async_trait]
impl StreamingPipeline for ModularPipeline {
    async fn start(&self) -> Result<CleanupGuard> {
        let pipeline_start_time = std::time::Instant::now();
        info!("🚀 启动模块化 ASR+LLM+TTS Pipeline: {}", self.session_id);
        // 创建事件发射器（读取可热更新标志）
        let emitter = Arc::new(EventEmitter::new(
            self.router.clone(),
            self.session_id.clone(),
            self.text_done_signal_only.clone(),
            self.signal_only.clone(),
        ));

        // 🚀 注意：session.created 已移至音频worker启动后发送，避免竞态条件
        // （移动到第 1261 行之后，确保服务器完全准备好接收音频数据）
        // emitter.session_created(ProtocolId::All).await;

        // 🚀 并行化：LLM init_session、TTS 配置、MCP 工具预加载 同时进行
        let parallel_init_start = std::time::Instant::now();
        info!("🚀 开始并行初始化: LLM + TTS + MCP | session_id={}", self.session_id);

        // 准备 LLM init_session 的 future
        let llm_client_clone = self.llm_client.clone();
        let session_id_for_llm = self.session_id.clone();
        let system_prompt_clone = self.system_prompt.clone();
        let llm_init_future = async move {
            let llm_init_start = std::time::Instant::now();
            if let Some(ref prompt) = system_prompt_clone {
                info!("🤖 初始化LLM会话，使用自定义系统提示词，长度: {}", prompt.len());
                llm_client_clone.init_session(&session_id_for_llm, Some(prompt.clone())).await;
            } else {
                warn!("⚠️ 初始化LLM会话时没有系统提示词！客户端可能没有发送system_prompt");
                llm_client_clone.init_session(&session_id_for_llm, None).await;
            }
            let elapsed = llm_init_start.elapsed();
            info!(
                "⏱️ [计时] LLM init_session 耗时: {:?} | session_id={}",
                elapsed, session_id_for_llm
            );
            elapsed
        };

        // 准备 TTS 配置的 future
        let tts_controller_clone = self.tts_controller.clone();
        let audio_output_config_clone = self.audio_output_config.clone();
        let session_id_for_tts = self.session_id.clone();
        let tts_config_future = async move {
            let tts_config_start = std::time::Instant::now();
            let output_config = {
                let config_guard = audio_output_config_clone.lock().await;
                config_guard.clone()
            };
            info!("🎵 配置TTS音频输出配置: {:?}", output_config);
            let result = tts_controller_clone.configure_output_config(output_config.clone()).await;
            let elapsed = tts_config_start.elapsed();
            info!(
                "⏱️ [计时] TTS configure_output_config 耗时: {:?} | session_id={}",
                elapsed, session_id_for_tts
            );
            (result, output_config, elapsed)
        };

        // 准备 MCP 工具预加载的 future（内部并行获取所有 MCP 客户端工具）
        let mcp_clients_clone = self.mcp_clients.clone();
        let session_id_for_mcp = self.session_id.clone();
        let mcp_tools_future = async move {
            let mcp_tools_start = std::time::Instant::now();
            if mcp_clients_clone.is_empty() {
                return ((Vec::new(), Vec::new(), session_id_for_mcp), std::time::Duration::ZERO);
            }

            info!("🔗 并行初始化 {} 个MCP客户端", mcp_clients_clone.len());

            // 🚀 并行获取所有 MCP 客户端的工具
            let mut mcp_futures = Vec::new();
            for mcp_client in &mcp_clients_clone {
                match mcp_client {
                    McpClientWrapper::Http { client, config } => {
                        let client_clone = client.clone();
                        let config_clone = config.clone();
                        let session_id_clone = session_id_for_mcp.clone();
                        mcp_futures.push(Box::pin(async move {
                            let http_mcp_start = std::time::Instant::now();
                            match crate::mcp::GLOBAL_MCP_TOOL_CACHE.get_http_mcp_tools(&client_clone).await {
                                Ok(tools) => {
                                    info!(
                                        "✅ 从HTTP MCP服务器预加载 {} 个工具到缓存 ({}) | ⏱️ 耗时: {:?}",
                                        tools.len(),
                                        config_clone.endpoint,
                                        http_mcp_start.elapsed()
                                    );
                                    let sources: Vec<(String, crate::mcp::ToolSourceType)> = tools
                                        .iter()
                                        .map(|t| {
                                            (
                                                t.name.clone(),
                                                crate::mcp::ToolSourceType::HttpMcp(config_clone.endpoint.clone()),
                                            )
                                        })
                                        .collect();
                                    (tools, sources)
                                },
                                Err(e) => {
                                    warn!(
                                        "⚠️ HTTP MCP工具预加载失败: {} (工具将在使用时动态获取) | ⏱️ 耗时: {:?}",
                                        e,
                                        http_mcp_start.elapsed()
                                    );
                                    // 后台重试
                                    let client_bg = client_clone.clone();
                                    let config_bg = config_clone.clone();
                                    tokio::spawn(async move {
                                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                                        match crate::mcp::GLOBAL_MCP_TOOL_CACHE.get_http_mcp_tools(&client_bg).await {
                                            Ok(tools) => {
                                                info!("✅ HTTP MCP工具后台预加载成功: {} 个工具 ({})", tools.len(), config_bg.endpoint);
                                                let llm_tools: Vec<crate::llm::llm::Tool> = tools.iter().map(|t| t.clone().into()).collect();
                                                crate::mcp::register_tool_sources(
                                                    &session_id_clone,
                                                    &llm_tools,
                                                    crate::mcp::ToolSourceType::HttpMcp(config_bg.endpoint.clone()),
                                                )
                                                .await;
                                            },
                                            Err(e) => info!("📝 HTTP MCP工具后台预加载仍失败: {} ({})", e, config_bg.endpoint),
                                        }
                                    });
                                    (Vec::new(), Vec::new())
                                },
                            }
                        })
                            as std::pin::Pin<
                                Box<dyn std::future::Future<Output = (Vec<crate::mcp::McpTool>, Vec<(String, crate::mcp::ToolSourceType)>)> + Send>,
                            >);
                    },
                    McpClientWrapper::WebSocket { manager, config } => {
                        let manager_clone = manager.clone();
                        let config_clone = config.clone();
                        mcp_futures.push(Box::pin(async move {
                            let ws_mcp_start = std::time::Instant::now();
                            match manager_clone.get_tools(&config_clone).await {
                                Ok(tools) => {
                                    info!(
                                        "✅ 从WebSocket MCP缓存获取到 {} 个工具 ({}) | ⏱️ 耗时: {:?}",
                                        tools.len(),
                                        config_clone.endpoint,
                                        ws_mcp_start.elapsed()
                                    );
                                    let sources: Vec<(String, crate::mcp::ToolSourceType)> = tools
                                        .iter()
                                        .map(|t| (t.name.clone(), crate::mcp::ToolSourceType::WsMcp(config_clone.endpoint.clone())))
                                        .collect();
                                    (tools, sources)
                                },
                                Err(e) => {
                                    info!(
                                        "📝 WebSocket MCP工具暂不可用: {} (将在后台连接) | ⏱️ 耗时: {:?}",
                                        e,
                                        ws_mcp_start.elapsed()
                                    );
                                    // 后台连接
                                    let manager_bg = manager_clone.clone();
                                    let config_bg = config_clone.clone();
                                    tokio::spawn(async move {
                                        if let Some(_client) = manager_bg.get_client(&config_bg).await {
                                            info!("🔗 WebSocket MCP客户端后台连接完成: {}", config_bg.endpoint);
                                        } else {
                                            info!("🔗 WebSocket MCP客户端正在后台连接中: {}", config_bg.endpoint);
                                        }
                                    });
                                    (Vec::new(), Vec::new())
                                },
                            }
                        })
                            as std::pin::Pin<
                                Box<dyn std::future::Future<Output = (Vec<crate::mcp::McpTool>, Vec<(String, crate::mcp::ToolSourceType)>)> + Send>,
                            >);
                    },
                }
            }

            // 并行执行所有 MCP 工具获取
            let results = futures::future::join_all(mcp_futures).await;

            // 合并结果
            let mut all_mcp_tools = Vec::new();
            let mut tool_sources = Vec::new();
            for (tools, sources) in results {
                all_mcp_tools.extend(tools);
                tool_sources.extend(sources);
            }

            let elapsed = mcp_tools_start.elapsed();
            info!(
                "⏱️ [计时] MCP工具预加载 总耗时: {:?} | 工具数: {}",
                elapsed,
                all_mcp_tools.len()
            );

            // 返回合并后的工具和来源
            ((all_mcp_tools, tool_sources, session_id_for_mcp), elapsed)
        };

        // 🚀 并行执行所有初始化任务
        let (llm_elapsed, (tts_result, _output_config, tts_elapsed), ((all_mcp_tools, tool_sources, _session_id_mcp), mcp_elapsed)) =
            tokio::join!(llm_init_future, tts_config_future, mcp_tools_future);

        // 检查 TTS 配置结果
        tts_result?;

        info!(
            "🚀 并行初始化完成 | LLM: {:?}, TTS: {:?}, MCP: {:?} | 总并行耗时: {:?}",
            llm_elapsed,
            tts_elapsed,
            mcp_elapsed,
            parallel_init_start.elapsed()
        );

        // 预热逻辑移除：MiniMax TTS 按需创建并在会话内复用 WS
        info!("🔌 MiniMax TTS 将按需创建客户端: {}", self.session_id);

        // 🆕 处理 MCP 工具结果
        let mcp_config_initialized = if !all_mcp_tools.is_empty() {
            // 注册 MCP 工具来源
            if !tool_sources.is_empty() {
                let mut sources_guard = crate::mcp::async_tools_manager::SESSION_TOOL_SOURCES.write().await;
                let entry = sources_guard
                    .entry(self.session_id.clone())
                    .or_insert_with(rustc_hash::FxHashMap::default);
                for (name, source) in tool_sources {
                    entry.insert(name, source);
                }
            }

            // 如果有工具，立即配置LLM参数（与现有tools合并；同名以MCP优先）
            let mcp_tools: Vec<crate::llm::llm::Tool> = all_mcp_tools.into_iter().map(|mcp_tool| mcp_tool.into()).collect();

            let mut params = self.llm_params.clone().unwrap_or_default();

            // 合并策略：先放入现有tools，再用MCP覆盖同名
            let mut merged: rustc_hash::FxHashMap<String, crate::llm::llm::Tool> = rustc_hash::FxHashMap::default();
            if let Some(existing) = params.tools.take() {
                for t in existing {
                    merged.insert(t.function.name.clone(), t);
                }
            }
            for t in mcp_tools {
                merged.insert(t.function.name.clone(), t);
            }

            let merged_vec = merged.into_values().collect::<Vec<_>>();
            params.tools = Some(merged_vec);

            // 若未显式设置 tool_choice 且存在工具，则默认 Auto
            if params.tool_choice.is_none()
                && let Some(ref v) = params.tools
                && !v.is_empty()
            {
                params.tool_choice = Some(crate::llm::llm::ToolChoice::auto());
            }

            Some(Some(params))
        } else {
            None
        };

        // 创建任务间通信通道（有界，避免无界积压）
        // 容量可通过环境变量 ASR_INPUT_CHANNEL_CAP 进行配置，默认 4096
        let input_channel_cap = std::env::var("ASR_INPUT_CHANNEL_CAP")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(4096);
        // 标准统一：ASR 输入通道（Audio/PttEnd）
        let (input_tx, input_rx_bounded) = mpsc::channel::<AsrInputMessage>(input_channel_cap);
        let (asr_to_llm_tx, asr_to_llm_rx) = mpsc::unbounded_channel::<(TurnContext, String)>();
        let (llm_to_tts_tx, _) = broadcast::channel::<(TurnContext, String)>(100);
        let llm_to_tts_rx = llm_to_tts_tx.subscribe();
        let (task_completion_tx, mut task_completion_rx) = mpsc::unbounded_channel::<TaskCompletion>();
        // 🔧 新增：ASR清理信号通道
        let (asr_cleanup_tx, asr_cleanup_rx) = mpsc::unbounded_channel::<()>();

        // 🆕 创建音频段保存通道
        // 🔧 已删除原有音频段通道，使用新的会话数据持久化系统

        // 🚀 新增：创建ASR到LLM的发送闭包，TTS 使用按需创建
        // 注意：volcEngine-tts 使用按需创建，不需要预先克隆引擎
        let _session_id_for_parallel = self.session_id.clone();
        let shared_flags_for_parallel = self.shared_flags.clone();
        let _tts_session_created_for_parallel = self.tts_session_created.clone(); // 🔧 重要修复：使用同一个标志
        let llm_tx_for_parallel = asr_to_llm_tx.clone(); // 并行处理任务用来发送到LLM

        // 创建专用通道，用于ASR到并行处理任务的通信
        let (parallel_llm_tx, mut parallel_llm_rx) = mpsc::unbounded_channel::<(TurnContext, String)>();

        // 🆕 保存direct_text_tx到结构体，用于处理文本输入（protocol_id=100+文本）
        {
            let mut direct_text_tx_guard = self.direct_text_tx.lock().await;
            *direct_text_tx_guard = Some(parallel_llm_tx.clone());
        }

        // 启动并行处理任务 - 简化版本，专注于消息路由
        let tts_controller_for_parallel = self.tts_controller.clone();
        let session_id_for_parallel = self.session_id.clone();
        // 🆕 创建音频阻断激活器
        let audio_blocking_activator = self.create_audio_blocking_activator();

        let _parallel_task = tokio::spawn(async move {
            info!("🚀 并行处理任务开始监听消息: session={}", session_id_for_parallel);
            while let Some((ctx, user_text)) = parallel_llm_rx.recv().await {
                debug!(
                    "📨 并行处理任务收到消息: session={}, response_id={}, text='{}'",
                    session_id_for_parallel,
                    ctx.response_id,
                    user_text.chars().take(50).collect::<String>()
                );
                // 🚀 关键优化：立即更新轮次上下文
                shared_flags_for_parallel
                    .assistant_response_context
                    .update_context(ctx.assistant_item_id.clone(), ctx.response_id.clone());
                info!("🔄 立即更新轮次上下文: response_id={}", ctx.response_id);

                // 🆕 关键功能：在发送到LLM前激活音频阻断锁，防止用户继续说话干扰当前轮次
                info!(
                    "🔒 即将激活音频阻断锁: session={}, response_id={}",
                    session_id_for_parallel, ctx.response_id
                );
                audio_blocking_activator.activate_lock(&ctx.response_id).await;
                info!(
                    "✅ 音频阻断锁激活完成: session={}, response_id={}",
                    session_id_for_parallel, ctx.response_id
                );

                // 🚀 架构修正：优先发送给LLM，并行预热TTS
                info!(
                    "📤 并行处理任务准备发送到LLM: session={}, response_id={}",
                    session_id_for_parallel, ctx.response_id
                );
                if let Err(e) = llm_tx_for_parallel.send((ctx.clone(), user_text.clone())) {
                    error!("❌ 并行处理任务发送到LLM失败: session={}, error={}", session_id_for_parallel, e);
                    continue;
                }
                info!(
                    "✅ 并行处理任务已成功转发用户消息到LLM: session={}, text='{}'",
                    session_id_for_parallel,
                    user_text.chars().take(50).collect::<String>()
                );

                // 🚀 多轮打断优化：智能判断是否需要预热TTS客户端
                let response_id_clone = ctx.response_id.clone();

                // 🎯 使用多轮打断友好的检查方法
                let can_reuse = tts_controller_for_parallel.can_reuse_or_interrupt_client().await;

                if !can_reuse {
                    info!("🌡️ 检测到无可复用TTS客户端，开始智能预热: response_id={}", response_id_clone);
                    match tts_controller_for_parallel.prewarm().await {
                        Ok(()) => {
                            info!("✅ 多轮打断场景TTS预热成功: response_id={}", response_id_clone);
                        },
                        Err(e) => {
                            warn!("⚠️ 多轮打断场景TTS预热失败: response_id={}, error={}", response_id_clone, e);
                        },
                    }
                } else {
                    info!("✅ TTS客户端可复用或中断，跳过预热: response_id={}", response_id_clone);
                }

                info!(
                    "🔌 TTS 客户端已预热（若失败会在轮次创建中降级处理），准备接收LLM输出: response_id={}",
                    ctx.response_id
                );
            }
        });

        // 🔧 已删除原有音频保存任务，使用新的会话数据持久化系统

        // 🔧 通道槽位：原子替换，支持 pipeline 重绑定场景
        // 当 start() 被多次调用时，旧的 sender 被 drop，旧任务会感知通道关闭并退出
        if let Some(old_tx) = self.input_tx_slot.swap(input_tx.clone()) {
            info!("🔄 [SLOT] input_tx 槽位已更新，旧通道被替换: session={}", self.session_id);
            drop(old_tx); // 显式 drop 旧 sender
        }

        // 🆕 创建音频入口worker，解耦 on_upstream 的CPU工作
        let (audio_ingress_tx, mut audio_ingress_rx) = mpsc::channel::<Vec<u8>>(1024);
        // 🔧 通道槽位：原子替换
        if let Some(old_tx) = self.audio_ingress_tx_slot.swap(audio_ingress_tx.clone()) {
            info!(
                "🔄 [SLOT] audio_ingress_tx 槽位已更新，旧通道被替换: session={}",
                self.session_id
            );
            drop(old_tx); // 显式 drop 旧 sender
        }

        let input_processor_for_worker = self.input_processor.clone();
        let session_id_for_worker = self.session_id.clone();
        // 🔧 关键：使用刚创建的 input_tx，确保 worker 使用当前轮次的 sender
        let input_tx_for_worker = Some(input_tx.clone());
        // 🆕 创建音频阻断检查器
        let audio_blocking_checker = self.create_audio_blocking_checker();

        // 🆕 创建 oneshot channel 确保 worker 真正启动后再发送 session.created
        let (worker_ready_tx, worker_ready_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            // ✅ 第一时间发送 worker 就绪信号
            let _ = worker_ready_tx.send(());
            info!("🎧 音频入口worker启动: session={}", session_id_for_worker);
            while let Some(audio_bytes) = audio_ingress_rx.recv().await {
                // 🔧 诊断日志：确认音频入口 worker 收到音频包
                tracing::debug!(
                    "🎧 [AUDIO-WORKER] 收到音频包 | session={} | bytes={}",
                    session_id_for_worker,
                    audio_bytes.len()
                );

                // 🆕 使用音频阻断检查器：如果处于锁定状态，直接丢弃音频包，不进行任何处理
                if audio_blocking_checker.should_block_audio().await {
                    // 仍在锁定期内，丢弃此音频包
                    info!(
                        "🚫 音频包被音频阻断锁丢弃: session={}, 包大小={} bytes",
                        session_id_for_worker,
                        audio_bytes.len()
                    );
                    continue;
                }

                let mut input_processor_guard = input_processor_for_worker.lock().await;
                match input_processor_guard.process_audio_chunk(&audio_bytes) {
                    Ok(audio_f32) => {
                        if audio_f32.is_empty() {
                            info!("🎤 [TRACE-AUDIO] 处理后音频为空，跳过 | session_id={}", session_id_for_worker);
                            continue;
                        }
                        if let Some(tx) = input_tx_for_worker.as_ref() {
                            let send_result = tx.try_send(AsrInputMessage::Audio(audio_f32));
                            if send_result.is_ok() {
                            } else {
                                warn!(
                                    "🎤 [TRACE-AUDIO] 音频包转发到ASR失败 | session_id={} | error={:?}",
                                    session_id_for_worker,
                                    send_result.err()
                                );
                            }
                        } else {
                            warn!("🎤 [TRACE-AUDIO] ASR输入通道未初始化 | session_id={}", session_id_for_worker);
                        }
                    },
                    Err(e) => {
                        warn!(
                            "🎤 [TRACE-AUDIO] 音频入口worker处理失败 | session_id={} | error={}",
                            session_id_for_worker, e
                        );
                        continue;
                    },
                }
            }
            info!("🎧 音频入口worker退出: session={}", session_id_for_worker);
        });

        // ✅ 等待 worker 真正启动后再发送 session.created
        let _ = worker_ready_rx.await;

        // ✅ 新增：在音频worker启动后才发送 session.created，确保服务器完全准备好
        // 这样可以避免客户端发送音频时服务器还没准备好接收的竞态条件
        emitter.session_created(ProtocolId::All).await;
        info!(
            "✅ session.created 已发送，服务器已完全准备好接收音频: {} | ⏱️ pipeline.start 总耗时: {:?}",
            self.session_id,
            pipeline_start_time.elapsed()
        );

        // 🆕 会话级别空闲超时归还监控任务（一次性定时器 + 活动重置）
        {
            let session_id_for_idle = self.session_id.clone();
            let idle_timeout = self.idle_return_timeout;
            let tts_controller_for_idle = self.tts_controller.clone();
            let idle_returned_once = self.idle_returned_once.clone();
            let idle_reset_notify = self.idle_reset_notify.clone();
            let last_idle_reset_time = self.last_idle_reset_time.clone();

            tokio::spawn(async move {
                // 🚀 优化：使用轻量通知机制，减少hot path开销
                let idle_base = *APP_START.get_or_init(std::time::Instant::now);
                let initial_last_secs = last_idle_reset_time.load(Ordering::Acquire);
                let initial_last = idle_base + std::time::Duration::from_secs(initial_last_secs);
                let mut deadline = tokio::time::Instant::from_std(initial_last + idle_timeout);
                let sleep = tokio::time::sleep_until(deadline);
                tokio::pin!(sleep);

                let mut last_processed_time = initial_last;

                loop {
                    tokio::select! {
                        // 🚀 优化：活动重置时使用更轻量的通知机制
                        _ = idle_reset_notify.notified() => {
                            // 🔧 关键优化：只在时间真正变化且超过最小间隔时才处理
                            // 避免频繁的通知导致的重复reset
                            const MIN_RESET_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
                            let new_last_secs = last_idle_reset_time.load(Ordering::Acquire);
                            let new_last = idle_base + std::time::Duration::from_secs(new_last_secs);
                            let interval = new_last.saturating_duration_since(last_processed_time);
                            if interval >= MIN_RESET_INTERVAL {
                                deadline = tokio::time::Instant::from_std(new_last + idle_timeout);
                                sleep.as_mut().reset(deadline);
                                // 重置一次性门槛，允许下一次空闲归还触发
                                idle_returned_once.store(false, Ordering::Release);
                                last_processed_time = new_last;
                                // debug!("🔄 Idle定时器重置: session={}, interval={}ms",
                                //        session_id_for_idle,
                                //        interval.as_millis());
                            }
                        }
                        // 🚀 优化：到期处理，减少不必要的重复计算
                        _ = &mut sleep => {
                            let now_std = std::time::Instant::now();
                            let last_secs = last_idle_reset_time.load(Ordering::Acquire);
                            let last = idle_base + std::time::Duration::from_secs(last_secs);
                            let idle_elapsed = last.elapsed();
                            if idle_elapsed >= idle_timeout {
                                let idle_secs = idle_elapsed.as_secs();
                                if !idle_returned_once.swap(true, Ordering::AcqRel) {
                                    info!("⏳ 会话空闲超时，准备归还TTS客户端: session={}, idle_for={}s (threshold={}s)",
                                          session_id_for_idle, idle_secs, idle_timeout.as_secs());
                                    tts_controller_for_idle.return_client().await;
                                    info!("🔙 空闲归还完成: session={}", session_id_for_idle);
                                }
                            }
                            // 🔧 优化：重新计算deadline，避免立即重新触发
                            let refreshed_secs = last_idle_reset_time.load(Ordering::Acquire);
                            let refreshed_last = idle_base + std::time::Duration::from_secs(refreshed_secs);
                            let effective_base = refreshed_last
                                .checked_duration_since(now_std)
                                .map(|_| refreshed_last)
                                .unwrap_or(now_std);
                            deadline = tokio::time::Instant::from_std(effective_base + idle_timeout);
                            sleep.as_mut().reset(deadline);
                            last_processed_time = effective_base;
                        }
                    }
                }
            });
        }

        // 🔧 关键修复：设置ASR任务状态为运行中
        self.asr_task_running.store(true, Ordering::Release);

        // 创建并启动 ASR 任务 (🆕 支持音频段保存)
        let asr_language_rx = self.asr_language_tx.subscribe();
        info!("🔍 创建ASR任务，语言设置: {:?}", self.asr_language);

        // 根据 SpeechMode 决定使用哪种 ASR 任务类型
        if matches!(self.speech_mode, crate::asr::SpeechMode::VadDeferred) {
            info!("🚀 创建ASR任务（VAD延迟到StopInput）");
            let asr_language_rx = self.asr_language_tx.subscribe();
            let asr_task = AsrTaskVadDeferred {
                base: super::asr_task_base::BaseAsrTaskConfig {
                    session_id: self.session_id.clone(),
                    asr_engine: self.asr_engine.clone(),
                    emitter: emitter.clone(),
                    router: self.router.clone(),
                    input_rx: input_rx_bounded,
                    shared_flags: self.shared_flags.clone(),
                    task_completion_tx: task_completion_tx.clone(),
                    simple_interrupt_manager: self.simple_interrupt_manager.clone(),
                    simple_interrupt_handler: Some(SimpleInterruptHandler::new(
                        self.session_id.clone(),
                        "ASR-Task-Deferred".to_string(),
                        self.simple_interrupt_manager.subscribe(),
                    )),
                    cleanup_rx: asr_cleanup_rx,
                    asr_language: self.asr_language.clone(),
                    asr_language_rx: Some(asr_language_rx),
                    current_turn_response_id: self.current_turn_response_id.clone(),
                    parallel_tts_tx: Some(parallel_llm_tx),
                },
                vad_runtime_rx: Some(self.asr_vad_tx.subscribe()),
            };
            let _asr_handle = tokio::spawn(async move {
                info!("🚀 启动ASR任务（VAD延迟到StopInput）");
                if let Err(e) = asr_task.run().await {
                    error!("ASR task (deferred) failed: {}", e);
                } else {
                    info!("✅ ASR任务（deferred）成功完成");
                }
            });
        } else if matches!(self.speech_mode, crate::asr::SpeechMode::PushToTalk) {
            info!("🚀 创建ASR任务（PTT模式）");
            let asr_task = super::asr_task_ptt::AsrTaskPtt {
                base: super::asr_task_base::BaseAsrTaskConfig {
                    session_id: self.session_id.clone(),
                    asr_engine: self.asr_engine.clone(),
                    emitter: emitter.clone(),
                    router: self.router.clone(),
                    input_rx: input_rx_bounded,
                    shared_flags: self.shared_flags.clone(),
                    task_completion_tx: task_completion_tx.clone(),
                    simple_interrupt_manager: self.simple_interrupt_manager.clone(),
                    simple_interrupt_handler: Some(SimpleInterruptHandler::new(
                        self.session_id.clone(),
                        "ASR-Task-PTT".to_string(),
                        self.simple_interrupt_manager.subscribe(),
                    )),
                    cleanup_rx: asr_cleanup_rx,
                    asr_language: self.asr_language.clone(),
                    asr_language_rx: Some(asr_language_rx),
                    current_turn_response_id: self.current_turn_response_id.clone(),
                    parallel_tts_tx: Some(parallel_llm_tx),
                },
            };
            let _asr_handle = tokio::spawn(async move {
                info!("🚀 启动ASR任务（PTT）");
                if let Err(e) = asr_task.run().await {
                    error!("ASR task (PTT) failed: {}", e);
                } else {
                    info!("✅ ASR任务（PTT）成功完成");
                }
            });
        } else {
            info!("🚀 创建ASR任务（VAD模式）");
            let asr_task = super::asr_task_vad::AsrTaskVad {
                base: super::asr_task_base::BaseAsrTaskConfig {
                    session_id: self.session_id.clone(),
                    asr_engine: self.asr_engine.clone(),
                    emitter: emitter.clone(),
                    router: self.router.clone(),
                    input_rx: input_rx_bounded,
                    shared_flags: self.shared_flags.clone(),
                    task_completion_tx: task_completion_tx.clone(),
                    simple_interrupt_manager: self.simple_interrupt_manager.clone(),
                    simple_interrupt_handler: Some(SimpleInterruptHandler::new(
                        self.session_id.clone(),
                        "ASR-Task-VAD".to_string(),
                        self.simple_interrupt_manager.subscribe(),
                    )),
                    cleanup_rx: asr_cleanup_rx,
                    asr_language: self.asr_language.clone(),
                    asr_language_rx: Some(asr_language_rx),
                    current_turn_response_id: self.current_turn_response_id.clone(),
                    parallel_tts_tx: Some(parallel_llm_tx),
                },
                vad_runtime_rx: Some(self.asr_vad_tx.subscribe()),
                simultaneous_segment_config: None, // 普通对话模式不启用字数断句
            };
            let _asr_handle = tokio::spawn(async move {
                info!("🚀 启动ASR任务（VAD）");
                if let Err(e) = asr_task.run().await {
                    error!("ASR task (VAD) failed: {}", e);
                } else {
                    info!("✅ ASR任务（VAD）成功完成");
                }
            });
        }

        // 🆕 创建并启动 LLM 任务（支持多个 MCP 客户端）
        // 🔧 修复：合并 MCP 和 prompt_endpoint 的工具，避免工具丢失
        let llm_params_final = match (mcp_config_initialized, self.llm_params.clone()) {
            // 情况1: MCP配置和现有配置都存在 - 合并工具
            (Some(Some(mut mcp_params)), Some(existing_params)) => {
                // 保存原始数量用于日志
                let mcp_count = mcp_params.tools.as_ref().map(|t| t.len()).unwrap_or(0);

                // 合并工具: MCP优先，去重同名工具
                let mut merged_tools = mcp_params.tools.unwrap_or_default();

                // 收集 MCP 工具名称用于去重
                let mcp_tool_names: std::collections::HashSet<String> = merged_tools.iter().map(|t| t.function.name.clone()).collect();

                let (existing_count, dedup_count) = if let Some(existing_tools) = existing_params.tools {
                    let total = existing_tools.len();
                    let mut added = 0;
                    let mut skipped_names = Vec::new();
                    for tool in existing_tools {
                        if !mcp_tool_names.contains(&tool.function.name) {
                            merged_tools.push(tool);
                            added += 1;
                        } else {
                            skipped_names.push(tool.function.name.clone());
                        }
                    }
                    if !skipped_names.is_empty() {
                        debug!("🔧 工具去重详情: 跳过的工具名 = {:?}", skipped_names);
                    }
                    (total, total - added)
                } else {
                    (0, 0)
                };

                if dedup_count > 0 {
                    info!("🔧 工具去重: 跳过 {} 个同名工具（MCP优先）", dedup_count);
                }

                if existing_count > 0 {
                    info!(
                        "🔧 合并工具: MCP={} + prompt_endpoint={}（去重{}） = {} 个工具",
                        mcp_count,
                        existing_count,
                        dedup_count,
                        merged_tools.len()
                    );
                }

                mcp_params.tools = if merged_tools.is_empty() { None } else { Some(merged_tools) };

                // 合并 tool_choice: MCP 优先，没有则用 existing
                if mcp_params.tool_choice.is_none() {
                    mcp_params.tool_choice = existing_params.tool_choice;
                }

                Some(mcp_params)
            },
            // 情况2: 只有 MCP 配置
            (Some(Some(mcp_params)), None) => Some(mcp_params),
            // 情况3: 只有现有配置或都没有
            (Some(None), existing) | (None, existing) => existing,
        };

        let _llm_handle = {
            info!("🚀 准备创建LLM任务: session={}", self.session_id);

            // 🆕 使用 LlmTaskV2 替代 LlmTask（system_prompt 直接从 llm_client.contexts 读取）
            let llm_task = LlmTaskV2::new(
                self.session_id.clone(),
                self.llm_client.clone(),
                emitter.clone(),
                llm_params_final,
                asr_to_llm_rx,
                llm_to_tts_tx.clone(),
                self.mcp_clients.clone(),
                self.shared_flags.clone(),
                self.enable_search.clone(),
                self.simple_interrupt_manager.clone(),
            );
            info!("✅ LLM任务结构体创建成功，准备启动: session={}", self.session_id);
            let session_id = self.session_id.clone();
            tokio::spawn(async move {
                info!("🚀 LLM任务开始运行: session={}", session_id);
                if let Err(e) = llm_task.run().await {
                    error!("❌ LLM任务运行失败: session={}, error={}", session_id, e);
                } else {
                    info!("✅ LLM任务正常退出: session={}", session_id);
                }
            })
        };

        // 提前克隆用于 TTS 任务的共享字段，避免在异步任务中捕获 &self 引用
        let session_id_cloned = self.session_id.clone();
        let tts_controller_cloned = self.tts_controller.clone();
        let router_cloned = self.router.clone();
        let shared_flags_cloned = self.shared_flags.clone();
        let tts_session_created_cloned = self.tts_session_created.clone();
        let simple_interrupt_manager_cloned = self.simple_interrupt_manager.clone();

        // 创建基于管线级别 TTS 控制器的 TTS 任务
        // 复制配置数值，避免在闭包中引用 self
        let initial_burst_count_val = self.initial_burst_count;
        let initial_burst_delay_val = self.initial_burst_delay_ms;
        let send_rate_mult_val = self.send_rate_multiplier;
        let current_turn_response_id_cloned = Arc::new(super::lockfree_response_id::LockfreeResponseIdReader::from_writer(
            &self.current_turn_response_id,
        ));

        // 提前获取音频输出配置
        let initial_output_config = {
            let config_guard = self.audio_output_config.lock().await;
            config_guard.clone()
        };

        // 🆕 TtsTask builder：每次创建新实例时初始化新的通道和内部状态
        let tts_task_builder = move |receiver: broadcast::Receiver<(TurnContext, String)>| {
            // 每个 TtsTask 实例需要自己的 trigger channel
            let (next_sentence_tx, next_sentence_rx) = mpsc::unbounded_channel();

            super::tts_task::TtsTask::new(
                session_id_cloned.clone(),
                tts_controller_cloned.clone(),
                emitter.clone(),
                router_cloned.clone(),
                receiver,
                tts_session_created_cloned.clone(),
                shared_flags_cloned.clone(),
                task_completion_tx.clone(),
                simple_interrupt_manager_cloned.clone(),
                Some(SimpleInterruptHandler::new(
                    session_id_cloned.clone(),
                    "TTS-Main-Simple".to_string(),
                    simple_interrupt_manager_cloned.subscribe(),
                )),
                initial_burst_count_val,
                initial_burst_delay_val,
                send_rate_mult_val,
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(crate::text_splitter::SimplifiedStreamingSplitter::new(None))),
                initial_output_config.clone(),
                Arc::new(Mutex::new(None)),
                current_turn_response_id_cloned.clone(),
                next_sentence_tx,
                next_sentence_rx,
                false, // is_translation_mode: 非同传模式，正常处理轮次切换
            )
        };
        // 启动首个 TTS 任务
        let first_tts_task = tts_task_builder(llm_to_tts_rx);
        tokio::spawn(async move {
            if let Err(e) = first_tts_task.run().await {
                error!("TTS task failed: {}", e);
            }
        });

        // 启动任务状态监控（用于调试和日志）
        // 克隆 Sender 以在监控任务中使用
        let llm_to_tts_tx_for_monitor = llm_to_tts_tx.clone();

        let _completion_monitor = {
            let session_id = self.session_id.clone();
            let asr_task_running = self.asr_task_running.clone();
            // let _tts_controller_for_monitor = self.tts_controller.clone();
            tokio::spawn(async move {
                // 🔧 修复：添加超时和健康检查，避免无限等待（支持长时间对话）
                let mut last_activity = std::time::Instant::now();
                const MONITOR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30); // 30秒
                const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

                loop {
                    tokio::select! {
                        completion_opt = task_completion_rx.recv() => {
                            match completion_opt {
                                Some(completion) => {
                                    last_activity = std::time::Instant::now();
                                    info!("📋 任务状态更新: {:?} - session: {}", completion, session_id);

                                    // 🔧 关键修复：处理ASR任务完成信号
                                    if completion == TaskCompletion::Asr {
                                        warn!("⚠️ ASR任务异常退出: session={}", session_id);
                                        asr_task_running.store(false, Ordering::Release);
                                    }

                                    // 🆕 TTS 任务退出，重新创建 receiver 并启动新任务
                                    if completion == TaskCompletion::Tts {
                                        warn!("⚠️ TTS 任务异常退出，正在重启: session={}", session_id);
                                        // 🆕 TTS 任务退出，重新创建 receiver 并启动新任务
                                        let new_recv = llm_to_tts_tx_for_monitor.subscribe();
                                        let new_task = tts_task_builder(new_recv);
                                        tokio::spawn(async move {
                                            if let Err(e) = new_task.run().await {
                                                error!("❌ TTS 任务重启失败: {}", e);
                                            } else {
                                                info!("✅ TTS 任务重启成功");
                                            }
                                        });
                                        info!("🔄 TTS 任务重启已触发: session={}", session_id);
                                    }

                                    // 🆕 LLM 完成：结束当前 MiniMax 任务，发送 task_finish 并关闭 WS
                                    if completion == TaskCompletion::Llm {
                                        // 改为延迟回收：不立刻结束MiniMax任务，等待TTS任务自行在队列耗尽后调用 finish
                                        info!("🧾 LLM 完成，延迟回收 MiniMax：等待TTS播报完成后由TTS任务自行关闭: session={}", session_id);
                                    }

                                    // 注意：这里不退出循环，因为任务可能会持续运行并报告状态
                                },
                                None => {
                                    info!("📋 任务监控通道已关闭: {}", session_id);
                                    break;
                                }
                            }
                        },
                        _ = tokio::time::sleep(CHECK_INTERVAL) => {
                            if last_activity.elapsed() > MONITOR_TIMEOUT {
                                info!("⏰ 任务监控超时退出: {} ({}秒无活动)", session_id, MONITOR_TIMEOUT.as_secs());
                                break;
                            }
                        }
                    }
                }
                info!("📋 任务监控结束: {}", session_id);
            })
        };

        // 准备清理资源
        let session_id_cleanup = self.session_id.clone();
        let llm_client_cleanup = self.llm_client.clone();
        let tts_session_created_cleanup = self.tts_session_created.clone();
        let simple_interrupt_manager_cleanup = self.simple_interrupt_manager.clone();
        let mcp_clients_cleanup = self.mcp_clients.clone(); // 🆕 多个 MCP 客户端
        // 🔧 修复：添加ASR清理信号发送器
        let asr_cleanup_tx_cleanup = asr_cleanup_tx.clone();
        // 🆕 添加TTS控制器用于资源清理
        let tts_controller_cleanup = self.tts_controller.clone();
        // 🆕 任务作用域用于统一关闭所有异步任务
        let task_scope_cleanup = self.task_scope.clone();

        // 🔧 使用 Arc::clone 避免所有权问题
        Ok(CleanupGuard::new(move || {
            info!("🧹 开始清理 ModularPipeline 资源: {}", session_id_cleanup);

            // 🔧 修复：首先发送ASR清理信号，确保ASR任务能正确清理
            if let Err(e) = asr_cleanup_tx_cleanup.send(()) {
                warn!("⚠️ 发送ASR清理信号失败: {}", e);
            } else {
                info!("✅ 已发送ASR清理信号: {}", session_id_cleanup);
            }

            // 克隆所有需要的变量以供异步任务使用
            let session_id_async = session_id_cleanup.clone();
            let tts_session_created_async = tts_session_created_cleanup.clone();
            let simple_interrupt_manager_async = simple_interrupt_manager_cleanup.clone();
            let llm_client_async = llm_client_cleanup.clone();
            let mcp_clients_async = mcp_clients_cleanup.clone();
            let tts_controller_async = tts_controller_cleanup.clone();
            let task_scope_async = task_scope_cleanup.clone();

            // 🔧 清理 Orchestrator 资源 (精准打断机制下，不等待长时间任务完成)
            tokio::spawn(async move {
                info!("🧹 异步清理 ModularPipeline 资源: {}", session_id_async);

                // 0. 🆕 关闭任务作用域，统一终止所有被追踪的异步任务
                task_scope_async.shutdown(std::time::Duration::from_millis(100)).await;

                // 1. 🔧 清理LLM客户端上下文 (释放会话内存)
                // 说明：显式清理会话级上下文，避免多轮累积占用
                llm_client_async.cleanup_session(&session_id_async).await;
                info!("💬 LLM客户端会话上下文已清理: {}", session_id_async);

                // 2. 🆕 关键修复：清理TTS客户端，确保归还到池中
                info!("🔓 清理TTS控制器，归还客户端到全局池: session={}", session_id_async);
                tts_controller_async.return_client().await;

                // 3. 🔧 清理TTS会话（如果由orchestrator创建）
                if tts_session_created_async.load(std::sync::atomic::Ordering::Acquire) {
                    info!("🔊 TTS会话标记为已创建，将由系统自动清理");
                } else {
                    info!("🔊 TTS会话不由orchestrator创建，跳过清理");
                }

                // 4. 🆕 清理中断管理器
                // 🆕 简化机制：广播系统关闭打断信号
                let _ = simple_interrupt_manager_async.broadcast_global_interrupt(session_id_async.clone(), SimpleInterruptReason::SystemShutdown);
                info!("⚡ 中断管理器已清理: {}", session_id_async);

                // 5. 清理LLM客户端
                let _ = &llm_client_async;

                // 6. 清理TTS会话创建标志
                let _ = &tts_session_created_async;

                // 7. 🆕 清理多个MCP客户端
                // 说明：HTTP 客户端仅清理本地工具缓存；WebSocket/stdio 通过管理器释放引用计数
                for wrapper in mcp_clients_async {
                    match wrapper {
                        crate::mcp::client::McpClientWrapper::Http { client: _, config } => {
                            // HTTP MCP 工具缓存采用自然失效 + LRU，不做主动释放
                            info!("ℹ️ HTTP MCP 工具缓存采用自然失效与LRU，不主动刷新: {}", config.endpoint);
                        },
                        crate::mcp::client::McpClientWrapper::WebSocket { manager, config } => {
                            // 释放引用，配合定时清理任务回收空闲连接
                            manager.release_client(&config.endpoint).await;
                            info!("🧹 已释放WebSocket MCP客户端引用: {}", config.endpoint);
                        },
                    }
                }

                // 8. 清理会话级工具数据（工具列表 + 来源注册）
                crate::mcp::clear_session_tools(&session_id_async).await;

                info!("✅ ModularPipeline 清理完成: {}", session_id_async);
            });
        }))
    }

    async fn on_upstream(&self, payload: BinaryMessage) -> Result<()> {
        use crate::rpc::protocol::CommandId;

        // 🆕 记录会话最近一次上游活动时间（通过秒级节流）
        // 使用全局单调基线计算秒，避免使用 now.elapsed() 得到近 0 的间隔
        let base = APP_START.get_or_init(std::time::Instant::now);
        // 任何上游活动都重置一次性空闲归还标记，允许下一次空闲期再次触发
        // 仅在状态从 true -> false 时打印一次（降级为 debug），避免每个分片都刷 INFO
        if self.idle_returned_once.swap(false, Ordering::AcqRel) {
            debug!("🔄 上游活动刷新，重置空闲归还门槛: session={}", self.session_id);
        }

        // 🚀 热点优化：智能节流idle timer重置，避免每个音频分片都触发
        let now_secs = base.elapsed().as_secs();
        let last_reset_secs = self.last_idle_reset_time.load(Ordering::Acquire);

        // 🔧 关键优化：秒级节流，进一步减少通知频率
        const THROTTLE_INTERVAL_SECS: u64 = 1;
        if now_secs.saturating_sub(last_reset_secs) >= THROTTLE_INTERVAL_SECS {
            // 使用CAS确保只有一个线程执行重置，避免重复操作
            if self
                .last_idle_reset_time
                .compare_exchange_weak(last_reset_secs, now_secs, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                // 热点路径：直接通知，无需加锁
                self.idle_reset_notify.notify_one();
                // debug!(
                //     "🔄 Idle定时器节流重置: session={}, interval={}s",
                //     self.session_id,
                //     now_secs.saturating_sub(last_reset_secs)
                // );
            }
        }

        match payload.header.command_id {
            CommandId::AudioChunk => {
                let audio_bytes = payload.payload;

                // 🎯 TRACE: 音频包进入Pipeline处理
                // info!(
                //     "🎤 [TRACE-AUDIO] Pipeline收到音频包 | session_id={} | audio_bytes_len={} | 准备投递到音频入口worker",
                //     self.session_id,
                //     audio_bytes.len()
                // );

                // 🔧 直接投递到音频入口worker（使用槽位）
                if let Some(tx) = self.audio_ingress_tx_slot.get() {
                    if tx.try_send(audio_bytes).is_err() {
                        error!("🎤 [TRACE-AUDIO] 音频入口拥塞 | session_id={}", self.session_id);
                        return Err(anyhow!("音频入口拥塞"));
                    }
                } else {
                    error!("🎤 [TRACE-AUDIO] 音频入口槽位为空 | session_id={}", self.session_id);
                    return Err(anyhow!("音频入口未初始化"));
                }
            },
            CommandId::Interrupt => {
                // 🆕 用户按钮打断：停止当前 ASR/LLM/TTS 输出，但不销毁会话
                info!("🛑 收到 Interrupt（用户按钮打断）: session_id={}", self.session_id);

                // 广播全局打断：复用现有“用户说话打断”的硬打断语义（保证立即停止 TTS 播放）
                let _ = self
                    .simple_interrupt_manager
                    .broadcast_global_interrupt(self.session_id.clone(), SimpleInterruptReason::UserSpeaking);

                // 🔧 额外的即时清理：即使下游任务尚未轮询到打断事件，也尽快停掉当前音频输出
                if let Err(e) = self.tts_controller.interrupt_session().await {
                    warn!("⚠️ Interrupt期间中断TTS失败: {}", e);
                }
                // 主动中止后台等待SessionFinished，避免极端情况下的资源悬挂
                self.tts_controller.abort_finish_wait().await;
                // 清理cleanup_rx，防止收到过期的清理通知
                {
                    let mut guard = self.tts_controller.finish_session_cleanup_rx.lock().await;
                    *guard = None;
                }
            },
            CommandId::Stop => {
                // 🛑 立即标记TTS停止待决，防止预热/拉起
                self.tts_controller.set_stop_pending(true);
                info!("🛑 Orchestrator.Stop: 标记stop_pending=true");

                // 🔧 发送PTT End到统一输入通道（使用槽位）
                if let Some(tx) = self.input_tx_slot.get() {
                    let _ = tx.try_send(AsrInputMessage::PttEnd);
                } else {
                    warn!("⚠️ 输入通道槽位为空，无法发送 Stop 事件: {}", self.session_id);
                }

                // 🆕 广播全局打断，让TTS音频任务立刻停止
                let _ = self
                    .simple_interrupt_manager
                    .broadcast_global_interrupt(self.session_id.clone(), SimpleInterruptReason::UserSpeaking);

                // 🆕 取消/重置当前TTS客户端，避免残留输出
                if let Err(e) = self.tts_controller.interrupt_session().await {
                    warn!("⚠️ Stop期间中断TTS失败: {}", e);
                }
                // 🆕 关键：主动中止后台等待SessionFinished，避免阻塞销毁WS
                info!("🛑 Orchestrator.Stop: 主动中止后台finish等待");
                self.tts_controller.abort_finish_wait().await;
                self.tts_controller.reset_client().await;
                info!("🧹 Orchestrator.Stop: 已重置TTS客户端");
            },
            CommandId::StopInput => {
                // 🆕 新增：处理 StopInput 命令 - 快速结束当前语音输入
                info!("🛑 收到 StopInput 命令，快速结束当前语音输入: {}", self.session_id);

                // 发送 PTT End 事件到统一输入通道，快速结束语音输入（使用槽位）
                if let Some(tx) = self.input_tx_slot.get() {
                    if let Err(e) = tx.try_send(AsrInputMessage::PttEnd) {
                        warn!("⚠️ 发送 StopInput PTT End 事件失败: {}", e);
                    } else {
                        info!("✅ StopInput 已转换为 PTT End 事件: {}", self.session_id);
                    }
                } else {
                    warn!("⚠️ 输入通道槽位为空，无法处理 StopInput: {}", self.session_id);
                }
            },
            CommandId::TextData => {
                // 🆕 处理文本输入（文本-LLM-TTS混合模式，支持protocol_id=100+文本输入）
                if let Ok(text) = String::from_utf8(payload.payload) {
                    if text.trim().is_empty() {
                        warn!("⚠️ 收到空的文本数据: session_id={}", self.session_id);
                        return Ok(());
                    }

                    let preview = if text.chars().count() > 50 {
                        let truncated: String = text.chars().take(50).collect();
                        format!("{}...", truncated)
                    } else {
                        text.clone()
                    };
                    info!("📝 ModularPipeline收到文本输入: {}", preview);

                    // 创建 TurnContext
                    let user_item_id = format!("msg_{}", nanoid::nanoid!(6));
                    let assistant_item_id = format!("asst_{}", nanoid::nanoid!(6));
                    let response_id = format!("resp_{}", nanoid::nanoid!(8));

                    // 获取新的轮次序列号
                    let turn_sequence = self.simple_interrupt_manager.start_new_turn();
                    let ctx = super::types::TurnContext::new(user_item_id, assistant_item_id, response_id.clone(), Some(turn_sequence));

                    // 🔧 关键修复：文本路径同样需要更新当前轮次的 response_id，确保 TTS/LLM/NLU 共享同一标识
                    self.current_turn_response_id.store(Some(response_id.clone()));
                    info!(
                        "🔒 文本输入存储新的response_id到LockfreeResponseId: session={}, response_id={}",
                        self.session_id, response_id
                    );

                    // 发送到 LLM 任务
                    let direct_text_tx_guard = self.direct_text_tx.lock().await;
                    if let Some(tx) = direct_text_tx_guard.as_ref() {
                        if let Err(e) = tx.send((ctx, text)) {
                            error!("❌ 发送文本到LLM任务失败: {}", e);
                            return Err(anyhow!("发送文本到LLM任务失败"));
                        }
                        info!("✅ 文本已发送到LLM任务: response_id={}", response_id);
                    } else {
                        error!("❌ 文本输入通道未初始化: session_id={}", self.session_id);
                        return Err(anyhow!("文本输入通道未初始化"));
                    }
                } else {
                    error!("❌ 文本数据解码失败: session_id={}", self.session_id);
                    return Err(anyhow!("文本数据解码失败"));
                }
            },
            _ => {
                // 忽略其它命令
            },
        }

        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    /// 处理客户端工具调用结果
    async fn handle_tool_call_result(&self, tool_result: super::tool_call_manager::ToolCallResult) -> Result<()> {
        info!("🔧 ModularPipeline处理工具调用结果: call_id={}", tool_result.call_id);

        // 🔧 优化：减少字符串克隆，直接使用引用
        let call_id = tool_result.call_id.clone();

        // 通过工具调用管理器发送结果（携带 is_error）
        match self.shared_flags.tool_call_manager.result_tx.send(tool_result) {
            Ok(_) => {
                info!("✅ 工具调用结果已转发到LLM任务: call_id={}", call_id);
                Ok(())
            },
            Err(_) => {
                error!("❌ 转发工具调用结果失败: call_id={}", call_id);
                Err(anyhow!("转发工具调用结果失败"))
            },
        }
    }

    /// 统一会话配置热更新入口
    async fn apply_session_config(&self, payload: &crate::rpc::protocol::MessagePayload) -> Result<()> {
        use crate::rpc::protocol::MessagePayload;

        info!("🔧 [apply_session_config] 开始处理配置更新: session_id={}", self.session_id);

        if let MessagePayload::SessionConfig {
            system_prompt,
            enable_search,
            search_config,
            voice_setting,
            vad_threshold,
            silence_duration_ms,
            min_speech_duration_ms,
            initial_burst_count,
            initial_burst_delay_ms,
            send_rate_multiplier,
            mcp_server_config,
            output_audio_config,
            input_audio_config,
            text_done_signal_only,
            signal_only,
            asr_language,
            asr_chinese_convert,
            tts_chinese_convert,
            tools,
            tool_choice,
            tools_endpoint,
            prompt_endpoint,
            offline_tools,
            ..
        } = payload
        {
            info!("✅ [apply_session_config] Payload 匹配成功: SessionConfig");

            // 1) 系统提示词
            if let Some(new_prompt) = system_prompt {
                info!("🔧 [apply_session_config] 检测到 system_prompt 更新");
                self.update_system_prompt(new_prompt.clone()).await?;
            }

            // 2) 搜索配置
            info!(
                "🔍 [apply_session_config] 检查搜索配置: enable_search={:?}, has_search_config={}",
                enable_search,
                search_config.is_some()
            );
            if enable_search.is_some() || search_config.is_some() {
                let enable = enable_search.unwrap_or(false);
                info!(
                    "🔧 [apply_session_config] 准备调用 update_search_configuration: enable={}",
                    enable
                );
                self.update_search_configuration(enable, search_config.clone()).await?;
            } else {
                info!("ℹ️ [apply_session_config] 未检测到搜索配置变更");
            }

            // 3) 语音设置
            if let Some(voice_config) = voice_setting {
                info!("🔧 [apply_session_config] 检测到 voice_setting 更新");
                self.update_voice_setting(voice_config.clone()).await?;
            }

            // 4) MCP 配置
            if let Some(new_mcp) = mcp_server_config {
                info!("🔧 [apply_session_config] 检测到 mcp_server_config 更新");
                self.compare_and_update_mcp_configuration(new_mcp.clone()).await?;
            }

            // 5) 音频输出配置
            if let Some(out_cfg_val) = output_audio_config {
                match serde_json::from_value::<crate::audio::OutputAudioConfig>(out_cfg_val.clone()) {
                    Ok(out_cfg) => {
                        self.configure_output_config(out_cfg).await?;
                    },
                    Err(e) => {
                        tracing::warn!("⚠️ 解析 output_audio_config 失败: {}", e);
                    },
                }
            }

            // 6) VAD 参数（运行时应用到当前 ASR 会话）
            if vad_threshold.is_some() || silence_duration_ms.is_some() || min_speech_duration_ms.is_some() {
                let _ = self
                    .asr_vad_tx
                    .send(Some((*vad_threshold, *silence_duration_ms, *min_speech_duration_ms)));
            }

            // 7) 节拍参数
            if initial_burst_count.is_some() || initial_burst_delay_ms.is_some() || send_rate_multiplier.is_some() {
                let burst = initial_burst_count.unwrap_or(0) as usize;
                let delay = initial_burst_delay_ms.unwrap_or(5) as u64;
                let rate = send_rate_multiplier.unwrap_or(1.0);
                self.update_pacing_config(burst, delay, rate).await?;
            }

            // 8) 音频输入配置
            if let Some(in_cfg_val) = input_audio_config {
                match serde_json::from_value::<crate::audio::input_processor::AudioInputConfig>(in_cfg_val.clone()) {
                    Ok(mut cfg) => {
                        cfg.auto_correct();
                        if let Err(e) = cfg.validate() {
                            tracing::warn!("⚠️ 输入音频配置验证失败: {}，使用默认配置", e);
                            cfg = crate::audio::input_processor::AudioInputConfig::default();
                        }
                        self.configure_audio_input_config(cfg).await?;
                    },
                    Err(e) => {
                        tracing::warn!("⚠️ 解析音频输入配置失败: {}，使用默认配置", e);
                        let default_cfg = crate::audio::input_processor::AudioInputConfig::default();
                        self.configure_audio_input_config(default_cfg).await?;
                    },
                }
            }

            // 9) text_done_signal_only
            if let Some(only_signal) = *text_done_signal_only {
                self.update_text_done_signal_only(only_signal).await?;
            }

            // 9.1) signal_only
            if let Some(only) = *signal_only {
                self.update_signal_only(only).await?;
            }

            // 10) ASR 语言
            if asr_language.is_some() {
                self.update_asr_language(asr_language.clone()).await?;
            }

            // 11) ASR 繁简转换模式
            if asr_chinese_convert.is_some() {
                self.update_asr_chinese_convert_mode(asr_chinese_convert.clone()).await?;
            }

            // 11.1) TTS 繁简转换模式
            if tts_chinese_convert.is_some() {
                self.update_tts_chinese_convert_mode(tts_chinese_convert.clone()).await?;
            }

            // 12) 离线工具列表
            if let Some(tools_list) = offline_tools {
                info!("🔧 [apply_session_config] 检测到 offline_tools 更新: {:?}", tools_list);
                crate::agents::turn_tracker::set_session_offline_tools(&self.session_id, tools_list.clone()).await;
            }

            // 13) 工具 & 端点（会中刷新）
            // 13.1 tools_endpoint 异步加载
            if let Some(endpoint) = tools_endpoint.clone() {
                info!("🔧 运行时加载 tools_endpoint: {}", endpoint);
                let async_manager = crate::mcp::get_global_async_tools_manager();
                if let Err(e) = async_manager.start_async_tools_loading(endpoint, self.session_id.clone()).await {
                    tracing::warn!("⚠️ 异步工具加载失败: {}", e);
                }
            }

            // 13.2 mcp/tools/tool_choice 合并（仅更新内部 llm_params；当前 LLM 任务可能在下一轮读取）
            if tools.is_some() || tool_choice.is_some() {
                let mut params = self.llm_params.clone().unwrap_or_default();
                if let Some(tools_val) = tools.clone()
                    && let Ok(parsed_tools) = serde_json::from_value::<Vec<crate::llm::llm::Tool>>(serde_json::Value::Array(tools_val.clone()))
                {
                    params.tools = Some(parsed_tools);
                }
                if let Some(choice_val) = tool_choice.clone()
                    && let Ok(choice) = serde_json::from_value::<crate::llm::llm::ToolChoice>(choice_val.clone())
                {
                    params.tool_choice = Some(choice);
                }
                // 由于当前方法签名为 &self，直接写 self.llm_params 不安全；
                // 将在下一轮 LLM 任务创建时读取（通过其他事件触发）。
                info!(
                    "✅ 解析 LLM 工具/选择策略 (会中)，将在下一轮任务创建时应用: tools={}, choice={}",
                    params.tools.as_ref().map(|v| v.len()).unwrap_or(0),
                    params.tool_choice.is_some()
                );
            }

            // 13.3 prompt_endpoint：获取远程配置并合并应用
            if let Some(endpoint) = prompt_endpoint.clone() {
                info!("🌐 [apply_session_config] 运行时获取三合一远程配置: {}", endpoint);
                let client = crate::rpc::remote_config::get_global_remote_config_client();
                match client.get_config(&endpoint).await {
                    Ok(rc) => {
                        info!("🌐 [apply_session_config] 远程配置获取成功，开始应用");
                        if let Some(sp) = rc.system_prompt.clone() {
                            info!("🔧 [apply_session_config] 远程配置包含 system_prompt");
                            self.update_system_prompt(sp).await?;
                        }
                        if let Some(mcp_cfg) = rc.mcp_server_config.clone() {
                            info!("🔧 [apply_session_config] 远程配置包含 mcp_server_config");
                            self.compare_and_update_mcp_configuration(mcp_cfg).await?;
                        }
                        // 🆕 处理 prompt_endpoint 返回的 tools（合并到 SESSION_TOOLS）
                        if let Some(tools_val) = rc.tools.clone() {
                            info!("🔧 [apply_session_config] 远程配置包含 tools: {} 个", tools_val.len());
                            // 解析为 LLM Tool 格式
                            if let Ok(parsed_tools) = serde_json::from_value::<Vec<crate::llm::llm::Tool>>(serde_json::Value::Array(tools_val)) {
                                // 合并到会话工具缓存（不覆盖已有工具）
                                let added = crate::mcp::merge_session_tools(&self.session_id, parsed_tools.clone()).await;
                                // 注册工具来源
                                crate::mcp::register_tool_sources(&self.session_id, &parsed_tools, crate::mcp::ToolSourceType::PromptEndpoint).await;
                                info!("✅ [apply_session_config] prompt_endpoint 工具已合并: 新增 {} 个", added);
                            } else {
                                tracing::warn!("⚠️ [apply_session_config] 解析 prompt_endpoint tools 失败");
                            }
                        }
                        if let Some(sc) = rc.search_config.clone() {
                            info!("🔧 [apply_session_config] 远程配置包含 search_config，默认启用搜索");
                            // 默认启用搜索
                            self.update_search_configuration(true, Some(sc)).await?;
                        } else {
                            info!("ℹ️ [apply_session_config] 远程配置不包含 search_config");
                        }
                        // 🆕 处理 prompt_endpoint 返回的 offline_tools
                        if let Some(tools_list) = rc.offline_tools.clone() {
                            info!("🔧 [apply_session_config] 远程配置包含 offline_tools: {:?}", tools_list);
                            crate::agents::turn_tracker::set_session_offline_tools(&self.session_id, tools_list).await;
                        } else {
                            info!("ℹ️ [apply_session_config] 远程配置不包含 offline_tools");
                        }
                        info!("✅ [apply_session_config] 运行时远程配置已应用");
                    },
                    Err(e) => {
                        tracing::warn!("⚠️ [apply_session_config] 获取远程配置失败（保持现有配置）: {}", e);
                    },
                }
            } else {
                info!("ℹ️ [apply_session_config] 未检测到 prompt_endpoint");
            }

            info!("✅ [apply_session_config] 配置更新处理完成: session_id={}", self.session_id);
        } else {
            warn!(
                "⚠️ [apply_session_config] Payload 类型不匹配，期望 SessionConfig，实际收到其他类型: session_id={}",
                self.session_id
            );
        }

        Ok(())
    }
}

// Config and state methods are in config.rs and state.rs
