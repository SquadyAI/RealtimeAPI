//! 媒体 Provider 层
//!
//! 定义 MediaProvider trait，抽象不同媒体源的搜索和播放能力

mod music_provider;

pub use music_provider::MusicProvider;

use super::types::{MediaItem, MediaSource};
use crate::agents::runtime::AgentHandles;
use async_trait::async_trait;

/// 媒体 Provider Trait
///
/// 抽象不同媒体源的搜索和播放能力
#[async_trait]
pub trait MediaProvider: Send + Sync {
    /// Provider 标识
    fn source(&self) -> MediaSource;

    /// 搜索媒体
    ///
    /// # Arguments
    /// * `query` - 搜索关键词
    /// * `limit` - 最大返回数量
    async fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<MediaItem>>;

    /// 播放媒体项
    ///
    /// # Arguments
    /// * `item` - 要播放的媒体项
    /// * `handles` - Agent 运行时句柄
    async fn play(&self, item: &MediaItem, handles: &AgentHandles<'_>) -> anyhow::Result<()>;
}
