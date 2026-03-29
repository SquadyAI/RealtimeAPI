mod buffer_manager;
pub mod pacing_algorithm;
pub mod timing_stats;
pub mod types;

pub use types::{PacedAudioChunk, PacingConfig, PrecisionTimingConfig, RealtimeAudioMetadata};

#[cfg(feature = "text-audio")]
use base64::engine::Engine as _;
#[cfg(feature = "text-audio")]
use base64::engine::general_purpose;
use bytes::BytesMut;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::time::Instant as TokioInstant;

use futures::FutureExt;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::rpc::pipeline::asr_llm_tts::LockfreeResponseIdReader;
use crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::SimpleInterruptHandler;
use crate::rpc::{realtime_event, session_router::SessionRouter};

use crate::audio::{AudioFormat, OpusEncoder, OutputAudioConfig};

use timing_stats::TimingStats;

pub struct PacedAudioSender {
    pub(crate) session_id: String,
    pub(crate) router: Arc<SessionRouter>,
    pub(crate) sample_rate: u32,
    pub(crate) channels: u32,
    pub(crate) output_config: OutputAudioConfig,
    pub(crate) rx: mpsc::Receiver<PacedAudioChunk>,
    pub(crate) interrupt_handler: Option<SimpleInterruptHandler>,
    pub(crate) send_rate_multiplier: f64,
    pub(crate) initial_burst_count: usize,
    pub(crate) initial_burst_delay_ms: u64,
    pub(crate) chunks_sent: usize,
    pub(crate) precomputed_frame_size_bytes: usize,

    pub(crate) max_buffer_ms: u32,
    pub(crate) pending_chunks: VecDeque<PacedAudioChunk>,
    pub(crate) buffer_start_time: Option<Instant>,
    pub(crate) pending_chunk_count: Arc<AtomicUsize>,
    pub(crate) merge_buffer: BytesMut,
    pub(crate) merge_metadata: Option<RealtimeAudioMetadata>,
    pub(crate) time_slice_last_send_time: Instant,
    pub(crate) time_slice_next_send_offset_us: u64,
    pub(crate) response_id: Arc<super::asr_llm_tts::LockfreeResponseIdReader>,
    pub(crate) assistant_item_id: Option<String>,
    pub(crate) output_buffer_started: bool,
    pub(crate) first_chunk_received: bool,
    pub(crate) loop_counter: usize,
    pub(crate) last_send_instant: Option<Instant>,
    pub(crate) precision_config: PrecisionTimingConfig,
    pub(crate) timing_stats: TimingStats,
    pub(crate) last_timing_report: Instant,
    pub(crate) is_responding_tx: Option<watch::Sender<bool>>,
    pub(crate) opus_encoder: Option<OpusEncoder>,
    pub(crate) controlled_production: bool,
    pub(crate) last_activity_time: Option<Instant>,
    pub(crate) idle_timeout_ms: u64,

    pub(crate) next_sentence_trigger_tx: Option<mpsc::UnboundedSender<crate::rpc::pipeline::asr_llm_tts::tts_task::NextSentenceTrigger>>,
    pub(crate) current_sentence_idx: usize,
    pub(crate) is_idle: bool,
    pub(crate) pacing_rx: watch::Receiver<PacingConfig>,
    pub(crate) pacing_rx_closed: bool,
    pub(crate) current_output_index: u32,
    pub(crate) final_chunk_processed: bool,
    pub(crate) last_response_id: Option<String>,
    pub(crate) aggregated_text: String,
    pub(crate) text_content_index: u32,
    pub(crate) text_done_sent: bool,
    pub(crate) signal_only: Option<Arc<AtomicBool>>,
    pub(crate) is_translation_mode: bool,
}

impl PacedAudioSender {
    /// Unified interrupt/reset: clear all buffers and reset state.
    pub(crate) fn clear_all_buffers_and_reset_state(&mut self, reason: &str) {
        let cleared_count = self.pending_chunks.len();
        if cleared_count > 0 {
            info!("[{}] 🛑 {}: 清空 {} 个待发送音频块", self.session_id, reason, cleared_count);
        }

        let current_response_id = self.response_id.load();
        if self.output_buffer_started && self.current_output_index > 0 {
            if let Some(response_id) = current_response_id.as_ref() {
                info!(
                    "[{}] 🛑 {}: 音频缓冲区已开始，发送output_audio_buffer.stopped事件",
                    self.session_id, reason
                );

                let wrapped_msg = crate::rpc::realtime_event::wrapped_output_audio_buffer_stopped(self.session_id.clone(), response_id);
                if let Ok(json) = wrapped_msg.to_json() {
                    info!(
                        "📤 [音频事件] session_id={}, event=output_audio_buffer.stopped, response_id={} (打断清理)",
                        self.session_id, response_id
                    );

                    let router = self.router.clone();
                    let session_id = self.session_id.clone();
                    tokio::spawn(async move {
                        let _ = router.send_to_client(&session_id, crate::rpc::WsMessage::Text(json)).await;
                    });
                }
            }
            self.final_chunk_processed = true;
        }

        self.pending_chunks.clear();
        self.merge_buffer.clear();
        self.merge_metadata = None;
        self.buffer_start_time = None;

        self.chunks_sent = 0;
        self.current_output_index = 0;
        self.timing_stats = TimingStats::new();
        self.last_send_instant = None;
        self.first_chunk_received = false;

        self.output_buffer_started = false;
        self.final_chunk_processed = false;

        self.assistant_item_id = None;

        self.pending_chunk_count.store(0, Ordering::Relaxed);

        self.time_slice_next_send_offset_us = 0;

        debug!("[{}] ✅ {}: 所有缓冲区已清空，状态已重置", self.session_id, reason);
    }

    /// Dynamically update assistant_item_id (session-level PacedSender).
    pub async fn update_response_context(&mut self, assistant_item_id: Option<String>) {
        self.assistant_item_id = assistant_item_id.clone();

        let current_response_id = self.response_id.load();
        if current_response_id.is_some() {
            self.output_buffer_started = false;
            self.final_chunk_processed = false;
        }

        if let (Some(resp_id), Some(assist_id)) = (current_response_id, &assistant_item_id) {
            info!(
                "[{}] 🔄 更新PacedSender响应上下文: response_id={}, assistant_item_id={} (已重置音频缓冲区标志)",
                self.session_id, resp_id, assist_id
            );
        }
    }

    /// Unified constructor.
    #[allow(clippy::too_many_arguments, clippy::new_ret_no_self)]
    pub fn new(
        session_id: String,
        router: Arc<SessionRouter>,
        sample_rate: u32,
        channels: u32,
        output_config: OutputAudioConfig,
        buffer_size: usize,
        interrupt_handler: Option<SimpleInterruptHandler>,
        send_rate_multiplier: f64,
        initial_burst_count: usize,
        initial_burst_delay_ms: u64,
        pacing_rx: watch::Receiver<PacingConfig>,
        max_buffer_ms: u32,
        response_id: Arc<LockfreeResponseIdReader>,
        assistant_item_id: Option<String>,
        is_responding_tx: Option<watch::Sender<bool>>,
        controlled_production: bool,
        signal_only: Option<Arc<AtomicBool>>,
        next_sentence_trigger_tx: Option<mpsc::UnboundedSender<crate::rpc::pipeline::asr_llm_tts::tts_task::NextSentenceTrigger>>,
        is_translation_mode: bool,
    ) -> (mpsc::Sender<PacedAudioChunk>, Arc<AtomicUsize>, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(buffer_size);
        let pending_chunk_count = Arc::new(AtomicUsize::new(0));

        let mut corrected_output_config = output_config.clone();
        corrected_output_config.auto_correct();

        if let Err(validation_error) = corrected_output_config.validate() {
            warn!("[{}] 输出音频配置验证失败: {}，使用默认配置", session_id, validation_error);
            corrected_output_config = OutputAudioConfig::default_pcm(20);
        }

        let mut sender = Self {
            session_id: session_id.clone(),
            router,
            sample_rate,
            channels,
            output_config: corrected_output_config.clone(),
            rx,
            interrupt_handler,
            send_rate_multiplier,
            initial_burst_count,
            initial_burst_delay_ms,
            chunks_sent: 0,
            max_buffer_ms,
            pending_chunks: VecDeque::new(),
            buffer_start_time: None,
            pending_chunk_count: pending_chunk_count.clone(),
            merge_buffer: BytesMut::with_capacity(40960),
            merge_metadata: None,
            time_slice_last_send_time: Instant::now(),
            time_slice_next_send_offset_us: 0,
            response_id,
            assistant_item_id,
            output_buffer_started: false,
            first_chunk_received: false,
            loop_counter: 0,
            last_send_instant: None,
            precision_config: PrecisionTimingConfig::default(),
            timing_stats: TimingStats::new(),
            last_timing_report: Instant::now(),
            is_responding_tx,
            precomputed_frame_size_bytes: 0,
            opus_encoder: None,
            controlled_production,
            last_activity_time: None,
            idle_timeout_ms: 2000,
            is_idle: true,
            pacing_rx,
            pacing_rx_closed: false,
            current_output_index: 0,
            final_chunk_processed: false,
            last_response_id: None,
            aggregated_text: String::new(),
            text_content_index: 0,
            text_done_sent: false,
            signal_only,
            next_sentence_trigger_tx,
            current_sentence_idx: 0,
            is_translation_mode,
        };

        sender.update_precomputed_frame_size();
        debug!(
            "[{}] 🎵 音频格式检查：format={:?}, is_opus={}",
            session_id,
            sender.output_config.format,
            sender.output_config.format.is_opus()
        );
        debug!(
            "[{}] 🎵 TTS音频参数：采样率={}Hz, 声道数={}, 发送倍率={:.3}x",
            session_id, sample_rate, channels, send_rate_multiplier
        );
        debug!(
            "[{}] 🎵 缓冲区配置：容量={}bytes, 帧大小={}bytes, 最大缓冲时间={}ms",
            session_id,
            sender.merge_buffer.capacity(),
            sender.precomputed_frame_size_bytes,
            max_buffer_ms
        );
        if sender.output_config.format.is_opus() {
            if let Some(config) = &sender.output_config.opus_config {
                debug!("[{}] 🎵 使用配置初始化Opus编码器: {:?}", session_id, config);
                match OpusEncoder::with_config(config.clone(), sample_rate, channels as u16) {
                    Ok(encoder) => {
                        info!(
                            "[{}] ✅ Opus编码器初始化成功: {}kHz, {}声道, {}ms帧长",
                            session_id,
                            sample_rate,
                            channels,
                            config.frame_duration_ms.unwrap_or(20)
                        );
                        sender.opus_encoder = Some(encoder);
                    },
                    Err(err) => {
                        error!(
                            "[{}] ❌ Opus编码器初始化失败: {} (配置: {:?}, 采样率: {}Hz, 声道: {})",
                            session_id, err, config, sample_rate, channels
                        );
                        warn!("[{}] 🔄 因编码器初始化失败回退到PCM格式", session_id);
                        sender.output_config.format = AudioFormat::PcmS16Le;
                    },
                }
            } else {
                error!("[{}] ❌ Opus格式但缺少opus_config配置", session_id);
                warn!("[{}] 🔄 因缺少Opus配置回退到PCM格式", session_id);
                sender.output_config.format = AudioFormat::PcmS16Le;
            }
        } else {
            debug!(
                "[{}] 🎵 非Opus格式，无需初始化编码器：format={:?}",
                session_id, sender.output_config.format
            );
        }

        let handle = tokio::spawn(async move {
            sender.run().await;
        });

        (tx, pending_chunk_count, handle)
    }

    /// Record activity time and exit idle state.
    pub(crate) fn record_activity(&mut self) {
        self.last_activity_time = Some(Instant::now());
        if self.is_idle {
            self.is_idle = false;
            debug!("[{}] 🔄 退出空闲状态，恢复活跃轮询", self.session_id);
        }
    }

    /// Check whether to enter idle state.
    pub(crate) fn check_idle_state(&mut self) -> bool {
        let should_idle = self.merge_buffer.is_empty()
            && self.pending_chunks.is_empty()
            && self
                .last_activity_time
                .map(|t| t.elapsed().as_millis() > self.idle_timeout_ms as u128)
                .unwrap_or(true);

        if should_idle && !self.is_idle {
            self.is_idle = true;
            info!(
                "[{}] 😴 进入空闲状态，暂停轮询（无数据{}ms）",
                self.session_id, self.idle_timeout_ms
            );
        } else if !should_idle && self.is_idle {
            self.is_idle = false;
            debug!("[{}] 🔄 退出空闲状态，恢复活跃轮询", self.session_id);
        }

        self.is_idle
    }

    /// Check whether a timer should be active.
    pub(crate) fn should_have_timer(&self) -> bool {
        !self.is_idle && (!self.merge_buffer.is_empty() || !self.pending_chunks.is_empty())
    }

    async fn run(&mut self) {
        debug!(
            "[{}] PacedAudioSender started with {}x speed, burst: {} chunks @ {}ms, max_buffer: {}ms",
            self.session_id, self.send_rate_multiplier, self.initial_burst_count, self.initial_burst_delay_ms, self.max_buffer_ms
        );

        debug!(
            "[{}] 🎯 精准节拍模式已启用: 误差阈值={}μs, 最大处理延迟={}μs",
            self.session_id, self.precision_config.error_threshold_us, self.precision_config.max_processing_delay_us
        );

        info!("[{}] 🚀 PacedAudioSender 主循环已启动，等待音频数据...", self.session_id);

        let mut next_send_time: Option<Instant> = None;
        let mut _is_actively_sending = false;

        loop {
            self.loop_counter += 1;

            let debug_interval = if self.is_idle { 10000 } else { 1000 };
            if self.loop_counter.is_multiple_of(debug_interval) {
                debug!(
                    "[{}] PacedAudioSender 运行状态 #{}：pending={}, sent={}, is_idle={}",
                    self.session_id,
                    self.loop_counter,
                    self.pending_chunks.len(),
                    self.chunks_sent,
                    self.is_idle
                );
            }

            if !self.is_idle && self.loop_counter.is_multiple_of(2000) {
                debug!(
                    "[{}] 🎵 主循环音频格式检查 #{}：format={:?}, is_opus={}, 编码器存在={}",
                    self.session_id,
                    self.loop_counter,
                    self.output_config.format,
                    self.output_config.format.is_opus(),
                    self.opus_encoder.is_some()
                );
            }

            let has_interrupt_handler = self.interrupt_handler.is_some();

            let interrupt_future = if let Some(handler) = &mut self.interrupt_handler {
                handler.wait_for_interrupt().boxed()
            } else {
                futures::future::pending().boxed()
            };

            let timer_future = if let Some(scheduled_time) = next_send_time {
                tokio::time::sleep_until(TokioInstant::from_std(scheduled_time)).boxed()
            } else {
                futures::future::pending().boxed()
            };

            let idle_wait_future = if self.is_idle {
                tokio::time::sleep(Duration::from_secs(30)).boxed()
            } else {
                futures::future::pending().boxed()
            };

            tokio::select! {
                biased;

                // High priority: interrupt signal
                interrupt_event_opt = interrupt_future, if has_interrupt_handler => {
                    match interrupt_event_opt {
                        Some(_event) => {
                            if self.interrupt_handler.as_ref().map(|h| h.is_interrupted_immutable()).unwrap_or(false) {
                                self.clear_all_buffers_and_reset_state("打断信号收到");

                                let mut drained = 0;
                                while self.rx.try_recv().is_ok() {
                                    drained += 1;
                                }
                                if drained > 0 {
                                    debug!("[{}] 🔄 额外丢弃 {} 个已在通道中的音频块", self.session_id, drained);
                                }

                                if let Some(h) = self.interrupt_handler.as_mut() {
                                    h.clear_interrupt_state();
                                }
                                info!("[{}] 🔄 PacedSender已处理完打断信号，清除打断状态", self.session_id);
                                continue;
                            }
                        }
                        None => {
                            info!("[{}] interrupt channel closed, PacedAudioSender exiting", self.session_id);

                            let remaining_count = self.pending_chunks.len();
                            if remaining_count > 0 {
                                info!("[{}] 🔧 中断时处理剩余 {} 个音频块（可能包含is_final块）", self.session_id, remaining_count);

                                while let Some(chunk) = self.pending_chunks.pop_front() {
                                    if chunk.is_final {
                                        if let Err(e) = self.send_chunk(chunk).await {
                                            warn!("[{}] 发送final块失败: {}", self.session_id, e);
                                        }
                                        break;
                                    }
                                }
                            }

                            self.clear_all_buffers_and_reset_state("中断通道关闭");
                            break;
                        }
                    }
                }

                // Timer-driven send
                _ = timer_future => {
                    if !self.should_have_timer() {
                        next_send_time = None;
                        self.check_idle_state();
                        continue;
                    }

                    self.try_generate_packets_from_buffer();
                    next_send_time = self.send_next_chunk().await;

                    if next_send_time.is_none() {
                        self.check_idle_state();
                    }
                }

                // Runtime pacing parameter updates
                res = self.pacing_rx.changed(), if !self.pacing_rx_closed => {
                    if res.is_ok() {
                        let cfg = self.pacing_rx.borrow().clone();
                        let old_mul = self.send_rate_multiplier;
                        let old_burst = self.initial_burst_count;
                        let old_delay = self.initial_burst_delay_ms;

                        let new_mul = cfg.send_rate_multiplier;
                        let new_burst = cfg.initial_burst_count;
                        let new_delay = cfg.initial_burst_delay_ms;

                        if (old_mul == new_mul) && (old_burst == new_burst) && (old_delay == new_delay) {
                            debug!(
                                "[{}] 🔄 节拍参数无变化，忽略: multiplier {:.3}, burst {}, delay {}ms",
                                self.session_id, new_mul, new_burst, new_delay
                            );
                        } else {
                            self.send_rate_multiplier = new_mul;
                            self.initial_burst_count = new_burst;
                            self.initial_burst_delay_ms = new_delay;

                            info!(
                                "[{}] 🔄 应用节拍参数更新: multiplier {:.3}→{:.3}, burst {}→{}, delay {}ms→{}ms",
                                self.session_id,
                                old_mul, self.send_rate_multiplier,
                                old_burst, self.initial_burst_count,
                                old_delay, self.initial_burst_delay_ms
                            );

                            self.last_send_instant = None;

                            if self.should_have_timer() {
                                let poll_ms = self.calculate_poll_interval_ms();
                                next_send_time = Some(Instant::now() + Duration::from_millis(poll_ms));
                            }
                        }
                    } else {
                        self.pacing_rx_closed = true;
                        info!("[{}] ⛔ pacing_rx 发送端已关闭，停止监听参数更新", self.session_id);
                    }
                }

                // Upstream audio input (lowest priority)
                chunk_opt = self.rx.recv() => {
                    match chunk_opt {
                        Some(chunk) => {
                            if !self.first_chunk_received {
                                if !chunk.audio_data.is_empty() {
                                    info!(
                                        "[{}] 🎉 PacedAudioSender 首次收到音频数据: {} bytes",
                                        self.session_id,
                                        chunk.audio_data.len()
                                    );
                                    self.first_chunk_received = true;
                                    _is_actively_sending = true;

                                    let paced_sender_first_audio_time = std::time::Instant::now();
                                    let session_id_for_timing = self.session_id.clone();
                                    let response_id_for_timing = self.response_id.load();

                                    tokio::spawn(async move {
                                        let resp_id_str = response_id_for_timing.as_deref();
                                        crate::rpc::pipeline::asr_llm_tts::timing_manager::record_node_time_and_try_report(
                                            &session_id_for_timing,
                                            crate::rpc::pipeline::asr_llm_tts::timing_manager::TimingNode::PacedSenderFirstAudio,
                                            paced_sender_first_audio_time,
                                            resp_id_str
                                        )
                                        .await;
                                    });
                                } else {
                                    debug!(
                                        "[{}] 🧹 忽略空音频包（清理/控制信号），不标记为首包",
                                        self.session_id
                                    );
                                }
                            }

                            if self.assistant_item_id.is_none()
                                && let Some(ref meta) = chunk.realtime_metadata {
                                    self.assistant_item_id = Some(meta.assistant_item_id.clone());
                                }

                            if self.interrupt_handler.as_ref().is_some_and(|h| h.is_interrupted_immutable()) {
                                debug!("[{}] 🛑 打断中丢弃上游音频输入: {} bytes", self.session_id, chunk.audio_data.len());
                            } else {
                                self.receive_audio_data(chunk);
                            }

                            if next_send_time.is_none() && self.should_have_timer() {
                                let next_time = if let Some(last_planned) = self.last_send_instant {
                                    last_planned
                                } else {
                                    Instant::now()
                                };

                                next_send_time = Some(next_time);

                                info!("[{}] 🚀 启动活跃轮询模式: 缓冲区={}bytes, 待发送包={}个, 空闲状态={}",
                                     self.session_id, self.merge_buffer.len(), self.pending_chunks.len(), self.is_idle);
                            }
                        }
                        None => {
                            info!("[{}] 📪 PacedAudioSender 音频通道已关闭，音频处理任务将重启", self.session_id);
                            info!("[{}] ℹ️ 这是正常的TtsClient切换，直接丢弃残留pending，避免播放过期音频", self.session_id);
                            self.clear_all_buffers_and_reset_state("音频通道关闭");
                            break;
                        }
                    }
                }

                // Idle wait
                _ = idle_wait_future, if self.is_idle => {
                    if self.check_idle_state() {
                        tracing::trace!("[{}] 😴 空闲状态持续，继续休眠等待", self.session_id);
                        continue;
                    } else {
                        debug!("[{}] 🔄 空闲状态被新数据唤醒", self.session_id);
                        if self.should_have_timer() {
                            next_send_time = Some(Instant::now());
                        }
                    }
                }
            }
        }
        info!(
            "[{}] PacedAudioSender stopped. Total chunks sent: {}",
            self.session_id, self.chunks_sent
        );

        if let Some(tx) = &self.is_responding_tx {
            let _ = tx.send(false);
            info!("[{}] 🔊 PacedAudioSender 完成播放，设置 is_responding = false", self.session_id);
        }
    }

    /// Send a single audio chunk to the client with all associated events.
    async fn send_chunk(&mut self, chunk: PacedAudioChunk) -> Result<(), String> {
        // Check interrupt before sending
        if self.interrupt_handler.as_ref().is_some_and(|h| h.is_interrupted_immutable()) {
            info!(
                "[{}] 🛑 send_chunk检测到打断信号，取消发送音频块: {} bytes",
                self.session_id,
                chunk.audio_data.len()
            );
            return Err("发送被打断".to_string());
        }

        let is_empty = chunk.audio_data.is_empty();

        // On-demand TTS: trigger next sentence generation before sending final chunk
        let should_trigger_next = chunk.is_final && (!is_empty || !chunk.turn_final);
        if should_trigger_next && let Some(ref tx) = self.next_sentence_trigger_tx {
            let trigger = crate::rpc::pipeline::asr_llm_tts::tts_task::NextSentenceTrigger { current_sentence_idx: self.current_sentence_idx };
            if tx.send(trigger).is_err() {
                debug!("[{}] ⚠️ 下一句触发信号发送失败（接收端已关闭）", self.session_id);
            } else {
                if is_empty {
                    info!(
                        "[{}] 🎯 事务性触发（空final）：即将处理句子{}的final控制块，触发下一句生成",
                        self.session_id, self.current_sentence_idx
                    );
                } else {
                    info!(
                        "[{}] 🎯 事务性触发：即将发送句子{}的is_final音频块，触发下一句生成",
                        self.session_id, self.current_sentence_idx
                    );
                }
                self.current_sentence_idx += 1;
            }
        }

        // Detect response_id switch → reset turn state
        if let Some(ref meta) = chunk.realtime_metadata {
            let incoming_resp_id = &meta.response_id;
            if self.last_response_id.as_ref() != Some(incoming_resp_id) {
                self.output_buffer_started = false;
                self.final_chunk_processed = false;
                self.current_output_index = 0;
                self.last_response_id = Some(incoming_resp_id.clone());
                self.aggregated_text.clear();
                self.text_content_index = 0;
                self.text_done_sent = false;
                debug!(
                    "[{}] 🔄 检测到response切换: last={:?} -> new={}, 复位started/final/index",
                    self.session_id, self.last_response_id, incoming_resp_id
                );
            }
        }

        // Handle empty-final-only scenario (no delta in this turn)
        if chunk.is_final && is_empty && !self.final_chunk_processed {
            if !chunk.turn_final {
                return Ok(());
            }
            let response_id_opt = if let Some(ref meta) = chunk.realtime_metadata {
                Some(meta.response_id.clone())
            } else {
                self.response_id.load()
            };

            if let Some(resp_id) = response_id_opt {
                let had_activity = self.output_buffer_started || self.text_content_index > 0 || self.current_output_index > 0;
                if had_activity {
                    self.output_buffer_started = false;
                    self.final_chunk_processed = true;

                    let signal_only_on = self.signal_only.as_ref().is_some_and(|f| f.load(Ordering::Acquire));
                    if !self.text_done_sent && !signal_only_on && !self.is_translation_mode && !self.aggregated_text.is_empty() {
                        let (assist_id_for_done, content_idx_for_done) = if let Some(ref meta) = chunk.realtime_metadata {
                            let effective_item_id = if !meta.assistant_item_id.is_empty() {
                                meta.assistant_item_id.clone()
                            } else if let Some(ref cached) = self.assistant_item_id {
                                cached.clone()
                            } else {
                                let new_id = format!("asst_{}", nanoid::nanoid!(6));
                                self.assistant_item_id = Some(new_id.clone());
                                new_id
                            };
                            (effective_item_id, meta.content_index)
                        } else {
                            (self.assistant_item_id.clone().unwrap_or_default(), self.text_content_index)
                        };

                        let text_done_msg = crate::rpc::realtime_event::wrapped_response_text_done(
                            self.session_id.clone(),
                            &resp_id,
                            &assist_id_for_done,
                            0,
                            content_idx_for_done,
                            &self.aggregated_text,
                        );
                        if let Ok(json) = text_done_msg.to_json() {
                            info!(
                                "📤 [文字事件] session_id={}, event=response.text.done (空final，仅在已started且有聚合文本时发送)",
                                self.session_id
                            );
                            let _ = self
                                .router
                                .send_to_client(&self.session_id, crate::rpc::WsMessage::Text(json))
                                .await;
                        }
                        self.text_done_sent = true;
                    }

                    let stopped_msg = realtime_event::wrapped_output_audio_buffer_stopped(self.session_id.clone(), &resp_id);
                    if let Ok(json) = stopped_msg.to_json() {
                        info!(
                            "📤 [音频事件] session_id={}, event=output_audio_buffer.stopped, response_id={} (空final，收尾)",
                            self.session_id, resp_id
                        );
                        let _ = self
                            .router
                            .send_to_client(&self.session_id, crate::rpc::WsMessage::Text(json))
                            .await;
                    }

                    self.aggregated_text.clear();
                    self.text_content_index = 0;
                } else {
                    self.output_buffer_started = false;
                    self.final_chunk_processed = true;
                    let stopped_msg = realtime_event::wrapped_output_audio_buffer_stopped(self.session_id.clone(), &resp_id);
                    if let Ok(json) = stopped_msg.to_json() {
                        info!(
                            "📤 [音频事件] session_id={}, event=output_audio_buffer.stopped, response_id={} (空final，无输出补发)",
                            self.session_id, resp_id
                        );
                        let _ = self
                            .router
                            .send_to_client(&self.session_id, crate::rpc::WsMessage::Text(json))
                            .await;
                    }
                    self.aggregated_text.clear();
                    self.text_content_index = 0;
                }
                self.last_response_id = Some(resp_id);
            } else {
                debug!(
                    "[{}] ⚠️ 收到空final chunk但缺少response_id，跳过started/stopped发送",
                    self.session_id
                );
            }

            return Ok(());
        }

        // Send output_audio_buffer.started before first non-empty audio
        if !is_empty && !self.output_buffer_started {
            let response_id_opt = if let Some(ref meta) = chunk.realtime_metadata {
                Some(meta.response_id.clone())
            } else {
                self.response_id.load()
            };

            if let Some(resp_id) = response_id_opt {
                if let Some(ref meta) = chunk.realtime_metadata {
                    let current_global_response_id = self.response_id.load();
                    if current_global_response_id.as_ref() != Some(&meta.response_id) {
                        self.final_chunk_processed = false;
                        info!(
                            "[{}] 🔄 新响应开始，复位 final_chunk_processed: response_id={}",
                            self.session_id, resp_id
                        );
                    }
                }

                self.output_buffer_started = true;
                self.current_output_index = 0;

                let wrapped_msg = realtime_event::wrapped_output_audio_buffer_started(self.session_id.clone(), &resp_id);
                if let Ok(json) = wrapped_msg.to_json() {
                    let _ = self
                        .router
                        .send_to_client(&self.session_id, crate::rpc::WsMessage::Text(json))
                        .await;
                }
            }
        }

        // Send response.audio.delta event
        if let Some(metadata) = &chunk.realtime_metadata {
            let is_final = chunk.is_final;

            // Send associated text (if any, and not signal_only, and not translation mode)
            let signal_only_on = self.signal_only.as_ref().is_some_and(|f| f.load(Ordering::Acquire));
            if !signal_only_on
                && !self.is_translation_mode
                && let Some(sentence_text) = &chunk.sentence_text
                && !sentence_text.is_empty()
            {
                if !self.output_buffer_started {
                    self.output_buffer_started = true;
                    self.current_output_index = 0;
                    let wrapped_msg = realtime_event::wrapped_output_audio_buffer_started(self.session_id.clone(), &metadata.response_id);
                    if let Ok(json) = wrapped_msg.to_json() {
                        let _ = self
                            .router
                            .send_to_client(&self.session_id, crate::rpc::WsMessage::Text(json))
                            .await;
                    }
                }
                if self.interrupt_handler.as_ref().is_some_and(|h| h.is_interrupted_immutable()) {
                    info!("[{}] 🛑 文字发送前检测到打断信号，取消发送", self.session_id);
                    return Err("发送前被打断".to_string());
                }

                let assistant_item_id_for_event = if !metadata.assistant_item_id.is_empty() {
                    metadata.assistant_item_id.clone()
                } else if let Some(ref cached) = self.assistant_item_id {
                    cached.clone()
                } else {
                    let new_id = format!("asst_{}", nanoid::nanoid!(6));
                    self.assistant_item_id = Some(new_id.clone());
                    new_id
                };

                let text_delta_msg = realtime_event::wrapped_response_text_delta(
                    self.session_id.clone(),
                    &metadata.response_id,
                    &assistant_item_id_for_event,
                    0,
                    metadata.content_index,
                    sentence_text,
                );
                if let Ok(json) = text_delta_msg.to_json() {
                    info!(
                        "📤 [文字事件] session_id={}, event=response.text.delta, text='{}' (与音频同步)",
                        self.session_id,
                        sentence_text.chars().take(50).collect::<String>()
                    );
                    let _ = self
                        .router
                        .send_to_client(&self.session_id, crate::rpc::WsMessage::Text(json))
                        .await;
                }
                self.aggregated_text.push_str(sentence_text);
                self.text_content_index = self.text_content_index.saturating_add(1);
            }

            // Non-empty audio → send delta
            if !is_empty {
                if self.interrupt_handler.as_ref().is_some_and(|h| h.is_interrupted_immutable()) {
                    info!("[{}] 🛑 消息发送前检测到打断信号，取消发送", self.session_id);
                    return Err("发送前被打断".to_string());
                }

                let output_index_to_send: u32 = self.current_output_index;
                self.current_output_index = self.current_output_index.saturating_add(1);

                #[cfg(all(feature = "binary-audio", feature = "text-audio"))]
                {
                    compile_error!("不能同时启用 binary-audio 和 text-audio features，请只选择其中一个");
                }

                #[cfg(all(not(feature = "binary-audio"), not(feature = "text-audio")))]
                {
                    compile_error!("必须启用 binary-audio 或 text-audio 中的一个 feature");
                }

                #[cfg(feature = "binary-audio")]
                {
                    let assistant_item_id_for_event = if !metadata.assistant_item_id.is_empty() {
                        metadata.assistant_item_id.clone()
                    } else if let Some(ref cached) = self.assistant_item_id {
                        cached.clone()
                    } else {
                        let new_id = format!("asst_{}", nanoid::nanoid!(6));
                        self.assistant_item_id = Some(new_id.clone());
                        new_id
                    };
                    match realtime_event::create_response_audio_delta_binary_message(
                        self.session_id.clone(),
                        &metadata.response_id,
                        &assistant_item_id_for_event,
                        output_index_to_send,
                        metadata.content_index,
                        chunk.audio_data.as_ref(),
                    ) {
                        Ok(binary_msg) => {
                            if let Ok(binary_bytes) = binary_msg.to_bytes() {
                                let _ = self
                                    .router
                                    .send_to_client(
                                        &self.session_id,
                                        crate::rpc::WsMessage::Binary(bytes::Bytes::from(binary_bytes)),
                                    )
                                    .await;
                            }
                        },
                        Err(e) => {
                            error!("❌ 创建二进制音频delta消息失败（已禁用回退）: {}", e);
                            return Err(format!("创建二进制音频delta消息失败: {}", e));
                        },
                    }
                }

                #[cfg(feature = "text-audio")]
                {
                    let assistant_item_id_for_event = if !metadata.assistant_item_id.is_empty() {
                        metadata.assistant_item_id.clone()
                    } else if let Some(ref cached) = self.assistant_item_id {
                        cached.clone()
                    } else {
                        let new_id = format!("asst_{}", nanoid::nanoid!(6));
                        self.assistant_item_id = Some(new_id.clone());
                        new_id
                    };
                    let b64_audio = general_purpose::STANDARD.encode(chunk.audio_data.as_ref());
                    let wrapped_msg = realtime_event::wrapped_response_audio_delta(
                        self.session_id.clone(),
                        &metadata.response_id,
                        &assistant_item_id_for_event,
                        output_index_to_send,
                        metadata.content_index,
                        &b64_audio,
                    );
                    if let Ok(json) = wrapped_msg.to_json() {
                        let _ = self
                            .router
                            .send_to_client(&self.session_id, crate::rpc::WsMessage::Text(json))
                            .await;
                    }
                }
            }

            // Final chunk with turn_final: send text.done then stopped
            if is_final && chunk.turn_final {
                info!(
                    "🔍 [PacedSender收到turn_final] is_final={}, turn_final={}, response_id={}, session={}",
                    is_final,
                    chunk.turn_final,
                    chunk
                        .realtime_metadata
                        .as_ref()
                        .map(|m| m.response_id.as_str())
                        .unwrap_or("unknown"),
                    self.session_id
                );
                self.output_buffer_started = false;
                self.final_chunk_processed = true;

                if !self.text_done_sent && !self.is_translation_mode {
                    let signal_only_on = self.signal_only.as_ref().is_some_and(|f| f.load(Ordering::Acquire));
                    if !signal_only_on {
                        let assistant_item_id_for_event = if !metadata.assistant_item_id.is_empty() {
                            metadata.assistant_item_id.clone()
                        } else if let Some(ref cached) = self.assistant_item_id {
                            cached.clone()
                        } else {
                            let new_id = format!("asst_{}", nanoid::nanoid!(6));
                            self.assistant_item_id = Some(new_id.clone());
                            new_id
                        };
                        let text_done_msg = crate::rpc::realtime_event::wrapped_response_text_done(
                            self.session_id.clone(),
                            &metadata.response_id,
                            &assistant_item_id_for_event,
                            0,
                            metadata.content_index,
                            &self.aggregated_text,
                        );
                        if let Ok(json) = text_done_msg.to_json() {
                            info!(
                                "📤 [文字事件] session_id={}, event=response.text.done (final块，在stopped之前)",
                                self.session_id
                            );
                            let _ = self
                                .router
                                .send_to_client(&self.session_id, crate::rpc::WsMessage::Text(json))
                                .await;
                        }
                    } else {
                        debug!(
                            "[{}] 🚫 signal_only=true，抑制发送 response.text.done: response_id={}",
                            self.session_id, metadata.response_id
                        );
                    }
                    self.text_done_sent = true;
                }

                let buffer_stopped_msg = realtime_event::wrapped_output_audio_buffer_stopped(self.session_id.clone(), &metadata.response_id);
                if let Ok(json) = buffer_stopped_msg.to_json() {
                    info!(
                        "📤 [音频事件] session_id={}, event=output_audio_buffer.stopped, response_id={} (来自is_final块，在text.done之后)",
                        self.session_id, metadata.response_id
                    );
                    let _ = self
                        .router
                        .send_to_client(&self.session_id, crate::rpc::WsMessage::Text(json))
                        .await;
                }

                self.aggregated_text.clear();
                self.text_content_index = 0;
            }
        }

        // Fallback: no metadata but final+turn_final → send text.done then stopped
        if chunk.realtime_metadata.is_none() && chunk.is_final && chunk.turn_final {
            self.output_buffer_started = false;
            self.final_chunk_processed = true;

            let resp_id_opt = self.response_id.load().or_else(|| self.last_response_id.clone());

            if let Some(resp_id) = resp_id_opt {
                if !self.text_done_sent && !self.is_translation_mode {
                    let signal_only_on = self.signal_only.as_ref().is_some_and(|f| f.load(Ordering::Acquire));
                    if !signal_only_on {
                        let assistant_item_id_for_event = if let Some(ref cached) = self.assistant_item_id {
                            cached.clone()
                        } else {
                            let new_id = format!("asst_{}", nanoid::nanoid!(6));
                            self.assistant_item_id = Some(new_id.clone());
                            new_id
                        };

                        let text_done_msg = crate::rpc::realtime_event::wrapped_response_text_done(
                            self.session_id.clone(),
                            &resp_id,
                            &assistant_item_id_for_event,
                            0,
                            self.text_content_index,
                            &self.aggregated_text,
                        );
                        if let Ok(json) = text_done_msg.to_json() {
                            info!(
                                "📤 [文字事件] session_id={}, event=response.text.done (fallback，无metadata，final在stopped之前)",
                                self.session_id
                            );
                            let _ = self
                                .router
                                .send_to_client(&self.session_id, crate::rpc::WsMessage::Text(json))
                                .await;
                        }
                    }
                    self.text_done_sent = true;
                }

                let stopped_msg = realtime_event::wrapped_output_audio_buffer_stopped(self.session_id.clone(), &resp_id);
                if let Ok(json) = stopped_msg.to_json() {
                    info!(
                        "📤 [音频事件] session_id={}, event=output_audio_buffer.stopped, response_id={} (fallback，无metadata)",
                        self.session_id, resp_id
                    );
                    let _ = self
                        .router
                        .send_to_client(&self.session_id, crate::rpc::WsMessage::Text(json))
                        .await;
                }

                self.aggregated_text.clear();
                self.text_content_index = 0;
            } else {
                warn!(
                    "[{}] ⚠️ 无法发送stopped（缺少response_id，且无last_response_id），final且无metadata",
                    self.session_id
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::pipeline::asr_llm_tts::LockfreeResponseId;
    use bytes::Bytes;

    #[tokio::test]
    async fn test_opus_format_creation() {
        let opus_format = AudioFormat::default_opus();

        assert!(opus_format.is_opus());
        assert!(!opus_format.is_pcm());
        assert_eq!(opus_format.bytes_per_sample(), 2);

        println!("✅ Opus 格式创建和方法测试通过");
    }

    #[test]
    fn test_audio_format_conversion() {
        let pcm_format = AudioFormat::parse("pcm");
        assert_eq!(pcm_format, AudioFormat::PcmS16Le);

        let opus_format = AudioFormat::parse("opus");
        assert!(opus_format.is_opus());
        assert!(opus_format.is_opus());

        let default_pcm = AudioFormat::default_pcm();
        assert_eq!(default_pcm, AudioFormat::PcmS16Le);

        let default_opus = AudioFormat::default_opus();
        assert!(default_opus.is_opus());

        println!("✅ AudioFormat 转换测试通过");
    }

    #[test]
    fn test_opus_config_creation() {
        let opus_config = OutputAudioConfig::default_opus(20);
        assert_eq!(opus_config.format, AudioFormat::Opus);
        assert_eq!(opus_config.slice_ms, 20);
        assert_eq!(opus_config.opus_config.as_ref().unwrap().frame_duration_ms, Some(20));

        let custom_opus_cfg = crate::audio::OpusEncoderConfig { bitrate: 64000, frame_duration_ms: Some(10), ..Default::default() };
        let custom_config = OutputAudioConfig::opus(10, custom_opus_cfg);
        assert_eq!(custom_config.slice_ms, 10);
        assert_eq!(custom_config.opus_config.as_ref().unwrap().bitrate, 64000);

        println!("✅ Opus 配置创建测试通过");
    }

    #[test]
    fn test_opus_frame_size_estimation() {
        let expected_size = (32000 * 20) / (8 * 1000);
        assert_eq!(expected_size, 80);

        let expected_size_10ms = (32000 * 10) / (8 * 1000);
        assert_eq!(expected_size_10ms, 40);

        println!("✅ Opus 帧大小估算测试通过");
    }

    #[test]
    fn test_opus_frame_duration_validation() {
        assert!(AudioFormat::validate_opus_frame_duration(5));
        assert!(AudioFormat::validate_opus_frame_duration(10));
        assert!(AudioFormat::validate_opus_frame_duration(20));
        assert!(AudioFormat::validate_opus_frame_duration(40));
        assert!(AudioFormat::validate_opus_frame_duration(60));

        assert!(!AudioFormat::validate_opus_frame_duration(15));
        assert!(!AudioFormat::validate_opus_frame_duration(25));
        assert!(!AudioFormat::validate_opus_frame_duration(30));

        println!("✅ Opus 帧时长验证测试通过");
    }

    #[test]
    fn test_closest_opus_frame_duration() {
        assert_eq!(AudioFormat::closest_opus_frame_duration(8), 10);
        assert_eq!(AudioFormat::closest_opus_frame_duration(15), 10);
        assert_eq!(AudioFormat::closest_opus_frame_duration(18), 20);
        assert_eq!(AudioFormat::closest_opus_frame_duration(25), 20);
        assert_eq!(AudioFormat::closest_opus_frame_duration(35), 40);
        assert_eq!(AudioFormat::closest_opus_frame_duration(50), 40);

        assert_eq!(AudioFormat::closest_opus_frame_duration(1), 5);
        assert_eq!(AudioFormat::closest_opus_frame_duration(100), 60);

        println!("✅ Opus 最接近帧时长选择测试通过");
    }

    #[test]
    fn test_time_slice_configuration() {
        let pcm_format = AudioFormat::PcmS16Le;
        assert!(pcm_format.is_pcm());
        assert!(!pcm_format.is_opus());

        let opus_format = AudioFormat::default_opus();
        assert!(!opus_format.is_pcm());
        assert!(opus_format.is_opus());

        let standards = AudioFormat::opus_standard_frame_durations();
        assert!(standards.contains(&20));
        assert!(standards.contains(&10));
        assert!(!standards.contains(&15));

        println!("✅ 时间片配置区别测试通过");
    }

    #[test]
    fn test_precomputed_frame_size() {
        let mut sender = PacedAudioSender {
            session_id: "test".to_string(),
            router: Arc::new(SessionRouter::new(Duration::from_secs(300))),
            sample_rate: 16000,
            channels: 1,
            output_config: OutputAudioConfig::default_pcm(20),
            rx: mpsc::channel(100).1,
            interrupt_handler: None,
            send_rate_multiplier: 1.0,
            initial_burst_count: 3,
            initial_burst_delay_ms: 10,
            chunks_sent: 0,
            max_buffer_ms: 100,
            pending_chunks: VecDeque::new(),
            buffer_start_time: None,
            pending_chunk_count: Arc::new(AtomicUsize::new(0)),
            merge_buffer: BytesMut::new(),
            merge_metadata: None,
            time_slice_last_send_time: Instant::now(),
            next_sentence_trigger_tx: None,
            current_sentence_idx: 0,
            time_slice_next_send_offset_us: 0,
            response_id: Arc::new(LockfreeResponseIdReader::from_writer(&LockfreeResponseId::new())),
            assistant_item_id: None,
            output_buffer_started: false,
            first_chunk_received: false,
            loop_counter: 0,
            last_send_instant: None,
            precision_config: PrecisionTimingConfig::default(),
            timing_stats: TimingStats::new(),
            last_timing_report: Instant::now(),
            is_responding_tx: None,
            precomputed_frame_size_bytes: 0,
            opus_encoder: None,
            controlled_production: true,
            last_activity_time: None,
            idle_timeout_ms: 2000,
            is_idle: true,
            pacing_rx: watch::channel(PacingConfig { send_rate_multiplier: 1.0, initial_burst_count: 0, initial_burst_delay_ms: 0 }).1,
            pacing_rx_closed: false,
            current_output_index: 0,
            final_chunk_processed: false,
            last_response_id: None,
            aggregated_text: String::new(),
            text_content_index: 0,
            text_done_sent: false,
            signal_only: None,
            is_translation_mode: false,
        };

        sender.update_precomputed_frame_size();

        // 16kHz, 1ch, 16-bit PCM, 20ms = 16000 * 1 * 2 * 20 / 1000 = 640 bytes
        assert_eq!(sender.precomputed_frame_size_bytes, 640);

        sender.output_config.format = AudioFormat::PcmS24Le;
        sender.update_precomputed_frame_size();
        // 24-bit PCM: 16000 * 1 * 3 * 20 / 1000 = 960 bytes
        assert_eq!(sender.precomputed_frame_size_bytes, 960);

        sender.channels = 2;
        sender.update_precomputed_frame_size();
        // Stereo 24-bit PCM: 16000 * 2 * 3 * 20 / 1000 = 1920 bytes
        assert_eq!(sender.precomputed_frame_size_bytes, 1920);
    }

    #[tokio::test]
    async fn test_empty_final_chunk_handling() {
        let (_tx, rx) = mpsc::channel(100);
        let pending_chunk_count = Arc::new(AtomicUsize::new(0));

        let mut sender = PacedAudioSender {
            session_id: "test_session".to_string(),
            router: Arc::new(SessionRouter::new(Duration::from_secs(300))),
            sample_rate: 16000,
            channels: 1,
            output_config: OutputAudioConfig::default_pcm(20),
            rx,
            interrupt_handler: None,
            send_rate_multiplier: 1.0,
            initial_burst_count: 3,
            initial_burst_delay_ms: 10,
            chunks_sent: 0,
            max_buffer_ms: 100,
            pending_chunks: VecDeque::new(),
            buffer_start_time: None,
            pending_chunk_count: pending_chunk_count.clone(),
            merge_buffer: BytesMut::new(),
            merge_metadata: None,
            time_slice_last_send_time: Instant::now(),
            next_sentence_trigger_tx: None,
            current_sentence_idx: 0,
            time_slice_next_send_offset_us: 0,
            response_id: Arc::new(LockfreeResponseIdReader::from_writer(&LockfreeResponseId::new())),
            assistant_item_id: None,
            output_buffer_started: true,
            first_chunk_received: true,
            loop_counter: 0,
            last_send_instant: None,
            precision_config: PrecisionTimingConfig::default(),
            timing_stats: TimingStats::new(),
            last_timing_report: Instant::now(),
            is_responding_tx: None,
            precomputed_frame_size_bytes: 640,
            opus_encoder: None,
            controlled_production: true,
            last_activity_time: None,
            idle_timeout_ms: 2000,
            is_idle: false,
            pacing_rx: watch::channel(PacingConfig { send_rate_multiplier: 1.0, initial_burst_count: 0, initial_burst_delay_ms: 0 }).1,
            pacing_rx_closed: false,
            current_output_index: 0,
            final_chunk_processed: false,
            last_response_id: None,
            aggregated_text: String::new(),
            text_content_index: 0,
            text_done_sent: false,
            signal_only: None,
            is_translation_mode: false,
        };

        let empty_final_chunk = PacedAudioChunk {
            sentence_text: None,
            audio_data: Bytes::new(),
            is_final: true,
            realtime_metadata: Some(RealtimeAudioMetadata {
                response_id: "test_response_123".to_string(),
                assistant_item_id: "test_assistant_456".to_string(),
                output_index: 0,
                content_index: 0,
            }),
            turn_final: true,
        };

        sender.receive_audio_data(empty_final_chunk);

        assert_eq!(sender.pending_chunks.len(), 1);
        assert_eq!(pending_chunk_count.load(Ordering::Relaxed), 1);

        if let Some(chunk) = sender.pending_chunks.front() {
            assert!(chunk.is_final);
            assert!(chunk.audio_data.is_empty());
            assert!(chunk.realtime_metadata.is_some());
        } else {
            panic!("Expected final chunk in pending_chunks");
        }

        println!("✅ 空final chunk处理测试通过");
    }
}
