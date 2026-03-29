//! Agent 工具过滤和注入工具函数
//!
//! 提供统一的工具过滤、去重和内置工具注入功能。
//! 详见 `docs/agent_tool_filtering.md` 了解各 Agent 的工具过滤规则。

use crate::llm::Tool;
use std::collections::HashSet;

/// 当 Agent 收到不支持的工具调用时返回的文本
pub const UNSUPPORTED_TOOL_TEXT: &str = "this function is not supported by the current system";

/// 根据关键字过滤共享工具（不区分大小写）
///
/// # 参数
/// - `shared_tools`: 共享工具列表（来自 MCP/外部工具）
/// - `keywords`: 关键字列表，工具名称包含任一关键字即被选中
///
/// # 返回
/// - `(Vec<Tool>, HashSet<String>)`: 选中的工具列表和工具名称集合（用于去重）
pub fn select_tools_by_keywords(shared_tools: &[Tool], keywords: &[&str]) -> (Vec<Tool>, HashSet<String>) {
    let lowered: Vec<String> = keywords.iter().map(|kw| kw.to_ascii_lowercase()).collect();
    let mut names = HashSet::new();
    let mut selected = Vec::new();

    for tool in shared_tools {
        let name_lower = tool.function.name.to_ascii_lowercase();
        if lowered.iter().any(|kw| name_lower.contains(kw)) && names.insert(tool.function.name.clone()) {
            selected.push(tool.clone());
        }
    }

    (selected, names)
}

/// 按精确名称追加工具（如果存在且未被选中）
///
/// 用于注入内置工具（如 `math`, `world_clock`, `search_web`）。
/// 如果工具不存在于 `shared_tools` 中，静默跳过，不报错。
///
/// # 参数
/// - `tools`: 当前工具列表（会被修改）
/// - `names`: 工具名称集合（用于去重，会被修改）
/// - `shared_tools`: 共享工具列表（从中查找目标工具）
/// - `target_name`: 目标工具名称（精确匹配）
///
/// # 返回
/// - `bool`: 是否成功追加工具
pub fn append_tool_by_name(tools: &mut Vec<Tool>, names: &mut HashSet<String>, shared_tools: &[Tool], target_name: &str) -> bool {
    if names.contains(target_name) {
        return false;
    }
    if let Some(tool) = shared_tools.iter().find(|t| t.function.name == target_name) {
        names.insert(target_name.to_string());
        tools.push(tool.clone());
        return true;
    }
    false
}

/// 收集工具名称到 HashSet，用于快速查找
///
/// 用于在工具调用时验证工具是否在允许列表中。
pub fn collect_tool_names(tools: &[Tool]) -> HashSet<String> {
    tools.iter().map(|t| t.function.name.clone()).collect()
}
