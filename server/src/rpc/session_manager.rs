use crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::SimpleInterruptManager;
use crate::rpc::{
    message_adapter::WsMessage,
    pipeline::StreamingPipeline,
    protocol::{self, ProtocolId},
    session_router::SessionRouter,
};
use anyhow::Result;
use dashmap::DashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, mpsc};
use tracing::{debug, error, info, warn};

/// 回调清理守卫 - 在析构时执行回调函数
struct CallbackGuard<F: FnOnce()> {
    callback: Option<F>,
}

impl<F: FnOnce()> CallbackGuard<F> {
    fn new(callback: F) -> Self {
        Self { callback: Some(callback) }
    }
}

impl<F: FnOnce()> Drop for CallbackGuard<F> {
    fn drop(&mut self) {
        if let Some(callback) = self.callback.take() {
            callback();
        }
    }
}

/// 会话管理器 - 专注于会话生命周期管理
pub struct SessionManager {
    /// 会话路由器
    router: Arc<SessionRouter>,
    /// 活跃的 Pipeline 实例 (session_id -> Pipeline)
    active_pipelines: Arc<DashMap<String, Arc<dyn StreamingPipeline + Send + Sync>>>,
    /// Pipeline 清理守卫 (session_id -> CleanupGuard)
    cleanup_guards: Arc<DashMap<String, crate::rpc::pipeline::CleanupGuard>>,
    /// 🚀 新增：记录成为孤儿（未绑定路由会话）的时间，用于自动超时回收
    orphan_since: Arc<DashMap<String, Instant>>,
    /// 对话持久化存储
    store: Arc<dyn crate::storage::ConversationStore>,
    /// Pipeline工厂
    pipeline_factory: Arc<dyn PipelineFactory + Send + Sync>,
    /// 正在创建中的会话通知器（事件驱动，避免忙等）
    creation_notifiers: Arc<DashMap<String, Arc<Notify>>>,
}

/// Pipeline工厂特征
#[async_trait::async_trait]
pub trait PipelineFactory: Send + Sync {
    async fn create_pipeline(
        &self,
        session_id: &str,
        connection_id: &str,
        protocol_id: ProtocolId,
        speech_mode: crate::asr::SpeechMode,
        payload: Option<&protocol::MessagePayload>,
    ) -> Result<Arc<dyn StreamingPipeline + Send + Sync>, String>;
}

impl SessionManager {
    pub fn new(router: Arc<SessionRouter>, store: Arc<dyn crate::storage::ConversationStore>, pipeline_factory: Arc<dyn PipelineFactory + Send + Sync>) -> Self {
        Self {
            router,
            active_pipelines: Arc::new(DashMap::new()),
            cleanup_guards: Arc::new(DashMap::new()),
            orphan_since: Arc::new(DashMap::new()),
            store,
            pipeline_factory,
            creation_notifiers: Arc::new(DashMap::new()),
        }
    }

    // get_session_timezone_location方法已删除 - 现在动态从IP地理位置获取

    /// 🆕 获取 VisionTts 继承所需的全部会话配置（一次 downcast）
    /// 返回: (打断管理器, text_done_only, signal_only, 输出配置, 节拍参数, TTS配置, 语音设置, 繁简转换模式)
    #[allow(clippy::type_complexity)]
    pub async fn get_session_vision_inherit(
        &self,
        session_id: &str,
    ) -> (
        Option<Arc<SimpleInterruptManager>>,
        Option<Arc<std::sync::atomic::AtomicBool>>,
        Option<Arc<std::sync::atomic::AtomicBool>>,
        Option<crate::audio::OutputAudioConfig>,
        Option<(usize, u64, f64)>,
        Option<crate::tts::minimax::MiniMaxConfig>,
        Option<crate::tts::minimax::VoiceSetting>,
        Option<crate::text_filters::ConvertMode>,
    ) {
        if let Some(pipeline) = self.active_pipelines.get(session_id)
            && let Some(mp) = pipeline
                .as_any()
                .downcast_ref::<crate::rpc::pipeline::asr_llm_tts::orchestrator::ModularPipeline>()
        {
            let chinese_convert = mp.get_shared_flags().tts_chinese_convert_mode.read().ok().map(|g| *g);
            return (
                Some(mp.get_simple_interrupt_manager().clone()),
                Some(mp.get_text_done_signal_only_flag()),
                Some(mp.get_signal_only_flag()),
                Some(mp.get_audio_output_config().await),
                Some(mp.get_pacing_params()),
                mp.get_tts_config(),
                mp.get_voice_setting(),
                chinese_convert,
            );
        }
        (None, None, None, None, None, None, None, None)
    }

    /// 🆕 获取底层路由器（用于在不注册Pipeline时直接发送事件）
    pub fn get_router(&self) -> Arc<SessionRouter> {
        self.router.clone()
    }

    /// 🚀 启动整条Pipeline的自动超时回收任务：
    /// 当某个Pipeline对应的会话在路由中不存在（断开/超时被移除），
    /// 且在 router.timeout() 时长内未被重新绑定，则自动销毁该Pipeline。
    /// 同时检查VisionTtsPipeline的一次性销毁标志。
    pub fn spawn_orphan_reclaimer(self: &Arc<Self>, router: Arc<SessionRouter>) {
        let this = Arc::clone(self);
        let orphan_timeout = Duration::from_secs(900); // 15分钟
        let check_interval = if orphan_timeout.as_secs() >= 2 {
            orphan_timeout / 2
        } else {
            Duration::from_secs(1)
        };

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(check_interval).await;

                // 复制当前所有活跃的 session_id，避免持锁期间做破坏性操作
                let session_ids: Vec<String> = this.active_pipelines.iter().map(|e| e.key().clone()).collect();

                for sid in session_ids {
                    // 检查VisionTtsPipeline的一次性销毁标志
                    if let Some(pipeline) = this.active_pipelines.get(&sid)
                        && let Some(vision_pipeline) = pipeline
                            .as_any()
                            .downcast_ref::<crate::rpc::pipeline::vision_tts::streaming_pipeline::VisionTtsPipeline>()
                        && vision_pipeline.should_destroy.load(std::sync::atomic::Ordering::Acquire)
                    {
                        tracing::info!("🗑️ 检测到VisionTtsPipeline标记销毁，开始清理: session_id={}", sid);
                        if let Err(e) = this.destroy_session(&sid).await {
                            tracing::warn!("⚠️ 自动销毁VisionTtsPipeline失败: session_id={}, error={}", sid, e);
                        } else {
                            this.orphan_since.remove(&sid);
                        }
                        continue;
                    }

                    // 如果路由侧仍存在该会话，则清除孤儿标记
                    if router.contains_session(&sid) {
                        this.orphan_since.remove(&sid);
                        continue;
                    }

                    // 标记或检查成为孤儿的时间
                    let now = Instant::now();
                    match this.orphan_since.get(&sid) {
                        Some(entered) => {
                            if now.duration_since(*entered.value()) >= orphan_timeout {
                                tracing::info!("🧹 Pipeline孤儿超时，开始自动销毁: session_id={}", sid);
                                // 调用会话销毁（触发 CleanupGuard 完整清理）
                                if let Err(e) = this.destroy_session(&sid).await {
                                    tracing::warn!("⚠️ 自动销毁Pipeline失败: session_id={}, error={}", sid, e);
                                } else {
                                    // 清理完成后移除孤儿标记
                                    this.orphan_since.remove(&sid);
                                }
                            }
                        },
                        None => {
                            // 第一次发现为孤儿，记录时间
                            this.orphan_since.insert(sid.clone(), now);
                            tracing::info!("⏳ 检测到Pipeline成为孤儿，进入回收计时: session_id={}", sid);
                        },
                    }
                }
            }
        });
    }

    /// 注册WebSocket连接
    pub async fn register_connection(&self, connection_id: String, ws_sender: mpsc::UnboundedSender<WsMessage>) {
        self.router.register_connection(connection_id, ws_sender).await;
    }

    /// 注销WebSocket连接
    pub async fn unregister_connection(&self, connection_id: &str) -> usize {
        // 🧹 中心化清理：连接级元数据在连接断开时必须移除，避免缓存长期增长
        // 注意：即使 actix_websocket 的 ConnectionMetadataCleanupGuard 也会清理，这里重复 remove 是幂等的（KISS）
        crate::rpc::connection_metadata::CONNECTION_METADATA_CACHE.remove(connection_id);
        self.handle_websocket_disconnect(connection_id).await
    }

    /// 创建会话
    pub async fn create_session(
        &self,
        session_id: &str,
        connection_id: &str,
        protocol_id: ProtocolId,
        speech_mode: crate::asr::SpeechMode,
        payload: Option<&protocol::MessagePayload>,
    ) -> Result<(), String> {
        let create_session_start = std::time::Instant::now();
        info!(
            "🆕 处理会话创建请求: session_id={}, connection_id={}, protocol_id={:?}",
            session_id, connection_id, protocol_id
        );

        // 并发保护（事件驱动）：针对相同 session_id 仅允许一个创建流程，其它并发调用等待通知
        use dashmap::mapref::entry::Entry;
        let session_key = session_id.to_string();
        let _notify_leader_guard;
        loop {
            match self.creation_notifiers.entry(session_key.clone()) {
                Entry::Occupied(entry) => {
                    let notify = entry.get().clone();
                    drop(entry);
                    info!("⏳ 检测到并发创建请求，等待之前的创建完成: session_id={}", session_id);
                    notify.notified().await;
                    // 之前的创建流程已结束，若Pipeline已存在则直接返回
                    if self.active_pipelines.contains_key(session_id) {
                        info!("✅ 并发创建已完成，session已存在: session_id={}", session_id);
                        return Ok(());
                    }
                    // 否则继续循环争取成为新的leader发起创建
                    continue;
                },
                Entry::Vacant(vacant) => {
                    let notify = Arc::new(Notify::new());
                    vacant.insert(notify.clone());
                    // 在函数结束时移除并通知所有等待者
                    _notify_leader_guard = CallbackGuard::new({
                        let creation_notifiers = self.creation_notifiers.clone();
                        let session_key_cloned = session_key.clone();
                        move || {
                            if let Some((_, n)) = creation_notifiers.remove(&session_key_cloned) {
                                n.notify_waiters();
                            }
                        }
                    });
                    break; // 成为leader，继续创建流程
                },
            }
        }

        // 🚀 关键：在调用router之前保存旧的protocol_id，用于后续检测Protocol切换
        let old_protocol_id_before_router = self.router.get_session_protocol_id(session_id);

        // 🔧 修复：首先调用router的create_virtual_session，它会正确处理重复请求
        let upstream_receiver = self
            .router
            .create_virtual_session(session_id, connection_id, protocol_id)
            .await?;

        // 🆕 创建 SessionContext（统一的 session 级别配置管理）
        crate::agents::turn_tracker::create_session_with_connection(session_id, connection_id).await;
        tracing::debug!(
            "🆕 创建 SessionContext: session_id={}, connection_id={}",
            session_id,
            connection_id
        );

        // 🌍 处理用户提供的时区、位置和ASR语言信息（注意：优先存储到 SESSION_METADATA）
        let (user_timezone, user_location, asr_language) = if let Some(payload) = payload {
            match payload {
                protocol::MessagePayload::SessionConfig { timezone, location, asr_language, .. } => (timezone.clone(), location.clone(), asr_language.clone()),
                _ => (None, None, None),
            }
        } else {
            (None, None, None)
        };

        // 🧭 新规则：按1-3合并后处理location
        // - 若结果为空(None或空字符串)，则异步使用IP->城市解析；不再走cleaner
        // - 若结果非空，则异步调用clean_location规范化
        let need_ip_city = user_location.as_ref().map(|s| s.trim().is_empty()).unwrap_or(true);

        if need_ip_city {
            let session_id_clone = session_id.to_string();
            let connection_id_clone = connection_id.to_string();
            let asr_language_clone = asr_language.clone();
            tracing::info!(
                "🌐 location为空，异步根据IP解析城市: session_id={}, connection_id={}",
                session_id_clone,
                connection_id_clone
            );

            tokio::spawn(async move {
                let client_ip = crate::rpc::connection_metadata::CONNECTION_METADATA_CACHE
                    .get(&connection_id_clone)
                    .and_then(|md| md.client_ip.clone());

                if let (Some(ip), Some(locator)) = (client_ip, crate::ip_geolocation::get_ip_geolocation_service()) {
                    // 使用 asr_language 获取对应语言的地名
                    match locator.lookup_with_language(&ip, asr_language_clone.as_deref()) {
                        Ok(geo) => {
                            if let Some(city) = geo.city {
                                // 🏙️ IP 解析得到的城市名直接使用（MaxMind 已经是标准格式），不需要 clean
                                crate::agents::turn_tracker::set_user_city(&session_id_clone, city.clone()).await;
                                tracing::info!("✅ IP城市解析完成并存储: session_id={}, city={}", session_id_clone, city);
                            } else {
                                tracing::warn!("⚠️ IP解析无城市信息: session_id={}", session_id_clone);
                            }
                        },
                        Err(e) => {
                            tracing::warn!("⚠️ IP地理位置解析失败: session_id={}, error={}", session_id_clone, e);
                        },
                    }
                } else {
                    tracing::warn!(
                        "⚠️ 无法进行IP城市解析：缺少IP或定位服务未初始化: session_id={}",
                        session_id_clone
                    );
                }
            });
        } else if let Some(ref loc) = user_location {
            let loc_clone = loc.clone();
            let session_id_clone = session_id.to_string();
            tracing::info!("🌍 异步规范化位置: session_id={}, 原始location={}", session_id, loc);

            tokio::spawn(async move {
                match crate::ip_geolocation::clean_location(&loc_clone).await {
                    Some(cleaned_city) => {
                        // 🆕 将规范化后的位置存储到 SessionContext
                        crate::agents::turn_tracker::set_user_city(&session_id_clone, cleaned_city.clone()).await;
                        tracing::info!(
                            "✅ 位置规范化完成（session级别）: session_id={}, 原始={}, 规范化={}",
                            session_id_clone,
                            loc_clone,
                            cleaned_city
                        );
                    },
                    None => {
                        tracing::warn!(
                            "⚠️ 位置规范化失败，保持原始值: session_id={}, location={}",
                            session_id_clone,
                            loc_clone
                        );
                    },
                }
            });
        }

        // 🆕 将用户提供的配置存储到 SessionContext
        if user_timezone.is_some() || user_location.is_some() || asr_language.is_some() {
            crate::agents::turn_tracker::set_session_user_info(
                session_id,
                None, // user_ip 稍后从 connection metadata 获取
                user_location.clone(),
                user_timezone.clone(),
                asr_language.clone(),
            )
            .await;
            tracing::info!(
                "🕒 用户提供时区/位置/ASR语言信息（session级别）: session_id={}, timezone={:?}, location={:?}, asr_language={:?}",
                session_id,
                user_timezone,
                user_location,
                asr_language
            );
        }

        // 🔧 关键修复：根据router的返回值决定后续操作
        match upstream_receiver {
            None => {
                // router返回None表示这是重复请求（同连接、同session），默认会被忽略
                // 🔧 按需求：在重复Start时，如果携带新的配置，则对现有Pipeline执行热更新
                info!(
                    "♻️ 检测到重复的startSession请求: session_id={}, connection_id={}",
                    session_id, connection_id
                );

                if let Some(payload) = payload {
                    if let Some(_pipeline) = self.active_pipelines.get(session_id) {
                        match self.update_session_configuration(session_id, payload).await {
                            Ok(_) => {
                                info!("✅ 重复Start已应用会话配置更新: session_id={}", session_id);
                            },
                            Err(e) => {
                                warn!("⚠️ 重复Start配置更新失败: session_id={}, error={}", session_id, e);
                            },
                        }
                    } else {
                        info!("⚠️ 重复Start携带配置，但Pipeline尚未创建，跳过更新: session_id={}", session_id);
                    }
                } else {
                    info!("🔁 重复Start未包含配置，跳过更新: session_id={}", session_id);
                }
                Ok(())
            },
            Some(receiver) => {
                // router返回Some(receiver)表示需要创建新Pipeline或重新启动上行任务

                // 🔧 使用原子操作检查Pipeline是否已存在
                if let Some(existing_pipeline) = self.active_pipelines.get(session_id) {
                    // 🚀 关键修复：检查protocol_id是否变化，如果变化则强制销毁并重建Pipeline
                    // 使用调用router之前保存的旧值，避免时序bug
                    let protocol_changed = old_protocol_id_before_router.is_some_and(|old| old != protocol_id);

                    if protocol_changed {
                        info!(
                            "🔄 检测到protocol_id变化（{:?} → {:?}），销毁旧Pipeline并重建: session_id={}",
                            old_protocol_id_before_router, protocol_id, session_id
                        );

                        // 释放existing_pipeline的引用，避免死锁
                        drop(existing_pipeline);

                        // 销毁旧Pipeline
                        if let Err(e) = self.destroy_session(session_id).await {
                            warn!("⚠️ 销毁旧Pipeline失败: session_id={}, error={}", session_id, e);
                        }

                        // 继续后续流程，创建新Pipeline（不要return）
                    } else {
                        // Protocol未变化，这是普通的会话重新绑定情况
                        info!("🔄 Pipeline已存在，重新启动上行消息处理任务: session_id={}", session_id);

                        // 🔧 新增：如果提供了新的配置，更新现有Pipeline的配置
                        if let Some(payload) = payload {
                            if let Err(e) = self.update_session_configuration(session_id, payload).await {
                                warn!("⚠️ 更新会话配置失败: session_id={}, error={}", session_id, e);
                            } else {
                                info!("✅ 会话配置已更新: session_id={}", session_id);
                            }
                        }

                        // 重新启动上行消息处理任务
                        let pipeline_clone = existing_pipeline.clone();
                        self.start_upstream_handler(session_id, Some(receiver), pipeline_clone).await;

                        info!("✅ 会话重新绑定完成: session_id={}", session_id);
                        return Ok(());
                    }
                }

                // Pipeline不存在，需要创建新的
                info!("🆕 创建新的Pipeline: session_id={}", session_id);

                // 保存会话配置
                self.save_session_config(session_id, protocol_id, speech_mode, payload).await;

                // 创建Pipeline
                let create_pipeline_start = std::time::Instant::now();
                let pipeline = self
                    .pipeline_factory
                    .create_pipeline(session_id, connection_id, protocol_id, speech_mode, payload)
                    .await?;
                info!(
                    "⏱️ [计时] create_pipeline 耗时: {:?} | session_id={}",
                    create_pipeline_start.elapsed(),
                    session_id
                );

                // 启动Pipeline
                let start_pipeline_start = std::time::Instant::now();
                let cleanup_guard = pipeline.start().await.map_err(|e| format!("启动Pipeline失败: {}", e))?;
                info!(
                    "⏱️ [计时] pipeline.start 耗时: {:?} | session_id={}",
                    start_pipeline_start.elapsed(),
                    session_id
                );

                // 保存清理守卫（防止立即被丢弃）
                self.cleanup_guards.insert(session_id.to_string(), cleanup_guard);

                // 启动上行消息处理任务
                self.start_upstream_handler(session_id, Some(receiver), pipeline.clone()).await;

                // 保存Pipeline引用
                self.active_pipelines.insert(session_id.to_string(), pipeline);

                info!(
                    "✅ 新会话创建成功: session_id={} | ⏱️ 总耗时: {:?}",
                    session_id,
                    create_session_start.elapsed()
                );
                Ok(())
            },
        }
    }

    /// 销毁会话
    pub async fn destroy_session(&self, session_id: &str) -> Result<(), String> {
        info!("🗑️ 销毁会话: {}", session_id);

        // 1. 从路由器中移除会话（这会关闭上行消息处理任务的接收器）
        self.router.remove_session(session_id).await;

        // 2. 移除并清理Pipeline
        if let Some((_, pipeline)) = self.active_pipelines.remove(session_id) {
            info!("🧹 开始异步清理Pipeline: {}", session_id);
            let session_id_for_cleanup = session_id.to_string();
            tokio::spawn(async move {
                info!("🧹 执行Pipeline清理: {}", session_id_for_cleanup);
                drop(pipeline);
                info!("✅ Pipeline清理完成: {}", session_id_for_cleanup);
            });
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // 3. 移除清理守卫（触发清理逻辑）
        if let Some((_, cleanup_guard)) = self.cleanup_guards.remove(session_id) {
            info!("🧹 移除清理守卫，触发Pipeline清理: {}", session_id);
            drop(cleanup_guard); // 这里会触发清理逻辑
        }

        // 4. 清理延迟计时数据
        let session_id_for_timing = session_id.to_string();
        tokio::spawn(async move {
            crate::rpc::pipeline::asr_llm_tts::timing_manager::cleanup_session_timing(&session_id_for_timing).await;
        });

        // 5. 移除 SessionContext（session级别的上下文管理）
        let session_id_for_cleanup = session_id.to_string();
        tokio::spawn(async move {
            crate::agents::turn_tracker::remove_tracker(&session_id_for_cleanup).await;
            tracing::info!("🧹 已移除 SessionContext: {}", session_id_for_cleanup);
        });

        // 6. 清理持久化/内存会话数据（避免长期驻留）
        // 🔧 修改：不再删除会话数据，以便用户重连时可以恢复上下文
        /*
        {
            let store = self.store.clone();
            let sid = session_id.to_string();
            tokio::spawn(async move {
                if let Err(e) = store.delete(&sid).await {
                    tracing::warn!("⚠️ 删除会话持久化数据失败: session_id={}, error={}", sid, e);
                } else {
                    tracing::info!("🧹 会话持久化数据已删除: {}", sid);
                }
            });
        }
        */
        tracing::info!("💾 保留会话持久化数据以支持恢复: {}", session_id);

        info!("✅ 会话销毁完成: {}", session_id);
        Ok(())
    }

    /// 重新绑定会话连接
    pub async fn rebind_session_connection(&self, session_id: &str, new_connection_id: &str, payload: Option<&protocol::MessagePayload>) -> Result<(), String> {
        info!(
            "🔄 重新绑定会话连接: session={}, new_connection={}",
            session_id, new_connection_id
        );

        if !self.active_pipelines.contains_key(session_id) {
            return Err(format!("会话Pipeline不存在，无法重新绑定: {}", session_id));
        }

        // 🔧 新增：如果提供了新的配置，更新现有Pipeline的配置
        if let Some(payload) = payload {
            if let Err(e) = self.update_session_configuration(session_id, payload).await {
                warn!("⚠️ 更新会话配置失败: session_id={}, error={}", session_id, e);
            } else {
                info!("✅ 会话配置已更新: session_id={}", session_id);
            }
        }

        // 🔧 简化：直接调用create_virtual_session，它会处理重用逻辑
        // 🆕 从现有会话获取protocol_id（rebind场景下protocol_id不应变化）
        let protocol_id = self
            .router
            .get_session_protocol_id(session_id)
            .ok_or_else(|| format!("无法获取会话协议类型: {}", session_id))?;
        let upstream_receiver = self
            .router
            .create_virtual_session(session_id, new_connection_id, protocol_id)
            .await?;

        // 🔧 关键修复：只有在返回Some(receiver)时才重新启动上行消息处理任务
        if let Some(receiver) = upstream_receiver {
            if let Some(pipeline) = self.active_pipelines.get(session_id) {
                self.start_upstream_handler(session_id, Some(receiver), pipeline.clone()).await;
                info!("✅ 会话 {} 重新启动了上行消息处理任务", session_id);
            }
        } else {
            info!("✅ 会话 {} 连接已是最新，无需重新启动上行消息处理任务", session_id);
        }

        info!("✅ 会话 {} 重新绑定到连接 {} 完成", session_id, new_connection_id);
        Ok(())
    }

    /// 转发消息到Pipeline
    /// 转发消息到Pipeline（带连接绑定校验）。
    ///
    /// 用于处理 WS 重连场景：同一 session_id 在短时间内可能同时收到旧连接残留包与新连接的包。
    /// 通过 connection_id 校验可避免旧连接干扰当前会话管线。
    pub async fn forward_message_from_connection(&self, session_id: &str, connection_id: &str, message: WsMessage) -> Result<(), String> {
        self.router
            .forward_upstream_from_connection(session_id, connection_id, message)
            .await
    }

    /// 处理工具调用结果
    pub async fn handle_tool_call_result(&self, session_id: &str, tool_result: crate::rpc::pipeline::asr_llm_tts::tool_call_manager::ToolCallResult) -> Result<()> {
        if let Some(pipeline) = self.active_pipelines.get(session_id) {
            pipeline.handle_tool_call_result(tool_result).await
        } else {
            Err(anyhow::anyhow!("会话不存在: {}", session_id))
        }
    }

    /// 获取活跃会话数量
    pub async fn get_active_session_count(&self) -> usize {
        self.router.active_session_count().await
    }

    /// 检查会话是否存在
    pub fn contains_session(&self, session_id: &str) -> bool {
        self.active_pipelines.contains_key(session_id)
    }

    /// 获取指定连接下的所有会话ID
    pub async fn get_session_ids_for_connection(&self, connection_id: &str) -> Vec<String> {
        self.router.session_ids_for_connection(connection_id)
    }

    /// 获取指定连接的WebSocket发送器
    pub fn get_connection_sender(&self, connection_id: &str) -> Option<mpsc::UnboundedSender<WsMessage>> {
        self.router.get_connection_sender(connection_id)
    }

    /// 🆕 直接通过 session_id 获取 session 的完整 metadata（用于 LLM 等组件）
    ///
    /// 返回：(user_timezone, user_location, asr_language, connection_id, connection_metadata)
    /// - user_timezone/user_location/asr_language 来自 SessionContext
    /// - connection_metadata 用于获取 client_ip（用于实时查询 IP geolocation）
    pub async fn get_session_metadata(
        &self,
        session_id: &str,
    ) -> Option<(
        Option<String>,                                              // user_timezone (from session)
        Option<String>,                                              // user_location (from session)
        Option<String>,                                              // asr_language (from session)
        Option<String>,                                              // connection_id
        Option<crate::rpc::connection_metadata::ConnectionMetadata>, // connection metadata (for IP)
    )> {
        // 1. 从 SessionContext 获取 session 特定的信息
        let session_info = crate::agents::turn_tracker::get_session_info(session_id).await;

        // 2. 获取 connection_id（优先从 SessionContext，fallback 到 router）
        let connection_id = if let Some((_, _, _, ref conn_id)) = session_info {
            conn_id.clone()
        } else {
            self.router.get_connection_id_for_session(session_id)
        };

        // 3. 获取 connection metadata
        let conn_meta = connection_id.as_ref().and_then(|cid| {
            crate::rpc::connection_metadata::CONNECTION_METADATA_CACHE
                .get(cid)
                .map(|m| m.clone())
        });

        // 4. 从 session_info 获取配置
        let (user_timezone, user_location, asr_language) = if let Some((tz, city, lang, _)) = session_info {
            (tz, city, lang)
        } else {
            (None, None, None)
        };

        Some((user_timezone, user_location, asr_language, connection_id, conn_meta))
    }

    /// 🔧 新增：为指定会话发送精确打断信号，清理当前活跃的响应
    pub async fn send_precise_interrupt_for_session(&self, session_id: &str) -> Result<bool, String> {
        info!("🎯 准备为会话发送精确打断信号: session_id={}", session_id);

        if let Some(pipeline) = self.active_pipelines.get(session_id) {
            // 尝试将 Pipeline 转型为 ModularPipeline
            if let Some(modular_pipeline) = pipeline
                .as_ref()
                .as_any()
                .downcast_ref::<crate::rpc::pipeline::asr_llm_tts::orchestrator::ModularPipeline>()
            {
                // 检查是否有活跃的响应
                let shared_flags = modular_pipeline.get_shared_flags();
                let is_responding = *shared_flags.is_responding_rx.borrow();

                if is_responding {
                    // 获取当前响应上下文用于精确打断
                    if let Some(context) = shared_flags.assistant_response_context.get_context_copy() {
                        info!(
                            "🎯 检测到活跃响应，发送精确打断: session_id={}, response_id={}",
                            session_id, context.response_id
                        );

                        // 🔧 使用UserSpeaking替代NewUserRequest，效果相同：打断当前TTS输出
                        modular_pipeline
                            .get_simple_interrupt_manager()
                            .broadcast_global_interrupt(
                                session_id.to_string(),
                                crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::InterruptReason::UserSpeaking,
                            )
                            .map_err(|e| format!("发送简化打断信号失败: {}", e))?;

                        info!("✅ 精确打断信号已发送: session_id={}", session_id);
                        Ok(true)
                    } else {
                        info!(
                            "⚠️ 检测到is_responding=true但无响应上下文，发送全局打断: session_id={}",
                            session_id
                        );

                        // 🔧 使用UserSpeaking替代NewUserRequest，效果相同：打断当前TTS输出
                        modular_pipeline
                            .get_simple_interrupt_manager()
                            .broadcast_global_interrupt(
                                session_id.to_string(),
                                crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::InterruptReason::UserSpeaking,
                            )
                            .map_err(|e| format!("发送全局简化打断信号失败: {}", e))?;

                        Ok(true)
                    }
                } else {
                    info!("✅ 当前无活跃响应，跳过打断: session_id={}", session_id);
                    Ok(false)
                }
            } else {
                warn!(
                    "⚠️ Pipeline不是ModularPipeline类型，无法发送精确打断: session_id={}",
                    session_id
                );
                Ok(false)
            }
        } else {
            Err(format!("会话Pipeline不存在: {}", session_id))
        }
    }

    // 私有方法

    /// 保存会话配置
    async fn save_session_config(&self, session_id: &str, protocol_id: ProtocolId, speech_mode: crate::asr::SpeechMode, _payload: Option<&protocol::MessagePayload>) {
        let config_json = serde_json::json!({
            "protocol_id": protocol_id as u8,
            "speech_mode": format!("{:?}", speech_mode),
            "created_at": chrono::Utc::now().to_rfc3339()
        });

        let record = crate::storage::ConversationRecord::new(session_id.to_string(), config_json);
        if let Err(e) = self.store.save(&record).await {
            warn!("⚠️ 保存会话配置失败: {}", e);
        }
    }

    /// 启动上行消息处理任务
    async fn start_upstream_handler(&self, session_id: &str, upstream_receiver: Option<mpsc::UnboundedReceiver<WsMessage>>, pipeline: Arc<dyn StreamingPipeline + Send + Sync>) {
        if let Some(mut receiver) = upstream_receiver {
            let session_id_clone = session_id.to_string();
            let active_pipelines = self.active_pipelines.clone();
            let router_ref = self.router.clone();

            tokio::spawn(async move {
                info!("📥 上行消息处理任务启动: {}", session_id_clone);

                let mut last_activity = std::time::Instant::now();
                // 🔧 修复：延长超时时间，避免在会话重用场景下过早退出
                const ACTIVITY_TIMEOUT: Duration = Duration::from_secs(300); // 5分钟，与会话超时一致
                const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(30); // 30秒检查一次

                loop {
                    tokio::select! {
                        message_result = receiver.recv() => {
                            match message_result {
                                Some(message) => {
                                    last_activity = std::time::Instant::now();
                                    if let WsMessage::Binary(data) = message
                                        && let Err(e) = Self::process_binary_message(&pipeline, &data).await {
                                            error!("处理上行消息失败: {}, session={}", e, session_id_clone);
                                            // 如果是音频解码相关错误，可能是Opus解码问题，不应该导致连接关闭
                                            let error_str = e.to_string();
                                            if error_str.contains("空音频数据")
                                                || error_str.contains("buffer too small")
                                                || error_str.contains("Opus解码错误") {
                                                debug!("检测到音频解码错误，可能是Opus解码问题，继续保持连接: {}", error_str);
                                                continue;
                                            } else {
                                                break;
                                            }
                                        }
                                },
                                None => {
                                    info!("📥 上行接收器已关闭，结束处理任务: {}", session_id_clone);
                                    break;
                                }
                            }
                        },
                        _ = tokio::time::sleep(HEALTH_CHECK_INTERVAL) => {
                            // 🔧 修复：只有在Pipeline被移除时才退出，不再基于活动超时
                            if !active_pipelines.contains_key(&session_id_clone) {
                                info!("🔍 Pipeline已被移除，上行消息处理任务退出: {}", session_id_clone);
                                break;
                            }

                            // 🆕 改进：当路由侧已不存在该会话时，主动退出，避免空转
                            if !router_ref.contains_session(&session_id_clone) {
                                info!("🔍 路由不存在此会话，上行消息处理任务退出: {}", session_id_clone);
                                break;
                            }

                            // 🔧 新增：记录活动状态但不退出，支持长时间会话
                            if last_activity.elapsed() > ACTIVITY_TIMEOUT {
                                info!("⏰ 上行消息处理任务长时间无活动: {} ({}分钟)，但Pipeline仍存在，继续等待",
                                    session_id_clone, ACTIVITY_TIMEOUT.as_secs() / 60);
                                // 重置活动时间，避免重复日志
                                last_activity = std::time::Instant::now();
                            }
                        }
                    }
                }

                // 🔧 修复：只有在Pipeline确实被移除时才从active_pipelines中移除
                // 避免在会话重用场景下误删Pipeline
                if !active_pipelines.contains_key(&session_id_clone) {
                    active_pipelines.remove(&session_id_clone);
                }
                info!("📥 上行消息处理任务结束: {}", session_id_clone);
            });
        }
    }

    /// 处理二进制消息
    async fn process_binary_message(pipeline: &Arc<dyn StreamingPipeline + Send + Sync>, data: &bytes::Bytes) -> Result<(), String> {
        if data.len() < protocol::BINARY_HEADER_SIZE {
            return Err("二进制包长度过小".to_string());
        }

        let binary_msg = protocol::BinaryMessage {
            header: protocol::BinaryHeader::from_bytes(&data[..protocol::BINARY_HEADER_SIZE]).map_err(|_| "解析二进制包头失败")?,
            payload: data[protocol::BINARY_HEADER_SIZE..].to_vec(),
        };

        // 🎯 TRACE: 音频包转发到Pipeline处理
        // if binary_msg.header.command_id == crate::rpc::protocol::CommandId::AudioChunk {
        //     info!(
        //         "🎤 [TRACE-AUDIO] 转发音频包到Pipeline | session_id={} | payload_size={} bytes | command_id={:?}",
        //         binary_msg.session_id(),
        //         binary_msg.payload.len(),
        //         binary_msg.header.command_id
        //     );
        // }

        let process_result = tokio::time::timeout(Duration::from_secs(5), pipeline.on_upstream(binary_msg)).await;

        match process_result {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(format!("Pipeline处理失败: {}", e)),
            Err(_) => Err("处理上行消息超时".to_string()),
        }
    }

    /// 处理WebSocket意外断开
    async fn handle_websocket_disconnect(&self, connection_id: &str) -> usize {
        info!("🔌 处理WebSocket断开: {}, 保留Pipeline等待重连", connection_id);

        // 🔧 关键修复：先获取会话列表，然后直接注销连接，避免逐个移除会话导致的状态不一致
        let session_ids = self.router.session_ids_for_connection(connection_id);
        info!(
            "🔗 连接 {} 下有 {} 个会话将保留Pipeline等待重连",
            connection_id,
            session_ids.len()
        );

        // 🚀 第一步：发送暂停信号（而不是关闭信号），让Pipeline暂停当前操作但保持运行
        for session_id in &session_ids {
            if let Some(pipeline) = self.active_pipelines.get(session_id) {
                // 尝试发送暂停信号给ModularPipeline
                if let Some(modular_pipeline) = pipeline
                    .as_any()
                    .downcast_ref::<crate::rpc::pipeline::asr_llm_tts::orchestrator::ModularPipeline>()
                {
                    info!("⏸️ 发送连接断开暂停信号: {}", session_id);
                    let _ = modular_pipeline.get_simple_interrupt_manager().broadcast_global_interrupt(
                        session_id.to_string(),
                        crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::InterruptReason::ConnectionLost,
                    );

                    // 🔧 如果处于同传模式，截断同传期间的 turns
                    let shared_flags = modular_pipeline.get_shared_flags();
                    if shared_flags.simul_interpret_enabled.load(std::sync::atomic::Ordering::Acquire) {
                        let start_count = shared_flags
                            .simul_interpret_turn_start_count
                            .load(std::sync::atomic::Ordering::Acquire);
                        crate::agents::turn_tracker::truncate_turns_to(session_id, start_count).await;
                        info!("🧹 断开连接时截断同传 turns 到 {} 条: {}", start_count, session_id);
                    }

                    // 🔧 重置连接断开时需要清理的临时状态（同声传译、媒体锁等）
                    shared_flags.reset_on_disconnect();
                }
            }
        }

        // 🚀 第二步：直接注销WebSocket连接（这会同时关闭所有相关的上行消息接收器）
        let cleaned_count = self.router.unregister_connection(connection_id).await;

        // 🚀 第三步：保留Pipeline资源，不进行清理
        info!(
            "💾 Pipeline资源已保留，等待客户端重连: connection_id={}, sessions={:?}",
            connection_id, session_ids
        );

        info!(
            "✅ WebSocket断开处理完成: {}, {} 个会话Pipeline已保留等待重连",
            connection_id, cleaned_count
        );
        cleaned_count
    }

    /// 🔧 新增：更新会话配置 - 只更新指定的配置项，避免意外丢弃
    async fn update_session_configuration(&self, session_id: &str, payload: &protocol::MessagePayload) -> Result<(), String> {
        if let Some(pipeline) = self.active_pipelines.get(session_id) {
            // 先处理会话级别（与具体管线无关）的元数据：timezone/location
            if let protocol::MessagePayload::SessionConfig { timezone, location, .. } = payload
                && (timezone.is_some() || location.is_some())
            {
                if let Some(tz) = timezone {
                    crate::agents::turn_tracker::set_user_timezone(session_id, tz.clone()).await;
                    tracing::info!(
                        "🔄 热更新用户时区（session级别）: session_id={}, timezone={:?}",
                        session_id,
                        timezone
                    );
                }

                if let Some(loc) = location {
                    let loc_clone = loc.clone();
                    let session_id_clone = session_id.to_string();
                    tracing::info!(
                        "🌍 启动异步位置解析任务（热更新）: session_id={}, 原始location={}",
                        session_id,
                        loc
                    );

                    tokio::spawn(async move {
                        match crate::ip_geolocation::clean_location(&loc_clone).await {
                            Some(cleaned_city) => {
                                crate::agents::turn_tracker::set_user_city(&session_id_clone, cleaned_city.clone()).await;
                                tracing::debug!(
                                    "📍 已更新SessionContext中的location（热更新）: session_id={}, new_location={}",
                                    session_id_clone,
                                    cleaned_city
                                );
                            },
                            None => {
                                tracing::warn!(
                                    "⚠️ 位置解析失败，保持原始值（热更新）: session_id={}, location={}",
                                    session_id_clone,
                                    loc_clone
                                );
                            },
                        }
                    });

                    // 暂存原始 location（等待解析）
                    crate::agents::turn_tracker::set_user_city(session_id, loc.clone()).await;
                    tracing::debug!("📍 暂存原始location（等待解析）: session_id={}, location={}", session_id, loc);
                }
            }

            // 统一入口：将其余配置交给具体管线实现处理
            pipeline.apply_session_config(payload).await.map_err(|e| e.to_string())
        } else {
            Err("会话不存在".to_string())
        }
    }
}

/// 全局SessionManager管理器
static GLOBAL_SESSION_MANAGER: OnceLock<Option<Arc<SessionManager>>> = OnceLock::new();

pub struct GlobalSessionManager;

impl GlobalSessionManager {
    /// 初始化全局SessionManager
    pub fn initialize(session_manager: Arc<SessionManager>) -> Result<(), Option<Arc<SessionManager>>> {
        GLOBAL_SESSION_MANAGER.set(Some(session_manager))
    }

    /// 获取全局SessionManager
    pub fn get() -> Option<Arc<SessionManager>> {
        GLOBAL_SESSION_MANAGER.get()?.as_ref().cloned()
    }

    /// 检查是否已初始化
    pub fn is_initialized() -> bool {
        GLOBAL_SESSION_MANAGER.get().is_some()
    }

    /// 检查是否可用
    pub fn is_available() -> bool {
        GLOBAL_SESSION_MANAGER.get().is_some_and(|opt| opt.is_some())
    }
}
