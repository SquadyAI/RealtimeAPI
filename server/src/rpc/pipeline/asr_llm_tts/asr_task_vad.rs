use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::asr::punctuation::{find_last_sentence_terminal, find_last_weak_break, is_only_punctuation};
use crate::asr::stabilizer::{AsrStabilizer, StabilizerConfig};
use crate::asr::{AsrResult, SpeechMode};
use crate::vad::VadState;

use super::asr_task_base::BaseAsrTaskConfig;
use super::asr_task_core::{AsrInputMessage, CoreEvent, poll_core_event};
use super::simple_interrupt_manager::InterruptReason as SimpleInterruptReason;
use super::types::{SimultaneousSegmentConfig, TaskCompletion, TurnContext};

/// VAD-only ASR task (独立实现，保持现VAD语义：段末即触发LLM)
#[allow(clippy::type_complexity)]
pub struct AsrTaskVad {
    pub base: BaseAsrTaskConfig,
    /// VAD 运行时参数更新接收器 (threshold, min_silence_ms, min_speech_ms)
    pub vad_runtime_rx: Option<watch::Receiver<Option<(Option<f32>, Option<u32>, Option<u32>)>>>,
    /// 同传字数断句配置（None = 不启用）
    pub simultaneous_segment_config: Option<SimultaneousSegmentConfig>,
}

impl AsrTaskVad {
    /// 检查是否需要断句（同传模式专用）
    fn check_segmentation(text: &str, weak_threshold: Option<usize>) -> Option<(usize, usize, bool)> {
        if let Some((pos, len)) = find_last_sentence_terminal(text) {
            return Some((pos, len, true));
        }

        if let Some(threshold) = weak_threshold {
            let units = AsrStabilizer::count_semantic_units(text);
            if units >= threshold {
                if let Some((pos, len)) = find_last_weak_break(text) {
                    return Some((pos, len, false));
                }
            }
        }

        None
    }

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

        info!("🎤 ASR VAD task starting for session {}", session_id);

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

        let task_start_instant = Instant::now();
        let mut current_user_item_id: Option<String> = None;
        let mut current_user_content_index: u32 = 0;
        let mut accumulated_text_buffer = String::new();
        let mut conversation_item_created_sent = false;
        let has_sent_completed_atomic = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let last_vad_state_shared = Arc::new(std::sync::Mutex::new(VadState::Silence));

        let current_turn_sequence = Arc::new(std::sync::atomic::AtomicU64::new(0));

        let mut interrupt_rx = simple_interrupt_manager.subscribe();
        let mut asr_language_rx = asr_language_rx;
        let mut asr_engine_rx = Some(shared_flags.asr_engine_notify_rx.clone());

        let simul_segment_config = self.simultaneous_segment_config.clone();

        let stabilizer: Option<Arc<std::sync::Mutex<AsrStabilizer>>> = simul_segment_config.as_ref().filter(|cfg| cfg.enabled).map(|cfg| {
            Arc::new(std::sync::Mutex::new(AsrStabilizer::with_config(StabilizerConfig {
                stability_threshold: cfg.stability_threshold,
                use_semantic_units: true,
                min_stable_units: cfg.min_stable_units,
            })))
        });

        let pending_buffer: Arc<std::sync::Mutex<String>> = Arc::new(std::sync::Mutex::new(String::new()));

        let weak_break_threshold = simul_segment_config
            .as_ref()
            .filter(|cfg| cfg.enabled)
            .map(|cfg| cfg.max_units as usize);

        loop {
            let has_sent_completed_atomic_clone = has_sent_completed_atomic.clone();
            let emitter_callback = emitter.clone();
            let session_id_cb = session_id.clone();
            let current_user_item_id_ref = &mut current_user_item_id;
            let current_user_content_index_ref = &mut current_user_content_index;
            let conversation_item_created_sent_ref = &mut conversation_item_created_sent;
            let accumulated_text_buffer_ref = &mut accumulated_text_buffer;
            let last_vad_state_shared_cl = last_vad_state_shared.clone();
            let current_turn_response_id_for_callback = current_turn_response_id.clone();
            let parallel_tts_tx_for_callback = parallel_tts_tx.clone();
            let shared_flags_for_callback = shared_flags.clone();
            let simple_interrupt_manager_for_cb = simple_interrupt_manager.clone();
            let current_turn_sequence_for_callback = current_turn_sequence.clone();
            let stabilizer_for_cb = stabilizer.clone();
            let pending_buffer_for_cb = pending_buffer.clone();
            let weak_break_threshold_for_cb = weak_break_threshold;
            let mut callback = move |asr_result: AsrResult| {
                let asr_result = {
                    let convert_mode = *shared_flags_for_callback.asr_chinese_convert_mode.read().unwrap();
                    AsrResult {
                        text: crate::text_filters::convert_text(&asr_result.text, convert_mode),
                        ..asr_result
                    }
                };

                // VAD state changes
                {
                    let last_state = {
                        let g = last_vad_state_shared_cl.lock().unwrap();
                        *g
                    };
                    if last_state != asr_result.vad_state {
                        let mut g = last_vad_state_shared_cl.lock().unwrap();
                        *g = asr_result.vad_state;
                        match asr_result.vad_state {
                            VadState::Speaking => {
                                if has_sent_completed_atomic_clone.load(Ordering::Acquire) {
                                    has_sent_completed_atomic_clone.store(false, Ordering::Release);
                                    info!("🔄 [VAD] Silence->Speaking：释放 has_sent_completed（新轮次开始）");
                                }

                                let item_id = if current_user_item_id_ref.is_none() {
                                    let new_item_id = format!("msg_{}", nanoid::nanoid!(6));
                                    *current_user_item_id_ref = Some(new_item_id.clone());
                                    *current_user_content_index_ref = 0;
                                    *conversation_item_created_sent_ref = false;
                                    new_item_id
                                } else {
                                    current_user_item_id_ref.as_ref().unwrap().clone()
                                };
                                let emitter = emitter_callback.clone();
                                let start_ms = task_start_instant.elapsed().as_millis() as u32;
                                tokio::spawn(async move {
                                    emitter.input_audio_buffer_speech_started(&item_id, start_ms).await;
                                });

                                let new_turn_sequence = simple_interrupt_manager_for_cb.start_new_turn();
                                current_turn_sequence_for_callback.store(new_turn_sequence, Ordering::Release);
                                let mgr = simple_interrupt_manager_for_cb.clone();
                                let session_id_clone = session_id_cb.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = mgr.broadcast_global_interrupt_with_turn(
                                        session_id_clone,
                                        super::simple_interrupt_manager::InterruptReason::UserSpeaking,
                                        new_turn_sequence,
                                    ) {
                                        error!("简化机制广播用户说话打断失败: {}", e);
                                    }
                                });
                            },
                            VadState::Silence => {
                                if let Some(ref item_id) = *current_user_item_id_ref {
                                    if !asr_result.text.trim().is_empty() {
                                        let end_ms = task_start_instant.elapsed().as_millis() as u32;
                                        let emitter = emitter_callback.clone();
                                        let id = item_id.clone();
                                        tokio::spawn(async move {
                                            emitter.input_audio_buffer_speech_stopped(&id, end_ms).await;
                                        });
                                    } else {
                                        info!("⚠️ VAD Silence但无文本，跳过speech_stopped避免前端卡住 (item: {})", item_id);

                                        *current_user_item_id_ref = None;
                                        *conversation_item_created_sent_ref = false;
                                        *current_user_content_index_ref = 0;
                                        accumulated_text_buffer_ref.clear();
                                    }
                                }
                            },
                        }
                    }
                }

                if !asr_result.text.trim().is_empty() {
                    if !*conversation_item_created_sent_ref {
                        if let Some(item_id) = current_user_item_id_ref.as_ref() {
                            let emitter_item = emitter_callback.clone();
                            let item_id_clone = item_id.clone();
                            tokio::spawn(async move {
                                emitter_item
                                    .conversation_item_created(&item_id_clone, "user", "in_progress", None)
                                    .await;
                            });
                        }
                        *conversation_item_created_sent_ref = true;
                    }

                    let new_text = asr_result.text.trim();
                    info!(
                        "🔍 [ASR_VAD] 收到文本: '{}', is_partial={}, 直接替换旧文本='{}'",
                        new_text, asr_result.is_partial, accumulated_text_buffer_ref
                    );
                    accumulated_text_buffer_ref.clear();
                    accumulated_text_buffer_ref.push_str(new_text);
                    info!("🔍 [ASR_VAD] 替换后文本: '{}'", accumulated_text_buffer_ref);
                    if asr_result.is_partial
                        && let Some(item_id) = current_user_item_id_ref.as_ref()
                    {
                        // Simultaneous segmentation logic
                        if let Some(ref stabilizer_arc) = stabilizer_for_cb {
                            {
                                let pb = pending_buffer_for_cb.lock().unwrap();
                                if !pb.is_empty() && !accumulated_text_buffer_ref.starts_with(pb.as_str()) {
                                    drop(pb);
                                    let mut pb = pending_buffer_for_cb.lock().unwrap();
                                    info!(
                                        "🔄 [同传断句] ASR 修订检测：pending_buffer='{}' 不是 ASR 文本的前缀，清空",
                                        pb.as_str()
                                    );
                                    pb.clear();
                                }
                            }

                            let stable_increment = {
                                let mut stab = stabilizer_arc.lock().unwrap();
                                stab.process(accumulated_text_buffer_ref)
                            };

                            if let Some(new_stable) = stable_increment {
                                {
                                    let mut pb = pending_buffer_for_cb.lock().unwrap();
                                    pb.push_str(&new_stable);
                                }

                                let pending_text = {
                                    let pb = pending_buffer_for_cb.lock().unwrap();
                                    pb.clone()
                                };

                                if let Some((break_idx, char_len, is_strong)) = Self::check_segmentation(&pending_text, weak_break_threshold_for_cb) {
                                    let text_to_send = &pending_text[..break_idx + char_len];

                                    if !text_to_send.trim().is_empty() && !is_only_punctuation(text_to_send) {
                                        if is_strong {
                                            info!("✂️ [同传断句] 强断句点，发送: '{}'", text_to_send);
                                        } else {
                                            info!(
                                                "✂️ [同传断句] 弱断句点（阈值 {:?}），发送: '{}'",
                                                weak_break_threshold_for_cb, text_to_send
                                            );
                                        }

                                        let user_item_id = current_user_item_id_ref
                                            .clone()
                                            .unwrap_or_else(|| format!("msg_{}", nanoid::nanoid!(6)));
                                        let assistant_item_id = format!("asst_{}", nanoid::nanoid!(6));
                                        let response_id = format!("resp_{}", nanoid::nanoid!(8));

                                        let mut turn_seq = current_turn_sequence_for_callback.load(Ordering::Acquire);
                                        if turn_seq == 0 {
                                            let new_turn = simple_interrupt_manager_for_cb.start_new_turn();
                                            current_turn_sequence_for_callback.store(new_turn, Ordering::Release);
                                            turn_seq = new_turn;
                                        }

                                        let ctx = TurnContext::new(user_item_id.clone(), assistant_item_id, response_id.clone(), Some(turn_seq));
                                        current_turn_response_id_for_callback.store(Some(response_id));

                                        if let Some(parallel_tts_tx) = &parallel_tts_tx_for_callback {
                                            if parallel_tts_tx.send((ctx, text_to_send.to_string())).is_err() {
                                                error!("⚠️ [同传断句] 并行处理任务通道已关闭");
                                            } else {
                                                info!("✅ [同传断句] 部分文本已发送到翻译");
                                            }
                                        }

                                        let emitter_for_segment = emitter_callback.clone();
                                        let segment_text = text_to_send.to_string();
                                        let segment_user_item_id = user_item_id;
                                        let segment_content_index = *current_user_content_index_ref;
                                        *current_user_content_index_ref += 1;
                                        tokio::spawn(async move {
                                            emitter_for_segment
                                                .conversation_item_input_audio_transcription_completed(&segment_user_item_id, segment_content_index, &segment_text)
                                                .await;
                                            info!("📤 [同传断句] 已发送分段 ASR completed: '{}'", segment_text);
                                        });

                                        {
                                            let mut pb = pending_buffer_for_cb.lock().unwrap();
                                            *pb = pending_text[break_idx + char_len..].to_string();
                                        }
                                    }
                                }
                            }
                        }

                        let emitter = emitter_callback.clone();
                        let id = item_id.clone();
                        let idx = *current_user_content_index_ref;
                        let delta_text = asr_result.text.clone();
                        *current_user_content_index_ref += 1;
                        tokio::spawn(async move {
                            emitter
                                .conversation_item_input_audio_transcription_delta(&id, idx, &delta_text)
                                .await;
                        });
                    }
                }

                // End of segment: send to LLM
                if !asr_result.is_partial
                    && !accumulated_text_buffer_ref.trim().is_empty()
                    && !is_only_punctuation(accumulated_text_buffer_ref)
                    && !has_sent_completed_atomic_clone.load(std::sync::atomic::Ordering::Acquire)
                {
                    let text_to_send = if let Some(ref stabilizer_arc) = stabilizer_for_cb {
                        let remaining = {
                            let mut stab = stabilizer_arc.lock().unwrap();
                            let rem = stab.get_remaining(accumulated_text_buffer_ref);
                            stab.reset();
                            rem
                        };
                        let pending = {
                            let mut pb = pending_buffer_for_cb.lock().unwrap();
                            let p = pb.clone();
                            pb.clear();
                            p
                        };
                        let mut combined = pending;
                        if let Some(rem) = remaining {
                            combined.push_str(&rem);
                        }
                        combined
                    } else {
                        accumulated_text_buffer_ref.clone()
                    };

                    if !text_to_send.trim().is_empty() && !is_only_punctuation(&text_to_send) {
                        let should_be_interrupted_response_id = {
                            let current_context = shared_flags_for_callback.assistant_response_context.get_context_copy();
                            current_context.map(|c| c.response_id)
                        };
                        if let Some(ref id) = should_be_interrupted_response_id {
                            current_turn_response_id_for_callback.store(Some(id.clone()));
                        }

                        let user_item_id = current_user_item_id_ref
                            .clone()
                            .unwrap_or_else(|| format!("msg_{}", nanoid::nanoid!(6)));
                        let assistant_item_id = format!("asst_{}", nanoid::nanoid!(6));
                        let response_id = format!("resp_{}", nanoid::nanoid!(8));
                        let mut turn_seq = current_turn_sequence_for_callback.load(Ordering::Acquire);
                        if turn_seq == 0 {
                            let new_turn = simple_interrupt_manager_for_cb.start_new_turn();
                            current_turn_sequence_for_callback.store(new_turn, Ordering::Release);
                            turn_seq = new_turn;
                        }
                        let ctx = TurnContext::new(user_item_id.clone(), assistant_item_id, response_id.clone(), Some(turn_seq));
                        current_turn_response_id_for_callback.store(Some(response_id.clone()));

                        if let Some(parallel_tts_tx) = &parallel_tts_tx_for_callback {
                            info!("🔍 [ASR_VAD] 发送文本到 LLM: '{}'", text_to_send);
                            if parallel_tts_tx.send((ctx, text_to_send.clone())).is_err() {
                                error!("⚠️ 并行处理任务通道已关闭");
                            } else {
                                has_sent_completed_atomic_clone.store(true, std::sync::atomic::Ordering::Release);
                                info!("🔍 [ASR_VAD] 文本已成功发送到 LLM");
                            }
                        } else {
                            error!("❌ 并行处理任务通道未配置");
                        }

                        let emitter = emitter_callback.clone();
                        let final_text_for_completed = text_to_send.clone();
                        let content_index = *current_user_content_index_ref;
                        *current_user_content_index_ref += 1;
                        tokio::spawn(async move {
                            emitter
                                .conversation_item_input_audio_transcription_completed(&user_item_id, content_index, &final_text_for_completed)
                                .await;
                            emitter.conversation_item_updated(&user_item_id, "user", "completed").await;
                            info!(
                                "📤 [段落结束] 发送 ASR completed: '{}' (content_index={})",
                                final_text_for_completed, content_index
                            );
                        });
                    }

                    *conversation_item_created_sent_ref = false;
                    *current_user_content_index_ref = 0;
                    accumulated_text_buffer_ref.clear();
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
                CoreEvent::InputAudio(audio_chunk) => {
                    if let Err(e) = asr_session.process_audio_chunk(audio_chunk, &mut callback).await {
                        error!("处理音频块失败: {}", e);
                    }
                },
                CoreEvent::PttEnd => {
                    info!("🛑 [VAD] 收到 StopInput/PTT End，立即 finalize: {}", session_id);

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
                                debug!("🔁 [VAD] 额外的 PttEnd 已忽略: {}", session_id);
                            },
                            Ok(AsrInputMessage::DirectText(_)) => {
                                warn!("⚠️ [VAD] StopInput 后收到 DirectText，已忽略: {}", session_id);
                            },
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                        }
                    }
                    if drained_audio_chunks > 0 {
                        debug!(
                            "🎧 [VAD] StopInput 前已 drain 尾音音频块数量: {} (session={})",
                            drained_audio_chunks, session_id
                        );
                    }

                    asr_session.stop_timeout_monitor().await;
                    let _ = asr_session.take_timeout_receiver();

                    if has_sent_completed_atomic.load(Ordering::Acquire) {
                        debug!("🛡️ [VAD] StopInput 到达但本段已完成，跳过 finalize: {}", session_id);
                    } else if let Err(e) = asr_session.finalize(&mut callback).await {
                        error!("❌ [VAD] StopInput finalize 失败: {}", e);
                    }
                },
                CoreEvent::Timeout => {
                    info!("⏰ 收到VAD超时事件: {}", session_id);
                    {
                        let mgr = simple_interrupt_manager.clone();
                        let sid = session_id.clone();
                        tokio::spawn(async move {
                            if let Err(e) = mgr.broadcast_global_interrupt(sid, SimpleInterruptReason::SessionTimeout) {
                                error!("简化机制VAD超时打断信号广播失败: {}", e);
                            } else {
                                info!("✅ 简化机制VAD超时打断信号已广播");
                            }
                        });
                    }

                    let has_sent = has_sent_completed_atomic.load(Ordering::Acquire);
                    if has_sent {
                        info!("🛡️ VAD模式已发送completed，跳过finalize以避免重复");
                    } else {
                        let _ = asr_session.finalize(&mut callback).await;
                    }
                },
                CoreEvent::TimeoutClosed => {
                    info!("⏳ VAD超时通道已关闭，移除接收器: {}", session_id);
                    let _ = asr_session.take_timeout_receiver();
                },
                CoreEvent::Interrupt(event) => {
                    if event.session_id == session_id && matches!(event.reason, SimpleInterruptReason::ConnectionLost) {
                        info!("🔌 [VAD] ConnectionLost，清理状态并重置ASR session: {}", session_id);
                        accumulated_text_buffer.clear();
                        conversation_item_created_sent = false;
                        current_user_content_index = 0;
                        current_user_item_id = None;

                        asr_session.reset().await;
                        {
                            let mut g = last_vad_state_shared.lock().unwrap();
                            *g = VadState::Silence;
                        }
                        info!("✅ [VAD] ASR session 已重置，准备接收新音频");
                    }
                },
                CoreEvent::Cleanup => {
                    asr_session.cleanup().await;
                    break;
                },
                CoreEvent::LanguageChanged(new_lang) => {
                    if new_lang != asr_language {
                        info!("🔄 ASR VAD语言变更，重建会话: {:?} -> {:?}", asr_language, new_lang);
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
                                error!("❌ 重建ASR会话失败: {}", e);
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
                        info!("🔄 [VAD] ASR 引擎变更: {:?} -> {:?}", current_asr_engine, new_engine);
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
                                    info!("✅ [VAD] ASR 会话已切换到引擎: {}", engine);
                                    asr_session.cleanup().await;
                                    asr_session = new_session;
                                    current_asr_engine = new_engine.clone();
                                },
                                Err(e) => {
                                    error!("❌ [VAD] 重建 ASR 会话失败: {}", e);
                                },
                            }
                        } else {
                            current_asr_engine = None;
                        }
                    } else {
                        debug!("🔄 [VAD] ASR 引擎未变化，跳过重建: {:?}", current_asr_engine);
                    }
                },
                CoreEvent::Closed => {
                    warn!("⚠️ [ASR-VAD] 输入通道关闭，ASR 任务即将退出: session={}", session_id);
                    asr_session.cleanup().await;
                    break;
                },
            }
        }

        info!("ASR VAD task finished: {}", session_id);
        let _ = task_completion_tx.send(TaskCompletion::Asr);
        Ok(())
    }
}
