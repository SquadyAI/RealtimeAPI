//! Translate Agent - 翻译 agent
//!
//! 处理 agent.language.translate intent
//! 支持两种模式：
//! 1. 单次翻译：直接调用 LLM 翻译
//! 2. 同声传译：设置标志位，切换到翻译管线

use anyhow::anyhow;
use async_trait::async_trait;
use futures_util::StreamExt;
use std::sync::atomic::Ordering;
use tracing::{debug, info, warn};

use super::stream_utils::merge_tool_call_delta;
use super::{Agent, AgentContext, AgentHandles};
use crate::llm::{ChatCompletionParams, ChatMessage, Tool, ToolCall, ToolChoice, ToolFunction};
use serde_json::json;

/// 最大工具调用循环次数
const MAX_TOOL_LOOPS: usize = 3;

/// 同传模式的触发关键词
#[allow(dead_code)]
const CONTINUOUS_KEYWORDS: &[&str] = &[
    "接下来",
    "以后",
    "一直",
    "所有",
    "都",
    "模式",
    "持续",
    "continuous",
    "everything",
    "同声传译",
    "同传",
    "实时翻译",
    "互译",
];

pub struct TranslateAgent;

impl Default for TranslateAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl TranslateAgent {
    pub fn new() -> Self {
        Self
    }

    /// 检测是否需要同传模式（而非单次翻译）
    #[allow(dead_code)]
    fn is_continuous_mode(text: &str) -> bool {
        let text_lower = text.to_lowercase();
        CONTINUOUS_KEYWORDS.iter().any(|kw| text_lower.contains(kw))
    }

    /// 选择 ASR 引擎（统一使用 WhisperLive）
    fn select_asr_engine(_lang_a: &str, _lang_b: &str) -> &'static str {
        "whisperlive"
    }

    /// 将语言代码转换为显示名称
    /// 使用统一的 lang 模块，支持 32 种语言
    fn lang_code_to_display_name(code: &str, display_lang: &str) -> String {
        crate::lang::get_display_name(code, display_lang).to_string()
    }

    /// 生成同传激活确认文本
    /// 支持：中英日韩粤、西班牙语、意大利语，其他 fallback 到英语
    fn generate_activation_text(lang_a: &str, lang_b: &str) -> String {
        let name_a = Self::lang_code_to_display_name(lang_a, lang_a);
        let name_b = Self::lang_code_to_display_name(lang_b, lang_a);

        match lang_a.to_lowercase().as_str() {
            // 中文
            "zh" | "zh-cn" | "zh-hans" | "zh-tw" | "zh-hk" | "zh-hant" => {
                format!("双向同声传译已启动：{}和{}互译。", name_a, name_b)
            },
            // 英语
            "en" | "en-us" | "en-gb" => {
                format!("Bidirectional interpretation activated: {} and {}.", name_a, name_b)
            },
            // 日语
            "ja" | "jp" => {
                format!("双方向同時通訳を開始：{}と{}の相互翻訳。", name_a, name_b)
            },
            // 韩语
            "ko" | "ko-kr" => {
                format!("양방향 동시통역 시작: {}와(과) {} 상호 번역.", name_a, name_b)
            },
            // 粤语
            "yue" | "yue-hk" | "cantonese" => {
                format!("雙向同聲傳譯已啟動：{}同{}互譯。", name_a, name_b)
            },
            // 西班牙语
            "es" | "es-es" | "es-mx" | "spanish" => {
                format!("Interpretación bidireccional activada: {} y {}.", name_a, name_b)
            },
            // 意大利语
            "it" | "it-it" | "italian" => {
                format!("Interpretazione bidirezionale attivata: {} e {}.", name_a, name_b)
            },
            // 其他语言 fallback 到英语
            _ => {
                format!("Bidirectional interpretation activated: {} and {}.", name_a, name_b)
            },
        }
    }

    /// 启动同传模式
    async fn start_simul_interpret(&self, lang_a: &str, lang_b: &str, session_id: &str, handles: &AgentHandles<'_>) {
        info!("🌍 TranslateAgent: 启动双向同声传译模式 {} <-> {}", lang_a, lang_b);

        // 选择 ASR 引擎
        let asr_engine = Self::select_asr_engine(lang_a, lang_b);
        info!("🎤 ASR 引擎选择: {} (lang_a={}, lang_b={})", asr_engine, lang_a, lang_b);

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

        // 设置 ASR 引擎偏好（用于后续管线切换时选择正确的引擎）
        {
            let mut engine = handles.shared_flags.preferred_asr_engine.lock().unwrap();
            *engine = Some(asr_engine.to_string());
        }

        // 发送 ASR 引擎变更通知，触发 ASR 任务重建会话
        let _ = handles.shared_flags.asr_engine_notify_tx.send(Some(asr_engine.to_string()));
        info!("📢 已发送 ASR 引擎变更通知: {}", asr_engine);

        // 生成确认文本
        let confirmation_text = Self::generate_activation_text(lang_a, lang_b);

        // 发送工具调用信令给客户端
        if let Some(emitter) = handles.emitter.upgrade() {
            let call_id = format!("translate_simul_{}", nanoid::nanoid!(6));
            let args = json!({
                "language_a": lang_a,
                "language_b": lang_b,
                "asr_engine": asr_engine,
            })
            .to_string();

            emitter
                .response_function_call_arguments_done(&handles.turn_context, &call_id, "start_simul_interpret", &args)
                .await;
            emitter
                .response_function_call_result_done(&handles.turn_context, &call_id, &confirmation_text)
                .await;
        }

        // 播报确认
        handles.tts_sink.send(&confirmation_text).await;

        info!("✅ 双向同声传译已启动: {} <-> {}, ASR={}", lang_a, lang_b, asr_engine);
        // 不写入对话历史（同传模式下不需要保存激活消息）
    }

    /// 创建同传启动工具定义
    fn simul_interpret_tool() -> Tool {
        Tool {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "start_simul_interpret".to_string(),
                description: "启动双向同声传译模式。当用户明确表示需要持续的实时翻译/同声传译时调用。\n\n调用条件：\n- 用户说「同声传译」「同传」「实时翻译」「互译」等关键词\n- 用户表达持续翻译的意图（如「接下来」「一直」「所有」）\n\n注意：必须明确知道两种语言才能调用。如果用户没有指定语言，先询问用户。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "language_a": {
                            "type": "string",
                            "description": "第一种语言代码，如：zh（中文）、en（英文）、ja（日语）、ko（韩语）、fr（法语）、de（德语）、es（西班牙语）、it（意大利语）等"
                        },
                        "language_b": {
                            "type": "string",
                            "description": "第二种语言代码"
                        }
                    },
                    "required": ["language_a", "language_b"]
                }),
            },
        }
    }

    /// 创建单次翻译工具定义
    #[allow(dead_code)]
    fn translate_once_tool() -> Tool {
        Tool {
            tool_type: "function".to_string(),
            function: ToolFunction {
                name: "translate_text".to_string(),
                description: "翻译一段文本。当用户只是需要翻译某个词、某句话时调用（非持续翻译）。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "需要翻译的文本"
                        },
                        "target_language": {
                            "type": "string",
                            "description": "目标语言代码，如：zh、en、ja、ko、fr、de、es、it 等"
                        }
                    },
                    "required": ["text", "target_language"]
                }),
            },
        }
    }

    /// 构建翻译 Agent 的工具列表
    /// 方案 A：只保留同传工具，单次翻译由 LLM 直接回答
    fn build_tools() -> Vec<Tool> {
        vec![
            Self::simul_interpret_tool(),
            // translate_once_tool 已移除：单次翻译由 LLM 直接回答，避免小模型 function calling 不稳定
        ]
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

    /// 构建系统提示词
    async fn build_system_prompt(ctx: &AgentContext) -> String {
        let structured_block = crate::agents::runtime::build_user_structured_block_async(ctx).await;

        let base_prompt = r#"<agentProfile>
你是一个专业的翻译助手。你的任务是帮助用户进行翻译。
<requirements>
  <requirement>始终使用用户的输入语言或要求的语言回复</requirement>
  <requirement>单次翻译（翻译一个词、一句话）直接回答，不调用工具</requirement>
</requirements>
<tools>
  <tool name="start_simul_interpret">用于启动同声传译模式（持续的实时翻译）</tool>
</tools>
<intentRules>
  <rule>用户说「同声传译」「同传」「实时翻译」「互译」或表达持续翻译意图 → 使用 start_simul_interpret</rule>
  <rule>用户只是想翻译某个词或某句话 → 直接回答翻译结果，不调用任何工具</rule>
  <rule>用户要启动同传但没有明确说两种语言 → 先询问需要哪两种语言互译</rule>
</intentRules>
<notes>
  <note>启动同传前必须确认两种语言。例如：「请问您需要哪两种语言互译？比如中英互译、中日互译？」</note>
  <note>单次翻译时，根据上下文推断目标语言（如果用户没有明确指定），直接给出翻译结果</note>
</notes>
</agentProfile>"#;

        crate::agents::runtime::build_agent_system_prompt_with_time_async(
            &ctx.session_id,
            ctx.role_prompt.as_deref(),
            base_prompt,
            &structured_block,
            ctx.user_now.as_deref(),
            ctx.extra.user_timezone.as_deref(),
        )
        .await
    }

    /// 处理单次翻译
    async fn handle_translate_once(&self, text: &str, target_lang: &str, handles: &AgentHandles<'_>) -> anyhow::Result<String> {
        let llm_client = handles.llm_client.upgrade().ok_or_else(|| anyhow!("LLM client unavailable"))?;

        let lang_name = Self::lang_code_to_display_name(target_lang, "en");

        let messages = vec![
            ChatMessage {
                role: Some("system".to_string()),
                content: Some(format!(
                    "You are a professional translator. Translate the following text to {}. Output only the translation, no explanations.",
                    lang_name
                )),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: Some("user".to_string()),
                content: Some(text.to_string()),
                tool_call_id: None,
                tool_calls: None,
            },
        ];

        let params = ChatCompletionParams { stream: Some(true), ..Default::default() };

        let stream = llm_client.chat_stream(messages, Some(params)).await?;
        let mut stream = Box::pin(stream);

        let mut result = String::new();

        while let Some(item) = stream.next().await {
            match item {
                Ok(choice) => {
                    if let Some(delta) = &choice.delta {
                        if let Some(content) = &delta.content {
                            result.push_str(content);
                            handles.tts_sink.send(content).await;
                        }
                    } else if let Some(message) = &choice.message {
                        if let Some(content) = &message.content {
                            result.push_str(content);
                            handles.tts_sink.send(content).await;
                        }
                    }
                },
                Err(e) => {
                    warn!("翻译流式错误: {}", e);
                    break;
                },
            }
        }

        Ok(result)
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
impl Agent for TranslateAgent {
    fn id(&self) -> &str {
        "agent.language.translate"
    }

    fn intents(&self) -> Vec<&str> {
        vec!["agent.language.translate"]
    }

    async fn run(&self, ctx: AgentContext, handles: AgentHandles<'_>) -> anyhow::Result<()> {
        let llm_client = handles.llm_client.upgrade().ok_or_else(|| anyhow!("LLM client unavailable"))?;

        info!("🌐 TranslateAgent 处理: '{}'", ctx.user_text);

        // 🎯 中心化：从 TurnTracker 获取对话历史
        let conversation = super::turn_tracker::get_history_messages(&ctx.session_id).await;

        // 构建消息列表
        let mut messages = Vec::with_capacity(conversation.len() + 2);

        // 添加系统提示词
        let system_prompt = Self::build_system_prompt(&ctx).await;
        messages.push(ChatMessage {
            role: Some("system".to_string()),
            content: Some(system_prompt),
            tool_call_id: None,
            tool_calls: None,
        });

        // 添加对话历史
        messages.extend(conversation);

        // 构建工具列表
        let tools = Self::build_tools();

        let mut final_text: Option<String> = None;

        // 工具调用循环
        for loop_index in 0..MAX_TOOL_LOOPS {
            if handles.cancel.is_cancelled() {
                debug!("TranslateAgent cancelled at loop {}", loop_index);
                super::turn_tracker::interrupt_turn(&ctx.session_id).await;
                return Ok(());
            }

            let is_last_loop = loop_index + 1 == MAX_TOOL_LOOPS;
            let params = Self::build_params(&tools, is_last_loop);

            // 流式 LLM 调用
            let stream = llm_client.chat_stream(messages.clone(), Some(params)).await?;
            let mut stream = Box::pin(stream);

            let mut accumulated_text = String::new();
            let mut accumulated_tool_calls: Vec<ToolCall> = Vec::new();
            let mut tool_calls_detected = false;
            let mut was_interrupted = false;

            // 流式处理
            loop {
                tokio::select! {
                    biased;

                    _ = handles.cancel.cancelled() => {
                        debug!("🛑 TranslateAgent interrupted at token boundary (loop {})", loop_index);
                        was_interrupted = true;
                        break;
                    }

                    maybe_item = stream.next() => {
                        match maybe_item {
                            Some(Ok(choice)) => {
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
                                warn!("TranslateAgent stream error at loop {}: {}", loop_index, e);
                                break;
                            }
                            None => break,
                        }
                    }
                }
            }

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
                    warn!("TranslateAgent: all tool calls filtered out at loop {}", loop_index);
                    continue;
                }

                for tool_call in valid_tool_calls {
                    let tool_name = tool_call.function.name.as_deref().unwrap_or("");
                    let call_id = tool_call.id.clone().unwrap_or_else(|| format!("translate_call_{}", loop_index));
                    let raw_args = tool_call.function.arguments.clone().unwrap_or_else(|| "{}".to_string());

                    info!("🔧 TranslateAgent 工具调用: {} args={}", tool_name, raw_args);

                    match tool_name {
                        "start_simul_interpret" => {
                            // 解析参数
                            let params: serde_json::Value = serde_json::from_str(&raw_args).unwrap_or_default();
                            let lang_a = params.get("language_a").and_then(|v| v.as_str()).unwrap_or("zh");
                            let lang_b = params.get("language_b").and_then(|v| v.as_str()).unwrap_or("en");

                            // 启动同传
                            self.start_simul_interpret(lang_a, lang_b, &ctx.session_id, &handles).await;
                            return Ok(());
                        },
                        "translate_text" => {
                            // 解析参数
                            let params: serde_json::Value = serde_json::from_str(&raw_args).unwrap_or_default();
                            let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            let target_lang = params.get("target_language").and_then(|v| v.as_str()).unwrap_or("en");

                            if text.is_empty() {
                                let error_text = "翻译文本为空";
                                messages.push(ChatMessage {
                                    role: Some("tool".to_string()),
                                    content: Some(error_text.to_string()),
                                    tool_call_id: Some(call_id),
                                    tool_calls: None,
                                });
                                continue;
                            }

                            // 执行翻译
                            match self.handle_translate_once(text, target_lang, &handles).await {
                                Ok(result) => {
                                    final_text = Some(result.clone());
                                    messages.push(ChatMessage {
                                        role: Some("tool".to_string()),
                                        content: Some(result),
                                        tool_call_id: Some(call_id),
                                        tool_calls: None,
                                    });
                                    // 单次翻译完成后直接返回
                                    let text = final_text.unwrap_or_default();
                                    if !text.trim().is_empty() {
                                        super::turn_tracker::add_assistant_message(&ctx.session_id, &text).await;
                                    }
                                    return Ok(());
                                },
                                Err(e) => {
                                    let error_text = format!("翻译失败: {}", e);
                                    messages.push(ChatMessage {
                                        role: Some("tool".to_string()),
                                        content: Some(error_text),
                                        tool_call_id: Some(call_id),
                                        tool_calls: None,
                                    });
                                },
                            }
                        },
                        _ => {
                            let error_text = format!("未知工具: {}", tool_name);
                            messages.push(ChatMessage {
                                role: Some("tool".to_string()),
                                content: Some(error_text),
                                tool_call_id: Some(call_id),
                                tool_calls: None,
                            });
                        },
                    }
                }
                continue;
            }

            // 无工具调用，直接使用文本回复（可能是追问用户）
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
