use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use nanoid::nanoid;
use serde_json::Value as JsonValue;
use tokio::sync::{Notify, broadcast};
use tracing::{error, trace, warn};

use crate::function_callback::CallResult;
use crate::mcp::{McpContent, McpManager, client::McpClientWrapper};
use crate::rpc::{EventEmitter, SharedFlags, SimpleInterruptHandler, ToolCallManager, TurnContext, WikiContext};
use crate::text_filters;

/// Snapshot of the turn information provided to an Agent.
#[derive(Clone, Default)]
pub struct AgentExtra {
    pub user_ip: Option<String>,
    pub user_city: Option<String>,
    pub user_timezone: Option<String>,
    pub asr_language: Option<String>,
    pub intent_label: Option<String>,
}

#[derive(Clone)]
pub struct AgentContext {
    pub session_id: String,
    pub user_text: String,
    pub user_now: Option<String>,
    pub turn_context: TurnContext,
    pub shared_flags: Arc<SharedFlags>,
    pub system_prompt: Option<String>,
    /// 从 system_prompt 中提取的 `<role>` 部分，用于注入专用 Agent
    pub role_prompt: Option<String>,
    /// 完整的工具列表（内置工具 + 外部工具，已合并去重）
    pub tools: Vec<crate::llm::llm::Tool>,
    /// 离线工具列表 - 用于拒绝逻辑判断
    pub offline_tools: Vec<String>,
    pub extra: AgentExtra,
    /// Wiki 知识库上下文（仅中文语言时可能有值）
    pub wiki_context: Option<WikiContext>,
}

/// Shared handles that allow an agent to interact with runtime services.
pub struct AgentHandles<'a> {
    pub llm_client: Weak<crate::llm::LlmClient>,
    pub shared_flags: Arc<SharedFlags>,
    pub enable_search: Arc<AtomicBool>,
    /// Raw MCP clients for agents that need low-level control (e.g. list tools, custom streaming).
    pub mcp_clients: Arc<Vec<McpClientWrapper>>,
    pub tts_sink: &'a dyn AgentTtsSink,
    /// High-level tool entry point; fans out to built-ins, MCP, or client tools transparently.
    pub tool_client: &'a dyn AgentToolClient,
    pub cancel: &'a dyn AgentCancelToken,
    pub interrupt_handler: Option<SimpleInterruptHandler>,
    /// Event emitter for sending signals to client (e.g. function_call notifications)
    pub emitter: Weak<EventEmitter>,
    /// Current turn context for emitting events
    pub turn_context: TurnContext,
}

#[async_trait]
pub trait Agent: Send + Sync {
    fn id(&self) -> &str;

    /// Returns the list of intent keys this agent handles.
    /// Default implementation returns just the agent id.
    fn intents(&self) -> Vec<&str> {
        vec![self.id()]
    }

    /// 执行 Agent 逻辑
    ///
    /// Agent 内部负责：
    /// - 通过 tts_sink 发送 TTS
    /// - 通过 turn_tracker 添加 assistant 消息
    /// - 被打断时调用 turn_tracker::interrupt_turn
    async fn run(&self, ctx: AgentContext, handles: AgentHandles<'_>) -> anyhow::Result<()>;
}

#[derive(Clone)]
pub struct AgentCancellationToken {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl Default for AgentCancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentCancellationToken {
    pub fn new() -> Self {
        Self { cancelled: Arc::new(AtomicBool::new(false)), notify: Arc::new(Notify::new()) }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters(); // 唤醒所有等待打断的 future
    }
}

/// 构建英文格式的时间前缀（用于注入到 system prompt 最前面）
///
/// 格式: [System Time: YYYY-MM-DD HH:MM:SS Monday, Timezone: +0800]
pub fn build_time_prefix(datetime: &str, timezone: &str, weekday: &str) -> String {
    // 将中文星期转换为英文
    let weekday_en = match weekday {
        "星期一" => "Monday",
        "星期二" => "Tuesday",
        "星期三" => "Wednesday",
        "星期四" => "Thursday",
        "星期五" => "Friday",
        "星期六" => "Saturday",
        "星期日" | "星期天" => "Sunday",
        _ => weekday, // 如果已经是英文或其他格式，保持不变
    };

    // 从 datetime 中提取日期和时间部分（移除中文星期）
    // datetime 格式: "2026-01-26 21:49:23 星期一"
    let datetime_parts: Vec<&str> = datetime.split_whitespace().collect();
    let date_time_only = if datetime_parts.len() >= 2 {
        format!("{} {}", datetime_parts[0], datetime_parts[1])
    } else {
        datetime.to_string()
    };

    format!("[System Time: {} {}, Timezone: {}]\n\n", date_time_only, weekday_en, timezone)
}

/// 根据 AgentContext 构建统一的结构化用户信息块（同步版本）
///
/// 注意：此函数不包含 USER_LOCATION，仅用于兼容。
/// 新代码应使用 `build_user_structured_block_async` 以正确处理 device_code。
pub fn build_user_structured_block(ctx: &AgentContext) -> String {
    let mut result = String::new();

    // 添加 wiki 知识库上下文（如果存在）
    if let Some(wiki) = &ctx.wiki_context {
        result.push_str(&format!(
            "\n\n<wiki_context>\n<title>{}</title>\n<content>{}</content>\n</wiki_context>",
            wiki.title, wiki.content
        ));
    }

    result
}

/// 异步版本：根据 AgentContext 构建统一的结构化用户信息块
///
/// - 7720 设备：仅包含 wiki_context（不注入 USER_LOCATION 和 CURRENT_WEEKDAY）
/// - 其他设备：包含完整的 User Information 块（与 turn_tracker 格式一致）
pub async fn build_user_structured_block_async(ctx: &AgentContext) -> String {
    let mut result = String::new();

    // 检查是否需要跳过 location 注入（从 turn_tracker 获取 device_code）
    let skip_location = {
        let tracker = super::turn_tracker::get_or_create_tracker(&ctx.session_id).await;
        let guard = tracker.read().await;
        guard.should_skip_location_injection()
    };

    // 对于非 7720 设备，添加完整的 User Information 块
    if !skip_location {
        // 从 IP 获取时区和位置信息（与 turn_tracker 一致）
        let (second_level_time, timezone_offset, location_info) = crate::llm::llm::get_timezone_and_location_info_from_ip(&ctx.session_id).await;

        // 解析时间组件
        let mut parts = second_level_time.split_whitespace();
        let current_date = parts.next().unwrap_or("");
        let current_time_only = parts.next().unwrap_or("");
        let current_weekday = parts.next().unwrap_or("");

        // 构建与 turn_tracker 一致的 User Information 块
        result.push_str(&format!(
            "\n\nUser Information:\n    USER_LOCATION: {}\n    CURRENT_DATE: {}\n    CURRENT_DATETIME: {}\n    CURRENT_TIME: {}\n    CURRENT_TIMEZONE: {}\n    CURRENT_WEEKDAY: {}",
            location_info, current_date, second_level_time, current_time_only, timezone_offset, current_weekday
        ));
    }

    // 添加 wiki 知识库上下文（如果存在）
    if let Some(wiki) = &ctx.wiki_context {
        result.push_str(&format!(
            "\n\n<wiki_context>\n<title>{}</title>\n<content>{}</content>\n</wiki_context>",
            wiki.title, wiki.content
        ));
    }

    result
}

/// 为专用 Agent 构建完整的 system prompt
///
/// 将时间前缀放在最前面，然后是 role_prompt，再拼接 agent 特定的 base_prompt，
/// 最后附加 structured_block（用户信息）和通用输出格式约束。
///
/// # Arguments
/// * `role_prompt` - 从外部 system_prompt 提取的 `<role>` 部分
/// * `base_prompt` - Agent 特定的 prompt（来自 SystemPromptRegistry）
/// * `structured_block` - 用户结构化信息（位置、时间等）
/// * `ctx` - AgentContext，用于获取时间信息
///
/// # Returns
/// 完整的 system prompt 字符串
pub fn build_agent_system_prompt(role_prompt: Option<&str>, base_prompt: &str, structured_block: &str) -> String {
    build_agent_system_prompt_with_time(role_prompt, base_prompt, structured_block, None, None)
}

/// 为专用 Agent 构建完整的 system prompt（带时间信息）
///
/// 注意：此函数总是使用 time_prefix 格式。
/// 新代码应使用 `build_agent_system_prompt_with_time_async` 以正确处理 device_code。
pub fn build_agent_system_prompt_with_time(role_prompt: Option<&str>, base_prompt: &str, structured_block: &str, user_now: Option<&str>, timezone_offset: Option<&str>) -> String {
    // 通用输出格式约束
    const OUTPUT_FORMAT_CONSTRAINT: &str = "\n\n<output_format>纯文本输出,严禁使用任何markdown格式</output_format>";

    // 构建时间前缀
    let time_prefix = if let (Some(datetime), Some(tz)) = (user_now, timezone_offset) {
        // 解析时间组件
        let parts: Vec<&str> = datetime.split_whitespace().collect();
        let weekday = parts.get(2).unwrap_or(&"");
        build_time_prefix(datetime, tz, weekday)
    } else {
        String::new()
    };

    match role_prompt {
        Some(role) if !role.trim().is_empty() => {
            format!(
                "{}{}\n\n{}{}{}",
                time_prefix, role, base_prompt, structured_block, OUTPUT_FORMAT_CONSTRAINT
            )
        },
        _ => {
            format!("{}{}{}{}", time_prefix, base_prompt, structured_block, OUTPUT_FORMAT_CONSTRAINT)
        },
    }
}

/// 异步版本：为专用 Agent 构建完整的 system prompt（根据 device_code 决定格式）
///
/// - 7720 设备：使用 time_prefix 在前面
/// - 其他设备：不使用 time_prefix（USER_LOCATION 已在 structured_block 中）
pub async fn build_agent_system_prompt_with_time_async(
    session_id: &str,
    role_prompt: Option<&str>,
    base_prompt: &str,
    structured_block: &str,
    user_now: Option<&str>,
    timezone_offset: Option<&str>,
) -> String {
    // 通用输出格式约束
    const OUTPUT_FORMAT_CONSTRAINT: &str = "\n\n<output_format>纯文本输出,严禁使用任何markdown格式</output_format>";

    // 检查是否需要跳过 location 注入（从 turn_tracker 获取 device_code）
    let skip_location = {
        let tracker = super::turn_tracker::get_or_create_tracker(session_id).await;
        let guard = tracker.read().await;
        guard.should_skip_location_injection()
    };

    // 7720 设备：使用 time_prefix
    // 其他设备：不使用 time_prefix（因为 structured_block 中已有 USER_LOCATION）
    let time_prefix = if skip_location {
        if let (Some(datetime), Some(tz)) = (user_now, timezone_offset) {
            let parts: Vec<&str> = datetime.split_whitespace().collect();
            let weekday = parts.get(2).unwrap_or(&"");
            build_time_prefix(datetime, tz, weekday)
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    match role_prompt {
        Some(role) if !role.trim().is_empty() => {
            format!(
                "{}{}\n\n{}{}{}",
                time_prefix, role, base_prompt, structured_block, OUTPUT_FORMAT_CONSTRAINT
            )
        },
        _ => {
            format!("{}{}{}{}", time_prefix, base_prompt, structured_block, OUTPUT_FORMAT_CONSTRAINT)
        },
    }
}

#[async_trait]
impl AgentCancelToken for AgentCancellationToken {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    async fn cancelled(&self) {
        // 如果已取消，立即返回
        if self.is_cancelled() {
            return;
        }
        // 否则等待 notify 信号
        self.notify.notified().await;
    }
}

#[async_trait]
pub trait AgentToolClient: Send + Sync {
    async fn call(&self, name: &str, args_json: &str) -> anyhow::Result<ToolCallOutcome>;
}

#[async_trait]
pub trait AgentTtsSink: Send + Sync {
    async fn send(&self, text: &str);
}

/// 打断令牌 trait，支持同步检查和异步等待
#[async_trait]
pub trait AgentCancelToken: Send + Sync {
    /// 同步检查是否已取消
    fn is_cancelled(&self) -> bool;

    /// 异步等待取消信号（用于 tokio::select! 实现真正的 next-token 打断）
    async fn cancelled(&self);
}

#[derive(Debug, Clone)]
pub struct ToolCallOutcome {
    pub result: CallResult,
    pub control: ToolControl,
    pub context_text: String,
}

#[derive(Debug, Clone)]
pub enum ToolControl {
    Continue,
    Respond(String),
    Stop,
    /// 工具调用被打断，agent 应返回 interrupted 状态
    Interrupted,
}

#[derive(Default)]
pub struct AgentRegistry {
    agents: FxHashMap<String, Arc<dyn Agent>>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self { agents: FxHashMap::default() }
    }

    /// Register an agent with multiple intent keys.
    pub fn register_with_intents(&mut self, agent: Arc<dyn Agent>, intents: &[&str]) {
        for intent in intents {
            self.agents.insert(intent.to_string(), agent.clone());
        }
    }

    /// 确保 FallbackAgent 已注册（系统启动时调用）
    /// 这是 LlmTaskV2 架构的关键保障，确保意图识别始终有兜底
    pub fn ensure_fallback_agent(&mut self, fallback_agent: Arc<dyn Agent>) {
        if !self.agents.contains_key("agent.fallback") {
            self.agents.insert("agent.fallback".to_string(), fallback_agent);
        }
    }

    /// 根据 intent 查找对应的 agent，确保始终返回结果
    /// 这是 LlmTaskV2 的标准入口点，体现了意图识别的必选特性
    ///
    /// 查找顺序：
    /// 1. 精确匹配 intent
    /// 2. 前缀匹配（支持 intent 子类）
    /// 3. 兜底返回 FallbackAgent
    pub fn get_agent(&self, intent: Option<&str>) -> Arc<dyn Agent> {
        // 1. 尝试精确匹配意图
        if let Some(intent) = intent {
            if let Some(agent) = self.agents.get(intent) {
                return agent.clone();
            }

            // 2. 尝试前缀匹配
            for (key, agent) in &self.agents {
                if intent.starts_with(key) {
                    return agent.clone();
                }
            }
        }

        // 3. 兜底：必须返回 FallbackAgent
        // 这确保了意图识别是 LlmTaskV2 的必选特性
        self.agents
            .get("agent.fallback")
            .expect("FallbackAgent must be registered in AgentRegistry")
            .clone()
    }
}

pub struct BroadcastAgentTtsSink {
    tts_tx: Weak<broadcast::Sender<(TurnContext, String)>>,
    _tts_guard: Arc<broadcast::Sender<(TurnContext, String)>>,
    ctx: TurnContext,
    shared_flags: Arc<SharedFlags>,
}

impl BroadcastAgentTtsSink {
    pub fn new(tts_tx_guard: Arc<broadcast::Sender<(TurnContext, String)>>, ctx: TurnContext, shared_flags: Arc<SharedFlags>) -> Self {
        let weak = Arc::downgrade(&tts_tx_guard);
        Self { tts_tx: weak, _tts_guard: tts_tx_guard, ctx, shared_flags }
    }
}

#[async_trait]
impl AgentTtsSink for BroadcastAgentTtsSink {
    async fn send(&self, text: &str) {
        if let Some(tts_tx) = self.tts_tx.upgrade() {
            // 使用统一的 TTS 前置过滤器（函数调用泄露过滤 + markdown 引用过滤 + 繁简转换）
            let filtered = text_filters::filter_for_tts(text, *self.shared_flags.tts_chinese_convert_mode.read().unwrap());
            let _ = tts_tx.send((self.ctx.clone(), filtered));
        } else {
            warn!("⚠️ Agent TTS channel dropped before send; skipping chunk.");
        }
    }
}

pub struct RuntimeToolClient {
    emitter: Weak<EventEmitter>,
    mcp_clients: Arc<Vec<McpClientWrapper>>,
    session_id: String,
    ctx: TurnContext,
    tool_call_manager: Arc<ToolCallManager>,
    /// 打断令牌，用于在等待客户端工具结果时检测打断
    cancel_token: AgentCancellationToken,
    /// ASR 识别的语言，用于注入天气等工具的 language 参数
    asr_language: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum ToolSource {
    Builtin,
    Mcp,
    Client,
}

impl ToolSource {
    fn label(self) -> &'static str {
        match self {
            ToolSource::Builtin => "builtin",
            ToolSource::Mcp => "mcp",
            ToolSource::Client => "client",
        }
    }
}

struct ToolDispatchResult {
    text: String,
    result: CallResult,
    control: ToolControl,
    source: ToolSource,
}

#[async_trait]
impl AgentToolClient for RuntimeToolClient {
    async fn call(&self, name: &str, args_json: &str) -> anyhow::Result<ToolCallOutcome> {
        // 对天气工具注入 asr_language -> weather language 映射
        let args_json = if let Some(asr_lang) = self
            .asr_language
            .as_ref()
            .filter(|_| name == "get_weather" || name == "query_weather")
        {
            if let Ok(mut obj) = serde_json::from_str::<JsonValue>(args_json) {
                if let Some(map) = obj.as_object_mut() {
                    let weather_lang = asr_lang.split('-').next().unwrap_or("en");
                    map.insert("language".to_string(), JsonValue::String(weather_lang.to_string()));
                    trace!(
                        "🌐 注入天气工具 language 参数: asr_language={} -> weather_lang={}",
                        asr_lang, weather_lang
                    );
                }
                serde_json::to_string(&obj).unwrap_or_else(|_| args_json.to_string())
            } else {
                args_json.to_string()
            }
        } else {
            args_json.to_string()
        };
        let args_json = args_json.as_str();

        let emitter = self
            .emitter
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("event emitter has been dropped"))?;
        let tool_call_id = format!("agent_{}_{}", name, nanoid!(6));
        emitter
            .response_function_call_arguments_done(&self.ctx, &tool_call_id, name, args_json)
            .await;

        // 🎯 中心化：记录工具调用开始
        super::turn_tracker::record_tool_call(&self.session_id, &tool_call_id, name, args_json).await;

        // 查询工具来源注册表，确定调用路径
        let registered_source = crate::mcp::get_tool_source(&self.session_id, name).await;
        let sources = self.pipeline_for(name, &registered_source);

        trace!(
            "🔧 工具 {} 调用路由: registered={:?}, pipeline={:?}",
            name,
            registered_source,
            sources.iter().map(|s| s.label()).collect::<Vec<_>>()
        );

        let mut last_error: Option<anyhow::Error> = None;
        for source in sources {
            match self.invoke_source(source, name, args_json, &tool_call_id).await {
                Ok(Some(dispatch)) => {
                    let ToolDispatchResult { text, result, control, source } = dispatch;
                    trace!("Tool '{}' handled via {} source (control={:?})", name, source.label(), control);
                    emitter
                        .response_function_call_result_done(&self.ctx, &tool_call_id, &text)
                        .await;

                    // 🎯 中心化：记录工具调用完成
                    let tracker_control_mode = match &control {
                        ToolControl::Continue => super::turn_tracker::ToolControlMode::Llm,
                        ToolControl::Stop => super::turn_tracker::ToolControlMode::Stop,
                        ToolControl::Respond(tts_text) => {
                            // TTS 模式：工具直接决定回复内容
                            super::turn_tracker::complete_tool_call(
                                &self.session_id,
                                &tool_call_id,
                                name,
                                &text,
                                super::turn_tracker::ToolControlMode::Tts,
                                Some(tts_text.clone()),
                                matches!(result, CallResult::Success(_)),
                            )
                            .await;
                            return Ok(ToolCallOutcome { result, control, context_text: text });
                        },
                        ToolControl::Interrupted => super::turn_tracker::ToolControlMode::Stop,
                    };
                    super::turn_tracker::complete_tool_call(
                        &self.session_id,
                        &tool_call_id,
                        name,
                        &text,
                        tracker_control_mode,
                        None,
                        matches!(result, CallResult::Success(_)),
                    )
                    .await;

                    return Ok(ToolCallOutcome { result, control, context_text: text });
                },
                Ok(None) => continue,
                Err(e) => {
                    trace!("Tool '{}' invocation via {} failed: {}", name, source.label(), e);
                    last_error = Some(e);
                },
            }
        }

        let fallback_text = if let Some(err) = last_error.as_ref() {
            format!("Tool {} invocation failed: {}", name, err)
        } else {
            format!("Tool {} handled by agent internally", name)
        };
        emitter
            .response_function_call_result_done(&self.ctx, &tool_call_id, &fallback_text)
            .await;

        // 🎯 中心化：记录工具调用失败
        super::turn_tracker::complete_tool_call(
            &self.session_id,
            &tool_call_id,
            name,
            &fallback_text,
            super::turn_tracker::ToolControlMode::Llm,
            None,
            false,
        )
        .await;

        let fallback_result = if last_error.is_some() {
            CallResult::Error("tool_invocation_failed".into())
        } else {
            CallResult::Success(serde_json::json!({ "text": "handled by agent" }))
        };

        Ok(ToolCallOutcome {
            result: fallback_result,
            control: ToolControl::Continue,
            context_text: fallback_text,
        })
    }
}

impl RuntimeToolClient {
    pub fn new(
        emitter: Weak<EventEmitter>,
        mcp_clients: Arc<Vec<McpClientWrapper>>,
        session_id: String,
        ctx: TurnContext,
        tool_call_manager: Arc<ToolCallManager>,
        cancel_token: AgentCancellationToken,
        asr_language: Option<String>,
    ) -> Self {
        Self {
            emitter,
            mcp_clients,
            session_id,
            ctx,
            tool_call_manager,
            cancel_token,
            asr_language,
        }
    }

    fn pipeline_for(&self, name: &str, registered_source: &Option<crate::mcp::ToolSourceType>) -> Vec<ToolSource> {
        // 内置工具优先
        if crate::function_callback::is_builtin_tool(name) {
            return vec![ToolSource::Builtin];
        }

        // 根据注册来源决定调用路径
        match registered_source {
            Some(crate::mcp::ToolSourceType::HttpMcp(_)) | Some(crate::mcp::ToolSourceType::WsMcp(_)) => {
                // 工具来自 MCP，只调用 MCP
                vec![ToolSource::Mcp]
            },
            Some(crate::mcp::ToolSourceType::ToolsEndpoint(_)) | Some(crate::mcp::ToolSourceType::PromptEndpoint) | Some(crate::mcp::ToolSourceType::Client) => {
                // 工具来自 tools_endpoint / prompt_endpoint 或客户端直传，只调用 Client
                vec![ToolSource::Client]
            },
            Some(crate::mcp::ToolSourceType::Builtin) => {
                vec![ToolSource::Builtin]
            },
            None => {
                // 未注册来源，回退到旧逻辑：先尝试 Client（因为可能是客户端直接提供的工具）
                // 避免盲目调用 MCP
                vec![ToolSource::Client]
            },
        }
    }

    async fn invoke_source(&self, source: ToolSource, name: &str, args_json: &str, tool_call_id: &str) -> anyhow::Result<Option<ToolDispatchResult>> {
        match source {
            ToolSource::Builtin => {
                let dispatch = self.run_builtin_tool(name, args_json).await?;
                Ok(Some(dispatch))
            },
            ToolSource::Mcp => Ok(self.try_call_mcp_tool(name, args_json, tool_call_id).await?),
            ToolSource::Client => Ok(self.try_call_client_tool(name, args_json, tool_call_id).await?),
        }
    }

    async fn run_builtin_tool(&self, name: &str, args_json: &str) -> anyhow::Result<ToolDispatchResult> {
        let params: FxHashMap<String, JsonValue> = serde_json::from_str(args_json).unwrap_or_default();
        let (text, call_result) = match crate::function_callback::handle_builtin_tool(name, &params).await {
            Ok(CallResult::Success(value)) => {
                let text = format_builtin_tool_result(name, &value);
                (text, CallResult::Success(value))
            },
            Ok(CallResult::Error(err)) => {
                let text = format!("Builtin tool {} execution failed: {}", name, err);
                (text.clone(), CallResult::Error(err))
            },
            Ok(CallResult::Async(task_id)) => {
                let text = format!("Builtin tool {} async task: {}", name, task_id);
                (text.clone(), CallResult::Async(task_id))
            },
            Err(e) => {
                let text = format!("Tool {} invocation failed: {}", name, e);
                (text.clone(), CallResult::Error(e.to_string()))
            },
        };
        Ok(ToolDispatchResult {
            text,
            result: call_result,
            control: ToolControl::Continue,
            source: ToolSource::Builtin,
        })
    }

    async fn try_call_mcp_tool(&self, name: &str, args_json: &str, tool_call_id: &str) -> anyhow::Result<Option<ToolDispatchResult>> {
        let args_value: Option<JsonValue> = serde_json::from_str(args_json).ok();
        for mcp_client in self.mcp_clients.iter() {
            match mcp_client {
                McpClientWrapper::Http { client, config } => {
                    if let Some(result) = self
                        .call_http_mcp_tool(client, config, name, args_value.clone(), tool_call_id)
                        .await?
                    {
                        return Ok(Some(ToolDispatchResult {
                            text: result.0,
                            result: result.1,
                            control: result.2,
                            source: ToolSource::Mcp,
                        }));
                    }
                },
                McpClientWrapper::WebSocket { manager, config } => {
                    if let Some(result) = self.call_ws_mcp_tool(manager, config, name, args_value.clone()).await? {
                        return Ok(Some(ToolDispatchResult {
                            text: result.0,
                            result: result.1,
                            control: ToolControl::Continue,
                            source: ToolSource::Mcp,
                        }));
                    }
                },
            }
        }
        Ok(None)
    }

    async fn call_http_mcp_tool(
        &self,
        client: &crate::mcp::HttpMcpClient,
        config: &crate::mcp::McpServerConfig,
        name: &str,
        args: Option<JsonValue>,
        tool_call_id: &str,
    ) -> anyhow::Result<Option<(String, CallResult, ToolControl)>> {
        let (user_ip, user_city) = (None, None);
        let forward_tc = crate::llm::llm::ToolCall {
            id: Some(tool_call_id.to_string()),
            call_type: Some("function".to_string()),
            index: None,
            function: crate::llm::llm::FunctionCall {
                name: Some(name.to_string()),
                arguments: Some(serde_json::to_string(&args).unwrap_or_default()),
            },
        };
        match client.call_tool(&self.session_id, "", &forward_tc, user_ip, user_city).await {
            Ok(resp) => {
                let payload = serde_json::to_string_pretty(&resp.payload).unwrap_or_else(|_| format!("{:?}", resp.payload));
                // MCP tts 模式优先使用 payload.tts_text 作为播报文本，避免把完整 JSON 读给用户
                let tts_text = resp.payload.get("tts_text").and_then(|v| v.as_str()).map(|s| s.to_string());
                // 解析 MCP 服务器返回的 control.mode
                let control = match resp.control.mode.as_str() {
                    "stop" => {
                        trace!("🛑 HTTP MCP 工具 {} 返回 control.mode=stop，Agent 将停止执行", name);
                        ToolControl::Stop
                    },
                    "tts" => {
                        trace!("🔊 HTTP MCP 工具 {} 返回 control.mode=tts，将直接朗读结果", name);
                        ToolControl::Respond(tts_text.clone().unwrap_or_else(|| payload.clone()))
                    },
                    other => {
                        trace!("➡️ HTTP MCP 工具 {} 返回 control.mode={}，继续 LLM 处理", name, other);
                        ToolControl::Continue
                    },
                };
                let context_text = match &control {
                    ToolControl::Respond(text) => text.clone(),
                    _ => payload.clone(),
                };
                Ok(Some((context_text, CallResult::Success(resp.payload.clone()), control)))
            },
            Err(e) => {
                error!("HTTP MCP {} invocation failed: {}", config.endpoint, e);
                Ok(Some((
                    format!("MCP tool execution failed: {}", e),
                    CallResult::Error(e.to_string()),
                    ToolControl::Continue,
                )))
            },
        }
    }

    async fn call_ws_mcp_tool(&self, manager: &Arc<McpManager>, config: &crate::mcp::McpServerConfig, name: &str, args: Option<JsonValue>) -> anyhow::Result<Option<(String, CallResult)>> {
        match manager.call_tool(config, &self.session_id, name, args).await {
            Ok(result) => {
                if result.is_error {
                    Ok(Some((
                        format!("MCP tool {} execution failed: {}", name, config.endpoint),
                        CallResult::Error("mcp_error".into()),
                    )))
                } else {
                    let text = result
                        .content
                        .iter()
                        .map(|c| match c {
                            McpContent::Text { text } => text.clone(),
                            McpContent::Image { mime_type, .. } => format!("[image {}]", mime_type),
                            McpContent::Resource { resource } => format!("[resource {}]", resource.uri),
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok(Some((text.clone(), CallResult::Success(serde_json::json!({ "text": text })))))
                }
            },
            Err(e) => {
                error!("WS MCP {} invocation failed: {}", config.endpoint, e);
                Ok(Some((
                    format!("MCP tool execution failed: {}", e),
                    CallResult::Error(e.to_string()),
                )))
            },
        }
    }

    async fn try_call_client_tool(&self, name: &str, args_json: &str, tool_call_id: &str) -> anyhow::Result<Option<ToolDispatchResult>> {
        let tool_call_manager = &self.tool_call_manager;
        tool_call_manager.add_pending_call(tool_call_id.to_string(), name.to_string(), args_json.to_string());

        let result_rx_opt = {
            let mut guard = tool_call_manager.result_rx.lock().unwrap();
            guard.take()
        };

        if let Some(mut rx) = result_rx_opt {
            let timeout_duration = std::time::Duration::from_secs(std::cmp::max(1u64, tool_call_manager.timeout_secs));
            let deadline = tokio::time::Instant::now() + timeout_duration;

            // 循环等待匹配的 call_id，丢弃过期的结果
            let dispatch = loop {
                tokio::select! {
                    biased; // 优先检查打断

                    // 打断分支
                    _ = self.cancel_token.cancelled() => {
                        trace!("🛑 客户端工具 {} 等待被打断: call_id={}", name, tool_call_id);
                        // 清理当前工具调用
                        tool_call_manager.remove_call(tool_call_id);
                        break Some(ToolDispatchResult {
                            text: "Tool invocation interrupted".to_string(),
                            result: CallResult::Error("interrupted".into()),
                            control: ToolControl::Interrupted,
                            source: ToolSource::Client,
                        });
                    }

                    // 超时分支
                    _ = tokio::time::sleep_until(deadline) => {
                        trace!("⏱️ 客户端工具 {} 调用超时: call_id={}", name, tool_call_id);
                        tool_call_manager.remove_call(tool_call_id);
                        break Some(ToolDispatchResult {
                            text: format!("Client tool {} invocation timeout", name),
                            result: CallResult::Error("client_timeout".into()),
                            control: ToolControl::Continue,
                            source: ToolSource::Client,
                        });
                    }

                    // 接收结果分支
                    recv_result = rx.recv() => {
                        match recv_result {
                            Some(res) => {
                                // 验证 call_id 是否匹配当前期望的工具调用
                                if res.call_id != tool_call_id {
                                    // 不是当前调用的结果，检查是否在 pending 列表中
                                    if !tool_call_manager.is_pending(&res.call_id) {
                                        // 过期的结果（可能是上一轮被打断的），丢弃并继续等待
                                        trace!(
                                            "🗑️ 丢弃过期的工具结果: expected={}, received={}, output_preview={}",
                                            tool_call_id,
                                            res.call_id,
                                            res.output.chars().take(50).collect::<String>()
                                        );
                                        continue;
                                    }
                                    // 是其他 pending 调用的结果，这种情况不应该发生（单次调用）
                                    // 但为了安全，也丢弃并继续
                                    trace!(
                                        "⚠️ 收到其他 pending 工具的结果: expected={}, received={}",
                                        tool_call_id,
                                        res.call_id
                                    );
                                    continue;
                                }

                                // call_id 匹配，正常处理
                                tool_call_manager.remove_call(tool_call_id);
                                let result = if res.is_error {
                                    CallResult::Error(res.output.clone())
                                } else {
                                    CallResult::Success(serde_json::json!({ "text": res.output.clone() }))
                                };
                                let control = match res.control_mode.as_deref().unwrap_or("llm") {
                                    "tts" => {
                                        let speak_text = res.tts_text.clone().unwrap_or(res.output.clone());
                                        ToolControl::Respond(speak_text)
                                    },
                                    "stop" => ToolControl::Stop,
                                    _ => ToolControl::Continue,
                                };
                                let context_text = res.tts_text.clone().unwrap_or(res.output.clone());
                                break Some(ToolDispatchResult { text: context_text, result, control, source: ToolSource::Client });
                            },
                            None => {
                                tool_call_manager.remove_call(tool_call_id);
                                break Some(ToolDispatchResult {
                                    text: format!("Client tool {} channel closed", name),
                                    result: CallResult::Error("client_channel_closed".into()),
                                    control: ToolControl::Continue,
                                    source: ToolSource::Client,
                                });
                            },
                        }
                    }
                }
            };

            // 归还 rx
            {
                let mut guard = tool_call_manager.result_rx.lock().unwrap();
                *guard = Some(rx);
            }

            return Ok(dispatch);
        }

        Ok(Some(ToolDispatchResult {
            text: format!("Client tool {} not supported", name),
            result: CallResult::Error("client_unsupported".into()),
            control: ToolControl::Continue,
            source: ToolSource::Client,
        }))
    }
}

fn format_builtin_tool_result(tool_name: &str, value: &serde_json::Value) -> String {
    match tool_name {
        "world_clock" => format_world_clock_result(value),
        "calculate" => format_math_result(value),
        "search_web" => format_search_result(value),
        // 同传工具的结果文本在 V2 流程中由上层处理，这里返回空字符串避免注入多余文本
        "start_simul_interpret" | "stop_simul_interpret" => String::new(),
        _ => serde_json::to_string_pretty(value).unwrap_or_else(|_| format!("Tool {} execution completed", tool_name)),
    }
}

fn format_math_result(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "Failed to format math result".to_string())
}

fn format_world_clock_result(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "Failed to format world clock result".to_string())
}

fn format_search_result(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "Failed to format search result".to_string())
}
