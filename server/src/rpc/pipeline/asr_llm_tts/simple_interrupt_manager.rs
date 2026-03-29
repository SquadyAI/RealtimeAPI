//! 简化的全局打断管理器 - 无状态、事务性打断机制
//!
//! 核心设计理念：
//! 1. 全局广播，下游自过滤
//! 2. 使用轮次序列号代替response_id
//! 3. 无状态，避免复杂的状态同步
//! 4. 事务性，打断信号包含完整上下文

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

/// 🔧 新增：打断事件统计
#[derive(Debug, Clone)]
pub struct InterruptStats {
    /// 处理的打断事件总数
    pub total_events_processed: Arc<AtomicU64>,
    /// 忽略的打断事件总数
    pub total_events_ignored: Arc<AtomicU64>,
    /// UserSpeaking事件计数
    pub user_speaking_events: Arc<AtomicU64>,
    /// UserPtt事件计数
    pub user_ptt_events: Arc<AtomicU64>,
    /// 全局打断事件计数
    pub global_interrupt_events: Arc<AtomicU64>,
}

impl Default for InterruptStats {
    fn default() -> Self {
        Self::new()
    }
}

impl InterruptStats {
    pub fn new() -> Self {
        Self {
            total_events_processed: Arc::new(AtomicU64::new(0)),
            total_events_ignored: Arc::new(AtomicU64::new(0)),
            user_speaking_events: Arc::new(AtomicU64::new(0)),
            user_ptt_events: Arc::new(AtomicU64::new(0)),
            global_interrupt_events: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 记录处理的事件
    pub fn record_processed_event(&self, reason: &InterruptReason) {
        self.total_events_processed.fetch_add(1, Ordering::Relaxed);
        match reason {
            InterruptReason::UserSpeaking => {
                self.user_speaking_events.fetch_add(1, Ordering::Relaxed);
            },
            InterruptReason::UserPtt => {
                self.user_ptt_events.fetch_add(1, Ordering::Relaxed);
            },
            InterruptReason::SessionTimeout | InterruptReason::SystemShutdown | InterruptReason::ConnectionLost => {
                self.global_interrupt_events.fetch_add(1, Ordering::Relaxed);
            },
        }
    }

    /// 记录忽略的事件
    pub fn record_ignored_event(&self) {
        self.total_events_ignored.fetch_add(1, Ordering::Relaxed);
    }

    /// 获取统计摘要
    pub fn get_summary(&self) -> String {
        format!(
            "processed={}, ignored={}, user_speaking={}, user_ptt={}, global={}",
            self.total_events_processed.load(Ordering::Relaxed),
            self.total_events_ignored.load(Ordering::Relaxed),
            self.user_speaking_events.load(Ordering::Relaxed),
            self.user_ptt_events.load(Ordering::Relaxed),
            self.global_interrupt_events.load(Ordering::Relaxed)
        )
    }
}

/// 全局轮次序列号管理器
#[derive(Debug)]
pub struct TurnSequenceManager {
    current_sequence: AtomicU64,
}

impl Default for TurnSequenceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TurnSequenceManager {
    pub fn new() -> Self {
        Self { current_sequence: AtomicU64::new(0) }
    }

    /// 生成新的轮次序列号
    pub fn next_turn(&self) -> u64 {
        self.current_sequence.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// 获取当前轮次序列号
    pub fn current_turn(&self) -> u64 {
        self.current_sequence.load(Ordering::Acquire)
    }
}

/// 简化的打断事件
#[derive(Debug, Clone)]
pub struct SimpleInterruptEvent {
    /// 会话ID
    pub session_id: String,
    /// 打断原因
    pub reason: InterruptReason,
    /// 打断发生时的轮次序列号
    pub turn_sequence: u64,
    /// 事件ID（用于追踪和去重）
    pub event_id: String,
    /// 时间戳
    pub timestamp: std::time::SystemTime,
    /// 🆕 打断信号发送时的高精度时间戳（用于延迟测量）
    pub sent_at: std::time::Instant,
}

/// 打断原因（简化版）
#[derive(Debug, Clone, PartialEq)]
pub enum InterruptReason {
    /// 用户开始说话（VAD触发）
    UserSpeaking,
    /// 用户PTT按下
    UserPtt,
    /// 会话超时
    SessionTimeout,
    /// 系统关闭
    SystemShutdown,
    /// 连接断开
    ConnectionLost,
}

impl std::fmt::Display for InterruptReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InterruptReason::UserSpeaking => write!(f, "用户开始说话"),
            InterruptReason::UserPtt => write!(f, "用户PTT"),
            InterruptReason::SessionTimeout => write!(f, "会话超时"),
            InterruptReason::SystemShutdown => write!(f, "系统关闭"),
            InterruptReason::ConnectionLost => write!(f, "连接断开"),
        }
    }
}

/// 简化的全局打断管理器
#[derive(Clone)]
pub struct SimpleInterruptManager {
    /// 广播发送器
    tx: broadcast::Sender<SimpleInterruptEvent>,
    /// 轮次序列号管理器
    turn_manager: Arc<TurnSequenceManager>,
    /// 管理器ID
    id: String,
}

impl Default for SimpleInterruptManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SimpleInterruptManager {
    /// 创建新的简化打断管理器
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(64); // 减少缓冲区大小
        let id = nanoid::nanoid!(6);

        info!("🚀 创建简化打断管理器: id={}", id);

        Self { tx, turn_manager: Arc::new(TurnSequenceManager::new()), id }
    }

    /// 订阅打断事件（无过滤，下游自处理）
    pub fn subscribe(&self) -> broadcast::Receiver<SimpleInterruptEvent> {
        let rx = self.tx.subscribe();
        debug!("📡 新的打断事件订阅者: manager_id={}", self.id);
        rx
    }

    /// 广播全局打断事件
    pub fn broadcast_global_interrupt(&self, session_id: String, reason: InterruptReason) -> Result<(), String> {
        self.broadcast_global_interrupt_with_turn(session_id, reason, self.turn_manager.current_turn())
    }

    /// 广播全局打断事件（指定目标轮次）
    pub fn broadcast_global_interrupt_with_turn(&self, session_id: String, reason: InterruptReason, target_turn_sequence: u64) -> Result<(), String> {
        let event = SimpleInterruptEvent {
            session_id: session_id.clone(),
            reason: reason.clone(),
            turn_sequence: target_turn_sequence,
            event_id: nanoid::nanoid!(8),
            timestamp: std::time::SystemTime::now(),
            sent_at: std::time::Instant::now(),
        };

        info!(
            "📢 [{}] 广播全局打断: session={}, reason={}, turn={}, event_id={}",
            self.id, event.session_id, event.reason, event.turn_sequence, event.event_id
        );

        match self.tx.send(event.clone()) {
            Ok(receiver_count) => {
                info!("✅ 全局打断已广播到 {} 个接收者", receiver_count);
                Ok(())
            },
            Err(e) => Err(format!("广播失败: {:?}", e)),
        }
    }

    /// 开始新轮次（返回新的轮次序列号）
    pub fn start_new_turn(&self) -> u64 {
        let turn_id = self.turn_manager.next_turn();
        info!("🆕 开始新轮次: turn_sequence={}", turn_id);
        turn_id
    }

    /// 获取当前轮次序列号
    pub fn current_turn(&self) -> u64 {
        self.turn_manager.current_turn()
    }

    /// 获取订阅者数量
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// 🔧 新增：验证轮次序列号一致性（用于调试和监控）
    pub fn validate_turn_consistency(&self, component_name: &str, local_turn: u64) -> bool {
        let global_turn = self.current_turn();
        let is_consistent = local_turn == global_turn;

        if !is_consistent {
            warn!(
                "⚠️ 轮次序列号不一致: {} local={}, global={}",
                component_name, local_turn, global_turn
            );
        } else {
            debug!("✅ 轮次序列号一致: {} turn={}", component_name, local_turn);
        }

        is_consistent
    }
}

/// 简化的打断处理器 - 每个任务自己决定是否处理打断
pub struct SimpleInterruptHandler {
    session_id: String,
    component_name: String,
    receiver: broadcast::Receiver<SimpleInterruptEvent>,
    /// 当前任务关联的轮次序列号
    associated_turn: Option<u64>,
    /// 🆕 当前打断状态（用于 is_interrupted_immutable 检查）
    interrupted_state: Arc<AtomicBool>,
    /// 🔧 新增：打断事件统计
    interrupt_stats: InterruptStats,
    /// 🆕 同声传译模式：忽略 UserSpeaking 打断，只响应系统级打断
    ignore_user_speaking: bool,
}

impl Clone for SimpleInterruptHandler {
    fn clone(&self) -> Self {
        // 通过 resubscribe 获取新的独立 receiver（广播语义，每个订阅者都有自己的光标）
        // 保留相同的组件名与绑定轮次，并共享打断状态与统计，避免状态丢失的错觉
        Self {
            session_id: self.session_id.clone(),
            component_name: self.component_name.clone(),
            receiver: self.receiver.resubscribe(),
            associated_turn: self.associated_turn,
            interrupted_state: self.interrupted_state.clone(),
            interrupt_stats: self.interrupt_stats.clone(),
            ignore_user_speaking: self.ignore_user_speaking,
        }
    }
}

impl SimpleInterruptHandler {
    /// 创建新的简化打断处理器
    pub fn new(session_id: String, component_name: String, receiver: broadcast::Receiver<SimpleInterruptEvent>) -> Self {
        Self {
            session_id,
            component_name,
            receiver,
            associated_turn: None,
            interrupted_state: Arc::new(AtomicBool::new(false)),
            interrupt_stats: InterruptStats::new(),
            ignore_user_speaking: false,
        }
    }

    /// 创建忽略用户说话打断的处理器（用于同声传译模式）
    /// 只响应系统级打断（SessionTimeout, SystemShutdown, ConnectionLost）
    pub fn new_ignore_user_speaking(session_id: String, component_name: String, receiver: broadcast::Receiver<SimpleInterruptEvent>) -> Self {
        Self {
            session_id,
            component_name,
            receiver,
            associated_turn: None,
            interrupted_state: Arc::new(AtomicBool::new(false)),
            interrupt_stats: InterruptStats::new(),
            ignore_user_speaking: true,
        }
    }

    /// 🆕 创建新的接收器用于独立监听（适用于tokio::select!场景）
    pub fn subscribe(&self) -> broadcast::Receiver<SimpleInterruptEvent> {
        let new_receiver = self.receiver.resubscribe();
        debug!("📡 [{}] 创建新的打断事件接收器用于独立监听", self.component_name);
        new_receiver
    }

    /// 创建一个新的处理器，使用新的组件名但保留相同的 ignore_user_speaking 设置
    /// 用于需要独立订阅但保持相同打断行为的场景（如 PacedSender）
    pub fn derive_with_name(&self, new_component_name: String) -> Self {
        Self {
            session_id: self.session_id.clone(),
            component_name: new_component_name,
            receiver: self.receiver.resubscribe(),
            associated_turn: None,                               // 新的处理器不继承轮次绑定
            interrupted_state: Arc::new(AtomicBool::new(false)), // 独立的打断状态
            interrupt_stats: InterruptStats::new(),
            ignore_user_speaking: self.ignore_user_speaking, // 保留打断行为设置
        }
    }

    /// 绑定到特定轮次
    pub fn bind_to_turn(&mut self, turn_sequence: u64) {
        self.associated_turn = Some(turn_sequence);
        // 🔧 修复：绑定新轮次时不立即清除打断状态，让其自然过期
        // 避免与 PacedSender 处理打断信号产生竞争
        info!("🔗 [{}] 绑定到轮次: {}", self.component_name, turn_sequence);
    }

    /// 检查是否有打断事件（非阻塞）
    pub fn check_interrupt(&mut self) -> Option<SimpleInterruptEvent> {
        let mut found_relevant_interrupt = false;
        let mut relevant_event = None;

        // 处理所有可用事件，但不在循环中立即返回
        while let Ok(event) = self.receiver.try_recv() {
            if self.should_handle_event(&event) {
                // 🆕 计算打断延迟
                let processing_latency = event.sent_at.elapsed();
                let latency_ms = processing_latency.as_secs_f64() * 1000.0;

                info!(
                    "🛑 [{}] 收到相关打断事件: reason={}, turn={}, event_id={}, 延迟={:.2}ms",
                    self.component_name, event.reason, event.turn_sequence, event.event_id, latency_ms
                );

                // 🆕 如果延迟超过预期阈值，记录警告
                if latency_ms > 50.0 {
                    warn!(
                        "⚠️ [{}] 打断延迟超过50ms阈值: {:.2}ms, event_id={}",
                        self.component_name, latency_ms, event.event_id
                    );
                }

                found_relevant_interrupt = true;
                relevant_event = Some(event);
            // 不要在这里 break，继续处理剩余事件以避免阻塞
            } else {
                debug!(
                    "🔄 [{}] 忽略不相关打断: reason={}, turn={} (当前绑定: {:?})",
                    self.component_name, event.reason, event.turn_sequence, self.associated_turn
                );
                // 🔧 记录忽略的事件统计
                self.interrupt_stats.record_ignored_event();
            }
        }

        // 🔧 正确的pub-sub架构：每个subscriber独立接收事件副本
        // 不清除状态，因为其他subscriber也需要接收同一事件
        if found_relevant_interrupt {
            // 只设置本地状态，不影响其他subscriber（使用安全方法）
            self.set_interrupt_state_if_needed(true);
            // 🔧 记录处理的事件统计
            if let Some(ref event) = relevant_event {
                self.interrupt_stats.record_processed_event(&event.reason);
            }
        }

        // 🔧 定期报告统计信息
        self.log_stats_if_needed();

        relevant_event
    }

    /// 等待打断事件（阻塞）
    pub async fn wait_for_interrupt(&mut self) -> Option<SimpleInterruptEvent> {
        loop {
            match self.receiver.recv().await {
                Ok(event) => {
                    if self.should_handle_event(&event) {
                        // 🆕 计算打断延迟
                        let processing_latency = event.sent_at.elapsed();
                        let latency_ms = processing_latency.as_secs_f64() * 1000.0;

                        info!(
                            "🛑 [{}] 等待到相关打断事件: reason={}, turn={}, event_id={}, 延迟={:.2}ms",
                            self.component_name, event.reason, event.turn_sequence, event.event_id, latency_ms
                        );

                        // 🆕 如果延迟超过预期阈值，记录警告
                        if latency_ms > 50.0 {
                            warn!(
                                "⚠️ [{}] 打断延迟超过50ms阈值: {:.2}ms, event_id={}",
                                self.component_name, latency_ms, event.event_id
                            );
                        }

                        // 🔧 正确的pub-sub架构：设置本地状态，不影响其他subscriber（使用安全方法）
                        self.set_interrupt_state_if_needed(true);
                        // 🔧 记录处理的事件统计
                        self.interrupt_stats.record_processed_event(&event.reason);
                        return Some(event);
                    } else {
                        debug!(
                            "🔄 [{}] 等待中忽略不相关打断: reason={}, turn={}",
                            self.component_name, event.reason, event.turn_sequence
                        );
                        // 🔧 记录忽略的事件统计
                        self.interrupt_stats.record_ignored_event();
                        continue;
                    }
                },
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(
                        "⚠️ [{}] 打断事件接收滞后 {} 条消息 (总处理量={},总忽略={},当前轮次={})",
                        self.component_name,
                        n,
                        self.interrupt_stats.total_events_processed.load(Ordering::Relaxed),
                        self.interrupt_stats.total_events_ignored.load(Ordering::Relaxed),
                        self.associated_turn.unwrap_or(0)
                    );
                    continue;
                },
                Err(broadcast::error::RecvError::Closed) => {
                    debug!("📡 [{}] 打断事件通道已关闭", self.component_name);
                    return None;
                },
            }
        }
    }

    /// 判断是否应该处理此打断事件（下游自决策）
    fn should_handle_event(&self, event: &SimpleInterruptEvent) -> bool {
        // 1. 会话ID必须匹配
        if event.session_id != self.session_id {
            debug!(
                "🔄 [{}] 跳过不同会话的打断事件: event_session={}, my_session={}",
                self.component_name, event.session_id, self.session_id
            );
            return false;
        }

        // 🔧 新增：详细的事件处理追踪日志
        debug!(
            "🔍 [{}] 打断事件处理分析: event_id={}, reason={:?}, event_turn={}, bound_turn={:?}, session={}",
            self.component_name, event.event_id, event.reason, event.turn_sequence, self.associated_turn, self.session_id
        );

        // 2. 如果任务没有绑定轮次，处理全局打断和用户打断
        if self.associated_turn.is_none() {
            // 🆕 同声传译模式：忽略 UserSpeaking 和 UserPtt
            let should_handle = if self.ignore_user_speaking {
                matches!(
                    event.reason,
                    InterruptReason::SessionTimeout | InterruptReason::SystemShutdown | InterruptReason::ConnectionLost
                )
            } else {
                matches!(
                    event.reason,
                    InterruptReason::SessionTimeout | InterruptReason::SystemShutdown | InterruptReason::ConnectionLost | InterruptReason::UserSpeaking | InterruptReason::UserPtt
                )
            };
            info!(
                "🔄 [{}] 未绑定轮次，处理打断: reason={:?}, should_handle={}, ignore_user_speaking={}",
                self.component_name, event.reason, should_handle, self.ignore_user_speaking
            );
            return should_handle;
        }

        // 3. 如果任务绑定了轮次，根据打断类型决定
        // 使用 if let 确保安全，避免 unwrap 可能导致的 panic
        if let Some(bound_turn) = self.associated_turn {
            match event.reason {
                // 全局打断：总是处理
                InterruptReason::SessionTimeout | InterruptReason::SystemShutdown | InterruptReason::ConnectionLost => {
                    info!(
                        "🛑 [{}] 收到全局打断: reason={:?}, bound_turn={}, event_turn={}",
                        self.component_name, event.reason, bound_turn, event.turn_sequence
                    );
                    true
                },
                // 轮次相关打断：区分UserSpeaking和UserPtt
                InterruptReason::UserSpeaking => {
                    // 🆕 同声传译模式：忽略 UserSpeaking
                    if self.ignore_user_speaking {
                        info!(
                            "🔄 [{}] 同传模式忽略用户说话打断: reason={:?}, bound_turn={}, event_turn={}",
                            self.component_name, event.reason, bound_turn, event.turn_sequence
                        );
                        false
                    } else {
                        // 🔧 重要修复：UserSpeaking（用户说话）应该立即打断一切，无论轮次
                        // 因为这表示用户开始说话，必须立即停止当前播放
                        info!(
                            "🔄 [{}] 用户说话打断检查: reason={:?}, bound_turn={}, event_turn={}, should_interrupt=true (总是打断)",
                            self.component_name, event.reason, bound_turn, event.turn_sequence
                        );
                        true // UserSpeaking总是触发打断
                    }
                },
                InterruptReason::UserPtt => {
                    // 🆕 同声传译模式：忽略 UserPtt
                    if self.ignore_user_speaking {
                        info!(
                            "🔄 [{}] 同传模式忽略PTT打断: reason={:?}, bound_turn={}, event_turn={}",
                            self.component_name, event.reason, bound_turn, event.turn_sequence
                        );
                        false
                    } else {
                        // 🔧 统一修复：PTT只处理新轮次（>），避免同轮次自打断
                        // 这与LLM任务的逻辑保持一致，确保系统行为统一
                        let should_interrupt = event.turn_sequence > bound_turn;
                        info!(
                            "🔄 [{}] PTT打断检查: reason={:?}, bound_turn={}, event_turn={}, should_interrupt={}",
                            self.component_name, event.reason, bound_turn, event.turn_sequence, should_interrupt
                        );
                        should_interrupt
                    }
                },
            }
        } else {
            // 理论上不会到达这里，因为前面已经检查过 is_none()
            // 但为了代码安全性和完整性，添加一个后备处理
            warn!("⚠️ [{}] 意外情况：associated_turn 在检查后仍为 None", self.component_name);
            false
        }
    }

    /// 解绑轮次（任务完成时调用）
    pub fn unbind_turn(&mut self) {
        if let Some(turn) = self.associated_turn.take() {
            info!("🔓 [{}] 解绑轮次: {}", self.component_name, turn);
        }
        // 🆕 解绑轮次时清除打断状态（使用安全方法）
        self.set_interrupt_state_if_needed(false);
    }

    /// 🔧 手动清除本地打断状态（pub-sub架构下，每个subscriber管理自己的状态）
    pub fn clear_interrupt_state(&self) {
        let was_interrupted = self.interrupted_state.swap(false, Ordering::AcqRel);
        if was_interrupted {
            debug!("🔄 [{}] 清除本地打断状态（不影响其他subscriber）", self.component_name);
        } else {
            debug!("🔄 [{}] 本地打断状态已经是false，无需清除", self.component_name);
        }
    }

    /// 🔧 新增：安全地设置打断状态（避免重复设置）
    pub fn set_interrupt_state_if_needed(&self, interrupted: bool) -> bool {
        let previous = self.interrupted_state.swap(interrupted, Ordering::AcqRel);
        if previous != interrupted {
            debug!("🔄 [{}] 打断状态变更: {} -> {}", self.component_name, previous, interrupted);
            true // 状态发生了变化
        } else {
            false // 状态没有变化
        }
    }

    /// 🔄 兼容性方法：检查是否被打断（等价于 check_interrupt().is_some()）
    pub fn is_interrupted(&mut self) -> bool {
        self.check_interrupt().is_some()
    }

    /// 🔄 兼容性方法：非可变版本的检查是否被打断
    pub fn is_interrupted_immutable(&self) -> bool {
        self.interrupted_state.load(Ordering::Acquire)
    }

    /// 🔄 兼容性方法：等待变化（简化为等待打断事件）
    pub async fn changed(&mut self) -> Result<(), tokio::sync::broadcast::error::RecvError> {
        match self.receiver.recv().await {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// 🔄 兼容性方法：触发打断信号（简化实现）
    pub async fn trigger_interrupt(&self, _session_id: String, _reason: InterruptReason) -> Result<(), String> {
        // 在简化架构中，触发信号由manager统一处理
        warn!(
            "🚨 [{}] trigger_interrupt被调用，但在简化架构中应使用manager.broadcast_global_interrupt",
            self.component_name
        );
        Err("在简化架构中请使用SimpleInterruptManager.broadcast_global_interrupt".to_string())
    }

    /// 🔧 新增：获取打断事件统计信息
    pub fn get_interrupt_stats(&self) -> String {
        format!("[{}] 打断事件统计: {}", self.component_name, self.interrupt_stats.get_summary())
    }

    /// 🔧 新增：定期报告统计信息（用于监控）
    pub fn log_stats_if_needed(&self) {
        let total = self.interrupt_stats.total_events_processed.load(Ordering::Relaxed) + self.interrupt_stats.total_events_ignored.load(Ordering::Relaxed);

        // 每处理100个事件报告一次统计
        if total > 0 && total.is_multiple_of(100) {
            info!("📊 {}", self.get_interrupt_stats());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{Duration, sleep};

    #[tokio::test]
    async fn test_simple_interrupt_flow() {
        let manager = SimpleInterruptManager::new();

        // 开始新轮次
        let turn1 = manager.start_new_turn();
        let turn2 = manager.start_new_turn();
        assert_eq!(turn2, turn1 + 1);

        // 创建处理器
        let mut handler1 = SimpleInterruptHandler::new("session1".to_string(), "TestTask1".to_string(), manager.subscribe());
        handler1.bind_to_turn(turn1);

        let mut handler2 = SimpleInterruptHandler::new("session1".to_string(), "TestTask2".to_string(), manager.subscribe());
        handler2.bind_to_turn(turn2);

        // 广播打断
        manager
            .broadcast_global_interrupt("session1".to_string(), InterruptReason::UserSpeaking)
            .unwrap();

        sleep(Duration::from_millis(10)).await;

        // 两个处理器都应该收到打断（因为是当前轮次的打断）
        assert!(handler1.check_interrupt().is_some());
        assert!(handler2.check_interrupt().is_some());
    }

    #[tokio::test]
    async fn test_unified_ptt_logic() {
        let manager = SimpleInterruptManager::new();

        let turn1 = manager.start_new_turn();
        let turn2 = manager.start_new_turn();

        let mut handler = SimpleInterruptHandler::new("session1".to_string(), "TestTask".to_string(), manager.subscribe());
        handler.bind_to_turn(turn1);

        // 🔧 测试统一的PTT逻辑：同轮次PTT不应该被处理
        manager
            .broadcast_global_interrupt_with_turn(
                "session1".to_string(),
                InterruptReason::UserPtt,
                turn1, // 同轮次
            )
            .unwrap();

        sleep(Duration::from_millis(10)).await;
        assert!(handler.check_interrupt().is_none(), "同轮次PTT不应该被处理");

        // 🔧 测试：新轮次PTT应该被处理
        manager
            .broadcast_global_interrupt_with_turn(
                "session1".to_string(),
                InterruptReason::UserPtt,
                turn2, // 新轮次
            )
            .unwrap();

        sleep(Duration::from_millis(10)).await;
        assert!(handler.check_interrupt().is_some(), "新轮次PTT应该被处理");
    }

    #[tokio::test]
    async fn test_interrupt_stats() {
        let manager = SimpleInterruptManager::new();
        let mut handler = SimpleInterruptHandler::new("session1".to_string(), "TestTask".to_string(), manager.subscribe());

        // 先绑定一个轮次，这样事件才会被处理
        let turn1 = manager.start_new_turn();
        handler.bind_to_turn(turn1);

        // 发送UserSpeaking事件（总是会被处理）
        manager
            .broadcast_global_interrupt("session1".to_string(), InterruptReason::UserSpeaking)
            .unwrap();

        sleep(Duration::from_millis(10)).await;

        // 处理第一个事件
        let event1 = handler.check_interrupt();
        assert!(event1.is_some(), "UserSpeaking事件应该被处理");

        // 发送一个新轮次的PTT事件（轮次号 > 当前绑定轮次，所以会被处理）
        let turn2 = manager.start_new_turn();
        manager
            .broadcast_global_interrupt_with_turn("session1".to_string(), InterruptReason::UserPtt, turn2)
            .unwrap();

        sleep(Duration::from_millis(10)).await;

        // 处理第二个事件
        let event2 = handler.check_interrupt();
        assert!(event2.is_some(), "新轮次PTT事件应该被处理");

        // 检查统计信息
        let stats = handler.get_interrupt_stats();
        assert!(stats.contains("processed="));
        assert!(stats.contains("user_speaking="));
        println!("统计信息: {}", stats);
    }

    #[tokio::test]
    async fn test_turn_filtering() {
        let manager = SimpleInterruptManager::new();

        let turn1 = manager.start_new_turn();
        let turn2 = manager.start_new_turn();

        let mut old_handler = SimpleInterruptHandler::new("session1".to_string(), "OldTask".to_string(), manager.subscribe());
        old_handler.bind_to_turn(turn1);

        let mut new_handler = SimpleInterruptHandler::new("session1".to_string(), "NewTask".to_string(), manager.subscribe());
        new_handler.bind_to_turn(turn2);

        // 广播用户说话打断
        manager
            .broadcast_global_interrupt("session1".to_string(), InterruptReason::UserSpeaking)
            .unwrap();

        sleep(Duration::from_millis(10)).await;

        // UserSpeaking 总是触发打断，无论轮次，所以两个 handler 都收到打断
        assert!(old_handler.check_interrupt().is_some());
        assert!(new_handler.check_interrupt().is_some());
    }
}
