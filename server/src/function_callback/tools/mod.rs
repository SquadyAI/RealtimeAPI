pub mod math;
pub mod search;
pub mod simul_interpret;
pub mod tavily_client;
pub mod world_clock;

use crate::function_callback::{CallResult, FunctionCallbackError};
use rustc_hash::FxHashMap;

/// 判断是否为内置工具调用
pub fn is_builtin_tool(tool_name: &str) -> bool {
    let simul_enabled = std::env::var("ENABLE_SIMUL_INTERPRET")
        .ok()
        .map(|v| {
            let s = v.to_ascii_lowercase();
            matches!(s.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(true);

    search::is_builtin_search_tool(tool_name)
        || math::is_builtin_math_tool(tool_name)
        || world_clock::is_builtin_world_clock_tool(tool_name)
        || (simul_enabled && matches!(tool_name, "start_simul_interpret" | "stop_simul_interpret"))
}

/// 处理所有内置工具调用的统一入口
pub async fn handle_builtin_tool(tool_name: &str, parameters: &FxHashMap<String, serde_json::Value>) -> Result<CallResult, FunctionCallbackError> {
    if search::is_builtin_search_tool(tool_name) {
        search::handle_builtin_search_tool(tool_name, parameters).await
    } else if math::is_builtin_math_tool(tool_name) {
        math::handle_builtin_math_tool(tool_name, parameters).await
    } else if world_clock::is_builtin_world_clock_tool(tool_name) {
        world_clock::handle_builtin_world_clock_tool(tool_name, parameters).await
    } else if matches!(tool_name, "start_simul_interpret" | "stop_simul_interpret") {
        simul_interpret::handle_builtin_simul_tool(tool_name, parameters).await
    } else {
        Err(FunctionCallbackError::NotImplemented(format!("未知的内置工具: {}", tool_name)))
    }
}
