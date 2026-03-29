//! Buffer management — merge buffer, auto-expansion, packet generation, config updates.

use bytes::{Bytes, BytesMut};
use std::sync::atomic::Ordering;
use std::time::Instant;
use tracing::{debug, error, info, warn};

use crate::audio::{AudioFormat, OpusEncoder, OutputAudioConfig, bytes_to_f32_vec};

use super::PacedAudioSender;
use super::types::PacedAudioChunk;

impl PacedAudioSender {
    /// Data receiver: accepts audio data and appends to merge buffer.
    pub(crate) fn receive_audio_data(&mut self, chunk: PacedAudioChunk) {
        self.record_activity();

        // Handle text-only empty chunks (SENTENCE_START events) — enqueue directly
        if let Some(ref text) = chunk.sentence_text {
            if !chunk.is_final && chunk.audio_data.is_empty() {
                info!(
                    "[{}] 📨 收到带文字的空块，直接加入发送队列: text='{}'",
                    self.session_id,
                    text.chars().take(50).collect::<String>()
                );
                self.pending_chunks.push_back(chunk);
                self.pending_chunk_count.fetch_add(1, Ordering::Relaxed);
                if self.buffer_start_time.is_none() {
                    self.buffer_start_time = Some(Instant::now());
                }
                return;
            }
        }

        // If chunk carries text AND audio, separate the text into its own empty chunk
        if let Some(text) = &chunk.sentence_text
            && !text.is_empty()
            && !chunk.audio_data.is_empty()
        {
            info!(
                "[{}] 📨 收到带文字的音频块，分离文字为单独空块入队: text='{}'",
                self.session_id,
                text.chars().take(50).collect::<String>()
            );
            let text_chunk = PacedAudioChunk {
                audio_data: Bytes::new(),
                is_final: false,
                realtime_metadata: chunk.realtime_metadata.clone(),
                sentence_text: chunk.sentence_text.clone(),
                turn_final: false,
            };
            self.pending_chunks.push_back(text_chunk);
            self.pending_chunk_count.fetch_add(1, Ordering::Relaxed);

            if self.buffer_start_time.is_none() {
                self.buffer_start_time = Some(Instant::now());
            }
        }

        // Handle final chunk with empty audio data
        if chunk.is_final && chunk.audio_data.is_empty() {
            self.process_final_data();
            self.try_generate_packets_from_buffer();

            if !self.pending_chunks.is_empty() {
                if let Some(last_chunk) = self.pending_chunks.back_mut() {
                    last_chunk.is_final = true;
                    last_chunk.turn_final = chunk.turn_final;
                    if last_chunk.realtime_metadata.is_none() {
                        last_chunk.realtime_metadata = chunk.realtime_metadata.clone();
                    }
                }
                debug!(
                    "[{}] 🎵 处理空final: 将最后一个待发送分片标记为final，避免重复 started/delta",
                    self.session_id
                );
            } else {
                self.pending_chunks.push_back(chunk);
                self.pending_chunk_count.fetch_add(1, Ordering::Relaxed);
                if self.buffer_start_time.is_none() {
                    self.buffer_start_time = Some(Instant::now());
                }
                debug!(
                    "[{}] 🎵 处理空final: 无剩余数据，保留空final分片以触发 stopped",
                    self.session_id
                );
            }

            return;
        }

        // Auto-expand buffer if needed
        let required_capacity = self.merge_buffer.len() + chunk.audio_data.len();
        if required_capacity > self.merge_buffer.capacity() {
            self.check_and_expand_buffer_if_needed_for_size(required_capacity);
        }

        self.add_chunk_to_buffer(chunk);

        // Immediately try to generate packets (supports initial burst)
        self.try_generate_packets_from_buffer();
    }

    /// Auto-expand the merge buffer to the required capacity.
    pub(crate) fn check_and_expand_buffer_if_needed_for_size(&mut self, required_capacity: usize) {
        let current_capacity = self.merge_buffer.capacity();

        if required_capacity > current_capacity {
            let new_capacity = (required_capacity as f64 * 1.5) as usize;
            let mut new_buffer = BytesMut::with_capacity(new_capacity);
            new_buffer.extend_from_slice(&self.merge_buffer);
            self.merge_buffer = new_buffer;
        }
    }

    /// Append an audio chunk to the merge buffer.
    pub(crate) fn add_chunk_to_buffer(&mut self, chunk: PacedAudioChunk) {
        self.merge_buffer.extend_from_slice(chunk.audio_data.as_ref());

        if self.merge_metadata.is_none() {
            self.merge_metadata = chunk.realtime_metadata.clone();
        }

        if chunk.is_final {
            self.process_final_data();
        }
    }

    /// Process final data: pad (Opus) or flush remaining samples (PCM).
    pub(crate) fn process_final_data(&mut self) {
        let frame_size_bytes = self.precomputed_frame_size_bytes;

        if self.output_config.format.is_opus() {
            self.try_generate_packets_from_buffer();

            if !self.merge_buffer.is_empty() && self.merge_buffer.len() < frame_size_bytes {
                let remaining_size = self.merge_buffer.len();
                let padding_size = frame_size_bytes - remaining_size;
                let padding = vec![0u8; padding_size];
                self.merge_buffer.extend_from_slice(&padding);

                info!(
                    "[{}] 🔧 最终帧填充(Opus): 原始={}bytes, 填充={}bytes, 最终={}bytes",
                    self.session_id, remaining_size, padding_size, frame_size_bytes
                );
            }
        } else {
            // PCM: flush remaining samples as a variable-length final packet
            if !self.merge_buffer.is_empty() {
                let remaining = self.merge_buffer.len();
                let pcm_frame_data = self.merge_buffer.split_to(remaining).freeze();

                let final_chunk = PacedAudioChunk {
                    audio_data: pcm_frame_data,
                    is_final: true,
                    realtime_metadata: self.merge_metadata.take(),
                    sentence_text: None,
                    turn_final: false,
                };

                self.pending_chunks.push_back(final_chunk);
                self.pending_chunk_count.fetch_add(1, Ordering::Relaxed);

                info!(
                    "[{}] 🎵 PCM最终分片(不填充): {}bytes (直接发送final)",
                    self.session_id, remaining
                );
            } else if let Some(last_chunk) = self.pending_chunks.back_mut() {
                last_chunk.is_final = true;
                last_chunk.turn_final = false;
                self.merge_metadata = None;
                info!("[{}] 🎵 PCM最终标记: 将最后一个待发送分片标记为final", self.session_id);
            }
        }

        // If final packets were generated, reset time-slice state for immediate flush
        if !self.pending_chunks.is_empty() {
            self.time_slice_next_send_offset_us = 0;
            self.last_send_instant = None;
        }
    }

    /// Producer side: generate all available full-frame packets from the merge buffer.
    pub(crate) fn try_generate_packets_from_buffer(&mut self) {
        if self.merge_buffer.is_empty() || self.merge_metadata.is_none() {
            return;
        }

        let available_frame_count = self.merge_buffer.len() / self.precomputed_frame_size_bytes;

        if available_frame_count == 0 {
            return;
        }

        let packets_to_send = available_frame_count;

        if packets_to_send == 0 {
            return;
        }

        for _ in 0..packets_to_send {
            let frame_size_bytes = self.precomputed_frame_size_bytes;

            if self.merge_buffer.len() >= frame_size_bytes {
                let pcm_frame_data = self.merge_buffer.split_to(frame_size_bytes).freeze();

                let output_audio_data = if self.output_config.format.is_opus() {
                    let pcm_conversion_result = bytes_to_f32_vec(pcm_frame_data.as_ref());
                    let pcm_samples = match pcm_conversion_result {
                        Ok(samples) => samples.0,
                        Err(err_msg) => {
                            warn!("[{}] PCM数据转换失败，跳过当前分片: {}", self.session_id, err_msg);
                            continue;
                        },
                    };

                    if let Some(encoder) = &mut self.opus_encoder {
                        match encoder.encode_frame(&pcm_samples) {
                            Ok(frame) => {
                                if frame.data.is_empty() {
                                    warn!("[{}] Opus编码产生空数据", self.session_id);
                                    pcm_frame_data.clone()
                                } else {
                                    frame.data.clone()
                                }
                            },
                            Err(e) => {
                                warn!("[{}] ❌ Opus编码失败: {}", self.session_id, e);
                                pcm_frame_data.clone()
                            },
                        }
                    } else {
                        warn!("[{}] Opus编码器未初始化，发送PCM数据", self.session_id);
                        pcm_frame_data.clone()
                    }
                } else {
                    pcm_frame_data
                };

                let audio_chunk = PacedAudioChunk {
                    audio_data: output_audio_data,
                    is_final: false,
                    realtime_metadata: self.merge_metadata.clone(),
                    sentence_text: None,
                    turn_final: false,
                };

                self.pending_chunks.push_back(audio_chunk);
                if self.buffer_start_time.is_none() {
                    self.buffer_start_time = Some(Instant::now());
                }
                self.pending_chunk_count.fetch_add(1, Ordering::Relaxed);
            } else {
                break;
            }
        }
    }

    /// Update precomputed frame size based on current output config.
    pub(crate) fn update_precomputed_frame_size(&mut self) {
        let bytes_per_sample = if self.output_config.format.is_opus() {
            2
        } else {
            self.output_config.format.bytes_per_sample() as usize
        };
        let channels = self.channels as usize;
        let sample_rate = self.sample_rate as usize;

        if self.output_config.slice_ms == 0 {
            warn!("[{}] ⚠️ 时间片长度为0，使用默认值20ms", self.session_id);
            self.output_config.slice_ms = 20;
        }

        let total_bytes = (sample_rate * channels * bytes_per_sample * self.output_config.slice_ms as usize) / 1000;

        self.precomputed_frame_size_bytes = total_bytes.max(1);
        debug!(
            "[{}] 🔧 更新预计算帧大小: {} bytes ({}ms时间片, 采样率={}Hz, 声道={}, 字节/样本={})",
            self.session_id, self.precomputed_frame_size_bytes, self.output_config.slice_ms, sample_rate, channels, bytes_per_sample
        );
    }

    /// Dynamically update the audio output configuration.
    pub fn update_output_config(&mut self, new_config: OutputAudioConfig) {
        if self.output_config.format == new_config.format && self.output_config.slice_ms == new_config.slice_ms {
            debug!("[{}] 音频输出配置无变化，跳过更新: {:?}", self.session_id, new_config);
            return;
        }

        info!(
            "[{}] 🔄 更新音频输出配置: {:?} -> {:?}",
            self.session_id, self.output_config, new_config
        );

        self.clear_all_buffers_and_reset_state("音频输出配置更新");
        self.time_slice_last_send_time = Instant::now();

        let mut corrected_config = new_config.clone();
        corrected_config.auto_correct();

        if let Err(validation_error) = corrected_config.validate() {
            warn!(
                "[{}] 新的音频输出配置验证失败: {}，保持原配置",
                self.session_id, validation_error
            );
            return;
        }

        self.output_config = corrected_config;
        self.update_precomputed_frame_size();

        if self.output_config.format.is_opus() {
            if let Some(config) = &self.output_config.opus_config {
                if let Some(encoder) = &mut self.opus_encoder {
                    if let Err(e) = encoder.update_config(config.clone()) {
                        warn!("[{}] Opus编码器配置更新失败: {}, 重新创建编码器", self.session_id, e);
                        match OpusEncoder::with_config(config.clone(), self.sample_rate, self.channels as u16) {
                            Ok(new_encoder) => self.opus_encoder = Some(new_encoder),
                            Err(e) => {
                                error!("[{}] 重新创建Opus编码器失败: {}", self.session_id, e);
                                self.opus_encoder = None;
                            },
                        }
                    }
                } else {
                    match OpusEncoder::with_config(config.clone(), self.sample_rate, self.channels as u16) {
                        Ok(encoder) => {
                            info!("[{}] 创建新的Opus编码器", self.session_id);
                            self.opus_encoder = Some(encoder);
                        },
                        Err(e) => {
                            error!("[{}] 创建Opus编码器失败: {}", self.session_id, e);
                            self.opus_encoder = None;
                        },
                    }
                }
            } else {
                warn!("[{}] Opus格式但缺少opus_config配置，清理编码器", self.session_id);
                self.opus_encoder = None;
            }
        } else {
            if self.opus_encoder.is_some() {
                info!("[{}] 清理Opus编码器（切换到非Opus格式）", self.session_id);
                self.opus_encoder = None;
            }
        }

        info!(
            "[{}] ✅ 音频输出配置更新完成: slice_ms={}ms, precomputed_frame_size={}bytes",
            self.session_id, self.output_config.slice_ms, self.precomputed_frame_size_bytes
        );
    }

    pub fn get_output_config(&self) -> OutputAudioConfig {
        self.output_config.clone()
    }

    pub fn get_audio_format(&self) -> AudioFormat {
        self.output_config.format.clone()
    }
}
