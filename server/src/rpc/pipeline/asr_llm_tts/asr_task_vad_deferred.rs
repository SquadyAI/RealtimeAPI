//! ASR task (VAD-based) that defers sending to LLM until StopInput
//!
//! 设计目的：
//! - 云端仍进行 VAD 分段与中间识别/最终识别，并向客户端发送语音开始/结束与转录事件
//! - 但不会在每个段落结束时立即把文本发给 LLM，而是等待 StopInput 信令到达后一次性发送
//! - 不处理/不响应 500ms 接收超时（不监听 VAD 超时事件）
//!
//! 原理：
//! - 保留 VAD 模式的大部分链路（不是 PTT 模式）
//! - 在 VAD Speaking/Silence 状态变更时发送 input_audio_buffer.* 事件
//! - 中间结果仅以 delta 事件发送；段落结束时发送 transcription.completed 事件，但不触发 LLM
//! - StopInput 到达后调用 AsrSession.finalize() 刷尾音频，并将累计的文本一次性发送至 LLM

use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::asr::{AsrResult, SpeechMode};
use crate::vad::VadState;

use super::asr_task_base::{BaseAsrTaskConfig, is_only_punctuation, remove_consecutive_duplicates};
use super::asr_task_core::{CoreEvent, poll_core_event};
use super::simple_interrupt_manager::InterruptReason as SimpleInterruptReason;
use super::types::{TaskCompletion, TurnContext};

#[allow(clippy::type_complexity)]
pub struct AsrTaskVadDeferred {
    pub base: BaseAsrTaskConfig,
    /// VAD 运行时参数更新接收器 (threshold, min_silence_ms, min_speech_ms)
    pub vad_runtime_rx: Option<watch::Receiver<Option<(Option<f32>, Option<u32>, Option<u32>)>>>,
}

impl AsrTaskVadDeferred {
    pub async fn run(mut self) -> Result<()> {
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

        info!("🎤 [Deferred] ASR VAD-deferred task starting: {}", session_id);

        let preferred_asr_engine = shared_flags.preferred_asr_engine.lock().unwrap().clone();
        let mut current_asr_engine = preferred_asr_engine.clone();

        let mut asr_session = asr_engine
            .create_session_with_preferred_model(
                session_id.clone(),
                SpeechMode::Vad,
                asr_language.clone(),
                preferred_asr_engine,
                current_turn_response_id.clone(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("创建 ASR 会话失败: {}", e))?;

        const DEFERRED_VAD_TIMEOUT_MS: u64 = 1000;
        asr_session.start_timeout_monitor_with(DEFERRED_VAD_TIMEOUT_MS).await;

        let aggregated_for_llm = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let current_user_item_id = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let mut conversation_item_created_sent = false;
        let mut current_user_content_index: u32 = 0;
        let mut last_vad_state = VadState::Silence;

        let mut asr_language_rx = asr_language_rx;
        let mut asr_engine_rx = Some(shared_flags.asr_engine_notify_rx.clone());
        let mut interrupt_rx = simple_interrupt_manager.subscribe();

        let task_start = Instant::now();
        let current_turn_sequence = Arc::new(std::sync::atomic::AtomicU64::new(0));

        let mut last_speech_end_time: Option<Instant> = None;
        let mut current_speech_start_time: Option<Instant> = None;
        const MIN_SPEECH_DURATION_MS: u128 = 150;
        const MAX_GAP_FROM_LAST_MS: u128 = 300;

        loop {
            let emitter_cb = emitter.clone();
            let session_id_cb = session_id.clone();
            let current_user_item_id_clone = current_user_item_id.clone();
            let conversation_item_created_sent_ref = &mut conversation_item_created_sent;
            let current_user_content_index_ref = &mut current_user_content_index;
            let aggregated_for_llm_clone = aggregated_for_llm.clone();
            let last_vad_state_ref = &mut last_vad_state;
            let last_speech_end_time_ref = &mut last_speech_end_time;
            let current_speech_start_time_ref = &mut current_speech_start_time;
            let parallel_tts_tx_for_callback = parallel_tts_tx.clone();
            let current_turn_response_id_for_cb = current_turn_response_id.clone();

            let simple_interrupt_manager_for_cb = simple_interrupt_manager.clone();
            let current_turn_sequence_for_cb = current_turn_sequence.clone();
            let shared_flags_for_callback = shared_flags.clone();
            let mut callback = move |asr_result: AsrResult| {
                let asr_result = {
                    let convert_mode = *shared_flags_for_callback.asr_chinese_convert_mode.read().unwrap();
                    AsrResult {
                        text: crate::text_filters::convert_text(&asr_result.text, convert_mode),
                        ..asr_result
                    }
                };

                // 1) VAD state changes
                let state_changed = *last_vad_state_ref != asr_result.vad_state;
                if state_changed {
                    let prev = *last_vad_state_ref;
                    *last_vad_state_ref = asr_result.vad_state;

                    match asr_result.vad_state {
                        VadState::Speaking => {
                            *current_speech_start_time_ref = Some(Instant::now());

                            let item_id = {
                                let mut guard = current_user_item_id_clone.lock().unwrap();
                                if guard.is_none() {
                                    let id = format!("msg_{}", nanoid::nanoid!(6));
                                    *guard = Some(id.clone());
                                    *current_user_content_index_ref = 0;
                                    *conversation_item_created_sent_ref = false;
                                    id
                                } else {
                                    guard.as_ref().unwrap().clone()
                                }
                            };

                            let emitter = emitter_cb.clone();
                            let start_ms = task_start.elapsed().as_millis() as u32;
                            let item_id_clone = item_id.clone();
                            tokio::spawn(async move {
                                emitter.input_audio_buffer_speech_started(&item_id_clone, start_ms).await;
                            });

                            let new_turn_sequence = simple_interrupt_manager_for_cb.start_new_turn();
                            current_turn_sequence_for_cb.store(new_turn_sequence, Ordering::Release);
                            let mgr = simple_interrupt_manager_for_cb.clone();
                            let session_id_clone = session_id_cb.clone();
                            tokio::spawn(async move {
                                let start = std::time::Instant::now();
                                match mgr.broadcast_global_interrupt_with_turn(session_id_clone, SimpleInterruptReason::UserSpeaking, new_turn_sequence) {
                                    Ok(()) => {
                                        info!(
                                            "✅ [Deferred] 简化机制打断信号已广播 (耗时: {:?}) - 使用新轮次: {}",
                                            start.elapsed(),
                                            new_turn_sequence
                                        );
                                    },
                                    Err(e) => {
                                        error!("❌ [Deferred] 广播用户说话打断失败: {}", e);
                                    },
                                }
                            });
                        },
                        VadState::Silence => {
                            if prev == VadState::Speaking {
                                *last_speech_end_time_ref = Some(Instant::now());

                                let user_item_id_opt = {
                                    let guard = current_user_item_id_clone.lock().unwrap();
                                    guard.clone()
                                };
                                if let Some(ref user_item_id) = user_item_id_opt {
                                    let emitter = emitter_cb.clone();
                                    let end_ms = task_start.elapsed().as_millis() as u32;
                                    let id = user_item_id.clone();
                                    tokio::spawn(async move {
                                        emitter.input_audio_buffer_speech_stopped(&id, end_ms).await;
                                    });
                                }
                            }
                        },
                    }
                }

                // 2) Text events
                if !asr_result.text.trim().is_empty() {
                    let user_item_id_opt = {
                        let guard = current_user_item_id_clone.lock().unwrap();
                        guard.clone()
                    };

                    if !*conversation_item_created_sent_ref {
                        if let Some(ref user_item_id) = user_item_id_opt {
                            let emitter = emitter_cb.clone();
                            let id = user_item_id.clone();
                            tokio::spawn(async move {
                                emitter.conversation_item_created(&id, "user", "in_progress", None).await;
                            });
                            *conversation_item_created_sent_ref = true;
                        }
                    }

                    if let Some(ref user_item_id) = user_item_id_opt {
                        let emitter = emitter_cb.clone();
                        let id = user_item_id.clone();
                        let content_index = *current_user_content_index_ref;
                        let text = asr_result.text.clone();

                        if asr_result.is_partial {
                            *current_user_content_index_ref += 1;
                            tokio::spawn(async move {
                                emitter
                                    .conversation_item_input_audio_transcription_delta(&id, content_index, &text)
                                    .await;
                            });
                        } else {
                            if !is_only_punctuation(&text) {
                                let mut guard = aggregated_for_llm_clone.lock().unwrap();
                                let new_text = text.trim();
                                info!("🔍 [Deferred] 追加段落文本: existing='{}', new='{}'", guard, new_text);
                                if !guard.is_empty() && !new_text.is_empty() {
                                    let last_char = guard.chars().last();
                                    let first_char = new_text.chars().next();
                                    let needs_space = match (last_char, first_char) {
                                        (Some(a), Some(b)) => a.is_alphanumeric() && b.is_alphanumeric(),
                                        _ => false,
                                    };
                                    if needs_space {
                                        guard.push(' ');
                                    }
                                }
                                guard.push_str(new_text);
                            }

                            let should_send_event = {
                                let now = Instant::now();
                                let speech_duration_ms = current_speech_start_time_ref
                                    .map(|start| now.duration_since(start).as_millis())
                                    .unwrap_or(u128::MAX);
                                let gap_from_last_ms = last_speech_end_time_ref
                                    .and_then(|last_end| {
                                        current_speech_start_time_ref.map(|start| {
                                            if start > last_end {
                                                start.duration_since(last_end).as_millis()
                                            } else {
                                                u128::MAX
                                            }
                                        })
                                    })
                                    .unwrap_or(u128::MAX);

                                let is_short_segment = speech_duration_ms < MIN_SPEECH_DURATION_MS && gap_from_last_ms < MAX_GAP_FROM_LAST_MS;

                                if is_short_segment {
                                    info!(
                                        "🔇 [Deferred] 过滤超短语音段: duration={}ms, gap={}ms, text='{}' (阈值: duration<{}ms, gap<{}ms)",
                                        speech_duration_ms, gap_from_last_ms, text, MIN_SPEECH_DURATION_MS, MAX_GAP_FROM_LAST_MS
                                    );
                                    false
                                } else {
                                    true
                                }
                            };

                            if should_send_event {
                                let emitter_done = emitter_cb.clone();
                                let id_done = id.clone();
                                let txt_done = text.clone();
                                tokio::spawn(async move {
                                    emitter_done
                                        .conversation_item_input_audio_transcription_completed(&id_done, 0, &txt_done)
                                        .await;
                                });
                            }

                            if let Some(ref tx) = parallel_tts_tx_for_callback {
                                let turn_seq = current_turn_sequence_for_cb.load(Ordering::Acquire);
                                if turn_seq > 0 && !is_only_punctuation(&text) {
                                    let assistant_item_id = format!("asst_{}", nanoid::nanoid!(6));
                                    let response_id = format!("resp_{}", nanoid::nanoid!(8));
                                    let ctx = TurnContext::new(id.clone(), assistant_item_id, response_id.clone(), Some(turn_seq));
                                    current_turn_response_id_for_cb.store(Some(response_id));

                                    let deduped_text = remove_consecutive_duplicates(&text);
                                    info!(
                                        "📤 [Deferred] 发送 ASR 最终结果到翻译任务: '{}' (原文: '{}')",
                                        deduped_text, text
                                    );
                                    let _ = tx.send((ctx, deduped_text));
                                }
                            }
                        }
                    } else {
                        warn!("[Deferred] 收到文本但没有 user_item_id，text='{}'", asr_result.text);
                    }
                }
            };

            match poll_core_event(
                &mut asr_session,
                &mut input_rx,
                &mut interrupt_rx,
                &mut cleanup_rx,
                &mut asr_language_rx,
                &mut self.vad_runtime_rx,
                &mut asr_engine_rx,
            )
            .await
            {
                CoreEvent::InputAudio(chunk) => {
                    asr_session.start_timeout_monitor_with(DEFERRED_VAD_TIMEOUT_MS).await;
                    if let Err(e) = asr_session.process_audio_chunk(chunk, &mut callback).await {
                        error!("处理音频块失败: {}", e);
                    }
                },
                CoreEvent::PttEnd => {
                    info!("🛑 [Deferred] 收到 StopInput，准备发送累积文本并 finalize: {}", session_id);

                    let pre_finalize_text = {
                        let guard = aggregated_for_llm.lock().unwrap();
                        guard.trim().to_string()
                    };

                    let user_item_id = {
                        let guard = current_user_item_id.lock().unwrap();
                        guard.clone().unwrap_or_else(|| format!("msg_{}", nanoid::nanoid!(6)))
                    };
                    let mut turn_seq = current_turn_sequence.load(Ordering::Acquire);
                    if turn_seq == 0 {
                        let new_turn = simple_interrupt_manager.start_new_turn();
                        current_turn_sequence.store(new_turn, Ordering::Release);
                        turn_seq = new_turn;
                    }

                    let mut any_text_sent = false;

                    if !pre_finalize_text.is_empty() && !is_only_punctuation(&pre_finalize_text) {
                        let assistant_item_id = format!("asst_{}", nanoid::nanoid!(6));
                        let response_id = format!("resp_{}", nanoid::nanoid!(8));
                        let ctx = TurnContext::new(user_item_id.clone(), assistant_item_id, response_id.clone(), Some(turn_seq));
                        current_turn_response_id.store(Some(response_id.clone()));
                        if let Some(ref tx) = parallel_tts_tx {
                            info!("📤 [Deferred] 发送 finalize 前文本: '{}'", pre_finalize_text);
                            let _ = tx.send((ctx, pre_finalize_text.clone()));
                            any_text_sent = true;
                        }

                        info!("🔄 [Deferred] 已发送完整文本，reset ASR 避免处理新轮次音频残留");
                        asr_session.reset().await;
                    }

                    {
                        let mut guard = aggregated_for_llm.lock().unwrap();
                        guard.clear();
                    }

                    if let Err(e) = asr_session.finalize(&mut callback).await {
                        error!("finalize 失败: {}", e);
                    }

                    asr_session.stop_timeout_monitor().await;

                    let post_finalize_text = {
                        let guard = aggregated_for_llm.lock().unwrap();
                        guard.trim().to_string()
                    };

                    if !post_finalize_text.is_empty() && !is_only_punctuation(&post_finalize_text) {
                        let assistant_item_id = format!("asst_{}", nanoid::nanoid!(6));
                        let response_id = format!("resp_{}", nanoid::nanoid!(8));
                        let ctx = TurnContext::new(user_item_id.clone(), assistant_item_id, response_id.clone(), Some(turn_seq));
                        current_turn_response_id.store(Some(response_id.clone()));
                        if let Some(ref tx) = parallel_tts_tx {
                            info!("📤 [Deferred] 发送 finalize 后文本: '{}'", post_finalize_text);
                            let _ = tx.send((ctx, post_finalize_text.clone()));
                            any_text_sent = true;
                        }
                    }

                    if any_text_sent {
                        let emitter_mark = emitter.clone();
                        let id_mark = user_item_id.clone();
                        tokio::spawn(async move {
                            emitter_mark.conversation_item_updated(&id_mark, "user", "completed").await;
                        });
                    } else {
                        info!("[Deferred] StopInput 到达但聚合文本为空或仅标点，跳过发送 LLM");
                    }

                    {
                        let mut guard = aggregated_for_llm.lock().unwrap();
                        guard.clear();
                    }
                    {
                        let mut guard = current_user_item_id.lock().unwrap();
                        *guard = None;
                    }
                    conversation_item_created_sent = false;
                    current_user_content_index = 0;
                    last_speech_end_time = None;
                    current_speech_start_time = None;
                },
                CoreEvent::Timeout => {
                    info!("⏳ [Deferred] 忽略1s静默超时事件，等待StopInput");
                    let _ = asr_session.take_timeout_receiver();
                },
                CoreEvent::TimeoutClosed => {
                    info!("⏳ [Deferred] 超时通道已关闭，移除接收器");
                    let _ = asr_session.take_timeout_receiver();
                },
                CoreEvent::Interrupt(event) => {
                    if event.session_id == session_id && matches!(event.reason, SimpleInterruptReason::ConnectionLost) {
                        info!("🔌 [Deferred] ConnectionLost，清理当前聚合文本与段状态: {}", session_id);
                        {
                            let mut guard = aggregated_for_llm.lock().unwrap();
                            guard.clear();
                        }
                        {
                            let mut guard = current_user_item_id.lock().unwrap();
                            *guard = None;
                        }
                        conversation_item_created_sent = false;
                        current_user_content_index = 0;
                        last_speech_end_time = None;
                        current_speech_start_time = None;

                        asr_session.reset().await;
                        last_vad_state = VadState::Silence;
                        info!("✅ [Deferred] ASR session 已重置，准备接收新音频");
                    }
                },
                CoreEvent::Cleanup => {
                    info!("🧹 [Deferred] 清理 ASR 会话: {}", session_id);
                    asr_session.cleanup().await;
                    break;
                },
                CoreEvent::LanguageChanged(new_lang) => {
                    if new_lang != asr_language {
                        asr_language = new_lang.clone();
                        match asr_engine
                            .create_session_with_auto_model_selection(session_id.clone(), SpeechMode::Vad, new_lang, current_turn_response_id.clone())
                            .await
                        {
                            Ok(new_session) => {
                                asr_session.cleanup().await;
                                asr_session = new_session;
                            },
                            Err(e) => {
                                error!("❌ 重建 ASR 会话失败: {}", e);
                            },
                        }
                    }
                },
                CoreEvent::VadConfigChanged(cfg) => {
                    if let Some((th, sil, sp)) = cfg {
                        asr_session.update_vad_params(th, sil, sp);
                    }
                },
                CoreEvent::AsrEngineChanged(new_engine) => {
                    if new_engine != current_asr_engine {
                        info!("🔄 [Deferred] ASR 引擎变更: {:?} -> {:?}", current_asr_engine, new_engine);
                        if let Some(ref engine) = new_engine {
                            match asr_engine
                                .create_session_with_preferred_model(
                                    session_id.clone(),
                                    SpeechMode::Vad,
                                    asr_language.clone(),
                                    Some(engine.clone()),
                                    current_turn_response_id.clone(),
                                )
                                .await
                            {
                                Ok(new_session) => {
                                    info!("✅ [Deferred] ASR 会话已切换到引擎: {}", engine);
                                    asr_session.cleanup().await;
                                    asr_session = new_session;
                                    current_asr_engine = new_engine.clone();
                                },
                                Err(e) => {
                                    error!("❌ [Deferred] 重建 ASR 会话失败: {}", e);
                                },
                            }
                        } else {
                            current_asr_engine = None;
                        }
                    } else {
                        debug!("🔄 [Deferred] ASR 引擎未变化，跳过重建: {:?}", current_asr_engine);
                    }
                },
                CoreEvent::Closed => {
                    asr_session.cleanup().await;
                    break;
                },
            }
        }

        info!("[Deferred] ASR task finished: {}", session_id);
        let _ = task_completion_tx.send(TaskCompletion::Asr);
        Ok(())
    }
}
