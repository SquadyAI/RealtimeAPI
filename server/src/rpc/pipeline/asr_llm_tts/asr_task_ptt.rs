use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::asr::{AsrResult, SpeechMode};

use super::asr_task_base::{BaseAsrTaskConfig, is_only_punctuation};
use super::asr_task_core::{AsrInputMessage, CoreEvent, poll_core_event};
use super::types::{TaskCompletion, TurnContext};

/// PTT-only ASR task
pub struct AsrTaskPtt {
    pub base: BaseAsrTaskConfig,
}

impl AsrTaskPtt {
    pub async fn run(self) -> Result<()> {
        // Destructure base to avoid self.base.field everywhere
        let BaseAsrTaskConfig {
            session_id,
            asr_engine,
            emitter,
            router: _,
            mut input_rx,
            shared_flags,
            task_completion_tx,
            simple_interrupt_manager,
            simple_interrupt_handler: _,
            mut cleanup_rx,
            mut asr_language,
            asr_language_rx,
            current_turn_response_id,
            parallel_tts_tx,
        } = self.base;

        info!("🎤 ASR PTT task starting for session {}", session_id);

        let _task_start_instant = Instant::now();

        let preferred_asr_engine = shared_flags.preferred_asr_engine.lock().unwrap().clone();
        let mut current_asr_engine = preferred_asr_engine.clone();

        let mut asr_session = asr_engine
            .create_session_with_preferred_model(
                session_id.clone(),
                SpeechMode::PushToTalk,
                asr_language.clone(),
                preferred_asr_engine,
                current_turn_response_id.clone(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("创建 ASR 会话失败: {}", e))?;

        asr_session.begin_speech();
        let mut ptt_started = true;

        let mut current_user_item_id: Option<String> = Some(format!("msg_{}", nanoid::nanoid!(6)));
        let mut current_user_content_index: u32 = 0;
        let mut accumulated_text_buffer = String::new();
        let mut conversation_item_created_sent = false;
        let has_sent_completed_atomic = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let mut interrupt_rx = simple_interrupt_manager.subscribe();
        let mut asr_language_rx = asr_language_rx;
        #[allow(clippy::type_complexity)]
        let mut vad_runtime_rx: Option<watch::Receiver<Option<(Option<f32>, Option<u32>, Option<u32>)>>> = None;
        let mut asr_engine_rx = Some(shared_flags.asr_engine_notify_rx.clone());

        loop {
            let _has_sent_completed_atomic_clone = has_sent_completed_atomic.clone();

            let emitter_callback = emitter.clone();
            let current_user_item_id_ref = &mut current_user_item_id;
            let conversation_item_created_sent_ref = &mut conversation_item_created_sent;
            let current_user_content_index_ref = &mut current_user_content_index;
            let accumulated_text_buffer_ref = &mut accumulated_text_buffer;
            let shared_flags_for_callback = shared_flags.clone();

            let mut callback = move |asr_result: AsrResult| {
                let asr_result = {
                    let convert_mode = *shared_flags_for_callback.asr_chinese_convert_mode.read().unwrap();
                    AsrResult {
                        text: crate::text_filters::convert_text(&asr_result.text, convert_mode),
                        ..asr_result
                    }
                };

                if asr_result.text.trim().is_empty() {
                    return;
                }

                if !*conversation_item_created_sent_ref && let Some(item_id) = current_user_item_id_ref.as_ref() {
                    let emitter_item = emitter_callback.clone();
                    let item_id_clone = item_id.clone();
                    tokio::spawn(async move {
                        emitter_item
                            .conversation_item_created(&item_id_clone, "user", "in_progress", None)
                            .await;
                    });
                    *conversation_item_created_sent_ref = true;
                }

                let text = asr_result.text.clone();
                if let Some(item_id) = current_user_item_id_ref.as_ref() {
                    let emitter = emitter_callback.clone();
                    let id = item_id.clone();
                    let content_index = *current_user_content_index_ref;
                    if asr_result.is_partial {
                        *current_user_content_index_ref += 1;
                        tokio::spawn(async move {
                            emitter
                                .conversation_item_input_audio_transcription_delta(&id, content_index, &text)
                                .await;
                        });
                    } else {
                        let new_text = text.trim();
                        if !new_text.is_empty() {
                            accumulated_text_buffer_ref.clear();
                            accumulated_text_buffer_ref.push_str(new_text);
                        }
                    }
                }
            };

            match poll_core_event(
                &mut asr_session,
                &mut input_rx,
                &mut interrupt_rx,
                &mut cleanup_rx,
                &mut asr_language_rx,
                &mut vad_runtime_rx,
                &mut asr_engine_rx,
            )
            .await
            {
                CoreEvent::InputAudio(audio_chunk) => {
                    if !ptt_started {
                        ptt_started = true;
                        asr_session.begin_speech();

                        has_sent_completed_atomic.store(false, std::sync::atomic::Ordering::Release);

                        let current_turn_for_interrupt = simple_interrupt_manager.current_turn();
                        if current_turn_for_interrupt > 0 {
                            let is_responding = *shared_flags.is_responding_rx.borrow();

                            if is_responding {
                                let target_turn = current_turn_for_interrupt;
                                let simple_interrupt_manager_for_ptt = simple_interrupt_manager.clone();
                                let session_id_for_ptt_interrupt = session_id.clone();
                                tokio::spawn(async move {
                                    let start_time = std::time::Instant::now();
                                    let result = simple_interrupt_manager_for_ptt.broadcast_global_interrupt_with_turn(
                                        session_id_for_ptt_interrupt,
                                        crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::InterruptReason::UserPtt,
                                        target_turn,
                                    );
                                    let elapsed = start_time.elapsed();
                                    match result {
                                        Ok(_) => {
                                            info!(
                                                "✅ PTT打断信号发送成功: target_turn={} (当前轮次), elapsed={:?}",
                                                target_turn, elapsed
                                            );
                                        },
                                        Err(e) => {
                                            error!(
                                                "❌ PTT打断信号发送失败: target_turn={} (当前轮次), error={}, elapsed={:?}",
                                                target_turn, e, elapsed
                                            );
                                        },
                                    }
                                });
                            } else {
                                info!(
                                    "⏭️ PTT打断被过滤：TTS首包未发送 (is_responding=false)，避免未播先断 | session={}, target_turn={}",
                                    session_id, current_turn_for_interrupt
                                );
                            }
                        }
                    }
                    if let Err(e) = asr_session.process_audio_chunk(audio_chunk, &mut callback).await {
                        error!("处理音频块失败: {}", e);
                    }
                },
                CoreEvent::PttEnd => {
                    info!("🛑 收到PTT End 事件，执行end_speech并发送到LLM");

                    let mut drained_audio_chunks = 0usize;
                    loop {
                        match input_rx.try_recv() {
                            Ok(AsrInputMessage::Audio(chunk)) => {
                                drained_audio_chunks += 1;
                                if let Err(e) = asr_session.process_audio_chunk(chunk, &mut callback).await {
                                    error!("处理尾音音频块失败: {}", e);
                                }
                            },
                            Ok(AsrInputMessage::PttEnd) => {
                                debug!("🔁 [PTT] 额外的 PttEnd 已忽略: {}", session_id);
                            },
                            Ok(AsrInputMessage::DirectText(_)) => {
                                warn!("⚠️ [PTT] StopInput 后收到 DirectText，已忽略: {}", session_id);
                            },
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                        }
                    }
                    if drained_audio_chunks > 0 {
                        debug!(
                            "🎧 [PTT] StopInput 前已 drain 尾音音频块数量: {} (session={})",
                            drained_audio_chunks, session_id
                        );
                    }

                    if let Err(e) = asr_session.end_speech(&mut callback).await {
                        error!("PTT end_speech 失败: {}", e);
                        if let Some(item_id) = current_user_item_id.as_ref() {
                            let emitter = emitter.clone();
                            let id = item_id.clone();
                            let msg = e.to_string();
                            tokio::spawn(async move {
                                emitter.asr_transcription_failed(&id, 0, "END_SPEECH_ERROR", &msg).await;
                            });
                        }
                    }

                    if !accumulated_text_buffer.trim().is_empty() && !is_only_punctuation(&accumulated_text_buffer) && !has_sent_completed_atomic.load(std::sync::atomic::Ordering::Acquire) {
                        let user_item_id = current_user_item_id
                            .clone()
                            .unwrap_or_else(|| format!("msg_{}", nanoid::nanoid!(6)));
                        let assistant_item_id = format!("asst_{}", nanoid::nanoid!(6));
                        let response_id = format!("resp_{}", nanoid::nanoid!(8));

                        let emitter_asr = emitter.clone();
                        let user_item_id_for_asr = user_item_id.clone();
                        let text_for_asr = accumulated_text_buffer.clone();
                        tokio::spawn(async move {
                            emitter_asr
                                .conversation_item_input_audio_transcription_completed(&user_item_id_for_asr, 0, &text_for_asr)
                                .await;
                            emitter_asr
                                .conversation_item_updated(&user_item_id_for_asr, "user", "completed")
                                .await;
                        });

                        let new_turn_sequence = simple_interrupt_manager.start_new_turn();
                        current_turn_response_id.store(Some(response_id.clone()));
                        let ctx = TurnContext::new(
                            user_item_id.clone(),
                            assistant_item_id,
                            response_id.clone(),
                            Some(new_turn_sequence),
                        );

                        if let Some(ref parallel_tts_tx) = parallel_tts_tx {
                            if parallel_tts_tx.send((ctx, accumulated_text_buffer.clone())).is_err() {
                                error!("⚠️ PTT End发送到并行处理任务失败：通道已关闭");
                            } else {
                                has_sent_completed_atomic.store(true, std::sync::atomic::Ordering::Release);
                            }
                        } else {
                            error!("❌ PTT End时并行处理任务通道未配置");
                        }

                        accumulated_text_buffer.clear();
                        conversation_item_created_sent = false;
                        current_user_content_index = 0;
                        ptt_started = false;
                    } else {
                        info!("🔍 PTT End时无有效文本或已发送，跳过并行处理任务");
                    }
                },
                CoreEvent::Timeout => { /* PTT不使用VAD超时 */ },
                CoreEvent::TimeoutClosed => { /* PTT忽略VAD超时通道关闭 */ },
                CoreEvent::Interrupt(event) => {
                    if event.session_id == session_id && matches!(event.reason, super::simple_interrupt_manager::InterruptReason::ConnectionLost) {
                        info!(
                            "🔄 ASR PTT收到ConnectionLost事件，重置has_sent_completed标志: session={}, event_id={}",
                            session_id, event.event_id
                        );
                        has_sent_completed_atomic.store(false, std::sync::atomic::Ordering::Release);
                        accumulated_text_buffer.clear();
                        conversation_item_created_sent = false;
                        current_user_content_index = 0;
                    }
                },
                CoreEvent::Cleanup => {
                    asr_session.cleanup().await;
                    break;
                },
                CoreEvent::LanguageChanged(new_lang) => {
                    if new_lang != asr_language {
                        info!("🔄 ASR PTT语言变更，重建会话: {:?} -> {:?}", asr_language, new_lang);
                        asr_language = new_lang.clone();
                        match asr_engine
                            .create_session_with_auto_model_selection(
                                session_id.clone(),
                                SpeechMode::PushToTalk,
                                new_lang,
                                current_turn_response_id.clone(),
                            )
                            .await
                        {
                            Ok(new_session) => {
                                asr_session.cleanup().await;
                                asr_session = new_session;
                                if ptt_started {
                                    asr_session.begin_speech();
                                }
                            },
                            Err(e) => {
                                error!("❌ 重建ASR会话失败: {}", e);
                            },
                        }
                    }
                },
                CoreEvent::VadConfigChanged(_) => {
                    // PTT 模式不使用 VAD 运行时配置更新
                },
                CoreEvent::AsrEngineChanged(new_engine) => {
                    if new_engine != current_asr_engine {
                        info!("🔄 [PTT] ASR 引擎变更: {:?} -> {:?}", current_asr_engine, new_engine);
                        if let Some(ref engine) = new_engine {
                            match asr_engine
                                .create_session_with_preferred_model(
                                    session_id.clone(),
                                    SpeechMode::PushToTalk,
                                    asr_language.clone(),
                                    Some(engine.clone()),
                                    current_turn_response_id.clone(),
                                )
                                .await
                            {
                                Ok(new_session) => {
                                    info!("✅ [PTT] ASR 会话已切换到引擎: {}", engine);
                                    asr_session.cleanup().await;
                                    asr_session = new_session;
                                    current_asr_engine = new_engine.clone();
                                },
                                Err(e) => {
                                    error!("❌ [PTT] 重建 ASR 会话失败: {}", e);
                                },
                            }
                        } else {
                            current_asr_engine = None;
                        }
                    } else {
                        debug!("🔄 [PTT] ASR 引擎未变化，跳过重建: {:?}", current_asr_engine);
                    }
                },
                CoreEvent::Closed => {
                    asr_session.cleanup().await;
                    break;
                },
            }
        }

        info!("ASR PTT task finished: {}", session_id);
        let _ = task_completion_tx.send(TaskCompletion::Asr);
        Ok(())
    }
}
