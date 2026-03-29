//! 统一媒体播放 Agent
//!
//! 将 MusicAgent 和 XimalayaAgent 统一为 MediaAgent，实现：
//! - Agent 锁机制（Intent 之前检查）
//! - 两阶段工具设计（search_media, play_media, exit_selection）
//! - 聚合搜索（按播放量排序）

#[allow(clippy::module_inception)]
pub mod media_agent;
pub mod providers;
pub mod types;

pub use media_agent::MediaAgent;
pub use providers::{MediaProvider, MusicProvider};
pub use types::{MediaAgentLock, MediaItem, MediaSource};
