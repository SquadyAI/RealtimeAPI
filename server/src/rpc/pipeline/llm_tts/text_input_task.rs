//! 文本输入任务 - ASR Task 的文本版替代
//!
//! 功能：接收文本输入 → 包装成 TurnContext → 转发给 LLM 任务

use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::rpc::pipeline::asr_llm_tts::types::TurnContext;

/// 文本输入任务
///
/// 这是 ASR 任务的文本版本替代，功能极其简单：
/// 1. 接收文本输入
/// 2. 包装成 TurnContext
/// 3. 转发给 LLM 任务
pub struct TextInputTask {
    session_id: String,
    text_rx: mpsc::UnboundedReceiver<String>,
    llm_tx: mpsc::UnboundedSender<(TurnContext, String)>,
    turn_counter: Arc<AtomicU64>,
}

impl TextInputTask {
    /// 创建新的文本输入任务
    pub fn new(session_id: String, text_rx: mpsc::UnboundedReceiver<String>, llm_tx: mpsc::UnboundedSender<(TurnContext, String)>) -> Self {
        Self { session_id, text_rx, llm_tx, turn_counter: Arc::new(AtomicU64::new(0)) }
    }

    /// 运行任务主循环
    pub async fn run(mut self) -> Result<()> {
        info!("📝 TextInputTask 启动: session={}", self.session_id);

        while let Some(text) = self.text_rx.recv().await {
            debug!("📥 收到文本输入: {} (session={})", text, self.session_id);

            // 生成轮次ID
            let turn_id = self.turn_counter.fetch_add(1, Ordering::SeqCst);
            let response_id = format!("text_{}", turn_id);
            let user_item_id = format!("user_{}", turn_id);
            let assistant_item_id = format!("asst_{}", turn_id);

            // 创建轮次上下文
            let ctx = TurnContext::new(user_item_id, assistant_item_id, response_id.clone(), Some(turn_id));

            // 转发到 LLM 任务
            if let Err(e) = self.llm_tx.send((ctx, text.clone())) {
                error!("❌ 发送文本到 LLM 失败: {} (session={})", e, self.session_id);
                break;
            }

            info!("✅ 文本已转发到 LLM: response_id={} (session={})", response_id, self.session_id);
        }

        info!("📝 TextInputTask 退出: session={}", self.session_id);
        Ok(())
    }
}
