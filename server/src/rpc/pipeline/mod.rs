use anyhow::Result;
use async_trait::async_trait;
use std::any::Any;
use std::sync::Arc;

// pub mod asr_llm_pipeline;
pub mod asr_llm_tts;
// pub mod asr_llm_tts_pipeline;
pub mod asr_only;
pub mod llm_tts;
pub mod paced_sender;
pub mod task_scope;
// 🔄 重构 TTS-Only 管线以使用 volcEngine-tts
pub mod translation;
pub mod tts_only;
pub mod vision_tts;

// pub use asr_llm_pipeline::AsrLlmPipeline;
/// 新版模块化Pipeline，推荐使用
pub use asr_llm_tts::orchestrator::ModularPipeline;
/// ASR-only Pipeline - 仅语音识别
pub use asr_only::AsrOnlyPipeline;
pub use llm_tts::LlmTtsPipeline;
// 🔄 重构后重新启用
pub use task_scope::SessionTaskScope;
pub use translation::TranslationPipeline;
pub use tts_only::EnhancedStreamingTtsOnlyPipeline;
pub use vision_tts::streaming_pipeline::VisionTtsPipeline;

/// 清理守卫 - 用于确保Pipeline资源正确释放
#[derive(Clone)]
pub struct CleanupGuard {
    /// 清理函数
    cleanup_fn: Arc<dyn Fn() + Send + Sync>,
}

impl CleanupGuard {
    pub fn new<F>(f: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        Self { cleanup_fn: Arc::new(f) }
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        (self.cleanup_fn)();
    }
}

#[async_trait]
pub trait StreamingPipeline: Send + Sync + Any {
    /// 初始化资源、注册回调。返回 CleanupGuard，用于会话结束时收尾。
    async fn start(&self) -> Result<CleanupGuard>;

    /// 处理来自客户端的二进制增量数据
    async fn on_upstream(&self, payload: super::protocol::BinaryMessage) -> Result<()>;

    /// 获取Any引用，用于运行时类型转换
    fn as_any(&self) -> &dyn Any;

    /// 处理客户端工具调用结果
    ///
    /// 默认实现：支持客户端工具调用，但建议Pipeline实现者重写此方法以提供更好的功能
    async fn handle_tool_call_result(&self, tool_result: asr_llm_tts::tool_call_manager::ToolCallResult) -> Result<()> {
        // 默认实现：记录工具调用结果但不做进一步处理
        // 具体的Pipeline实现（如ModularPipeline）应该重写此方法以提供完整的工具调用支持
        tracing::warn!(
            "🔧 收到工具调用结果但使用默认实现，建议Pipeline重写handle_tool_call_result方法: call_id={}, output_len={}",
            tool_result.call_id,
            tool_result.output.len()
        );

        // 返回成功，允许客户端工具调用功能正常工作
        // 具体的处理逻辑由Pipeline的具体实现来完成
        Ok(())
    }

    /// 会话配置热更新统一入口（与 startSession 的 SessionConfig 对齐）
    ///
    /// 默认实现为 no-op，具体管线按需覆盖实现。
    async fn apply_session_config(&self, _payload: &super::protocol::MessagePayload) -> Result<()> {
        Ok(())
    }
}

// NOTE: 旧巨型管线暂时保留文件以供参考，但不再导出供外部使用
// pub use asr_llm_tts_pipeline::AsrLlmTtsPipeline;
