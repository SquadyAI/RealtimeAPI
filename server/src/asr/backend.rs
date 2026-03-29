/// ASR 后端统一 trait，用于抽象不同模型的推理实现。
/// 任何具体后端都应实现该 trait，以便 AsrEngine 等上层逻辑通过动态分发调用。
use std::error::Error;

use super::types::VoiceText;

use async_trait::async_trait;

#[async_trait]
pub trait AsrBackend: Send + Sync {
    /// 对增量音频执行流式识别。
    ///
    /// 参数说明：
    /// * `audio` - 单声道 16-kHz PCM，float32 范围 -1.0..1.0
    /// * `is_last` - 是否为当前语音段最后一帧，用于触发强制完成解码
    ///
    /// 返回：
    /// * `Ok(Some(VoiceText))`  - 当模型输出了可用结果
    /// * `Ok(None)`            - 仍在等待更多音频
    /// * `Err(err)`            - 推理/解码过程中出现错误
    async fn streaming_recognition(&mut self, audio: &[f32], is_last: bool, enable_final_inference: bool) -> Result<Option<VoiceText>, Box<dyn Error + Send + Sync>>;

    /// 彻底重置流式前端与内部状态。
    fn reset_streaming(&mut self);

    /// 🆕 会话级别的完全重置，包括降噪器等所有上下文（可选实现）。
    /// 用于会话真正结束时的彻底清理。默认调用 `reset_streaming`。
    fn session_reset(&mut self) {
        self.reset_streaming();
    }

    /// 软重置，仅清空特征缓冲但保留部分上下文（可选实现）。
    /// 默认直接调用 `reset_streaming`。
    fn soft_reset_streaming(&mut self) {
        self.reset_streaming();
    }

    /// 中间处理重置，清空历史状态但保留最近的lfr_m个分片（可选实现）。
    /// 用于delta_process_silence_chunk中间处理后的状态管理。
    /// 默认直接调用 `soft_reset_streaming`。
    fn intermediate_reset_streaming(&mut self) {
        self.soft_reset_streaming();
    }

    /// 中间处理识别，立即对累积的特征进行ONNX推理（可选实现）。
    /// 用于delta_process_silence_chunk中间处理，立即获得识别结果。
    ///
    /// 参数说明：
    /// * `min_features` - 最小特征块数，只有当新增特征块数 >= 此值时才进行推理
    ///
    /// 默认返回None（不支持中间处理识别）。
    async fn intermediate_recognition(&mut self, min_features: u32) -> Result<Option<VoiceText>, Box<dyn Error + Send + Sync>> {
        let _ = min_features; // 避免未使用参数警告
        Ok(None)
    }

    /// 获取中间处理累积的文本（不进行推理）（可选实现）。
    /// 用于VAD超时时获取已经累积的中间处理结果。
    ///
    /// 默认返回None（不支持中间处理）。
    fn get_intermediate_accumulated_text(&self) -> Option<String> {
        None
    }
}
