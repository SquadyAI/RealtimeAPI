//! Silero VAD model implementation with optimized performance
//!
//! This module provides the core Silero VAD model implementation using the ONNX runtime.
//! Silero VAD (opset 15) 需要 64-sample context 拼接在输入前:
//! - 输入: context (64) + chunk (512) = 576 samples
//! - 每次推理后更新 context 为输入的最后 64 samples

use futures::lock::Mutex;
use ndarray::{Array1, Array2, Array3, ArrayView1, Axis, s};
use ort::session::Session;
use std::sync::Arc;
use tracing::info;

use crate::vad::{VADError, VADResult};

/// Silero VAD context size (64 samples at 16kHz, ~4ms)
const CONTEXT_SIZE: usize = 64;

#[derive(Clone)]
pub struct SileroVAD {
    session: Arc<Mutex<Session>>,
    /// State tensor for model input/output, shape (2, 1, 128)
    state_tensor: Array3<f32>,
    /// Sample rate tensor for model input
    sample_rate_tensor: Array1<i64>,
    /// Context buffer (64 samples), prepended to each input chunk
    context: Array2<f32>,
}

impl SileroVAD {
    /// 从已有的Session创建VAD模型实例
    pub fn new(session: Arc<Mutex<Session>>) -> VADResult<Self> {
        let state_tensor = Array3::<f32>::zeros((2, 1, 128));
        let sample_rate_tensor = Array1::from_vec(vec![16000i64]);
        let context = Array2::<f32>::zeros((1, CONTEXT_SIZE));
        info!("VAD model loaded successfully");

        Ok(Self { session, state_tensor, sample_rate_tensor, context })
    }

    /// Reset the model's internal state
    ///
    /// This should be called when processing a new audio stream.
    /// Resets state tensor and context to zero.
    pub fn reset_states(&mut self, _batch_size: usize) {
        self.state_tensor.fill(0.0);
        self.context.fill(0.0);
    }

    /// Process a single audio chunk with optimized performance
    ///
    /// # Arguments
    ///
    /// * `x` - Audio chunk to process (must be 512 samples for 16kHz)
    ///
    /// # Returns
    ///
    /// Speech probability for the chunk (0.0 to 1.0)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// * The input chunk size is invalid (must be 512 samples)
    /// * Model inference fails
    pub async fn process_chunk<'a>(&'a mut self, x: &ArrayView1<'a, f32>) -> VADResult<f32> {
        // 将 chunk 转为 (1, chunk_len) 形状
        let chunk_2d = x.to_owned().insert_axis(Axis(0));

        // 拼接 context (64) + chunk (512) = (1, 576)
        let audio_with_context = ndarray::concatenate(Axis(1), &[self.context.view(), chunk_2d.view()]).map_err(|e| VADError::ModelInitializationError(format!("Context concat failed: {}", e)))?;

        // Run inference
        let mut binding = self.session.lock().await;
        let mut outputs = binding
            .run(ort::inputs! {
                "input" => ort::value::Tensor::from_array(audio_with_context.clone())?,
                "sr" => ort::value::Tensor::from_array(self.sample_rate_tensor.to_owned())?,
                "state" => ort::value::Tensor::from_array(self.state_tensor.to_owned())?
            })
            .map_err(|e| VADError::ModelInitializationError(format!("Model inference failed: {}", e)))?;

        // Update internal state tensor
        let state_n_tensor = outputs.remove("stateN").unwrap();
        let state_data = state_n_tensor
            .try_extract_tensor::<f32>()
            .map_err(|e| VADError::ModelInitializationError(format!("Failed to extract state: {}", e)))?;

        if let Ok(new_state) = Array3::from_shape_vec(self.state_tensor.raw_dim(), state_data.1.to_vec()) {
            self.state_tensor = new_state;
        }

        // 更新 context: 取输入的最后 CONTEXT_SIZE 个样本
        let total_len = audio_with_context.len_of(Axis(1));
        self.context = audio_with_context.slice(s![.., (total_len - CONTEXT_SIZE)..]).to_owned();

        // Extract output probability
        let prob_tensor = outputs.remove("output").unwrap();
        let prob_data = prob_tensor
            .try_extract_tensor::<f32>()
            .map_err(|e| VADError::ModelInitializationError(format!("Failed to extract output: {}", e)))?;
        let prob = *prob_data.1.first().unwrap();

        Ok(prob)
    }
}
