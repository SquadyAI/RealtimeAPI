//! 全局会话数据存储管理器
//!
//! 提供全局唯一的存储实例管理，支持懒加载和线程安全访问

use anyhow::Result;
use std::sync::{Arc, OnceLock};
use tracing::{info, warn};

use super::config::StorageConfig;
use super::session_data::SessionDataStore;

/// 全局存储管理器
///
/// 使用 OnceLock 确保全局唯一性和线程安全
static GLOBAL_SESSION_STORE: OnceLock<Option<Arc<dyn SessionDataStore>>> = OnceLock::new();

/// 全局存储管理器
pub struct GlobalSessionStoreManager;

impl GlobalSessionStoreManager {
    /// 初始化全局存储（只能调用一次）
    pub async fn initialize() -> Result<()> {
        let store_option = match Self::create_store().await {
            Ok(store) => {
                info!("✅ 全局会话数据存储初始化成功");
                Some(store)
            },
            Err(e) => {
                warn!("⚠️ 全局会话数据存储初始化失败，会话数据将不会持久化: {}", e);
                info!("💡 这是正常行为：生产环境未配置数据库时不进行持久化");
                None
            },
        };

        GLOBAL_SESSION_STORE
            .set(store_option)
            .map_err(|_| anyhow::anyhow!("全局存储已经初始化过了"))?;

        Ok(())
    }

    /// 获取全局存储实例
    ///
    /// 返回 None 表示存储不可用（初始化失败或未初始化）
    pub fn get() -> Option<Arc<dyn SessionDataStore>> {
        GLOBAL_SESSION_STORE.get()?.as_ref().cloned()
    }

    /// 检查存储是否已初始化
    pub fn is_initialized() -> bool {
        GLOBAL_SESSION_STORE.get().is_some()
    }

    /// 检查存储是否可用
    pub fn is_available() -> bool {
        GLOBAL_SESSION_STORE.get().map(|opt| opt.is_some()).unwrap_or(false)
    }

    /// 创建存储实例（内部方法）
    async fn create_store() -> Result<Arc<dyn SessionDataStore>> {
        info!("🗄️ 正在创建全局会话数据存储...");

        let config = StorageConfig::from_env();
        let store = config.create_store().await?;

        info!(
            "✅ 全局会话数据存储创建成功: {:?}",
            if matches!(config, StorageConfig::InMemory) {
                "内存存储"
            } else {
                "PostgreSQL存储"
            }
        );

        Ok(store)
    }

    /// 获取存储统计信息（如果可用）
    pub async fn get_stats() -> Option<String> {
        let _store = Self::get()?;
        // 这里可以扩展为获取实际的统计信息
        Some("全局存储状态: 可用".to_string())
    }

    /// 强制重新初始化（仅用于测试）
    #[cfg(test)]
    pub fn reset_for_test() {
        // 这是一个占位符实现，实际的重置逻辑可能需要更复杂的处理
        // 在当前的实现中，我们不支持在测试之间重置全局存储
        // 测试应该通过创建独立的存储实例来隔离
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_global_store_manager() {
        // 重置测试状态
        GlobalSessionStoreManager::reset_for_test();

        // 检查未初始化状态
        assert!(!GlobalSessionStoreManager::is_initialized());
        assert!(!GlobalSessionStoreManager::is_available());
        assert!(GlobalSessionStoreManager::get().is_none());

        // 初始化
        GlobalSessionStoreManager::initialize().await.unwrap();

        // 检查初始化后状态
        assert!(GlobalSessionStoreManager::is_initialized());
        // 在测试环境中，应该能够成功创建内存存储
        assert!(GlobalSessionStoreManager::is_available());
        assert!(GlobalSessionStoreManager::get().is_some());

        // 测试重复初始化应该失败
        let result = GlobalSessionStoreManager::initialize().await;
        assert!(result.is_err());
    }
}
