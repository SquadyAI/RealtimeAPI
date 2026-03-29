//! MediaAgent 核心实现
//!
//! 统一的媒体播放 Agent，整合音乐和喜马拉雅等媒体源。
//! 使用 LLM 工具调用来判断用户意图。

use anyhow::anyhow;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use super::providers::{MediaProvider, MusicProvider};
use super::types::MediaItem;
use crate::agents::stream_utils::merge_tool_call_delta;
use crate::agents::{Agent, AgentContext, AgentHandles, ToolControl};
use crate::llm::{ChatCompletionParams, ChatMessage, Tool, ToolCall, ToolChoice, ToolFunction};
use crate::rpc::SharedFlags;

/// 最大 LLM 循环次数
const MAX_LLM_LOOPS: usize = 3;

pub struct MediaAgent {
    providers: Vec<Arc<dyn MediaProvider>>,
}

impl Default for MediaAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaAgent {
    pub fn new() -> Self {
        Self { providers: vec![Arc::new(MusicProvider::new())] }
    }

    // ==================== 工具定义 ====================

    /// 搜索媒体工具
    fn search_media_tool() -> Tool {
        Tool {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "search_media".to_string(),
                description: "搜索媒体内容（音乐、有声书、播客等）。用于用户想要播放某个内容时进行搜索。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "搜索关键词，如歌手名、歌曲名、专辑名、节目名等"
                        }
                    },
                }),
            },
        }
    }

    /// 播放媒体工具（包含待选列表信息）
    fn play_media_tool(pending_results: &[MediaItem]) -> Tool {
        let list_desc = pending_results
            .iter()
            .enumerate()
            .map(|(i, item)| format!("{}. 《{}》 - {}", i, item.title, item.source.display_name()))
            .collect::<Vec<_>>()
            .join("\n");

        Tool {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "play_media".to_string(),
                description: format!(
                    "播放搜索结果中的某一项。用户可能说'第一个'、'第二个'或直接说标题。\n\n当前待选列表（index 从 0 开始）：\n{}",
                    list_desc
                ),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "index": {
                            "type": "integer",
                            "description": "要播放的结果序号，从 0 开始。'第一个'对应 0，'第二个'对应 1，以此类推"
                        }
                    },
                    "required": ["index"]
                }),
            },
        }
    }

    /// 退出选择工具
    fn exit_selection_tool() -> Tool {
        Tool {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "exit_selection".to_string(),
                description: "退出选择，不播放任何内容。用户可能说'不要了'、'取消'、'算了'等。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
            },
        }
    }

    /// 根据锁状态返回工具集
    fn get_tools(is_locked: bool, pending_results: &Option<Vec<MediaItem>>) -> Vec<Tool> {
        if is_locked {
            if let Some(results) = pending_results {
                vec![
                    Self::search_media_tool(),
                    Self::play_media_tool(results),
                    Self::exit_selection_tool(),
                ]
            } else {
                vec![Self::search_media_tool()]
            }
        } else {
            vec![Self::search_media_tool()]
        }
    }

    // ==================== 搜索和播放 ====================

    /// 聚合搜索所有来源
    async fn aggregate_search(&self, query: &str, limit_per_source: usize) -> Vec<MediaItem> {
        let futures: Vec<_> = self.providers.iter().map(|p| p.search(query, limit_per_source)).collect();

        let results = futures::future::join_all(futures).await;
        let mut all_results = Vec::new();

        for (provider, result) in self.providers.iter().zip(results) {
            match result {
                Ok(items) => {
                    info!("📻 MediaAgent: {:?} 搜索到 {} 个结果", provider.source(), items.len());
                    all_results.extend(items);
                },
                Err(e) => {
                    warn!("📻 MediaAgent: {:?} 搜索失败: {}", provider.source(), e);
                },
            }
        }

        // 按播放量排序
        all_results.sort_by_key(|item| std::cmp::Reverse(item.play_count.unwrap_or(0)));

        all_results
    }

    /// 播放媒体项
    async fn play_item(&self, item: &MediaItem, handles: &AgentHandles<'_>) -> anyhow::Result<()> {
        let provider = self
            .providers
            .iter()
            .find(|p| p.source() == item.source)
            .ok_or_else(|| anyhow!("未知的媒体来源: {:?}", item.source))?;

        provider.play(item, handles).await
    }

    /// 格式化搜索结果 TTS
    fn format_results_tts(items: &[MediaItem]) -> String {
        let mut parts = vec!["为您找到以下内容：".to_string()];

        for (i, item) in items.iter().take(5).enumerate() {
            parts.push(format!("第{}个，{}，来自{}", i + 1, item.title, item.source.display_name()));
        }

        parts.push("请问要播放哪一个？".to_string());
        parts.join("，")
    }

    // ==================== 锁管理 ====================

    fn acquire_lock(flags: &SharedFlags, results: Vec<MediaItem>) {
        let mut guard = flags.media_agent_lock.lock().unwrap();
        guard.acquire(results);
        info!("🔒 MediaAgent: 获取锁");
    }

    fn release_lock(flags: &SharedFlags) {
        let mut guard = flags.media_agent_lock.lock().unwrap();
        guard.release();
        info!("🔓 MediaAgent: 释放锁");
    }

    // ==================== 工具处理 ====================

    /// 处理 search_media 工具调用
    async fn handle_search_media(&self, args: &Value, handles: &AgentHandles<'_>) -> anyhow::Result<(String, Option<Vec<MediaItem>>)> {
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");

        if query.is_empty() {
            return Ok(("请告诉我您想搜索什么内容".to_string(), None));
        }

        info!("📻 MediaAgent: 搜索 '{}'", query);
        let results = self.aggregate_search(query, 3).await;

        match results.len() {
            0 => {
                let tts = "抱歉，没有找到相关内容";
                handles.tts_sink.send(tts).await;
                Ok((tts.to_string(), None))
            },
            1 => {
                let item = &results[0];
                let tts = format!("正在为您播放《{}》", item.title);
                handles.tts_sink.send(&tts).await;
                // 捕获播放错误，TTS 已经在 provider 中处理
                if let Err(e) = self.play_item(item, handles).await {
                    warn!("📻 MediaAgent: 单结果播放失败: {}", e);
                    return Ok(("播放失败".to_string(), None));
                }
                Ok((tts, None))
            },
            _ => {
                // 多结果，获取锁
                Self::acquire_lock(&handles.shared_flags, results.clone());
                let tts = Self::format_results_tts(&results);
                handles.tts_sink.send(&tts).await;
                Ok((tts, Some(results)))
            },
        }
    }

    /// 处理 play_media 工具调用
    async fn handle_play_media(&self, args: &Value, handles: &AgentHandles<'_>) -> anyhow::Result<String> {
        let index = args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

        let pending = {
            let guard = handles.shared_flags.media_agent_lock.lock().unwrap();
            guard.pending_results_cloned()
        };

        let Some(results) = pending else {
            let tts = "没有待选择的内容";
            handles.tts_sink.send(tts).await;
            return Ok(tts.to_string());
        };

        let Some(item) = results.get(index) else {
            let tts = format!("序号超出范围，只有 {} 个结果", results.len());
            handles.tts_sink.send(&tts).await;
            return Ok(tts);
        };

        let tts = format!("正在为您播放《{}》", item.title);
        handles.tts_sink.send(&tts).await;

        // 播放并捕获错误，无论成功失败都释放锁
        let play_result = self.play_item(item, handles).await;

        // 释放锁（确保无论播放成功或失败都释放）
        Self::release_lock(&handles.shared_flags);

        // 处理播放结果
        if let Err(e) = play_result {
            warn!("📻 MediaAgent: 选择播放失败: {}", e);
            // TTS 已经在 provider 中处理，这里返回失败信息
            return Ok("播放失败".to_string());
        }

        Ok(tts)
    }

    /// 处理 exit_selection 工具调用
    async fn handle_exit_selection(&self, handles: &AgentHandles<'_>) -> anyhow::Result<String> {
        Self::release_lock(&handles.shared_flags);
        let tts = "好的，已取消";
        handles.tts_sink.send(tts).await;
        Ok(tts.to_string())
    }

    // ==================== System Prompt ====================

    fn build_system_prompt(is_locked: bool, pending_results: &Option<Vec<MediaItem>>) -> String {
        if is_locked {
            let list = pending_results
                .as_ref()
                .map(|r| {
                    r.iter()
                        .enumerate()
                        .map(|(i, item)| format!("{}. 《{}》 - {}", i + 1, item.title, item.source.display_name()))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();

            format!(
                r#"你是一个媒体播放助手。用户正在从搜索结果中选择要播放的内容。

当前待选列表：
{}

根据用户的回复，调用相应的工具：
- 如果用户选择了某一项（如"第一个"、"播放那个音乐"、直接说标题），调用 play_media，index 从 0 开始
- 如果用户想退出（如"不要了"、"取消"、"算了"），调用 exit_selection
- 如果用户想搜索其他内容（如"换一个搜周杰伦"），调用 search_media

重要：用户说"第一个"对应 index=0，"第二个"对应 index=1，以此类推。"#,
                list
            )
        } else {
            r#"你是一个媒体播放助手。用户想要播放音乐或有声内容。
调用 search_media 工具来搜索用户想要的内容。
从用户的话中提取搜索关键词，例如歌手名、歌曲名、专辑名、节目名等。"#
                .to_string()
        }
    }

    // ==================== LLM 调用流程 ====================

    /// 处理工具调用
    async fn process_tool_call(&self, tool_call: &ToolCall, handles: &AgentHandles<'_>) -> anyhow::Result<(ToolControl, String)> {
        let tool_name = tool_call.function.name.as_deref().unwrap_or("");
        let raw_args = tool_call.function.arguments.clone().unwrap_or_else(|| "{}".to_string());

        let args: Value = serde_json::from_str(&raw_args).unwrap_or(json!({}));

        info!("📻 MediaAgent: 处理工具调用 {} args={}", tool_name, raw_args);

        match tool_name {
            "search_media" => {
                let (response, pending) = self.handle_search_media(&args, handles).await?;
                if pending.is_some() {
                    // 有待选结果，返回 Respond 结束本轮
                    Ok((ToolControl::Respond(response.clone()), response))
                } else {
                    // 单结果或无结果，返回 Stop
                    Ok((ToolControl::Stop, response))
                }
            },
            "play_media" => {
                let response = self.handle_play_media(&args, handles).await?;
                Ok((ToolControl::Stop, response))
            },
            "exit_selection" => {
                let response = self.handle_exit_selection(handles).await?;
                Ok((ToolControl::Stop, response))
            },
            _ => {
                warn!("📻 MediaAgent: 未知工具 {}", tool_name);
                Ok((ToolControl::Continue, format!("未知工具: {}", tool_name)))
            },
        }
    }

    /// 过滤有效的工具调用
    fn filter_valid_tool_calls(tool_calls: Vec<ToolCall>) -> Vec<ToolCall> {
        tool_calls
            .into_iter()
            .filter(|tc| {
                let has_valid_id = tc.id.as_ref().map(|id| !id.is_empty()).unwrap_or(false);
                let has_valid_name = tc.function.name.as_ref().map(|n| !n.is_empty()).unwrap_or(false);
                has_valid_id && has_valid_name
            })
            .map(|mut tc| {
                if tc.function.arguments.is_none() {
                    tc.function.arguments = Some("{}".to_string());
                }
                tc
            })
            .collect()
    }
}

#[async_trait]
impl Agent for MediaAgent {
    fn id(&self) -> &str {
        "agent.media"
    }

    fn intents(&self) -> Vec<&str> {
        vec![
            "agent.media", // agent自身ID，用于直接匹配
            "agent.media.music",
            "agent.music.play",
            "agent.music.query",
        ]
    }

    async fn run(&self, ctx: AgentContext, handles: AgentHandles<'_>) -> anyhow::Result<()> {
        let llm_client = handles.llm_client.upgrade().ok_or_else(|| anyhow!("llm client unavailable"))?;

        // 获取锁状态和待选结果
        let (is_locked, pending_results) = {
            let guard = ctx.shared_flags.media_agent_lock.lock().unwrap();
            (guard.is_locked(), guard.pending_results_cloned())
        };

        info!(
            "📻 MediaAgent: 开始处理, is_locked={}, user_text='{}'",
            is_locked, ctx.user_text
        );

        // 获取工具集
        let tools = Self::get_tools(is_locked, &pending_results);

        // 构建消息
        let system_prompt = Self::build_system_prompt(is_locked, &pending_results);
        let mut messages = vec![
            ChatMessage {
                role: Some("system".to_string()),
                content: Some(system_prompt),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: Some("user".to_string()),
                content: Some(ctx.user_text.clone()),
                tool_call_id: None,
                tool_calls: None,
            },
        ];

        let mut final_text: Option<String> = None;

        // LLM 循环
        for loop_index in 0..MAX_LLM_LOOPS {
            if handles.cancel.is_cancelled() {
                debug!("📻 MediaAgent: 被打断 at loop {}", loop_index);
                crate::agents::turn_tracker::interrupt_turn(&ctx.session_id).await;
                return Ok(());
            }

            let is_last_loop = loop_index + 1 == MAX_LLM_LOOPS;
            let params = ChatCompletionParams {
                tools: Some(tools.clone()),
                tool_choice: Some(if is_last_loop { ToolChoice::none() } else { ToolChoice::auto() }),
                stream: Some(true),
                ..Default::default()
            };

            // 流式 LLM 调用
            let stream = llm_client.chat_stream(messages.clone(), Some(params)).await?;
            let mut stream = Box::pin(stream);

            let mut accumulated_text = String::new();
            let mut accumulated_tool_calls: Vec<ToolCall> = Vec::new();
            let mut tool_calls_detected = false;
            let mut was_interrupted = false;

            loop {
                tokio::select! {
                    biased;

                    _ = handles.cancel.cancelled() => {
                        debug!("📻 MediaAgent: interrupted at token boundary (loop {})", loop_index);
                        was_interrupted = true;
                        break;
                    }

                    maybe_item = stream.next() => {
                        match maybe_item {
                            Some(Ok(choice)) => {
                                if let Some(delta) = &choice.delta {
                                    if let Some(text) = &delta.content {
                                        if !text.is_empty() && !tool_calls_detected {
                                            accumulated_text.push_str(text);
                                            handles.tts_sink.send(text).await;
                                        }
                                    }
                                    if let Some(tc_delta) = &delta.tool_calls {
                                        if !tc_delta.is_empty() {
                                            tool_calls_detected = true;
                                            for d in tc_delta {
                                                merge_tool_call_delta(&mut accumulated_tool_calls, d);
                                            }
                                        }
                                    }
                                } else if let Some(message) = &choice.message {
                                    if let Some(text) = &message.content {
                                        if !text.is_empty() && !tool_calls_detected {
                                            accumulated_text.push_str(text);
                                            handles.tts_sink.send(text).await;
                                        }
                                    }
                                    if let Some(tcs) = &message.tool_calls {
                                        if !tcs.is_empty() {
                                            tool_calls_detected = true;
                                            accumulated_tool_calls.extend(tcs.clone());
                                        }
                                    }
                                }
                            }
                            Some(Err(e)) => {
                                error!("📻 MediaAgent: stream error at loop {}: {}", loop_index, e);
                                break;
                            }
                            None => break,
                        }
                    }
                }
            }

            if was_interrupted {
                crate::agents::turn_tracker::interrupt_turn(&ctx.session_id).await;
                return Ok(());
            }

            // 构建 assistant 消息
            let assistant_message = ChatMessage {
                role: Some("assistant".to_string()),
                content: if tool_calls_detected { None } else { Some(accumulated_text.clone()) },
                tool_call_id: None,
                tool_calls: if tool_calls_detected {
                    Some(Self::filter_valid_tool_calls(accumulated_tool_calls.clone()))
                } else {
                    None
                },
            };
            messages.push(assistant_message);

            // 处理工具调用
            if tool_calls_detected {
                let valid_tool_calls = Self::filter_valid_tool_calls(accumulated_tool_calls);
                if valid_tool_calls.is_empty() {
                    warn!("📻 MediaAgent: all tool calls filtered out at loop {}", loop_index);
                    continue;
                }

                for tool_call in valid_tool_calls {
                    if handles.cancel.is_cancelled() {
                        crate::agents::turn_tracker::interrupt_turn(&ctx.session_id).await;
                        return Ok(());
                    }

                    let call_id = tool_call.id.clone().unwrap_or_else(|| format!("media_tool_{}", loop_index));

                    let (control, tool_result) = self.process_tool_call(&tool_call, &handles).await?;

                    // 添加工具结果到消息
                    messages.push(ChatMessage {
                        role: Some("tool".to_string()),
                        content: Some(tool_result.clone()),
                        tool_call_id: Some(call_id),
                        tool_calls: None,
                    });

                    match control {
                        ToolControl::Stop => {
                            // TTS 模式下，complete_tool_call 已添加 assistant 消息
                            return Ok(());
                        },
                        ToolControl::Respond(_text) => {
                            // TTS 模式下，complete_tool_call 已添加 assistant 消息
                            return Ok(());
                        },
                        ToolControl::Interrupted => {
                            crate::agents::turn_tracker::interrupt_turn(&ctx.session_id).await;
                            return Ok(());
                        },
                        ToolControl::Continue => {
                            // 继续下一轮
                        },
                    }
                }
                continue;
            }

            // 无工具调用，使用文本回复
            if !accumulated_text.trim().is_empty() {
                final_text = Some(accumulated_text);
            }
            break;
        }

        if let Some(text) = final_text {
            if !text.trim().is_empty() {
                // 🎯 中心化：添加 assistant 消息到 TurnTracker
                crate::agents::turn_tracker::add_assistant_message(&ctx.session_id, &text).await;
            }
        }

        Ok(())
    }
}
