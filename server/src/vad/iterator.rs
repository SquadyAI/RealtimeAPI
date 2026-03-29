//! Voice Activity Detection iterator implementation
//!
//! This module provides the VAD iterator for processing audio streams and detecting speech segments.
//! It handles both streaming and batch processing of audio data with zero-copy optimizations.
//!
//! ## 首音丢失问题修复 (Fixed First Audio Loss Issue)
//!
//! ### 问题描述
//! 在之前的实现中，VAD检测到语音开始时会丢失最开始的音频片段。这是因为：
//! 1. VAD需要连续检测到 `min_speech_duration_ms` 时长的语音才认为语音真正开始
//! 2. 但padding buffer只保存了 `speech_pad_samples` 的历史音频
//! 3. 这导致语音段开始时，实际上丢失了开始的 `min_speech_duration_ms` 时间的音频
//!
//! ### 修复方案
//! 1. **扩展历史缓冲区大小**：
//!    - 原来：只保存 `speech_pad_samples` 的历史音频
//!    - 修复后：保存 `min_speech_duration_ms + speech_pad_samples` 的历史音频
//!
//! 2. **往回追溯音频**：
//!    - 在语音开始时，包含完整的历史音频（包括回溯的 `min_speech_duration_ms` 部分）
//!    - 确保ASR能够获得完整的语音开始段
//!
//! ### 技术细节
//! ```
//! 时间轴示例 (假设 min_speech_duration_ms = 160ms, speech_pad_samples = 2560 = 160ms):
//!
//! 修复前：
//! |---静音---|---语音检测(160ms)---|🎤开始|  <- 丢失开始的160ms
//!           |--padding(160ms)--|
//!
//! 修复后：
//! |---静音---|---语音检测(160ms)---|🎤开始|
//! |----历史缓冲区(320ms)-------|     <- 包含完整的开始音频
//!           |--回溯(160ms)--|--padding(160ms)--|
//! ```
//!
//! ### 配置影响
//! - `min_speech_duration_ms`: 连续语音检测时长，影响触发灵敏度
//! - `speech_pad_samples`: 额外的padding音频，提供更多上下文
//! - 总历史缓冲区大小 = `min_speech_duration_ms + speech_pad_samples`

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use ndarray::ArrayView1;

use crate::vad::VADResult;
use crate::vad::model::SileroVAD;
use crate::vad::semantic_vad::{SmartTurnPredictor, SmartTurnSessionPool, features};

const CHUNK_SIZE: usize = 512;
const SAMPLE_RATE: usize = 16000;

/// Represents the output of the VAD iterator.
/// It contains the audio data for a speech segment and metadata.
#[derive(Debug, Clone)]
pub struct VadEvent {
    /// The actual audio data for the speech chunk.
    pub audio: Vec<f32>,
    /// Indicates if this is the first chunk of a speech utterance.
    pub is_first: bool,
    /// Indicates if this is the last chunk of a speech utterance.
    pub is_last: bool,
    /// SmartTurn否决了话轮结束判断（仅当is_last=true时有意义）
    /// 当为true时，表示SileroVAD认为语音结束，但SmartTurn认为用户只是停顿
    /// 上层应暂存ASR结果，并启动超时定时器
    pub smart_turn_vetoed: bool,
}

/// A stateful VAD iterator that buffers audio and yields speech segments.
///
/// This iterator is designed to replicate the behavior of the Python `pysilero-vad`
/// VADIterator. It internally buffers audio, identifies speech segments, and
/// returns them as chunks of audio data, including a configurable amount of
/// padding before the speech starts.
///
/// ## 语义VAD二层过滤 (SemanticVAD Two-Layer Filtering)
///
/// 简化流程：
/// 1. SileroVAD检测语音开始 → 新语音段加padding，延续语音段不加
/// 2. SileroVAD检测语音结束 → 交给SmartTurn判断
/// 3. SmartTurn否决 → 继续累积，标记为延续语音段
/// 4. SmartTurn确认 → 输出is_last=true，重置为新语音段
// 注意：由于SmartTurnPredictor包含ONNX Session，不支持Clone
pub struct VADIterator {
    model: SileroVAD,
    threshold: f32,
    min_silence_chunks: usize,
    min_speech_chunks: usize,
    speech_pad_chunks: usize,

    // Internal state
    in_speech: bool,
    silence_chunks_counter: usize,
    speech_chunks_counter: usize,
    /// Ring buffer to hold pre-speech padding audio.
    padding_buffer: VecDeque<f32>,

    // Timeout mechanism - 使用Arc<Mutex>以便在多个线程间共享
    timeout_state: Arc<Mutex<TimeoutState>>,

    // === 语义VAD (SmartTurn) 相关字段 ===
    /// SmartTurn预测器（可选，未启用时为None，退化为原VADIterator行为）
    smart_turn: Option<SmartTurnPredictor>,
    /// SmartTurn话轮结束判断阈值
    semantic_threshold: f32,
    /// 完整语音段累积Buffer - 跨越多个SileroVAD事件持续累积
    speech_segment_buffer: Vec<f32>,
    /// 是否是新语音段（true=需要padding，false=延续语音段不需要padding）
    is_new_segment: bool,
}

/// 超时状态，用于在多个线程间共享
#[derive(Debug)]
pub struct TimeoutState {
    last_process_time: Option<Instant>,
    timeout_active: bool,
    timeout_sender: Option<tokio::sync::mpsc::UnboundedSender<VadEvent>>,
    // 🆕 取消发送器，用于停止超时监控任务
    cancel_sender: Option<tokio::sync::mpsc::UnboundedSender<()>>,
}

impl TimeoutState {
    fn new() -> Self {
        Self {
            last_process_time: None,
            timeout_active: false,
            timeout_sender: None,
            cancel_sender: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VadState {
    Speaking,
    #[default]
    Silence,
}

impl VADIterator {
    /// Create a new stateful VAD iterator.
    pub fn new(model: SileroVAD, threshold: f32, min_silence_duration_ms: u32, min_speech_duration_ms: u32, speech_pad_samples: u32) -> Self {
        Self::new_with_semantic(
            model,
            threshold,
            min_silence_duration_ms,
            min_speech_duration_ms,
            speech_pad_samples,
            None, // smart_turn_session: 不启用语义VAD
            0.5,  // semantic_threshold (default)
        )
    }

    /// 创建带语义VAD功能的VAD迭代器（使用 SmartTurn Session Pool）
    pub fn new_with_semantic(
        model: SileroVAD,
        threshold: f32,
        min_silence_duration_ms: u32,
        min_speech_duration_ms: u32,
        speech_pad_samples: u32,
        smart_turn_pool: Option<SmartTurnSessionPool>,
        semantic_threshold: f32,
    ) -> Self {
        const SR: u32 = 16000;
        let samples_per_ms = SR / 1000;

        // Convert ms to chunks, rounding up.
        let samples_per_chunk = CHUNK_SIZE as u32;
        let min_silence_samples = min_silence_duration_ms * samples_per_ms;
        let min_silence_chunks = min_silence_samples.div_ceil(samples_per_chunk);

        let min_speech_samples = min_speech_duration_ms * samples_per_ms;
        let min_speech_chunks = min_speech_samples.div_ceil(samples_per_chunk);

        let speech_pad_samples = if speech_pad_samples == 0 {
            let auto_samples = min_speech_samples.div_ceil(samples_per_chunk) * samples_per_chunk;
            auto_samples.max(samples_per_chunk)
        } else {
            speech_pad_samples
        };

        let speech_pad_chunks = speech_pad_samples / samples_per_chunk;

        // 🔧 修复首音丢失问题：增加padding buffer大小
        // 需要保存 min_speech_duration_ms + speech_pad_samples 的历史音频
        // 这样在语音开始时可以往回追溯完整的语音开始部分
        let total_history_samples = min_speech_samples + speech_pad_samples;
        let total_history_chunks = total_history_samples.div_ceil(samples_per_chunk);

        // 使用 SmartTurn Session Pool 创建预测器
        let smart_turn = smart_turn_pool.map(|pool| {
            tracing::info!(
                "✅ 语义VAD (SmartTurn) 已启用（使用 Session Pool，{} 个 Session）",
                pool.pool_size()
            );
            SmartTurnPredictor::new(pool)
        });

        Self {
            model,
            threshold,
            min_silence_chunks: min_silence_chunks as usize,
            min_speech_chunks: min_speech_chunks as usize,
            speech_pad_chunks: speech_pad_chunks as usize,
            in_speech: false,
            silence_chunks_counter: 0,
            speech_chunks_counter: 0,
            // 🔧 使用扩展的历史缓冲区大小
            padding_buffer: VecDeque::with_capacity(total_history_chunks as usize * CHUNK_SIZE),
            // 🆕 初始化语音结束冷却期机制 - 设置100ms冷却期
            timeout_state: Arc::new(Mutex::new(TimeoutState::new())),
            // 语义VAD字段
            smart_turn,
            semantic_threshold,
            speech_segment_buffer: Vec::new(),
            is_new_segment: true, // 初始为新语音段
        }
    }

    /// Reset the iterator state completely.
    ///
    /// This should be called when starting a new, independent audio stream.
    /// It resets the VAD model's internal states and clears all buffers.
    pub async fn reset(&mut self) {
        self.in_speech = false;
        self.silence_chunks_counter = 0;
        self.speech_chunks_counter = 0;
        self.padding_buffer.clear();
        self.model.reset_states(1);

        // 重置语义VAD相关状态
        self.speech_segment_buffer.clear();
        self.is_new_segment = true;

        let mut timeout_state = self.timeout_state.lock().await;
        timeout_state.last_process_time = None;
        timeout_state.timeout_active = false;
        timeout_state.timeout_sender = None;
    }

    /// 更新检测阈值
    pub fn set_threshold(&mut self, new_threshold: f32) {
        self.threshold = new_threshold.clamp(0.0, 1.0);
    }

    /// 更新最小静音时长 (毫秒)
    pub fn set_min_silence_duration_ms(&mut self, ms: u32) {
        self.min_silence_chunks = (ms as usize * 16 / CHUNK_SIZE).max(1);
    }

    /// 更新最小连续语音时长 (毫秒)
    pub fn set_min_speech_duration_ms(&mut self, ms: u32) {
        self.min_speech_chunks = (ms as usize * 16 / CHUNK_SIZE).max(1);
    }

    /// Process a single audio chunk and return a speech event if detected.
    ///
    /// The input chunk must have a size of 512, 1024, or 1536 samples.
    ///
    /// ## 语义VAD二层过滤逻辑
    ///
    /// 当启用语义VAD时：
    /// 1. SileroVAD检测语音活动，累积完整语音段到speech_segment_buffer
    /// 2. SileroVAD认为语音结束时，用完整语音段调用SmartTurn
    /// 3. SmartTurn确认语义结束 -> 输出is_last=true
    /// 4. SmartTurn否决 -> 继续等待，不清空Buffer
    pub async fn process_chunk(&mut self, x: &ArrayView1<'_, f32>) -> VADResult<Option<VadEvent>> {
        // 🔧 先克隆音频数据，解耦生命周期
        let current_chunk: Vec<f32> = x.to_vec();

        // 🔧 修复：正确的超时监控逻辑
        {
            let mut timeout_state = self.timeout_state.lock().await;

            // 只要有音频输入就更新时间戳（无论是否在语音状态）
            if timeout_state.timeout_active {
                timeout_state.last_process_time = Some(Instant::now());
            }
        }

        // 始终维护滚动历史缓冲（包含静音与语音），以确保段首固定回溯可用
        // 最大长度：min_speech_duration_ms 对应的语音窗口 + speech_pad_samples 对应的固定回溯 + 1个chunk
        // 只在新语音段且静音状态时维护padding_buffer
        if !self.in_speech && self.is_new_segment {
            self.padding_buffer.extend(current_chunk.iter());

            // 🔧 使用扩展的历史缓冲区大小（min_speech + speech_pad + 1个chunk）
            let min_speech_samples = (self.min_speech_chunks * CHUNK_SIZE) as u32;
            let speech_pad_samples = (self.speech_pad_chunks * CHUNK_SIZE) as u32;
            let samples_per_chunk = CHUNK_SIZE as u32;
            let max_padding_samples = min_speech_samples + speech_pad_samples + samples_per_chunk;

            if self.padding_buffer.len() > max_padding_samples as usize {
                let overflow = self.padding_buffer.len() - max_padding_samples as usize;
                self.padding_buffer.drain(..overflow);
            }
        }

        // 使用ArrayView1创建临时视图用于模型推理
        let chunk_view = ndarray::ArrayView1::from(&current_chunk);
        let silero_start = Instant::now();
        let prob = self.model.process_chunk(&chunk_view).await?;
        let silero_elapsed = silero_start.elapsed();
        let is_speech = prob >= self.threshold;

        // 仅在状态边界附近记录详细日志，避免刷屏
        if (is_speech && !self.in_speech) || (!is_speech && self.in_speech) {
            tracing::debug!(
                "📊 [SileroVAD] prob={:.3}, is_speech={}, in_speech={}, silero_ms={:.2}",
                prob,
                is_speech,
                self.in_speech,
                silero_elapsed.as_secs_f64() * 1000.0
            );
        }

        if is_speech {
            if !self.in_speech {
                // 不在语音状态，检查是否达到连续语音触发阈值
                self.speech_chunks_counter += 1;
                self.silence_chunks_counter = 0; // 重置静音计数器

                if self.speech_chunks_counter >= self.min_speech_chunks {
                    // 达到连续语音触发阈值，开始语音段
                    self.in_speech = true;
                    self.speech_chunks_counter = 0; // 重置语音计数器

                    tracing::info!(
                        "📢 [SileroVAD] Silence→Speaking: prob={:.3}, threshold={:.3}, min_speech_chunks={}",
                        prob,
                        self.threshold,
                        self.min_speech_chunks
                    );

                    // 🔧 语音开始时停止超时监控，因为用户正在说话
                    {
                        let mut timeout_state = self.timeout_state.lock().await;
                        timeout_state.timeout_active = false;
                        timeout_state.last_process_time = None;
                        tracing::debug!("🔧 VAD检测到语音开始，停止超时监控");
                    }

                    // 🔧 修复首音丢失：首包应包含“min_speech + speech_pad”的历史 + 当前块
                    let min_speech_samples = self.min_speech_chunks * CHUNK_SIZE;
                    let speech_pad_samples = self.speech_pad_chunks * CHUNK_SIZE;
                    let desired_history_samples = min_speech_samples.saturating_add(speech_pad_samples);

                    let padding_len = self.padding_buffer.len();
                    let current_chunk_len = current_chunk.len();
                    // 历史可用长度 = 缓冲区长度 - 当前块长度（尾部是当前块）
                    let history_available_len = padding_len.saturating_sub(current_chunk_len);
                    // 实际复制的历史长度 = 期望历史 与 实际可用历史 的较小值
                    let history_to_copy = desired_history_samples.min(history_available_len);
                    // 历史片段起点：从“历史结尾（紧邻当前块之前）”往前回溯 history_to_copy
                    let history_start_index = history_available_len.saturating_sub(history_to_copy);

                    let mut speech_audio = Vec::with_capacity(history_to_copy.saturating_add(current_chunk_len));
                    if history_to_copy > 0 {
                        speech_audio.extend(
                            self.padding_buffer
                                .iter()
                                .skip(history_start_index)
                                .take(history_to_copy)
                                .cloned(),
                        );
                    }
                    // 拼接当前块
                    speech_audio.extend(current_chunk.iter().cloned());

                    // 清空历史缓冲，为后续段准备
                    self.padding_buffer.clear();

                    // 🆕 将首包音频加入完整语音段Buffer
                    if self.is_new_segment {
                        // 新语音段：清空buffer并用首包填充（含padding）
                        self.speech_segment_buffer.clear();
                        self.speech_segment_buffer.extend(&speech_audio);
                    } else {
                        // 延续语音段：只追加当前块（不清空已有内容，padding已在之前的buffer中）
                        self.speech_segment_buffer.extend(current_chunk.iter().cloned());
                    }

                    tracing::info!(
                        "🎤 VAD语音段开始：包含历史音频 {}ms ({}样本) + 当前块 {}样本",
                        (history_to_copy as f32 / 16.0) as u32,
                        history_to_copy,
                        current_chunk_len
                    );

                    return Ok(Some(VadEvent {
                        audio: speech_audio,
                        is_first: true,
                        is_last: false,
                        smart_turn_vetoed: false,
                    }));
                } else {
                    // 还未达到触发阈值
                    // 🆕 延续语音段时，这些帧也要累积到buffer（它们是用户语音的一部分）
                    if !self.is_new_segment {
                        self.speech_segment_buffer.extend(current_chunk.iter().cloned());
                    }
                    // 新语音段时，滚动历史缓冲已在本次函数开头维护
                }
            } else {
                // 已经在语音状态，重置计数器并返回当前块
                self.silence_chunks_counter = 0;
                self.speech_chunks_counter = 0;

                // 🆕 累积到完整语音段Buffer
                self.speech_segment_buffer.extend(current_chunk.iter().cloned());

                return Ok(Some(VadEvent {
                    audio: current_chunk,
                    is_first: false,
                    is_last: false,
                    smart_turn_vetoed: false,
                }));
            }
        } else {
            // Silence
            self.speech_chunks_counter = 0; // 重置语音计数器

            if self.in_speech {
                self.silence_chunks_counter += 1;

                // 🆕 累积静音到完整语音段Buffer（直到真正结束）
                self.speech_segment_buffer.extend(current_chunk.iter().cloned());

                if self.silence_chunks_counter >= self.min_silence_chunks {
                    // SileroVAD认为语音结束了
                    tracing::info!(
                        "📢 [SileroVAD] Speaking→Silence: prob={:.3}, threshold={:.3}, silence_chunks={}/{}",
                        prob,
                        self.threshold,
                        self.silence_chunks_counter,
                        self.min_silence_chunks
                    );

                    // 🆕 语义VAD二层过滤：调用SmartTurn判断
                    if self.smart_turn.is_some() {
                        // 对完整语音段进行语义判断
                        let smart_turn_start = Instant::now();
                        let turn_end_prob = self
                            .predict_turn_end()
                            .await
                            .map_err(|e| tracing::warn!("⚠️ SmartTurn预测失败: {}", e))
                            .unwrap_or(1.0); // 失败时认为是结束，触发ASR处理
                        let smart_turn_elapsed = smart_turn_start.elapsed();

                        tracing::info!(
                            "🧠 SmartTurn判断：话轮结束概率 = {:.3}, 阈值 = {:.3}, 语音段长度 = {}ms, 推理耗时 = {:?}",
                            turn_end_prob,
                            self.semantic_threshold,
                            self.speech_segment_buffer.len() * 1000 / SAMPLE_RATE,
                            smart_turn_elapsed
                        );

                        if turn_end_prob >= self.semantic_threshold {
                            // SmartTurn确认：真正的语义结束
                            self.in_speech = false;
                            self.speech_segment_buffer.clear();
                            self.padding_buffer.clear();
                            self.is_new_segment = true; // 重置为新语音段

                            tracing::info!("✅ SmartTurn确认话轮结束");
                            return Ok(Some(VadEvent {
                                audio: Vec::new(),
                                is_first: false,
                                is_last: true,
                                smart_turn_vetoed: false,
                            }));
                        } else {
                            // SmartTurn否决：用户只是停顿，但仍需触发ASR以获取结果
                            // 🔧 关键修改：返回带smart_turn_vetoed标记的is_last事件
                            // 上层ASR会暂存结果并启动定时器，而非直接发送给LLM
                            self.in_speech = false;
                            self.is_new_segment = false; // 下次开始不加padding（延续语音段）
                            self.silence_chunks_counter = 0;
                            // 注意：不清空speech_segment_buffer，以便后续继续累积

                            tracing::info!(
                                "🔄 SmartTurn否决，触发ASR但暂存结果，Buffer={}ms",
                                self.speech_segment_buffer.len() * 1000 / SAMPLE_RATE
                            );

                            // 🔧 返回带标记的is_last事件，让ASR进行推理但暂存结果
                            return Ok(Some(VadEvent {
                                audio: Vec::new(),
                                is_first: false,
                                is_last: true,
                                smart_turn_vetoed: true,
                            }));
                        }
                    }

                    // 未启用语义VAD或语音段太短：直接结束
                    self.in_speech = false;
                    self.speech_segment_buffer.clear();
                    self.padding_buffer.clear();
                    self.is_new_segment = true;

                    tracing::info!("📢 [SileroVAD] 语音段结束（无SmartTurn）");
                    return Ok(Some(VadEvent {
                        audio: Vec::new(),
                        is_first: false,
                        is_last: true,
                        smart_turn_vetoed: false,
                    }));
                } else {
                    // Still in speech (during temporary silence), return the current chunk
                    return Ok(Some(VadEvent {
                        audio: current_chunk,
                        is_first: false,
                        is_last: false,
                        smart_turn_vetoed: false,
                    }));
                }
            } else {
                // Still in silence
                // 🆕 延续语音段时，静音帧也要累积到buffer（确保SmartTurn收到完整音频，包括停顿）
                if !self.is_new_segment {
                    self.speech_segment_buffer.extend(current_chunk.iter().cloned());
                }
                // padding_buffer已在函数开头统一维护（仅对新语音段生效）
            }
        }

        Ok(None)
    }

    /// 启动超时监控 - 返回一个接收器，用于接收超时事件
    /// 这个方法只设置监控状态，不启动后台任务
    pub async fn start_timeout_monitor(&self, _timeout_duration_ms: u64) -> tokio::sync::mpsc::UnboundedReceiver<VadEvent> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        {
            let mut timeout_state = self.timeout_state.lock().await;
            timeout_state.timeout_sender = Some(tx);
            timeout_state.timeout_active = true; // 🔧 修复：激活超时监控
        }

        rx
    }

    /// 🆕 设置取消发送器（由ASR模块管理任务生命周期时使用）
    pub async fn set_cancel_sender(&self, cancel_sender: tokio::sync::mpsc::UnboundedSender<()>) {
        let mut timeout_state = self.timeout_state.lock().await;
        timeout_state.cancel_sender = Some(cancel_sender);
    }

    /// 🆕 停止超时监控任务
    pub async fn stop_timeout_monitor(&self) {
        let mut timeout_state = self.timeout_state.lock().await;

        // 发送取消信号
        if let Some(cancel_sender) = timeout_state.cancel_sender.take() {
            let _ = cancel_sender.send(()); // 忽略发送失败（可能已关闭）
            tracing::info!("🛑 已发送VAD超时监控取消信号");
        }

        // 清理状态
        timeout_state.timeout_active = false;
        timeout_state.timeout_sender = None;
        timeout_state.last_process_time = None;
    }

    /// 获取超时状态的克隆引用，用于在独立线程中监控
    pub fn get_timeout_state(&self) -> Arc<Mutex<TimeoutState>> {
        self.timeout_state.clone()
    }

    /// 使用SmartTurn预测话轮结束概率
    async fn predict_turn_end(&mut self) -> anyhow::Result<f32> {
        // 先克隆需要的数据，避免借用冲突
        let audio_to_process = self.speech_segment_buffer.clone();

        // 音频预处理：零均值单位方差归一化
        let processed_audio = Self::preprocess_audio_static(&audio_to_process);

        // 提取Mel频谱特征
        let mel_features = features::log_mel_spectrogram(&processed_audio)?;

        // SmartTurn预测（现在是async）
        if let Some(ref smart_turn) = self.smart_turn {
            let prob = smart_turn.predict(mel_features).await?;
            Ok(prob)
        } else {
            // 不应该发生，但作为fallback返回1.0（认为结束）
            Ok(1.0)
        }
    }

    /// 音频预处理：零均值单位方差归一化（静态方法）
    fn preprocess_audio_static(audio: &[f32]) -> Vec<f32> {
        if audio.is_empty() {
            return audio.to_vec();
        }

        let mean = audio.iter().sum::<f32>() / audio.len() as f32;
        let variance = audio
            .iter()
            .map(|&sample| {
                let centered = sample - mean;
                centered * centered
            })
            .sum::<f32>()
            / audio.len() as f32;
        let denom = (variance + 1e-7).sqrt();

        if denom == 0.0 {
            audio.iter().map(|_| 0.0).collect()
        } else {
            audio.iter().map(|&sample| (sample - mean) / denom).collect()
        }
    }
}

/// 独立的超时监控任务 - 完全事务驱动，超时后自动退出
pub async fn run_timeout_monitor(
    timeout_state: Arc<Mutex<TimeoutState>>,
    timeout_duration_ms: u64,
    mut cancel_rx: tokio::sync::mpsc::UnboundedReceiver<()>, // 🆕 取消信号接收器
) {
    // 🔧 修复：降低轮询频率，避免过度poll
    let mut interval = tokio::time::interval(Duration::from_millis(50)); // 50ms检查间隔

    loop {
        tokio::select! {
            // 🆕 监听取消信号
            _ = cancel_rx.recv() => {
                tracing::info!("🛑 VAD超时监控收到取消信号，退出监控循环");
                break;
            }

            // 原有的超时检查逻辑
            _ = interval.tick() => {
                let mut state = timeout_state.lock().await;

                if !state.timeout_active {
                    // 🔧 修复：如果监控不活跃，说明已经发送过超时事件，应该退出任务
                    tracing::info!("🛑 VAD超时监控检测到非活跃状态，任务退出");
                    break;
                }

                if let Some(last_time) = state.last_process_time {
                    let elapsed = last_time.elapsed();
                    if elapsed >= Duration::from_millis(timeout_duration_ms) {
                        // 超时发生，发送超时事件
                        if let Some(sender) = state.timeout_sender.take() {
                            let timeout_event = VadEvent { audio: Vec::new(), is_first: false, is_last: true, smart_turn_vetoed: false };

                            tracing::info!("⏰ VAD超时触发，发送超时事件 ({}ms)", timeout_duration_ms);
                            // 发送超时事件（忽略发送错误）
                            let _ = sender.send(timeout_event);
                        }

                        // 🔧 修复：发送超时事件后，完全停止监控，任务将在下次循环时退出
                        state.timeout_active = false;
                        state.last_process_time = None;
                        state.timeout_sender = None;

                        tracing::info!("✅ VAD超时事件已发送，监控任务即将退出");
                        // 下次循环时会检测到 !timeout_active 并退出
                    }
                }
            }
        }
    }

    // tracing::info!("✅ VAD超时监控任务已退出");
}
