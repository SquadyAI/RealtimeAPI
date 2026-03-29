use async_trait::async_trait;
use rustc_hash::FxHashMap;
use serde_json;
use sqlx::{PgPool, Row};
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, error, info};

use super::{ConversationRecord, ConversationStore};

/// Cache entry with expiration support
#[derive(Debug, Clone)]
enum CacheEntry {
    /// Complete conversation record
    Record { record: ConversationRecord, cached_at: Instant, ttl: Duration },
    /// Placeholder for pending save operations
    Pending { cached_at: Instant, ttl: Duration },
    /// Marker for non-existent sessions
    NotFound { cached_at: Instant, ttl: Duration },
}

impl CacheEntry {
    /// Create a cache entry for an existing record
    fn new_record(record: ConversationRecord, ttl: Duration) -> Self {
        Self::Record { record, cached_at: Instant::now(), ttl }
    }

    /// Create a placeholder for pending operations
    fn new_pending(ttl: Duration) -> Self {
        Self::Pending { cached_at: Instant::now(), ttl }
    }

    /// Create a marker for non-existent sessions
    fn new_not_found(ttl: Duration) -> Self {
        Self::NotFound { cached_at: Instant::now(), ttl }
    }

    /// Create a cache entry for an optional record (None means not found)
    #[allow(dead_code)]
    fn new_optional(record: Option<ConversationRecord>, ttl: Duration) -> Self {
        match record {
            Some(r) => Self::new_record(r, ttl),
            None => Self::new_not_found(ttl),
        }
    }

    fn is_expired(&self) -> bool {
        let elapsed = match self {
            Self::Record { cached_at, .. } => cached_at.elapsed(),
            Self::Pending { cached_at, .. } => cached_at.elapsed(),
            Self::NotFound { cached_at, .. } => cached_at.elapsed(),
        };
        match self {
            Self::Record { ttl, .. } => elapsed > *ttl,
            Self::Pending { ttl, .. } => elapsed > *ttl,
            Self::NotFound { ttl, .. } => elapsed > *ttl,
        }
    }

    /// Get the record if this is a Record variant
    fn as_record(&self) -> Option<&ConversationRecord> {
        match self {
            Self::Record { record, .. } => Some(record),
            _ => None,
        }
    }

    /// Check if this is a Pending variant
    fn is_pending(&self) -> bool {
        matches!(self, Self::Pending { .. })
    }

    /// Check if this is a NotFound variant
    fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }
}

/// PostgreSQL storage implementation with in-memory caching
#[derive(Debug)]
pub struct PgStore {
    pool: PgPool,
    // Session cache to reduce database queries
    cache: RwLock<FxHashMap<String, CacheEntry>>,
    cache_ttl: Duration,
    max_cache_size: usize,
    write_tx: mpsc::Sender<ConversationRecord>,
}

impl PgStore {
    pub fn new(pool: PgPool) -> Self {
        let (write_tx, mut write_rx) = mpsc::channel(100);
        let pool_clone = pool.clone();
        tokio::spawn(async move {
            while let Some(record) = write_rx.recv().await {
                match PgStore::save_db_static(&pool_clone, &record).await {
                    Ok(_) => debug!("Successfully saved conversation record to database"),
                    Err(e) => error!("Failed to save conversation record to database: {}", e),
                }
            }
        });
        let max_cache_size = std::env::var("PG_STORE_CACHE_CAPACITY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(100_000);
        Self {
            pool,
            cache: RwLock::new(FxHashMap::default()),
            cache_ttl: Duration::from_secs(300), // 5 minutes cache TTL
            max_cache_size,                      // LRU 上限，默认10万，可通过环境变量覆盖
            write_tx,
        }
    }

    /// Initialize database tables
    pub async fn init_tables(&self) -> anyhow::Result<()> {
        info!("Initializing PostgreSQL conversations table...");

        let query = r#"
            CREATE TABLE IF NOT EXISTS conversations (
                session_id      VARCHAR(64) PRIMARY KEY,
                config          JSONB        NOT NULL,
                messages        JSONB        NOT NULL,
                updated_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW()
            )
        "#;

        sqlx::query(query)
            .execute(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create conversations table: {}", e))?;

        // Create session_configs table
        let session_cfg_query = r#"
            CREATE TABLE IF NOT EXISTS session_configs (
                session_id      VARCHAR(64) PRIMARY KEY,
                config          JSONB        NOT NULL,
                created_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
                updated_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW()
            )
        "#;

        sqlx::query(session_cfg_query)
            .execute(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create session_configs table: {}", e))?;

        // Create index to optimize query performance
        let index_query = r#"
            CREATE INDEX IF NOT EXISTS idx_conversations_updated_at
            ON conversations(updated_at DESC)
        "#;

        sqlx::query(index_query)
            .execute(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create index: {}", e))?;

        info!("PostgreSQL tables initialization completed");
        Ok(())
    }

    /// Ensure cache size does not exceed limit, clean expired entries and oldest entries
    async fn ensure_cache_size(&self, cache: &mut FxHashMap<String, CacheEntry>) {
        // First clean up expired entries
        let initial_size = cache.len();
        cache.retain(|_key, entry| !entry.is_expired());

        let expired_count = initial_size - cache.len();
        if expired_count > 0 {
            debug!("Cleaned {} expired cache entries", expired_count);
        }

        // If still exceeding size limit, clean oldest entries
        if cache.len() >= self.max_cache_size {
            let excess_count = cache.len() - self.max_cache_size + 10; // Clean 10 more to avoid frequent cleaning
            let mut entries: Vec<(String, Instant)> = cache
                .iter()
                .map(|(k, v)| {
                    let cached_at = match v {
                        CacheEntry::Record { cached_at, .. } => *cached_at,
                        CacheEntry::Pending { cached_at, .. } => *cached_at,
                        CacheEntry::NotFound { cached_at, .. } => *cached_at,
                    };
                    (k.clone(), cached_at)
                })
                .collect();

            // Sort by cache time, oldest first
            entries.sort_by_key(|(_, cached_at)| *cached_at);

            for (key, _) in entries.into_iter().take(excess_count) {
                cache.remove(&key);
            }

            debug!(
                "Cache cleanup completed: current size={}, max limit={}",
                cache.len(),
                self.max_cache_size
            );
        }
    }

    /// Clean up expired sessions (optional, for periodic maintenance)
    pub async fn cleanup_old_sessions(&self, days: i32) -> anyhow::Result<u64> {
        let query = r#"
            DELETE FROM conversations
            WHERE updated_at < NOW() - INTERVAL $1 DAY
        "#;

        let result = sqlx::query(query)
            .bind(days)
            .execute(&self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to clean up expired sessions: {}", e))?;

        let deleted_count = result.rows_affected();
        if deleted_count > 0 {
            info!("Cleaned up {} expired sessions (older than {} days)", deleted_count, days);
        } else {
            debug!("No expired sessions to clean up (older than {} days)", days);
        }

        Ok(deleted_count)
    }

    /// Static database save method used by background task
    /// Uses transaction to ensure consistency between conversations and session_configs tables
    async fn save_db_static(pool: &PgPool, record: &ConversationRecord) -> anyhow::Result<()> {
        let messages_json = serde_json::to_value(&record.messages).map_err(|e| anyhow::anyhow!("Failed to serialize messages: {}", e))?;

        // Use transaction to ensure consistency
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to begin transaction: {}", e))?;

        // Save to conversations table
        let query = r#"
            INSERT INTO conversations (session_id, config, messages, updated_at)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (session_id)
            DO UPDATE SET
                config = $2,
                messages = $3,
                updated_at = $4
        "#;

        sqlx::query(query)
            .bind(&record.session_id)
            .bind(&record.config)
            .bind(&messages_json)
            .bind(record.updated_at)
            .execute(&mut *tx)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to save to conversations table: {}", e))?;

        // Sync to session_configs table
        let session_cfg_query = r#"
            INSERT INTO session_configs (session_id, config, updated_at)
            VALUES ($1, $2, NOW())
            ON CONFLICT (session_id)
            DO UPDATE SET
                config = EXCLUDED.config,
                updated_at = NOW()
        "#;

        sqlx::query(session_cfg_query)
            .bind(&record.session_id)
            .bind(&record.config)
            .execute(&mut *tx)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to sync session_configs table: {}", e))?;

        // Commit transaction
        tx.commit()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to commit transaction: {}", e))?;

        debug!(
            "Successfully saved conversation record to database: session_id={}",
            record.session_id
        );
        Ok(())
    }
}

#[async_trait]
impl ConversationStore for PgStore {
    async fn load(&self, session_id: &str) -> anyhow::Result<Option<ConversationRecord>> {
        let cache_key = session_id.to_string();

        // First try to read from cache (read lock)
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.get(&cache_key)
                && !entry.is_expired()
            {
                // Handle different cache entry types
                if entry.is_pending() {
                    // For pending saves, we skip cache and go directly to database
                    // to ensure we get the latest persisted data
                    debug!("Session {} has pending save, querying database", session_id);
                } else if entry.is_not_found() {
                    debug!("Session {} confirmed as non-existent from cache", session_id);
                    return Ok(None);
                } else if let Some(record) = entry.as_record() {
                    debug!("Loaded session {} from cache (cache hit)", session_id);
                    return Ok(Some(record.clone()));
                }
            }
        }

        // Cache miss or expired, need to query from database
        debug!("Loading session {} from database (cache miss/expired)", session_id);

        let query = r#"
            SELECT session_id, config, messages, updated_at
            FROM conversations
            WHERE session_id = $1
        "#;

        let row_result = sqlx::query(query).bind(session_id).fetch_optional(&self.pool).await;

        match row_result {
            Ok(Some(row)) => {
                let config: serde_json::Value = row
                    .try_get("config")
                    .map_err(|e| anyhow::anyhow!("Failed to parse config field: {}", e))?;

                let messages_json: serde_json::Value = row
                    .try_get("messages")
                    .map_err(|e| anyhow::anyhow!("Failed to parse messages field: {}", e))?;

                let messages = serde_json::from_value(messages_json).map_err(|e| anyhow::anyhow!("Failed to deserialize messages: {}", e))?;

                let updated_at: chrono::DateTime<chrono::Utc> = row
                    .try_get("updated_at")
                    .map_err(|e| anyhow::anyhow!("Failed to parse updated_at field: {}", e))?;

                let record = ConversationRecord { session_id: session_id.to_string(), config, messages, updated_at };

                debug!(
                    "Successfully loaded session {} from database, message count: {}",
                    session_id,
                    record.messages.len()
                );

                // Update cache
                {
                    let mut cache = self.cache.write().await;
                    self.ensure_cache_size(&mut cache).await;
                    cache.insert(cache_key, CacheEntry::new_record(record.clone(), self.cache_ttl));
                }
                Ok(Some(record))
            },
            Ok(None) => {
                debug!("Session {} does not exist in database", session_id);
                // Update cache
                {
                    let mut cache = self.cache.write().await;
                    self.ensure_cache_size(&mut cache).await;
                    cache.insert(cache_key, CacheEntry::new_not_found(self.cache_ttl));
                }
                Ok(None)
            },
            Err(e) => {
                error!("Failed to load session {} from database: {}", session_id, e);
                Err(anyhow::anyhow!("Database query failed: {}", e))
            },
        }
    }

    async fn save(&self, record: &ConversationRecord) -> anyhow::Result<()> {
        // Use lightweight placeholder to avoid blocking operations
        {
            let mut cache = self.cache.write().await;
            // Insert a pending placeholder instead of cloning the entire record
            cache.insert(record.session_id.clone(), CacheEntry::new_pending(self.cache_ttl));
        }

        // Send to background queue for persistence
        self.write_tx
            .send(record.clone())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send record to write queue: {}", e))?;

        debug!("Session {} has been queued for saving", record.session_id);
        Ok(())
    }

    async fn delete(&self, session_id: &str) -> anyhow::Result<()> {
        // Write-only policy: do not delete from PostgreSQL.
        // We only clear in-memory cache to avoid stale data occupying memory.
        let mut cache = self.cache.write().await;
        cache.remove(&session_id.to_string());
        debug!(
            "Write-only PG store: skipped DB delete, cleared cache for session {}",
            session_id
        );
        Ok(())
    }

    async fn list_sessions(&self) -> anyhow::Result<Vec<String>> {
        let query = r#"
            SELECT session_id
            FROM conversations
            ORDER BY updated_at DESC
        "#;

        let rows_result = sqlx::query(query).fetch_all(&self.pool).await;

        match rows_result {
            Ok(rows) => {
                let sessions: Result<Vec<String>, _> = rows.into_iter().map(|row| row.try_get::<String, _>("session_id")).collect();

                match sessions {
                    Ok(session_list) => {
                        debug!("Listed {} sessions from database", session_list.len());
                        Ok(session_list)
                    },
                    Err(e) => {
                        error!("Failed to parse session list: {}", e);
                        Err(anyhow::anyhow!("Failed to parse session list: {}", e))
                    },
                }
            },
            Err(e) => {
                error!("Failed to query session list: {}", e);
                Err(anyhow::anyhow!("Database query failed: {}", e))
            },
        }
    }
}
