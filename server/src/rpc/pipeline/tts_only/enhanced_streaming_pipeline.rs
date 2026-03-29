//! 增强版TTS-Only流式管线
//!
//! 基于`enhanced_text_processor.rs`提供的高级文本处理能力，提供完整的流式TTS服务。
//! 主要功能：
//! - 📡 WebSocket连接管理
//! - 🔄 连接预热和健康检查
//! - 📝 流式文本输入处理
//! - 🎵 实时音频输出
//! - ⏰ 智能超时管理
//! - 🛑 优雅的停止和清理机制
//! - 🔧 连接回收和重用（可选）

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, broadcast, mpsc, watch};
use tracing::{debug, error, info, warn};

use crate::audio::{OpusEncoderConfig, OutputAudioConfig};
use crate::rpc::{
    pipeline::{CleanupGuard, StreamingPipeline},
    protocol::{BinaryMessage, CommandId},
    session_router::SessionRouter,
};
use crate::tts::minimax::{MiniMaxConfig, VoiceSetting};

use super::config::{TtsInputTimeout, TtsProcessorConfig};
// 🔧 迁移：导入新的简化打断机制
use super::super::asr_llm_tts::simple_interrupt_manager::{InterruptReason as SimpleInterruptReason, SimpleInterruptManager};

// 🆕 新增：导入TTS池管理模块

use super::super::asr_llm_tts::event_emitter::EventEmitter;
use super::super::asr_llm_tts::tts_task::TtsController;
use super::super::asr_llm_tts::types::{SharedFlags, TurnContext};

/// 连接状态概览（与orchestrator.rs保持一致）
#[derive(Debug, Clone)]
pub struct TtsConnectionStatus {
    /// TTS连接是否已预热
    pub tts_preheated: bool,
    /// 是否有预热会话可用
    pub preheat_session_available: bool,
    /// 会话ID
    pub session_id: String,
    /// 预热耗时
    pub preheat_duration_ms: Option<u64>,
    /// 连接是否健康
    pub connection_healthy: bool,
}

/// 🆕 回收状态管理
#[derive(Debug, Clone)]
pub struct RecycleStatus {
    /// 是否启用自动回收
    pub auto_recycle_enabled: bool,
    /// WebSocket连接是否断开
    pub websocket_disconnected: bool,
    /// 音频播放是否完成
    pub audio_playback_finished: bool,
    /// 最后一次活动时间
    pub last_activity: Instant,
    /// 空闲超时时长
    pub idle_timeout: Duration,
}

/// 🆕 TTS优化状态报告（用于调试首音频延迟优化效果）
#[derive(Debug, Clone)]
pub struct OptimizationStatus {
    /// 连接是否已预热
    pub connection_preheated: bool,
    /// 预热会话是否可用
    pub preheat_session_available: bool,
    /// 是否应用了语音配置
    pub voice_config_applied: bool,
    /// 会话ID
    pub session_id: String,
    /// 优化级别描述
    pub optimization_level: String,
}

impl RecycleStatus {
    fn new() -> Self {
        Self {
            auto_recycle_enabled: true,
            websocket_disconnected: false,
            audio_playback_finished: false,
            last_activity: Instant::now(),
            idle_timeout: Duration::from_secs(300), // 5分钟空闲超时
        }
    }

    /// 更新活动时间
    fn update_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    /// 检查是否应该回收
    pub fn should_recycle(&self) -> bool {
        if !self.auto_recycle_enabled {
            return false;
        }

        // WebSocket断开时立即回收
        if self.websocket_disconnected {
            return true;
        }

        // 音频播放完成且空闲超时时回收
        if self.audio_playback_finished && self.last_activity.elapsed() > self.idle_timeout {
            return true;
        }

        // 长时间空闲时回收
        self.last_activity.elapsed() > self.idle_timeout
    }
}

/// 增强版流式TTS-Only管线
#[allow(clippy::type_complexity)]
pub struct EnhancedStreamingTtsOnlyPipeline {
    /// 会话ID
    pub session_id: String,
    /// 会话路由器
    pub router: Arc<SessionRouter>,
    /// 文本输入通道
    pub text_tx: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    /// 输入超时重置通道
    pub input_timeout_tx: Arc<Mutex<Option<watch::Sender<bool>>>>,
    /// 配置
    pub config: TtsProcessorConfig,
    /// 语音设置
    pub voice_setting: Option<VoiceSetting>,
    /// 打断管理器
    pub interrupt_manager: Arc<SimpleInterruptManager>,
    /// 回收状态
    pub recycle_status: Arc<Mutex<RecycleStatus>>,
    /// 清理守卫
    pub cleanup_guard: Arc<Mutex<Option<CleanupGuard>>>,
    /// 🆕 新增：TTS控制器
    pub tts_controller: Arc<TtsController>,
    /// 🆕 新增：运行状态标志
    pub is_running: Arc<AtomicBool>,
    /// 🆕 新增：TTS会话创建标志
    pub tts_session_created: Arc<AtomicBool>,
    /// 🆕 新增：TTS预连接结果
    pub tts_preconnection_result: Arc<Mutex<Option<Result<(), String>>>>,
    /// 🆕 新增：连接状态
    pub status: Arc<Mutex<TtsConnectionStatus>>,
    /// 🆕 新增：Pipeline级别的当前轮次 response_id（可写）
    pub current_turn_response_id: Arc<crate::rpc::pipeline::asr_llm_tts::LockfreeResponseId>,
    /// 🆕 新增：繁简转换模式（支持运行时更新）
    pub tts_chinese_convert_mode: std::sync::RwLock<crate::text_filters::ConvertMode>,
    /// 🆕 新增：信令控制标志（供 emitter 共享）
    pub text_done_signal_only_flag: Arc<std::sync::atomic::AtomicBool>,
    pub signal_only_flag: Arc<std::sync::atomic::AtomicBool>,
    /// 🆕 统一TTS任务：LLM→TTS 广播发送端（供 on_upstream 发送文本）
    pub llm_to_tts_tx: Arc<Mutex<Option<broadcast::Sender<(TurnContext, String)>>>>,
    /// 🆕 统一TTS任务句柄
    pub tts_task_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// 🆕 事件发射器（与 orchestrator 一致）
    pub emitter: Arc<EventEmitter>,
    /// 🆕 共享标志（与 orchestrator 一致）
    pub shared_flags: Arc<SharedFlags>,
    /// 🆕 是否已收到客户端 StopInput 信号
    pub stop_input_received: Arc<AtomicBool>,
}

impl EnhancedStreamingTtsOnlyPipeline {
    /// 创建增强版TTS-Only管线
    pub fn new(
        session_id: String,
        router: Arc<SessionRouter>,
        tts_config: Option<MiniMaxConfig>,
        voice_setting: Option<VoiceSetting>,
        input_timeout_config: Option<TtsInputTimeout>,
        chinese_convert: Option<String>,
    ) -> Self {
        info!("🚀 创建增强版流式TTS-Only Pipeline: {}", session_id);

        // 🔧 优化：减少字符串克隆，使用引用
        if let Some(ref vs) = voice_setting {
            info!(
                "🎙️ TTS语音设置: voice_id={:?}, speed={:?}, vol={:?}",
                vs.voice_id, vs.speed, vs.vol
            );
        } else {
            info!("🎙️ 使用默认TTS语音设置");
        }

        // 🔧 使用默认配置，自动同步控速参数
        let config = TtsProcessorConfig { input_timeout: input_timeout_config.clone(), ..TtsProcessorConfig::default() };

        // 🔧 修复：克隆voice_setting在移动到结构体之前
        let voice_setting_clone = voice_setting.clone();

        // 🆕 创建 TTS 控制器 - 只创建一次
        let tts_controller = Arc::new(TtsController::new(tts_config.clone(), voice_setting.clone()));

        // 🆕 创建打断管理器
        let interrupt_manager = Arc::new(SimpleInterruptManager::new());

        // 🆕 创建回收状态
        let recycle_status = Arc::new(Mutex::new(RecycleStatus::new()));

        // 🆕 创建清理守卫
        let cleanup_guard = Arc::new(Mutex::new(None));

        let emitter = Arc::new(EventEmitter::new(
            router.clone(),
            session_id.clone(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ));

        let pipeline = Self {
            session_id: session_id.clone(),
            router: router.clone(),
            text_tx: Arc::new(Mutex::new(None)),
            input_timeout_tx: Arc::new(Mutex::new(None)),
            config,
            voice_setting: voice_setting_clone,
            interrupt_manager,
            recycle_status,
            cleanup_guard,
            tts_controller, // 使用上面创建的tts_controller
            is_running: Arc::new(AtomicBool::new(false)),
            tts_session_created: Arc::new(AtomicBool::new(false)),
            tts_preconnection_result: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(TtsConnectionStatus {
                tts_preheated: false,
                preheat_session_available: false,
                session_id: String::new(),
                preheat_duration_ms: None,
                connection_healthy: false,
            })),
            current_turn_response_id: Arc::new(crate::rpc::pipeline::asr_llm_tts::LockfreeResponseId::new()),
            tts_chinese_convert_mode: std::sync::RwLock::new(
                chinese_convert
                    .as_deref()
                    .map(crate::text_filters::ConvertMode::from)
                    .unwrap_or(crate::text_filters::ConvertMode::None),
            ),
            text_done_signal_only_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            signal_only_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            llm_to_tts_tx: Arc::new(Mutex::new(None)),
            tts_task_handle: Arc::new(Mutex::new(None)),
            emitter,
            shared_flags: Arc::new(SharedFlags::new()),
            stop_input_received: Arc::new(AtomicBool::new(false)),
        };

        info!("✅ 增强版TTS-Only Pipeline创建完成，预热正在后台进行: {}", pipeline.session_id);
        pipeline
    }

    // 旧的全局池入口已移除：MiniMax按需创建与会话内复用

    /// 使用MiniMax后端创建管线
    pub fn new_with_minimax(session_id: String, router: Arc<SessionRouter>, tts_config: Option<MiniMaxConfig>, voice_setting: Option<VoiceSetting>) -> Self {
        // 默认5分钟输入超时
        let input_timeout = Some(TtsInputTimeout {
            timeout_duration: Duration::from_secs(300),
            enable_warning: true,
            warning_interval: Duration::from_secs(60),
        });

        Self::new(session_id, router, tts_config.clone(), voice_setting, input_timeout, None)
    }

    /// 🆕 新增：创建TTS配置（用于回退模式）
    #[allow(dead_code)]
    async fn create_tts_config(&self) -> Option<MiniMaxConfig> {
        // 使用默认配置（从环境变量读取）
        let config = MiniMaxConfig::default();

        Some(config)
    }

    /// 自定义配置创建管线
    pub fn new_with_config(
        session_id: &str,
        router: Arc<SessionRouter>,
        tts_config: Option<MiniMaxConfig>,
        config: TtsProcessorConfig,
        voice_setting: Option<VoiceSetting>,
        chinese_convert: Option<String>,
    ) -> Self {
        let mut pipeline = Self::new(
            session_id.to_string(),
            router,
            tts_config,
            voice_setting,
            config.input_timeout.clone(),
            chinese_convert,
        );
        pipeline.config = config;
        pipeline
    }

    /// 🆕 新增：支持音频输出配置的创建函数
    pub fn new_with_audio_config(
        session_id: String,
        router: Arc<SessionRouter>,
        tts_config: Option<MiniMaxConfig>,
        voice_setting: Option<VoiceSetting>,
        output_audio_config: OutputAudioConfig,
        chinese_convert: Option<String>,
    ) -> Self {
        info!("🚀 创建支持音频输出配置的增强版流式TTS-Only Pipeline: {}", session_id);
        info!("🎵 音频输出配置: {:?}", output_audio_config);

        // 创建包含音频输出配置的处理器配置
        let config = TtsProcessorConfig { output_audio_config: Some(output_audio_config), ..Default::default() };

        Self::new_with_config(&session_id, router, tts_config, config, voice_setting, chinese_convert)
    }

    /// 🆕 获取TTS预热状态（参考orchestrator.rs）
    pub async fn get_tts_preconnection_status(&self) -> Option<Result<(), String>> {
        let result_guard = self.tts_preconnection_result.lock().await;
        result_guard.clone()
    }

    /// 🆕 等待TTS预热完成（带超时）
    pub async fn wait_for_tts_preconnection(&self, timeout_ms: u64) -> Result<(), String> {
        let timeout = Duration::from_millis(timeout_ms);
        let start_time = Instant::now();

        loop {
            // 检查预热结果
            {
                let result_guard = self.tts_preconnection_result.lock().await;
                if let Some(result) = result_guard.as_ref() {
                    return result.clone();
                }
            }

            // 检查超时
            if start_time.elapsed() >= timeout {
                return Err("TTS预热等待超时".to_string()); // 🔧 简化错误消息
            }

            // 短暂等待后重试
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// 🆕 等待预热会话就绪（带超时）- 参考orchestrator.rs的优化检查
    pub async fn wait_for_preheat_session_ready(&self, timeout_ms: u64) -> Result<bool, String> {
        let timeout = Duration::from_millis(timeout_ms);
        let start_time = Instant::now();

        loop {
            // 首先确保连接预热完成
            if let Some(Err(e)) = self.get_tts_preconnection_status().await {
                return Err(format!("TTS连接预热失败: {}", e));
            }

            // 检查预热会话是否可用
            let has_preheat = self.tts_controller.has_preheat_session_available().await;
            if has_preheat {
                return Ok(true);
            }

            // 检查超时
            if start_time.elapsed() >= timeout {
                return Ok(false); // 超时但不是错误，可能预热会话创建稍慢
            }

            // 短暂等待后重试
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// 🆕 获取连接状态概览
    pub async fn get_connection_status(&self) -> TtsConnectionStatus {
        let preheated = self.get_tts_preconnection_status().await;
        let is_preheated = preheated.as_ref().is_some_and(|r| r.is_ok());
        let connection_healthy = self.tts_controller.is_connection_healthy().await;

        let preheat_session_available = self.tts_controller.has_preheat_session_available().await;
        TtsConnectionStatus {
            tts_preheated: is_preheated,
            preheat_session_available,
            session_id: self.session_id.clone(),
            preheat_duration_ms: None, // 可以扩展记录预热耗时
            connection_healthy,
        }
    }

    /// 🆕 获取回收状态
    pub async fn get_recycle_status(&self) -> RecycleStatus {
        let status_guard = self.recycle_status.lock().await;
        status_guard.clone()
    }

    /// 🆕 更新回收状态
    async fn update_recycle_status<F>(&self, updater: F)
    where
        F: FnOnce(&mut RecycleStatus),
    {
        let mut status_guard = self.recycle_status.lock().await;
        updater(&mut status_guard);
    }

    /// 🆕 启动连接监控任务
    #[allow(dead_code)]
    async fn start_connection_monitor(&self) -> Result<()> {
        let tts_controller = self.tts_controller.clone();
        let session_id = self.session_id.clone();
        let is_running = self.is_running.clone();
        let recycle_status = self.recycle_status.clone();
        let cleanup_guard = self.cleanup_guard.clone();
        let tts_session_created = self.tts_session_created.clone();

        tokio::spawn(async move {
            info!("🔧 启动连接健康监控: session={}", session_id);

            while is_running.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_secs(30)).await;

                // 🔧 修复：只在有活跃TTS会话时检查连接健康
                // 避免在会话正常结束后报告连接断开为错误
                if !tts_session_created.load(Ordering::Acquire) {
                    debug!("📊 跳过连接健康检查：无活跃TTS会话 session={}", session_id);
                    continue;
                }

                let connection_healthy = tts_controller.is_connection_healthy().await;

                if !connection_healthy {
                    warn!("⚠️ 检测到TTS连接不健康: session={}", session_id);

                    // 更新回收状态
                    {
                        let mut status_guard = recycle_status.lock().await;
                        status_guard.websocket_disconnected = true;
                        info!("🔌 标记WebSocket连接断开，将触发回收: session={}", session_id);
                    }

                    // 触发自动回收
                    Self::trigger_auto_recycle(cleanup_guard.clone(), session_id.clone()).await;
                    break;
                }
            }

            info!("🔚 连接健康监控结束: session={}", session_id);
        });

        Ok(())
    }

    /// 🆕 启动音频播放完成监控（监控TTS引擎事件 + PacedSender缓冲区）
    #[allow(dead_code)]
    async fn start_audio_completion_monitor(&self) -> Result<()> {
        let tts_controller = self.tts_controller.clone();
        let session_id = self.session_id.clone();
        let is_running = self.is_running.clone();
        let recycle_status = self.recycle_status.clone();
        // let cleanup_guard = self.cleanup_guard.clone();

        tokio::spawn(async move {
            info!("🎵 启动增强版音频播放完成监控: session={}", session_id);

            // 创建音频接收器监听播放完成事件（广播模式）
            match tts_controller.subscribe_audio().await {
                Ok(mut audio_rx) => {
                    let mut tts_engine_finished = false;

                    while is_running.load(Ordering::Acquire) {
                        match audio_rx.recv().await {
                            Ok(chunk) => {
                                // 检查TTS引擎完成信号（MiniMax: 任务完成以 is_final && sequence_id==u64::MAX 判定）
                                if chunk.is_final && chunk.sequence_id == u64::MAX {
                                    info!("🏁 检测到TTS任务完成(final+MAX) (session={})", session_id);
                                    tts_engine_finished = true;

                                    // 标记会话结束
                                    info!("✅ 已标记TTS会话结束，连接健康监控将停止: session={}", session_id);
                                }

                                // 如果TTS引擎已完成，标记状态（回收由 TtsTask 完成信号驱动）
                                if tts_engine_finished {
                                    let mut status_guard = recycle_status.lock().await;
                                    status_guard.audio_playback_finished = true;
                                    status_guard.update_activity();
                                }

                                // 如果是正常的音频块，更新活动时间
                                if !chunk.data.is_empty() {
                                    let mut status_guard = recycle_status.lock().await;
                                    status_guard.update_activity();
                                }
                            },
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                info!("🔇 音频广播通道关闭: session={}", session_id);
                                break;
                            },
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                warn!("⚠️ 音频处理滞后 {} 条消息，继续处理", n);
                                continue;
                            },
                        }
                    }
                },
                Err(e) => {
                    warn!("⚠️ 无法订阅音频流，跳过音频播放完成监控: session={}, error={:?}", session_id, e);
                },
            }

            info!("🔚 音频播放完成监控结束: session={}", session_id);
        });

        Ok(())
    }

    /// 🆕 触发自动回收
    async fn trigger_auto_recycle(cleanup_guard: Arc<Mutex<Option<CleanupGuard>>>, session_id: String) {
        info!("🧹 触发自动回收: session={}", session_id);

        let mut guard_option = cleanup_guard.lock().await;
        if let Some(guard) = guard_option.take() {
            info!("🔄 执行清理操作: session={}", session_id);
            drop(guard); // 触发清理
            info!("✅ 自动回收完成: session={}", session_id);
        } else {
            info!("📝 清理防护已不存在，可能已被回收: session={}", session_id);
        }
    }

    /// 获取当前响应ID
    pub async fn get_current_response_id(&self) -> Option<String> {
        self.current_turn_response_id.load()
    }

    /// 🆕 触发输入超时重置
    pub async fn reset_input_timeout(&self) -> Result<()> {
        // 更新活动时间
        self.update_recycle_status(|status| status.update_activity()).await;

        let timeout_tx_guard = self.input_timeout_tx.lock().await;
        if let Some(tx) = timeout_tx_guard.as_ref() {
            if tx.send(true).is_err() {
                return Err(anyhow!("超时信号通道已关闭"));
            }
            debug!("✅ 输入超时已重置: session={}", self.session_id);
        }
        Ok(())
    }

    /// 停止文本输入（保持会话活跃）
    pub async fn stop_text_input(&self) {
        info!("🛑 停止文本输入: session={}", self.session_id);

        // 🔧 关键修复：不立即调用processor.stop_input_only()
        // 而是发送特殊停止信号到文本通道，让所有已发送的文本先被处理
        let text_tx_guard = self.text_tx.lock().await;
        if let Some(tx) = text_tx_guard.as_ref() {
            // 发送停止输入信号到通道，这样text_receiver会在处理完所有已有文本后再设置停止标志
            if let Err(e) = tx.send("__STOP_INPUT__".to_string()) {
                warn!("发送停止输入信号失败: {}", e);
            } else {
                info!("✅ 已发送停止输入信号到文本通道，等待现有文本处理完成");
            }
        } else {
            warn!("文本通道未初始化，透传 StopInput 到 TtsTask");
            if let Some(tx) = self.llm_to_tts_tx.lock().await.as_ref()
                && let Some(resp_id) = self.current_turn_response_id.load()
            {
                let ctx = TurnContext::new("user_".to_string(), format!("asst_{}", nanoid::nanoid!(6)), resp_id, Some(1));
                let _ = tx.send((ctx, "__TURN_COMPLETE__".to_string()));
            }
        }
    }

    /// 停止整个处理器
    pub async fn stop_processor(&self) {
        info!("🔌 停止整个处理器: session={}", self.session_id);
        // 广播中断并归还客户端
        let _ = self
            .interrupt_manager
            .broadcast_global_interrupt(self.session_id.clone(), SimpleInterruptReason::UserSpeaking);
        self.tts_controller.return_client().await;
    }
}

#[async_trait]
impl StreamingPipeline for EnhancedStreamingTtsOnlyPipeline {
    async fn start(&self) -> Result<CleanupGuard> {
        info!("🚀 启动增强版TTS-Only Pipeline: {}", self.session_id);

        // 🆕 配置音频输出配置到TTS控制器（与orchestrator.rs保持一致）
        // 🔧 优化：TTS-only pipeline用后即弃，在session启动时一次性配置
        if let Some(ref audio_config) = self.config.output_audio_config {
            info!("🎵 配置TTS音频输出配置: {:?}", audio_config);
            self.tts_controller.configure_output_config(audio_config.clone()).await?;
        } else {
            info!("🎵 使用默认PCM音频输出配置");
            let default_config = OutputAudioConfig::default_pcm(20);
            self.tts_controller.configure_output_config(default_config).await?;
        }

        // 🆕 统一接入 TtsTask（与 orchestrator.rs 一致）
        let (llm_to_tts_tx, llm_to_tts_rx) = broadcast::channel::<(TurnContext, String)>(100);
        {
            let mut g = self.llm_to_tts_tx.lock().await;
            *g = Some(llm_to_tts_tx);
        }

        // 任务完成通道（TtsTask 需要一个发送端）
        let (task_completion_tx, mut task_completion_rx) = mpsc::unbounded_channel();

        // 文本分割器与首块记录标志
        let text_splitter_first_chunk_recorded = Arc::new(AtomicBool::new(false));
        let text_splitter = Arc::new(Mutex::new(crate::text_splitter::SimplifiedStreamingSplitter::new(None)));

        // 初始音频输出配置
        let initial_output_config = if let Some(ref cfg) = self.config.output_audio_config {
            cfg.clone()
        } else {
            OutputAudioConfig::default_pcm(20)
        };

        // 下一句触发通道
        let (next_sentence_tx, next_sentence_rx) = mpsc::unbounded_channel();

        let current_turn_response_id_reader = Arc::new(super::super::asr_llm_tts::lockfree_response_id::LockfreeResponseIdReader::from_writer(&self.current_turn_response_id));

        let tts_task = super::super::asr_llm_tts::tts_task::TtsTask::new(
            self.session_id.clone(),
            self.tts_controller.clone(),
            self.emitter.clone(),
            self.router.clone(),
            llm_to_tts_rx,
            self.tts_session_created.clone(),
            self.shared_flags.clone(),
            task_completion_tx,
            self.interrupt_manager.clone(),
            Some(
                super::super::asr_llm_tts::simple_interrupt_manager::SimpleInterruptHandler::new(
                    self.session_id.clone(),
                    "TTS-Only-Main".to_string(),
                    self.interrupt_manager.subscribe(),
                ),
            ),
            self.config.initial_burst_count,
            self.config.initial_burst_delay_ms,
            self.config.send_rate_multiplier,
            text_splitter_first_chunk_recorded,
            text_splitter,
            initial_output_config,
            Arc::new(Mutex::new(None)),
            current_turn_response_id_reader,
            next_sentence_tx,
            next_sentence_rx,
            false, // is_translation_mode: 非同传模式
        );

        let handle = tokio::spawn(async move {
            if let Err(e) = tts_task.run().await {
                error!("TTS-Only TtsTask failed: {}", e);
            }
        });
        {
            let mut h = self.tts_task_handle.lock().await;
            *h = Some(handle);
        }

        // 更新状态
        {
            let mut status = self.status.lock().await;
            status.connection_healthy = true;
        }

        // 🆕 等待 TtsTask 完成信号后再回收，确保已播放完毕
        {
            let recycle_status = self.recycle_status.clone();
            let cleanup_guard = self.cleanup_guard.clone();
            let session_id = self.session_id.clone();
            let stop_flag = self.stop_input_received.clone();
            tokio::spawn(async move {
                while let Some(completion) = task_completion_rx.recv().await {
                    if matches!(completion, super::super::asr_llm_tts::types::TaskCompletion::Tts) {
                        {
                            let mut status_guard = recycle_status.lock().await;
                            status_guard.audio_playback_finished = true;
                            status_guard.update_activity();
                        }
                        // 等待客户端 StopInput 信号再回收
                        if stop_flag.load(Ordering::Acquire) {
                            Self::trigger_auto_recycle(cleanup_guard.clone(), session_id.clone()).await;
                            break;
                        } else {
                            loop {
                                if stop_flag.load(Ordering::Acquire) {
                                    Self::trigger_auto_recycle(cleanup_guard.clone(), session_id.clone()).await;
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(50)).await;
                            }
                            break;
                        }
                    }
                }
            });
        }

        // 🔧 创建清理守卫 - 采用完全owned的方式，避免任何引用
        let session_id_for_cleanup = self.session_id.clone();
        let router_for_cleanup = self.router.clone();
        let tts_controller_for_cleanup = self.tts_controller.clone();

        let cleanup_guard = CleanupGuard::new(move || {
            info!("🧹 增强版TTS-Only Pipeline清理开始: {}", session_id_for_cleanup);

            // 所有变量都是owned的，没有引用
            let session_id_async = session_id_for_cleanup.clone();
            let router_async = router_for_cleanup.clone();
            let tts_ctrl_async = tts_controller_for_cleanup.clone();

            tokio::spawn(async move {
                // 清理时确保TTS客户端被归还
                info!("🔓 清理时归还TTS客户端到全局池: session={}", session_id_async);
                tts_ctrl_async.return_client().await;

                router_async.remove_session(&session_id_async).await;
                info!("🧹 增强版TTS-Only Pipeline清理完成: {}", session_id_async);
            });
        });

        info!("✅ 增强版TTS-Only Pipeline启动完成(统一TtsTask): {}", self.session_id);
        Ok(cleanup_guard)
    }

    async fn on_upstream(&self, payload: BinaryMessage) -> Result<()> {
        // 更新活动时间
        self.update_recycle_status(|status| status.update_activity()).await;

        match payload.header.command_id {
            CommandId::TextData => {
                // 🔧 优化：减少字符串分配，直接使用 UTF-8 验证
                let text = match String::from_utf8(payload.payload) {
                    Ok(t) => t,
                    Err(_) => return Err(anyhow!("文本解码失败")), // 🔧 简化错误消息
                };

                if text.trim().is_empty() {
                    return Err(anyhow!("接收到空文本数据"));
                }

                info!("📝 增强版TTS-Only接收文本: {}", text.chars().take(50).collect::<String>());

                // 重置输入超时（沿用原机制）
                if (self.reset_input_timeout().await).is_err() {
                    warn!("⚠️ 重置输入超时失败");
                }

                // 构造/获取本轮 TurnContext
                let (ctx, is_new_turn) = {
                    let existing = self.current_turn_response_id.load();
                    let is_new = existing.is_none();
                    let response_id = existing.clone().unwrap_or_else(|| {
                        let rid = format!("resp_{}", nanoid::nanoid!(8));
                        self.current_turn_response_id.store(Some(rid.clone()));
                        rid
                    });
                    let assistant_item_id = format!("asst_{}", nanoid::nanoid!(6));
                    (
                        TurnContext::new("user_".to_string(), assistant_item_id, response_id, Some(1)),
                        is_new,
                    )
                };

                // 新轮次：发送 response.created
                if is_new_turn {
                    self.emitter.response_created(&ctx).await;
                    info!("📤 已发送 response.created: response_id={}", ctx.response_id);
                }

                // 发送 (ctx, text) 给统一 TtsTask（先做繁简转换）
                let maybe_tx = { self.llm_to_tts_tx.lock().await.clone() };
                if let Some(tx) = maybe_tx {
                    let mode = *self.tts_chinese_convert_mode.read().unwrap();
                    let filtered = crate::text_filters::filter_for_tts(&text, mode);
                    let _ = tx.send((ctx, filtered));
                } else {
                    return Err(anyhow!("TTS任务未初始化"));
                }
            },
            CommandId::Start => {
                info!("📨 增强版TTS-Only接收Start命令");
                // 重置输入超时
                if (self.reset_input_timeout().await).is_err() {
                    warn!("⚠️ 重置输入超时失败");
                }
                // 🧹 清除任何遗留的停止待决标志，允许本次会话继续
                self.tts_controller.set_stop_pending(false);
                // 重置 StopInput 标志
                self.stop_input_received.store(false, Ordering::Release);
            },
            CommandId::Interrupt => {
                // 🆕 用户按钮打断：停止当前输出，但不销毁会话
                info!("🛑 增强版TTS-Only接收Interrupt命令（按钮打断）: {}", self.session_id);

                // 广播打断：复用 UserSpeaking 的硬打断语义，确保立即停播放
                let _ = self
                    .interrupt_manager
                    .broadcast_global_interrupt(self.session_id.clone(), SimpleInterruptReason::UserSpeaking);

                // 即时停止当前音频输出（不设置 stop_pending，允许后续继续输入）
                if let Err(e) = self.tts_controller.interrupt_session().await {
                    warn!("⚠️ Interrupt期间中断TTS失败: {}", e);
                }
                // 中止后台等待（若存在）
                self.tts_controller.abort_finish_wait().await;
                {
                    let mut guard = self.tts_controller.finish_session_cleanup_rx.lock().await;
                    *guard = None;
                }
            },
            CommandId::Stop => {
                info!("🛑 增强版TTS-Only接收Stop命令（完全停止）");
                // 🛑 立刻阻止预热/拉起
                self.tts_controller.set_stop_pending(true);
                // 视为收到 StopInput
                self.stop_input_received.store(true, Ordering::Release);
                // 广播打断
                let _ = self
                    .interrupt_manager
                    .broadcast_global_interrupt(self.session_id.clone(), SimpleInterruptReason::UserSpeaking);
                // 取消/重置TTS
                if let Err(e) = self.tts_controller.interrupt_session().await {
                    warn!("⚠️ 中断TTS失败: {}", e);
                }
                self.tts_controller.reset_client().await;
                // 发送轮次完成信号给统一 TtsTask（触发 turn-final 注入与 done/stopped）
                if let Some(tx) = self.llm_to_tts_tx.lock().await.as_ref()
                    && let Some(resp_id) = self.current_turn_response_id.load()
                {
                    let ctx = TurnContext::new("user_".to_string(), format!("asst_{}", nanoid::nanoid!(6)), resp_id, Some(1));
                    let _ = tx.send((ctx, "__TURN_COMPLETE__".to_string()));
                }
            },
            CommandId::StopInput => {
                info!("🛑 增强版TTS-Only接收StopInput命令（仅停止输入）");
                // 记录 StopInput 标志
                self.stop_input_received.store(true, Ordering::Release);
                // 发出 LLM 轮次结束信号给 TtsTask
                if let Some(tx) = self.llm_to_tts_tx.lock().await.as_ref()
                    && let Some(resp_id) = self.current_turn_response_id.load()
                {
                    let ctx = TurnContext::new("user_".to_string(), format!("asst_{}", nanoid::nanoid!(6)), resp_id, Some(1));
                    let _ = tx.send((ctx, "__TURN_COMPLETE__".to_string()));
                }
            },
            _ => {
                warn!("增强版TTS-Only Pipeline接收到未支持的命令: {:?}", payload.header.command_id);
            },
        }

        Ok(())
    }

    async fn handle_tool_call_result(&self, _tool_result: super::super::asr_llm_tts::tool_call_manager::ToolCallResult) -> Result<()> {
        // 增强版TTS-Only pipeline不支持工具调用
        warn!("增强版TTS-Only Pipeline不支持工具调用功能");
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    /// 统一会话配置热更新入口（TTS-only 适配）
    async fn apply_session_config(&self, payload: &crate::rpc::protocol::MessagePayload) -> Result<()> {
        use crate::rpc::protocol::MessagePayload;

        if let MessagePayload::SessionConfig {
            voice_setting,
            output_audio_config,
            initial_burst_count,
            initial_burst_delay_ms,
            send_rate_multiplier,
            asr_language,
            text_done_signal_only,
            signal_only,
            tts_chinese_convert,
            ..
        } = payload
        {
            // 1) 语音设置
            if let Some(vs) = voice_setting {
                let setting: crate::tts::minimax::VoiceSetting = serde_json::from_value(vs.clone()).map_err(|e| anyhow!("解析语音设置失败: {}", e))?;
                self.tts_controller
                    .update_voice_setting(setting)
                    .await
                    .map_err(|e| anyhow!("更新语音设置失败: {}", e))?;
            }

            // 2) 输出音频配置
            if let Some(out_cfg_val) = output_audio_config {
                match serde_json::from_value::<crate::audio::OutputAudioConfig>(out_cfg_val.clone()) {
                    Ok(cfg) => {
                        self.tts_controller.configure_output_config(cfg).await?;
                    },
                    Err(e) => {
                        warn!("⚠️ 解析 output_audio_config 失败: {}", e);
                    },
                }
            }

            // 3) PacedSender 节拍参数
            if initial_burst_count.is_some() || initial_burst_delay_ms.is_some() || send_rate_multiplier.is_some() {
                let burst = initial_burst_count.unwrap_or(0) as usize;
                let delay = initial_burst_delay_ms.unwrap_or(5) as u64;
                let rate = send_rate_multiplier.unwrap_or(1.0);
                self.tts_controller.update_pacing(burst, delay, rate).await;
            }

            // 4) 语言（用于 TTS start_task 的 language_boost）
            if asr_language.is_some() {
                self.tts_controller.set_language(asr_language.clone()).await;
            }

            // 5) 文本/信令开关
            if let Some(only) = *text_done_signal_only {
                self.text_done_signal_only_flag
                    .store(only, std::sync::atomic::Ordering::Release);
            }
            if let Some(only) = *signal_only {
                self.signal_only_flag.store(only, std::sync::atomic::Ordering::Release);
            }

            // 6) TTS 繁简转换模式
            if let Some(mode_str) = tts_chinese_convert.clone() {
                // 转换为枚举
                let cmode: crate::text_filters::ConvertMode = mode_str.as_str().into();
                // 更新管线字段（线程安全）
                if let Ok(mut guard) = self.tts_chinese_convert_mode.write() {
                    *guard = cmode;
                }
                // 统一由管线字段承载，TtsTask侧不感知
            }
        }

        Ok(())
    }
}

/// 创建增强版TTS-Only管线的便捷函数
pub fn create_enhanced_tts_only_pipeline(session_id: String, router: Arc<SessionRouter>, tts_config: Option<MiniMaxConfig>, voice_setting: Option<VoiceSetting>) -> EnhancedStreamingTtsOnlyPipeline {
    EnhancedStreamingTtsOnlyPipeline::new_with_minimax(session_id, router, tts_config, voice_setting)
}

/// 🆕 新增：使用TTS池创建增强版TTS-Only管线的便捷函数
// 旧的全局池便捷函数已移除
/// 🆕 新增：创建支持Opus输出的TTS-Only管线
pub fn create_enhanced_tts_only_pipeline_with_opus(
    session_id: String,
    router: Arc<SessionRouter>,
    tts_config: Option<MiniMaxConfig>,
    voice_setting: Option<VoiceSetting>,
    opus_config: Option<OpusEncoderConfig>,
) -> EnhancedStreamingTtsOnlyPipeline {
    // 创建Opus音频输出配置
    let mut opus_encoder_config = opus_config.unwrap_or_default();

    // 🔧 修复：确保frame_duration_ms与slice_ms保持一致
    let slice_ms = 20; // 默认20ms时间片
    opus_encoder_config.frame_duration_ms = Some(slice_ms); // 确保帧长与时间片一致

    let output_config = OutputAudioConfig::opus(slice_ms, opus_encoder_config);

    EnhancedStreamingTtsOnlyPipeline::new_with_audio_config(session_id, router, tts_config, voice_setting, output_config, None)
}

/// 🆕 新增：创建支持自定义音频格式的TTS-Only管线
pub fn create_enhanced_tts_only_pipeline_with_audio_format(
    session_id: String,
    router: Arc<SessionRouter>,
    tts_config: Option<MiniMaxConfig>,
    voice_setting: Option<VoiceSetting>,
    output_audio_config: OutputAudioConfig,
    chinese_convert: Option<String>,
    language: Option<String>,
) -> EnhancedStreamingTtsOnlyPipeline {
    let pipeline = EnhancedStreamingTtsOnlyPipeline::new_with_audio_config(
        session_id,
        router,
        tts_config,
        voice_setting,
        output_audio_config,
        chinese_convert,
    );

    // 将语言设置给 TTS 控制器（用于 start_task 的 language_boost）
    let ctrl = pipeline.tts_controller.clone();
    tokio::spawn(async move {
        ctrl.set_language(language).await;
    });

    pipeline
}
