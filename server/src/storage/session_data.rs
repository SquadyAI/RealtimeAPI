//! 会话轮次数据归档存储 (SessionDataStore)
//!
//! # 职责
//!
//! 归档每个对话轮次的原始数据，用于音频回放、数据分析、审计追踪。
//!
//! # 数据结构
//!
//! - 主键: response_id (每个轮次的唯一标识)
//! - 存储表:
//!   - `asr_audio_data`: 用户语音输入的原始音频
//!   - `tts_audio_data`: TTS 生成的音频输出
//!   - `conversation_metadata`: LLM 生成的文本及元数据
//!   - `vision_image_data`: Vision 功能的图片数据
//!
//! # 实现
//!
//! - `PgSessionDataStore`: PostgreSQL 持久化存储，带内存缓存和后台异步写入
//! - `InMemorySessionDataStore`: 内存 fallback，当 PG 不可用时使用

use bytes::Bytes;
use chrono::{DateTime, Utc};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

/// 自定义序列化函数，将Bytes转换为Vec<u8>
fn serialize_bytes<S>(bytes: &Option<Bytes>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match bytes {
        Some(b) => serializer.serialize_some(b.as_ref()),
        None => serializer.serialize_none(),
    }
}

/// 自定义反序列化函数，将Vec<u8>转换为Bytes
fn deserialize_bytes<'de, D>(deserializer: D) -> Result<Option<Bytes>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<Vec<u8>> = Deserialize::deserialize(deserializer)?;
    Ok(opt.map(Bytes::from))
}

/// 会话数据（基于时间戳自然排序）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTurnData {
    /// 会话ID
    pub session_id: String,

    /// 时间戳（用于自然排序）
    pub timestamp: DateTime<Utc>,

    /// 用户触发LLM时输入的音频分片数据
    #[serde(serialize_with = "serialize_bytes", deserialize_with = "deserialize_bytes", skip_serializing_if = "Option::is_none")]
    pub user_audio_chunks: Option<Bytes>,

    /// 用户音频分片的元数据
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_audio_metadata: Option<AudioMetadata>,

    /// LLM输入到TTS的文字内容
    pub llm_to_tts_text: String,

    /// TTS返回的完整音频流
    #[serde(serialize_with = "serialize_bytes", deserialize_with = "deserialize_bytes", skip_serializing_if = "Option::is_none")]
    pub tts_output_audio: Option<Bytes>,

    /// TTS输出音频的元数据
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_audio_metadata: Option<AudioMetadata>,

    /// 其他元数据
    #[serde(flatten)]
    pub metadata: FxHashMap<String, serde_json::Value>,
}

impl SessionTurnData {
    pub fn new(session_id: String, llm_to_tts_text: String) -> Self {
        Self {
            session_id,
            timestamp: Utc::now(),
            user_audio_chunks: None,
            user_audio_metadata: None,
            llm_to_tts_text,
            tts_output_audio: None,
            tts_audio_metadata: None,
            metadata: FxHashMap::default(),
        }
    }

    /// 创建完整的会话数据（包含所有三个核心内容）
    pub fn with_complete_data(
        session_id: String,
        user_audio_chunks: Bytes,
        user_audio_metadata: AudioMetadata,
        llm_to_tts_text: String,
        tts_output_audio: Bytes,
        tts_audio_metadata: AudioMetadata,
    ) -> Self {
        Self {
            session_id,
            timestamp: Utc::now(),
            user_audio_chunks: Some(user_audio_chunks),
            user_audio_metadata: Some(user_audio_metadata),
            llm_to_tts_text,
            tts_output_audio: Some(tts_output_audio),
            tts_audio_metadata: Some(tts_audio_metadata),
            metadata: FxHashMap::default(),
        }
    }

    /// 生成唯一的键（session_id + timestamp）
    pub fn generate_key(&self) -> String {
        format!(
            "{}_{}",
            self.session_id,
            self.timestamp
                .timestamp_nanos_opt()
                .unwrap_or_else(|| self.timestamp.timestamp())
        )
    }
}

/// 音频元数据
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioMetadata {
    /// 音频格式（如Opus, PCM等）
    pub format: String,

    /// 采样率（Hz）
    pub sample_rate: u32,

    /// 声道数
    pub channels: u16,

    /// 音频时长（毫秒）
    pub duration_ms: u32,

    /// 音频大小（字节）
    pub size_bytes: usize,
}

/// 图片元数据
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageMetadata {
    /// 图片格式（如jpeg, png, webp等）
    pub format: String,

    /// 图片MIME类型
    pub mime_type: String,

    /// 图片大小（字节）
    pub size_bytes: usize,

    /// 图片宽度（像素，可选）
    pub width: Option<u32>,

    /// 图片高度（像素，可选）
    pub height: Option<u32>,
}

/// 会话数据存储接口（基于response_id作为主键）
///
/// 职责：归档每个对话轮次的原始数据，用于数据分析和审计追踪。
/// 注意：当前设计为纯写入（归档），不提供读取接口。
#[async_trait::async_trait]
pub trait SessionDataStore: Send + Sync {
    /// 保存ASR音频数据
    async fn save_asr_audio_data(&self, response_id: &str, session_id: &str, user_audio_chunks: Option<Bytes>, user_audio_metadata: Option<AudioMetadata>) -> anyhow::Result<()>;

    /// 保存TTS音频数据
    async fn save_tts_audio_data(&self, response_id: &str, session_id: &str, tts_output_audio: Option<Bytes>, tts_audio_metadata: Option<AudioMetadata>) -> anyhow::Result<()>;

    /// 保存对话元数据
    async fn save_conversation_metadata(
        &self,
        response_id: &str,
        session_id: &str,
        llm_to_tts_text: String,
        metadata: Option<std::collections::HashMap<String, serde_json::Value>>,
    ) -> anyhow::Result<()>;

    /// 保存Vision图片数据
    async fn save_vision_image_data(
        &self,
        response_id: &str,
        session_id: &str,
        image_data: Bytes,
        image_metadata: ImageMetadata,
        user_prompt: Option<String>,
        llm_response: Option<String>,
    ) -> anyhow::Result<()>;
}

/// Vision图片数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionImageData {
    /// response ID
    pub response_id: String,

    /// 会话ID
    pub session_id: String,

    /// 图片数据
    #[serde(serialize_with = "serialize_bytes", deserialize_with = "deserialize_bytes")]
    pub image_data: Option<Bytes>,

    /// 图片元数据
    pub image_metadata: ImageMetadata,

    /// 用户提示词
    pub user_prompt: Option<String>,

    /// LLM响应内容（完整文本）
    pub llm_response: Option<String>,

    /// 创建时间
    pub created_at: DateTime<Utc>,

    /// 更新时间
    pub updated_at: DateTime<Utc>,
}

/// PostgreSQL存储实现
pub mod pg_store {
    use super::*;
    use async_trait::async_trait;
    use sqlx::PgPool;
    use tokio::sync::mpsc;
    use tracing::{debug, error, info};

    /// PostgreSQL会话数据存储实现（纯写入，用于数据归档）
    #[derive(Debug)]
    pub struct PgSessionDataStore {
        pool: PgPool,
        write_tx: mpsc::Sender<SessionTurnData>,
    }

    impl PgSessionDataStore {
        pub fn new(pool: PgPool) -> Self {
            let (write_tx, mut write_rx) = mpsc::channel(100);
            let pool_clone = pool.clone();

            // 启动后台写入任务
            tokio::spawn(async move {
                while let Some(data) = write_rx.recv().await {
                    match Self::save_to_db_static(&pool_clone, &data).await {
                        Ok(_) => debug!("成功保存会话轮次数据到数据库"),
                        Err(e) => error!("保存会话轮次数据到数据库失败: {}", e),
                    }
                }
            });

            Self { pool, write_tx }
        }

        /// 初始化数据库表
        pub async fn init_tables(&self) -> anyhow::Result<()> {
            info!("初始化PostgreSQL会话数据表...");

            // 创建ASR音频数据表
            let asr_table_query = r#"
                CREATE TABLE IF NOT EXISTS asr_audio_data (
                    response_id          VARCHAR(64) PRIMARY KEY,
                    session_id           VARCHAR(64) NOT NULL,
                    user_audio_chunks    BYTEA,
                    user_audio_metadata  JSONB,
                    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at           TIMESTAMPTZ NOT NULL DEFAULT NOW()
                )
            "#;

            sqlx::query(asr_table_query)
                .execute(&self.pool)
                .await
                .map_err(|e| anyhow::anyhow!("创建ASR音频数据表失败: {}", e))?;

            // 创建TTS音频数据表
            let tts_table_query = r#"
                CREATE TABLE IF NOT EXISTS tts_audio_data (
                    response_id          VARCHAR(64) PRIMARY KEY,
                    session_id           VARCHAR(64) NOT NULL,
                    tts_output_audio     BYTEA,
                    tts_audio_metadata   JSONB,
                    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at           TIMESTAMPTZ NOT NULL DEFAULT NOW()
                )
            "#;

            sqlx::query(tts_table_query)
                .execute(&self.pool)
                .await
                .map_err(|e| anyhow::anyhow!("创建TTS音频数据表失败: {}", e))?;

            // 创建对话元数据表
            let metadata_table_query = r#"
                CREATE TABLE IF NOT EXISTS conversation_metadata (
                    response_id          VARCHAR(64) PRIMARY KEY,
                    session_id           VARCHAR(64) NOT NULL,
                    llm_to_tts_text      TEXT NOT NULL,
                    metadata             JSONB,
                    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at           TIMESTAMPTZ NOT NULL DEFAULT NOW()
                )
            "#;

            sqlx::query(metadata_table_query)
                .execute(&self.pool)
                .await
                .map_err(|e| anyhow::anyhow!("创建对话元数据表失败: {}", e))?;

            // 创建Vision图片数据表
            let vision_table_query = r#"
                CREATE TABLE IF NOT EXISTS vision_image_data (
                    response_id          VARCHAR(64) PRIMARY KEY,
                    session_id           VARCHAR(64) NOT NULL,
                    image_data           BYTEA NOT NULL,
                    image_metadata       JSONB NOT NULL,
                    user_prompt          TEXT,
                    llm_response         TEXT,
                    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at           TIMESTAMPTZ NOT NULL DEFAULT NOW()
                )
            "#;

            sqlx::query(vision_table_query)
                .execute(&self.pool)
                .await
                .map_err(|e| anyhow::anyhow!("创建Vision图片数据表失败: {}", e))?;

            // 创建索引以优化按session_id查询
            let vision_index_query = r#"
                CREATE INDEX IF NOT EXISTS idx_vision_image_data_session_id
                ON vision_image_data(session_id, created_at DESC)
            "#;

            sqlx::query(vision_index_query)
                .execute(&self.pool)
                .await
                .map_err(|e| anyhow::anyhow!("创建Vision图片数据索引失败: {}", e))?;

            info!("PostgreSQL会话数据表初始化完成");
            Ok(())
        }

        /// 静态数据库保存方法（供后台任务使用）
        async fn save_to_db_static(pool: &PgPool, data: &SessionTurnData) -> anyhow::Result<()> {
            // 从metadata中获取data_type
            let data_type = data.metadata.get("data_type").and_then(|v| v.as_str()).unwrap_or("unknown");

            // 从metadata中获取response_id
            let response_id = data
                .metadata
                .get("response_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Missing response_id in metadata"))?;

            match data_type {
                "asr_audio" => {
                    // 保存到ASR音频数据表
                    let query = r#"
                        INSERT INTO asr_audio_data (
                            response_id, session_id, user_audio_chunks,
                            user_audio_metadata, created_at, updated_at
                        )
                        VALUES ($1, $2, $3, $4, NOW(), NOW())
                        ON CONFLICT (response_id)
                        DO UPDATE SET
                            user_audio_chunks = $3,
                            user_audio_metadata = $4,
                            updated_at = NOW()
                    "#;

                    let user_audio_bytes = data.user_audio_chunks.as_ref().map(|d| d.as_ref());
                    let user_audio_metadata_json = data.user_audio_metadata.as_ref().map(serde_json::to_value).transpose()?;

                    sqlx::query(query)
                        .bind(response_id)
                        .bind(&data.session_id)
                        .bind(user_audio_bytes)
                        .bind(user_audio_metadata_json)
                        .execute(pool)
                        .await
                        .map_err(|e| anyhow::anyhow!("保存ASR音频数据失败: {}", e))?;

                    debug!(
                        "成功保存ASR音频数据到数据库: session_id={}, response_id={}",
                        data.session_id, response_id
                    );
                },
                "tts_audio" => {
                    // 保存到TTS音频数据表
                    let query = r#"
                        INSERT INTO tts_audio_data (
                            response_id, session_id, tts_output_audio,
                            tts_audio_metadata, created_at, updated_at
                        )
                        VALUES ($1, $2, $3, $4, NOW(), NOW())
                        ON CONFLICT (response_id)
                        DO UPDATE SET
                            tts_output_audio = $3,
                            tts_audio_metadata = $4,
                            updated_at = NOW()
                    "#;

                    let tts_audio_bytes = data.tts_output_audio.as_ref().map(|d| d.as_ref());
                    let tts_audio_metadata_json = data.tts_audio_metadata.as_ref().map(serde_json::to_value).transpose()?;

                    sqlx::query(query)
                        .bind(response_id)
                        .bind(&data.session_id)
                        .bind(tts_audio_bytes)
                        .bind(tts_audio_metadata_json)
                        .execute(pool)
                        .await
                        .map_err(|e| anyhow::anyhow!("保存TTS音频数据失败: {}", e))?;

                    debug!(
                        "成功保存TTS音频数据到数据库: session_id={}, response_id={}",
                        data.session_id, response_id
                    );
                },
                "conversation_metadata" => {
                    // 保存到对话元数据表
                    let query = r#"
                        INSERT INTO conversation_metadata (
                            response_id, session_id, llm_to_tts_text,
                            metadata, created_at, updated_at
                        )
                        VALUES ($1, $2, $3, $4, NOW(), NOW())
                        ON CONFLICT (response_id)
                        DO UPDATE SET
                            llm_to_tts_text = $3,
                            metadata = $4,
                            updated_at = NOW()
                    "#;

                    let metadata_json = if data.metadata.is_empty() {
                        None
                    } else {
                        // 移除response_id和data_type后再保存metadata
                        let mut clean_metadata = data.metadata.clone();
                        clean_metadata.remove("response_id");
                        clean_metadata.remove("data_type");

                        if clean_metadata.is_empty() {
                            None
                        } else {
                            Some(serde_json::to_value(&clean_metadata)?)
                        }
                    };

                    sqlx::query(query)
                        .bind(response_id)
                        .bind(&data.session_id)
                        .bind(&data.llm_to_tts_text)
                        .bind(metadata_json)
                        .execute(pool)
                        .await
                        .map_err(|e| anyhow::anyhow!("保存对话元数据失败: {}", e))?;

                    debug!(
                        "成功保存对话元数据到数据库: session_id={}, response_id={}",
                        data.session_id, response_id
                    );
                },
                _ => {
                    // 停止写入旧表以避免与新表重复
                    debug!(
                        "跳过未知数据类型写入旧表: session_id={}, response_id={}",
                        data.session_id, response_id
                    );
                    return Ok(());
                },
            }

            Ok(())
        }
    }

    #[async_trait]
    impl SessionDataStore for PgSessionDataStore {
        async fn save_asr_audio_data(&self, response_id: &str, session_id: &str, user_audio_chunks: Option<Bytes>, user_audio_metadata: Option<AudioMetadata>) -> anyhow::Result<()> {
            let mut data = SessionTurnData::new(session_id.to_string(), "".to_string());

            if let (Some(audio_chunks), Some(audio_metadata_val)) = (user_audio_chunks, user_audio_metadata) {
                data.user_audio_chunks = Some(audio_chunks);
                data.user_audio_metadata = Some(audio_metadata_val);
            }

            data.metadata.insert("response_id".to_string(), serde_json::json!(response_id));
            data.metadata.insert("data_type".to_string(), serde_json::json!("asr_audio"));

            self.write_tx
                .send(data)
                .await
                .map_err(|e| anyhow::anyhow!("发送数据到写入队列失败: {}", e))?;

            debug!("ASR音频数据已排队保存: session_id={}, response_id={}", session_id, response_id);
            Ok(())
        }

        async fn save_tts_audio_data(&self, response_id: &str, session_id: &str, tts_output_audio: Option<Bytes>, tts_audio_metadata: Option<AudioMetadata>) -> anyhow::Result<()> {
            let mut data = SessionTurnData::new(session_id.to_string(), "".to_string());

            if let (Some(output_audio), Some(tts_metadata_val)) = (tts_output_audio, tts_audio_metadata) {
                data.tts_output_audio = Some(output_audio);
                data.tts_audio_metadata = Some(tts_metadata_val);
            }

            data.metadata.insert("response_id".to_string(), serde_json::json!(response_id));
            data.metadata.insert("data_type".to_string(), serde_json::json!("tts_audio"));

            self.write_tx
                .send(data)
                .await
                .map_err(|e| anyhow::anyhow!("发送数据到写入队列失败: {}", e))?;

            debug!("TTS音频数据已排队保存: session_id={}, response_id={}", session_id, response_id);
            Ok(())
        }

        async fn save_conversation_metadata(
            &self,
            response_id: &str,
            session_id: &str,
            llm_to_tts_text: String,
            metadata: Option<std::collections::HashMap<String, serde_json::Value>>,
        ) -> anyhow::Result<()> {
            let mut data = SessionTurnData::new(session_id.to_string(), llm_to_tts_text);

            if let Some(meta) = metadata {
                let mut fx_meta = FxHashMap::default();
                for (k, v) in meta {
                    fx_meta.insert(k, v);
                }
                fx_meta.insert("response_id".to_string(), serde_json::json!(response_id));
                fx_meta.insert("data_type".to_string(), serde_json::json!("conversation_metadata"));
                data.metadata = fx_meta;
            } else {
                data.metadata.insert("response_id".to_string(), serde_json::json!(response_id));
                data.metadata
                    .insert("data_type".to_string(), serde_json::json!("conversation_metadata"));
            }

            self.write_tx
                .send(data)
                .await
                .map_err(|e| anyhow::anyhow!("发送数据到写入队列失败: {}", e))?;

            debug!("对话元数据已排队保存: session_id={}, response_id={}", session_id, response_id);
            Ok(())
        }

        async fn save_vision_image_data(
            &self,
            response_id: &str,
            session_id: &str,
            image_data: Bytes,
            image_metadata: super::ImageMetadata,
            user_prompt: Option<String>,
            llm_response: Option<String>,
        ) -> anyhow::Result<()> {
            // 直接保存到数据库（不使用SessionTurnData结构）
            let query = r#"
                INSERT INTO vision_image_data (
                    response_id, session_id, image_data,
                    image_metadata, user_prompt, llm_response, created_at, updated_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, NOW(), NOW())
                ON CONFLICT (response_id)
                DO UPDATE SET
                    image_data = EXCLUDED.image_data,
                    image_metadata = EXCLUDED.image_metadata,
                    user_prompt = EXCLUDED.user_prompt,
                    llm_response = EXCLUDED.llm_response,
                    updated_at = NOW()
            "#;

            let image_metadata_json = serde_json::to_value(&image_metadata).map_err(|e| anyhow::anyhow!("序列化图片元数据失败: {}", e))?;

            sqlx::query(query)
                .bind(response_id)
                .bind(session_id)
                .bind(image_data.as_ref())
                .bind(image_metadata_json)
                .bind(user_prompt.as_deref())
                .bind(llm_response.as_deref())
                .execute(&self.pool)
                .await
                .map_err(|e| anyhow::anyhow!("保存Vision图片数据失败: {}", e))?;

            debug!("Vision图片数据已保存: session_id={}, response_id={}", session_id, response_id);
            Ok(())
        }
    }
}

/// 内存存储实现（用于开发/测试，或PG不可用时的fallback）
pub mod in_memory_store {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use tokio::sync::RwLock;
    use tracing::debug;

    /// 内存会话数据存储实现（纯写入，用于数据归档）
    #[derive(Debug)]
    pub struct InMemorySessionDataStore {
        data: RwLock<HashMap<String, Vec<SessionTurnData>>>,
    }

    impl InMemorySessionDataStore {
        pub fn new() -> Self {
            Self { data: RwLock::new(HashMap::new()) }
        }
    }

    impl Default for InMemorySessionDataStore {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait]
    impl SessionDataStore for InMemorySessionDataStore {
        async fn save_asr_audio_data(&self, response_id: &str, session_id: &str, user_audio_chunks: Option<Bytes>, user_audio_metadata: Option<AudioMetadata>) -> anyhow::Result<()> {
            let mut data = SessionTurnData::new(session_id.to_string(), "".to_string());

            if let (Some(audio_chunks), Some(audio_metadata_val)) = (user_audio_chunks, user_audio_metadata) {
                data.user_audio_chunks = Some(audio_chunks);
                data.user_audio_metadata = Some(audio_metadata_val);
            }

            data.metadata.insert("response_id".to_string(), serde_json::json!(response_id));
            data.metadata.insert("data_type".to_string(), serde_json::json!("asr_audio"));

            let mut data_map = self.data.write().await;
            let session_data = data_map.entry(session_id.to_string()).or_insert_with(Vec::new);
            session_data.push(data);

            debug!(
                "💾 内存存储：保存ASR音频数据: session_id={}, response_id={}",
                session_id, response_id
            );
            Ok(())
        }

        async fn save_tts_audio_data(&self, response_id: &str, session_id: &str, tts_output_audio: Option<Bytes>, tts_audio_metadata: Option<AudioMetadata>) -> anyhow::Result<()> {
            let mut data = SessionTurnData::new(session_id.to_string(), "".to_string());

            if let (Some(output_audio), Some(tts_metadata_val)) = (tts_output_audio, tts_audio_metadata) {
                data.tts_output_audio = Some(output_audio);
                data.tts_audio_metadata = Some(tts_metadata_val);
            }

            data.metadata.insert("response_id".to_string(), serde_json::json!(response_id));
            data.metadata.insert("data_type".to_string(), serde_json::json!("tts_audio"));

            let mut data_map = self.data.write().await;
            let session_data = data_map.entry(session_id.to_string()).or_insert_with(Vec::new);
            session_data.push(data);

            debug!(
                "💾 内存存储：保存TTS音频数据: session_id={}, response_id={}",
                session_id, response_id
            );
            Ok(())
        }

        async fn save_conversation_metadata(
            &self,
            response_id: &str,
            session_id: &str,
            llm_to_tts_text: String,
            metadata: Option<std::collections::HashMap<String, serde_json::Value>>,
        ) -> anyhow::Result<()> {
            let mut data = SessionTurnData::new(session_id.to_string(), llm_to_tts_text);

            if let Some(meta) = metadata {
                let mut fx_meta = FxHashMap::default();
                for (k, v) in meta {
                    fx_meta.insert(k, v);
                }
                fx_meta.insert("response_id".to_string(), serde_json::json!(response_id));
                fx_meta.insert("data_type".to_string(), serde_json::json!("conversation_metadata"));
                data.metadata = fx_meta;
            } else {
                data.metadata.insert("response_id".to_string(), serde_json::json!(response_id));
                data.metadata
                    .insert("data_type".to_string(), serde_json::json!("conversation_metadata"));
            }

            let mut data_map = self.data.write().await;
            let session_data = data_map.entry(session_id.to_string()).or_insert_with(Vec::new);
            session_data.push(data);

            debug!(
                "💾 内存存储：保存对话元数据: session_id={}, response_id={}",
                session_id, response_id
            );
            Ok(())
        }

        async fn save_vision_image_data(
            &self,
            response_id: &str,
            session_id: &str,
            image_data: Bytes,
            image_metadata: super::ImageMetadata,
            user_prompt: Option<String>,
            llm_response: Option<String>,
        ) -> anyhow::Result<()> {
            let vision_data = super::VisionImageData {
                response_id: response_id.to_string(),
                session_id: session_id.to_string(),
                image_data: Some(image_data),
                image_metadata,
                user_prompt,
                llm_response,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };

            let mut data = SessionTurnData::new(session_id.to_string(), "".to_string());
            data.metadata.insert("response_id".to_string(), serde_json::json!(response_id));
            data.metadata.insert("data_type".to_string(), serde_json::json!("vision_image"));
            data.metadata
                .insert("vision_data".to_string(), serde_json::to_value(&vision_data)?);

            let mut data_map = self.data.write().await;
            let session_data = data_map.entry(session_id.to_string()).or_insert_with(Vec::new);
            session_data.push(data);

            debug!(
                "💾 内存存储：保存Vision图片数据: session_id={}, response_id={}",
                session_id, response_id
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn test_session_turn_data_creation() {
        let data = SessionTurnData::new("test_session".to_string(), "Hi there!".to_string());

        assert_eq!(data.session_id, "test_session");
        assert_eq!(data.llm_to_tts_text, "Hi there!");
        assert!(data.user_audio_chunks.is_none());
        assert!(data.tts_output_audio.is_none());
    }

    #[test]
    fn test_session_turn_data_with_audio() {
        let audio_metadata = AudioMetadata {
            format: "opus".to_string(),
            sample_rate: 16000,
            channels: 1,
            duration_ms: 1000,
            size_bytes: 5,
        };

        let user_audio = Bytes::from(vec![1, 2, 3]);
        let tts_audio = Bytes::from(vec![4, 5, 6]);

        let data = SessionTurnData::with_complete_data(
            "test_session".to_string(),
            user_audio.clone(),
            audio_metadata.clone(),
            "Hi there!".to_string(),
            tts_audio.clone(),
            audio_metadata.clone(),
        );

        assert_eq!(data.session_id, "test_session");
        assert_eq!(data.llm_to_tts_text, "Hi there!");
        assert_eq!(data.user_audio_chunks, Some(user_audio));
        assert_eq!(data.tts_output_audio, Some(tts_audio));
    }

    #[test]
    fn test_generate_key() {
        let data = SessionTurnData::new("test_session".to_string(), "Hi there!".to_string());

        let key = data.generate_key();
        assert!(key.contains("test_session"));
        assert!(key.contains("_"));
    }

    #[tokio::test]
    async fn test_in_memory_store_save_methods() {
        let store = in_memory_store::InMemorySessionDataStore::new();

        // 测试保存 ASR 音频
        let audio_metadata = AudioMetadata {
            format: "pcm_s16le".to_string(),
            sample_rate: 16000,
            channels: 1,
            duration_ms: 1000,
            size_bytes: 32000,
        };
        store
            .save_asr_audio_data(
                "resp_1",
                "session_1",
                Some(Bytes::from(vec![1, 2, 3])),
                Some(audio_metadata.clone()),
            )
            .await
            .unwrap();

        // 测试保存 TTS 音频
        store
            .save_tts_audio_data("resp_1", "session_1", Some(Bytes::from(vec![4, 5, 6])), Some(audio_metadata))
            .await
            .unwrap();

        // 测试保存对话元数据
        store
            .save_conversation_metadata("resp_1", "session_1", "Hello world".to_string(), None)
            .await
            .unwrap();
    }
}
