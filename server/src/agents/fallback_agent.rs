//! Fallback Agent - 通用闲聊 agent
//!
//! 当 intent 为空或无法匹配到专用 agent 时使用。
//! 实现完整的 LLM 对话流程，包括工具调用支持。

use anyhow::anyhow;
use async_trait::async_trait;
use futures_util::StreamExt;
use std::sync::atomic::Ordering;
use tracing::{debug, error, info, warn};

use super::stream_utils::merge_tool_call_delta;
use super::tool_utils::UNSUPPORTED_TOOL_TEXT;
use super::{Agent, AgentContext, AgentHandles, ToolControl};
use crate::llm::{ChatCompletionParams, ChatMessage, Tool, ToolCall, ToolChoice};
use std::collections::HashSet;

/// 最大工具调用循环次数，防止无限循环
const MAX_TOOL_LOOPS: usize = 5;

pub struct FallbackAgent;

impl Default for FallbackAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl FallbackAgent {
    pub fn new() -> Self {
        Self
    }

    fn ensure_tool_call_id(tool_call: &ToolCall, loop_index: usize, call_index: usize) -> String {
        tool_call
            .id
            .clone()
            .unwrap_or_else(|| format!("fallback_tool_{}_{}", loop_index, call_index))
    }

    /// 构建 LLM 请求参数
    fn build_params(tools: &[Tool], is_last_loop: bool) -> ChatCompletionParams {
        let mut params = ChatCompletionParams::default();
        if !tools.is_empty() {
            params.tools = Some(tools.to_vec());
            params.tool_choice = Some(if is_last_loop { ToolChoice::none() } else { ToolChoice::auto() });
        }
        params.stream = Some(true);
        params
    }

    /// 处理单个工具调用
    #[allow(clippy::too_many_arguments)]
    async fn process_tool_call(
        &self,
        tool_call: &ToolCall,
        loop_index: usize,
        call_index: usize,
        session_id: &str,
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
        debug!("FallbackAgent processing tool call: name={}, args={}", tool_name, raw_args);

        // 特殊处理：同声传译启动工具
        if tool_name == "start_simul_interpret" {
            return self
                .handle_start_simul_interpret(&call_id, &raw_args, session_id, handles, messages)
                .await;
        }

        // 通过 RuntimeToolClient 调用工具（自动路由到内置/MCP/客户端）
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
            }
            return ToolControl::Respond(speak_text);
        }
        outcome.control
    }

    /// 将语言代码转换为对应语言的本地化名称
    fn lang_code_to_display_name(code: &str, display_lang: &str) -> String {
        // 根据显示语言返回对应的语言名称
        let is_chinese_display = matches!(display_lang.to_lowercase().as_str(), "zh" | "zh-cn" | "zh-hans");

        match code.to_lowercase().as_str() {
            "zh" | "zh-cn" | "zh-hans" | "chinese" => if is_chinese_display { "中文" } else { "Chinese" }.to_string(),
            "en" | "en-us" | "en-gb" | "english" => if is_chinese_display { "英文" } else { "English" }.to_string(),
            "ja" | "jp" | "japanese" => if is_chinese_display { "日语" } else { "Japanese" }.to_string(),
            "ko" | "korean" => if is_chinese_display { "韩语" } else { "Korean" }.to_string(),
            "fr" | "french" => if is_chinese_display { "法语" } else { "French" }.to_string(),
            "de" | "german" => if is_chinese_display { "德语" } else { "German" }.to_string(),
            "es" | "spanish" => if is_chinese_display { "西班牙语" } else { "Spanish" }.to_string(),
            "pt" | "portuguese" => if is_chinese_display { "葡萄牙语" } else { "Portuguese" }.to_string(),
            "ru" | "russian" => if is_chinese_display { "俄语" } else { "Russian" }.to_string(),
            "it" | "italian" => if is_chinese_display { "意大利语" } else { "Italian" }.to_string(),
            "ar" | "arabic" => if is_chinese_display { "阿拉伯语" } else { "Arabic" }.to_string(),
            "th" | "thai" => if is_chinese_display { "泰语" } else { "Thai" }.to_string(),
            "vi" | "vietnamese" => if is_chinese_display { "越南语" } else { "Vietnamese" }.to_string(),
            "id" | "indonesian" => if is_chinese_display { "印尼语" } else { "Indonesian" }.to_string(),
            "ms" | "malay" => if is_chinese_display { "马来语" } else { "Malay" }.to_string(),
            "nl" | "dutch" => if is_chinese_display { "荷兰语" } else { "Dutch" }.to_string(),
            "pl" | "polish" => if is_chinese_display { "波兰语" } else { "Polish" }.to_string(),
            "tr" | "turkish" => if is_chinese_display { "土耳其语" } else { "Turkish" }.to_string(),
            "hi" | "hindi" => if is_chinese_display { "印地语" } else { "Hindi" }.to_string(),
            "yue" | "cantonese" => if is_chinese_display { "粤语" } else { "Cantonese" }.to_string(),
            _ => code.to_string(),
        }
    }

    /// 处理同声传译启动工具调用（双向互译模式）
    async fn handle_start_simul_interpret(&self, call_id: &str, raw_args: &str, session_id: &str, handles: &AgentHandles<'_>, _messages: &mut [ChatMessage]) -> ToolControl {
        info!("🟣 FallbackAgent: 触发双向同声传译模式");

        // 解析参数
        let params: std::collections::HashMap<String, serde_json::Value> = serde_json::from_str(raw_args).unwrap_or_default();

        let lang_a = params.get("language_a").and_then(|v| v.as_str()).unwrap_or("zh");
        let lang_b = params.get("language_b").and_then(|v| v.as_str()).unwrap_or("en");

        // 记录进入同传前的 turn 数量（用于退出时截断）
        let turn_count = super::turn_tracker::get_turns_count(session_id).await;
        handles
            .shared_flags
            .simul_interpret_turn_start_count
            .store(turn_count, Ordering::Release);
        info!("📝 记录同传起始 turn 数量: {}", turn_count);

        // 设置同声传译标志
        handles.shared_flags.simul_interpret_enabled.store(true, Ordering::Release);
        {
            let mut a = handles.shared_flags.simul_interpret_language_a.lock().unwrap();
            *a = lang_a.to_string();
        }
        {
            let mut b = handles.shared_flags.simul_interpret_language_b.lock().unwrap();
            *b = lang_b.to_string();
        }

        // 将语言代码转换为显示名称
        let name_a = Self::lang_code_to_display_name(lang_a, lang_a);
        let name_b = Self::lang_code_to_display_name(lang_b, lang_a);

        // 生成工具结果（优先使用工具提供的 tts_text；否则按 language_a 选择本地化默认文案）
        let result_text = if let Some(tts) = params
            .get("tts_text")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            tts.to_string()
        } else {
            match lang_a.to_lowercase().as_str() {
                "zh" | "zh-cn" | "zh-hans" => format!("双向同声传译已启动：{}和{}互译。", name_a, name_b),
                "en" | "en-us" | "en-gb" => format!("Bidirectional interpretation activated: {} and {}.", name_a, name_b),
                "ja" | "jp" => format!("双方向同時通訳を開始：{}と{}の相互翻訳。", name_a, name_b),
                "ko" => format!("양방향 동시 통역 활성화: {} 및 {} 상호 번역.", name_a, name_b),
                "fr" => format!("Interprétation bidirectionnelle activée: {} et {}.", name_a, name_b),
                "de" => format!("Bidirektionale Übersetzung aktiviert: {} und {}.", name_a, name_b),
                "es" => format!("Interpretación bidireccional activada: {} y {}.", name_a, name_b),
                _ => format!("Bidirectional interpretation activated: {} and {}.", name_a, name_b),
            }
        };

        info!("✅ 双向同声传译标志已设置: lang_a={}, lang_b={}", lang_a, lang_b);

        // 发送工具调用信令给客户端（包含 language_a 和 language_b）
        if let Some(emitter) = handles.emitter.upgrade() {
            // 构建包含语言信息的参数 JSON
            let args_with_languages = serde_json::json!({
                "language_a": lang_a,
                "language_b": lang_b,
                "tts_text": result_text.clone()
            })
            .to_string();

            emitter
                .response_function_call_arguments_done(&handles.turn_context, call_id, "start_simul_interpret", &args_with_languages)
                .await;
            emitter
                .response_function_call_result_done(&handles.turn_context, call_id, &result_text)
                .await;
        }

        // 直接通过 TTS 播报，不将确认文案写入对话上下文
        // 返回 Stop 以终止本轮 LLM 生成，避免把"激活同传"的回复保存到历史
        handles.tts_sink.send(&result_text).await;
        ToolControl::Stop
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
                // 补齐缺失的 arguments
                if tc.function.arguments.is_none() {
                    tc.function.arguments = Some("{}".to_string());
                }
                tc
            })
            .collect()
    }
}

#[async_trait]
impl Agent for FallbackAgent {
    fn id(&self) -> &str {
        "agent.fallback"
    }

    async fn run(&self, ctx: AgentContext, handles: AgentHandles<'_>) -> anyhow::Result<()> {
        let llm_client = handles.llm_client.upgrade().ok_or_else(|| anyhow!("llm client unavailable"))?;

        // 🎯 中心化：从 TurnTracker 获取完整消息（含 system prompt + 时间注入）
        let mut messages = super::turn_tracker::get_llm_messages(&ctx.session_id).await;

        // 注入 wiki 上下文到 system prompt（如果存在）
        if let Some(wiki) = &ctx.wiki_context {
            if let Some(system_msg) = messages.iter_mut().find(|m| m.role.as_deref() == Some("system")) {
                if let Some(content) = &mut system_msg.content {
                    content.push_str(&format!(
                        "\n\n<wiki_context>\n<title>{}</title>\n<content>{}</content>\n</wiki_context>",
                        wiki.title, wiki.content
                    ));
                    debug!("📚 已注入 wiki 上下文: title={}", wiki.title);
                }
            }
        }

        // FallbackAgent 使用所有可用工具（不过滤）
        // 工具列表已在 build_merged_tools() 中合并（内置工具 + 外部工具，已去重）
        debug!("FallbackAgent tools: {} available", ctx.tools.len());
        let allowed_tool_names: HashSet<String> = ctx.tools.iter().map(|t| t.function.name.clone()).collect();

        let mut final_text: Option<String> = None;

        // 工具调用循环
        for loop_index in 0..MAX_TOOL_LOOPS {
            if handles.cancel.is_cancelled() {
                debug!("FallbackAgent cancelled at loop {}", loop_index);
                super::turn_tracker::interrupt_turn(&ctx.session_id).await;
                return Ok(());
            }

            let is_last_loop = loop_index + 1 == MAX_TOOL_LOOPS;
            let params = Self::build_params(&ctx.tools, is_last_loop);

            // 流式 LLM 调用
            let stream = llm_client.chat_stream(messages.clone(), Some(params)).await?;
            let mut stream = Box::pin(stream);

            let mut accumulated_text = String::new();
            let mut accumulated_tool_calls: Vec<ToolCall> = Vec::new();
            let mut tool_calls_detected = false;
            let mut was_interrupted = false;

            // 使用 tokio::select! 实现真正的 next-token 打断
            loop {
                tokio::select! {
                    biased; // 优先检查打断分支

                    // 打断分支：与 stream.next() 真正并发
                    _ = handles.cancel.cancelled() => {
                        debug!("🛑 FallbackAgent interrupted at token boundary (loop {})", loop_index);
                        was_interrupted = true;
                        break;
                    }

                    // Token 分支
                    maybe_item = stream.next() => {
                        match maybe_item {
                            Some(Ok(choice)) => {
                                // 处理 delta（流式增量）
                                if let Some(delta) = &choice.delta {
                                    if let Some(text) = &delta.content
                                        && !text.is_empty() && !tool_calls_detected {
                                            accumulated_text.push_str(text);
                                            handles.tts_sink.send(text).await;
                                        }
                                    if let Some(tc_delta) = &delta.tool_calls
                                        && !tc_delta.is_empty() {
                                            tool_calls_detected = true;
                                            for d in tc_delta {
                                                merge_tool_call_delta(&mut accumulated_tool_calls, d);
                                            }
                                        }
                                }
                                // 处理 message（完整消息，非流式兼容）
                                else if let Some(message) = &choice.message {
                                    if let Some(text) = &message.content
                                        && !text.is_empty() && !tool_calls_detected {
                                            accumulated_text.push_str(text);
                                            handles.tts_sink.send(text).await;
                                        }
                                    if let Some(tcs) = &message.tool_calls
                                        && !tcs.is_empty() {
                                            tool_calls_detected = true;
                                            for tc in tcs {
                                                accumulated_tool_calls.push(tc.clone());
                                            }
                                        }
                                }
                            }
                            Some(Err(e)) => {
                                error!("FallbackAgent stream error at loop {}: {}", loop_index, e);
                                break;
                            }
                            None => break, // Stream 结束
                        }
                    }
                }
            }

            // 如果被打断，立即返回（不写入上下文）
            if was_interrupted {
                super::turn_tracker::interrupt_turn(&ctx.session_id).await;
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
                    warn!("FallbackAgent: all tool calls filtered out at loop {}", loop_index);
                    continue;
                }

                info!(
                    "FallbackAgent processing {} tool calls at loop {}",
                    valid_tool_calls.len(),
                    loop_index
                );
                let should_stop = false;

                for (call_index, tool_call) in valid_tool_calls.iter().enumerate() {
                    if handles.cancel.is_cancelled() {
                        super::turn_tracker::interrupt_turn(&ctx.session_id).await;
                        return Ok(());
                    }

                    match self
                        .process_tool_call(
                            tool_call,
                            loop_index,
                            call_index,
                            &ctx.session_id,
                            &handles,
                            &mut messages,
                            &allowed_tool_names,
                        )
                        .await
                    {
                        ToolControl::Continue => {},
                        ToolControl::Stop => {
                            return Ok(());
                        },
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
                // 继续下一轮循环，让 LLM 根据工具结果生成最终回复
                continue;
            }

            // 无工具调用，直接使用文本回复
            if !accumulated_text.trim().is_empty() {
                final_text = Some(accumulated_text);
            }
            break;
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
