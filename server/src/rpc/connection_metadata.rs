//! Connection metadata management module
//!
//! This module provides structures and utilities for storing and managing
//! metadata associated with active WebSocket connections, such as IP geolocation
//! information.

use crate::ip_geolocation::types::IpAddress;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::debug;

/// Metadata associated with a WebSocket connection
///
/// 注意：这里只存储纯粹的连接信息，不存储任何 session 特定的配置
/// Session 相关的配置（timezone、location等）应该使用 SessionMetadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionMetadata {
    /// Unique identifier for the connection
    pub connection_id: String,

    /// Client IP address
    pub client_ip: Option<IpAddress>,

    /// Timestamp when the connection was established
    pub connected_at: chrono::DateTime<chrono::Utc>,
}

/// Global cache for connection metadata (key: connection_id)
pub static CONNECTION_METADATA_CACHE: Lazy<Arc<DashMap<String, ConnectionMetadata>>> = Lazy::new(|| Arc::new(DashMap::new()));

// SessionMetadata 已迁移到 agents::turn_tracker::SessionContext

/// Connection metadata cleanup guard
///
/// This struct is responsible for automatically removing connection metadata
/// from the global cache when it's dropped, ensuring proper cleanup of resources.
pub struct ConnectionMetadataCleanupGuard {
    pub connection_id: String,
}

impl Drop for ConnectionMetadataCleanupGuard {
    fn drop(&mut self) {
        debug!("🗑️ 清理连接元数据缓存: connection_id={}", self.connection_id);
        CONNECTION_METADATA_CACHE.remove(&self.connection_id);
    }
}

impl ConnectionMetadata {
    /// Create new connection metadata
    pub fn new(connection_id: String, client_ip: Option<IpAddress>) -> Self {
        let now = chrono::Utc::now();
        Self { connection_id, client_ip, connected_at: now }
    }

    /// Get string representation of client IP
    pub fn client_ip_str(&self) -> Option<String> {
        self.client_ip.as_ref().map(|ip| match ip {
            IpAddress::V4(ipv4) => ipv4.to_string(),
            IpAddress::V6(ipv6) => ipv6.to_string(),
        })
    }
}

// SessionMetadata impl 和 SessionMetadataCleanupGuard 已迁移到 agents::turn_tracker
