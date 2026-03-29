//! 功能回调模块 (Function Callback)
//!
//! 负责外部API调用、功能路由、响应处理
//! 目标延迟: 1-2ms (本地路由) + API响应时间

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

// 子模块
pub mod math_calculator;
pub mod searxng_client;
pub mod tools;

// Re-export types for backward compatibility
pub use tools::math::{create_math_tools, handle_builtin_math_tool, is_builtin_math_tool};
pub use tools::search::{BuiltinSearchManager, create_search_tools, get_builtin_search_manager, handle_builtin_search_tool, is_builtin_search_tool};
pub use tools::simul_interpret::{create_simul_tools, handle_builtin_simul_tool};
pub use tools::world_clock::{create_world_clock_tools, handle_builtin_world_clock_tool, is_builtin_world_clock_tool};
pub use tools::{handle_builtin_tool, is_builtin_tool};

/// 功能回调结果
#[derive(Debug, Clone)]
pub struct FunctionCallResult {
    /// 功能名称
    pub function_name: String,
    /// 调用参数
    pub parameters: FxHashMap<String, serde_json::Value>,
    /// 执行结果
    pub result: CallResult,
    /// 执行时间戳
    pub timestamp: SystemTime,
    /// 处理延迟 (ms)
    pub latency_ms: u64,
    /// 是否成功
    pub success: bool,
    /// 错误信息
    pub error_message: Option<String>,
    /// 元数据
    pub metadata: FxHashMap<String, String>,
}

/// 调用结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CallResult {
    /// 成功结果
    Success(serde_json::Value),
    /// 错误结果
    Error(String),
    /// 异步结果 (返回任务ID)
    Async(String),
}

/// 功能回调错误类型
#[derive(Debug, thiserror::Error)]
pub enum FunctionCallbackError {
    #[error("IO错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("功能未找到: {0}")]
    FunctionNotFound(String),

    #[error("无效参数: {0}")]
    InvalidParameters(String),

    #[error("权限被拒绝: {0}")]
    PermissionDenied(String),

    #[error("注册表已满: {0}")]
    RegistryFull(String),

    #[error("API调用失败: {0}")]
    ApiCallFailed(String),

    #[error("超时: {0}")]
    Timeout(String),

    #[error("网络错误: {0}")]
    Network(String),

    #[error("配置错误: {0}")]
    Config(String),

    #[error("未实现: {0}")]
    NotImplemented(String),

    #[error("其他错误: {0}")]
    Other(String),
}
