//! Pacing algorithm — burst control, send-rate calculation, consumption scheduling.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tracing::{debug, info};

use super::PacedAudioSender;

/// Pure calculation: poll interval (ms) = slice_ms / send_rate_multiplier.
#[inline]
pub fn poll_interval_ms(slice_ms: u32, send_rate_multiplier: f64) -> u64 {
    let interval = slice_ms as f64 / send_rate_multiplier;
    (interval.round() as u64).max(1)
}

/// Pure calculation: audio duration in microseconds for a PCM buffer.
#[inline]
pub fn audio_duration_us(audio_data_len: usize, bytes_per_sample: u32, channels: u32, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    let total_samples = audio_data_len as f64 / (bytes_per_sample as f64 * channels as f64);
    let duration_us = (total_samples * 1_000_000.0) / sample_rate as f64;
    duration_us.round() as u64
}

/// Pure calculation: send delay (us) based on pacing parameters.
#[inline]
pub fn send_delay_us(chunks_sent: usize, initial_burst_count: usize, initial_burst_delay_ms: u64, slice_ms: u32, send_rate_multiplier: f64) -> u64 {
    if chunks_sent < initial_burst_count {
        let burst_ms = initial_burst_delay_ms.max(1);
        return burst_ms * 1000;
    }
    let send_interval_ms = slice_ms as f64 / send_rate_multiplier;
    let delay_us = (send_interval_ms * 1000.0).round() as u64;
    delay_us.max(1_000)
}

impl PacedAudioSender {
    /// Poll interval (ms): slice_ms / send_rate_multiplier.
    #[inline]
    pub(crate) fn calculate_poll_interval_ms(&self) -> u64 {
        poll_interval_ms(self.output_config.slice_ms, self.send_rate_multiplier)
    }

    /// Audio duration in microseconds for a given PCM buffer.
    #[inline]
    pub(crate) fn calculate_audio_duration_us(&self, audio_data: &[u8]) -> u64 {
        audio_duration_us(
            audio_data.len(),
            self.output_config.format.bytes_per_sample(),
            self.channels,
            self.sample_rate,
        )
    }

    /// Send delay (µs) based on configured slice_ms and send_rate_multiplier.
    #[inline]
    pub(crate) fn calculate_send_delay_us(&self) -> u64 {
        send_delay_us(
            self.chunks_sent,
            self.initial_burst_count,
            self.initial_burst_delay_ms,
            self.output_config.slice_ms,
            self.send_rate_multiplier,
        )
    }

    /// Consumer side: send the next audio chunk and return the next scheduled send time.
    pub(crate) async fn send_next_chunk(&mut self) -> Option<Instant> {
        // Check interrupt before sending
        if self.interrupt_handler.as_ref().is_some_and(|h| h.is_interrupted_immutable()) {
            let cleared_count = self.pending_chunks.len();
            self.pending_chunks.clear();
            self.pending_chunk_count.store(0, Ordering::Relaxed);
            self.buffer_start_time = None;

            info!(
                "[{}] 🛑 打断处理：清空消费队列 {} 个包，保留生产缓冲区",
                self.session_id, cleared_count
            );
            return None;
        }

        if self.pending_chunks.is_empty() {
            return None;
        }

        // Consumption strategy
        let should_send = if self.controlled_production {
            true
        } else {
            if let Some(buffer_start_time) = self.buffer_start_time {
                let elapsed_ms = buffer_start_time.elapsed().as_millis() as u32;
                elapsed_ms >= self.max_buffer_ms || self.pending_chunks.len() >= 3
            } else {
                !self.pending_chunks.is_empty()
            }
        };

        if !should_send {
            if let Some(buffer_start_time) = self.buffer_start_time {
                let elapsed_ms = buffer_start_time.elapsed().as_millis() as u32;
                tracing::trace!(
                    "[{}] 🕒 等待缓冲时间: {}ms / {}ms",
                    self.session_id,
                    elapsed_ms,
                    self.max_buffer_ms
                );
            }
            let poll_interval_ms = self.calculate_poll_interval_ms();
            let next_time = if let Some(last_send) = self.last_send_instant {
                last_send + Duration::from_millis(poll_interval_ms)
            } else {
                Instant::now() + Duration::from_millis(poll_interval_ms)
            };
            return Some(next_time);
        }

        // Pre-send interrupt check
        if self.interrupt_handler.as_ref().is_some_and(|h| h.is_interrupted_immutable()) {
            info!("[{}] 🛑 准备发送时检测到打断信号，清空所有待发送并重置状态", self.session_id);
            self.clear_all_buffers_and_reset_state("准备发送时检测到打断信号");

            let mut drained = 0;
            while self.rx.try_recv().is_ok() {
                drained += 1;
            }
            if drained > 0 {
                debug!("[{}] 🔄 额外丢弃 {} 个已在通道中的音频块(发送前打断)", self.session_id, drained);
            }

            return None;
        }

        let planned_send_time = if let Some(last_planned) = self.last_send_instant {
            last_planned
        } else {
            Instant::now()
        };

        if let Some(chunk) = self.pending_chunks.pop_front() {
            let audio_duration_us = if self.output_config.format.is_opus() {
                if chunk.audio_data.is_empty() {
                    0
                } else {
                    (self.output_config.slice_ms as u64) * 1000
                }
            } else {
                self.calculate_audio_duration_us(chunk.audio_data.as_ref())
            };

            if self.chunks_sent.is_multiple_of(100) || self.chunks_sent == 0 {
                tracing::info!(
                    "[{}] Sending chunk #{}: {} bytes, duration: {:.2}ms",
                    self.session_id,
                    self.chunks_sent + 1,
                    chunk.audio_data.len(),
                    audio_duration_us as f64 / 1000.0
                );
            }

            // Final interrupt check before actual send
            if self.interrupt_handler.as_ref().is_some_and(|h| h.is_interrupted_immutable()) {
                info!("[{}] 🛑 发送音频块前检测到打断信号，取消发送", self.session_id);
                self.pending_chunks.push_front(chunk);
                let cleared_count = self.pending_chunks.len();
                self.pending_chunks.clear();
                self.pending_chunk_count.store(0, Ordering::Relaxed);
                self.buffer_start_time = None;

                info!(
                    "[{}] 🛑 打断处理：清空消费队列 {} 个包，保留生产缓冲区",
                    self.session_id, cleared_count
                );
                return None;
            }

            let send_start_time = Instant::now();

            if let Err(e) = self.send_chunk(chunk).await {
                info!("[{}] Failed to send audio chunk: {}", self.session_id, e);
                return None;
            }

            let actual_send_time = Instant::now();
            let processing_delay_us = (actual_send_time - send_start_time).as_micros() as u64;

            self.timing_stats.update_processing_delay(processing_delay_us);
            self.timing_stats.update_cumulative_error(planned_send_time, actual_send_time);

            self.chunks_sent += 1;
            self.pending_chunk_count.fetch_sub(1, Ordering::Relaxed);

            let base_delay_us = self.calculate_send_delay_us();

            // Catch-up: send chunks whose planned time has already passed
            let mut next_planned_time = planned_send_time + Duration::from_micros(base_delay_us);
            let mut now_instant = Instant::now();
            let in_burst = self.chunks_sent < self.initial_burst_count;
            let mut remaining_budget = if in_burst { (self.initial_burst_count - self.chunks_sent).min(8) } else { 1 };
            while now_instant >= next_planned_time && remaining_budget > 0 {
                if self.interrupt_handler.as_ref().is_some_and(|h| h.is_interrupted_immutable()) {
                    info!("[{}] 🛑 补偿发送前检测到打断，停止补发", self.session_id);
                    break;
                }

                if let Some(extra_chunk) = self.pending_chunks.pop_front() {
                    if let Err(e) = self.send_chunk(extra_chunk).await {
                        info!("[{}] 补偿发送失败: {}", self.session_id, e);
                        break;
                    }
                    self.chunks_sent += 1;
                    self.pending_chunk_count.fetch_sub(1, Ordering::Relaxed);
                    next_planned_time += Duration::from_micros(base_delay_us);
                    now_instant = Instant::now();
                    remaining_budget -= 1;
                } else {
                    break;
                }
            }

            // Periodic stability report
            if self.last_timing_report.elapsed() >= Duration::from_secs(5) {
                debug!(
                    "[{}] 🎯 节拍稳定性报告: {}, 基础延迟: {}μs",
                    self.session_id,
                    self.timing_stats.get_stability_report(),
                    base_delay_us
                );
                self.last_timing_report = Instant::now();
            }

            if self.pending_chunks.is_empty() {
                self.buffer_start_time = None;
            }

            self.last_send_instant = Some(next_planned_time);

            // Rebase if planned time is still in the past
            let now_after = Instant::now();
            if next_planned_time <= now_after {
                let lag_us = now_after.saturating_duration_since(next_planned_time).as_micros();
                debug!(
                    "[{}] ⏱️ 计划时间仍在过去，重定位至 now+base_delay: lag_us={}, base_delay_us={}",
                    self.session_id, lag_us, base_delay_us
                );
                let rebased = now_after + Duration::from_micros(base_delay_us);
                next_planned_time = rebased;
                self.last_send_instant = Some(next_planned_time);
            }

            if self.chunks_sent.is_multiple_of(500) {
                debug!(
                    "[{}] 🚀 消费端节拍: 消费模式={}, 下次发送延迟={}μs, 队列剩余={}",
                    self.session_id,
                    if self.controlled_production { "节拍受控" } else { "立即消费" },
                    base_delay_us,
                    self.pending_chunks.len()
                );
            }

            Some(next_planned_time)
        } else {
            None
        }
    }
}
