//! Session Context - 中心化的会话上下文管理器
//!
//! 核心职责：
//! 1. 管理 Session 级别的配置和用户信息
//! 2. 按 Turn 记录用户的完整交互历史
//! 3. 提供 LLM 需要的 messages 格式
//! 4. 中心化记录工具调用及结果
//!
//! 生命周期：
//! - SessionContext: 整个会话（多轮对话）
//! - TurnRecord: 单轮交互
//!
//! Session 级别信息：
//! - system_prompt, role_prompt
//! - user_ip, user_city, user_timezone, asr_language

use chrono::{DateTime, Utc};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::llm::{ChatMessage, FunctionCall, ToolCall};
use crate::telemetry;

/// 工具调用的控制模式（决定后续流程）
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolControlMode {
    /// 继续 LLM 处理（默认）- 工具结果送回 LLM 生成回复
    #[default]
    Llm,
    /// 直接 TTS 输出 - 跳过 LLM，工具指定的文本作为回复
    Tts,
    /// 停止当前轮次 - 不生成回复
    Stop,
}

/// 轮次状态
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    #[default]
    InProgress,
    Completed,
    Interrupted,
}

/// 单轮交互记录
///
/// 每轮交互 = 用户输入 + 一系列 ChatMessage（assistant/tool）
/// messages 直接存储 ChatMessage，反映真实的对话流程
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    /// 轮次 ID
    pub turn_id: String,
    /// 用户 ASR 结果
    pub user_text: String,
    /// 意图识别结果（元数据）
    pub intent: Option<String>,
    /// 路由到的 Agent ID（元数据）
    pub agent_id: Option<String>,
    /// 本轮的消息列表（assistant/tool 消息，按时间顺序累积）
    pub messages: Vec<ChatMessage>,
    /// 轮次状态
    pub status: TurnStatus,
    /// 开始时间
    pub started_at: DateTime<Utc>,
    /// 结束时间
    pub ended_at: Option<DateTime<Utc>>,
    /// 待完成的工具调用（用于中间状态追踪，不序列化）
    #[serde(skip)]
    pending_tool_calls: FxHashMap<String, ToolCall>,
    /// 工具调用开始时间（用于计算持续时间，不序列化）
    #[serde(skip)]
    tool_call_start_times: FxHashMap<String, Instant>,
    /// 轮次开始的 Instant（用于计算持续时间，不序列化）
    #[serde(skip)]
    started_instant: Option<Instant>,
}

impl TurnRecord {
    pub fn new(turn_id: String, user_text: String) -> Self {
        Self {
            turn_id,
            user_text,
            intent: None,
            agent_id: None,
            messages: Vec::new(),
            status: TurnStatus::InProgress,
            started_at: Utc::now(),
            ended_at: None,
            pending_tool_calls: FxHashMap::default(),
            tool_call_start_times: FxHashMap::default(),
            started_instant: Some(Instant::now()),
        }
    }

    /// 获取轮次持续时间（毫秒）
    pub fn duration_ms(&self) -> u64 {
        self.started_instant.map(|i| i.elapsed().as_millis() as u64).unwrap_or(0)
    }

    pub fn set_intent(&mut self, intent: Option<String>) {
        self.intent = intent;
    }

    pub fn set_agent(&mut self, agent_id: &str) {
        self.agent_id = Some(agent_id.to_string());
    }

    /// 记录工具调用开始（LLM 返回 tool_calls 时调用）
    /// 此时不立即添加到 messages，等待工具执行完成后一起添加
    pub fn record_tool_call(&mut self, call_id: String, name: String, arguments: String) {
        let tool_call = ToolCall {
            id: Some(call_id.clone()),
            call_type: Some("function".to_string()),
            index: None,
            function: FunctionCall { name: Some(name), arguments: Some(arguments) },
        };
        self.pending_tool_calls.insert(call_id.clone(), tool_call);
        self.tool_call_start_times.insert(call_id, Instant::now());
    }

    /// 获取工具调用持续时间（毫秒）
    pub fn get_tool_call_duration_ms(&self, call_id: &str) -> u64 {
        self.tool_call_start_times
            .get(call_id)
            .map(|i| i.elapsed().as_millis() as u64)
            .unwrap_or(0)
    }

    /// 记录工具调用完成，返回控制模式供调用方判断后续流程
    ///
    /// 这会添加两条消息到 messages：
    /// 1. assistant 消息（带 tool_calls）
    /// 2. tool 结果消息
    ///
    /// 如果是 TTS 模式，还会添加第三条 assistant 消息作为最终回复
    pub fn complete_tool_call(&mut self, call_id: &str, result: String, control_mode: ToolControlMode, tts_text: Option<String>, _success: bool) -> Option<ToolControlMode> {
        // 清理工具调用计时
        self.tool_call_start_times.remove(call_id);

        // 找到对应的 pending tool call
        if let Some(tool_call) = self.pending_tool_calls.remove(call_id) {
            // 添加 assistant message（带 tool_calls）
            self.messages.push(ChatMessage {
                role: Some("assistant".to_string()),
                content: None,
                tool_call_id: None,
                tool_calls: Some(vec![tool_call]),
            });

            // 添加 tool 结果 message
            self.messages.push(ChatMessage {
                role: Some("tool".to_string()),
                content: Some(result),
                tool_call_id: Some(call_id.to_string()),
                tool_calls: None,
            });

            // 如果是 TTS 模式，直接添加 assistant 回复
            if control_mode == ToolControlMode::Tts {
                if let Some(text) = tts_text {
                    self.messages.push(ChatMessage {
                        role: Some("assistant".to_string()),
                        content: Some(text),
                        tool_call_id: None,
                        tool_calls: None,
                    });
                }
            }

            return Some(control_mode);
        }

        None
    }

    /// 追加 assistant 消息（最终回复或中间回复，无工具调用）
    pub fn push_assistant_message(&mut self, content: String) {
        if !content.trim().is_empty() {
            self.messages.push(ChatMessage {
                role: Some("assistant".to_string()),
                content: Some(content),
                tool_call_id: None,
                tool_calls: None,
            });
        }
    }

    /// 标记轮次完成
    pub fn complete(&mut self) {
        self.status = TurnStatus::Completed;
        self.ended_at = Some(Utc::now());
    }

    /// 标记轮次被打断
    pub fn mark_interrupted(&mut self) {
        self.status = TurnStatus::Interrupted;
        self.ended_at = Some(Utc::now());
        // 清空未完成的工具调用
        self.pending_tool_calls.clear();
    }

    /// 转换为上下文消息（包含 user 消息）
    pub fn to_context_messages(&self) -> Vec<ChatMessage> {
        let mut result = Vec::with_capacity(1 + self.messages.len());

        // 1. 用户消息
        result.push(ChatMessage {
            role: Some("user".to_string()),
            content: Some(self.user_text.clone()),
            tool_call_id: None,
            tool_calls: None,
        });

        // 2. 被打断的轮次不添加后续消息
        if self.status == TurnStatus::Interrupted {
            return result;
        }

        // 3. 追加本轮的所有消息（包括中间的工具调用消息）
        result.extend(self.messages.clone());

        result
    }

    /// 获取最后一条 assistant 消息的内容
    pub fn last_assistant_content(&self) -> Option<String> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role.as_deref() == Some("assistant") && m.content.is_some())
            .and_then(|m| m.content.clone())
    }
}

/// Session Context - 会话级别的上下文管理器
///
/// 包含 Session 级别的配置、用户信息和轮次历史
/// 替代原有的 SessionMetadata，统一管理所有 session 级别的信息
#[derive(Debug)]
pub struct SessionContext {
    // ========== Session 标识 ==========
    pub session_id: String,
    /// 所属的 connection_id（用于获取 ConnectionMetadata）
    pub connection_id: Option<String>,

    // ========== Session 级别配置 ==========
    pub system_prompt: Option<String>,
    /// 从 system_prompt 提取的 <role> 部分（缓存）
    pub role_prompt: Option<String>,
    /// 离线工具列表 - 用于拒绝逻辑判断
    pub offline_tools: Vec<String>,

    // ========== Session 级别用户信息 ==========
    pub user_ip: Option<String>,
    pub user_city: Option<String>,
    pub user_timezone: Option<String>,
    pub asr_language: Option<String>,
    /// 设备代码（用于判断是否跳过 location 注入等）
    pub device_code: Option<String>,

    // ========== 轮次历史 ==========
    turns: Vec<TurnRecord>,
    current_turn: Option<TurnRecord>,
    max_turns: usize,
}

/// 兼容性别名
pub type TurnTracker = SessionContext;

impl SessionContext {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            connection_id: None,
            system_prompt: None,
            role_prompt: None,
            offline_tools: Vec::new(),
            user_ip: None,
            user_city: None,
            user_timezone: None,
            asr_language: None,
            device_code: None,
            turns: Vec::new(),
            current_turn: None,
            max_turns: 10,
        }
    }

    /// 创建带 connection_id 的 SessionContext
    pub fn new_with_connection(session_id: String, connection_id: String) -> Self {
        Self {
            session_id,
            connection_id: Some(connection_id),
            system_prompt: None,
            role_prompt: None,
            offline_tools: Vec::new(),
            user_ip: None,
            user_city: None,
            user_timezone: None,
            asr_language: None,
            device_code: None,
            turns: Vec::new(),
            current_turn: None,
            max_turns: 10,
        }
    }

    /// 设置设备代码
    pub fn set_device_code(&mut self, device_code: Option<String>) {
        self.device_code = device_code;
    }

    /// 检查是否需要跳过 location 注入（deviceCode=7720, 8105, 7981, 7943）
    pub fn should_skip_location_injection(&self) -> bool {
        const SKIP_LOCATION_DEVICE_CODES: &[&str] = &["7720", "8105", "7981", "7943"];
        if let Some(ref code) = self.device_code {
            SKIP_LOCATION_DEVICE_CODES.contains(&code.as_str())
        } else {
            false
        }
    }

    /// 设置离线工具列表
    pub fn set_offline_tools(&mut self, tools: Vec<String>) {
        self.offline_tools = tools;
    }

    pub fn with_system_prompt(mut self, prompt: Option<String>) -> Self {
        self.set_system_prompt(prompt);
        self
    }

    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = max_turns;
        self
    }

    /// 设置 system_prompt 并自动提取 role_prompt
    ///
    /// 如果是本地化语言（vi/id/th/hi/es/fr/de/pt/it/ru/tr/uk/pl/nl/el/ro/cs/fi/ar/sv/no/da/af），
    /// 则从 assistant.rs 对应语言的提示词中提取 role，而不是从客户端提供的 prompt 中提取。
    pub fn set_system_prompt(&mut self, prompt: Option<String>) {
        self.system_prompt = prompt;
        self.refresh_role_prompt();
    }

    /// 设置 session 级别的用户信息
    ///
    /// 注意：设置 asr_language 后会重新计算 role_prompt，
    /// 因为本地化语言需要从 assistant.rs 中提取 role。
    pub fn set_user_info(&mut self, user_ip: Option<String>, user_city: Option<String>, user_timezone: Option<String>, asr_language: Option<String>) {
        self.user_ip = user_ip;
        self.user_city = user_city;
        self.user_timezone = user_timezone;

        // 如果语言发生变化，需要重新计算 role_prompt
        let language_changed = self.asr_language != asr_language;
        self.asr_language = asr_language;

        // 重新计算 role_prompt（如果语言变化且有 system_prompt）
        if language_changed {
            self.refresh_role_prompt();
        }
    }

    /// 根据当前语言重新计算 role_prompt
    fn refresh_role_prompt(&mut self) {
        // 需要服务端本地化的语言列表
        const LOCALIZED_LANGUAGES: &[&str] = &[
            "vi", "id", "th", "hi", "es", "fr", "de", "pt", "it", "ru", "tr", "uk", "pl", "nl", "el", "ro", "cs", "fi", "ar", "sv", "no", "da", "af",
        ];

        // 检查当前语言是否需要本地化
        let needs_localization = self
            .asr_language
            .as_ref()
            .map(|lang| {
                let normalized = crate::agents::SystemPromptRegistry::normalize_language(lang);
                LOCALIZED_LANGUAGES.contains(&normalized)
            })
            .unwrap_or(false);

        // 重新提取 role_prompt
        self.role_prompt = if needs_localization {
            // 本地化语言：从 assistant.rs 对应语言的提示词中提取 role
            let lang = self.asr_language.as_deref().unwrap_or("en");
            crate::agents::SystemPromptRegistry::global()
                .get("assistant", lang)
                .and_then(|assistant_prompt| crate::agents::role_extractor::extract_role_from_system_prompt(&assistant_prompt))
        } else {
            // 客户端语言（zh/en/ja/ko）：从客户端提供的 prompt 中提取 role
            self.system_prompt
                .as_ref()
                .and_then(|sp| crate::agents::role_extractor::extract_role_from_system_prompt(sp))
        };
    }

    /// 开始新的一轮交互
    pub fn start_turn(&mut self, turn_id: String, user_text: String) -> &mut TurnRecord {
        if let Some(mut prev) = self.current_turn.take() {
            if prev.status == TurnStatus::InProgress {
                prev.mark_interrupted();
            }
            self.archive_turn(prev);
        }
        self.current_turn = Some(TurnRecord::new(turn_id, user_text));
        self.current_turn.as_mut().unwrap()
    }

    fn archive_turn(&mut self, turn: TurnRecord) {
        self.turns.push(turn);
        if self.turns.len() > self.max_turns {
            let remove_count = self.turns.len() - self.max_turns;
            self.turns.drain(0..remove_count);
        }
    }

    pub fn current_turn_mut(&mut self) -> Option<&mut TurnRecord> {
        self.current_turn.as_mut()
    }

    pub fn current_turn(&self) -> Option<&TurnRecord> {
        self.current_turn.as_ref()
    }

    pub fn finish_turn(&mut self) {
        if let Some(mut turn) = self.current_turn.take() {
            turn.complete();
            self.archive_turn(turn);
        }
    }

    pub fn interrupt_turn(&mut self) {
        if let Some(mut turn) = self.current_turn.take() {
            turn.mark_interrupted();
            self.archive_turn(turn);
        }
    }

    pub fn turns(&self) -> &[TurnRecord] {
        &self.turns
    }

    /// 获取已归档的 turns 数量（不包括 current_turn）
    pub fn get_turns_count(&self) -> usize {
        self.turns.len()
    }

    /// 截断 turns 到指定数量，同时清除 current_turn
    /// 用于退出同声传译时清除同传期间的所有上下文
    pub fn truncate_turns_to(&mut self, count: usize) {
        self.turns.truncate(count);
        self.current_turn = None;
        debug!("🔄 TurnTracker: 截断 turns 到 {} 条，清除 current_turn", count);
    }

    /// 构建完整的 LLM messages（system + 历史 + 当前轮次的所有消息）
    ///
    /// 返回的消息格式符合 LLM API 要求：
    /// 1. system（如果有）
    /// 2. 历史轮次的 user/assistant/tool 消息
    /// 3. 当前轮次的 user + 已累积的 assistant/tool 消息
    pub fn build_messages(&self) -> Vec<ChatMessage> {
        let mut messages = Vec::new();

        // 1. System prompt
        if let Some(ref prompt) = self.system_prompt {
            if !prompt.trim().is_empty() {
                debug!("📋 构建消息: session={}, system_prompt长度={}", self.session_id, prompt.len());
                messages.push(ChatMessage {
                    role: Some("system".to_string()),
                    content: Some(prompt.clone()),
                    tool_call_id: None,
                    tool_calls: None,
                });
            } else {
                warn!("⚠️ 构建消息: session={}, system_prompt为空字符串", self.session_id);
            }
        } else {
            warn!("⚠️ 构建消息: session={}, system_prompt为None", self.session_id);
        }

        // 2. 历史轮次（完整的 user + assistant/tool 消息）
        for turn in &self.turns {
            messages.extend(turn.to_context_messages());
        }

        // 3. 当前轮次（user + 已累积的消息）
        if let Some(ref current) = self.current_turn {
            // 添加 user 消息
            messages.push(ChatMessage {
                role: Some("user".to_string()),
                content: Some(current.user_text.clone()),
                tool_call_id: None,
                tool_calls: None,
            });
            // 添加已累积的 assistant/tool 消息（工具调用循环中的中间消息）
            messages.extend(current.messages.clone());
        }

        messages
    }

    /// 构建历史 messages（不含 system prompt，用于意图识别）
    pub fn build_conversation_history(&self) -> Vec<ChatMessage> {
        let mut messages = Vec::new();

        for turn in &self.turns {
            messages.extend(turn.to_context_messages());
        }

        if let Some(ref current) = self.current_turn {
            messages.push(ChatMessage {
                role: Some("user".to_string()),
                content: Some(current.user_text.clone()),
                tool_call_id: None,
                tool_calls: None,
            });
            // 不包含当前轮次的中间消息（意图识别只需要历史）
        }

        messages
    }

    /// 构建 Agent 可用的 messages（不含 system，供 Agent 内部使用）
    ///
    /// 与 build_messages 的区别：不包含 system prompt，Agent 会自己添加
    pub fn build_agent_messages(&self) -> Vec<ChatMessage> {
        let mut messages = Vec::new();

        // 历史轮次
        for turn in &self.turns {
            messages.extend(turn.to_context_messages());
        }

        // 当前轮次
        if let Some(ref current) = self.current_turn {
            messages.push(ChatMessage {
                role: Some("user".to_string()),
                content: Some(current.user_text.clone()),
                tool_call_id: None,
                tool_calls: None,
            });
            // 包含已累积的中间消息
            messages.extend(current.messages.clone());
        }

        messages
    }

    pub fn clear(&mut self) {
        self.turns.clear();
        self.current_turn = None;
        info!("🧹 TurnTracker 已清空: session={}", self.session_id);
    }

    /// 构建 LLM 请求所需的完整 messages（含时间注入）
    ///
    /// 这是给 LLM 发送请求时使用的方法，会：
    /// 1. 在 system prompt 后附加用户时区/位置信息
    /// 2. 清理和格式化消息历史
    ///
    /// 注意：
    /// - deviceCode=7720, 8105, 7981, 7943 的设备：使用 time_prefix 在前面，不注入 USER_LOCATION 和 CURRENT_WEEKDAY
    /// - 其他设备：使用 User Information 块在后面（包含 USER_LOCATION 和 CURRENT_WEEKDAY）
    pub async fn build_llm_messages(&self) -> Vec<ChatMessage> {
        let raw_messages = self.build_messages();

        // 获取时区和位置信息
        let (second_level_time, timezone_offset, location_info) = crate::llm::llm::get_timezone_and_location_info_from_ip(&self.session_id).await;

        // 解析时间组件
        let mut parts = second_level_time.split_whitespace();
        let current_date = parts.next().unwrap_or("");
        let current_time_only = parts.next().unwrap_or("");
        let current_weekday = parts.next().unwrap_or("");

        // 检查是否需要跳过 location 注入（deviceCode=7720, 8105, 7981, 7943）
        let skip_location = self.should_skip_location_injection();

        let mut result = Vec::with_capacity(raw_messages.len());
        let mut first_system = true;

        for msg in raw_messages {
            if msg.role.as_deref() == Some("system") {
                let content = msg.content.clone().unwrap_or_default();
                // 跳过旧的时间信息块
                if content.starts_with("[系统时间:") || content.starts_with("User Information:") || content.starts_with("[Current Time:") || content.starts_with("[System Time:") {
                    continue;
                }
                // 注入时间/位置信息
                let final_content = if first_system {
                    first_system = false;
                    if skip_location {
                        // 7720, 8105, 7981, 7943 设备：time_prefix 在前面，不注入 USER_LOCATION 和 CURRENT_WEEKDAY
                        debug!("⏭️ deviceCode=7720/8105/7981/7943: 使用 time_prefix，跳过 USER_LOCATION 和 CURRENT_WEEKDAY");
                        let time_prefix = crate::agents::runtime::build_time_prefix(&second_level_time, &timezone_offset, current_weekday);
                        format!("{}{}", time_prefix, content)
                    } else {
                        // 其他设备：User Information 块在后面（原来的行为）
                        let structured_block = format!(
                            "\n\nUser Information:\n    USER_LOCATION: {}\n    CURRENT_DATE: {}\n    CURRENT_DATETIME: {}\n    CURRENT_TIME: {}\n    CURRENT_TIMEZONE: {}\n    CURRENT_WEEKDAY: {}",
                            location_info, current_date, second_level_time, current_time_only, timezone_offset, current_weekday
                        );
                        format!("{}{}", content, structured_block)
                    }
                } else {
                    content
                };
                result.push(ChatMessage {
                    role: Some("system".to_string()),
                    content: Some(final_content),
                    tool_call_id: None,
                    tool_calls: msg.tool_calls.clone(),
                });
            } else {
                // 对 tool_calls 进行清理
                let mut m = msg.clone();
                if let Some(tc) = m.tool_calls.take() {
                    m.tool_calls = Some(crate::llm::llm::sanitize_tool_calls(tc));
                }
                result.push(m);
            }
        }

        result
    }
}

// ============================================================================
// 全局 TurnTracker 管理
// ============================================================================

#[allow(clippy::type_complexity)]
static TURN_TRACKERS: std::sync::OnceLock<Arc<RwLock<FxHashMap<String, Arc<RwLock<TurnTracker>>>>>> = std::sync::OnceLock::new();

#[allow(clippy::type_complexity)]
fn get_global_trackers() -> &'static Arc<RwLock<FxHashMap<String, Arc<RwLock<TurnTracker>>>>> {
    TURN_TRACKERS.get_or_init(|| Arc::new(RwLock::new(FxHashMap::default())))
}

pub async fn get_or_create_tracker(session_id: &str) -> Arc<RwLock<TurnTracker>> {
    let trackers = get_global_trackers();

    {
        let read_guard = trackers.read().await;
        if let Some(tracker) = read_guard.get(session_id) {
            return tracker.clone();
        }
    }

    let mut write_guard = trackers.write().await;
    write_guard
        .entry(session_id.to_string())
        .or_insert_with(|| {
            debug!("📝 创建新的 TurnTracker: session={}", session_id);
            Arc::new(RwLock::new(TurnTracker::new(session_id.to_string())))
        })
        .clone()
}

pub async fn get_tracker(session_id: &str) -> Option<Arc<RwLock<TurnTracker>>> {
    let trackers = get_global_trackers();
    let read_guard = trackers.read().await;
    read_guard.get(session_id).cloned()
}

pub async fn remove_tracker(session_id: &str) {
    let trackers = get_global_trackers();
    let mut write_guard = trackers.write().await;
    if write_guard.remove(session_id).is_some() {
        debug!("🗑️ 移除 TurnTracker: session={}", session_id);
    }
}

pub async fn set_session_system_prompt(session_id: &str, prompt: Option<String>) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;
    guard.set_system_prompt(prompt);
}

/// 设置 session 级别的离线工具列表
pub async fn set_session_offline_tools(session_id: &str, tools: Vec<String>) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;
    guard.set_offline_tools(tools);
    debug!("🔧 设置离线工具: session={}, tools={:?}", session_id, guard.offline_tools);
}

/// 获取 session 级别的离线工具列表
pub async fn get_session_offline_tools(session_id: &str) -> Vec<String> {
    let tracker = get_or_create_tracker(session_id).await;
    let guard = tracker.read().await;
    guard.offline_tools.clone()
}

/// 设置 session 级别的设备代码（用于判断是否跳过 location 注入等）
pub async fn set_session_device_code(session_id: &str, device_code: Option<String>) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;
    if device_code.is_some() {
        debug!("🔧 设置设备代码: session={}, device_code={:?}", session_id, device_code);
    }
    guard.set_device_code(device_code);
}

/// 设置 session 级别的用户信息（首次连接时调用）
pub async fn set_session_user_info(session_id: &str, user_ip: Option<String>, user_city: Option<String>, user_timezone: Option<String>, asr_language: Option<String>) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;
    guard.set_user_info(user_ip, user_city, user_timezone, asr_language);
    debug!("👤 设置用户信息: session={}", session_id);
}

/// 创建带 connection_id 的 SessionContext
pub async fn create_session_with_connection(session_id: &str, connection_id: &str) {
    let trackers = get_global_trackers();
    let mut write_guard = trackers.write().await;
    write_guard.entry(session_id.to_string()).or_insert_with(|| {
        debug!(
            "📝 创建新的 SessionContext: session={}, connection={}",
            session_id, connection_id
        );
        Arc::new(RwLock::new(SessionContext::new_with_connection(
            session_id.to_string(),
            connection_id.to_string(),
        )))
    });
}

/// 默认最大轮次数
const DEFAULT_MAX_TURNS: usize = 20;

/// 默认系统提示词（后备值，可通过 DEFAULT_SYSTEM_PROMPT 环境变量覆盖）
fn default_system_prompt() -> String {
    std::env::var("DEFAULT_SYSTEM_PROMPT").unwrap_or_else(|_| "You are a helpful voice assistant. Keep responses concise and conversational. Reply in the same language as the user.".to_string())
}

/// 初始化会话（含从存储恢复）
///
/// 这是创建或恢复会话的主入口，会：
/// 1. 检查会话是否已存在
/// 2. 尝试从存储恢复历史
/// 3. 设置 system_prompt
pub async fn init_session(session_id: &str, system_prompt: Option<String>, store: &dyn crate::storage::ConversationStore) {
    // 检查是否已存在
    if let Some(tracker) = get_tracker(session_id).await {
        // 会话已存在，但如果传入了新的 system_prompt，需要更新
        if let Some(ref prompt) = system_prompt {
            let mut guard = tracker.write().await;
            guard.set_system_prompt(Some(prompt.clone()));
            info!("📝 会话 {} 已存在，更新 system_prompt，长度: {}", session_id, prompt.len());
        } else {
            debug!("📝 会话 {} 已存在，无新 prompt，跳过初始化", session_id);
        }
        return;
    }

    // 尝试从存储恢复
    let store_load_start = std::time::Instant::now();
    let final_prompt = match store.load(session_id).await {
        Ok(Some(record)) => {
            debug!(
                "📁 从存储恢复会话 {} 历史对话，消息数量: {} | ⏱️ store.load 耗时: {:?}",
                session_id,
                record.messages.len(),
                store_load_start.elapsed()
            );

            // 从存储配置中提取 system_prompt
            if let Some(ref sp) = system_prompt {
                info!("📝 使用新传入的 system_prompt，长度: {}", sp.len());
                system_prompt
            } else {
                let from_storage = record
                    .config
                    .get("system_prompt")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let restored = from_storage.or_else(|| Some(default_system_prompt()));

                if let Some(ref prompt) = restored {
                    info!(
                        "📝 从存储恢复 system_prompt，长度: {}, 来源: {}",
                        prompt.len(),
                        if record.config.get("system_prompt").is_some() {
                            "数据库"
                        } else {
                            "默认值"
                        }
                    );
                }

                restored
            }
        },
        Ok(None) => {
            debug!(
                "📝 创建新的会话上下文: {} | ⏱️ store.load 耗时: {:?}",
                session_id,
                store_load_start.elapsed()
            );
            system_prompt.or_else(|| Some(default_system_prompt()))
        },
        Err(e) => {
            tracing::warn!(
                "⚠️ 恢复会话 {} 失败: {}，创建新上下文 | ⏱️ store.load 耗时: {:?}",
                session_id,
                e,
                store_load_start.elapsed()
            );
            system_prompt.or_else(|| Some(default_system_prompt()))
        },
    };

    // 创建 SessionContext 并设置 prompt
    let tracker = get_or_create_tracker(session_id).await;
    {
        let mut guard = tracker.write().await;
        guard.set_system_prompt(final_prompt);
        guard.max_turns = DEFAULT_MAX_TURNS;
    }

    debug!("📝 为会话 {} 初始化上下文完成", session_id);
}

/// 检查会话是否存在
pub async fn has_session(session_id: &str) -> bool {
    get_tracker(session_id).await.is_some()
}

/// 更新用户城市（IP 解析或位置规范化后调用）
pub async fn set_user_city(session_id: &str, city: String) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;
    guard.user_city = Some(city);
}

/// 更新用户时区
pub async fn set_user_timezone(session_id: &str, timezone: String) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;
    guard.user_timezone = Some(timezone);
}

/// 更新 ASR 语言
pub async fn set_asr_language(session_id: &str, language: String) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;
    guard.asr_language = Some(language);
}

/// 获取 session 的用户信息（替代 get_session_metadata）
/// 返回：(user_timezone, user_city, asr_language, connection_id)
pub async fn get_session_info(session_id: &str) -> Option<(Option<String>, Option<String>, Option<String>, Option<String>)> {
    let tracker = get_tracker(session_id).await?;
    let guard = tracker.read().await;
    Some((
        guard.user_timezone.clone(),
        guard.user_city.clone(),
        guard.asr_language.clone(),
        guard.connection_id.clone(),
    ))
}

// ============================================================================
// 中心化操作函数
// ============================================================================

/// 开始新轮次
pub async fn start_turn(session_id: &str, turn_id: &str, user_text: &str) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;
    guard.start_turn(turn_id.to_string(), user_text.to_string());
    debug!("🎯 开始新轮次: session={}, turn={}", session_id, turn_id);

    // 异步追踪（不阻塞）
    telemetry::emit(telemetry::TraceEvent::TurnStart {
        session_id: session_id.to_string(),
        turn_id: turn_id.to_string(),
        user_text: user_text.to_string(),
        intent: None,
        agent_id: None,
    });
}

/// 设置意图识别结果
pub async fn set_intent(session_id: &str, intent: Option<String>) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;
    if let Some(turn) = guard.current_turn_mut() {
        turn.set_intent(intent.clone());
        debug!("🎯 设置意图: session={}, intent={:?}", session_id, intent);
    }
}

/// 设置 Agent 路由结果
pub async fn set_agent(session_id: &str, agent_id: &str) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;
    if let Some(turn) = guard.current_turn_mut() {
        turn.set_agent(agent_id);
        debug!("🤖 设置 Agent: session={}, agent={}", session_id, agent_id);
    }
}

/// 记录工具调用开始（中心化调用点：在 runtime.rs 调用）
pub async fn record_tool_call(session_id: &str, call_id: &str, name: &str, arguments: &str) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;
    let turn_id = guard.current_turn().map(|t| t.turn_id.clone());
    if let Some(turn) = guard.current_turn_mut() {
        turn.record_tool_call(call_id.to_string(), name.to_string(), arguments.to_string());
        debug!("🔧 记录工具调用: session={}, call_id={}, name={}", session_id, call_id, name);
    }

    // 异步追踪（不阻塞）
    if let Some(tid) = turn_id {
        telemetry::emit(telemetry::TraceEvent::ToolCallStart {
            session_id: session_id.to_string(),
            turn_id: tid,
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        });
    }
}

/// 完成工具调用记录（中心化调用点：在 runtime.rs 调用）
pub async fn complete_tool_call(
    session_id: &str,
    call_id: &str,
    name: &str, // 工具名（原子化上报需要）
    result: &str,
    control_mode: ToolControlMode,
    tts_text: Option<String>,
    success: bool,
) -> Option<ToolControlMode> {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;

    // 获取追踪所需信息
    let (turn_id, duration_ms) = guard
        .current_turn()
        .map(|t| (t.turn_id.clone(), t.get_tool_call_duration_ms(call_id)))
        .unwrap_or_default();

    if let Some(turn) = guard.current_turn_mut() {
        let mode = turn.complete_tool_call(call_id, result.to_string(), control_mode, tts_text, success);
        debug!(
            "✅ 完成工具调用: session={}, call_id={}, name={}, mode={:?}",
            session_id, call_id, name, mode
        );

        // 异步追踪（不阻塞）
        if !turn_id.is_empty() {
            telemetry::emit(telemetry::TraceEvent::ToolCallEnd {
                session_id: session_id.to_string(),
                turn_id,
                call_id: call_id.to_string(),
                name: name.to_string(),
                result: result.to_string(),
                success,
                duration_ms,
            });
        }

        return mode;
    }
    None
}

/// 添加 assistant 消息到当前轮次
///
/// 用于非 Agent 场景（如关键词触发的回复）
/// Agent 场景应在内部通过 TurnRecord::push_assistant_message 添加
pub async fn add_assistant_message(session_id: &str, content: &str) {
    if content.trim().is_empty() {
        return;
    }
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;
    if let Some(turn) = guard.current_turn_mut() {
        turn.push_assistant_message(content.to_string());
        debug!("📝 添加 assistant 消息: session={}", session_id);
    }
}

/// 完成当前轮次
pub async fn finish_turn(session_id: &str) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;

    // 先把 turn 信息 clone 出来（避免后面 await 时的借用冲突）
    let (turn_id, response, duration_ms, intent, agent_id) = match guard.current_turn() {
        Some(t) => (
            t.turn_id.clone(),
            t.last_assistant_content(),
            t.duration_ms(),
            t.intent.clone(),
            t.agent_id.clone(),
        ),
        None => {
            guard.finish_turn();
            debug!("✅ 完成轮次: session={}", session_id);
            return;
        },
    };

    // 历史上下文不单独上报：LLM post body 已经包含 messages（避免重复与增大 payload）
    let history_messages_json: Option<String> = None;

    guard.finish_turn();
    debug!("✅ 完成轮次: session={}", session_id);

    // 异步追踪（不阻塞）
    telemetry::emit(telemetry::TraceEvent::TurnEnd {
        session_id: session_id.to_string(),
        turn_id,
        assistant_response: response,
        duration_ms,
        status: "completed".to_string(),
        intent,
        agent_id,
        history_messages_json,
    });
}

/// 标记当前轮次被打断
pub async fn interrupt_turn(session_id: &str) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;

    let (turn_id, response, duration_ms, intent, agent_id) = match guard.current_turn() {
        Some(t) => (
            t.turn_id.clone(),
            t.last_assistant_content(),
            t.duration_ms(),
            t.intent.clone(),
            t.agent_id.clone(),
        ),
        None => {
            guard.interrupt_turn();
            debug!("🛑 轮次被打断: session={}", session_id);
            return;
        },
    };

    // 历史上下文不单独上报：LLM post body 已经包含 messages（避免重复与增大 payload）
    let history_messages_json: Option<String> = None;

    guard.interrupt_turn();
    debug!("🛑 轮次被打断: session={}", session_id);

    // 异步追踪（不阻塞）
    telemetry::emit(telemetry::TraceEvent::TurnEnd {
        session_id: session_id.to_string(),
        turn_id,
        assistant_response: response,
        duration_ms,
        status: "interrupted".to_string(),
        intent,
        agent_id,
        history_messages_json,
    });
}

/// 获取对话历史（不含 system prompt，用于意图识别）
pub async fn get_conversation_history(session_id: &str) -> Vec<ChatMessage> {
    let tracker = get_or_create_tracker(session_id).await;
    let guard = tracker.read().await;
    guard.build_conversation_history()
}

/// 获取对话历史（不含 system prompt，包含当前轮次的中间消息）
///
/// 供有自己 system prompt 的 Agent 使用
pub async fn get_history_messages(session_id: &str) -> Vec<ChatMessage> {
    let tracker = get_or_create_tracker(session_id).await;
    let guard = tracker.read().await;
    guard.build_agent_messages()
}

/// 清空会话历史
pub async fn clear_session(session_id: &str) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;
    guard.clear();
}

/// 获取已归档的 turns 数量（不包括 current_turn）
pub async fn get_turns_count(session_id: &str) -> usize {
    let tracker = get_or_create_tracker(session_id).await;
    let guard = tracker.read().await;
    guard.get_turns_count()
}

/// 截断 turns 到指定数量，同时清除 current_turn
/// 用于退出同声传译时清除同传期间的所有上下文
pub async fn truncate_turns_to(session_id: &str, count: usize) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;
    guard.truncate_turns_to(count);
}

/// 获取 LLM 请求所需的完整 messages（含时间注入）
///
/// 这是给 LLM 发送请求时使用的主要方法
pub async fn get_llm_messages(session_id: &str) -> Vec<ChatMessage> {
    let tracker = get_or_create_tracker(session_id).await;
    let guard = tracker.read().await;
    guard.build_llm_messages().await
}

/// 更新系统提示词
pub async fn update_system_prompt(session_id: &str, prompt: String) {
    let tracker = get_or_create_tracker(session_id).await;
    let mut guard = tracker.write().await;
    guard.set_system_prompt(Some(prompt));
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_conversation() {
        let mut turn = TurnRecord::new("turn_1".into(), "你好".into());
        turn.push_assistant_message("你好！有什么可以帮你？".into());
        turn.complete();

        let messages = turn.to_context_messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Some("user".into()));
        assert_eq!(messages[1].role, Some("assistant".into()));
    }

    #[test]
    fn test_tool_call_flow() {
        let mut turn = TurnRecord::new("turn_1".into(), "提醒我喝水".into());

        // 记录工具调用
        turn.record_tool_call("call_123".into(), "reminder".into(), r#"{"content":"喝水"}"#.into());

        // 完成工具调用
        turn.complete_tool_call(
            "call_123",
            r#"{"success":"已设置提醒"}"#.into(),
            ToolControlMode::Llm,
            None,
            true,
        );

        // 最终回复
        turn.push_assistant_message("好的,已为你设置提醒".into());
        turn.complete();

        let messages = turn.to_context_messages();
        // user + assistant(tool_calls) + tool + assistant(final)
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, Some("user".into()));
        assert_eq!(messages[1].role, Some("assistant".into()));
        assert!(messages[1].tool_calls.is_some());
        assert_eq!(messages[2].role, Some("tool".into()));
        assert_eq!(messages[3].role, Some("assistant".into()));
    }

    #[test]
    fn test_tts_mode_tool_call() {
        let mut turn = TurnRecord::new("turn_1".into(), "播放音乐".into());

        turn.record_tool_call("call_456".into(), "play_music".into(), r#"{"name":"歌曲"}"#.into());

        // TTS 模式：工具指定回复文本
        turn.complete_tool_call(
            "call_456",
            r#"{"status":"playing"}"#.into(),
            ToolControlMode::Tts,
            Some("正在播放歌曲".into()),
            true,
        );

        turn.complete();

        let messages = turn.to_context_messages();
        // user + assistant(tool_calls) + tool + assistant(tts_text)
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[3].content, Some("正在播放歌曲".into()));
    }

    #[test]
    fn test_multiple_tool_calls_in_loop() {
        let mut turn = TurnRecord::new("turn_1".into(), "查询天气并设置提醒".into());

        // 第一次工具调用
        turn.record_tool_call("call_1".into(), "weather".into(), "{}".into());
        turn.complete_tool_call("call_1", "晴天".into(), ToolControlMode::Llm, None, true);

        // 第二次工具调用
        turn.record_tool_call("call_2".into(), "reminder".into(), "{}".into());
        turn.complete_tool_call("call_2", "已设置".into(), ToolControlMode::Llm, None, true);

        // 最终回复
        turn.push_assistant_message("今天天气晴朗，已为你设置提醒".into());
        turn.complete();

        let messages = turn.to_context_messages();
        // user + (assistant+tool) + (assistant+tool) + assistant(final)
        assert_eq!(messages.len(), 6);
    }

    #[test]
    fn test_tracker_build_messages_with_current_turn() {
        let mut tracker = TurnTracker::new("test_session".into());
        tracker.set_system_prompt(Some("你是一个助手".into()));

        // 第一轮（完成）
        {
            let turn = tracker.start_turn("turn_1".into(), "你好".into());
            turn.push_assistant_message("你好！".into());
        }
        tracker.finish_turn();

        // 第二轮（当前进行中，有工具调用）
        {
            let turn = tracker.start_turn("turn_2".into(), "查天气".into());
            turn.record_tool_call("call_1".into(), "weather".into(), "{}".into());
            turn.complete_tool_call("call_1", "晴天".into(), ToolControlMode::Llm, None, true);
        }

        let messages = tracker.build_messages();
        // system + user1 + assistant1 + user2 + assistant(tool_calls) + tool
        assert_eq!(messages.len(), 6);
        assert_eq!(messages[0].role, Some("system".into()));
        assert_eq!(messages[1].content, Some("你好".into()));
        assert_eq!(messages[2].content, Some("你好！".into()));
        assert_eq!(messages[3].content, Some("查天气".into()));
        assert!(messages[4].tool_calls.is_some());
        assert_eq!(messages[5].role, Some("tool".into()));
    }

    #[test]
    fn test_interrupted_turn() {
        let mut turn = TurnRecord::new("turn_1".into(), "搜索".into());
        turn.push_assistant_message("正在搜索...".into());
        turn.mark_interrupted();

        let messages = turn.to_context_messages();
        // 被打断只保留 user 消息
        assert_eq!(messages.len(), 1);
    }
}
