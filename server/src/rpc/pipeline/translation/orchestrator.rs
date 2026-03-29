//! 同声传译管线编排器
//!
//! 架构：ASR Task → Translation Task → TTS Task
//! 复用：asr_llm_tts 的 ASR 和 TTS 任务，新增翻译任务

use anyhow::Result;
use async_trait::async_trait;
use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::{Mutex, broadcast, mpsc};
use tracing::{error, info, warn};

use crate::asr::{AsrEngine, SpeechMode};
use crate::audio::OutputAudioConfig;
use crate::llm::LlmClient;
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
            tts_task::{TtsController, TtsTask},
            types::{SharedFlags, SimultaneousSegmentConfig, TaskCompletion, TurnContext},
        },
    },
    protocol::{BinaryMessage, ProtocolId},
    session_router::SessionRouter,
};
use crate::text_splitter::SimplifiedStreamingSplitter;
use crate::tts::minimax::{MiniMaxConfig, VoiceSetting};

use super::{language_router::get_actual_tts_language, translation_task::TranslationTask};

/// 选择 ASR 引擎（统一使用 WhisperLive）
fn select_asr_engine(_lang_a: &str, _lang_b: &str) -> &'static str {
    "whisperlive"
}

/// 同声传译管线
pub struct TranslationPipeline {
    session_id: String,
    router: Arc<SessionRouter>,
    asr_engine: Arc<AsrEngine>,
    llm_client: Arc<LlmClient>,
    tts_config: Option<MiniMaxConfig>,
    speech_mode: SpeechMode,
    from_language: String,
    to_language: String,
    voice_setting: Option<VoiceSetting>,

    // 基础设施（参考 asr_llm_tts）
    input_tx: Arc<Mutex<Option<mpsc::Sender<AsrInputMessage>>>>,
    shared_flags: Arc<SharedFlags>,
    simple_interrupt_manager: Arc<SimpleInterruptManager>,
    output_audio_config: OutputAudioConfig,
    input_processor: Arc<Mutex<crate::audio::input_processor::AudioInputProcessor>>,
}

impl TranslationPipeline {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: String,
        router: Arc<SessionRouter>,
        asr_engine: Arc<AsrEngine>,
        llm_client: Arc<LlmClient>,
        tts_config: Option<MiniMaxConfig>,
        speech_mode: SpeechMode,
        from_language: String,
        to_language: String,
        voice_setting: Option<VoiceSetting>,
        output_audio_config: OutputAudioConfig,
        input_audio_config: crate::audio::input_processor::AudioInputConfig,
    ) -> Self {
        info!(
            "🌍 创建同声传译管线: {} → {}, mode={:?}",
            from_language, to_language, speech_mode
        );

        let simple_interrupt_manager = Arc::new(SimpleInterruptManager::new());
        let shared_flags = Arc::new(SharedFlags::new());

        // 选择 ASR 引擎
        let asr_engine_type = select_asr_engine(&from_language, &to_language);
        {
            let mut engine = shared_flags.preferred_asr_engine.lock().unwrap();
            *engine = Some(asr_engine_type.to_string());
        }
        info!(
            "🎤 同声传译 ASR 引擎选择: {} (语言对: {} <-> {})",
            asr_engine_type, from_language, to_language
        );

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

        Self {
            session_id,
            router,
            asr_engine,
            llm_client,
            tts_config,
            speech_mode,
            from_language,
            to_language,
            voice_setting,
            input_tx: Arc::new(Mutex::new(None)),
            shared_flags,
            simple_interrupt_manager,
            output_audio_config,
            input_processor,
        }
    }
}

#[async_trait]
impl StreamingPipeline for TranslationPipeline {
    async fn start(&self) -> Result<CleanupGuard> {
        info!("▶️ 启动同声传译管线: {}", self.session_id);

        // 1. 创建事件发射器
        let emitter = Arc::new(EventEmitter::new(
            self.router.clone(),
            self.session_id.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        ));

        // 发送 session.created 事件
        emitter.session_created(ProtocolId::Translation).await;

        // 2. 创建任务协调 channels
        let (task_completion_tx, mut task_completion_rx) = mpsc::unbounded_channel::<TaskCompletion>();

        // ASR → Translation channel
        let (parallel_llm_tx, parallel_llm_rx) = mpsc::unbounded_channel::<(TurnContext, String)>();

        // Translation → TTS channel
        let (llm_to_tts_tx, llm_to_tts_rx) = broadcast::channel::<(TurnContext, String)>(1000);

        // 3. 创建 TTS 控制器（带 fallback 语言）
        let actual_language = get_actual_tts_language(&self.to_language);
        if actual_language != self.to_language {
            info!("🔄 TTS 语言 fallback: '{}' → '{}'", self.to_language, actual_language);
        }

        let mut tts_controller = TtsController::new(self.tts_config.clone(), self.voice_setting.clone());
        tts_controller.set_interrupt_manager(self.simple_interrupt_manager.clone());

        // 设置 TTS 语言（用于 language_boost）
        let tts_controller = Arc::new(tts_controller);
        {
            let ctrl = tts_controller.clone();
            let lang = Some(actual_language.clone());
            tokio::spawn(async move {
                ctrl.set_language(lang).await;
            });
        }

        // 配置 TTS 输出格式
        tts_controller.configure_output_config(self.output_audio_config.clone()).await?;

        // 4. 启动 ASR Task
        let (input_tx, input_rx) = mpsc::channel::<AsrInputMessage>(1000);
        let (asr_cleanup_tx, asr_cleanup_rx) = mpsc::unbounded_channel::<()>();

        // 保存 input_tx 用于 on_upstream
        {
            let mut tx_guard = self.input_tx.lock().await;
            *tx_guard = Some(input_tx.clone());
        }

        // 创建轮次响应ID（Writer/Reader分离）
        let current_turn_response_id = Arc::new(crate::rpc::pipeline::asr_llm_tts::lockfree_response_id::LockfreeResponseId::new());
        let current_turn_response_id_reader = Arc::new(current_turn_response_id.reader());

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
                            "ASR-VAD-Translation".to_string(),
                            self.simple_interrupt_manager.subscribe(),
                        )),
                        cleanup_rx: asr_cleanup_rx,
                        asr_language: Some(self.from_language.clone()),
                        asr_language_rx: None,
                        current_turn_response_id: current_turn_response_id.clone(),
                        parallel_tts_tx: Some(parallel_llm_tx.clone()),
                    },
                    vad_runtime_rx: None,
                    // 同传字数断句配置：启用字数断句，达到阈值后在断句点处提前发送翻译
                    simultaneous_segment_config: Some(SimultaneousSegmentConfig::enabled_with_defaults()),
                };

                tokio::spawn(async move {
                    info!("🎤 启动 ASR VAD 任务");
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
                            "ASR-PTT-Translation".to_string(),
                            self.simple_interrupt_manager.subscribe(),
                        )),
                        cleanup_rx: asr_cleanup_rx,
                        asr_language: Some(self.from_language.clone()),
                        asr_language_rx: None,
                        current_turn_response_id: current_turn_response_id.clone(),
                        parallel_tts_tx: Some(parallel_llm_tx),
                    },
                };

                tokio::spawn(async move {
                    info!("🎤 启动 ASR PTT 任务");
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
                            "ASR-VadDeferred-Translation".to_string(),
                            self.simple_interrupt_manager.subscribe(),
                        )),
                        cleanup_rx: asr_cleanup_rx,
                        asr_language: Some(self.from_language.clone()),
                        asr_language_rx: None,
                        current_turn_response_id: current_turn_response_id.clone(),
                        parallel_tts_tx: Some(parallel_llm_tx),
                    },
                    vad_runtime_rx: None,
                };

                tokio::spawn(async move {
                    info!("🎤 启动 ASR VadDeferred 任务（同声传译）");
                    if let Err(e) = asr_task.run().await {
                        error!("❌ ASR VadDeferred 任务失败: {}", e);
                    }
                });
            },
        }

        // 5. 启动 Translation Task
        // 同声传译模式：使用忽略用户说话打断的处理器，让翻译按队列顺序完成
        let translation_task = TranslationTask::new(
            self.session_id.clone(),
            self.llm_client.clone(),
            self.from_language.clone(),
            self.to_language.clone(),
            parallel_llm_rx,       // 接收 ASR 输出
            llm_to_tts_tx.clone(), // 发送到 TTS
            Some(SimpleInterruptHandler::new_ignore_user_speaking(
                self.session_id.clone(),
                "Translation-Task".to_string(),
                self.simple_interrupt_manager.subscribe(),
            )),
            emitter.clone(), // 发送 delta/done 事件到客户端
        );

        tokio::spawn(async move {
            info!("🌍 启动翻译任务");
            if let Err(e) = translation_task.run().await {
                error!("❌ 翻译任务失败: {}", e);
            }
        });

        // 6. 启动 TTS Task
        // 创建 NextSentenceTrigger channel（按需音频生成）
        let (next_sentence_tx, next_sentence_rx) = mpsc::unbounded_channel();

        // 在将 tts_controller 移动到 TtsTask 前，先克隆一份用于后续 CleanupGuard
        let tts_ctrl_for_cleanup = tts_controller.clone();

        // 同声传译模式：TTS 使用忽略用户说话打断的处理器，按队列顺序播放
        let tts_task = TtsTask::new(
            self.session_id.clone(),
            tts_controller,
            emitter.clone(),
            self.router.clone(),
            llm_to_tts_rx,
            Arc::new(AtomicBool::new(false)), // tts_session_created
            self.shared_flags.clone(),
            task_completion_tx.clone(),
            self.simple_interrupt_manager.clone(),
            Some(SimpleInterruptHandler::new_ignore_user_speaking(
                self.session_id.clone(),
                "TTS-Translation".to_string(),
                self.simple_interrupt_manager.subscribe(),
            )),
            10,                                                           // initial_burst_count
            50,                                                           // initial_burst_delay_ms
            1.0,                                                          // send_rate_multiplier
            Arc::new(AtomicBool::new(false)),                             // text_splitter_first_chunk_recorded
            Arc::new(Mutex::new(SimplifiedStreamingSplitter::new(None))), // text_splitter
            self.output_audio_config.clone(),                             // initial_output_config
            Arc::new(Mutex::new(None)),                                   // audio_handler_task
            current_turn_response_id_reader,                              // 使用 Reader 而不是 Writer
            next_sentence_tx,                                             // 按需生成触发器
            next_sentence_rx,                                             // 按需生成接收器
            true,                                                         // 🆕 is_translation_mode: 同声传译模式，不清空旧缓冲区
        );

        tokio::spawn(async move {
            info!("🔊 启动 TTS 任务");
            if let Err(e) = tts_task.run().await {
                error!("❌ TTS 任务失败: {}", e);
            }
        });

        // 7. 启动任务完成监控
        let session_id = self.session_id.clone();
        tokio::spawn(async move {
            while let Some(completion) = task_completion_rx.recv().await {
                info!("✅ 任务完成: {:?}", completion);
            }
            info!("📪 任务完成监控结束: {}", session_id);
        });

        // 8. 返回清理守卫（避免捕获 &self，完全使用已拥有的克隆）
        let session_id_cleanup = self.session_id.clone();
        let cleanup_tx = asr_cleanup_tx;

        Ok(CleanupGuard::new(move || {
            info!("🧹 清理同声传译管线: {}", session_id_cleanup);
            let _ = cleanup_tx.send(());
            let sid = session_id_cleanup.clone();
            let tts = tts_ctrl_for_cleanup.clone();
            tokio::spawn(async move {
                // 归还 TTS 客户端
                tts.return_client().await;
                info!("🔊 已归还TTS客户端（translation）: {}", sid);
            });
        }))
    }

    async fn on_upstream(&self, payload: BinaryMessage) -> Result<()> {
        use crate::rpc::protocol::CommandId;

        match payload.header.command_id {
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
                let tx_guard = self.input_tx.lock().await;
                if let Some(ref tx) = *tx_guard {
                    let _ = tx.send(AsrInputMessage::PttEnd).await;
                }
            },
            CommandId::StopInput => {
                // VadDeferred 模式下的停止输入信号，触发翻译
                info!("🛑 收到 StopInput 信号，触发翻译: {}", self.session_id);
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
}
