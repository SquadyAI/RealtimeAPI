use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Notify;

use async_trait::async_trait;
use ndarray::{Array1, Array2, Array3};
use ort::{
    execution_providers::{CPUExecutionProvider, CUDAExecutionProvider, ExecutionProviderDispatch},
    session::{Session, builder::GraphOptimizationLevel},
    value::Value,
};
use tracing::{error, info, warn};

use crate::asr::backend::AsrBackend;
use crate::asr::types::VoiceText;

/// Parakeet RNNT 后端实现
pub struct ParakeetRnntBackend {
    /// ONNX Session池
    session_pool: Arc<SessionPool>,
    /// 音频特征提取器
    feature_extractor: ParakeetFeatureExtractor,
    /// 解码器状态
    decoder_state: Option<DecoderState>,
}

/// ONNX Session池，用于在多个Parakeet实例间共享模型权重
pub struct SessionPool {
    sessions: Arc<tokio::sync::Mutex<Vec<Session>>>,
    max_sessions: usize,
    notify: Arc<Notify>,
}

impl SessionPool {
    /// 创建新的Session池
    pub fn new(sessions: Vec<Session>) -> Self {
        let max_sessions = sessions.len();
        Self {
            sessions: Arc::new(tokio::sync::Mutex::new(sessions)),
            max_sessions,
            notify: Arc::new(Notify::new()),
        }
    }

    /// 从池中获取一个Session
    pub async fn acquire(&self) -> SessionGuard {
        let start_time = std::time::Instant::now();
        let mut wait_count = 0u32;

        loop {
            let notified = {
                let mut sessions = self.sessions.lock().await;
                if let Some(session) = sessions.pop() {
                    let waited = start_time.elapsed().as_millis();
                    if waited > 0 {
                        info!(
                            "📦 Parakeet Session 获取成功，剩余 {}/{} (等待: {}ms, 次数: {})",
                            sessions.len(), self.max_sessions, waited, wait_count
                        );
                    } else {
                        info!("📦 Parakeet Session 获取成功，剩余 {}/{}", sessions.len(), self.max_sessions);
                    }
                    return SessionGuard { session: Some(session), pool: self.sessions.clone(), notify: self.notify.clone() };
                }
                // 无可用，获取通知句柄
                self.notify.notified()
            };

            wait_count = wait_count.saturating_add(1);
            if wait_count == 1 {
                warn!("⚠️  Parakeet Session 池耗尽，等待释放... (池大小: {})", self.max_sessions);
            }
            notified.await;
        }
    }

    /// 获取当前可用的 Session 数量
    pub async fn available_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// 获取总的 Session 数量
    pub fn total_count(&self) -> usize {
        self.max_sessions
    }

    /// 获取使用中的 Session 数量
    pub async fn in_use_count(&self) -> usize {
        self.max_sessions - self.available_count().await
    }
}

pub struct SessionGuard {
    session: Option<Session>,
    pool: Arc<tokio::sync::Mutex<Vec<Session>>>,
    notify: Arc<Notify>,
}

impl SessionGuard {
    pub fn session(&mut self) -> &mut Session {
        self.session.as_mut().unwrap()
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            // 异步释放Session回池中
            let pool = self.pool.clone();
            let notify = self.notify.clone();
            tokio::spawn(async move {
                let mut sessions = pool.lock().await;
                sessions.push(session);
                // 通知一个等待者
                notify.notify_one();
            });
        }
    }
}

/// 解码器状态
#[derive(Clone)]
struct DecoderState {
    /// 编码器输出缓存
    encoder_outputs: Array3<f32>,
    /// 当前时间步
    time_step: usize,
}

/// Parakeet特征提取器
pub struct ParakeetFeatureExtractor {
    /// 采样率
    sample_rate: u32,
    /// 帧长度 (ms)
    frame_length_ms: f32,
    /// 帧移 (ms)
    frame_shift_ms: f32,
    /// Mel滤波器组数量
    n_mels: usize,
    /// 音频缓冲区
    audio_buffer: Vec<f32>,
}

impl Default for ParakeetFeatureExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl ParakeetFeatureExtractor {
    pub fn new() -> Self {
        Self {
            sample_rate: 16000,
            frame_length_ms: 25.0,
            frame_shift_ms: 10.0,
            n_mels: 80,
            audio_buffer: Vec::new(),
        }
    }

    /// 添加音频数据
    pub fn add_audio(&mut self, audio: &[f32]) {
        self.audio_buffer.extend_from_slice(audio);
    }

    /// 提取mel特征
    pub fn extract_mel_features(&mut self) -> Result<Array2<f32>, Box<dyn Error + Send + Sync>> {
        if self.audio_buffer.is_empty() {
            return Ok(Array2::zeros((0, self.n_mels)));
        }

        // 计算帧参数
        let frame_length = (self.sample_rate as f32 * self.frame_length_ms / 1000.0) as usize;
        let frame_shift = (self.sample_rate as f32 * self.frame_shift_ms / 1000.0) as usize;

        // 计算帧数
        let num_frames = if self.audio_buffer.len() >= frame_length {
            (self.audio_buffer.len() - frame_length) / frame_shift + 1
        } else {
            0
        };

        if num_frames == 0 {
            return Ok(Array2::zeros((0, self.n_mels)));
        }

        // 简化的mel特征提取（实际应该使用更复杂的mel滤波器组）
        let mut features = Array2::zeros((num_frames, self.n_mels));

        for frame_idx in 0..num_frames {
            let start_idx = frame_idx * frame_shift;
            let end_idx = start_idx + frame_length;

            if end_idx > self.audio_buffer.len() {
                break;
            }

            // 提取当前帧
            let frame = &self.audio_buffer[start_idx..end_idx];

            // 应用汉明窗
            let _windowed_frame: Vec<f32> = frame
                .iter()
                .enumerate()
                .map(|(i, &sample)| {
                    let window =
                        0.54 - 0.46 * (2.0 * std::f64::consts::PI * i as f64 / (frame_length - 1) as f64).cos();
                    sample * window as f32
                })
                .collect();

            // 简化的FFT和mel特征计算（这里使用占位实现）
            // 实际应该使用FFT和mel滤波器组
            for mel_idx in 0..self.n_mels {
                features[[frame_idx, mel_idx]] = frame.iter().sum::<f32>() / frame.len() as f32;
            }
        }

        // 应用对数变换
        for i in 0..features.shape()[0] {
            for j in 0..features.shape()[1] {
                features[[i, j]] = (features[[i, j]] + 1e-8).ln();
            }
        }

        Ok(features)
    }

    /// 重置缓冲区
    pub fn reset(&mut self) {
        self.audio_buffer.clear();
    }
}

impl ParakeetRnntBackend {
    /// 创建新实例
    pub fn new() -> Result<Self, Box<dyn Error + Send + Sync>> {
        // 创建特征提取器
        let feature_extractor = ParakeetFeatureExtractor::new();

        // 创建空的Session池（暂时为空，后续会加载模型）
        let session_pool = Arc::new(SessionPool::new(Vec::new()));

        Ok(Self { session_pool, feature_extractor, decoder_state: None })
    }

    /// 加载ONNX模型
    pub fn load_model<P: AsRef<Path>>(
        encoder_path: P,
        pool_size: usize,
    ) -> Result<Arc<SessionPool>, Box<dyn Error + Send + Sync>> {
        info!("🔄 开始加载Parakeet ONNX模型: {}", encoder_path.as_ref().display());

        // 配置执行提供程序
        let mut providers: Vec<ExecutionProviderDispatch> = Vec::new();

        // 使用第一个可见的 CUDA 设备
        if let Some(device_id) = crate::gpu_utils::get_first_visible_cuda_device() {
            let cuda_provider = CUDAExecutionProvider::default().with_device_id(device_id);
            providers.push(cuda_provider.build());
            info!("🚀 Parakeet 使用 CUDA 设备 ID: {}", device_id);
        }

        // 总是添加 CPU 提供程序作为后备
        providers.push(CPUExecutionProvider::default().into());

        // 创建多个Session实例以支持并发
        let mut sessions = Vec::with_capacity(pool_size);
        for i in 0..pool_size {
            let session_result = Session::builder()?
                .with_optimization_level(GraphOptimizationLevel::Level3)?
                .with_execution_providers(providers.clone())?
                .with_parallel_execution(true)?
                .with_intra_threads(32)?
                .commit_from_file(encoder_path.as_ref());

            match session_result {
                Ok(session) => {
                    sessions.push(session);
                    info!("✅ 创建Parakeet ONNX Session {}/{} 完成", i + 1, pool_size);
                },
                Err(e) => {
                    error!("❌ 创建Parakeet ONNX Session {}/{} 失败: {}", i + 1, pool_size, e);
                    return Err(e.into());
                },
            }
        }

        let session_pool = Arc::new(SessionPool::new(sessions));
        info!("🎉 Parakeet Session池创建完成，大小: {}", pool_size);

        Ok(session_pool)
    }

    /// 从Session池创建实例
    pub fn from_session_pool(session_pool: Arc<SessionPool>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let feature_extractor = ParakeetFeatureExtractor::new();

        Ok(Self { session_pool, feature_extractor, decoder_state: None })
    }

    /// 重置流式识别状态
    pub fn reset_streaming(&mut self) {
        self.feature_extractor.reset();
        self.decoder_state = None;
    }

    /// 软重置流式识别状态
    pub fn soft_reset_streaming(&mut self) {
        self.reset_streaming();
    }

    /// 流式识别接口
    pub async fn streaming_recognition(
        &mut self,
        audio: &[f32],
        _is_last: bool,
        _enable_final_inference: bool,
    ) -> Result<Option<VoiceText>, Box<dyn Error + Send + Sync>> {
        // 添加音频数据
        self.feature_extractor.add_audio(audio);

        // 提取mel特征
        let mel_features = self.feature_extractor.extract_mel_features()?;

        if mel_features.shape()[0] == 0 {
            return Ok(None);
        }

        // 获取Session
        let mut session_guard = self.session_pool.acquire().await;
        let session = session_guard.session();

        // 准备输入
        let _batch_size = 1;
        let time_steps = mel_features.shape()[0];
        let _feature_dim = mel_features.shape()[1];

        // 重塑特征为 [batch_size, time_steps, feature_dim]
        let features_3d = mel_features.insert_axis(ndarray::Axis(0));

        // 创建长度数组
        let lengths = Array1::from_vec(vec![time_steps as i64]);

        // 准备ONNX输入
        let inputs = ort::inputs! {
            "audio_signal" => Value::from_array(features_3d)?,
            "length" => Value::from_array(lengths)?,
        };

        // 运行推理
        let outputs = session.run(inputs)?;

        // 获取输出
        let encoder_output = outputs.get("outputs").unwrap();
        let _encoder_output_arr = encoder_output.try_extract_array::<f32>()?;

        let encoded_lengths = outputs.get("encoded_lengths").unwrap();
        let _encoded_lengths_arr = encoded_lengths.try_extract_array::<i64>()?;

        // 更新解码器状态
        // if self.decoder_state.is_none() {
        //     self.decoder_state = Some(DecoderState {
        //         encoder_outputs: encoder_output_arr.clone(),
        //         time_step: 0,
        //     });
        // } else {
        //     // 更新现有状态
        //     let state = self.decoder_state.as_mut().unwrap();
        //     // 这里应该实现更复杂的状态管理逻辑
        //     state.encoder_outputs = encoder_output_arr.clone();
        // }

        // // 简化的解码逻辑（实际应该实现完整的RNN-T解码）
        // // 这里只是返回一个占位结果
        //     Ok(Some(voice_text_from_text("parakeet recognition result".to_string())))
        // } else {
        Ok(None)
        // }
    }
}

#[async_trait]
impl AsrBackend for ParakeetRnntBackend {
    async fn streaming_recognition(
        &mut self,
        audio: &[f32],
        _is_last: bool,
        _enable_final_inference: bool,
    ) -> Result<Option<VoiceText>, Box<dyn Error + Send + Sync>> {
        ParakeetRnntBackend::streaming_recognition(self, audio, _is_last, _enable_final_inference).await
    }

    fn reset_streaming(&mut self) {
        ParakeetRnntBackend::reset_streaming(self)
    }

    fn soft_reset_streaming(&mut self) {
        ParakeetRnntBackend::soft_reset_streaming(self)
    }
}

// 线程安全声明
unsafe impl Send for ParakeetRnntBackend {}
unsafe impl Sync for ParakeetRnntBackend {}
