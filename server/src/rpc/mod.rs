pub mod actix_rpc_system;
pub mod actix_websocket;
pub mod config;
pub mod connection_metadata;
pub mod error;
pub mod event_handler;
pub mod message_adapter;
pub mod pipeline;
pub mod pipeline_factory;
pub mod protocol;
pub mod realtime_event;
pub mod remote_config;
pub mod request_normalizer;
pub mod session_manager;
pub mod session_router;
pub mod tts_pool; // 🆕 新增：TTS池管理模块

use crate::AsrEngine;
use crate::llm::LlmClient;
pub use actix_rpc_system::ActixRpcSystem;
pub use actix_websocket::{ActixAppState, actix_websocket_handler};
use anyhow::Result;
pub use config::RpcConfig;
pub use error::{RpcError, RpcResult};
pub use event_handler::EventHandler;
pub use message_adapter::{WsCloseFrame, WsMessage};
pub use pipeline::StreamingPipeline;
pub use pipeline_factory::PipelineFactory;
pub use protocol::{CommandId, ProtocolId};
pub use session_manager::{GlobalSessionManager, SessionManager};
pub use session_router::SessionRouter;

// Stable re-exports: external modules (llm/, agents/, asr/) import from here
// instead of reaching into pipeline::asr_llm_tts internals.
pub use pipeline::asr_llm_tts::event_emitter::EventEmitter;
pub use pipeline::asr_llm_tts::intent::{IntentClient, IntentResult, WikiContext};
pub use pipeline::asr_llm_tts::session_data_integration;
pub use pipeline::asr_llm_tts::simple_interrupt_manager::{SimpleInterruptHandler, SimpleInterruptManager};
pub use pipeline::asr_llm_tts::tool_call_manager::{ToolCallManager, ToolCallResult};
pub use pipeline::asr_llm_tts::types::{SharedFlags, TurnContext};
pub use pipeline::asr_llm_tts::{GuidedChoiceSelector, LockfreeResponseId, SelectorConfig};
// Axum imports removed - using actix-web instead
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::warn;

/// 虚拟会话状态
#[derive(Debug, Clone, PartialEq)]
pub enum VirtualSessionState {
    Created,      // 会话映射已创建，但ASR会话还未就绪
    Initializing, // ASR会话正在初始化
    Ready,        // ASR会话完全就绪，可以处理音频
    Active,       // 正在处理音频
    Processing,   // 处理中
    Closed,
}

/// 虚拟会话
#[derive(Debug)]
pub struct VirtualSession {
    pub connection_id: String,
    pub session_id: String,
    pub protocol_id: ProtocolId,
    pub created_at: Instant,
    pub last_activity: Instant,
    pub message_count: u64,
    pub state: VirtualSessionState,
}

/// 会话回调类型 - 用于处理ASR结果
pub type SessionCallback = Arc<dyn Fn(crate::asr::AsrResult) + Send + Sync>;

/// WebSocket多会话管理器 - 基于Pipeline架构
/// 🔧 重构：现在作为SessionManager和EventHandler的适配器
pub struct WebSocketSessionManager {
    /// 会话管理器
    session_manager: Arc<SessionManager>,
    /// 事件处理器
    event_handler: Arc<EventHandler>,
}

impl WebSocketSessionManager {
    pub async fn new(asr_engine: Arc<AsrEngine>, llm_client: Option<Arc<LlmClient>>, store: Arc<dyn crate::storage::ConversationStore>) -> Self {
        let router = Arc::new(SessionRouter::new(Duration::from_secs(300)));

        // 启动超时检查定时任务 (间隔 timeout / 2)
        {
            let router_clone = router.clone();
            let interval = router_clone.timeout();
            tokio::spawn(async move {
                let sleep_dur = interval / 2;
                loop {
                    tokio::time::sleep(sleep_dur).await;
                    router_clone.check_timeouts().await;
                }
            });
        }

        let mcp_manager = Arc::new(crate::mcp::McpManager::new());

        // 🆕 初始化全局会话数据存储
        if let Err(e) = crate::storage::GlobalSessionStoreManager::initialize().await {
            warn!("⚠️ 全局会话数据存储初始化失败: {}", e);
        }

        // 创建Pipeline工厂
        let pipeline_factory = PipelineFactory::new(router.clone(), asr_engine, llm_client, mcp_manager);

        // 🔧 修复：同步初始化全局TTS池，确保在处理连接前完成
        // 注释掉异步初始化，改为在main.rs中统一初始化
        // TTS池初始化已在main.rs中完成，这里不需要重复初始化

        // 创建会话管理器
        let session_manager = Arc::new(SessionManager::new(router.clone(), store, Arc::new(pipeline_factory)));

        // 🌍 注册全局SessionManager
        if GlobalSessionManager::initialize(session_manager.clone()).is_err() {
            tracing::warn!("全局SessionManager已经初始化过了");
        } else {
            tracing::info!("✅ 全局SessionManager初始化成功");
        }

        // 🆕 启动整条Pipeline的自动超时回收器（基于路由超时）
        session_manager.spawn_orphan_reclaimer(router);

        // 创建事件处理器
        let event_handler = Arc::new(EventHandler::new(session_manager.clone()));

        Self { session_manager, event_handler }
    }

    /// 注册WebSocket连接
    pub async fn register_connection(&self, connection_id: String, ws_sender: mpsc::UnboundedSender<WsMessage>) {
        self.session_manager.register_connection(connection_id, ws_sender).await;
    }

    /// 注销 WebSocket 连接（断开场景）- 清理所有关联的管线
    /// 🔧 新架构：分离 WebSocket 连接管理和 Virtual Session 生命周期
    pub async fn unregister_connection(&self, connection_id: &str) -> usize {
        self.session_manager.unregister_connection(connection_id).await
    }

    /// 🆕 新增：彻底清理会话（包括Pipeline）- 用于显式会话销毁
    pub async fn destroy_session_completely(&self, session_id: &str) -> Result<(), String> {
        self.session_manager.destroy_session(session_id).await
    }

    /// 🆕 新增：恢复会话连接绑定 - 用于重连时快速恢复
    pub async fn rebind_session_connection(&self, session_id: &str, new_connection_id: &str, payload: Option<&protocol::MessagePayload>) -> Result<(), String> {
        self.session_manager
            .rebind_session_connection(session_id, new_connection_id, payload)
            .await
    }

    /// 销毁指定会话（由客户端显式请求）
    pub async fn destroy_virtual_session_by_client_id(&self, session_id: &str, _connection_id: &str) -> Result<(), String> {
        self.session_manager.destroy_session(session_id).await
    }

    /// 🆕 处理WebSocket断开 - 清理所有关联的管线
    pub async fn handle_websocket_disconnect(&self, connection_id: &str) -> usize {
        self.session_manager.unregister_connection(connection_id).await
    }

    pub async fn get_active_session_count(&self) -> usize {
        self.session_manager.get_active_session_count().await
    }

    /// 处理工具调用结果
    pub async fn handle_tool_call_result(&self, session_id: &str, tool_result: ToolCallResult) -> Result<()> {
        self.session_manager.handle_tool_call_result(session_id, tool_result).await
    }

    /// 处理WebSocket文本消息
    pub async fn handle_websocket_message(&self, ws_message: protocol::WebSocketMessage, connection_id: &str, ws_tx: &tokio::sync::mpsc::UnboundedSender<WsMessage>) -> Result<(), String> {
        self.event_handler
            .handle_websocket_message(ws_message, connection_id, ws_tx)
            .await
    }

    /// 处理二进制消息
    pub async fn handle_binary_message(&self, header: protocol::BinaryHeader, data: &bytes::Bytes, connection_id: &str) -> Result<(), String> {
        self.event_handler.handle_binary_message(header, data, connection_id).await
    }

    /// 获取指定连接下的所有会话ID
    pub async fn get_session_ids_for_connection(&self, connection_id: &str) -> Vec<String> {
        self.session_manager.get_session_ids_for_connection(connection_id).await
    }
}

// 旧的axum WebSocket处理函数已移除 - 现在使用actix-ws版本

/// RPC系统的主要入口点 - 基于Pipeline架构
pub struct RpcSystem {
    running: Arc<std::sync::atomic::AtomicBool>,
    #[allow(dead_code)]
    config: RpcConfig,
}

impl RpcSystem {
    /// 创建新的RPC系统实例
    pub async fn new(config: RpcConfig) -> Result<Self> {
        Ok(Self { running: Arc::new(std::sync::atomic::AtomicBool::new(true)), config })
    }

    /// 停止RPC系统
    pub async fn stop(&self) -> Result<()> {
        self.running.store(false, std::sync::atomic::Ordering::Release);
        tracing::info!("RPC系统已停止");
        Ok(())
    }
}
