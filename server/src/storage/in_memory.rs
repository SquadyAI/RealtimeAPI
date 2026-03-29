use async_trait::async_trait;
use rustc_hash::FxHashMap;
use std::collections::VecDeque;
use tokio::sync::RwLock;
use tracing::debug;

use super::{ConversationRecord, ConversationStore};

/// 内存存储实现（开发/测试用，或数据库不可用时的回退选项）
#[derive(Debug)]
pub struct InMemoryStore {
    records: RwLock<FxHashMap<String, ConversationRecord>>,
    order: RwLock<VecDeque<String>>,
    capacity: usize,
}

impl InMemoryStore {
    pub fn new() -> Self {
        let capacity = std::env::var("INMEM_STORE_CAPACITY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(100000);
        Self {
            records: RwLock::new(FxHashMap::default()),
            order: RwLock::new(VecDeque::new()),
            capacity,
        }
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ConversationStore for InMemoryStore {
    async fn load(&self, session_id: &str) -> anyhow::Result<Option<ConversationRecord>> {
        // 统一锁顺序：先获取 records 锁，再获取 order 锁，避免死锁
        let record = {
            let records = self.records.read().await;
            records.get(session_id).cloned()
        };

        if record.is_some() {
            // 移动到队尾（最近使用）
            // 注意：这里只获取 order 写锁，不影响 records 读锁
            let mut order = self.order.write().await;
            if let Some(pos) = order.iter().position(|k| k == session_id) {
                order.remove(pos);
            }
            order.push_back(session_id.to_string());
        }

        if record.is_some() {
            debug!("📁 内存存储：加载会话 {} 成功", session_id);
        } else {
            debug!("📁 内存存储：会话 {} 不存在", session_id);
        }

        Ok(record)
    }

    async fn save(&self, record: &ConversationRecord) -> anyhow::Result<()> {
        // 写入记录
        {
            let mut records = self.records.write().await;
            records.insert(record.session_id.clone(), record.clone());
        }

        // 更新 LRU 顺序
        let evict_ids: Vec<String> = {
            let mut order = self.order.write().await;
            if let Some(pos) = order.iter().position(|k| k == &record.session_id) {
                order.remove(pos);
            }
            order.push_back(record.session_id.clone());

            // 收集需要逐出的 ID（先在 order 锁内确定要驱逐的 ID）
            let mut evict_ids = Vec::new();
            while order.len() > self.capacity {
                if let Some(evict_id) = order.pop_front() {
                    evict_ids.push(evict_id);
                }
            }
            evict_ids
        };

        // 在 order 锁外删除被逐出的记录，避免死锁
        // 锁顺序：records → order（与 load() 方法一致）
        if !evict_ids.is_empty() {
            let mut records = self.records.write().await;
            for evict_id in evict_ids {
                if records.remove(&evict_id).is_some() {
                    debug!("🧹 InMemoryStore LRU 驱逐: {}", evict_id);
                }
            }
        }

        debug!(
            "💾 内存存储：保存会话 {} 成功，消息数量: {}",
            record.session_id,
            record.messages.len()
        );

        Ok(())
    }

    async fn delete(&self, session_id: &str) -> anyhow::Result<()> {
        let removed = {
            let mut records = self.records.write().await;
            records.remove(session_id)
        };
        // 从 LRU 队列移除
        {
            let mut order = self.order.write().await;
            if let Some(pos) = order.iter().position(|k| k == session_id) {
                order.remove(pos);
            }
        }

        if removed.is_some() {
            debug!("🗑️ 内存存储：删除会话 {} 成功", session_id);
        } else {
            debug!("🗑️ 内存存储：会话 {} 不存在，无需删除", session_id);
        }

        Ok(())
    }

    async fn list_sessions(&self) -> anyhow::Result<Vec<String>> {
        // 返回按最近使用排序的列表（从最近到最久）
        let order = self.order.read().await;
        let mut sessions: Vec<String> = order.iter().cloned().collect();
        sessions.reverse(); // 最近使用优先

        debug!("📋 内存存储：列出 {} 个会话", sessions.len());
        Ok(sessions)
    }
}
