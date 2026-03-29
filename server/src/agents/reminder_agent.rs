use async_trait::async_trait;
use chrono::Utc;

use super::stream_utils::{StreamOptions, build_assistant_message, process_stream_with_interrupt};
use super::system_prompt_registry::SystemPromptRegistry;
use super::tool_utils::{UNSUPPORTED_TOOL_TEXT, append_tool_by_name, collect_tool_names};
use super::{Agent, AgentContext, AgentHandles, ToolCallOutcome, ToolControl};
use crate::llm::{ChatCompletionParams, ChatMessage, Tool, ToolCall, ToolChoice, ToolFunction};
use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use tracing::debug;

const MAX_AGENT_LOOPS: usize = 4;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ReminderFunctionPayload {
    /// 支持两种格式：工具定义的 "title" 和历史对话中的 "content"
    #[serde(alias = "title", alias = "content")]
    title: String,
    /// 支持两种格式：工具定义的 "time" 和历史对话中的 "startAt"
    #[serde(alias = "time", alias = "startAt")]
    remind_at: String,
    /// 支持两种格式：工具定义的 "type" 和历史对话中的 "reminderType"
    #[serde(alias = "type", alias = "reminderType", default = "ReminderFunctionPayload::default_reminder_type")]
    reminder_type: String,
}

impl ReminderFunctionPayload {
    fn default_reminder_type() -> String {
        "set".to_string()
    }
}

pub struct ReminderAgent;

impl Default for ReminderAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl ReminderAgent {
    pub fn new() -> Self {
        Self
    }

    /// 合并专用工具 + 共享工具库（专用工具优先）
    ///
    /// # 工具过滤规则
    /// - 关键字过滤: 包含 "remind" 关键字的工具（不区分大小写）
    ///
    /// # 内置工具注入
    /// - `world_clock`: 时间查询工具（用于提醒时间计算）
    ///
    /// # 工具合并流程
    /// 1. 添加专用工具 "reminder"
    /// 2. 从共享工具中筛选包含 "remind" 关键字的工具
    /// 3. 追加内置工具（如果存在于 shared_tools 中）
    fn merge_tools(shared_tools: &[Tool]) -> Vec<Tool> {
        use std::collections::HashSet;
        let mut tools = Vec::new();
        let mut tool_names: HashSet<String> = HashSet::new();

        // 1. 专用工具优先
        let reminder_tool = Self::reminder_tool();
        tool_names.insert(reminder_tool.function.name.clone());
        tools.push(reminder_tool);

        // 2. 合并过滤后的共享工具库（去重）
        // 只保留包含 remind 关键字的工具
        for t in shared_tools {
            if !tool_names.contains(&t.function.name) {
                let name_lower = t.function.name.to_ascii_lowercase();
                if name_lower.contains("remind") {
                    tool_names.insert(t.function.name.clone());
                    tools.push(t.clone());
                }
            }
        }

        // 3. 注入内置工具: world_clock
        append_tool_by_name(&mut tools, &mut tool_names, shared_tools, "world_clock");

        tools
    }

    /// 检查是否有可用的日程/提醒工具（需要外部工具支持）
    fn has_reminder_tools(shared_tools: &[Tool]) -> bool {
        shared_tools.iter().any(|t| {
            let name = t.function.name.to_lowercase();
            name.contains("remind") || name.contains("calendar") || name.contains("schedule")
        })
    }

    fn reminder_tool() -> Tool {
        Tool {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "reminder".to_string(),
                description: "设置提醒或日程".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "title": {
                            "type": "string",
                            "description": "提醒的简短描述"
                        },
                        "time": {
                            "type": "string",
                            "description": "提醒时间，格式为 YYYY-MM-DD HH:MM:SS（使用当地时间）"
                        },
                        "type": {
                            "type": "string",
                            "enum": ["set"],
                        }
                    },
                    "required": ["title", "time", "type"],
                    "additionalProperties": false
                }),
            },
        }
    }

    fn build_loop_params(is_last_loop: bool, tools: &[Tool]) -> ChatCompletionParams {
        ChatCompletionParams {
            tools: Some(tools.to_vec()),
            tool_choice: Some(if is_last_loop { ToolChoice::none() } else { ToolChoice::auto() }),
            stream: Some(true),
            response_format: None,
            ..Default::default()
        }
    }

    fn ensure_tool_call_id(tool_call: &ToolCall, loop_index: usize, call_index: usize) -> String {
        tool_call
            .id
            .clone()
            .unwrap_or_else(|| format!("reminder_tool_call_{}_{}", loop_index, call_index))
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_tool_call(
        &self,
        tool_call: &ToolCall,
        loop_index: usize,
        call_index: usize,
        handles: &AgentHandles<'_>,
        messages: &mut Vec<ChatMessage>,
        allowed_tools: &HashSet<String>,
        has_streamed_text: bool,
    ) -> ToolControl {
        let tool_name = tool_call.function.name.as_deref().unwrap_or("").to_string();
        let call_id = Self::ensure_tool_call_id(tool_call, loop_index, call_index);
        if tool_name.trim().is_empty() {
            let text = "assistant 发起的工具调用缺少名称".to_string();
            messages.push(ChatMessage {
                role: Some("tool".into()),
                content: Some(text),
                tool_call_id: Some(call_id),
                tool_calls: None,
            });
            return ToolControl::Continue;
        }

        if !allowed_tools.contains(&tool_name) {
            messages.push(ChatMessage {
                role: Some("tool".into()),
                content: Some(UNSUPPORTED_TOOL_TEXT.to_string()),
                tool_call_id: Some(call_id),
                tool_calls: None,
            });
            return ToolControl::Continue;
        }

        let raw_args = tool_call.function.arguments.clone().unwrap_or_else(|| "{}".to_string());
        let outcome = match self.invoke_tool(&tool_name, &raw_args, handles).await {
            Ok(outcome) => outcome,
            Err(err) => {
                let text = format!("工具 {} 调用失败: {}", tool_name, err);
                messages.push(ChatMessage {
                    role: Some("tool".into()),
                    content: Some(text),
                    tool_call_id: Some(call_id),
                    tool_calls: None,
                });
                return ToolControl::Continue;
            },
        };

        // 先处理控制信号：Stop / Interrupted 不发送 TTS、不落上下文
        match outcome.control {
            ToolControl::Stop => ToolControl::Stop,
            ToolControl::Interrupted => ToolControl::Interrupted,
            ToolControl::Respond(ref tts_text) => {
                let tool_text = if outcome.context_text.trim().is_empty() {
                    format!("工具 {} 未返回可用文本。", tool_name)
                } else {
                    outcome.context_text.clone()
                };
                let speak_text = if tts_text.trim().is_empty() { tool_text } else { tts_text.clone() };
                if speak_text.trim().is_empty() {
                    return ToolControl::Continue;
                }
                // 只有当 LLM 没有流式输出过文字时，才发送工具返回的 TTS
                // 避免 LLM 输出 + 工具返回 导致两次确认
                if !has_streamed_text {
                    handles.tts_sink.send(&speak_text).await;
                }
                ToolControl::Respond(speak_text)
            },
            ToolControl::Continue => {
                let tool_text = if outcome.context_text.trim().is_empty() {
                    format!("工具 {} 未返回可用文本。", tool_name)
                } else {
                    outcome.context_text.clone()
                };

                messages.push(ChatMessage {
                    role: Some("tool".into()),
                    content: Some(tool_text.clone()),
                    tool_call_id: Some(call_id),
                    tool_calls: None,
                });

                ToolControl::Continue
            },
        }
    }

    async fn invoke_tool(&self, tool_name: &str, raw_args: &str, handles: &AgentHandles<'_>) -> anyhow::Result<ToolCallOutcome> {
        let args = if tool_name == "reminder" {
            Self::convert_reminder_args(raw_args)?
        } else {
            raw_args.to_string()
        };
        handles.tool_client.call(tool_name, &args).await
    }

    fn convert_reminder_args(raw_args: &str) -> anyhow::Result<String> {
        if raw_args.trim().is_empty() {
            return Err(anyhow!("提醒工具参数为空"));
        }
        let payload: ReminderFunctionPayload = serde_json::from_str(raw_args)?;
        let ReminderFunctionPayload { title, remind_at, reminder_type } = payload;
        if title.trim().is_empty() || remind_at.trim().is_empty() {
            return Err(anyhow!("提醒参数缺少 title 或 remindAt"));
        }
        let reminder_type = if reminder_type.trim().is_empty() { "set".to_string() } else { reminder_type };
        Ok(json!({
            "content": title,
            "startAt": remind_at,
            "reminderType": reminder_type
        })
        .to_string())
    }
}

/// Runtime-facing reminder agent that wraps the domain logic and integrates with shared handles.
#[async_trait]
impl Agent for ReminderAgent {
    fn id(&self) -> &str {
        "agent.reminder"
    }

    fn intents(&self) -> Vec<&str> {
        vec!["reminder", "agent.reminder.set"]
    }

    async fn run(&self, ctx: AgentContext, handles: AgentHandles<'_>) -> anyhow::Result<()> {
        let llm_client = handles.llm_client.upgrade().ok_or_else(|| anyhow!("llm client unavailable"))?;
        let user_now = ctx
            .user_now
            .clone()
            .unwrap_or_else(|| Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());

        // 🎯 中心化：从 TurnTracker 获取对话历史
        let conversation = super::turn_tracker::get_history_messages(&ctx.session_id).await;

        debug!(
            "Starting agent run with user_now: {}, conversation len: {}",
            user_now,
            conversation.len()
        );
        let mut messages = Vec::with_capacity(conversation.len() + 8);

        // 构建结构化用户时间/地点信息块（公共函数）
        let structured_block = crate::agents::runtime::build_user_structured_block_async(&ctx).await;
        let language = ctx.extra.asr_language.as_deref().unwrap_or("zh");
        let agent_id = self.id();
        let base_prompt = SystemPromptRegistry::global()
            .get(agent_id, language)
            .unwrap_or_else(|| panic!("System prompt not found for agent {} with language {}", agent_id, language));
        let system_prompt = crate::agents::runtime::build_agent_system_prompt_with_time_async(
            &ctx.session_id,
            ctx.role_prompt.as_deref(),
            &base_prompt,
            &structured_block,
            ctx.user_now.as_deref(),
            ctx.extra.user_timezone.as_deref(),
        )
        .await;
        messages.push(ChatMessage {
            role: Some("system".into()),
            content: Some(system_prompt),
            tool_call_id: None,
            tool_calls: None,
        });
        messages.extend(conversation);
        debug!("Initial conversation messages: {}", messages.len());

        // 检查是否有可用的日程/提醒工具（需要外部工具支持），没有则 reject（不保存到上下文，避免污染后续对话）
        if !Self::has_reminder_tools(&ctx.tools) {
            let reject_msg = match language.split('-').next().unwrap_or(language) {
                "zh" => "抱歉，日程提醒功能暂时不可用。",
                "ja" => "申し訳ございませんが、現在リマインダー機能はご利用いただけません。",
                "ko" => "죄송합니다. 현재 알림/일정 기능을 사용할 수 없습니다.",
                "es" => "Lo siento, la función de recordatorios no está disponible en este momento.",
                "it" => "Mi dispiace, la funzione promemoria non è disponibile al momento.",
                _ => "Sorry, reminder/schedule is not available at the moment.",
            };
            handles.tts_sink.send(reject_msg).await;
            // 功能不可用：只播报，不保存到上下文
            return Ok(());
        }

        // 合并专用工具 + 共享工具库
        let tools = Self::merge_tools(&ctx.tools);
        let allowed_tool_names = collect_tool_names(&tools);
        debug!("Available tools: {} (shared: {})", tools.len(), ctx.tools.len());
        let mut final_text: Option<String> = None;

        for loop_index in 0..MAX_AGENT_LOOPS {
            debug!("Starting loop {} (max loops: {})", loop_index, MAX_AGENT_LOOPS);
            if handles.cancel.is_cancelled() {
                debug!("Agent cancelled, stopping");
                super::turn_tracker::interrupt_turn(&ctx.session_id).await;
                return Ok(());
            }

            let params = Self::build_loop_params(loop_index + 1 == MAX_AGENT_LOOPS, &tools);
            debug!(
                "Loop {}: is_last_loop: {}, tools count: {}",
                loop_index,
                loop_index + 1 == MAX_AGENT_LOOPS,
                tools.len()
            );
            let request_body = json!({
                "messages": &messages,
            });
            debug!(
                "Loop {}: LLM request body: {}",
                loop_index,
                serde_json::to_string_pretty(&request_body).unwrap_or_default()
            );

            // 使用通用流式处理函数，自动支持 next-token 打断
            let stream = llm_client.chat_stream(messages.clone(), Some(params)).await?;
            let options = StreamOptions::new(handles.cancel)
                .with_tts(handles.tts_sink)
                .with_tag("ReminderAgent");
            let stream_result = process_stream_with_interrupt(Box::pin(stream), options).await;

            // 如果被打断，立即返回
            if stream_result.was_interrupted {
                debug!("ReminderAgent interrupted at loop {}", loop_index);
                super::turn_tracker::interrupt_turn(&ctx.session_id).await;
                return Ok(());
            }

            let has_streamed_text = !stream_result.text.is_empty();
            let message = build_assistant_message(&stream_result);
            let tool_calls = message.tool_calls.clone();
            let reply_content = message.content.clone();
            debug!(
                "Loop {}: received message with content: '{}', tool_calls: {}",
                loop_index,
                reply_content.as_deref().unwrap_or(""),
                tool_calls.as_ref().map(|tc| tc.len()).unwrap_or(0)
            );
            messages.push(message);

            // Check if we have actual tool calls to process
            if let Some(tool_calls) = tool_calls
                && !tool_calls.is_empty()
            {
                debug!("Loop {}: processing {} tool calls", loop_index, tool_calls.len());
                let should_stop = false;
                for (call_index, tool_call) in tool_calls.iter().enumerate() {
                    debug!(
                        "Loop {}: processing tool call {}: {}",
                        loop_index,
                        call_index,
                        tool_call.function.name.as_deref().unwrap_or("")
                    );
                    match self
                        .process_tool_call(
                            tool_call,
                            loop_index,
                            call_index,
                            &handles,
                            &mut messages,
                            &allowed_tool_names,
                            has_streamed_text,
                        )
                        .await
                    {
                        ToolControl::Continue => {
                            debug!("Loop {}: tool call {} returned Continue", loop_index, call_index);
                        },
                        ToolControl::Respond(_) => {
                            // TTS 模式：complete_tool_call 已添加 assistant 消息，直接返回
                            return Ok(());
                        },
                        ToolControl::Stop => {
                            debug!("Loop {}: tool call {} returned Stop", loop_index, call_index);
                            return Ok(());
                        },
                        ToolControl::Interrupted => {
                            debug!("Loop {}: tool call {} was interrupted", loop_index, call_index);
                            super::turn_tracker::interrupt_turn(&ctx.session_id).await;
                            return Ok(());
                        },
                    }
                }

                if should_stop {
                    debug!("Loop {}: should_stop is true, breaking", loop_index);
                    break;
                }
                continue;
            }

            let reply = reply_content.unwrap_or_default();
            debug!("Loop {}: no tool calls, processing reply: '{}'", loop_index, reply);
            if reply.trim().is_empty() {
                debug!("Loop {}: reply is empty, continuing", loop_index);
                continue;
            }

            if !has_streamed_text {
                debug!("Loop {}: sending reply to TTS: '{}'", loop_index, reply);
                handles.tts_sink.send(&reply).await;
            } else {
                debug!("Loop {}: reply already streamed to TTS; skipping full resend", loop_index);
            }
            final_text = Some(reply);
            debug!("Loop {}: set final_text and breaking", loop_index);
            break;
        }

        if let Some(text) = final_text {
            if !text.trim().is_empty() {
                debug!("Agent run completed: returning text '{}'", text);
                // 🎯 中心化：添加 assistant 消息到 TurnTracker
                super::turn_tracker::add_assistant_message(&ctx.session_id, &text).await;
            } else {
                debug!("Agent run completed: final_text is empty");
            }
        } else {
            debug!("Agent run completed: no final_text");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentCancelToken, AgentContext, AgentExtra, AgentHandles, AgentToolClient, AgentTtsSink};
    use crate::function_callback::CallResult;
    use crate::mcp::client::McpClientWrapper;
    use crate::rpc::{SharedFlags, TurnContext};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex, Weak};

    #[tokio::test]
    async fn process_tool_call_converts_reminder_payload_before_invocation() {
        // Initialize tracing for debug logs
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("realtime=debug".parse().unwrap()))
            .try_init();

        debug!("Starting test: process_tool_call_converts_reminder_payload_before_invocation");
        let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let recording_client = RecordingToolClient::new(
            calls.clone(),
            ToolCallOutcome {
                result: CallResult::Success(json!({"ok": true})),
                control: ToolControl::Continue,
                context_text: "done".into(),
            },
        );
        let tts = RecordingTtsSink::default();
        let cancel = TestCancelToken;
        let handles = test_handles(&recording_client, &tts, &cancel);

        let agent = ReminderAgent::new();
        let reminder_args = r#"{"title":"喝水","time":"2025-05-01 09:00:00","type":"set"}"#;
        let tool_call = ToolCall {
            id: Some("call-1".into()),
            call_type: Some("function".into()),
            index: Some(0),
            function: crate::llm::FunctionCall { name: Some("reminder".into()), arguments: Some(reminder_args.into()) },
        };
        let mut messages = Vec::new();

        let allowed_tools: std::collections::HashSet<String> = vec!["reminder".to_string()].into_iter().collect();
        let exec = agent
            .process_tool_call(&tool_call, 0, 0, &handles, &mut messages, &allowed_tools, false)
            .await;
        assert!(matches!(exec, ToolControl::Continue));

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (name, args) = &calls[0];
        assert_eq!(name, "reminder");
        let value: serde_json::Value = serde_json::from_str(args).unwrap();
        assert_eq!(value["content"], "喝水");
        assert_eq!(value["startAt"], "2025-05-01 09:00:00");
        assert_eq!(value["reminderType"], "set");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role.as_deref(), Some("tool"));
    }

    #[tokio::test]
    async fn process_tool_call_streams_tts_on_respond() {
        // Initialize tracing for debug logs
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("realtime=debug".parse().unwrap()))
            .try_init();

        debug!("Starting test: process_tool_call_streams_tts_on_respond");
        let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let recording_client = RecordingToolClient::new(
            calls,
            ToolCallOutcome {
                result: CallResult::Success(json!({})),
                control: ToolControl::Respond("播报内容".into()),
                context_text: "ignored".into(),
            },
        );
        let tts = RecordingTtsSink::default();
        let cancel = TestCancelToken;
        let handles = test_handles(&recording_client, &tts, &cancel);

        let agent = ReminderAgent::new();
        let tool_call = ToolCall {
            id: Some("call-respond".into()),
            call_type: Some("function".into()),
            index: Some(0),
            function: crate::llm::FunctionCall { name: Some("mock_respond_tool".into()), arguments: Some("{}".into()) },
        };
        let mut messages = Vec::new();

        let allowed_tools: std::collections::HashSet<String> = vec!["mock_respond_tool".to_string()].into_iter().collect();
        let exec = agent
            .process_tool_call(&tool_call, 0, 0, &handles, &mut messages, &allowed_tools, false)
            .await;
        match exec {
            ToolControl::Respond(text) => assert_eq!(text, "播报内容"),
            other => panic!("unexpected execution outcome: {:?}", other),
        }
        let spoken = tts.entries.lock().unwrap();
        assert_eq!(spoken.as_slice(), &["播报内容".to_string()]);
    }

    #[test]
    fn convert_reminder_args_success() {
        // Initialize tracing for debug logs
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("realtime=debug".parse().unwrap()))
            .try_init();

        debug!("Starting test: convert_reminder_args_success");
        let args = r#"{"title":"开会","time":"2025-01-01 09:00:00","type":"set"}"#;
        let converted = ReminderAgent::convert_reminder_args(args).expect("should convert");
        let value: serde_json::Value = serde_json::from_str(&converted).unwrap();
        assert_eq!(value["content"], "开会");
        assert_eq!(value["startAt"], "2025-01-01 09:00:00");
        assert_eq!(value["reminderType"], "set");
    }

    #[test]
    fn convert_reminder_args_rejects_missing_fields() {
        // Initialize tracing for debug logs
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("realtime=debug".parse().unwrap()))
            .try_init();

        debug!("Starting test: convert_reminder_args_rejects_missing_fields");
        let args = r#"{"title":"","remindAt":""}"#;
        assert!(ReminderAgent::convert_reminder_args(args).is_err());
    }

    #[test]
    fn ensure_tool_call_id_generates_fallback() {
        // Initialize tracing for debug logs
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("realtime=debug".parse().unwrap()))
            .try_init();

        debug!("Starting test: ensure_tool_call_id_generates_fallback");
        let tool_call = ToolCall {
            id: None,
            call_type: Some("function".into()),
            index: Some(0),
            function: crate::llm::FunctionCall { name: Some("reminder".into()), arguments: None },
        };
        let generated = ReminderAgent::ensure_tool_call_id(&tool_call, 1, 2);
        assert!(generated.contains("reminder_tool_call_1_2"));
    }

    #[tokio::test]
    async fn reminder_agent_cases_and_logging_loop_mode() -> Result<(), Box<dyn std::error::Error>> {
        // Initialize tracing for debug logs
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("realtime=debug".parse().unwrap()))
            .try_init();

        debug!("Starting test: reminder_agent_cases_and_logging_loop_mode");
        let cfg = crate::llm::LlmConfig::default();
        let enabled = std::env::var("REMINDER_AGENT_E2E")
            .ok()
            .map(|v| matches!(v.as_str(), "1" | "true" | "on"))
            .unwrap_or(true)
            || cfg.base_url.contains("localhost");
        if !enabled {
            eprintln!("[reminder_agent_cases_and_logging_loop_mode] skipped: set REMINDER_AGENT_E2E=1 or point LLM_BASE_URL to localhost");
            return Ok(());
        }

        let llm = Arc::new(crate::llm::LlmClient::from_config(cfg));
        let agent = ReminderAgent::new();
        let user_now = "2025-10-29 10:00:00";
        // "设提醒",
        // "记得叫我去",
        // "下周四下午三点提醒我开会",
        // "下个月1号上午9点提醒我体检",
        // "半年后提醒我复查",
        // "明年1月3日下午2点提醒我出差",
        // "下个礼拜日提醒我去超市",
        // "2分钟后提醒我喝水",
        let cases = [
            "100分钟后提醒我出门",
            "200分钟后提醒我出门",
            "300分钟后提醒我出门",
            "400分钟后提醒我出门",
        ];

        for (idx, text) in cases.iter().enumerate() {
            debug!("Processing test case {}: '{}'", idx, text);
            let shared_flags = Arc::new(SharedFlags::new());
            let ctx = AgentContext {
                session_id: format!("session_{}", idx),
                user_text: (*text).to_string(),
                user_now: Some(user_now.to_string()),
                turn_context: TurnContext::new(
                    format!("user_item_{}", idx),
                    format!("assistant_item_{}", idx),
                    format!("response_{}", idx),
                    Some(idx as u64),
                ),
                shared_flags: shared_flags.clone(),
                system_prompt: None,
                role_prompt: None,
                tools: Vec::new(),
                offline_tools: Vec::new(),
                extra: AgentExtra::default(),
                wiki_context: None,
            };

            let tool_client = RecordingToolClient::new(
                Arc::new(Mutex::new(Vec::new())),
                ToolCallOutcome {
                    result: CallResult::Success(json!({"text": "ok"})),
                    control: ToolControl::Respond("好的，已经记录。".into()),
                    context_text: "工具完成".into(),
                },
            );
            let tts_sink = RecordingTtsSink::default();
            let handles = AgentHandles {
                llm_client: Arc::downgrade(&llm),
                shared_flags: shared_flags.clone(),
                enable_search: Arc::new(AtomicBool::new(true)),
                mcp_clients: Arc::new(Vec::<McpClientWrapper>::new()),
                tts_sink: &tts_sink,
                tool_client: &tool_client,
                cancel: &TestCancelToken,
                interrupt_handler: None,
                emitter: std::sync::Weak::new(),
                turn_context: TurnContext::new(
                    format!("user_item_{}", idx),
                    format!("assistant_item_{}", idx),
                    format!("response_{}", idx),
                    Some(idx as u64),
                ),
            };

            debug!("Test case {}: about to call agent.run()", idx);
            agent.run(ctx, handles).await?;
            let tool_calls = tool_client.calls();
            debug!("Test case {} completed. Tool calls: {}", idx, tool_calls.len());
            if tool_calls.is_empty() {
                let spoken = tts_sink.entries();
                debug!("Test case {}: no tool calls, checking TTS entries: {:?}", idx, spoken);
                let has_non_empty_spoken = spoken.iter().any(|s| !s.trim().is_empty());
                assert!(
                    has_non_empty_spoken,
                    "input `{}` should produce a follow-up question. TTS entries: {:?}",
                    text, spoken,
                );
            } else {
                for (name, args_json) in tool_calls {
                    debug!("Processing tool call: name={}, args={}", name, args_json);
                    if name == "reminder" {
                        let value: serde_json::Value = serde_json::from_str(&args_json)?;
                        let start_at = value
                            .get("startAt")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| anyhow::anyhow!("startAt missing"))?;
                        debug!("Validating reminder time: {}", start_at);
                        chrono::NaiveDateTime::parse_from_str(start_at, "%Y-%m-%d %H:%M:%S")?;
                    }
                }
            }
        }

        Ok(())
    }

    fn test_handles<'a>(tool_client: &'a dyn AgentToolClient, tts_sink: &'a dyn AgentTtsSink, cancel: &'a dyn AgentCancelToken) -> AgentHandles<'a> {
        AgentHandles {
            llm_client: Weak::new(),
            shared_flags: Arc::new(SharedFlags::new()),
            enable_search: Arc::new(AtomicBool::new(true)),
            mcp_clients: Arc::new(Vec::<McpClientWrapper>::new()),
            tts_sink,
            tool_client,
            cancel,
            interrupt_handler: None,
            emitter: std::sync::Weak::new(),
            turn_context: TurnContext::new(
                "test_user_item".to_string(),
                "test_assistant_item".to_string(),
                "test_response".to_string(),
                Some(0),
            ),
        }
    }

    #[derive(Clone)]
    struct RecordingToolClient {
        calls: Arc<Mutex<Vec<(String, String)>>>,
        outcome: ToolCallOutcome,
    }

    impl RecordingToolClient {
        fn new(calls: Arc<Mutex<Vec<(String, String)>>>, outcome: ToolCallOutcome) -> Self {
            Self { calls, outcome }
        }

        fn calls(&self) -> Vec<(String, String)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl AgentToolClient for RecordingToolClient {
        async fn call(&self, name: &str, args_json: &str) -> anyhow::Result<ToolCallOutcome> {
            self.calls.lock().unwrap().push((name.to_string(), args_json.to_string()));
            // 内置工具使用真实实现
            if crate::function_callback::is_builtin_tool(name) {
                let params: rustc_hash::FxHashMap<String, serde_json::Value> = serde_json::from_str(args_json).unwrap_or_default();
                let (text, result) = match crate::function_callback::handle_builtin_tool(name, &params).await {
                    Ok(crate::function_callback::CallResult::Success(value)) => {
                        let text = serde_json::to_string(&value).unwrap_or_default();
                        (text, crate::function_callback::CallResult::Success(value))
                    },
                    Ok(crate::function_callback::CallResult::Error(err)) => (format!("工具错误: {}", err), crate::function_callback::CallResult::Error(err)),
                    Ok(crate::function_callback::CallResult::Async(task_id)) => (
                        format!("异步任务: {}", task_id),
                        crate::function_callback::CallResult::Async(task_id),
                    ),
                    Err(e) => (
                        format!("调用失败: {}", e),
                        crate::function_callback::CallResult::Error(e.to_string()),
                    ),
                };
                return Ok(ToolCallOutcome { result, control: ToolControl::Continue, context_text: text });
            }
            // 非内置工具使用 mock
            Ok(self.outcome.clone())
        }
    }

    #[derive(Default)]
    struct RecordingTtsSink {
        entries: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl AgentTtsSink for RecordingTtsSink {
        async fn send(&self, text: &str) {
            self.entries.lock().unwrap().push(text.to_string());
        }
    }

    impl RecordingTtsSink {
        fn entries(&self) -> Vec<String> {
            self.entries.lock().unwrap().clone()
        }
    }

    struct TestCancelToken;

    #[async_trait::async_trait]
    impl AgentCancelToken for TestCancelToken {
        fn is_cancelled(&self) -> bool {
            false
        }

        async fn cancelled(&self) {
            // 测试中永远不会被取消，所以永远等待
            std::future::pending::<()>().await
        }
    }
}
