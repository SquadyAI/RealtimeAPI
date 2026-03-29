use anyhow::anyhow;
use async_trait::async_trait;

use super::stream_utils::{StreamOptions, build_assistant_message, ensure_tool_call_id, process_stream_with_interrupt};
use super::system_prompt_registry::SystemPromptRegistry;
use super::tool_utils::{UNSUPPORTED_TOOL_TEXT, append_tool_by_name, collect_tool_names, select_tools_by_keywords};
use super::{Agent, AgentContext, AgentHandles, ToolControl};
use crate::llm::{ChatCompletionParams, ChatMessage, Tool, ToolCall, ToolChoice};
use std::collections::HashSet;

const MAX_AGENT_LOOPS: usize = 3;

pub struct DeviceControlAgent;

impl Default for DeviceControlAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceControlAgent {
    pub fn new() -> Self {
        Self
    }

    /// 合并过滤后的共享工具库
    ///
    /// # 工具过滤规则
    /// - 关键字过滤: 包含 "device", "control", 或 "volume" 关键字的工具（不区分大小写）
    ///
    /// # 内置工具注入
    /// - `math`: 数学计算工具（用于设备参数计算等）
    ///
    /// # 工具合并流程
    /// 1. 从共享工具中筛选包含指定关键字的工具
    /// 2. 追加内置工具 math（如果存在）
    fn merge_tools(shared_tools: &[Tool]) -> Vec<Tool> {
        let (mut tools, mut names) = select_tools_by_keywords(shared_tools, &["device", "control", "volume"]);
        append_tool_by_name(&mut tools, &mut names, shared_tools, "math");
        tools
    }

    /// 检查是否有可用的设备控制工具
    fn has_device_control_tools(tools: &[Tool]) -> bool {
        tools.iter().any(|t| {
            let name = t.function.name.to_lowercase();
            name.contains("device") || name.contains("control") || name.contains("volume")
        })
    }

    fn build_loop_params(is_last_loop: bool, tools: &[Tool]) -> ChatCompletionParams {
        let mut params = ChatCompletionParams::default();
        if !tools.is_empty() {
            params.tools = Some(tools.to_vec());
            params.tool_choice = Some(if is_last_loop { ToolChoice::none() } else { ToolChoice::auto() });
        }
        params.stream = Some(true);
        params
    }

    async fn process_tool_call(
        &self,
        tool_call: &ToolCall,
        loop_index: usize,
        call_index: usize,
        handles: &AgentHandles<'_>,
        messages: &mut Vec<ChatMessage>,
        allowed_tools: &HashSet<String>,
    ) -> ToolControl {
        let tool_name = tool_call.function.name.as_deref().unwrap_or("").to_string();
        let call_id = ensure_tool_call_id(tool_call, "device_control", loop_index, call_index);

        if tool_name.trim().is_empty() {
            messages.push(ChatMessage {
                role: Some("tool".into()),
                content: Some("工具调用缺少名称".to_string()),
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
        let outcome = match handles.tool_client.call(&tool_name, &raw_args).await {
            Ok(outcome) => outcome,
            Err(e) => {
                let error_text = format!("工具 {} 调用失败: {}", tool_name, e);
                messages.push(ChatMessage {
                    role: Some("tool".into()),
                    content: Some(error_text),
                    tool_call_id: Some(call_id),
                    tool_calls: None,
                });
                return ToolControl::Continue;
            },
        };

        let tool_text = if outcome.context_text.trim().is_empty() {
            format!("工具 {} 执行完成", tool_name)
        } else {
            outcome.context_text.clone()
        };

        messages.push(ChatMessage {
            role: Some("tool".into()),
            content: Some(tool_text.clone()),
            tool_call_id: Some(call_id),
            tool_calls: None,
        });

        // 处理 Respond 情况：发送 TTS
        if let ToolControl::Respond(ref tts_text) = outcome.control {
            let speak_text = if tts_text.trim().is_empty() { tool_text } else { tts_text.clone() };
            if !speak_text.trim().is_empty() {
                handles.tts_sink.send(&speak_text).await;
                return ToolControl::Respond(speak_text);
            }
            return ToolControl::Continue;
        }
        outcome.control
    }
}

#[async_trait]
impl Agent for DeviceControlAgent {
    fn id(&self) -> &str {
        "agent.device.control"
    }

    fn intents(&self) -> Vec<&str> {
        vec!["device_control", "agent.device.battery"]
    }

    async fn run(&self, ctx: AgentContext, handles: AgentHandles<'_>) -> anyhow::Result<()> {
        let llm_client = handles.llm_client.upgrade().ok_or_else(|| anyhow!("llm client unavailable"))?;

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

        // 🎯 中心化：从 TurnTracker 获取对话历史
        let conversation = super::turn_tracker::get_history_messages(&ctx.session_id).await;

        let mut messages = vec![ChatMessage {
            role: Some("system".into()),
            content: Some(system_prompt),
            tool_call_id: None,
            tool_calls: None,
        }];
        messages.extend(conversation);

        let tools = Self::merge_tools(&ctx.tools);

        // 检查是否有可用的设备控制工具，没有则 reject（不保存到上下文，避免污染后续对话）
        if !Self::has_device_control_tools(&tools) {
            let reject_msg = match language.split('-').next().unwrap_or(language) {
                "zh" => "抱歉，设备控制功能暂时不可用。",
                "ja" => "申し訳ございませんが、現在デバイス制御機能はご利用いただけません。",
                "ko" => "죄송합니다. 현재 기기 제어 기능을 사용할 수 없습니다.",
                "es" => "Lo siento, el control del dispositivo no está disponible en este momento.",
                "it" => "Mi dispiace, il controllo del dispositivo non è disponibile al momento.",
                _ => "Sorry, device control is not available at the moment.",
            };
            handles.tts_sink.send(reject_msg).await;
            // 功能不可用：只播报，不保存到上下文
            return Ok(());
        }

        let allowed_tool_names = collect_tool_names(&tools);
        let mut final_text: Option<String> = None;

        for loop_index in 0..MAX_AGENT_LOOPS {
            if handles.cancel.is_cancelled() {
                super::turn_tracker::interrupt_turn(&ctx.session_id).await;
                return Ok(());
            }

            let params = Self::build_loop_params(loop_index + 1 == MAX_AGENT_LOOPS, &tools);
            let stream = llm_client.chat_stream(messages.clone(), Some(params)).await?;
            let options = StreamOptions::new(handles.cancel)
                .with_tts(handles.tts_sink)
                .with_tag("DeviceControlAgent");

            let result = process_stream_with_interrupt(Box::pin(stream), options).await;

            if result.was_interrupted {
                super::turn_tracker::interrupt_turn(&ctx.session_id).await;
                return Ok(());
            }

            let has_streamed_text = !result.text.is_empty();
            let response_text = result.text.clone();
            let assistant_message = build_assistant_message(&result);
            messages.push(assistant_message);

            if result.has_tool_calls && !result.tool_calls.is_empty() {
                let tool_calls = result.tool_calls.clone();
                let should_stop = false;
                for (call_index, tool_call) in tool_calls.iter().enumerate() {
                    match self
                        .process_tool_call(tool_call, loop_index, call_index, &handles, &mut messages, &allowed_tool_names)
                        .await
                    {
                        ToolControl::Continue => {},
                        ToolControl::Stop => return Ok(()),
                        ToolControl::Interrupted => {
                            super::turn_tracker::interrupt_turn(&ctx.session_id).await;
                            return Ok(());
                        },
                        ToolControl::Respond(_) => {
                            // TTS 模式：complete_tool_call 已添加 assistant 消息，直接返回
                            return Ok(());
                        },
                    }
                }
                if should_stop {
                    break;
                }
                continue;
            }

            if !result.text.trim().is_empty() {
                if !has_streamed_text {
                    handles.tts_sink.send(&response_text).await;
                }
                final_text = Some(response_text);
                break;
            }
        }

        if let Some(text) = final_text {
            if !text.trim().is_empty() {
                // 🎯 中心化：添加 assistant 消息到 TurnTracker
                super::turn_tracker::add_assistant_message(&ctx.session_id, &text).await;
            }
        }

        Ok(())
    }
}
