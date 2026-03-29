use super::tool_call_manager::ToolCallManager;
use crate::agents::media_agent::MediaAgentLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch};

/// 🆕 优化的助手响应上下文 - 使用原子操作减少锁定
#[derive(Debug)]
pub struct OptimizedAssistantResponseContext {
    /// 助手项目ID
    pub assistant_item_id: Arc<parking_lot::RwLock<String>>,
    /// 响应ID（只读）
    pub response_id: Arc<super::lockfree_response_id::LockfreeResponseIdReader>,
    /// 上下文是否有效
    pub is_valid: AtomicBool,
    /// 最后更新时间戳
    pub last_updated: AtomicU64,
}

impl Default for OptimizedAssistantResponseContext {
    fn default() -> Self {
        Self::new()
    }
}

impl OptimizedAssistantResponseContext {
    pub fn new() -> Self {
        Self {
            assistant_item_id: Arc::new(parking_lot::RwLock::new(format!("asst_{}", nanoid::nanoid!(6)))),
            response_id: Arc::new(super::lockfree_response_id::LockfreeResponseIdReader::from_writer(
                &super::lockfree_response_id::LockfreeResponseId::new(),
            )),
            is_valid: AtomicBool::new(false),
            last_updated: AtomicU64::new(0),
        }
    }

    /// 快速更新上下文（减少锁定时间）
    pub fn update_context(&self, assistant_item_id: String, _response_id: String) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // 使用scope锁定减少锁定时间
        {
            let mut assistant_id_guard = self.assistant_item_id.write();
            *assistant_id_guard = assistant_item_id;
        }
        // 注意：response_id 只能由 ASR Task 写入，这里不需要更新

        self.last_updated.store(now, Ordering::Release);
        self.is_valid.store(true, Ordering::Release);
    }

    /// 快速获取上下文副本（如果有效）
    pub fn get_context_copy(&self) -> Option<AssistantResponseContext> {
        if !self.is_valid.load(Ordering::Acquire) {
            return None;
        }

        let assistant_item_id = self.assistant_item_id.read().clone();
        let response_id = self.response_id.load();

        if assistant_item_id.is_empty() || response_id.is_none() {
            return None;
        }

        Some(AssistantResponseContext { assistant_item_id, response_id: response_id.unwrap() })
    }

    /// 清除上下文
    pub fn clear(&self) {
        self.is_valid.store(false, Ordering::Release);
    }

    /// 检查上下文是否有效
    pub fn is_valid(&self) -> bool {
        self.is_valid.load(Ordering::Acquire)
    }
}

/// Shared state and signals across pipeline tasks
#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct SharedFlags {
    /// 系统是否正在响应（TTS播放中）
    pub is_responding_tx: watch::Sender<bool>,
    pub is_responding_rx: watch::Receiver<bool>,
    /// 打断信号（用户打断时设置）
    pub interrupt_tx: watch::Sender<bool>,
    pub interrupt_rx: watch::Receiver<bool>,
    /// 🆕 优化的当前助手响应上下文信息（使用无锁结构）
    pub assistant_response_context: Arc<OptimizedAssistantResponseContext>,
    /// 工具调用管理器
    pub tool_call_manager: Arc<ToolCallManager>,
    /// 🆕 测试用的ASR到LLM通道 (可选，用于直接文本输入测试)
    pub asr_to_llm_tx: Arc<Mutex<Option<mpsc::UnboundedSender<(TurnContext, String)>>>>,
    /// 🆕 ASR 繁简转换模式配置（支持运行时更新）
    pub asr_chinese_convert_mode: Arc<std::sync::RwLock<crate::text_filters::ConvertMode>>,
    /// 🆕 TTS 繁简转换模式配置（支持运行时更新）
    pub tts_chinese_convert_mode: Arc<std::sync::RwLock<crate::text_filters::ConvertMode>>,
    /// 🆕 同声传译模式开关（开启后所有轮次使用同传代理）
    pub simul_interpret_enabled: Arc<AtomicBool>,
    /// 🆕 同声传译语言A（双向互译的第一种语言）
    pub simul_interpret_language_a: Arc<Mutex<String>>,
    /// 🆕 同声传译语言B（双向互译的第二种语言）
    pub simul_interpret_language_b: Arc<Mutex<String>>,
    /// 🆕 表情选择提示词（完整的 emoji 选择指令）
    pub emoji_prompt: Arc<Mutex<Option<String>>>,
    /// 🆕 ASR 引擎偏好（用于同传模式动态选择 ASR 引擎）
    pub preferred_asr_engine: Arc<Mutex<Option<String>>>,
    /// 🆕 ASR 引擎变更通知（用于同传模式动态切换 ASR 引擎）
    pub asr_engine_notify_tx: watch::Sender<Option<String>>,
    pub asr_engine_notify_rx: watch::Receiver<Option<String>>,
    /// 🆕 媒体 Agent 锁状态（支持多结果选择流程）
    pub media_agent_lock: Arc<Mutex<MediaAgentLock>>,
    /// 🆕 同声传译进入时的历史 turn 数量（用于退出时截断）
    pub simul_interpret_turn_start_count: Arc<AtomicUsize>,
}

impl Default for SharedFlags {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedFlags {
    pub fn new() -> Self {
        let (is_responding_tx, is_responding_rx) = watch::channel(false);
        let (interrupt_tx, interrupt_rx) = watch::channel(false);
        let (asr_engine_notify_tx, asr_engine_notify_rx) = watch::channel(None);
        Self {
            is_responding_tx,
            is_responding_rx,
            interrupt_tx,
            interrupt_rx,
            assistant_response_context: Arc::new(OptimizedAssistantResponseContext::new()),
            tool_call_manager: Arc::new(ToolCallManager::new(30)), // 30秒超时
            asr_to_llm_tx: Arc::new(Mutex::new(None)),
            asr_chinese_convert_mode: Arc::new(std::sync::RwLock::new(crate::text_filters::ConvertMode::None)),
            tts_chinese_convert_mode: Arc::new(std::sync::RwLock::new(crate::text_filters::ConvertMode::None)),
            simul_interpret_enabled: Arc::new(AtomicBool::new(false)),
            simul_interpret_language_a: Arc::new(Mutex::new("zh".to_string())),
            simul_interpret_language_b: Arc::new(Mutex::new("en".to_string())),
            emoji_prompt: Arc::new(Mutex::new(None)),
            preferred_asr_engine: Arc::new(Mutex::new(None)),
            asr_engine_notify_tx,
            asr_engine_notify_rx,
            media_agent_lock: Arc::new(Mutex::new(MediaAgentLock::new())),
            simul_interpret_turn_start_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// 重置连接断开时需要清理的临时状态
    /// 保留用户配置（asr_chinese_convert_mode, tts_chinese_convert_mode, emoji_prompt 等）
    pub fn reset_on_disconnect(&self) {
        // 重置同声传译状态
        let was_simul = self.simul_interpret_enabled.swap(false, Ordering::AcqRel);
        if was_simul {
            tracing::info!("🔄 SharedFlags: 重置同声传译模式");
        }

        // 重置同声传译语言为默认值
        {
            let mut a = self.simul_interpret_language_a.lock().unwrap();
            let mut b = self.simul_interpret_language_b.lock().unwrap();
            *a = "zh".to_string();
            *b = "en".to_string();
        }

        // 重置同声传译 turn 起始计数
        self.simul_interpret_turn_start_count.store(0, Ordering::Release);

        // 重置 ASR 引擎偏好
        {
            let mut pref = self.preferred_asr_engine.lock().unwrap();
            *pref = None;
        }

        // 重置响应状态
        let _ = self.is_responding_tx.send(false);
        let _ = self.interrupt_tx.send(false);

        // 清理助手响应上下文
        self.assistant_response_context.clear();

        // 重置媒体 Agent 锁
        {
            let mut lock = self.media_agent_lock.lock().unwrap();
            *lock = MediaAgentLock::new();
        }

        tracing::debug!("✅ SharedFlags: 连接断开状态重置完成");
    }
}

/// Context for the current AI response being generated
#[derive(Clone, Debug)]
pub struct AssistantResponseContext {
    pub assistant_item_id: String,
    pub response_id: String,
}

/// TurnContext - 每轮对话的上下文信息（简化版本）
#[derive(Clone, Debug)]
pub struct TurnContext {
    pub user_item_id: String,
    pub assistant_item_id: String,
    pub response_id: String,
    /// 🆕 轮次序列号（用于简化的打断机制）
    pub turn_sequence: Option<u64>,
}

impl TurnContext {
    pub fn new(user_item_id: String, assistant_item_id: String, response_id: String, turn_sequence: Option<u64>) -> Self {
        Self { user_item_id, assistant_item_id, response_id, turn_sequence }
    }
}

/// Used for tasks to signal their completion to the orchestrator
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskCompletion {
    Asr,
    Llm,
    Tts,
}

/// 同声传译分段配置
/// 使用"投票稳定化 + 多语言断句点"算法进行断句
#[derive(Debug, Clone)]
pub struct SimultaneousSegmentConfig {
    /// 是否启用同传断句
    pub enabled: bool,
    /// 稳定性阈值：连续 N 次 ASR 结果相同才算稳定
    /// 推荐值：2（延迟约 3s）或 3（延迟约 4.5s，更稳定）
    pub stability_threshold: usize,
    /// 最小稳定单位数：至少需要多少语义单位才发送
    /// 避免频繁发送很短的片段
    pub min_stable_units: usize,
    /// 弱断句字数阈值（语义单位：中文1字=1，英文1词=1）
    /// 只有稳定文本达到此阈值后，才会在弱断句点处断句
    pub max_units: u32,
}

impl Default for SimultaneousSegmentConfig {
    fn default() -> Self {
        Self { enabled: false, stability_threshold: 2, min_stable_units: 3, max_units: 15 }
    }
}

impl SimultaneousSegmentConfig {
    /// 创建启用的配置
    pub fn enabled_with_defaults() -> Self {
        Self { enabled: true, ..Default::default() }
    }
}
