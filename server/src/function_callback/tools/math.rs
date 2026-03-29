use crate::function_callback::math_calculator;
use crate::function_callback::{CallResult, FunctionCallbackError};
use rustc_hash::FxHashMap;

/// 创建数学计算工具定义
pub fn create_math_tools() -> Vec<crate::llm::llm::Tool> {
    const CALC_DESCRIPTION: &str =
        r#"basic math calculator,Supported operations: +, -, *, /, //, %, ^, sin, cos, tan, log, ln, sqrt, abs, pi, e. Examples: '2+3*4', 'sin(pi/2)', 'sqrt(144)+log(100)', '100//60'"#;

    vec![crate::llm::llm::Tool {
        tool_type: "function".to_string(),
        function: crate::llm::llm::ToolFunction {
            name: "math".to_string(),
            description: CALC_DESCRIPTION.to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "equation": {
                        "type": "string",
                    }
                },
            }),
        },
    }]
}

/// 判断是否为内置数学工具调用
pub fn is_builtin_math_tool(tool_name: &str) -> bool {
    matches!(tool_name, "math")
}

/// 处理内置数学工具调用
pub async fn handle_builtin_math_tool(tool_name: &str, parameters: &FxHashMap<String, serde_json::Value>) -> Result<CallResult, FunctionCallbackError> {
    match tool_name {
        "math" => math_calculator::calculate_function(parameters).await,
        _ => Err(FunctionCallbackError::NotImplemented(format!("未知的数学工具: {}", tool_name))),
    }
}
