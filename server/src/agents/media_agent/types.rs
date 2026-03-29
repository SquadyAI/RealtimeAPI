//! 媒体 Agent 相关的类型定义

use serde::{Deserialize, Serialize};

/// 媒体来源
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaSource {
    /// 音乐
    Music,
}

impl MediaSource {
    pub fn display_name(&self) -> &'static str {
        match self {
            MediaSource::Music => "音乐",
        }
    }
}

/// 统一的媒体项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaItem {
    /// 唯一标识
    pub id: String,
    /// 媒体来源
    pub source: MediaSource,
    /// 标题
    pub title: String,
    /// 副标题/作者
    pub subtitle: Option<String>,
    /// 封面图片
    pub cover_url: Option<String>,
    /// 播放次数
    pub play_count: Option<i64>,
    /// 原始数据（用于播放命令）
    pub raw_data: serde_json::Value,
}

impl MediaItem {
    pub fn new(id: impl Into<String>, source: MediaSource, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            source,
            title: title.into(),
            subtitle: None,
            cover_url: None,
            play_count: None,
            raw_data: serde_json::Value::Null,
        }
    }

    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub fn with_cover_url(mut self, url: impl Into<String>) -> Self {
        self.cover_url = Some(url.into());
        self
    }

    pub fn with_play_count(mut self, count: i64) -> Self {
        self.play_count = Some(count);
        self
    }

    pub fn with_raw_data(mut self, data: serde_json::Value) -> Self {
        self.raw_data = data;
        self
    }
}

/// Agent 锁状态（每个 WebSocket 连接独立）
#[derive(Debug, Clone, Default)]
pub struct MediaAgentLock {
    /// 是否锁定
    locked: bool,
    /// 待选择的媒体项
    pending_results: Option<Vec<MediaItem>>,
}

impl MediaAgentLock {
    pub fn new() -> Self {
        Self::default()
    }

    /// 获取锁
    pub fn acquire(&mut self, results: Vec<MediaItem>) {
        self.locked = true;
        self.pending_results = Some(results);
    }

    /// 释放锁
    pub fn release(&mut self) {
        self.locked = false;
        self.pending_results = None;
    }

    /// 检查是否锁定
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// 获取待选结果（不可变引用）
    pub fn pending_results(&self) -> Option<&Vec<MediaItem>> {
        self.pending_results.as_ref()
    }

    /// 获取待选结果（克隆）
    pub fn pending_results_cloned(&self) -> Option<Vec<MediaItem>> {
        self.pending_results.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_media_source_display_name() {
        assert_eq!(MediaSource::Music.display_name(), "音乐");
    }

    #[test]
    fn test_media_item_builder() {
        let item = MediaItem::new("123", MediaSource::Music, "测试标题")
            .with_subtitle("测试副标题")
            .with_play_count(1000)
            .with_cover_url("http://example.com/cover.jpg");

        assert_eq!(item.id, "123");
        assert_eq!(item.source, MediaSource::Music);
        assert_eq!(item.title, "测试标题");
        assert_eq!(item.subtitle, Some("测试副标题".to_string()));
        assert_eq!(item.play_count, Some(1000));
        assert_eq!(item.cover_url, Some("http://example.com/cover.jpg".to_string()));
    }

    #[test]
    fn test_media_agent_lock_acquire_release() {
        let mut lock = MediaAgentLock::new();

        // 初始状态
        assert!(!lock.is_locked());
        assert!(lock.pending_results().is_none());

        // 获取锁
        let items = vec![
            MediaItem::new("1", MediaSource::Music, "Track 1"),
            MediaItem::new("2", MediaSource::Music, "Track 2"),
        ];
        lock.acquire(items);

        assert!(lock.is_locked());
        assert_eq!(lock.pending_results().unwrap().len(), 2);

        // 释放锁
        lock.release();
        assert!(!lock.is_locked());
        assert!(lock.pending_results().is_none());
    }

    #[test]
    fn test_media_item_serialization() {
        let item = MediaItem::new("123", MediaSource::Music, "测试").with_raw_data(serde_json::json!({"key": "value"}));

        let json = serde_json::to_string(&item).unwrap();
        let deserialized: MediaItem = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, "123");
        assert_eq!(deserialized.source, MediaSource::Music);
        assert_eq!(deserialized.title, "测试");
        assert_eq!(deserialized.raw_data["key"], "value");
    }
}
