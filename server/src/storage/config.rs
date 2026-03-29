//! 会话数据存储配置模块
//!
//! 提供存储实例的创建和配置管理

use anyhow::Result;
use std::sync::Arc;
use tracing::{debug, error, info};

use super::session_data::{SessionDataStore, in_memory_store::InMemorySessionDataStore};
use super::{ConversationStore, InMemoryStore, PgStore};
use once_cell::sync::OnceCell;
use tokio::sync::mpsc;

// 🆕 延迟报告数据结构与写入器
#[derive(Debug, Clone)]
pub struct LatencyRecord {
    pub session_id: String,
    pub response_id: String,
    pub turn_count: i32,
    pub asr_final_ms: Option<i64>,
    pub llm_first_token_ms: Option<i64>,
    pub tts_first_audio_ms: Option<i64>,
    pub paced_first_audio_ms: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

static LATENCY_WRITER: OnceCell<mpsc::Sender<LatencyRecord>> = OnceCell::new();

// 全局共享的PostgreSQL连接池（在会话数据存储与对话存储之间复用）
static PG_POOL: OnceCell<sqlx::PgPool> = OnceCell::new();

async fn get_or_init_pg_pool(database_url: &str, max_connections: Option<u32>) -> anyhow::Result<sqlx::PgPool> {
    if let Some(pool) = PG_POOL.get() {
        return Ok(pool.clone());
    }

    use sqlx::postgres::PgPoolOptions;

    let mut pool_options = PgPoolOptions::new();
    if let Some(max_conns) = max_connections {
        pool_options = pool_options.max_connections(max_conns);
    }

    let pool = pool_options
        .connect(database_url)
        .await
        .map_err(|e| anyhow::anyhow!("连接PostgreSQL失败: {}", e))?;

    let _ = PG_POOL.set(pool.clone());
    Ok(pool)
}

/// 存储配置类型
#[derive(Debug, Clone, Default)]
pub enum StorageConfig {
    /// 内存存储（开发/测试用）
    #[default]
    InMemory,
    /// PostgreSQL存储（生产用）
    PostgreSQL { database_url: String, max_connections: Option<u32> },
}

impl StorageConfig {
    /// 从环境变量创建存储配置
    pub fn from_env() -> Self {
        // 检查是否设置了数据库URL
        if let Ok(database_url) = std::env::var("DATABASE_URL") {
            let max_connections = std::env::var("DB_MAX_CONNECTIONS").ok().and_then(|v| v.parse().ok());

            info!(
                "📊 使用PostgreSQL存储: database_url已设置, max_connections={:?}",
                max_connections
            );
            Self::PostgreSQL { database_url, max_connections }
        } else {
            info!("📊 使用内存存储: DATABASE_URL未设置");
            Self::InMemory
        }
    }

    /// 创建存储实例
    pub async fn create_store(&self) -> Result<Arc<dyn SessionDataStore>> {
        match self {
            StorageConfig::InMemory => {
                info!("🗄️ 创建内存存储实例");
                Ok(Arc::new(InMemorySessionDataStore::new()))
            },
            StorageConfig::PostgreSQL { database_url, max_connections } => {
                info!("🗄️ 创建PostgreSQL存储实例: max_connections={:?}", max_connections);

                {
                    use super::session_data::pg_store::PgSessionDataStore;

                    let pool = get_or_init_pg_pool(database_url, *max_connections).await?;

                    let store = PgSessionDataStore::new(pool);

                    // 初始化数据库表
                    store
                        .init_tables()
                        .await
                        .map_err(|e| anyhow::anyhow!("初始化数据库表失败: {}", e))?;

                    // 🆕 初始化延迟报告表与后台写入队列
                    if let Err(e) = init_latency_writer().await {
                        error!("初始化延迟报告写入器失败: {}", e);
                    }

                    info!("✅ PostgreSQL存储实例创建成功");
                    let store: Arc<dyn SessionDataStore> = Arc::new(store);
                    Ok(store)
                }
            },
        }
    }

    /// 创建对话存储实例（conversations / session_configs）
    pub async fn create_conversation_store(&self) -> Result<Arc<dyn ConversationStore>> {
        match self {
            StorageConfig::InMemory => {
                info!("🗄️ 创建内存对话存储实例");
                Ok(Arc::new(InMemoryStore::new()) as Arc<dyn ConversationStore>)
            },
            StorageConfig::PostgreSQL { database_url, max_connections } => {
                info!("🗄️ 创建PostgreSQL对话存储实例: max_connections={:?}", max_connections);
                let pool = get_or_init_pg_pool(database_url, *max_connections).await?;

                let store = PgStore::new(pool);

                // 初始化数据库表（conversations / session_configs）
                store
                    .init_tables()
                    .await
                    .map_err(|e| anyhow::anyhow!("初始化对话表失败: {}", e))?;

                // 🆕 初始化延迟报告表与后台写入队列
                if let Err(e) = init_latency_writer().await {
                    error!("初始化延迟报告写入器失败: {}", e);
                }

                info!("✅ PostgreSQL对话存储实例创建成功");
                Ok(Arc::new(store) as Arc<dyn ConversationStore>)
            },
        }
    }
}

// 🆕 初始化延迟报告写入器：创建表 + 启动后台消费者
async fn init_latency_writer() -> anyhow::Result<()> {
    if LATENCY_WRITER.get().is_some() {
        return Ok(());
    }

    let pool = PG_POOL.get().cloned().ok_or_else(|| anyhow::anyhow!("PG_POOL 未初始化"))?;

    // 自动建表（幂等）
    let create_table_sql = r#"
        CREATE TABLE IF NOT EXISTS latency_reports (
            session_id          VARCHAR(64) NOT NULL,
            response_id         VARCHAR(64) NOT NULL,
            turn_count          INTEGER     NOT NULL,
            asr_final_ms        BIGINT,
            llm_first_token_ms  BIGINT,
            tts_first_audio_ms  BIGINT,
            paced_first_audio_ms BIGINT,
            created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            PRIMARY KEY (session_id, response_id)
        )
    "#;
    sqlx::query(create_table_sql).execute(&pool).await?;

    // 启动后台写入任务（无阻塞insert）
    let (tx, mut rx) = mpsc::channel::<LatencyRecord>(1024);
    tokio::spawn(async move {
        while let Some(rec) = rx.recv().await {
            let insert_sql = r#"
                INSERT INTO latency_reports (
                    session_id, response_id, turn_count,
                    asr_final_ms, llm_first_token_ms, tts_first_audio_ms, paced_first_audio_ms,
                    created_at
                ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
                ON CONFLICT (session_id, response_id) DO UPDATE SET
                    turn_count = EXCLUDED.turn_count,
                    asr_final_ms = EXCLUDED.asr_final_ms,
                    llm_first_token_ms = EXCLUDED.llm_first_token_ms,
                    tts_first_audio_ms = EXCLUDED.tts_first_audio_ms,
                    paced_first_audio_ms = EXCLUDED.paced_first_audio_ms,
                    created_at = EXCLUDED.created_at
            "#;
            let res = sqlx::query(insert_sql)
                .bind(&rec.session_id)
                .bind(&rec.response_id)
                .bind(rec.turn_count)
                .bind(rec.asr_final_ms)
                .bind(rec.llm_first_token_ms)
                .bind(rec.tts_first_audio_ms)
                .bind(rec.paced_first_audio_ms)
                .bind(rec.created_at)
                .execute(&pool)
                .await;
            if let Err(e) = res {
                error!("写入延迟报告失败: {}", e);
            } else {
                debug!("已写入延迟报告: session_id={}, response_id={}", rec.session_id, rec.response_id);
            }
        }
    });

    let _ = LATENCY_WRITER.set(tx);
    Ok(())
}

/// 🆕 非阻塞入队延迟报告
pub fn enqueue_latency_report(record: LatencyRecord) -> Result<()> {
    if let Some(tx) = LATENCY_WRITER.get() {
        // 尝试非阻塞发送，失败则丢弃以避免阻塞主流程
        if tx.try_send(record).is_err() {
            debug!("延迟报告队列已满，丢弃一条记录");
        }
        Ok(())
    } else {
        // 未初始化PG，不做任何操作
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_in_memory_storage_config() {
        let config = StorageConfig::InMemory;
        let store = config.create_store().await.unwrap();

        // 测试基本功能
        store
            .save_conversation_metadata("resp_1", "test_session", "hi there".to_string(), None)
            .await
            .unwrap();
    }

    #[test]
    fn test_storage_config_from_env() {
        // 测试默认配置（没有设置环境变量）
        unsafe {
            std::env::remove_var("DATABASE_URL");
        }
        let config = StorageConfig::from_env();
        assert!(matches!(config, StorageConfig::InMemory));
    }

    #[tokio::test]
    async fn test_storage_config_creation() {
        let config = StorageConfig::InMemory;
        let store = config.create_store().await.unwrap();

        // 测试基本功能
        store
            .save_conversation_metadata("resp_1", "test_session", "hi there".to_string(), None)
            .await
            .unwrap();
    }
}
