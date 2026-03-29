//! 通用流式 LLM 处理工具，支持真正的 next-token 打断
//!
//! 所有 agent 可以使用这个模块来处理 LLM stream，自动获得打断支持。

use futures_util::StreamExt;
use tracing::{debug, error, warn};

use crate::llm::{ChatMessage, ToolCall};

use super::{AgentCancelToken, AgentTtsSink};

/// 检测文本是否看起来像 JSON 或工具调用参数（用于过滤 LLM 错误输出的工具参数）
///
/// 注意：此函数会丢弃匹配的内容。对于需要提取字段的情况（如函数调用泄露），
/// 不要在这里捕获，让 text_filters::filter_for_tts 在 TTS 层处理。
fn looks_like_json(text: &str) -> bool {
    let trimmed = text.trim();
    // 检测以 { 或 [ 开头，且包含引号的内容
    if (trimmed.starts_with('{') || trimmed.starts_with('[')) && trimmed.contains('"') {
        return true;
    }
    // 检测 LLM 错误输出的工具调用片段（如 "arguments": {...}）
    if trimmed.contains("\"arguments\"") && trimmed.contains('{') {
        return true;
    }
    false
}

/// 流式处理结果
#[derive(Debug)]
pub struct StreamResult {
    /// 累积的文本内容
    pub text: String,
    /// 累积的工具调用
    pub tool_calls: Vec<ToolCall>,
    /// 是否检测到工具调用
    pub has_tool_calls: bool,
    /// 是否被打断
    pub was_interrupted: bool,
}

impl StreamResult {
    pub fn empty() -> Self {
        Self {
            text: String::new(),
            tool_calls: Vec::new(),
            has_tool_calls: false,
            was_interrupted: false,
        }
    }

    pub fn interrupted() -> Self {
        Self {
            text: String::new(),
            tool_calls: Vec::new(),
            has_tool_calls: false,
            was_interrupted: true,
        }
    }
}

/// 配置选项
pub struct StreamOptions<'a> {
    /// 是否实时发送文本到 TTS
    pub stream_to_tts: bool,
    /// TTS sink（如果 stream_to_tts 为 true）
    pub tts_sink: Option<&'a dyn AgentTtsSink>,
    /// 打断令牌
    pub cancel: &'a dyn AgentCancelToken,
    /// 日志标签（用于调试）
    pub log_tag: &'a str,
}

impl<'a> StreamOptions<'a> {
    pub fn new(cancel: &'a dyn AgentCancelToken) -> Self {
        Self { stream_to_tts: false, tts_sink: None, cancel, log_tag: "stream" }
    }

    pub fn with_tts(mut self, tts_sink: &'a dyn AgentTtsSink) -> Self {
        self.stream_to_tts = true;
        self.tts_sink = Some(tts_sink);
        self
    }

    pub fn with_tag(mut self, tag: &'a str) -> Self {
        self.log_tag = tag;
        self
    }
}

/// 使用 tokio::select! 处理 LLM stream，支持真正的 next-token 打断
///
/// # 参数
/// - `stream`: LLM 返回的流（已 pin）
/// - `options`: 配置选项
///
/// # 返回
/// - `StreamResult`: 包含累积的文本、工具调用和打断状态
pub async fn process_stream_with_interrupt<S, E>(stream: std::pin::Pin<Box<S>>, options: StreamOptions<'_>) -> StreamResult
where
    S: futures_util::Stream<Item = Result<crate::llm::llm::Choice, E>>,
    E: std::fmt::Display,
{
    // 使用 tokio::pin! 来安全地处理 !Unpin 的 stream
    tokio::pin!(stream);
    let mut result = StreamResult::empty();

    loop {
        tokio::select! {
            biased; // 优先检查打断分支

            // 打断分支：与 stream.next() 真正并发
            _ = options.cancel.cancelled() => {
                debug!("🛑 {} interrupted at token boundary", options.log_tag);
                result.was_interrupted = true;
                break;
            }

            // Token 分支
            maybe_item = stream.next() => {
                match maybe_item {
                    Some(Ok(choice)) => {
                        process_choice(&choice, &mut result, &options).await;
                    }
                    Some(Err(e)) => {
                        error!("{} stream error: {}", options.log_tag, e);
                        break;
                    }
                    None => break, // Stream 结束
                }
            }
        }
    }

    result
}

/// 处理单个 choice（内部函数）
async fn process_choice(choice: &crate::llm::llm::Choice, result: &mut StreamResult, options: &StreamOptions<'_>) {
    // 处理 delta（流式增量）
    if let Some(delta) = &choice.delta {
        if let Some(text) = &delta.content
            && !text.is_empty()
            && !result.has_tool_calls
        {
            // 过滤 JSON 格式的内容（LLM 可能错误地把工具参数当作文本输出）
            if looks_like_json(text) {
                warn!("🚫 过滤疑似 JSON 的 LLM 输出: '{}'", text);
            } else {
                result.text.push_str(text);
                if options.stream_to_tts
                    && let Some(tts) = options.tts_sink
                {
                    tts.send(text).await;
                }
            }
        }
        if let Some(tc_delta) = &delta.tool_calls
            && !tc_delta.is_empty()
        {
            result.has_tool_calls = true;
            for d in tc_delta {
                merge_tool_call_delta(&mut result.tool_calls, d);
            }
        }
    }
    // 处理 message（完整消息，非流式兼容）
    else if let Some(message) = &choice.message {
        if let Some(text) = &message.content
            && !text.is_empty()
            && !result.has_tool_calls
        {
            // 过滤 JSON 格式的内容（LLM 可能错误地把工具参数当作文本输出）
            if looks_like_json(text) {
                warn!("🚫 过滤疑似 JSON 的 LLM 输出: '{}'", text);
            } else {
                result.text.push_str(text);
                if options.stream_to_tts
                    && let Some(tts) = options.tts_sink
                {
                    tts.send(text).await;
                }
            }
        }
        if let Some(tcs) = &message.tool_calls
            && !tcs.is_empty()
        {
            result.has_tool_calls = true;
            for tc in tcs {
                result.tool_calls.push(tc.clone());
            }
        }
    }
}

/// 合并工具调用增量（通用函数，所有 agent 可复用）
pub fn merge_tool_call_delta(accumulated_calls: &mut Vec<ToolCall>, delta: &ToolCall) {
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
        // 如果没有 id，生成一个基于 index 的 id
        let id = delta.id.clone().or_else(|| delta.index.map(|idx| format!("tool_call_{}", idx)));
        let new_call = ToolCall {
            id,
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

/// 构建 assistant 消息（通用函数）
/// 如果有工具调用，确保每个 tool_call 都有 id（用于与 tool 结果消息关联）
pub fn build_assistant_message(result: &StreamResult) -> ChatMessage {
    let tool_calls = if result.has_tool_calls && !result.tool_calls.is_empty() {
        // 确保每个 tool_call 都有 id
        let calls_with_ids: Vec<ToolCall> = result
            .tool_calls
            .iter()
            .enumerate()
            .map(|(i, tc)| {
                let mut tc = tc.clone();
                if tc.id.is_none() {
                    tc.id = Some(format!("tool_call_{}", i));
                }
                tc
            })
            .collect();
        Some(calls_with_ids)
    } else {
        None
    };

    ChatMessage {
        role: Some("assistant".to_string()),
        content: if result.has_tool_calls { None } else { Some(result.text.clone()) },
        tool_call_id: None,
        tool_calls,
    }
}

/// 生成工具调用 ID（通用函数）
pub fn ensure_tool_call_id(tool_call: &ToolCall, prefix: &str, loop_index: usize, call_index: usize) -> String {
    tool_call
        .id
        .clone()
        .unwrap_or_else(|| format!("{}_tool_{}_{}", prefix, loop_index, call_index))
}
