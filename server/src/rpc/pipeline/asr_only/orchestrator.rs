//! ASR-only 管线编排器
//!
//! 架构：音频输入 → VAD → ASR → 转录事件输出
//! 仅提供语音识别功能，不包含 LLM 和 TTS 处理。

use anyhow::Result;
use async_trait::async_trait;
use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::{Mutex, mpsc, watch};
use tracing::{error, info, warn};

use crate::asr::{AsrEngine, SpeechMode};
use crate::rpc::{
    pipeline::{
        CleanupGuard, StreamingPipeline,
        asr_llm_tts::{
            asr_task_base::BaseAsrTaskConfig,
            asr_task_core::AsrInputMessage,
            asr_task_ptt::AsrTaskPtt,
            asr_task_vad::AsrTaskVad,
            asr_task_vad_deferred::AsrTaskVadDeferred,
            event_emitter::EventEmitter,
            simple_interrupt_manager::{SimpleInterruptHandler, SimpleInterruptManager},
            types::{SharedFlags, TaskCompletion},
        },
    },
    protocol::{BinaryMessage, ProtocolId},
    session_router::SessionRouter,
};

/// ASR-only 管线
///
/// 仅提供语音识别功能，适用于只需要语音转文字的场景。
/// 支持三种语音分段模式：VAD、PTT、VadDeferred
#[allow(clippy::type_complexity)]
pub struct AsrOnlyPipeline {
    session_id: String,
    router: Arc<SessionRouter>,
    asr_engine: Arc<AsrEngine>,
    speech_mode: SpeechMode,
    asr_language: Option<String>,

    // 基础设施
    input_tx: Arc<Mutex<Option<mpsc::Sender<AsrInputMessage>>>>,
    shared_flags: Arc<SharedFlags>,
    simple_interrupt_manager: Arc<SimpleInterruptManager>,
    input_processor: Arc<Mutex<crate::audio::input_processor::AudioInputProcessor>>,

    // 热更新支持
    /// ASR 语言热更新发送端
    asr_language_tx: watch::Sender<Option<String>>,
    /// VAD 运行时参数热更新发送端 (threshold, min_silence_ms, min_speech_ms)
    vad_runtime_tx: watch::Sender<Option<(Option<f32>, Option<u32>, Option<u32>)>>,
}

impl AsrOnlyPipeline {
    /// 创建 ASR-only 管线
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: String,
        router: Arc<SessionRouter>,
        asr_engine: Arc<AsrEngine>,
        speech_mode: SpeechMode,
        asr_language: Option<String>,
        input_audio_config: crate::audio::input_processor::AudioInputConfig,
    ) -> Self {
        info!(
            "🎤 创建 ASR-only 管线: session_id={}, mode={:?}, language={:?}",
            session_id, speech_mode, asr_language
        );

        let simple_interrupt_manager = Arc::new(SimpleInterruptManager::new());
        let shared_flags = Arc::new(SharedFlags::new());

        // 初始化音频输入处理器
        let input_processor = match crate::audio::AudioInputProcessor::new(input_audio_config.clone()) {
            Ok(processor) => {
                info!(
                    "✅ 音频输入处理器初始化成功: format={:?}, sample_rate={}",
                    input_audio_config.format, input_audio_config.sample_rate
                );
                Arc::new(Mutex::new(processor))
            },
            Err(e) => {
                error!("❌ 音频输入处理器初始化失败: {}, 使用默认配置", e);
                Arc::new(Mutex::new(crate::audio::AudioInputProcessor::new(Default::default()).unwrap()))
            },
        };

        // 创建热更新 watch channels
        let (asr_language_tx, _) = watch::channel(asr_language.clone());
        let (vad_runtime_tx, _) = watch::channel(None);

        Self {
            session_id,
            router,
            asr_engine,
            speech_mode,
            asr_language,
            input_tx: Arc::new(Mutex::new(None)),
            shared_flags,
            simple_interrupt_manager,
            input_processor,
            asr_language_tx,
            vad_runtime_tx,
        }
    }
}

#[async_trait]
impl StreamingPipeline for AsrOnlyPipeline {
    async fn start(&self) -> Result<CleanupGuard> {
        info!("▶️ 启动 ASR-only 管线: {}", self.session_id);

        // 1. 创建事件发射器
        let emitter = Arc::new(EventEmitter::new(
            self.router.clone(),
            self.session_id.clone(),
            Arc::new(AtomicBool::new(false)), // text_done_signal_only
            Arc::new(AtomicBool::new(false)), // signal_only
        ));

        // 发送 session.created 事件
        emitter.session_created(ProtocolId::Asr).await;

        // 2. 创建任务协调 channels
        let (task_completion_tx, mut task_completion_rx) = mpsc::unbounded_channel::<TaskCompletion>();

        // ASR 输出 channel（ASR-only 模式下不需要下游任务，但仍需接收用于打印/日志）
        // 使用 unbounded channel 接收转录结果（仅用于日志记录）
        let (asr_output_tx, mut asr_output_rx) = mpsc::unbounded_channel::<(crate::rpc::pipeline::asr_llm_tts::types::TurnContext, String)>();

        // 3. 启动 ASR Task
        let (input_tx, input_rx) = mpsc::channel::<AsrInputMessage>(1000);
        let (asr_cleanup_tx, asr_cleanup_rx) = mpsc::unbounded_channel::<()>();

        // 保存 input_tx 用于 on_upstream
        {
            let mut tx_guard = self.input_tx.lock().await;
            *tx_guard = Some(input_tx.clone());
        }

        // 创建轮次响应ID（Writer/Reader分离）
        let current_turn_response_id = Arc::new(crate::rpc::pipeline::asr_llm_tts::lockfree_response_id::LockfreeResponseId::new());

        let session_id_clone = self.session_id.clone();

        // 创建热更新 watch receivers
        let asr_language_rx = self.asr_language_tx.subscribe();
        let vad_runtime_rx = self.vad_runtime_tx.subscribe();

        match self.speech_mode {
            SpeechMode::Vad => {
                let asr_task = AsrTaskVad {
                    base: BaseAsrTaskConfig {
                        session_id: self.session_id.clone(),
                        asr_engine: self.asr_engine.clone(),
                        emitter: emitter.clone(),
                        router: self.router.clone(),
                        input_rx,
                        shared_flags: self.shared_flags.clone(),
                        task_completion_tx: task_completion_tx.clone(),
                        simple_interrupt_manager: self.simple_interrupt_manager.clone(),
                        simple_interrupt_handler: Some(SimpleInterruptHandler::new(
                            self.session_id.clone(),
                            "ASR-VAD-Only".to_string(),
                            self.simple_interrupt_manager.subscribe(),
                        )),
                        cleanup_rx: asr_cleanup_rx,
                        asr_language: self.asr_language.clone(),
                        asr_language_rx: Some(asr_language_rx),
                        current_turn_response_id: current_turn_response_id.clone(),
                        parallel_tts_tx: Some(asr_output_tx.clone()),
                    },
                    vad_runtime_rx: Some(vad_runtime_rx),
                    simultaneous_segment_config: None, // ASR-only 模式不启用字数断句
                };

                tokio::spawn(async move {
                    info!("🎤 启动 ASR VAD 任务 (ASR-only)");
                    if let Err(e) = asr_task.run().await {
                        error!("❌ ASR VAD 任务失败: {}", e);
                    }
                });
            },
            SpeechMode::PushToTalk => {
                let asr_task = AsrTaskPtt {
                    base: BaseAsrTaskConfig {
                        session_id: self.session_id.clone(),
                        asr_engine: self.asr_engine.clone(),
                        emitter: emitter.clone(),
                        router: self.router.clone(),
                        input_rx,
                        shared_flags: self.shared_flags.clone(),
                        task_completion_tx: task_completion_tx.clone(),
                        simple_interrupt_manager: self.simple_interrupt_manager.clone(),
                        simple_interrupt_handler: Some(SimpleInterruptHandler::new(
                            self.session_id.clone(),
                            "ASR-PTT-Only".to_string(),
                            self.simple_interrupt_manager.subscribe(),
                        )),
                        cleanup_rx: asr_cleanup_rx,
                        asr_language: self.asr_language.clone(),
                        asr_language_rx: Some(asr_language_rx),
                        current_turn_response_id: current_turn_response_id.clone(),
                        parallel_tts_tx: Some(asr_output_tx.clone()),
                    },
                };

                tokio::spawn(async move {
                    info!("🎤 启动 ASR PTT 任务 (ASR-only)");
                    if let Err(e) = asr_task.run().await {
                        error!("❌ ASR PTT 任务失败: {}", e);
                    }
                });
            },
            SpeechMode::VadDeferred => {
                let asr_task = AsrTaskVadDeferred {
                    base: BaseAsrTaskConfig {
                        session_id: self.session_id.clone(),
                        asr_engine: self.asr_engine.clone(),
                        emitter: emitter.clone(),
                        router: self.router.clone(),
                        input_rx,
                        shared_flags: self.shared_flags.clone(),
                        task_completion_tx: task_completion_tx.clone(),
                        simple_interrupt_manager: self.simple_interrupt_manager.clone(),
                        simple_interrupt_handler: Some(SimpleInterruptHandler::new(
                            self.session_id.clone(),
                            "ASR-VadDeferred-Only".to_string(),
                            self.simple_interrupt_manager.subscribe(),
                        )),
                        cleanup_rx: asr_cleanup_rx,
                        asr_language: self.asr_language.clone(),
                        asr_language_rx: Some(asr_language_rx),
                        current_turn_response_id: current_turn_response_id.clone(),
                        parallel_tts_tx: Some(asr_output_tx),
                    },
                    vad_runtime_rx: Some(vad_runtime_rx),
                };

                tokio::spawn(async move {
                    info!("🎤 启动 ASR VadDeferred 任务 (ASR-only)");
                    if let Err(e) = asr_task.run().await {
                        error!("❌ ASR VadDeferred 任务失败: {}", e);
                    }
                });
            },
        }

        // 4. 启动 ASR 输出日志记录器（ASR-only 模式不需要下游处理，但记录转录结果）
        tokio::spawn(async move {
            while let Some((ctx, transcript)) = asr_output_rx.recv().await {
                info!(
                    "📝 [ASR-only] 转录完成: session={}, response_id={}, text='{}'",
                    session_id_clone, ctx.response_id, transcript
                );
            }
            info!("📪 ASR 输出接收器结束: {}", session_id_clone);
        });

        // 5. 启动任务完成监控
        let session_id = self.session_id.clone();
        tokio::spawn(async move {
            while let Some(completion) = task_completion_rx.recv().await {
                info!("✅ [ASR-only] 任务完成: {:?}", completion);
            }
            info!("📪 任务完成监控结束: {}", session_id);
        });

        // 6. 返回清理守卫
        let session_id_cleanup = self.session_id.clone();
        let cleanup_tx = asr_cleanup_tx;

        Ok(CleanupGuard::new(move || {
            info!("🧹 清理 ASR-only 管线: {}", session_id_cleanup);
            let _ = cleanup_tx.send(());
        }))
    }

    async fn on_upstream(&self, payload: BinaryMessage) -> Result<()> {
        use crate::rpc::protocol::CommandId;

        match payload.header.command_id {
            CommandId::Start => {
                // PTT 模式开始信号 - ASR Task 内部会自动调用 begin_speech()
                // 这里只记录日志，实际处理在 ASR Task 中
                info!("▶️ [ASR-only] 收到 Start 信号: {}", self.session_id);
            },
            CommandId::AudioChunk => {
                let audio_bytes = payload.payload;

                // 使用音频输入处理器解码音频数据
                let audio_f32 = {
                    let mut processor = self.input_processor.lock().await;
                    match processor.process_audio_chunk(&audio_bytes) {
                        Ok(audio) => {
                            if audio.is_empty() {
                                return Ok(());
                            }
                            audio
                        },
                        Err(e) => {
                            error!("❌ 音频解码失败: {}", e);
                            return Err(anyhow::anyhow!("音频解码失败: {}", e));
                        },
                    }
                };

                // 转发到 ASR Task
                let tx_guard = self.input_tx.lock().await;
                if let Some(ref tx) = *tx_guard {
                    let msg = AsrInputMessage::Audio(audio_f32);
                    if let Err(e) = tx.send(msg).await {
                        error!("❌ 发送音频到 ASR 失败: {}", e);
                    }
                } else {
                    warn!("⚠️ ASR input_tx 未初始化");
                }
            },
            CommandId::Stop => {
                // PTT 结束信号
                info!("🛑 [ASR-only] 收到 Stop 信号: {}", self.session_id);
                let tx_guard = self.input_tx.lock().await;
                if let Some(ref tx) = *tx_guard {
                    let _ = tx.send(AsrInputMessage::PttEnd).await;
                }
            },
            CommandId::StopInput => {
                // VadDeferred 模式下的停止输入信号
                info!("🛑 [ASR-only] 收到 StopInput 信号: {}", self.session_id);
                let tx_guard = self.input_tx.lock().await;
                if let Some(ref tx) = *tx_guard {
                    let _ = tx.send(AsrInputMessage::PttEnd).await;
                }
            },
            _ => {
                // 其他命令忽略
            },
        }

        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    /// 会话配置热更新
    async fn apply_session_config(&self, payload: &crate::rpc::protocol::MessagePayload) -> Result<()> {
        if let crate::rpc::protocol::MessagePayload::SessionConfig { asr_language, vad_threshold, silence_duration_ms, min_speech_duration_ms, .. } = payload {
            // 更新 ASR 语言
            if let Some(lang) = asr_language {
                info!("🔄 [ASR-only] 热更新 ASR 语言: {:?}", lang);
                let _ = self.asr_language_tx.send(Some(lang.clone()));
            }

            // 更新 VAD 运行时参数
            if vad_threshold.is_some() || silence_duration_ms.is_some() || min_speech_duration_ms.is_some() {
                info!(
                    "🔄 [ASR-only] 热更新 VAD 参数: threshold={:?}, silence_duration_ms={:?}, min_speech_duration_ms={:?}",
                    vad_threshold, silence_duration_ms, min_speech_duration_ms
                );
                let _ = self
                    .vad_runtime_tx
                    .send(Some((*vad_threshold, *silence_duration_ms, *min_speech_duration_ms)));
            }
        }

        Ok(())
    }
}
