use anyhow::{Context, Result, ensure};
use ndarray::{Array3, ArrayView1};
// ndarray crate for array operations
use ort::{
    init,
    session::{Session, builder::GraphOptimizationLevel},
    value::Tensor,
};
use std::collections::VecDeque;
use std::time::Instant;
pub mod features;

/// 增强的VAD事件结构，兼容原有 VadEvent 接口并添加语义信息
#[derive(Debug, Clone)]
pub struct EnhancedVadEvent {
    /// 音频数据
    pub audio: Vec<f32>,
    /// 是否为第一个事件
    pub is_first: bool,
    /// 是否为最后一个事件
    pub is_last: bool,
    /// 语义VAD判断的话轮结束概率 (0.0-1.0)
    pub semantic_probability: Option<f32>,
}

impl EnhancedVadEvent {
    /// 创建新的事件
    pub fn new(audio: Vec<f32>, is_first: bool, is_last: bool, semantic_probability: Option<f32>) -> Self {
        Self { audio, is_first, is_last, semantic_probability }
    }

    /// 转换为原有的 VadEvent 格式（保持兼容性）
    pub fn to_vad_event(&self) -> crate::vad::VadEvent {
        crate::vad::VadEvent {
            audio: self.audio.clone(),
            is_first: self.is_first,
            is_last: self.is_last,
            smart_turn_vetoed: false, // EnhancedVadEvent内部处理了语义判断，转换时默认不否决
        }
    }

    /// 判断语义上是否为话轮结束
    pub fn is_semantic_turn_end(&self, threshold: f32) -> bool {
        self.semantic_probability.map(|prob| prob >= threshold).unwrap_or(false)
    }
}

/// 简化的语义VAD事件，直接包含预测概率
#[derive(Debug, Clone)]
pub struct SemanticVadEvent {
    /// 话轮结束概率
    pub turn_end_prob: f32,
    /// 是否为第一个事件
    pub is_first: bool,
    /// 是否为最后一个事件
    pub is_last: bool,
    /// 音频数据
    pub audio: Vec<f32>,
}

impl SemanticVadEvent {
    /// 创建新的事件
    pub fn new(turn_end_prob: f32, is_first: bool, is_last: bool, audio: Vec<f32>) -> Self {
        Self { turn_end_prob, is_first, is_last, audio }
    }

    /// 从 VAD 事件转换为语义 VAD 事件
    pub fn from_vad_event(vad_event: &crate::vad::VadEvent, turn_end_prob: f32, is_first: bool) -> Self {
        Self {
            turn_end_prob,
            is_first,
            is_last: vad_event.is_last,
            audio: vad_event.audio.clone(),
        }
    }

    /// 从 EnhancedVadEvent 转换为 SemanticVadEvent
    pub fn from_enhanced_vad_event(event: &EnhancedVadEvent) -> Option<Self> {
        event.semantic_probability.map(|prob| Self {
            turn_end_prob: prob,
            is_first: event.is_first,
            is_last: event.is_last,
            audio: event.audio.clone(),
        })
    }

    /// 判断是否为话轮结束（基于阈值）
    pub fn is_turn_end(&self, threshold: f32) -> bool {
        self.turn_end_prob >= threshold
    }
}

/// SemanticVAD Iterator 配置
#[derive(Debug, Clone)]
pub struct SemanticVADConfig {
    /// 预测阈值
    pub threshold: f32,
    /// 目标音频时长（秒）
    pub target_duration_seconds: usize,
    /// 最小音频时长（秒）
    pub min_duration_seconds: usize,
    /// 滑动窗口步长（秒）
    pub step_duration_seconds: usize,
}

impl Default for SemanticVADConfig {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            target_duration_seconds: 8,
            min_duration_seconds: 2,
            step_duration_seconds: 1,
        }
    }
}

/// SemanticVADIterator 配置
#[derive(Debug, Clone)]
pub struct SemanticVADIteratorConfig {
    /// SileroVAD 语音检测阈值 (0.0-1.0)
    pub threshold: f32,
    /// SileroVAD 最小静音时长（毫秒）- 缩短以提高灵敏度
    pub min_silence_duration_ms: u32,
    /// SileroVAD 最小语音时长（毫秒）
    pub min_speech_duration_ms: u32,
    /// 语义VAD阈值
    pub semantic_threshold: f32,
    /// 触发语义VAD的静音时长（毫秒）
    pub semantic_trigger_silence_ms: u32,
}

impl Default for SemanticVADIteratorConfig {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            min_silence_duration_ms: 100,
            min_speech_duration_ms: 160,
            semantic_threshold: 0.5,
            semantic_trigger_silence_ms: 100,
        }
    }
}

/// SemanticVADIterator - 集成 SileroVAD 和 SemanticVAD 的协同VAD检测器
///
/// 工作流程：
/// 1. SileroVAD 全时检测语音活动（更灵敏，min_silence_duration_ms=100ms）
/// 2. 当 SileroVAD 检测到静音100ms时，触发语义VAD判断
/// 3. 如果语义判断不是话轮结束，继续缓冲音频，等待更完整的语义
/// 4. 如果语义判断是话轮结束，输出事件并清空缓冲区
/// 5. 确保不丢弃任何音频段，语义不结束时继续缓冲
pub struct SemanticVADIterator {
    /// SileroVAD 实例 - 负责基础的语音活动检测
    silero_vad: crate::vad::model::SileroVAD,
    /// 语义VAD预测器 - 负责话轮结束判断（使用全局共享Session）
    semantic_predictor: SmartTurnPredictor,
    /// 配置参数
    config: SemanticVADIteratorConfig,

    // 内部状态
    current_speech_buffer: VecDeque<f32>, // 当前语音段缓冲区
    pending_audio_buffer: VecDeque<f32>,  // 等待语义判断的音频缓冲区
    in_speech: bool,                      // 是否在语音状态
    silence_duration_ms: u32,             // 当前静音持续时间
    last_audio_time: Option<Instant>,     // 最后音频时间

    // 常量
    sample_rate: u32,
}

impl SemanticVADIterator {
    /// 创建新的 SemanticVADIterator（使用 SmartTurn Session Pool）
    ///
    /// # Arguments
    /// * `silero_vad` - SileroVAD 实例
    /// * `smart_turn_pool` - SmartTurn Session Pool
    /// * `semantic_config` - 配置参数
    pub fn new(silero_vad: crate::vad::model::SileroVAD, smart_turn_pool: SmartTurnSessionPool, semantic_config: SemanticVADIteratorConfig) -> Self {
        let semantic_predictor = SmartTurnPredictor::new(smart_turn_pool);

        Self {
            silero_vad,
            semantic_predictor,
            config: semantic_config,
            current_speech_buffer: VecDeque::new(),
            pending_audio_buffer: VecDeque::new(),
            in_speech: false,
            silence_duration_ms: 0,
            last_audio_time: None,
            sample_rate: 16000,
        }
    }

    /// 处理音频块，返回增强的VAD事件
    ///
    /// # Arguments
    /// * `audio_chunk` - 音频数据（ArrayView1<f32>）
    ///
    /// # Returns
    /// * `Some(EnhancedVadEvent)` - 检测到语音事件
    /// * `None` - 无事件
    pub async fn process_chunk(&mut self, audio_chunk: &ArrayView1<'_, f32>) -> Result<Option<EnhancedVadEvent>> {
        // 🔧 先克隆音频数据，解耦生命周期
        let current_chunk: Vec<f32> = audio_chunk.to_vec();
        let chunk_len = current_chunk.len();

        // 更新最后音频时间
        self.last_audio_time = Some(Instant::now());

        // 使用 SileroVAD 检测语音活动（使用新的视图）
        let chunk_view = ndarray::ArrayView1::from(&current_chunk);
        let speech_prob = self.silero_vad.process_chunk(&chunk_view).await?;
        let is_speech = speech_prob >= self.config.threshold;

        // 将音频添加到缓冲区
        self.current_speech_buffer.extend(current_chunk.iter().cloned());
        self.pending_audio_buffer.extend(current_chunk.iter().cloned());

        if is_speech {
            // 检测到语音
            if !self.in_speech {
                // 语音开始
                self.in_speech = true;
                self.silence_duration_ms = 0;

                // 清空待处理缓冲区，开始新的语音段
                self.pending_audio_buffer.clear();
                self.pending_audio_buffer.extend(&self.current_speech_buffer);

                tracing::info!("🎤 SemanticVAD检测到语音开始");

                // 返回语音开始事件
                Ok(Some(EnhancedVadEvent::new(
                    current_chunk,
                    true,
                    false,
                    None, // 语义概率在语音开始时未知
                )))
            } else {
                // 继续语音
                self.silence_duration_ms = 0;
                self.pending_audio_buffer.extend(current_chunk.iter().cloned());

                Ok(Some(EnhancedVadEvent::new(
                    current_chunk,
                    false,
                    false,
                    None, // 中间语音段不进行语义判断
                )))
            }
        } else {
            // 检测到静音
            if self.in_speech {
                // 在语音段中检测到静音
                self.silence_duration_ms += (chunk_len * 1000 / self.sample_rate as usize) as u32;

                // 检查是否达到触发语义VAD的静音时长
                if self.silence_duration_ms >= self.config.semantic_trigger_silence_ms {
                    // 触发语义VAD判断
                    if let Some(event) = self.perform_semantic_judgment().await? {
                        // 语义判断认为是话轮结束
                        self.in_speech = false;
                        self.silence_duration_ms = 0;
                        self.current_speech_buffer.clear();

                        Ok(Some(event))
                    } else {
                        // 语义判断不认为是话轮结束，继续缓冲
                        tracing::debug!("🔄 语义VAD判断不是话轮结束，继续缓冲音频");
                        Ok(None) // 不输出事件，继续缓冲
                    }
                } else {
                    // 静音时长未达到语义判断阈值，继续语音
                    Ok(Some(EnhancedVadEvent::new(current_chunk, false, false, None)))
                }
            } else {
                // 不在语音状态，忽略静音
                Ok(None)
            }
        }
    }

    /// 执行语义VAD判断
    async fn perform_semantic_judgment(&mut self) -> Result<Option<EnhancedVadEvent>> {
        let audio_samples: Vec<f32> = self.pending_audio_buffer.iter().copied().collect();

        if audio_samples.len() < (2 * self.sample_rate as usize) {
            // 音频太短（少于2秒），无法进行有效语义判断
            tracing::debug!(
                "🔇 音频段太短({}ms)，跳过语义判断",
                (audio_samples.len() * 1000 / self.sample_rate as usize)
            );
            return Ok(None);
        }

        // 音频预处理
        let processed_audio = self.preprocess_audio(&audio_samples);

        // 特征提取
        let features = features::log_mel_spectrogram(&processed_audio)?;

        // 语义VAD预测
        let turn_end_prob = self.semantic_predictor.predict(features).await?;

        tracing::info!("🧠 语义VAD判断：话轮结束概率 = {:.3}", turn_end_prob);

        // 检查是否达到阈值
        if turn_end_prob >= self.config.semantic_threshold {
            let event = EnhancedVadEvent::new(
                audio_samples.clone(),
                false, // 不是第一个事件
                true,  // 是最后一个事件
                Some(turn_end_prob),
            );

            tracing::info!("✅ 语义VAD判断为话轮结束，输出事件");
            Ok(Some(event))
        } else {
            // 语义判断不认为是话轮结束，继续缓冲
            Ok(None)
        }
    }

    /// 强制完成当前语音段（用于超时或手动结束）
    ///
    /// # Returns
    /// * `Some(EnhancedVadEvent)` - 如果有缓冲的音频则输出事件
    /// * `None` - 无音频数据
    pub async fn force_finish(&mut self) -> Result<Option<EnhancedVadEvent>> {
        if self.pending_audio_buffer.is_empty() {
            return Ok(None);
        }

        let audio_samples: Vec<f32> = self.pending_audio_buffer.iter().copied().collect();

        // 对剩余音频进行语义判断
        if audio_samples.len() >= (2 * self.sample_rate as usize) {
            // 足够长的音频，进行语义判断
            let processed_audio = self.preprocess_audio(&audio_samples);
            let features = features::log_mel_spectrogram(&processed_audio)?;
            let turn_end_prob = self.semantic_predictor.predict(features).await?;

            let event = EnhancedVadEvent::new(audio_samples.clone(), false, true, Some(turn_end_prob));

            // 重置状态
            self.reset();

            Ok(Some(event))
        } else {
            // 音频太短，直接返回结束事件
            let event = EnhancedVadEvent::new(audio_samples.clone(), false, true, None);

            // 重置状态
            self.reset();

            Ok(Some(event))
        }
    }

    /// 重置迭代器状态
    pub fn reset(&mut self) {
        self.current_speech_buffer.clear();
        self.pending_audio_buffer.clear();
        self.in_speech = false;
        self.silence_duration_ms = 0;
        self.last_audio_time = None;

        // 重置 SileroVAD 状态
        self.silero_vad.reset_states(1);

        tracing::info!("🔄 SemanticVADIterator状态已重置");
    }

    /// 音频预处理：零均值单位方差归一化
    fn preprocess_audio(&self, audio: &[f32]) -> Vec<f32> {
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

    /// 获取当前缓冲区大小（毫秒）
    pub fn buffer_duration_ms(&self) -> u32 {
        (self.pending_audio_buffer.len() * 1000 / self.sample_rate as usize) as u32
    }

    /// 获取当前静音持续时间（毫秒）
    pub fn current_silence_duration_ms(&self) -> u32 {
        self.silence_duration_ms
    }

    /// 检查是否在语音状态
    pub fn is_in_speech(&self) -> bool {
        self.in_speech
    }

    /// 更新配置参数
    pub fn update_config(&mut self, config: SemanticVADIteratorConfig) {
        self.config = config;
    }

    /// 获取当前配置的副本
    pub fn get_config(&self) -> &SemanticVADIteratorConfig {
        &self.config
    }
}

/// SemanticVAD 过滤器 - 接管 VADIterator 输出的两层架构
///
/// 这个架构的工作流程：
/// 1. SileroVAD 检测语音活动，输出 VadEvent
/// 2. SemanticVADFilter 接收 VadEvent，累积音频
/// 3. 当检测到语音结束时，进行语义判断
/// 4. 只有当语义VAD判断是话轮结束时才输出，否则过滤掉
pub struct SemanticVADFilter {
    predictor: SmartTurnPredictor,
    config: SemanticVADConfig,

    // 内部状态
    audio_buffer: VecDeque<f32>,
    is_first_event: bool,
    current_segment_audio: VecDeque<f32>, // 当前语音段的音频

    // 常量
    sample_rate: usize,
}

impl SemanticVADFilter {
    /// 创建新的 SemanticVADFilter（使用 SmartTurn Session Pool）
    pub fn new(smart_turn_pool: SmartTurnSessionPool, config: SemanticVADConfig) -> Self {
        let predictor = SmartTurnPredictor::new(smart_turn_pool);

        Self {
            predictor,
            config,
            audio_buffer: VecDeque::new(),
            is_first_event: true,
            current_segment_audio: VecDeque::new(),
            sample_rate: 16_000,
        }
    }

    /// 使用默认配置创建 SemanticVADFilter
    pub fn new_with_defaults(smart_turn_pool: SmartTurnSessionPool) -> Self {
        Self::new(smart_turn_pool, SemanticVADConfig::default())
    }

    /// 处理 VAD 事件，进行语义过滤
    ///
    /// # Arguments
    /// * `vad_event` - SileroVAD 产生的事件
    ///
    /// # Returns
    /// * `Some(SemanticVadEvent)` - 当且仅当语义VAD判断是话轮结束时返回
    /// * `None` - 语义VAD判断不是话轮结束，过滤掉该事件
    pub async fn process_vad_event(&mut self, vad_event: &crate::vad::VadEvent) -> Result<Option<SemanticVadEvent>> {
        // 累积音频
        self.current_segment_audio.extend(&vad_event.audio);

        if vad_event.is_first {
            // 新的语音段开始，重置缓冲区
            self.audio_buffer.clear();
            self.audio_buffer.extend(&vad_event.audio);
        } else if !vad_event.audio.is_empty() {
            // 继续累积音频
            self.audio_buffer.extend(&vad_event.audio);
        }

        // 当检测到语音结束时，进行语义判断
        if vad_event.is_last && !vad_event.audio.is_empty() {
            return self.check_turn_end(vad_event).await;
        }

        Ok(None)
    }

    /// 处理音频块（用于实时流处理）
    ///
    /// # Arguments
    /// * `audio_chunk` - 音频块数据
    ///
    /// # Returns
    /// * `Some(SemanticVadEvent)` - 当检测到话轮结束时返回事件
    /// * `None` - 继续累积音频
    pub async fn process_audio_chunk(&mut self, audio_chunk: &[f32]) -> Result<Option<SemanticVadEvent>> {
        // 将音频块添加到缓冲区
        self.audio_buffer.extend(audio_chunk);
        self.current_segment_audio.extend(audio_chunk);

        // 限制缓冲区大小，避免内存无限增长
        let max_buffer_size = self.config.target_duration_seconds * self.sample_rate * 2;
        if self.audio_buffer.len() > max_buffer_size {
            let overflow = self.audio_buffer.len() - max_buffer_size;
            self.audio_buffer.drain(..overflow);
        }

        // 这个方法主要用于不依赖VAD事件的场景，通常不会直接触发语义判断
        Ok(None)
    }

    /// 完成处理，对剩余音频进行最终判断
    pub async fn finish(&mut self) -> Result<Option<SemanticVadEvent>> {
        if self.current_segment_audio.len() >= self.config.min_duration_seconds * self.sample_rate {
            // 创建一个虚拟的 VAD 事件进行最终判断
            let mock_vad_event = crate::vad::VadEvent {
                audio: self.current_segment_audio.iter().copied().collect(),
                is_first: self.is_first_event,
                is_last: true,
                smart_turn_vetoed: false, // 强制结束时不使用否决机制
            };
            return self.check_turn_end(&mock_vad_event).await;
        }
        Ok(None)
    }

    /// 重置过滤器状态
    pub fn reset(&mut self) {
        self.audio_buffer.clear();
        self.current_segment_audio.clear();
        self.is_first_event = true;
    }

    /// 获取当前缓冲区大小（毫秒）
    pub fn buffer_duration_ms(&self) -> usize {
        (self.current_segment_audio.len() as f32 / self.sample_rate as f32 * 1000.0) as usize
    }

    /// 检查话轮结束的核心逻辑
    async fn check_turn_end(&mut self, vad_event: &crate::vad::VadEvent) -> Result<Option<SemanticVadEvent>> {
        let segment_size = self.current_segment_audio.len();
        let min_samples = self.config.min_duration_seconds * self.sample_rate;

        if segment_size < min_samples {
            // 语音段太短，无法进行有效判断，直接过滤掉
            self.current_segment_audio.clear();
            return Ok(None);
        }

        // 准备音频数据进行特征提取
        let audio_vec: Vec<f32> = self.current_segment_audio.iter().copied().collect();

        // 进行音频预处理和特征提取
        let processed_audio = self.preprocess_audio(&audio_vec);
        let features = features::log_mel_spectrogram(&processed_audio)?;

        // 进行预测
        let turn_end_prob = self.predictor.predict(features).await?;

        // 重置当前语音段
        self.current_segment_audio.clear();

        // 检查是否达到阈值
        if turn_end_prob >= self.config.threshold {
            let event = SemanticVadEvent::from_vad_event(vad_event, turn_end_prob, self.is_first_event);
            self.is_first_event = false;
            Ok(Some(event))
        } else {
            // 未达到阈值，过滤掉该事件
            Ok(None)
        }
    }

    /// 音频预处理：零均值单位方差归一化
    fn preprocess_audio(&self, audio: &[f32]) -> Vec<f32> {
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

use futures::lock::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// SmartTurn 单个 Session 类型别名
pub type SmartTurnSession = Arc<Mutex<Session>>;

/// Session Pool 内部数据（单个 Arc 包装，clone 开销最小）
struct SmartTurnSessionPoolInner {
    sessions: Box<[SmartTurnSession]>, // 固定大小，比 Vec 少一个 capacity 字段
    next_idx: AtomicUsize,
}

/// SmartTurn Session Pool - 支持多 Session 并行推理
///
/// 使用轮询（Round-Robin）策略分配 Session，实现负载均衡。
/// 每个 Session 使用 1×1 线程配置，多 Session 并行效率高于单 Session 多线程。
///
/// ## 优化
/// - 单个 `Arc` 包装所有数据，clone 只需一次原子操作
/// - 使用 `Box<[T]>` 代替 `Vec<T>`，减少内存开销
#[derive(Clone)]
pub struct SmartTurnSessionPool(Arc<SmartTurnSessionPoolInner>);

impl SmartTurnSessionPool {
    /// 创建 Session Pool
    ///
    /// # Arguments
    /// * `pool_size` - Session 数量，建议 4-8
    pub fn new(pool_size: usize) -> Result<Self> {
        let pool_size = pool_size.max(1); // 至少 1 个 Session

        // ORT 初始化（仅首次）
        init().with_name("smart-turn-pool").commit()?;

        let mut sessions = Vec::with_capacity(pool_size);
        for i in 0..pool_size {
            let session = Session::builder()?
                .with_optimization_level(GraphOptimizationLevel::Level3)?
                .with_inter_threads(1)?
                .with_intra_threads(4)? // 写死为4以优化SmartTurn推理延迟
                .with_memory_pattern(true)?
                .commit_from_memory(crate::vad::SMART_TURN_MODEL_DATA)
                .with_context(|| format!("Failed to load SmartTurn ONNX model for session {}", i))?;

            sessions.push(Arc::new(Mutex::new(session)));
        }

        tracing::info!("✅ SmartTurn Session Pool 创建成功：{} 个 Session", pool_size);

        Ok(Self(Arc::new(SmartTurnSessionPoolInner {
            sessions: sessions.into_boxed_slice(),
            next_idx: AtomicUsize::new(0),
        })))
    }

    /// 获取下一个 Session（轮询策略）
    #[inline]
    pub fn get_session(&self) -> SmartTurnSession {
        let idx = self.0.next_idx.fetch_add(1, Ordering::Relaxed) % self.0.sessions.len();
        self.0.sessions[idx].clone()
    }

    /// 获取 Pool 大小
    #[inline]
    pub fn pool_size(&self) -> usize {
        self.0.sessions.len()
    }
}

/// SmartTurn 模型固定输入尺寸
const SMART_TURN_INPUT_SHAPE: [usize; 3] = [1, 80, 800]; // [batch, n_mels, frames]

pub struct SmartTurnPredictor {
    pool: SmartTurnSessionPool,
}

impl SmartTurnPredictor {
    /// 从 Session Pool 创建 SmartTurnPredictor
    pub fn new(pool: SmartTurnSessionPool) -> Self {
        Self { pool }
    }

    /// 创建 SmartTurn Session Pool（由 VADEngine 调用一次）
    ///
    /// # Arguments
    /// * `pool_size` - Session 数量，默认 4
    pub fn create_session_pool(pool_size: usize) -> Result<SmartTurnSessionPool> {
        tracing::info!("🔧 SmartTurn 配置: pool_size={}, intra_threads=4 (写死优化)", pool_size);
        SmartTurnSessionPool::new(pool_size)
    }

    /// 使用 IoBinding 进行推理（固定输入尺寸优化）
    ///
    /// 自动从 Pool 获取 Session，实现负载均衡
    pub async fn predict(&self, input_features: Array3<f32>) -> Result<f32> {
        let dims = input_features.dim();

        // 验证输入尺寸
        ensure!(
            dims == (SMART_TURN_INPUT_SHAPE[0], SMART_TURN_INPUT_SHAPE[1], SMART_TURN_INPUT_SHAPE[2]),
            "SmartTurn expects fixed input shape {:?}, got {:?}",
            SMART_TURN_INPUT_SHAPE,
            dims
        );

        let (raw, offset) = input_features.into_raw_vec_and_offset();
        let start = offset.unwrap_or(0);
        ensure!(start == 0, "SmartTurn expects contiguous feature buffers");

        // 创建输入 tensor
        let input_tensor = Tensor::from_array((SMART_TURN_INPUT_SHAPE, raw))?;

        // 从 Pool 获取 Session（轮询）
        let session_ref = self.pool.get_session();
        let mut session = session_ref.lock().await;

        // 使用 IoBinding
        let mut binding = session.create_binding()?;

        // 绑定输入
        binding.bind_input("input_features", &input_tensor)?;

        // 绑定输出到 session allocator（避免额外分配）
        let mem_info = session.allocator().memory_info();
        binding.bind_output_to_device("logits", &mem_info)?;

        // 执行推理
        let outputs = session.run_binding(&binding)?;

        // 提取输出概率
        let out_val = outputs.get("logits").context("Missing 'logits' in model outputs")?;
        let (_shape, data) = out_val.try_extract_tensor::<f32>()?;
        let probability = data[0];

        Ok(probability)
    }
}
