use anyhow::anyhow;
use async_trait::async_trait;
use tracing::{debug, info};

use super::stream_utils::{StreamOptions, build_assistant_message, process_stream_with_interrupt};
use super::system_prompt_registry::SystemPromptRegistry;
use super::tool_utils::{UNSUPPORTED_TOOL_TEXT, append_tool_by_name, collect_tool_names};
use super::{Agent, AgentContext, AgentHandles, ToolControl};
use crate::llm::{ChatCompletionParams, ChatMessage, Tool, ToolCall, ToolChoice};
use std::collections::HashSet;

const MAX_AGENT_LOOPS: usize = 3;

/// 需要强制调用 search_web 的 intent 列表
/// 这些 intent 表示用户需要实时信息，应该先搜索再回答
/// 注意：
/// - 天气类 intent 不在此列表中，因为有专门的 get_weather 工具
/// - 时间类 intent (agent.datetime.query, agent.information.time) 已移至 FallbackAgent
///   因为时间信息已在 system prompt 的时间前缀中，不需要搜索
const FORCE_SEARCH_INTENTS: &[&str] = &[
    "agent.finance.stock",
    "agent.search.query",
    "agent.qa.domain",
    "agent.information.currency",
    "agent.information.date",
    "agent.information.legal",
    "agent.information.movie",
    "agent.information.news",
];

pub struct SearchAgent;

impl Default for SearchAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchAgent {
    pub fn new() -> Self {
        Self
    }

    /// 检查是否有天气工具可用（仅检查 tools）
    fn has_weather_tool(tools: &[Tool]) -> bool {
        tools.iter().any(|t| {
            let name = t.function.name.to_lowercase();
            name.contains("weather") || name.contains("get_weather")
        })
    }

    /// 检查是否有 search_web 工具可用
    fn has_search_web_tool(tools: &[Tool]) -> bool {
        tools.iter().any(|t| t.function.name == "search_web")
    }

    /// 判断当前 intent 是否需要强制调用 search_web
    fn should_force_search(intent: Option<&str>) -> bool {
        match intent {
            Some(intent_label) => FORCE_SEARCH_INTENTS.contains(&intent_label),
            None => false,
        }
    }

    fn build_loop_params(is_last_loop: bool, tools: &[Tool], force_search: bool) -> ChatCompletionParams {
        let mut params = ChatCompletionParams::default();
        if !tools.is_empty() {
            params.tools = Some(tools.to_vec());
            params.tool_choice = Some(if is_last_loop {
                ToolChoice::none()
            } else if force_search {
                // 强制调用 search_web 工具
                ToolChoice::function("search_web")
            } else {
                ToolChoice::auto()
            });
        }
        params.stream = Some(true);
        params
    }

    /// 过滤允许的工具（白名单模式）
    ///
    /// # 工具过滤规则
    /// - 关键字过滤: 包含 "query", "search", 或 "weather" 关键字的工具（不区分大小写）
    ///
    /// # 内置工具注入
    /// - `world_clock`: 时间查询工具（用于时间相关搜索）
    /// - `math`: 数学计算工具（用于计算相关搜索）
    /// - `search_web`: 搜索工具（仅在 `enable_search` 为 true 时存在）
    ///
    /// # 工具合并流程
    /// 1. 从共享工具中筛选包含指定关键字的工具
    /// 2. 追加内置工具 world_clock, math, search_web（如果存在）
    fn filter_allowed_tools(&self, all_tools: &[Tool]) -> Vec<Tool> {
        use std::collections::HashSet;
        let mut tools = Vec::new();
        let mut names: HashSet<String> = HashSet::new();

        // 1. 包含关键词的工具
        for tool in all_tools {
            let name_lower = tool.function.name.to_ascii_lowercase();
            if name_lower.contains("query") || name_lower.contains("search") || name_lower.contains("weather") {
                if names.insert(tool.function.name.clone()) {
                    tools.push(tool.clone());
                }
            }
        }

        // 2. 注入内置工具: world_clock, math, search_web
        append_tool_by_name(&mut tools, &mut names, all_tools, "world_clock");
        append_tool_by_name(&mut tools, &mut names, all_tools, "math");
        append_tool_by_name(&mut tools, &mut names, all_tools, "search_web");

        tools
    }

    fn ensure_tool_call_id(tool_call: &ToolCall, loop_index: usize, call_index: usize) -> String {
        tool_call
            .id
            .clone()
            .unwrap_or_else(|| format!("search_tool_call_{}_{}", loop_index, call_index))
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
        let call_id = Self::ensure_tool_call_id(tool_call, loop_index, call_index);

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
        debug!("SearchAgent processing tool call: name={}, args={}", tool_name, raw_args);

        // 调用工具
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
        // 其他控制信号直接透传，Stop 不再丢弃，确保不会继续发音或持久化
        outcome.control
    }
}

#[async_trait]
impl Agent for SearchAgent {
    fn id(&self) -> &str {
        "agent.search.query"
    }

    fn intents(&self) -> Vec<&str> {
        vec!["agent.finance.stock", "agent.search.query", "agent.information.weather"]
    }

    async fn run(&self, ctx: AgentContext, handles: AgentHandles<'_>) -> anyhow::Result<()> {
        let llm_client = handles.llm_client.upgrade().ok_or_else(|| anyhow!("llm client unavailable"))?;

        let language = ctx.extra.asr_language.as_deref().unwrap_or("zh");

        // 如果是天气查询意图，检查是否有天气工具
        if ctx.extra.intent_label.as_deref() == Some("agent.information.weather") && !Self::has_weather_tool(&ctx.tools) {
            let reject_msg = match language.split('-').next().unwrap_or(language) {
                "zh" => "抱歉，天气查询功能暂时不可用。",
                "ja" => "申し訳ございませんが、現在天気予報機能はご利用いただけません。",
                "ko" => "죄송합니다. 현재 날씨 조회 기능을 사용할 수 없습니다.",
                "es" => "Lo siento, la consulta del clima no está disponible en este momento.",
                "it" => "Mi dispiace, la consultazione meteo non è disponibile al momento.",
                _ => "Sorry, weather query is not available at the moment.",
            };
            handles.tts_sink.send(reject_msg).await;
            return Ok(());
        }

        let structured_block = crate::agents::runtime::build_user_structured_block_async(&ctx).await;
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

        // 白名单过滤：只允许包含query/weather/search的工具和agent专用工具
        let filtered_tools = self.filter_allowed_tools(&ctx.tools);
        debug!(
            "SearchAgent tools: {} available, {} after filtering",
            ctx.tools.len(),
            filtered_tools.len()
        );

        // 🔍 强制搜索逻辑：对于特定 intent，使用 tool_choice 强制 LLM 调用 search_web
        let force_search = Self::should_force_search(ctx.extra.intent_label.as_deref()) && Self::has_search_web_tool(&filtered_tools);

        if force_search {
            info!(
                "🔍 SearchAgent: 强制搜索模式，intent={:?}, 将使用 tool_choice 强制 LLM 调用 search_web",
                ctx.extra.intent_label
            );
        }

        let tools_for_llm = filtered_tools.clone();
        let allowed_tool_names = collect_tool_names(&tools_for_llm);

        let mut final_text: Option<String> = None;

        // 跟踪是否已经执行过搜索，避免重复强制搜索
        let mut has_searched = false;

        for loop_index in 0..MAX_AGENT_LOOPS {
            debug!("SearchAgent loop {} starting", loop_index);
            if handles.cancel.is_cancelled() {
                super::turn_tracker::interrupt_turn(&ctx.session_id).await;
                return Ok(());
            }

            // 只在第一轮且未搜索过时强制搜索
            let should_force_this_loop = force_search && loop_index == 0 && !has_searched;
            let params = Self::build_loop_params(loop_index + 1 == MAX_AGENT_LOOPS, &tools_for_llm, should_force_this_loop);

            // 使用通用流式处理函数
            let stream = llm_client.chat_stream(messages.clone(), Some(params)).await?;
            let options = StreamOptions::new(handles.cancel)
                .with_tts(handles.tts_sink)
                .with_tag("SearchAgent");
            let stream_result = process_stream_with_interrupt(Box::pin(stream), options).await;

            if stream_result.was_interrupted {
                debug!("SearchAgent interrupted at loop {}", loop_index);
                super::turn_tracker::interrupt_turn(&ctx.session_id).await;
                return Ok(());
            }

            let has_streamed_text = !stream_result.text.is_empty();

            // 构建 assistant 消息（build_assistant_message 会自动确保 tool_call.id 存在）
            let message = build_assistant_message(&stream_result);
            // 获取带 id 的 tool_calls（与 assistant 消息中的一致）
            let tool_calls_with_ids = message.tool_calls.clone();
            messages.push(message);

            // 处理工具调用
            if let Some(ref tool_calls) = tool_calls_with_ids {
                if !tool_calls.is_empty() {
                    debug!("SearchAgent processing {} tool calls", tool_calls.len());

                    for (call_index, tool_call) in tool_calls.iter().enumerate() {
                        // 标记是否调用了 search_web
                        if tool_call.function.name.as_deref() == Some("search_web") {
                            has_searched = true;
                        }

                        match self
                            .process_tool_call(tool_call, loop_index, call_index, &handles, &mut messages, &allowed_tool_names)
                            .await
                        {
                            ToolControl::Respond(_) => {
                                // TTS 模式：complete_tool_call 已添加 assistant 消息，直接返回
                                return Ok(());
                            },
                            ToolControl::Interrupted => {
                                super::turn_tracker::interrupt_turn(&ctx.session_id).await;
                                return Ok(());
                            },
                            ToolControl::Stop => return Ok(()),
                            ToolControl::Continue => {},
                        }
                    }
                    // 工具已处理但未生成最终文本，继续下一轮尝试
                    continue;
                }
            }

            // 无工具调用，使用文本回复
            if !stream_result.text.trim().is_empty() {
                if !has_streamed_text {
                    handles.tts_sink.send(&stream_result.text).await;
                }
                final_text = Some(stream_result.text);
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
