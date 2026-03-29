//! 音乐 Provider 实现
//!
//! 当前为预留结构，search/play 方法暂时返回空结果。
//! 用户后续提供音乐 API 后再实现。

use async_trait::async_trait;
use tracing::info;

use super::MediaProvider;
use crate::agents::media_agent::types::{MediaItem, MediaSource};
use crate::agents::runtime::AgentHandles;

/// 音乐 Provider
///
/// TODO: 用户后续提供音乐 API 后实现
pub struct MusicProvider;

impl Default for MusicProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MusicProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl MediaProvider for MusicProvider {
    fn source(&self) -> MediaSource {
        MediaSource::Music
    }

    async fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<MediaItem>> {
        info!("🎵 MusicProvider: 搜索 '{}', limit={} (暂未实现，返回空结果)", query, limit);

        // TODO: 用户后续提供音乐 API 后实现
        Ok(vec![])
    }

    async fn play(&self, item: &MediaItem, _handles: &AgentHandles<'_>) -> anyhow::Result<()> {
        info!("🎵 MusicProvider: 播放 '{}' (暂未实现)", item.title);

        // TODO: 用户后续提供音乐 API 后实现
        Err(anyhow::anyhow!("音乐播放功能暂未实现"))
    }
}
