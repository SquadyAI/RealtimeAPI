//! 存储模块
//!
//! # 存储接口职责边界
//!
//! ## ConversationStore - 会话配置与对话历史存储
//!
//! **职责**: 存储和恢复多轮对话的上下文
//! - 表: `conversations`, `session_configs`
//! - 数据: session_id, config (会话配置), messages (对话历史)
//! - 用途: 会话恢复、多轮对话上下文管理
//! - 实现: `PgStore` (PostgreSQL), `InMemoryStore` (内存 LRU)
//!
//! ## SessionDataStore - 轮次原始数据归档存储
//!
//! **职责**: 归档每个对话轮次的原始数据（音频、文本、图片）
//! - 表: `asr_audio_data`, `tts_audio_data`, `conversation_metadata`, `vision_image_data`
//! - 数据: 基于 response_id 的 ASR 音频、TTS 音频、LLM 文本、Vision 图片
//! - 用途: 音频回放、数据分析、审计追踪
//! - 实现: `PgSessionDataStore` (PostgreSQL), `InMemorySessionDataStore` (内存 fallback)
//!
//! ## Fallback 机制
//!
//! 当 PostgreSQL 不可用时，内存存储作为 fallback，使用 LRU 策略保留必要数据。

use crate::llm::llm::ChatMessage;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod config;
pub mod global_manager;
pub mod in_memory;
pub mod pg_store;
pub mod session_data;

pub use config::StorageConfig;
pub use global_manager::GlobalSessionStoreManager;
pub use in_memory::InMemoryStore;
pub use pg_store::PgStore;
pub use session_data::{AudioMetadata, ImageMetadata, SessionDataStore, SessionTurnData, VisionImageData};

/// 对话记录结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationRecord {
    /// 会话ID
    pub session_id: String,
    /// 会话配置（原始 session_config JSON）
    pub config: serde_json::Value,
    /// 对话历史消息（不含system_prompt）
    pub messages: Vec<ChatMessage>,
    /// 更新时间
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl ConversationRecord {
    pub fn new(session_id: String, config: serde_json::Value) -> Self {
        Self { session_id, config, messages: Vec::new(), updated_at: chrono::Utc::now() }
    }

    pub fn with_messages(mut self, messages: Vec<ChatMessage>) -> Self {
        self.messages = messages;
        self.updated_at = chrono::Utc::now();
        self
    }
}

/// 对话存储接口
#[async_trait]
pub trait ConversationStore: Send + Sync {
    /// 加载对话记录
    async fn load(&self, session_id: &str) -> anyhow::Result<Option<ConversationRecord>>;

    /// 保存对话记录
    async fn save(&self, record: &ConversationRecord) -> anyhow::Result<()>;

    /// 删除对话记录
    async fn delete(&self, session_id: &str) -> anyhow::Result<()>;

    /// 列出所有会话ID（用于管理）
    async fn list_sessions(&self) -> anyhow::Result<Vec<String>>;
}
