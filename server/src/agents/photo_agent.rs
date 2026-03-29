use anyhow::anyhow;
use async_trait::async_trait;
use tracing::debug;

use super::stream_utils::{StreamOptions, process_stream_with_interrupt};
use super::system_prompt_registry::SystemPromptRegistry;
use super::tool_utils::{UNSUPPORTED_TOOL_TEXT, collect_tool_names};
use super::{Agent, AgentContext, AgentHandles, ToolControl};
use crate::llm::{ChatCompletionParams, ChatMessage, Tool, ToolCall, ToolChoice};
use std::collections::HashSet;

pub struct PhotoAgent;

impl Default for PhotoAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl PhotoAgent {
    pub fn new() -> Self {
        Self
    }

    /// 合并专用工具 + 过滤后的共享工具库（专用工具优先）
    ///
    /// # 工具过滤规则
    /// - 关键字过滤: 包含 "visual" 或 "photo" 关键字的工具（不区分大小写）
    ///
    /// # 内置工具注入
    /// - `search_web`: 搜索工具（仅在 `enable_search` 为 true 时存在，用于搜索图片相关信息）
    ///
    /// # 工具合并流程
    /// 1. 添加专用工具 "photo_capture" 和 "photo_recognize"
    /// 2. 从共享工具中筛选包含 "visual" 或 "photo" 关键字的工具
    /// 3. 追加内置工具 search_web（如果存在）
    fn merge_tools(shared_tools: &[Tool]) -> Vec<Tool> {
        use std::collections::HashSet;
        let mut tools = Vec::new();
        let mut tool_names: HashSet<String> = HashSet::new();

        // // 1. 专用工具优先
        // let photo_tools = vec![
        //     Tool {
        //         tool_type: "function".to_string(),
        //         function: ToolFunction {
        //             name: "photo_capture".to_string(),
        //             description: "仅拍照保存，不进行识别".to_string(),
        //             parameters: json!({
        //                 "type": "object",
        //                 "properties": {
        //                     "description": {
        //                         "type": "string",
        //                         "description": "照片的简短描述（可选）"
        //                     }
        //                 },
        //                 "required": [],
        //                 "additionalProperties": false
        //             }),
        //         },
        //     },
        //     Tool {
        //         tool_type: "function".to_string(),
        //         function: ToolFunction {
        //             name: "photo_recognize".to_string(),
        //             description: "拍照并识别图片内容".to_string(),
        //             parameters: json!({
        //                 "type": "object",
        //                 "properties": {
        //                     "query": {
        //                         "type": "string",
        //                         "description": "用户想要识别的内容或问题"
        //                     }
        //                 },
        //                 "required": [],
        //                 "additionalProperties": false
        //             }),
        //         },
        //     },
        // ];

        // for t in photo_tools {
        //     tool_names.insert(t.function.name.clone());
        //     tools.push(t);
        // }

        // 2. 合并过滤后的共享工具库（去重）
        // 只保留包含 visual/photo 关键字的工具
        for t in shared_tools {
            if !tool_names.contains(&t.function.name) {
                let name_lower = t.function.name.to_ascii_lowercase();
                if name_lower.contains("visual") || name_lower.contains("photo") {
                    tool_names.insert(t.function.name.clone());
                    tools.push(t.clone());
                }
            }
        }

        // 3. 注入内置工具: search_web
        // append_tool_by_name(&mut tools, &mut tool_names, shared_tools, "search_web");

        tools
    }

    /// 检查是否有可用的拍照/视觉工具（需要外部工具支持）
    fn has_photo_tools(tools: &[Tool], offline_tools: &[String]) -> bool {
        // 检查 tools 中是否有 visual/photo/camera 关键字
        let has_in_tools = tools.iter().any(|t| {
            let name = t.function.name.to_lowercase();
            name.contains("visual") || name.contains("photo") || name.contains("camera")
        });

        // 检查 offline_tools 中是否有 take_photo/take_picture
        let has_in_offline = offline_tools.iter().any(|name| {
            let name_lower = name.to_lowercase();
            name_lower.contains("take_photo") || name_lower.contains("take_picture")
        });

        has_in_tools || has_in_offline
    }

    fn build_params(tools: &[Tool]) -> ChatCompletionParams {
        ChatCompletionParams {
            tools: Some(tools.to_vec()),
            tool_choice: Some(ToolChoice::auto()),
            stream: Some(true),
            response_format: None,
            ..Default::default()
        }
    }

    fn ensure_tool_call_id(tool_call: &ToolCall, call_index: usize) -> String {
        tool_call
            .id
            .clone()
            .unwrap_or_else(|| format!("photo_tool_call_{}", call_index))
    }

    /// 处理工具调用，遵循 ToolControl 语义（stop 直接终止，不触发 TTS）
    async fn process_tool_call(&self, tool_call: &ToolCall, call_index: usize, handles: &AgentHandles<'_>, allowed_tools: &HashSet<String>) -> ToolControl {
        let tool_name = tool_call.function.name.as_deref().unwrap_or("").to_string();
        let call_id = Self::ensure_tool_call_id(tool_call, call_index);

        if tool_name.trim().is_empty() {
            debug!("PhotoAgent: tool call missing name, call_id={}", call_id);
            return ToolControl::Continue;
        }

        if !allowed_tools.contains(&tool_name) {
            debug!("PhotoAgent: unsupported tool {}", tool_name);
            return ToolControl::Respond(UNSUPPORTED_TOOL_TEXT.to_string());
        }

        let raw_args = tool_call.function.arguments.clone().unwrap_or_else(|| "{}".to_string());
        debug!("PhotoAgent: calling tool {} with args {}", tool_name, raw_args);

        // 调用实际的拍照/识别工具，尊重工具控制信号
        match handles.tool_client.call(&tool_name, &raw_args).await {
            Ok(outcome) => match outcome.control {
                ToolControl::Stop => ToolControl::Stop,
                ToolControl::Interrupted => ToolControl::Interrupted,
                ToolControl::Respond(tts_text) => {
                    let speak = if tts_text.trim().is_empty() { outcome.context_text } else { tts_text };
                    if !speak.trim().is_empty() {
                        handles.tts_sink.send(&speak).await;
                    }
                    ToolControl::Respond(speak)
                },
                ToolControl::Continue => {
                    let speak = outcome.context_text;
                    if speak.trim().is_empty() {
                        ToolControl::Continue
                    } else {
                        handles.tts_sink.send(&speak).await;
                        ToolControl::Respond(speak)
                    }
                },
            },
            Err(e) => ToolControl::Respond(format!("工具调用失败: {}", e)),
        }
    }
}

#[async_trait]
impl Agent for PhotoAgent {
    fn id(&self) -> &str {
        "agent.qa.visual"
    }

    fn intents(&self) -> Vec<&str> {
        vec!["agent.qa.visual"]
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

        // 检查是否有可用的拍照工具（需要外部工具支持），没有则 reject（不保存到上下文，避免污染后续对话）
        if !Self::has_photo_tools(&ctx.tools, &ctx.offline_tools) {
            let reject_msg = match language.split('-').next().unwrap_or(language) {
                "zh" => "抱歉，拍照功能暂时不可用。",
                "ja" => "申し訳ございませんが、現在撮影機能はご利用いただけません。",
                "ko" => "죄송합니다. 현재 사진 촬영 기능을 사용할 수 없습니다.",
                "es" => "Lo siento, la captura de fotos no está disponible en este momento.",
                "it" => "Mi dispiace, la funzione fotografica non è disponibile al momento.",
                _ => "Sorry, photo capture is not available at the moment.",
            };
            handles.tts_sink.send(reject_msg).await;
            // 功能不可用：只播报，不保存到上下文
            return Ok(());
        }

        // 合并专用工具 + 共享工具库
        let tools = Self::merge_tools(&ctx.tools);
        let allowed_tool_names = collect_tool_names(&tools);
        debug!("PhotoAgent tools: {} (shared: {})", tools.len(), ctx.tools.len());

        // LLM 只调用一次
        let params = Self::build_params(&tools);
        let stream = llm_client.chat_stream(messages.clone(), Some(params)).await?;
        let options = StreamOptions::new(handles.cancel)
            .with_tts(handles.tts_sink)
            .with_tag("PhotoAgent");
        let stream_result = process_stream_with_interrupt(Box::pin(stream), options).await;

        // 如果被打断，立即返回
        if stream_result.was_interrupted {
            debug!("PhotoAgent interrupted");
            super::turn_tracker::interrupt_turn(&ctx.session_id).await;
            return Ok(());
        }

        // 处理工具调用，遵循 ToolControl
        if stream_result.has_tool_calls && !stream_result.tool_calls.is_empty() {
            for (call_index, tool_call) in stream_result.tool_calls.iter().enumerate() {
                match self
                    .process_tool_call(tool_call, call_index, &handles, &allowed_tool_names)
                    .await
                {
                    ToolControl::Respond(_) => {
                        // TTS 模式下，complete_tool_call 已添加 assistant 消息
                        return Ok(());
                    },
                    ToolControl::Stop => {
                        return Ok(());
                    },
                    ToolControl::Interrupted => {
                        super::turn_tracker::interrupt_turn(&ctx.session_id).await;
                        return Ok(());
                    },
                    ToolControl::Continue => {
                        // 继续处理后续工具调用（如有）
                    },
                }
            }
            return Ok(());
        }

        // 无工具调用，返回文本回复
        if !stream_result.text.trim().is_empty() {
            // 🎯 中心化：添加 assistant 消息到 TurnTracker
            super::turn_tracker::add_assistant_message(&ctx.session_id, &stream_result.text).await;
        }

        Ok(())
    }
}
