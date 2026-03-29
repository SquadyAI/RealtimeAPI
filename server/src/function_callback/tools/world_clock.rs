use crate::function_callback::{CallResult, FunctionCallbackError};
use crate::geodata::cities500;
use chrono::{Datelike, TimeZone, Utc};
use chrono_tz::Tz;
use rustc_hash::FxHashMap;
use tracing::{debug, info, warn};

/// 创建世界时钟工具定义
pub fn create_world_clock_tools() -> Vec<crate::llm::llm::Tool> {
    vec![crate::llm::llm::Tool {
        tool_type: "function".to_string(),
        function: crate::llm::llm::ToolFunction {
            name: "world_clock".to_string(),
            description: "Get current time for a specified location (world clock). Query one location at a time. If user asks for multiple cities, prioritize the most relevant one. Provide one parameter (choose one):\n\n- timezone: IANA timezone name (e.g., Asia/Shanghai, America/New_York)\n- city: City name \n\nOutput fields: query_time_utc, timezone, local_time (%Y-%m-%d %H:%M:%S), weekday (Chinese), utc_offset.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "timezone": {
                        "type": "string",
                        "description": "Single IANA timezone name (e.g., Asia/Shanghai, America/New_York)",
                    },
                    "city": {
                        "type": "string",
                        "description": "city name (supports Chinese/English aliases, e.g., 北京/Beijing, 纽约/New York, 東京/Tokyo)",
                    }
                },
                "oneOf": [
                    { "required": ["timezone"] },
                    { "required": ["city"] }
                ]
            }),
        },
    }]
}

/// 判断是否为内置世界时钟工具调用
pub fn is_builtin_world_clock_tool(tool_name: &str) -> bool {
    matches!(tool_name, "world_clock")
}

/// 处理内置世界时钟工具调用
pub async fn handle_builtin_world_clock_tool(tool_name: &str, parameters: &FxHashMap<String, serde_json::Value>) -> Result<CallResult, FunctionCallbackError> {
    match tool_name {
        "world_clock" => {
            info!("🕒 world_clock 调用");
            // 支持 city 或 timezone 二选一（city 优先走离线数据库）
            let mut city_input: Option<String> = None;
            let tz_str_owned: Option<String> = if let Some(city) = parameters.get("city").and_then(|v| v.as_str()) {
                city_input = Some(city.to_string());
                info!("🕒 world_clock: 尝试根据城市解析: {}", city);
                cities500::resolve_timezone(city, None)
            } else {
                // 记录传入的时区参数（如果有）
                if let Some(tz) = parameters.get("timezone").and_then(|v| v.as_str()) {
                    info!("🕒 world_clock: 使用传入的时区: {}", tz);
                }
                parameters
                    .get("timezone")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
            };

            let tz_str = match tz_str_owned {
                Some(v) => v,
                None => {
                    if city_input.is_some() {
                        warn!("🕒 world_clock: Failed to resolve city, suggest using IANA timezone or more specific city name");
                        return Err(FunctionCallbackError::InvalidParameters(
                            "Failed to resolve city name: please use IANA 'timezone' (e.g., Asia/Shanghai) or provide a more specific city name".to_string(),
                        ));
                    } else {
                        warn!("🕒 world_clock: Missing parameters, need to provide 'timezone' or 'city'");
                        return Err(FunctionCallbackError::InvalidParameters(
                            "Missing valid parameters: please provide 'timezone' (IANA) or 'city'".to_string(),
                        ));
                    }
                },
            };

            debug!("🕒 world_clock: 解析得到时区字符串: {}", tz_str);

            // 固定输出格式
            let fmt = "%Y-%m-%d %H:%M:%S";
            let now_utc = Utc::now();
            match tz_str.parse::<Tz>() {
                Ok(tz) => {
                    let local_time = tz.from_utc_datetime(&now_utc.naive_utc());
                    let weekday_zh = match local_time.weekday() {
                        chrono::Weekday::Mon => "Monday",
                        chrono::Weekday::Tue => "Tuesday",
                        chrono::Weekday::Wed => "Wednesday",
                        chrono::Weekday::Thu => "Thursday",
                        chrono::Weekday::Fri => "Friday",
                        chrono::Weekday::Sat => "Saturday",
                        chrono::Weekday::Sun => "Sunday",
                    };
                    let offset = local_time.format("%z").to_string();
                    let local_time_str = local_time.format(fmt).to_string();

                    let payload = serde_json::json!({
                        "utc_time": now_utc.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                        "timezone": tz_str,
                        "local_time": local_time_str,
                        "weekday": weekday_zh,
                        "utc_offset": offset
                    });
                    info!(
                        "🕒 world_clock: 成功 tz={} local_time={} offset={}",
                        payload.get("timezone").and_then(|v| v.as_str()).unwrap_or(""),
                        payload.get("local_time").and_then(|v| v.as_str()).unwrap_or(""),
                        payload.get("utc_offset").and_then(|v| v.as_str()).unwrap_or("")
                    );

                    Ok(CallResult::Success(payload))
                },
                Err(_) => {
                    warn!("🕒 world_clock: Invalid IANA timezone name: {}", tz_str);
                    Err(FunctionCallbackError::InvalidParameters(
                        "Invalid IANA timezone name".to_string(),
                    ))
                },
            }
        },
        _ => Err(FunctionCallbackError::NotImplemented(format!(
            "Unknown world clock tool: {}",
            tool_name
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weekday_is_valid(s: &str) -> bool {
        matches!(
            s,
            "Monday" | "Tuesday" | "Wednesday" | "Thursday" | "Friday" | "Saturday" | "Sunday"
        )
    }

    #[tokio::test]
    async fn world_clock_timezone_ok() -> Result<(), Box<dyn std::error::Error>> {
        let mut params = FxHashMap::default();
        params.insert("timezone".to_string(), serde_json::json!("Asia/Tokyo"));

        let res = handle_builtin_world_clock_tool("world_clock", &params).await?;
        match res {
            CallResult::Success(v) => {
                assert_eq!(v.get("timezone").and_then(|x| x.as_str()), Some("Asia/Tokyo"));
                let local_time = v.get("local_time").and_then(|x| x.as_str()).expect("missing local_time");
                assert!(chrono::NaiveDateTime::parse_from_str(local_time, "%Y-%m-%d %H:%M:%S").is_ok());
                let weekday = v.get("weekday").and_then(|x| x.as_str()).expect("missing weekday");
                assert!(weekday_is_valid(weekday));
                let offset = v.get("utc_offset").and_then(|x| x.as_str()).expect("missing utc_offset");
                assert_eq!(offset.len(), 5, "offset should be like +0900 or -0400");
            },
            other => panic!("expected Success, got {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn world_clock_city_ok() -> Result<(), Box<dyn std::error::Error>> {
        // Ensure dataset path is resolvable regardless of cwd
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("cities500.txt");
        // 测试代码中设置环境变量是安全的
        // unsafe 是必要的，因为 std::env::set_var 不是线程安全的
        unsafe {
            std::env::set_var("GEONAMES_CITIES_FILE", &path);
        }

        let mut params = FxHashMap::default();
        params.insert("city".to_string(), serde_json::json!("Tokyo"));

        let res = handle_builtin_world_clock_tool("world_clock", &params).await?;
        match res {
            CallResult::Success(v) => {
                assert_eq!(v.get("timezone").and_then(|x| x.as_str()), Some("Asia/Tokyo"));
            },
            other => panic!("expected Success, got {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn world_clock_invalid_timezone() {
        let mut params = FxHashMap::default();
        params.insert("timezone".to_string(), serde_json::json!("Invalid/Timezone"));

        let err = handle_builtin_world_clock_tool("world_clock", &params).await.unwrap_err();
        match err {
            FunctionCallbackError::InvalidParameters(_) => {},
            other => panic!("expected InvalidParameters, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn world_clock_missing_params() {
        let params: FxHashMap<String, serde_json::Value> = FxHashMap::default();
        let err = handle_builtin_world_clock_tool("world_clock", &params).await.unwrap_err();
        match err {
            FunctionCallbackError::InvalidParameters(_) => {},
            other => panic!("expected InvalidParameters, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn world_clock_unknown_city() {
        let mut params = FxHashMap::default();
        params.insert("city".to_string(), serde_json::json!("NonExistentCityNameXyz"));
        let err = handle_builtin_world_clock_tool("world_clock", &params).await.unwrap_err();
        match err {
            FunctionCallbackError::InvalidParameters(_) => {},
            other => panic!("expected InvalidParameters, got {other:?}"),
        }
    }
}
