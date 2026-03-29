use std::sync::Arc;

use tokio::sync::broadcast;

use super::simple_interrupt_manager::{InterruptReason, SimpleInterruptEvent, SimpleInterruptManager};

/// 中断服务，负责管理全局中断事件，供ASR、LLM、TTS等模块订阅和触发
#[derive(Clone)]
pub struct InterruptService {
    manager: Arc<SimpleInterruptManager>,
}

impl InterruptService {
    pub fn new(manager: Arc<SimpleInterruptManager>) -> Self {
        Self { manager }
    }

    pub fn manager(&self) -> Arc<SimpleInterruptManager> {
        self.manager.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SimpleInterruptEvent> {
        self.manager.subscribe()
    }

    pub fn broadcast_global_interrupt(&self, session_id: String, reason: InterruptReason) -> Result<(), String> {
        self.manager.broadcast_global_interrupt(session_id, reason)
    }

    pub fn broadcast_global_interrupt_with_turn(&self, session_id: String, reason: InterruptReason, turn: u64) -> Result<(), String> {
        self.manager.broadcast_global_interrupt_with_turn(session_id, reason, turn)
    }

    pub fn start_new_turn(&self) -> u64 {
        self.manager.start_new_turn()
    }

    pub fn current_turn(&self) -> u64 {
        self.manager.current_turn()
    }
}
