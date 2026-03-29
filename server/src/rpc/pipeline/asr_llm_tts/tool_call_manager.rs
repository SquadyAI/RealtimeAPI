use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// 工具调用状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallState {
    /// 等待客户端执行
    WaitingForExecution,
    /// 已完成执行
    Completed,
    /// 执行失败或超时
    Failed(String),
}

/// 待处理的工具调用信息
#[derive(Debug, Clone)]
pub struct PendingToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    pub state: ToolCallState,
    pub created_at: std::time::Instant,
}

/// 工具调用管理器
#[derive(Debug, Clone)]
pub struct ToolCallManager {
    /// 待处理的工具调用
    pub pending_calls: Arc<Mutex<FxHashMap<String, PendingToolCall>>>,
    /// 工具调用结果通道 - 发送完整 ToolCallResult
    pub result_tx: mpsc::UnboundedSender<ToolCallResult>,
    /// 工具调用结果接收端
    pub result_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<ToolCallResult>>>>,
    /// 超时时间（秒）
    pub timeout_secs: u64,
}

impl ToolCallManager {
    pub fn new(timeout_secs: u64) -> Self {
        let (result_tx, result_rx) = mpsc::unbounded_channel();
        Self {
            pending_calls: Arc::new(Mutex::new(FxHashMap::default())),
            result_tx,
            result_rx: Arc::new(Mutex::new(Some(result_rx))),
            timeout_secs,
        }
    }

    /// 添加待处理的工具调用
    pub fn add_pending_call(&self, call_id: String, name: String, arguments: String) {
        let mut pending = self.pending_calls.lock().unwrap();
        pending.insert(
            call_id.clone(),
            PendingToolCall {
                call_id,
                name,
                arguments,
                state: ToolCallState::WaitingForExecution,
                created_at: std::time::Instant::now(),
            },
        );
    }

    /// 标记工具调用为完成
    pub fn mark_completed(&self, call_id: &str) -> bool {
        let mut pending = self.pending_calls.lock().unwrap();
        if let Some(call) = pending.get_mut(call_id) {
            call.state = ToolCallState::Completed;
            true
        } else {
            false
        }
    }

    /// 标记工具调用为失败
    pub fn mark_failed(&self, call_id: &str, error: String) -> bool {
        let mut pending = self.pending_calls.lock().unwrap();
        if let Some(call) = pending.get_mut(call_id) {
            call.state = ToolCallState::Failed(error);
            true
        } else {
            false
        }
    }

    /// 移除已完成的工具调用
    pub fn remove_call(&self, call_id: &str) -> Option<PendingToolCall> {
        let mut pending = self.pending_calls.lock().unwrap();
        pending.remove(call_id)
    }

    /// 检查超时的工具调用
    pub fn check_timeouts(&self) -> Vec<String> {
        let mut pending = self.pending_calls.lock().unwrap();
        let now = std::time::Instant::now();
        let timeout_duration = std::time::Duration::from_secs(self.timeout_secs);

        let mut timed_out_calls = Vec::new();

        for (call_id, call) in pending.iter_mut() {
            if call.state == ToolCallState::WaitingForExecution && now.duration_since(call.created_at) > timeout_duration {
                call.state = ToolCallState::Failed("Timeout".to_string());
                timed_out_calls.push(call_id.clone());
            }
        }

        timed_out_calls
    }

    /// 获取所有待处理的工具调用
    pub fn get_pending_calls(&self) -> Vec<PendingToolCall> {
        let pending = self.pending_calls.lock().unwrap();
        pending.values().cloned().collect()
    }

    /// 清除所有待处理的工具调用（打断时调用）
    pub fn clear_all(&self) -> usize {
        let mut pending = self.pending_calls.lock().unwrap();
        let count = pending.len();
        pending.clear();
        count
    }

    /// 检查指定的 call_id 是否在待处理列表中
    pub fn is_pending(&self, call_id: &str) -> bool {
        let pending = self.pending_calls.lock().unwrap();
        pending.contains_key(call_id)
    }
}

/// 客户端工具调用结果消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub call_id: String,
    pub output: String,
    #[serde(default)]
    pub is_error: bool,
    /// 可选控制模式："llm" | "tts" | "stop"
    #[serde(default)]
    pub control_mode: Option<String>,
    /// 当 control_mode 为 "tts" 时可携带直出文本
    #[serde(default)]
    pub tts_text: Option<String>,
}
