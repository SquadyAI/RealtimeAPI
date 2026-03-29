use crate::env_utils::{env_bool_or_default, env_or_default, env_string_or_default};
use crate::telemetry;
use anyhow::anyhow;
use async_stream::stream;
use chrono::{Datelike, TimeZone};
use chrono_tz::Tz;
use futures_util::StreamExt;
use lazy_static::lazy_static;
use regex::Regex;
use reqwest::Client;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use tracing::{debug, error, info, trace, warn};
use uuid;

lazy_static! {
    /// 预编译的正则表达式：匹配 True（不区分大小写，完整单词）
    static ref RE_TRUE: Regex = Regex::new(r"(?i)\bTrue\b").unwrap();
    /// 预编译的正则表达式：匹配 False（不区分大小写，完整单词）
    static ref RE_FALSE: Regex = Regex::new(r"(?i)\bFalse\b").unwrap();
    /// 预编译的正则表达式：匹配 None
    static ref RE_NONE: Regex = Regex::new(r"(?i)\bNone\b").unwrap();
    /// 预编译的正则表达式：移除 } 或 ] 前面的尾随逗号
    static ref RE_TRAILING_COMMA: Regex = Regex::new(r",\s*([}\]])").unwrap();
    /// 预编译的正则表达式：给未加引号的键名补引号
    static ref RE_UNQUOTED_KEY: Regex = Regex::new(r"([\{,]\s*)([A-Za-z_][A-Za-z0-9_\-]*)\s*:").unwrap();
    /// 缓存上海时区解析结果，避免重复解析
    static ref SHANGHAI_TZ: Tz = "Asia/Shanghai".parse().unwrap();
}

/// 校验字符串是否为合法 JSON
fn is_valid_json(s: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(s).is_ok()
}

/// 尝试尽可能修复"像JSON"的字符串，返回合法JSON字符串
fn repair_json_like_string(input: &str) -> Option<String> {
    let mut candidate = input.trim().to_string();

    // 1) 去除代码围栏 ``` 或 ```json 标记
    if candidate.starts_with("```") {
        let mut lines = candidate.lines();
        // 跳过第一行 ``` or ```json
        lines.next();
        let mut body = String::new();
        for line in lines {
            if line.trim_start().starts_with("```") {
                break;
            }
            body.push_str(line);
            body.push('\n');
        }
        candidate = body.trim().to_string();
    }

    // 2) 提取首尾花括号之间的内容（如果包含多余文本）
    if let (Some(start), Some(end)) = (candidate.find('{'), candidate.rfind('}'))
        && start < end
    {
        candidate = candidate[start..=end].to_string();
    }

    // 快速路径：已经是合法JSON
    if is_valid_json(&candidate) {
        return Some(candidate);
    }

    // 3) 尝试 JSON5 解析（支持单引号、无引号键、尾随逗号等）
    if let Ok(value) = json5::from_str::<serde_json::Value>(&candidate) {
        return serde_json::to_string(&value).ok();
    }

    // 4) 常见修复：
    let mut s = candidate;

    // 4.1 将 True/False/None 等转为 JSON 规范（使用预编译的正则表达式）
    s = RE_TRUE.replace_all(&s, "true").to_string();
    s = RE_FALSE.replace_all(&s, "false").to_string();
    s = RE_NONE.replace_all(&s, "null").to_string();

    // 4.2 移除 } 或 ] 前面的尾随逗号（使用预编译的正则表达式）
    s = RE_TRAILING_COMMA.replace_all(&s, "$1").to_string();

    // 4.3 如果整体没有双引号但有单引号，尝试将单引号替换为双引号
    if !s.contains('"') && s.contains('\'') {
        s = s.replace('\'', "\"");
    }

    // 4.4 给未加引号的键名补引号：匹配 { 或 , 后面的键名直到冒号
    // 注意：这是启发式，尽量避免破坏已加引号的键（使用预编译的正则表达式）
    let mut last;
    loop {
        last = s.clone();
        s = RE_UNQUOTED_KEY
            .replace_all(&s, |caps: &regex::Captures| format!("{}\"{}\":", &caps[1], &caps[2]))
            .to_string();
        if s == last {
            break;
        }
    }

    // 4.5 如果不是以 { 开头而看起来是键值对，尽量包裹成对象
    if !s.trim_start().starts_with('{') && s.contains(':') {
        s = format!("{{{}}}", s);
    }

    if is_valid_json(&s) {
        return Some(s);
    }
    if let Ok(value) = json5::from_str::<serde_json::Value>(&s) {
        return serde_json::to_string(&value).ok();
    }

    None
}

/// 规范化并清洗工具调用，避免发送到后端时出现无效的 arguments 或缺少字段
pub fn sanitize_tool_calls(mut tool_calls: Vec<ToolCall>) -> Vec<ToolCall> {
    // 🚀 性能优化：如果工具调用为空，直接返回
    if tool_calls.is_empty() {
        return tool_calls;
    }

    let mut sanitized: Vec<ToolCall> = Vec::with_capacity(tool_calls.len());

    for mut tc in tool_calls.drain(..) {
        // 确保 type 字段
        if tc.call_type.as_ref().map(|t| t.trim().is_empty()).unwrap_or(true) {
            tc.call_type = Some("function".to_string());
        }

        // 确保 id 字段
        if tc.id.as_ref().map(|s| s.trim().is_empty()).unwrap_or(true) {
            let uuid_str = uuid::Uuid::new_v4().to_string().replace('-', "");
            let truncated: String = uuid_str.chars().take(24).collect();
            tc.id = Some(format!("call_{}", truncated));
        }

        // 函数名必须存在；若缺失，尽量推断或填充占位符而不是丢弃
        let has_name = tc.function.name.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false);
        if !has_name {
            // 尝试从 arguments 中推断 name/function/tool 字段
            let mut inferred: Option<String> = None;
            if let Some(args_str) = &tc.function.arguments {
                // 尝试修复并解析
                if let Some(fixed) = repair_json_like_string(args_str)
                    && let Ok(val) = serde_json::from_str::<serde_json::Value>(&fixed)
                {
                    if let Some(name) = val.get("name").and_then(|v| v.as_str()) {
                        inferred = Some(name.to_string());
                    } else if let Some(name) = val.get("function").and_then(|v| v.as_str()) {
                        inferred = Some(name.to_string());
                    } else if let Some(name) = val.get("tool").and_then(|v| v.as_str()) {
                        inferred = Some(name.to_string());
                    }
                }
            }

            if let Some(name) = inferred {
                tc.function.name = Some(name);
                warn!("🔧 工具调用缺少函数名，已从arguments推断填充，id={:?}", tc.id);
            } else {
                let placeholder = "noop".to_string();
                tc.function.name = Some(placeholder);
                warn!("🔧 工具调用缺少函数名，已填充占位符 'noop'，id={:?}", tc.id);
            }
        }

        // 确保 arguments 存在且为合法 JSON 字符串
        match tc.function.arguments.as_mut() {
            Some(args) => {
                let trimmed = args.trim();
                if trimmed.is_empty() {
                    warn!("🔧 工具参数为空：id={:?}，将填充为 '{}'", tc.id, "{}");
                    *args = "{}".to_string();
                } else if !is_valid_json(trimmed) {
                    // 先尝试修复
                    match repair_json_like_string(trimmed) {
                        Some(fixed) => {
                            debug!("🔧 工具参数已修复为合法JSON：id={:?}, 修复后长度={}", tc.id, fixed.len());
                            *args = fixed;
                        },
                        None => {
                            warn!("🔧 工具参数无法修复为合法JSON：id={:?}，回退为 '{}'", tc.id, "{}");
                            *args = "{}".to_string();
                        },
                    }
                }
            },
            None => {
                tc.function.arguments = Some("{}".to_string());
            },
        }

        sanitized.push(tc);
    }

    sanitized
}

/// 🔧 修复：timezone和location独立fallback处理
///
/// 新的优先级设计：
/// - timezone: 用户提供 > IP地理位置推测 > 默认UTC+8
/// - location: 用户提供 > IP地理位置推测 > 默认中国
///
/// 关键设计：每次调用都获取实时时间，确保LLM获取的时间信息总是最新的
///
/// 根据 ASR 语言偏好返回城市名称：
/// - 中文语音环境：保持用户原文
/// - 非中文语音环境：统一映射为官方英文名（GeoNames `name`/`asciiname`）
fn translate_location_for_language(location: &str, preferred_language: Option<&str>) -> String {
    let is_chinese_lang = preferred_language
        .map(|lang| matches!(lang.to_ascii_lowercase().as_str(), "zh" | "zh-cn" | "chinese"))
        .unwrap_or(true);

    if is_chinese_lang {
        location.to_string()
    } else {
        crate::geodata::cities500::get_english_name(location).unwrap_or_else(|| location.to_string())
    }
}

pub async fn get_timezone_and_location_info_from_ip(session_id: &str) -> (String, String, String) {
    let _start_time = std::time::Instant::now();

    // 初始化fallback值
    let mut final_timezone: Option<String> = None;
    let mut final_location: Option<String> = None;
    let mut _timezone_source = "默认";
    let mut _location_source = "默认";

    // 🔧 通过SessionManager获取metadata
    let metadata_result = if let Some(session_manager) = crate::rpc::GlobalSessionManager::get() {
        session_manager.get_session_metadata(session_id).await
    } else {
        None
    };

    if let Some((ref user_timezone, ref user_location, ref asr_language, ref _connection_id, ref conn_metadata)) = metadata_result {
        tracing::debug!("📍 已获取session元数据: session_id={}", session_id);

        // 🔧 关键修复：timezone和location分别独立处理

        // 1. 处理timezone（独立fallback）
        // 优先级: session配置 > IP地理位置（实时查询）
        if let Some(tz) = user_timezone {
            final_timezone = Some(tz.clone());
            _timezone_source = "用户提供（session级别）";
            tracing::debug!("🕒 timezone使用session配置: session_id={}, timezone={}", session_id, tz);
        } else if let Some(conn_meta) = &conn_metadata {
            // 实时查询 IP geolocation（使用 asr_language 获取对应语言的地名）
            if let (Some(ip), Some(locator)) = (conn_meta.client_ip.clone(), crate::ip_geolocation::get_ip_geolocation_service())
                && let Ok(geo) = locator.lookup_with_language(&ip, asr_language.as_deref())
                && let Some(ip_timezone) = geo.timezone
            {
                final_timezone = Some(ip_timezone.clone());
                _timezone_source = "IP地理位置";
                tracing::debug!("🌍 timezone使用IP地理位置: session_id={}, timezone={}", session_id, ip_timezone);
            }
        }

        // 2. 处理location（独立fallback）
        // 优先级: session配置 > IP地理位置（实时查询）
        if let Some(loc) = user_location {
            let translated_location = translate_location_for_language(loc, asr_language.as_deref());
            final_location = Some(translated_location);
            _location_source = "用户提供（session级别）";
            tracing::debug!(
                "📍 location使用session配置: session_id={}, location={}",
                session_id,
                final_location.as_ref().unwrap()
            );
        } else if let Some(conn_meta) = &conn_metadata {
            // 实时查询 IP geolocation（使用 asr_language 获取对应语言的地名）
            if let (Some(ip), Some(locator)) = (conn_meta.client_ip.clone(), crate::ip_geolocation::get_ip_geolocation_service())
                && let Ok(geo) = locator.lookup_with_language(&ip, asr_language.as_deref())
            {
                // 构建IP地理位置信息
                let location_parts = vec![
                    geo.country.as_deref().unwrap_or(""),
                    geo.region.as_deref().unwrap_or(""),
                    geo.city.as_deref().unwrap_or(""),
                ];
                let ip_location = location_parts
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<&str>>()
                    .join(" ");

                if !ip_location.is_empty() {
                    final_location = Some(ip_location);
                    _location_source = "IP地理位置";
                    tracing::debug!(
                        "🌍 location使用IP地理位置: session_id={}, location={}",
                        session_id,
                        final_location.as_ref().unwrap()
                    );
                }
            }
        }
    } else {
        tracing::debug!("⚠️ 没有找到metadata for session_id={}", session_id);
    }

    // 3. 调用实现函数（支持独立的None值，传递 asr_language 用于多语言格式化）
    get_timezone_and_location_info_impl_independent(
        final_timezone.as_deref(),
        final_location.as_deref(),
        metadata_result.as_ref().and_then(|(_, _, lang, _, _)| lang.as_deref()),
    )
}

/// 🆕 支持独立fallback的时区和位置信息实现（支持中文/英文双语）
fn get_timezone_and_location_info_impl_independent(timezone: Option<&str>, location: Option<&str>, language: Option<&str>) -> (String, String, String) {
    // 🚀 关键设计：每次调用都获取实时UTC时间，确保时间的准确性
    let utc_now = chrono::Utc::now();

    // 判断是否使用中文（只有 zh/chinese 使用中文，其他统一英文）
    let is_chinese = matches!(
        language.map(|s| s.to_lowercase()).as_deref(),
        Some("zh") | Some("zh-cn") | Some("chinese")
    );

    // Helper function: 根据语言返回星期格式
    let get_weekday_str = |weekday: chrono::Weekday| -> &'static str {
        if is_chinese {
            match weekday {
                chrono::Weekday::Mon => "星期一",
                chrono::Weekday::Tue => "星期二",
                chrono::Weekday::Wed => "星期三",
                chrono::Weekday::Thu => "星期四",
                chrono::Weekday::Fri => "星期五",
                chrono::Weekday::Sat => "星期六",
                chrono::Weekday::Sun => "星期日",
            }
        } else {
            // 非中文统一使用英文
            match weekday {
                chrono::Weekday::Mon => "Monday",
                chrono::Weekday::Tue => "Tuesday",
                chrono::Weekday::Wed => "Wednesday",
                chrono::Weekday::Thu => "Thursday",
                chrono::Weekday::Fri => "Friday",
                chrono::Weekday::Sat => "Saturday",
                chrono::Weekday::Sun => "Sunday",
            }
        }
    };

    // 1. 处理timezone（独立fallback到UTC+8）
    let (final_time, final_offset) = if let Some(tz_str) = timezone {
        if let Ok(tz) = tz_str.parse::<Tz>() {
            let tz_time = tz.from_utc_datetime(&utc_now.naive_utc());
            let weekday_str = get_weekday_str(tz_time.weekday());
            let tz_formatted = format!("{} {}", tz_time.format("%Y-%m-%d %H:%M:%S"), weekday_str);
            let tz_offset = tz_time.format("%z").to_string();
            (tz_formatted, tz_offset)
        } else {
            // 时区解析失败，fallback到UTC+8
            warn!("⚠️ 时区解析失败: {}, 使用默认UTC+8", tz_str);
            let tz_time = SHANGHAI_TZ.from_utc_datetime(&utc_now.naive_utc());
            let weekday_str = get_weekday_str(tz_time.weekday());
            let default_formatted = format!("{} {}", tz_time.format("%Y-%m-%d %H:%M:%S"), weekday_str);
            let default_offset = tz_time.format("%z").to_string();
            (default_formatted, default_offset)
        }
    } else {
        // 没有提供timezone，使用默认UTC+8
        let tz_time = SHANGHAI_TZ.from_utc_datetime(&utc_now.naive_utc());
        let weekday_str = get_weekday_str(tz_time.weekday());
        let default_formatted = format!("{} {}", tz_time.format("%Y-%m-%d %H:%M:%S"), weekday_str);
        let default_offset = tz_time.format("%z").to_string();
        (default_formatted, default_offset)
    };

    // 2. 处理location（独立fallback到中国）
    // 返回纯位置信息字符串，不包含"位于 "前缀，便于在上层构建结构化块
    let location_info = if let Some(loc) = location {
        loc.to_string()
    } else {
        // 没有提供location，根据语言使用默认值
        if is_chinese {
            "亚洲/上海 (中国标准时间)".to_string()
        } else {
            "Asia/Shanghai (China Standard Time)".to_string()
        }
    };

    (final_time, final_offset, location_info)
}

/// HTTP协议版本选择
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum HttpVersion {
    /// 自动协商（推荐）
    #[default]
    Auto,
    /// 强制使用 HTTP/1.1
    Http1Only,
    /// 强制使用 HTTP/2
    Http2Only,
}

/// LLM客户端配置
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub timeout_secs: u64,
    pub http_version: HttpVersion,
    pub skip_cert_verification: bool,
    pub max_connections_per_host: usize,
    pub connection_timeout_secs: u64,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_key: env_string_or_default("LLM_API_KEY", ""),
            base_url: env_string_or_default("LLM_BASE_URL", ""),
            model: env_string_or_default("LLM_MODEL", ""),
            timeout_secs: env_or_default("LLM_TIMEOUT_SECS", 10),
            http_version: HttpVersion::default(),
            skip_cert_verification: env_bool_or_default("LLM_SKIP_CERT_VERIFICATION", true),
            max_connections_per_host: env_or_default("LLM_MAX_CONNECTIONS_PER_HOST", 10),
            connection_timeout_secs: env_or_default("LLM_CONNECTION_TIMEOUT_SECS", 10),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>, // "system" | "user" | "assistant" | "tool"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// OpenAI兼容: 当role为"tool"时需要提供tool_call_id
    #[serde(skip_serializing_if = "Option::is_none", rename = "tool_call_id")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

// Function Call 相关数据结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub tool_type: String, // "function"
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    #[serde(alias = "Description")]
    pub description: String,
    pub parameters: Value, // JSON Schema
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub call_type: Option<String>, // "function"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>, // 流式响应中的索引
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>, // JSON字符串，可能为空
}

#[derive(Debug, Default, Clone)]
struct ToolCallTagExtractor {
    pending: String,
}

/// 裸 JSON 格式工具调用检测器
/// 用于处理 Qwen3 等模型直接输出 JSON 格式工具调用（没有 `<tool_call>` 标签）的情况
#[derive(Debug, Default, Clone)]
struct RawJsonToolCallExtractor {
    pending: String,
    brace_depth: i32,
    in_string: bool,
    escape_next: bool,
}

impl RawJsonToolCallExtractor {
    /// 检测文本是否看起来像裸 JSON 工具调用的开始
    fn looks_like_tool_call_json(text: &str) -> bool {
        let trimmed = text.trim();
        // 检测是否以 { 开头且包含工具调用相关的键
        if trimmed.starts_with('{') {
            return trimmed.contains("\"name\"") || trimmed.contains("\"function\"") || trimmed.contains("\"arguments\"");
        }
        // 检测是否是工具调用 JSON 的片段（以 "key": 开头）
        if (trimmed.starts_with("\"name\"") || trimmed.starts_with("\"function\"") || trimmed.starts_with("\"arguments\"")) && trimmed.contains(':') {
            return true;
        }
        false
    }

    /// 处理一个文本片段，尝试提取裸 JSON 工具调用
    fn process_chunk(&mut self, chunk: &str, choice_index: u32, id_counter: &mut u64) -> (String, Vec<ToolCall>) {
        self.pending.push_str(chunk);

        // 如果还没开始收集 JSON，检查是否应该开始
        if self.brace_depth == 0 {
            // 查找 JSON 开始位置
            if let Some(start_pos) = self.pending.find('{') {
                // 检查 { 之前的文本
                let before_brace = &self.pending[..start_pos];
                // 检查整个累积的文本是否看起来像工具调用 JSON
                if Self::looks_like_tool_call_json(&self.pending) {
                    self.brace_depth = 1;
                    // 返回 { 之前的文本（如果有的话）
                    let text_before = before_brace.to_string();
                    self.pending = self.pending[start_pos + 1..].to_string();
                    // 继续处理剩余部分
                    let (remaining_text, calls) = self.continue_parsing(choice_index, id_counter);
                    return (text_before + &remaining_text, calls);
                }
            }
            // 不是工具调用 JSON，返回原文本
            let text = std::mem::take(&mut self.pending);
            return (text, Vec::new());
        }

        self.continue_parsing(choice_index, id_counter)
    }

    /// 继续解析已经开始的 JSON
    fn continue_parsing(&mut self, choice_index: u32, id_counter: &mut u64) -> (String, Vec<ToolCall>) {
        let mut text_out = String::new();
        let mut tool_calls = Vec::new();

        let chars: Vec<char> = self.pending.chars().collect();
        let mut i = 0;
        let mut json_end = None;

        while i < chars.len() {
            let ch = chars[i];

            if self.escape_next {
                self.escape_next = false;
                i += 1;
                continue;
            }

            if ch == '\\' && self.in_string {
                self.escape_next = true;
                i += 1;
                continue;
            }

            if ch == '"' && !self.escape_next {
                self.in_string = !self.in_string;
                i += 1;
                continue;
            }

            if !self.in_string {
                if ch == '{' {
                    self.brace_depth += 1;
                } else if ch == '}' {
                    self.brace_depth -= 1;
                    if self.brace_depth == 0 {
                        json_end = Some(i);
                        break;
                    }
                }
            }
            i += 1;
        }

        if let Some(end_idx) = json_end {
            // 找到完整的 JSON
            let json_content: String = chars[..end_idx].iter().collect();
            let full_json = format!("{{{}}}", json_content);

            // 尝试解析为工具调用
            if let Some(mut parsed) = Self::parse_raw_json_tool_call(&full_json, choice_index, id_counter) {
                debug!(
                    "🔧 检测到裸 JSON 格式工具调用 (choice_index={})，自动转换为结构化工具调用",
                    choice_index
                );
                tool_calls.append(&mut parsed);
            } else {
                // 解析失败，作为普通文本输出
                warn!(
                    "⚠️ 裸 JSON 工具调用解析失败: {}",
                    full_json.chars().take(100).collect::<String>()
                );
                text_out.push_str(&full_json);
            }

            // 处理剩余文本
            self.pending = chars[end_idx + 1..].iter().collect();
            self.in_string = false;
            self.escape_next = false;
        }
        // 如果没有找到完整的 JSON，保持 pending 状态，等待更多输入

        (text_out, tool_calls)
    }

    /// 解析裸 JSON 格式的工具调用
    fn parse_raw_json_tool_call(json: &str, choice_index: u32, id_counter: &mut u64) -> Option<Vec<ToolCall>> {
        use serde_json::Value;

        let value: Value = serde_json::from_str(json).ok()?;
        let obj = value.as_object()?;

        // 尝试多种格式
        let (name, arguments) = if let Some(func) = obj.get("function").and_then(|v| v.as_object()) {
            // 格式: {"function": {"name": "...", "arguments": {...}}}
            let name = func.get("name").and_then(|v| v.as_str())?;
            let args = func.get("arguments").cloned();
            (name.to_string(), args)
        } else if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
            // 格式: {"name": "...", "arguments": {...}}
            let args = obj.get("arguments").cloned();
            (name.to_string(), args)
        } else {
            return None;
        };

        let arguments_str = match arguments {
            Some(Value::String(s)) => s,
            Some(v) => serde_json::to_string(&v).ok()?,
            None => "{}".to_string(),
        };

        let call = ToolCall {
            id: Some(generate_tool_call_id(choice_index, id_counter)),
            call_type: Some("function".to_string()),
            index: None,
            function: FunctionCall { name: Some(name), arguments: Some(arguments_str) },
        };

        Some(vec![call])
    }

    fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.brace_depth == 0
    }
}

fn merge_tool_call_delta(accumulated_calls: &mut Vec<ToolCall>, delta: &ToolCall) {
    if let Some(delta_index) = delta.index
        && let Some(existing) = accumulated_calls.iter_mut().find(|tc| tc.index == Some(delta_index))
    {
        update_existing_tool_call(existing, delta);
        return;
    }
    if let Some(delta_id) = &delta.id
        && !delta_id.is_empty()
        && let Some(existing) = accumulated_calls.iter_mut().find(|tc| tc.id.as_ref() == Some(delta_id))
    {
        update_existing_tool_call(existing, delta);
        return;
    }
    let has_essential_info = delta.id.is_some() || delta.function.name.is_some();
    if has_essential_info {
        let new_call = ToolCall {
            id: delta.id.clone(),
            call_type: delta.call_type.clone().or_else(|| Some("function".to_string())),
            index: delta.index,
            function: delta.function.clone(),
        };
        accumulated_calls.push(new_call);
    }
}

fn update_existing_tool_call(existing: &mut ToolCall, delta: &ToolCall) {
    if existing.id.is_none() && delta.id.is_some() {
        existing.id = delta.id.clone();
    }
    if existing.call_type.is_none() && delta.call_type.is_some() {
        existing.call_type = delta.call_type.clone();
    }
    if existing.index.is_none() && delta.index.is_some() {
        existing.index = delta.index;
    }
    if existing.function.name.is_none() && delta.function.name.is_some() {
        existing.function.name = delta.function.name.clone();
    }
    if let Some(delta_args) = &delta.function.arguments {
        if let Some(existing_args) = &mut existing.function.arguments {
            existing_args.push_str(delta_args);
        } else {
            existing.function.arguments = Some(delta_args.clone());
        }
    }
}

impl ToolCallTagExtractor {
    fn process_chunk(&mut self, chunk: &str, choice_index: u32, id_counter: &mut u64) -> (String, Vec<ToolCall>) {
        self.pending.push_str(chunk);
        Self::drain_completed_segments(&mut self.pending, choice_index, id_counter)
    }

    fn is_idle(&self) -> bool {
        self.pending.is_empty()
    }

    fn drain_completed_segments(buffer: &mut String, choice_index: u32, id_counter: &mut u64) -> (String, Vec<ToolCall>) {
        let mut text_out = String::new();
        let mut tool_calls = Vec::new();
        loop {
            if let Some(start) = buffer.find("<tool_call>") {
                if start > 0 {
                    text_out.push_str(&buffer[..start]);
                }
                buffer.drain(..start);
                if let Some(end_offset) = buffer.find("</tool_call>") {
                    let closing_len = "</tool_call>".len();
                    let block_len = end_offset + closing_len;
                    let block: String = buffer.drain(..block_len).collect();
                    if let Some(mut parsed) = parse_tool_call_block(&block, choice_index, id_counter) {
                        tool_calls.append(&mut parsed);
                    } else {
                        warn!("⚠️ 未能解析 tool_call 标签内容，choice_index={}", choice_index);
                    }
                } else {
                    // 等待结束标签
                    break;
                }
            } else {
                if !buffer.is_empty() {
                    text_out.push_str(buffer);
                    buffer.clear();
                }
                break;
            }
        }

        (text_out, tool_calls)
    }
}

fn parse_tool_call_block(block: &str, choice_index: u32, id_counter: &mut u64) -> Option<Vec<ToolCall>> {
    let inner = block
        .trim()
        .trim_start_matches("<tool_call>")
        .trim_end_matches("</tool_call>")
        .trim();

    if inner.is_empty() {
        return None;
    }

    if inner.starts_with('[') {
        let values: Vec<Value> = serde_json::from_str(inner).ok()?;
        let mut calls = Vec::new();
        for value in values {
            if let Some(call) = build_tool_call_from_value(value, choice_index, id_counter) {
                calls.push(call);
            }
        }
        if calls.is_empty() { None } else { Some(calls) }
    } else if inner.starts_with('{') {
        serde_json::from_str::<Value>(inner)
            .ok()
            .and_then(|value| build_tool_call_from_value(value, choice_index, id_counter).map(|call| vec![call]))
    } else {
        parse_legacy_tool_call_format(inner, choice_index, id_counter)
    }
}

fn build_tool_call_from_value(value: Value, choice_index: u32, id_counter: &mut u64) -> Option<ToolCall> {
    let obj = value.as_object()?;
    let (name_opt, args_opt) = if let Some(function) = obj.get("function").and_then(|v| v.as_object()) {
        (
            function.get("name").and_then(|v| v.as_str()),
            function.get("arguments").cloned(),
        )
    } else {
        (obj.get("name").and_then(|v| v.as_str()), obj.get("arguments").cloned())
    };

    let name = name_opt?.trim();
    if name.is_empty() {
        return None;
    }
    let arguments = normalize_arguments_value(args_opt);

    let mut call = ToolCall {
        id: obj
            .get("id")
            .or_else(|| obj.get("tool_call_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| Some(generate_tool_call_id(choice_index, id_counter))),
        call_type: Some("function".to_string()),
        index: None,
        function: FunctionCall { name: Some(name.to_string()), arguments: Some(arguments) },
    };

    // 如果 JSON 中提供了 index，则保留
    if let Some(idx) = obj.get("index").and_then(|v| v.as_u64()) {
        call.index = Some(idx as u32);
    }

    Some(call)
}

fn normalize_arguments_value(value_opt: Option<Value>) -> String {
    match value_opt {
        Some(Value::String(s)) => s,
        Some(v) => serde_json::to_string(&v).unwrap_or_else(|_| "{}".to_string()),
        None => "{}".to_string(),
    }
}

fn parse_legacy_tool_call_format(body: &str, choice_index: u32, id_counter: &mut u64) -> Option<Vec<ToolCall>> {
    let lower = body.to_lowercase();
    let name_idx = lower.find("name=")?;
    let args_idx = lower.find("arguments=")?;
    if name_idx > args_idx {
        return None;
    }

    let name_part = body[name_idx + 5..args_idx].trim();
    if name_part.is_empty() {
        return None;
    }
    let name = name_part.trim_matches(|c| c == '"' || c == '\'' || c == '`');
    let args_part = body[args_idx + 10..].trim();
    let arguments = if args_part.is_empty() { "{}".to_string() } else { args_part.to_string() };

    let call = ToolCall {
        id: Some(generate_tool_call_id(choice_index, id_counter)),
        call_type: Some("function".to_string()),
        index: None,
        function: FunctionCall { name: Some(name.to_string()), arguments: Some(arguments) },
    };
    Some(vec![call])
}

fn generate_tool_call_id(choice_index: u32, id_counter: &mut u64) -> String {
    *id_counter += 1;
    format!("detected_tool_call_{}_{}", choice_index, *id_counter)
}

fn detect_tool_calls_in_stream_message(
    message: &mut ChatMessage,
    choice_index: u32,
    tag_extractors: &mut FxHashMap<u32, ToolCallTagExtractor>,
    raw_json_extractors: &mut FxHashMap<u32, RawJsonToolCallExtractor>,
    id_counter: &mut u64,
) {
    let Some(content) = message.content.take() else {
        return;
    };

    // 检测是否需要 <tool_call> 标签格式处理
    let needs_tag_detection = tag_extractors.contains_key(&choice_index) || content.contains("<tool_call>");
    // 检测是否需要裸 JSON 格式处理
    let needs_raw_json_detection = raw_json_extractors.contains_key(&choice_index) || RawJsonToolCallExtractor::looks_like_tool_call_json(&content);

    if !needs_tag_detection && !needs_raw_json_detection {
        message.content = Some(content);
        return;
    }

    let mut clean_text = content.clone();
    let mut all_parsed_calls = Vec::new();

    // 优先处理 <tool_call> 标签格式
    if needs_tag_detection {
        let (text, mut parsed_calls, should_remove) = {
            let extractor = tag_extractors.entry(choice_index).or_default();
            let (text, calls) = extractor.process_chunk(&clean_text, choice_index, id_counter);
            let idle = extractor.is_idle();
            (text, calls, idle)
        };
        if should_remove {
            tag_extractors.remove(&choice_index);
        }
        clean_text = text;
        all_parsed_calls.append(&mut parsed_calls);
    }

    // 然后处理裸 JSON 格式（如果 <tool_call> 没有匹配到）
    if all_parsed_calls.is_empty() && needs_raw_json_detection {
        let (text, mut parsed_calls, should_remove) = {
            let extractor = raw_json_extractors.entry(choice_index).or_default();
            let (text, calls) = extractor.process_chunk(&clean_text, choice_index, id_counter);
            let idle = extractor.is_idle();
            (text, calls, idle)
        };
        if should_remove {
            raw_json_extractors.remove(&choice_index);
        }
        clean_text = text;
        all_parsed_calls.append(&mut parsed_calls);
    }

    if clean_text.trim().is_empty() {
        message.content = None;
    } else {
        message.content = Some(clean_text);
    }

    if !all_parsed_calls.is_empty() {
        debug!(
            "🔧 流式检测到 {} 个工具调用 (choice_index={})，自动转换为结构化工具调用",
            all_parsed_calls.len(),
            choice_index
        );
        let entry = message.tool_calls.get_or_insert_with(Vec::new);
        entry.append(&mut all_parsed_calls);
    }
}

/// OpenAI tool_choice 格式
/// - "none" / "auto" / "required" 是字符串
/// - 强制指定工具时是对象: {"type": "function", "function": {"name": "xxx"}}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    /// 字符串形式: "none", "auto", "required"
    String(ToolChoiceString),
    /// 对象形式: {"type": "function", "function": {"name": "xxx"}}
    Function(ToolChoiceFunction),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoiceString {
    None,
    Auto,
    Required,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolChoiceFunction {
    #[serde(rename = "type")]
    pub choice_type: String,
    pub function: ToolFunctionChoice,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunctionChoice {
    pub name: String,
}

impl ToolChoice {
    pub fn none() -> Self {
        ToolChoice::String(ToolChoiceString::None)
    }

    pub fn auto() -> Self {
        ToolChoice::String(ToolChoiceString::Auto)
    }

    pub fn required() -> Self {
        ToolChoice::String(ToolChoiceString::Required)
    }

    pub fn function(name: impl Into<String>) -> Self {
        ToolChoice::Function(ToolChoiceFunction {
            choice_type: "function".to_string(),
            function: ToolFunctionChoice { name: name.into() },
        })
    }
}

impl From<&str> for ToolChoice {
    fn from(s: &str) -> Self {
        match s {
            "none" => ToolChoice::none(),
            "auto" => ToolChoice::auto(),
            "required" => ToolChoice::required(),
            _ => ToolChoice::auto(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionParams {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub max_tokens: Option<u32>,
    pub repetition_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f32>,
    pub chat_template_kwargs: Option<Value>,
    /// OpenAI-compatible: response_format with json_schema
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    /// Explicitly control stream flag in non-streaming API
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Extra vendor-specific body fields to merge at top-level
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<Value>,
    // Function Call 相关参数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
}

impl Default for ChatCompletionParams {
    fn default() -> Self {
        Self {
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
            repetition_penalty: Some(1.05),
            presence_penalty: None,
            min_p: None,
            chat_template_kwargs: None, // Some(serde_json::json!({"enable_thinking": false}))
            response_format: None,
            stream: None,
            extra_body: None,
            tools: None,
            tool_choice: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repetition_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<Value>,
    // Function Call 相关字段
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<ChatMessage>,
    pub finish_reason: Option<String>,
}

/// 会话上下文管理器
#[derive(Debug, Clone)]
pub struct SessionContext {
    /// 对话历史记录
    pub messages: Vec<ChatMessage>,
    /// 最大保留的对话轮数（按 user 消息计数；tool 消息不计入轮数）
    pub max_messages: usize,
    /// 原始 system prompt（不包含时间戳）
    pub base_system_prompt: Option<String>,
    // timezone和location字段已移除 - 现在总是动态从IP地理位置获取
    /// 会话ID（用于获取IP地理位置信息）
    pub session_id: Option<String>,
}

impl SessionContext {
    /// 将消息历史裁剪到指定轮数（按 user 消息计数；保留与这些 user 轮相关的 assistant/tool 消息）
    fn trim_messages_to_max_turns(messages: &mut Vec<ChatMessage>, max_turns: usize) {
        if max_turns == 0 {
            messages.clear();
            return;
        }

        let mut user_indices: Vec<usize> = Vec::new();
        for (i, m) in messages.iter().enumerate() {
            if m.role.as_deref() == Some("user") {
                user_indices.push(i);
            }
        }

        if user_indices.len() <= max_turns {
            return;
        }

        // 保留最近 max_turns 个 user 开始的那一轮（包含该 user 之后的 assistant/tool 消息）
        let keep_from = user_indices[user_indices.len() - max_turns];
        messages.drain(0..keep_from);
    }

    /// 创建新的会话上下文，带session_id（时区和位置信息将动态从IP地理位置获取）
    pub fn new_with_session_id(max_messages: usize, system_prompt: Option<String>, session_id: Option<String>) -> Self {
        let base_system_prompt = system_prompt.clone();

        Self { messages: Vec::new(), max_messages, base_system_prompt, session_id }
    }

    // with_timezone相关构造函数已删除 - 现在总是动态从IP地理位置获取

    /// 从历史消息创建会话上下文（用于从存储恢复，时区和位置信息将动态从IP地理位置获取）
    ///
    /// 注意：session_id 是必需的，用于获取用户的地理位置和时区信息，以便为LLM提供准确的时间/位置上下文
    pub fn from_messages(max_messages: usize, system_prompt: Option<String>, messages: Vec<ChatMessage>, session_id: String) -> Self {
        let base_system_prompt = system_prompt.clone();

        // 应用轮数限制（按 user 消息计数；tool 消息不占轮数）
        let mut messages = messages;
        Self::trim_messages_to_max_turns(&mut messages, max_messages);
        // 不调用 ensure_user_boundaries_internal
        // 恢复时不应删除有效的 assistant 消息，边界处理在 get_messages 中进行

        Self { messages, max_messages, base_system_prompt, session_id: Some(session_id) }
    }

    // with_timezone相关构造函数已删除 - 现在总是动态从IP地理位置获取

    /// 添加用户消息
    pub fn add_user_message(&mut self, content: String) {
        debug!("📝 添加用户消息: '{}'", content);

        // 移除历史中所有的时间 system 消息（保证只有一个 sysTime）
        self.messages.retain(|msg| {
            if let Some(role) = &msg.role
                && role == "system"
                && let Some(content) = &msg.content
            {
                // 如果是时间 system/信息块消息，移除它
                return !(content.starts_with("[系统时间:") || content.starts_with("User Information:") || content.starts_with("[Current Time:") || content.starts_with("[System Time:"));
            }
            true
        });
        // 仅添加用户消息；时间 system 注入改为在 get_messages_fast 组合阶段统一处理
        self.add_message(ChatMessage {
            role: Some("user".to_string()),
            content: Some(content),
            tool_call_id: None,
            tool_calls: None,
        });
        debug!("📊 当前消息数量: {}", self.messages.len());
    }

    /// 添加助手回复
    pub fn add_assistant_message(&mut self, content: String) {
        debug!("🤖 添加助手消息: '{}'", content);
        self.add_message(ChatMessage {
            role: Some("assistant".to_string()),
            content: Some(content),
            tool_call_id: None,
            tool_calls: None,
        });
        debug!("📊 当前消息数量: {}", self.messages.len());
    }

    /// 添加助手回复（带工具调用）
    pub fn add_assistant_message_with_tools(&mut self, content: String, tool_calls: Option<Vec<ToolCall>>) {
        // debug!("🤖 添加助手消息(含工具): '{}'", content);
        let sanitized_tools = tool_calls.map(sanitize_tool_calls);
        if let Some(ref tools) = sanitized_tools {
            debug!("🔧 工具调用数量(已清洗): {}", tools.len());
        }
        self.add_message(ChatMessage {
            role: Some("assistant".to_string()),
            content: Some(content),
            tool_call_id: None,
            tool_calls: sanitized_tools,
        });
        debug!("📊 当前消息数量: {}", self.messages.len());
    }

    /// 添加工具调用结果
    pub fn add_tool_message(&mut self, tool_call_id: String, content: String) {
        self.add_message(ChatMessage {
            role: Some("tool".to_string()),
            content: Some(content),
            tool_call_id: Some(tool_call_id),
            tool_calls: None,
        });
    }

    /// 添加消息并管理历史长度
    fn add_message(&mut self, message: ChatMessage) {
        self.messages.push(message);

        // 保持对话轮数在限制内（按 user 消息计数；tool 消息不占轮数）
        Self::trim_messages_to_max_turns(&mut self.messages, self.max_messages);
        // 不在这里调用 ensure_user_boundaries_internal
        // 边界处理已在 get_messages 中实现，存储时不应删除 assistant 消息
    }

    /// 内部实现：确保消息列表的第一个和最后一个都是 user 角色
    #[allow(dead_code)]
    fn ensure_user_boundaries_internal(messages: &mut Vec<ChatMessage>) {
        if messages.is_empty() {
            return;
        }

        // 如果第一个消息不是 user，移除它
        while let Some(first_msg) = messages.first()
            && first_msg.role.as_deref() != Some("user")
        {
            messages.remove(0);
            if messages.is_empty() {
                break;
            }
        }

        // 如果最后一个消息不是 user，移除它
        while let Some(last_msg) = messages.last()
            && last_msg.role.as_deref() != Some("user")
        {
            messages.pop();
            if messages.is_empty() {
                break;
            }
        }
    }

    /// 将消息历史回滚/截断到指定长度（用于 stop 模式撤销本轮对话痕迹）
    pub fn truncate_to(&mut self, len: usize) {
        let target_len = std::cmp::min(len, self.messages.len());
        self.messages.truncate(target_len);
    }

    /// 🚀 性能优化：快速获取消息历史（避免长时间持有锁）
    pub async fn get_messages(&self) -> Vec<ChatMessage> {
        // 快速克隆消息历史，最小化锁持有时间
        let history = self.messages.clone();

        // 在锁外处理消息
        let mut all_messages = Vec::with_capacity(history.len() + 1);

        // 1. 洗对话历史：移除所有时间 system 消息（后续统一在最后一个user之前注入）
        let history_had_any = !history.is_empty();
        let history_no_time: Vec<ChatMessage> = history
            .into_iter()
            .filter(|m| {
                if m.role.as_deref() == Some("system")
                    && let Some(content) = &m.content
                {
                    return !(content.starts_with("[系统时间:") || content.starts_with("User Information:") || content.starts_with("[Current Time:") || content.starts_with("[System Time:"));
                }
                true
            })
            .collect();

        // 2. 快速处理历史消息
        let first_user_index = history_no_time.iter().position(|m| m.role.as_deref() == Some("user"));
        if let Some(idx) = first_user_index {
            if idx > 0 {
                warn!("🔧 丢弃前置的非user消息 {} 条，避免以 assistant/tool 开头", idx);
            }

            // 获取时区和位置信息用于时间前缀
            let (second_level_time, timezone_offset, _location_info) = get_timezone_and_location_info_from_ip(self.session_id.as_deref().unwrap_or("")).await;

            // 解析时间组件
            let mut parts = second_level_time.split_whitespace();
            let _current_date = parts.next().unwrap_or("");
            let _current_time_only = parts.next().unwrap_or("");
            let current_weekday = parts.next().unwrap_or("");

            // 构建英文时间前缀
            let time_prefix = crate::agents::runtime::build_time_prefix(&second_level_time, &timezone_offset, current_weekday);

            // 2.1 首先添加基础 system prompt（如果存在）- 固定在开头，时间前缀在最前面
            if let Some(base_prompt) = &self.base_system_prompt {
                all_messages.push(ChatMessage {
                    role: Some("system".to_string()),
                    content: Some(format!("{}{}", time_prefix, base_prompt)),
                    tool_call_id: None,
                    tool_calls: None,
                });
            }

            // 2.1.5 保留首个 user 之前的所有非时间 system 消息
            for pre in history_no_time.iter().take(idx) {
                if pre.role.as_deref() == Some("system") {
                    all_messages.push(ChatMessage {
                        role: Some("system".to_string()),
                        content: pre.content.clone(),
                        tool_call_id: None,
                        tool_calls: None,
                    });
                }
            }

            // 2.2 计算最后一个 user 的位置
            let last_user_index = history_no_time
                .iter()
                .rposition(|m| m.role.as_deref() == Some("user"))
                .unwrap_or(idx);

            // 2.3 追加从首个 user 到最后一个 user 之前的所有消息（保持顺序）
            for msg in history_no_time.iter().skip(idx).take(last_user_index.saturating_sub(idx)) {
                let mut m = msg.clone();
                if let Some(tc) = m.tool_calls.take() {
                    m.tool_calls = Some(sanitize_tool_calls(tc));
                }
                all_messages.push(m);
            }

            // 2.5 追加最后一个 user 消息
            if last_user_index < history_no_time.len() {
                let mut last_user_msg = history_no_time[last_user_index].clone();
                if let Some(tc) = last_user_msg.tool_calls.take() {
                    last_user_msg.tool_calls = Some(sanitize_tool_calls(tc));
                }
                all_messages.push(last_user_msg);
            }

            // 2.6 追加最后一个 user 之后的消息（一般为空，兼容特殊情况）
            for msg in history_no_time.iter().skip(last_user_index + 1) {
                let mut m = msg.clone();
                if let Some(tc) = m.tool_calls.take() {
                    m.tool_calls = Some(sanitize_tool_calls(tc));
                }
                all_messages.push(m);
            }
        } else if history_had_any {
            warn!("🔧 历史中不存在用户消息，丢弃全部非system历史，避免以 assistant/tool 开头");
        }

        all_messages
    }

    /// 清空对话历史（保留系统消息）
    pub fn clear(&mut self) {
        self.messages.clear();
    }
}

/// 连接池管理器 - 管理HTTP连接的健康状态和keep-alive
#[derive(Debug)]
pub struct ConnectionPoolManager {
    /// 上次keep-alive检查时间
    last_keepalive_check: std::time::Instant,
    /// keep-alive检查间隔（秒）
    keepalive_interval_secs: u64,
    /// 连接池健康状态
    pool_healthy: bool,
    /// 连接数统计
    active_connections: usize,
}

impl Default for ConnectionPoolManager {
    fn default() -> Self {
        Self {
            last_keepalive_check: std::time::Instant::now(),
            keepalive_interval_secs: 30, // 每30秒检查一次
            pool_healthy: true,
            active_connections: 0,
        }
    }
}

impl ConnectionPoolManager {
    pub fn new() -> Self {
        Self {
            last_keepalive_check: std::time::Instant::now(),
            keepalive_interval_secs: 30, // 每30秒检查一次
            pool_healthy: true,
            active_connections: 0,
        }
    }

    /// 检查是否需要进行keep-alive检查
    pub fn should_perform_keepalive(&self) -> bool {
        self.last_keepalive_check.elapsed().as_secs() >= self.keepalive_interval_secs
    }

    /// 更新最后检查时间
    pub fn update_last_keepalive_check(&mut self) {
        self.last_keepalive_check = std::time::Instant::now();
    }

    /// 设置连接池健康状态
    pub fn set_pool_health(&mut self, healthy: bool) {
        self.pool_healthy = healthy;
    }

    /// 获取连接池健康状态
    pub fn is_pool_healthy(&self) -> bool {
        self.pool_healthy
    }

    /// 增加活跃连接数
    pub fn increment_active_connections(&mut self) {
        self.active_connections += 1;
    }

    /// 减少活跃连接数
    pub fn decrement_active_connections(&mut self) {
        if self.active_connections > 0 {
            self.active_connections -= 1;
        }
    }

    /// 获取活跃连接数
    pub fn get_active_connections(&self) -> usize {
        self.active_connections
    }
}

/// 连接池统计信息
#[derive(Debug, Clone)]
pub struct ConnectionPoolStats {
    pub is_healthy: bool,
    pub active_connections: usize,
    pub last_keepalive_check: std::time::Instant,
}

pub struct LlmClient {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub timeout_secs: u64,
    pub client: Client,
    // contexts 已迁移到 agents::turn_tracker::SessionContext
    // default_max_messages/default_system_prompt 已迁移到 turn_tracker 常量
    /// 🆕 连接预热状态
    pub connection_warmed: Arc<tokio::sync::RwLock<bool>>,
    /// 🆕 连接池管理状态
    pub connection_pool_manager: Arc<tokio::sync::RwLock<ConnectionPoolManager>>,
    /// 🆕 对话持久化存储
    pub store: Arc<dyn crate::storage::ConversationStore>,
}

impl std::fmt::Debug for LlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmClient")
            .field("api_key", &"[HIDDEN]")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("timeout_secs", &self.timeout_secs)
            .field("client", &"[HTTP_CLIENT]")
            .finish()
    }
}

impl LlmClient {
    /// 使用配置结构创建客户端（推荐方式）
    pub fn from_config(config: LlmConfig) -> Self {
        Self::from_config_with_store(config, Arc::new(crate::storage::InMemoryStore::new()))
    }

    /// 使用配置结构和存储创建客户端（完整配置）
    pub fn from_config_with_store(config: LlmConfig, store: Arc<dyn crate::storage::ConversationStore>) -> Self {
        info!("🚀 创建LLM客户端，协议版本: {:?}", config.http_version);

        // 🚀 优化的HTTP客户端配置 - 针对连接复用优化
        let mut client_builder = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .danger_accept_invalid_certs(config.skip_cert_verification)
            // 🔧 连接池优化配置
            .pool_max_idle_per_host(config.max_connections_per_host)
            .pool_idle_timeout(Duration::from_secs(90)) // 空闲连接保持90秒
            .connect_timeout(Duration::from_secs(config.connection_timeout_secs))
            .tcp_keepalive(Duration::from_secs(60)) // TCP keep-alive 60秒
            .tcp_nodelay(true); // 禁用Nagle算法，减少延迟

        // 🚀 根据配置选择HTTP协议版本
        client_builder = match config.http_version {
            HttpVersion::Auto => {
                info!("📡 LLM HTTP协议: 自动协商 (推荐)");
                client_builder
                    .http2_keep_alive_interval(Duration::from_secs(30)) // HTTP/2 keep-alive
                    .http2_keep_alive_timeout(Duration::from_secs(5)) // HTTP/2 keep-alive超时
                    .http2_keep_alive_while_idle(true) // 空闲时也保持keep-alive
            },
            HttpVersion::Http1Only => {
                info!("📡 LLM HTTP协议: 强制 HTTP/1.1");
                client_builder.http1_only() // 强制使用HTTP/1.1
            },
            HttpVersion::Http2Only => {
                info!("📡 LLM HTTP协议: 强制 HTTP/2");
                client_builder
                    .http2_prior_knowledge() // 强制使用HTTP/2
                    .http2_keep_alive_interval(Duration::from_secs(30)) // HTTP/2 keep-alive
                    .http2_keep_alive_timeout(Duration::from_secs(5)) // HTTP/2 keep-alive超时
                    .http2_keep_alive_while_idle(true) // 空闲时也保持keep-alive
            },
        };

        let client = client_builder.build().expect("Failed to create optimized HTTP client");

        // default_max_messages/default_system_prompt 已迁移到 turn_tracker::DEFAULT_MAX_TURNS/DEFAULT_SYSTEM_PROMPT

        let llm_client = Self {
            api_key: config.api_key,
            base_url: config.base_url,
            model: config.model,
            timeout_secs: config.timeout_secs,
            client,
            connection_warmed: Arc::new(tokio::sync::RwLock::new(false)),
            connection_pool_manager: Arc::new(tokio::sync::RwLock::new(ConnectionPoolManager::new())),
            store,
        };

        // 🌡️ 启动连接预热（异步，不阻塞创建）
        llm_client.warm_connection_async();

        // 🚀 启动连接池维护任务
        llm_client.start_connection_pool_maintenance();

        llm_client
    }

    /// 🌡️ 异步连接预热 - 建立并缓存连接到连接池
    fn warm_connection_async(&self) {
        let client = self.client.clone();
        let base_url = self.base_url.clone();
        let api_key = self.api_key.clone();
        let connection_warmed = self.connection_warmed.clone();

        tokio::spawn(async move {
            let start_time = std::time::Instant::now();
            info!("🌡️ 开始LLM连接预热: {}", base_url);

            // 🚀 优化：使用更轻量级的健康检查请求来预热连接
            let health_check_url = format!("{}/models", base_url); // 尝试models端点

            let result = client
                .head(&health_check_url) // 🚀 优化：使用HEAD请求而不是GET
                .header("Authorization", format!("Bearer {}", api_key))
                .header("User-Agent", "realtime-llm-client/1.0")
                .timeout(Duration::from_secs(3)) // 🚀 优化：减少超时时间
                .send()
                .await;

            match result {
                Ok(response) => {
                    let status = response.status();
                    info!(
                        "✅ LLM连接预热成功: {} (状态: {}, 耗时: {:?})",
                        base_url,
                        status,
                        start_time.elapsed()
                    );

                    // 🚀 优化：HEAD请求不需要读取响应体
                    debug!("📡 LLM连接预热HEAD请求完成");
                },
                Err(e) => {
                    // 预热失败不是致命错误，只是无法享受连接复用优化
                    warn!("⚠️ LLM连接预热失败: {} - {} (耗时: {:?})", base_url, e, start_time.elapsed());

                    // 尝试简单的连接测试
                    let simple_result = client.head(&base_url).timeout(Duration::from_secs(3)).send().await;

                    if simple_result.is_ok() {
                        info!("✅ LLM连接简单预热成功: {}", base_url);
                    } else {
                        warn!("⚠️ LLM连接完全预热失败: {}", base_url);
                    }
                },
            }

            // 标记预热完成
            {
                let mut warmed = connection_warmed.write().await;
                *warmed = true;
            }
        });
    }

    /// 🆕 检查连接预热状态
    pub async fn is_connection_warmed(&self) -> bool {
        *self.connection_warmed.read().await
    }

    /// 🆕 等待连接预热完成
    pub async fn wait_for_connection_warm(&self, timeout_ms: u64) -> bool {
        let timeout = Duration::from_millis(timeout_ms);
        let start_time = std::time::Instant::now();

        while start_time.elapsed() < timeout {
            if self.is_connection_warmed().await {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    /// 🌡️ 公开的连接预热方法 - 启动异步连接预热
    pub fn start_connection_warm(&self) {
        self.warm_connection_async();
    }

    /// 🆕 启动连接池维护任务
    pub fn start_connection_pool_maintenance(&self) {
        let client = self.client.clone();
        let base_url = self.base_url.clone();
        let api_key = self.api_key.clone();
        let pool_manager = self.connection_pool_manager.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60)); // 🚀 优化：减少检查频率
            loop {
                interval.tick().await;

                // 检查是否需要进行keep-alive
                let should_check = {
                    let manager = pool_manager.read().await;
                    manager.should_perform_keepalive()
                };

                if should_check {
                    let start_time = std::time::Instant::now();
                    // 🚀 优化：使用更轻量级的健康检查
                    let health_result = client
                        .head(&base_url)
                        .header("Authorization", format!("Bearer {}", api_key))
                        .header("User-Agent", "realtime-llm-client/1.0")
                        .timeout(Duration::from_secs(2)) // 🚀 优化：减少超时时间
                        .send()
                        .await;

                    let is_healthy = health_result.is_ok();

                    // 更新连接池状态
                    {
                        let mut manager = pool_manager.write().await;
                        manager.update_last_keepalive_check();
                        manager.set_pool_health(is_healthy);
                    }

                    if !is_healthy {
                        let elapsed = start_time.elapsed();
                        warn!("⚠️ 连接池健康检查失败 (耗时: {:?})", elapsed);
                    } else {
                        trace!("✅ 连接池健康检查成功 (耗时: {:?})", start_time.elapsed());
                    }
                }
            }
        });
    }

    /// 🆕 获取连接池状态信息
    pub async fn get_connection_pool_stats(&self) -> ConnectionPoolStats {
        let manager = self.connection_pool_manager.read().await;
        ConnectionPoolStats {
            is_healthy: manager.is_pool_healthy(),
            active_connections: manager.get_active_connections(),
            last_keepalive_check: manager.last_keepalive_check,
        }
    }

    /// 初始化会话上下文（委托给 turn_tracker）
    pub async fn init_session(&self, session_id: &str, system_prompt: Option<String>) {
        crate::agents::turn_tracker::init_session(session_id, system_prompt, self.store.as_ref()).await;
    }

    /// 清理会话上下文
    pub async fn cleanup_session(&self, session_id: &str) {
        crate::agents::turn_tracker::remove_tracker(session_id).await;
        debug!("🗑️ 清理会话 {} 的LLM上下文", session_id);
    }

    /// 清空会话对话历史
    pub async fn clear_session_history(&self, session_id: &str) {
        crate::agents::turn_tracker::clear_session(session_id).await;
        debug!("🧹 清空会话 {} 的对话历史", session_id);
    }

    pub async fn chat(&self, messages: Vec<ChatMessage>, params: Option<ChatCompletionParams>) -> anyhow::Result<ChatCompletionResponse> {
        // 🔒 锁定流式：将非流式调用聚合自 chat_stream，彻底丢弃直连非流式HTTP路径
        let stream = self.chat_stream(messages, params).await?;
        futures_util::pin_mut!(stream);

        let mut accumulated_text = String::new();
        let mut accumulated_tool_calls: Vec<ToolCall> = Vec::new();
        let mut tool_calls_detected = false;
        let mut final_finish_reason: Option<String> = None;
        let mut last_choice_index: u32 = 0;

        let start_ts = chrono::Utc::now().timestamp() as u64;
        while let Some(item) = stream.next().await {
            match item {
                Ok(choice) => {
                    last_choice_index = choice.index;
                    if let Some(delta) = &choice.delta {
                        if let Some(text) = &delta.content
                            && !text.is_empty()
                        {
                            accumulated_text.push_str(text);
                        }
                        if let Some(tc_delta) = &delta.tool_calls
                            && !tc_delta.is_empty()
                        {
                            tool_calls_detected = true;
                            for d in tc_delta {
                                merge_tool_call_delta(&mut accumulated_tool_calls, d);
                            }
                        }
                    } else if let Some(message) = &choice.message {
                        if let Some(text) = &message.content
                            && !text.is_empty()
                        {
                            accumulated_text.push_str(text);
                        }
                        if let Some(tcs) = &message.tool_calls
                            && !tcs.is_empty()
                        {
                            tool_calls_detected = true;
                            for tc in tcs {
                                accumulated_tool_calls.push(tc.clone());
                            }
                        }
                    }
                    if let Some(fr) = &choice.finish_reason {
                        final_finish_reason = Some(fr.clone());
                    }
                },
                Err(e) => {
                    return Err(anyhow!("聚合流式响应失败: {}", e));
                },
            }
        }

        let message = ChatMessage {
            role: Some("assistant".to_string()),
            content: if tool_calls_detected { None } else { Some(accumulated_text) },
            tool_call_id: None,
            tool_calls: if tool_calls_detected { Some(accumulated_tool_calls) } else { None },
        };

        let choice = Choice {
            index: last_choice_index,
            message: Some(message),
            delta: None,
            finish_reason: Some(if tool_calls_detected {
                "tool_calls".to_string()
            } else {
                final_finish_reason.unwrap_or_else(|| "stop".to_string())
            }),
        };

        let response = ChatCompletionResponse {
            id: format!("chatcmpl-stream-agg-{}", uuid::Uuid::new_v4()),
            object: "chat.completion".to_string(),
            created: start_ts,
            model: self.model.clone(),
            choices: vec![choice],
        };

        Ok(response)
    }

    pub async fn chat_stream(&self, messages: Vec<ChatMessage>, params: Option<ChatCompletionParams>) -> anyhow::Result<impl futures_util::Stream<Item = anyhow::Result<Choice>>> {
        const MAX_RETRY_ATTEMPTS: u32 = 3;
        let mut retry_count = 0;
        let mut last_error: Option<String> = None;

        while retry_count < MAX_RETRY_ATTEMPTS {
            retry_count += 1;

            if retry_count > 1 {
                info!(
                    "🔄 LLM请求重试 ({}/{}): session_id={}",
                    retry_count, MAX_RETRY_ATTEMPTS, "unknown"
                );
            }

            let params = params.clone().unwrap_or_default();
            let req = ChatCompletionRequest {
                model: self.model.clone(),
                messages: messages.clone(),
                stream: Some(true),
                temperature: params.temperature,
                top_p: params.top_p,
                top_k: params.top_k,
                max_tokens: params.max_tokens,
                repetition_penalty: params.repetition_penalty,
                presence_penalty: params.presence_penalty,
                min_p: params.min_p,
                chat_template_kwargs: params.chat_template_kwargs,
                response_format: params.response_format.clone(),
                tools: params.tools,
                tool_choice: params.tool_choice,
            };

            // 简化请求日志
            info!(
                "LLM 请求: url={}/chat/completions, model={}, msgs={}, timeout={}s",
                self.base_url,
                self.model,
                messages.len(),
                self.timeout_secs
            );

            let url = format!("{}/chat/completions", self.base_url);

            // 🚀 性能优化：使用更高效的序列化
            let req_json = match serde_json::to_string(&req) {
                Ok(json) => json,
                Err(e) => {
                    error!("❌ [LLM] 请求体序列化失败: {}", e);
                    return Err(anyhow!("请求体序列化失败: {}", e));
                },
            };
            // reqwest::RequestBuilder::body 会消耗 String；这里保留一份给 telemetry 上报
            let req_json_for_body = req_json.clone();

            // 🚀 性能优化：只在调试模式下打印完整请求体
            if tracing::enabled!(tracing::Level::DEBUG) {
                info!("LLM 请求体(JSON): {}", req_json);
            } else {
                info!("LLM 请求体大小: {} bytes", req_json.len());
            }

            let started = std::time::Instant::now();
            let resp = timeout(Duration::from_secs(self.timeout_secs), async {
                info!("🟢 [LLM] POST {} ({} bytes)", url, req_json.len());

                self.client
                    .post(&url)
                    .header("Authorization", format!("Bearer {}", self.api_key))
                    .header("Content-Type", "application/json")
                    .body(req_json_for_body) // 🔧 使用已序列化的JSON字符串
                    .send()
                    .await
            })
            .await;

            match resp {
                Ok(Ok(response)) => {
                    let status = response.status();
                    if !status.is_success() {
                        // 🔧 改进错误处理：先检查状态码，再读取响应体
                        let response_text = match response.text().await {
                            Ok(text) => text,
                            Err(e) => format!("无法读取响应内容: {}", e),
                        };

                        error!("❌ LLM HTTP响应错误: status={}, body={}", status, response_text);

                        // 🆕 针对不同状态码给出具体建议
                        match status.as_u16() {
                            500 => {
                                error!("💡 HTTP 500错误建议:");
                                error!("   1. 检查模型名称是否正确: {}", self.model);
                                error!("   2. 检查请求参数是否兼容");
                                error!("   3. 检查服务器日志");
                                if req.tools.is_some() {
                                    error!("   4. 当前请求包含tools，可能是工具调用格式不兼容");
                                }
                                if req.chat_template_kwargs.is_some() {
                                    error!("   5. 当前请求包含chat_template_kwargs，可能不被支持");
                                }
                            },
                            401 => error!("💡 认证失败，请检查API Key是否正确"),
                            404 => error!("💡 模型或端点不存在，请检查URL和模型名称"),
                            _ => error!("💡 其他HTTP错误，请检查服务器状态"),
                        }

                        // 对于HTTP错误，不进行重试（通常是配置问题）
                        return Err(anyhow!("LLM HTTP响应错误 {}: {}", status, response_text));
                    }

                    // Langfuse telemetry：上报 LLM post body（包含历史 messages / tools / params）
                    if let Some(ctx) = telemetry::current_trace_context() {
                        telemetry::emit(telemetry::TraceEvent::LlmGeneration {
                            session_id: ctx.session_id,
                            turn_id: ctx.turn_id,
                            model: self.model.clone(),
                            input_messages: messages.len(),
                            request_json: Some(req_json.clone()),
                            output_text: None,
                            input_tokens: None,
                            output_tokens: None,
                            latency_ms: started.elapsed().as_millis() as u64,
                        });
                    }

                    // 成功获取响应，返回流
                    let stream = response.bytes_stream();
                    return Ok(stream! {
                        // 🚀 性能优化：预分配缓冲区大小
                        let mut buffer = String::with_capacity(4096);
                        let mut chunk_count = 0;
                        let mut has_received_data = false;
                        let mut stream_tool_call_detectors: FxHashMap<u32, ToolCallTagExtractor> = FxHashMap::default();
                        let mut stream_raw_json_detectors: FxHashMap<u32, RawJsonToolCallExtractor> = FxHashMap::default();
                        let mut stream_detected_counter: u64 = 0;

                        futures_util::pin_mut!(stream);
                        while let Some(chunk) = stream.next().await {
                            match chunk {
                                Ok(bytes) => {
                                    chunk_count += 1;
                                    // 🚀 性能优化：避免不必要的字符串转换
                                    let chunk_str = String::from_utf8_lossy(&bytes);
                                    buffer.push_str(&chunk_str);
                                    // 🚀 性能优化：使用更高效的字符串处理
                                    while let Some(line_end) = buffer.find('\n') {
                                            let line = buffer[..line_end].to_string();
                                            buffer = buffer[line_end + 1..].to_string();

                                            if let Some(data) = line.strip_prefix("data: ") {
                                                if data.trim() == "[DONE]" {
                                                    info!("🏁 LLM流式响应结束标记 [DONE]");
                                                    break;
                                                }
                                                if !data.trim().is_empty() {
                                                    has_received_data = true;

                                                    match serde_json::from_str::<ChatCompletionResponse>(data.trim()) {
                                                        Ok(resp) => {
                                                            for mut choice in resp.choices {
                                                                if let Some(delta) = choice.delta.as_mut() {
                                                                    detect_tool_calls_in_stream_message(
                                                                        delta,
                                                                        choice.index,
                                                                        &mut stream_tool_call_detectors,
                                                                        &mut stream_raw_json_detectors,
                                                                        &mut stream_detected_counter,
                                                                    );
                                                                }
                                                                if let Some(message) = choice.message.as_mut() {
                                                                    detect_tool_calls_in_stream_message(
                                                                        message,
                                                                        choice.index,
                                                                        &mut stream_tool_call_detectors,
                                                                        &mut stream_raw_json_detectors,
                                                                        &mut stream_detected_counter,
                                                                    );
                                                                }
                                                                // 将delta转换为message格式以保持一致性
                                                                if choice.delta.is_some() {
                                                                    yield Ok(choice);
                                                                } else if choice.message.is_some() {
                                                                    // 处理完整消息的情况
                                                                    yield Ok(choice);
                                                                }
                                                            }
                                                        },
                                                        Err(e) => {
                                                            warn!("⚠️ LLM流式响应JSON解析失败: {}", e);
                                                            if chunk_count <= 5 {
                                                                warn!("📄 原始数据: {}", data.trim());
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                    }
                                }
                                Err(e) => {
                                    // 记录网络错误但不立即中断，尝试继续处理剩余数据
                                    warn!("⚠️ 流式响应网络错误(继续尝试): {}", e);
                                    yield Err(anyhow!("流式响应网络错误: {}", e));
                                    break;
                                }
                            }
                        }

                        // 🆕 流结束时的总结日志
                        if !has_received_data {
                            error!("❌ LLM流式响应没有接收到任何有效数据！可能的原因：");
                            error!("   1. 模型名称错误: {}", self.model);
                            error!("   2. 服务器地址错误: {}", self.base_url);
                        } else {
                            info!("✅ LLM流式响应处理完成，共处理 {} 个数据块", chunk_count);
                        }
                    });
                },
                Ok(Err(e)) => {
                    let error_msg = format!("{}", e);
                    last_error = Some(error_msg.clone());
                    error!(
                        "❌ [LLM] HTTP请求失败 (尝试 {}/{}): {}",
                        retry_count, MAX_RETRY_ATTEMPTS, error_msg
                    );

                    if retry_count < MAX_RETRY_ATTEMPTS {
                        info!("🔄 立即重试");
                        continue;
                    }
                },
                Err(_) => {
                    error!("❌ [LLM] HTTP请求超时 ({}秒)", self.timeout_secs);
                    return Err(anyhow!("HTTP请求超时"));
                },
            }
        }

        // 所有重试都失败了
        let error_msg = last_error.unwrap_or_else(|| "未知错误".to_string());
        Err(anyhow!("HTTP请求失败: {} (重试{}次后)", error_msg, MAX_RETRY_ATTEMPTS))
    }

    /// 创建工具定义的便捷方法
    pub fn create_tool(name: &str, description: &str, parameters: Value) -> Tool {
        Tool {
            tool_type: "function".to_string(),
            function: ToolFunction { name: name.to_string(), description: description.to_string(), parameters },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_system_prompt_structure() {
        let system_prompt = "你是一个AI助手。".to_string();
        let mut context = SessionContext::new_with_session_id(10, Some(system_prompt.clone()), Some("test_session".to_string()));

        // 添加用户消息
        context.add_user_message("你好".to_string());

        let messages = context.get_messages().await;

        // 验证消息结构：系统提示词应该在最后一个用户消息之前
        let last_user_idx = messages
            .iter()
            .rposition(|m| m.role.as_deref() == Some("user"))
            .expect("应该有用户消息");

        // 检查系统提示词
        let system_msg = messages
            .iter()
            .find(|m| m.role.as_deref() == Some("system") && m.content.as_ref().is_some_and(|c| c.contains("你是一个AI助手。")))
            .expect("未找到系统消息");

        // 验证系统提示词在正确的位置（紧邻最后一个用户消息）
        let system_idx = messages
            .iter()
            .position(|m| m.role.as_deref() == Some("system") && m.content.as_ref().is_some_and(|c| c.contains("你是一个AI助手。")))
            .expect("未找到系统消息");
        assert_eq!(system_idx + 1, last_user_idx, "系统提示词应该紧邻最后一个用户消息之前");

        let system_content = system_msg.content.as_ref().unwrap();
        // 验证时间前缀在最前面
        assert!(system_content.starts_with("[System Time:"), "系统提示词应该以时间前缀开头");
        assert!(system_content.contains("Timezone:"), "系统提示词应该包含时区");
        // 验证原始 prompt 内容也在
        assert!(system_content.contains("你是一个AI助手。"));
        // 验证不再使用旧格式
        assert!(!system_content.contains("User Information:"));
    }

    #[tokio::test]
    async fn test_timezone_detection() {
        // 测试IP地理位置获取函数
        let (time, offset, location) = get_timezone_and_location_info_from_ip("test_session").await;

        // 验证时间格式（包含星期几，长度会更长）
        assert!(time.len() >= 19); // "YYYY-MM-DD HH:MM:SS Weekday"
        // 验证包含英文星期（无 session metadata 时 language=None，使用英文）
        let has_weekday = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"]
            .iter()
            .any(|d| time.contains(d));
        assert!(has_weekday, "time should contain an English weekday, got: {}", time);
        // 验证时区偏移格式
        assert!(offset.starts_with('+') || offset.starts_with('-'));
        assert!(offset.len() >= 5); // "+0800" or "-0500"
        // 验证返回了位置信息（可能是默认值）
        assert!(!location.is_empty());

        println!("Time: {}, Offset: {}, Location: {}", time, offset, location);
    }

    #[test]
    fn test_independent_timezone_location_fallback() {
        // 🆕 测试timezone和location独立fallback逻辑
        // 注意：直接测试底层实现函数，不依赖GlobalSessionManager和CONNECTION_METADATA_CACHE

        // 测试场景1：只提供timezone，不提供location
        {
            let (time, offset, location) = get_timezone_and_location_info_impl_independent(Some("America/New_York"), None, None);

            println!(
                "🕒 场景1(只有timezone) - Time: {}, Offset: {}, Location: {}",
                time, offset, location
            );

            // timezone应该使用用户提供的America/New_York时区（东部时间）
            // 东部时间在标准时间是UTC-5(-0500)，夏令时是UTC-4(-0400)
            assert!(offset.starts_with("-")); // 应该是负偏移
            // location应该fallback到默认（英文，因为 language=None）
            assert!(location.contains("Asia/Shanghai") || location.contains("China Standard Time"));
        }

        // 测试场景2：只提供location，不提供timezone
        {
            let (time, offset, location) = get_timezone_and_location_info_impl_independent(None, Some("美国纽约"), None);

            // timezone应该fallback到默认UTC+8
            assert_eq!(offset, "+0800");
            // location应该使用用户提供的
            assert!(location.contains("美国纽约"));

            println!(
                "📍 场景2(只有location) - Time: {}, Offset: {}, Location: {}",
                time, offset, location
            );
        }

        // 测试场景3：都不提供，完全使用默认值
        {
            let (time, offset, location) = get_timezone_and_location_info_impl_independent(None, None, None);

            // 都应该使用默认值（英文，因为 language=None）
            assert_eq!(offset, "+0800");
            assert!(location.contains("Asia/Shanghai") || location.contains("China Standard Time"));

            println!(
                "🌍 场景3(都使用默认) - Time: {}, Offset: {}, Location: {}",
                time, offset, location
            );
        }

        // 测试场景4：都提供
        {
            let (time, offset, location) = get_timezone_and_location_info_impl_independent(Some("Europe/London"), Some("英国伦敦"), None);

            // 都应该使用用户提供的
            assert!(offset == "+0000" || offset == "+0100"); // DST可能影响偏移
            assert!(location.contains("英国伦敦"));

            println!(
                "🇬🇧 场景4(都有用户提供) - Time: {}, Offset: {}, Location: {}",
                time, offset, location
            );
        }
    }

    #[tokio::test]
    async fn test_system_position_llm_task_flow() {
        // 模拟 llm_task 的调用流程：多轮 user/assistant，检查 system 的位置
        let system_prompt = "你是一个AI助手。".to_string();
        let mut context = SessionContext::new_with_session_id(50, Some(system_prompt.clone()), Some("test_session".to_string()));

        // 在首个 user 之前插入一条保留的 system
        context.messages.push(ChatMessage {
            role: Some("system".to_string()),
            content: Some("这是保留的系统消息".to_string()),
            tool_call_id: None,
            tool_calls: None,
        });

        // 第一次用户提问 -> 助手回答
        context.add_user_message("现在几点了？".to_string());
        context.add_assistant_message("13:44".to_string());

        // 第二次用户提问（最后一个 user）
        context.add_user_message("饭号是不是很适合散步？".to_string());

        // 组装发送给 LLM 的消息
        let messages = context.get_messages().await;

        // 1) 保留的非时间 system 应该在消息列表前部（在首个 user 之前）
        let preserved_index = messages
            .iter()
            .position(|m| m.role.as_deref() == Some("system") && m.content.as_deref() == Some("这是保留的系统消息"))
            .expect("未找到保留的系统消息");

        // 2) 验证保留的系统消息在第一个用户消息之前
        let first_user_index = messages
            .iter()
            .position(|m| m.role.as_deref() == Some("user"))
            .expect("应该有用户消息");
        assert!(preserved_index < first_user_index, "保留的系统消息应该在第一个用户消息之前");

        // 3) 验证系统消息仅出现一次，并且位于消息列表开头
        let system_indexes: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                if m.role.as_deref() == Some("system") {
                    if let Some(c) = &m.content {
                        if c.contains("你是一个AI助手。") {
                            return Some(i);
                        }
                    }
                }
                None
            })
            .collect();
        assert_eq!(system_indexes.len(), 1, "系统 prompt 应该只注入一次");
        let system_idx = system_indexes[0];
        assert_eq!(system_idx, 0, "系统 prompt 应该在消息列表开头");

        // 4) 系统消息的内容包含时间前缀和基础 prompt
        let system_content = messages[system_idx].content.as_ref().unwrap();
        assert!(system_content.starts_with("[System Time:"), "系统 prompt 应该以时间前缀开头");
        assert!(
            system_content.contains("你是一个AI助手。"),
            "系统 prompt 应该包含基础 prompt 内容"
        );
        // 验证不再使用旧格式
        assert!(!system_content.contains("User Information:"));
    }

    #[test]
    fn tool_call_tag_extractor_parses_json_block() {
        let mut counter = 0;
        let mut extractor = ToolCallTagExtractor { pending: String::new() };
        let (clean_text, calls) = extractor.process_chunk(
            "<tool_call>{\"name\":\"reminder\",\"arguments\":{\"title\":\"喝水\"}}</tool_call>",
            0,
            &mut counter,
        );
        assert!(clean_text.is_empty());
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.function.name.as_deref(), Some("reminder"));
        assert!(call.function.arguments.as_deref().unwrap().contains("喝水"));
    }

    #[test]
    fn tool_call_tag_extractor_handles_streaming_chunks() {
        let mut counter = 0;
        let mut extractor = ToolCallTagExtractor { pending: String::new() };
        let (part_text, part_calls) = extractor.process_chunk("<tool_call>{\"name\":\"rem", 0, &mut counter);
        assert!(part_text.is_empty());
        assert!(part_calls.is_empty());

        let (final_text, calls) = extractor.process_chunk("inder\",\"arguments\":{\"title\":\"喝水\"}}</tool_call>谢谢", 0, &mut counter);
        assert_eq!(final_text, "谢谢");
        assert_eq!(calls.len(), 1);
        assert_eq!(extractor.pending, "");
    }
}
