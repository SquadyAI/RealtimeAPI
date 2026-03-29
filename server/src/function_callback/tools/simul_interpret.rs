use crate::function_callback::{CallResult, FunctionCallbackError};
use rustc_hash::FxHashMap;

/// 创建同声传译工具定义
pub fn create_simul_tools() -> Vec<crate::llm::llm::Tool> {
    vec![
        crate::llm::llm::Tool {
            tool_type: "function".to_string(),
            function: crate::llm::llm::ToolFunction {
                name: "start_simul_interpret".to_string(),
                description: "Start bidirectional simultaneous interpretation mode (continuously translate between two languages, auto-detecting which language user speaks).\n\nTrigger criteria: Does the user express continuous intent?\n\nTrigger conditions: Contains continuous keywords like「接下来」「以后」「一直」「所有」「都」「everything」「模式」\n\nExamples that trigger:\n- 把我接下来的话翻译成英文 → language_a=zh, language_b=en\n- 开启中英同声传译模式\n- translate everything between English and Japanese\n\nExamples that do NOT trigger (one-time translation or language switch):\n- 把这句话翻译成英文\n- 用英文回答\n\nWhen user says '同声传译' without specifying languages, do NOT call this tool - ask for clarification first.\n\nOptional: provide tts_text as activation confirmation.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "language_a": {
                            "type": "string",
                            "description": "First language code, e.g., zh (Chinese), en (English), ja (Japanese), ko (Korean), fr (French), de (German), es (Spanish), etc."
                        },
                        "language_b": {
                            "type": "string",
                            "description": "Second language code, e.g., zh (Chinese), en (English), ja (Japanese), ko (Korean), fr (French), de (German), es (Spanish), etc."
                        },
                        "tts_text": {
                            "type": "string",
                            "description": "Optional activation confirmation to speak immediately."
                        }
                    },
                    "required": ["language_a", "language_b"]
                }),
            },
        },
        crate::llm::llm::Tool {
            tool_type: "function".to_string(),
            function: crate::llm::llm::ToolFunction {
                name: "stop_simul_interpret".to_string(),
                description: "Exit simultaneous interpretation mode. IMPORTANT: tts_text must be in the same language as user's input, do NOT translate.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "tts_text": {
                            "type": "string",
                            "description": "Exit confirmation message. Must match user's input language, do NOT translate."
                        }
                    },
                    "required": ["tts_text"]
                }),
            },
        }
    ]
}

/// 处理内置同声传译工具调用
///
/// 注意：此函数只返回占位符结果，实际的模式切换逻辑在 llm_task.rs 中执行。
/// 这里提供统一的入口，确保架构一致性。
pub async fn handle_builtin_simul_tool(tool_name: &str, _parameters: &FxHashMap<String, serde_json::Value>) -> Result<CallResult, FunctionCallbackError> {
    match tool_name {
        // 在 V2 中，start/stop 的业务逻辑已在 Agent/V2 流中处理，这里返回最小占位结果以避免重复文本注入
        "start_simul_interpret" => Ok(CallResult::Success(serde_json::json!({ "handled_by": "llm_task_v2" }))),
        "stop_simul_interpret" => Ok(CallResult::Success(serde_json::json!({ "handled_by": "llm_task_v2" }))),
        _ => Err(FunctionCallbackError::NotImplemented(format!("未知的同传工具: {}", tool_name))),
    }
}
