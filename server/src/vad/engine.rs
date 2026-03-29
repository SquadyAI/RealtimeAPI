use futures::lock::Mutex;
use ort::{
    execution_providers::CPUExecutionProvider,
    session::{Session, builder::GraphOptimizationLevel},
};
use std::sync::Arc;

use tracing::info;

use crate::vad::{MODEL_DATA, SileroVAD, SmartTurnPredictor, SmartTurnSessionPool, VADIterator, VADResult};

/// VAD引擎 - 管理ONNX Session并创建VAD实例
pub struct VADEngine {
    /// SileroVAD Session（全局共享1个）
    session: Arc<Mutex<Session>>,
    /// SmartTurn Session Pool（多 Session 并行推理）
    smart_turn_pool: SmartTurnSessionPool,
}

const DEFAULT_SMART_TURN_POOL_SIZE: usize = 4;

impl VADEngine {
    pub fn new() -> VADResult<Self> {
        Self::with_pool_size(DEFAULT_SMART_TURN_POOL_SIZE)
    }

    pub fn with_pool_size(pool_size: usize) -> VADResult<Self> {
        info!("Creating VAD engine (CPU mode)");

        let cpu_provider = CPUExecutionProvider::default();
        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_log_level(ort::logging::LogLevel::Warning)?
            .with_execution_providers(vec![cpu_provider.build()])?
            .with_intra_threads(1)?
            .with_parallel_execution(false)?
            .with_memory_pattern(true)?
            .commit_from_memory(MODEL_DATA)?;

        let smart_turn_pool = SmartTurnPredictor::create_session_pool(pool_size).map_err(|e| crate::vad::VADError::ConfigurationError(e.to_string()))?;
        info!("✅ SmartTurn Pool: {} sessions", smart_turn_pool.pool_size());

        Ok(Self { session: Arc::new(Mutex::new(session)), smart_turn_pool })
    }

    pub fn smart_turn_pool(&self) -> SmartTurnSessionPool {
        self.smart_turn_pool.clone()
    }

    /// 创建新的VAD迭代器（不带语义VAD）
    pub fn create_vad_iterator(&self, threshold: f32, min_silence_duration_ms: u32, min_speech_duration_ms: u32, speech_pad_samples: u32) -> VADResult<VADIterator> {
        let model = SileroVAD::new(self.session.clone())?;
        Ok(VADIterator::new(
            model,
            threshold,
            min_silence_duration_ms,
            min_speech_duration_ms,
            speech_pad_samples,
        ))
    }

    /// 创建带语义VAD的迭代器
    pub fn create_vad_iterator_with_semantic(
        &self,
        threshold: f32,
        min_silence_duration_ms: u32,
        min_speech_duration_ms: u32,
        speech_pad_samples: u32,
        semantic_enabled: bool,
        semantic_threshold: f32,
    ) -> VADResult<VADIterator> {
        let model = SileroVAD::new(self.session.clone())?;
        let pool = if semantic_enabled { Some(self.smart_turn_pool.clone()) } else { None };

        Ok(VADIterator::new_with_semantic(
            model,
            threshold,
            min_silence_duration_ms,
            min_speech_duration_ms,
            speech_pad_samples,
            pool,
            semantic_threshold,
        ))
    }
}
