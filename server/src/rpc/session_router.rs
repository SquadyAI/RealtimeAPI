use super::message_adapter::WsMessage;
use super::realtime_event;
use dashmap::DashMap;
use serde_json;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{error, info, trace, warn};

/// 虚拟会话数据结构。只保存路由必须维护的元数据。
#[derive(Debug)]
pub struct VirtualSession {
    pub session_id: String,                       // 长度 16 的 nanoid
    pub connection_id: String,                    // 所属 websocket 的 nanoid
    pub sender: mpsc::UnboundedSender<WsMessage>, // 向业务侧发送的通道
    pub last_activity: Instant,                   // 最近一次收 / 发 消息时间
    pub protocol_id: super::protocol::ProtocolId, // Pipeline协议类型（用于检测模式切换）
}

/// 纯路由层：负责连接与虚拟会话管理，以及会话超时监控。
pub struct SessionRouter {
    /// connection_id -> 发送到 websocket 的通道
    connection_map: DashMap<String, mpsc::UnboundedSender<WsMessage>>,
    /// session_id -> VirtualSession
    session_map: DashMap<String, VirtualSession>,
    /// connection_id -> 该连接下属的所有 session_id
    connection_sessions: DashMap<String, HashSet<String>>,
    /// 会话空闲超时时长
    timeout: Duration,
}

impl std::fmt::Debug for SessionRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRouter")
            .field("connection_map", &"[CONNECTION_MAP]")
            .field("session_map", &"[SESSION_MAP]")
            .field("connection_sessions", &"[CONNECTION_SESSIONS]")
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl SessionRouter {
    /// 创建新的 SessionRouter，并启动超时检测任务。
    pub fn new(timeout: Duration) -> Self {
        SessionRouter {
            connection_map: DashMap::new(),
            session_map: DashMap::new(),
            connection_sessions: DashMap::new(),
            timeout,
        }
    }

    /// 注册新的 websocket 连接。
    pub async fn register_connection(&self, connection_id: String, sender: mpsc::UnboundedSender<WsMessage>) {
        self.connection_map.insert(connection_id, sender);
    }

    /// 注销 websocket 连接，并清理其所有虚拟会话。返回被清理的虚拟会话数量。
    pub async fn unregister_connection(&self, connection_id: &str) -> usize {
        // 移除连接级 sender
        self.connection_map.remove(connection_id);

        // 取出所有 session_id
        let session_ids = self
            .connection_sessions
            .remove(connection_id)
            .map(|(_, set)| set)
            .unwrap_or_default();

        // 🔧 关键修复：安全清理 session_map，避免竞态条件
        // 在快速重连场景下，session 可能已经被重新绑定到新连接
        // 只有当 session 的 connection_id 仍然是当前正在注销的连接时才移除
        // ⚠️ 重要：这里必须防止竞态 —— session 可能在此刻已被 startSession 重绑到新连接。
        // 若直接 remove(session_id) 会误删新连接的会话绑定，造成“上行仍进Pipeline，但下行事件发不回去/会话缺失”等问题。
        let mut actually_removed = 0usize;
        for sid in &session_ids {
            // 使用 remove_if 模式：先检查 connection_id 是否匹配，再决定是否移除
            let should_remove = self
                .session_map
                .get(sid)
                .map(|vs| vs.connection_id == connection_id)
                .unwrap_or(false);

            if should_remove {
                self.session_map.remove(sid);
                actually_removed += 1;
                trace!("🧹 移除会话: session_id={}, connection_id={}", sid, connection_id);
            } else {
                info!(
                    "🔄 跳过移除已重新绑定的会话: session_id={}, old_connection={}",
                    sid, connection_id
                );
            }
        }

        // 🔧 更新活跃会话数监控指标
        let active_count = self.session_map.len() as i64;
        crate::monitoring::update_active_sessions(active_count);

        actually_removed
    }

    /// 创建 / 复用 虚拟会话。
    /// 返回值说明：
    /// - `Ok(Some(receiver))` 表示新建会话或需要重新启动上行消息处理任务（包括protocol_id变化时强制重建）
    /// - `Ok(None)` 表示会话已存在且为同一连接的重复请求，应当忽略
    /// - `Err(msg)` 表示操作失败
    pub async fn create_virtual_session(&self, session_id: &str, connection_id: &str, protocol_id: super::protocol::ProtocolId) -> Result<Option<mpsc::UnboundedReceiver<WsMessage>>, String> {
        // 检查连接是否存在
        if !self.connection_map.contains_key(connection_id) {
            return Err("连接未注册".into());
        }

        // 如果会话已存在 -> 检查是否需要重新绑定
        if let Some(mut existing) = self.session_map.get_mut(session_id) {
            let old_conn = existing.connection_id.clone();

            // 🔧 关键修复：如果是同一个连接ID
            if old_conn == connection_id {
                let last_activity_seconds = existing.last_activity.elapsed().as_secs();
                existing.last_activity = Instant::now();

                // 🚀 新增：检测protocol_id变化（模式切换，如Tts→All），强制重建Pipeline
                let old_protocol_id = existing.protocol_id;
                if old_protocol_id != protocol_id {
                    info!(
                        "🔄 检测到protocol_id变化，强制重建Pipeline: session_id={}, old={:?}, new={:?}",
                        session_id, old_protocol_id, protocol_id
                    );
                    // 更新protocol_id
                    existing.protocol_id = protocol_id;
                    // 创建新通道
                    let (tx, rx) = mpsc::unbounded_channel();
                    existing.sender = tx;
                    info!("✅ Protocol切换完成，已创建新上行通道: session_id={}", session_id);
                    return Ok(Some(rx));
                }

                // 🆕 优雅处理：检测现有发送端是否仍可用；若不可用，重建通道并返回 Some(rx)
                let need_recreate_channel = {
                    let test_send = existing.sender.send(WsMessage::Ping(bytes::Bytes::new()));
                    test_send.is_err()
                };

                if need_recreate_channel {
                    let (tx, rx) = mpsc::unbounded_channel();
                    existing.sender = tx;
                    info!(
                        "✅ 同连接重复Start：检测到旧通道不可用，已重建上行通道: session_id={}",
                        session_id
                    );
                    return Ok(Some(rx));
                }

                // 原行为：通道可用，仅刷新活跃时间并忽略
                if last_activity_seconds < 5 {
                    tracing::debug!(
                        "🔄 忽略重复的startSession请求 ({}秒前刚活跃): session_id={}, connection_id={}",
                        last_activity_seconds,
                        session_id,
                        connection_id
                    );
                } else {
                    info!(
                        "🔄 会话已绑定到同一连接，忽略重复的startSession请求 (上次活跃{}秒前): session_id={}, connection_id={}",
                        last_activity_seconds, session_id, connection_id
                    );
                }

                return Ok(None);
            }

            // 不同连接ID，执行真正的重新绑定
            info!(
                "🔄 会话重新绑定: session_id={}, old_connection={}, new_connection={}",
                session_id, old_conn, connection_id
            );

            existing.connection_id = connection_id.to_string();
            existing.last_activity = Instant::now();
            existing.protocol_id = protocol_id; // 🆕 更新protocol_id

            // 🔧 修复：更新 connection_sessions 映射时避免死锁
            // 从旧连接中移除会话
            if let Some(mut set) = self.connection_sessions.get_mut(&old_conn) {
                set.remove(session_id);
                // 如果集合为空，移除整个条目
                if set.is_empty() {
                    drop(set); // 释放锁
                    self.connection_sessions.remove(&old_conn);
                }
            } else {
                // 如果get_mut失败，尝试直接移除
                self.connection_sessions.remove(&old_conn);
            }

            // 添加到新连接
            self.connection_sessions
                .entry(connection_id.to_string())
                .or_default()
                .insert(session_id.to_string());

            // 🔧 修复：在会话重用场景下，创建新的通道以重新启动上行消息处理任务
            let (tx, rx) = mpsc::unbounded_channel();

            // 更新现有会话的发送器
            existing.sender = tx;

            info!("✅ 会话重新绑定完成，创建新的上行消息通道: session_id={}", session_id);

            // 返回新的接收器，以便重新启动上行消息处理任务
            return Ok(Some(rx));
        }

        // 创建全新的会话
        let (tx, rx) = mpsc::unbounded_channel();

        // 插入 session_map
        let vs = VirtualSession {
            session_id: session_id.to_string(),
            connection_id: connection_id.to_string(),
            sender: tx,
            last_activity: Instant::now(),
            protocol_id, // 🆕 保存protocol_id
        };
        self.session_map.insert(session_id.to_string(), vs);

        // 更新 connection_sessions
        self.connection_sessions
            .entry(connection_id.to_string())
            .or_default()
            .insert(session_id.to_string());

        info!(
            "✅ 创建全新虚拟会话: session_id={}, connection_id={}",
            session_id, connection_id
        );

        // 🔧 更新活跃会话数监控指标
        let active_count = self.session_map.len() as i64;
        crate::monitoring::update_active_sessions(active_count);

        Ok(Some(rx))
    }

    /// 记录会话相关的活动（上/下行）。
    pub async fn touch_session(&self, session_id: &str) {
        if let Some(mut session) = self.session_map.get_mut(session_id) {
            session.last_activity = Instant::now();
        }
    }

    /// 获取会话的protocol_id（用于rebind场景）
    pub fn get_session_protocol_id(&self, session_id: &str) -> Option<super::protocol::ProtocolId> {
        self.session_map.get(session_id).map(|s| s.protocol_id)
    }

    /// 向客户端（通过 websocket）下发消息。
    pub async fn send_to_client(&self, session_id: &str, payload: WsMessage) -> Result<(), String> {
        // 🔧 调试：打印下行通道信令（过滤音频相关消息以提升性能）
        match &payload {
            WsMessage::Text(_text) => {
                // 如果INFO级别未启用，直接跳过解析与日志
                // if tracing::enabled!(tracing::Level::INFO) {
                //     // 先用快速子串检查绕过JSON解析（常见音频/高频路径）
                //     let t = text.as_str();
                //     if !(t.contains("response.audio") || t.contains("audio")) {
                //         // 解析下行事件名称（只在需要日志时执行）
                //         let event_name = parse_downstream_event_name(t);
                //         if !is_audio_related_event(&event_name) {
                //             info!(
                //                 "📤 [下行信令] session_id={}, event={}, payload_size={}bytes",
                //                 session_id,
                //                 event_name,
                //                 text.len()
                //             );
                //         }
                //     }
                // }
            },
            WsMessage::Binary(_data) => {
                // 二进制数据通常是音频，不记录日志以提升性能
                // 只在TRACE级别记录简化信息
                // if tracing::enabled!(tracing::Level::TRACE) {
                //     trace!("📤 [下行信令] session_id={}, payload_type=binary", session_id);
                // }
            },
            WsMessage::Close(_) => {
                info!("📤 [下行信令] session_id={}, payload_type=close", session_id);
            },
            WsMessage::Ping(_) => {
                // Ping/Pong 也比较频繁，降级到DEBUG级别
                // if tracing::enabled!(tracing::Level::DEBUG) {
                //     trace!("📤 [下行信令] session_id={}, payload_type=ping", session_id);
                // }
            },
            WsMessage::Pong(_) => {
                // if tracing::enabled!(tracing::Level::DEBUG) {
                //     trace!("📤 [下行信令] session_id={}, payload_type=pong", session_id);
                // }
            },
        }

        // 1. 查找会话所属 connection_id
        let connection_id = {
            if let Some(vs) = self.session_map.get(session_id) {
                vs.connection_id.clone()
            } else {
                return Err("会话不存在".into());
            }
        };

        // 2. 克隆 Sender，立即释放读锁，避免后续 await 再次拿写锁造成死锁
        let sender_opt = { self.connection_map.get(&connection_id).map(|sender| sender.clone()) };

        if let Some(sender) = sender_opt {
            if sender.send(payload).is_err() {
                warn!(
                    "📤 向 websocket 发送失败: session_id={}, connection_id={}",
                    session_id, connection_id
                );
                return Err("向 websocket 发送失败".to_string());
            }

            // 3. 更新 last_activity（需要写锁，但此时已释放读锁）
            self.touch_session(session_id).await;
            Ok(())
        } else {
            warn!(
                "📤 连接 sender 不存在: session_id={}, connection_id={}",
                session_id, connection_id
            );
            Err("连接 sender 不存在".into())
        }
    }

    /// 将来自指定连接的上行数据路由给业务侧（带连接绑定校验）。
    ///
    /// 设计目的：
    /// - WS 重连会导致同一个 session_id 在短时间内出现“旧连接残留包 + 新连接已重绑”的并发窗口
    /// - 若不校验 connection_id，旧连接仍能持续向该 session 注入 AudioChunk/StopInput 等，干扰 VAD/ASR/打断状态机
    ///
    /// 行为：
    /// - 若 session 不存在：返回 Err("会话不存在")
    /// - 若 session 已绑定到其他连接：忽略该上行包并返回 Ok(())
    pub async fn forward_upstream_from_connection(&self, session_id: &str, connection_id: &str, payload: WsMessage) -> Result<(), String> {
        // 克隆业务侧 Sender，避免与 touch_session 写冲突
        let (bound_conn, tx) = self
            .session_map
            .get(session_id)
            .map(|vs| (vs.connection_id.clone(), vs.sender.clone()))
            .ok_or_else(|| "会话不存在".to_string())?;

        // 已重绑到其他连接：忽略旧连接残留包（不算错误，避免触发"现场恢复"逻辑造成反复震荡）
        if bound_conn != connection_id {
            trace!(
                "⏭️ 忽略非当前绑定连接的上行包: session_id={}, from_connection={}, bound_connection={}",
                session_id, connection_id, bound_conn
            );
            return Ok(());
        }

        match tx.send(payload) {
            Ok(_) => {
                if tracing::enabled!(tracing::Level::TRACE) {
                    trace!("📤 forward_upstream(from_connection) 成功: session_id={}", session_id);
                }
                self.touch_session(session_id).await;
                Ok(())
            },
            Err(e) => {
                error!(
                    "向业务通道发送失败: session_id={}, connection_id={}, error={}",
                    session_id, connection_id, e
                );
                Err("向业务通道发送失败".into())
            },
        }
    }

    /// 内部函数：检查并处理超时会话。
    pub(crate) async fn check_timeouts(&self) {
        let now = Instant::now();
        // 收集需要超时的会话，避免持锁时间过长
        let expired: Vec<String> = self
            .session_map
            .iter()
            .filter_map(|entry| {
                let vs = entry.value();
                if now.duration_since(vs.last_activity) >= self.timeout {
                    Some(vs.session_id.clone())
                } else {
                    None
                }
            })
            .collect();

        for sid in expired {
            // 发送超时通知
            let _ = self.send_to_client(&sid, build_timeout_msg(&sid)).await;
            // 从表中删除
            self.remove_session(&sid).await;
        }
    }

    /// 移除 session，不发送任何消息。
    pub async fn remove_session(&self, session_id: &str) {
        let connection_id_opt = self.session_map.remove(session_id).map(|(_, vs)| vs.connection_id);

        // 🔧 更新活跃会话数监控指标
        let active_count = self.session_map.len() as i64;
        crate::monitoring::update_active_sessions(active_count);

        if let Some(conn_id) = connection_id_opt {
            // 🔧 修复：避免get_mut可能的阻塞
            if let Some(mut set) = self.connection_sessions.get_mut(&conn_id) {
                set.remove(session_id);
                if set.is_empty() {
                    drop(set); // 释放锁
                    self.connection_sessions.remove(&conn_id);
                }
            } else {
                // 如果get_mut失败，直接尝试移除整个条目
                self.connection_sessions.remove(&conn_id);
            }
        }
    }

    /// 获取当前活跃会话数量
    pub async fn active_session_count(&self) -> usize {
        self.session_map.len()
    }

    /// 获取指定连接下的会话数量
    pub async fn sessions_under_connection(&self, connection_id: &str) -> usize {
        self.connection_sessions.get(connection_id).map(|s| s.len()).unwrap_or(0)
    }

    /// 返回指定连接下所有 session_id 的克隆列表
    pub fn session_ids_for_connection(&self, connection_id: &str) -> Vec<String> {
        self.connection_sessions
            .get(connection_id)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// 获取配置的空闲超时时长
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// 获取指定会话的connection_id
    pub fn get_connection_id_for_session(&self, session_id: &str) -> Option<String> {
        self.session_map.get(session_id).map(|vs| vs.connection_id.clone())
    }

    /// 检查会话是否存在
    pub fn contains_session(&self, session_id: &str) -> bool {
        self.session_map.contains_key(session_id)
    }

    /// 获取指定连接的WebSocket发送器
    pub fn get_connection_sender(&self, connection_id: &str) -> Option<mpsc::UnboundedSender<WsMessage>> {
        self.connection_map.get(connection_id).map(|sender| sender.clone())
    }
}

/// 判断是否为音频相关事件（用于过滤高频日志）
#[allow(dead_code)]
fn is_audio_related_event(event_name: &str) -> bool {
    // 音频相关的事件名称模式
    let audio_patterns = [
        "response.audio",
        "conversation.item.audio",
        "audio_start",
        "audio_end",
        "audio_chunk",
        // "response.audio_transcript.delta",
        "response.audio.delta",
        // "response.audio_transcript.done",
        // "response.audio.done",
    ];

    let event_lower = event_name.to_lowercase();

    // 检查是否包含音频相关关键词
    audio_patterns.iter().any(|pattern| event_lower.contains(pattern))
}

/// 解析下行事件名称
#[allow(dead_code)]
fn parse_downstream_event_name(text: &str) -> String {
    // 尝试解析JSON并提取事件名称
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
        // 尝试多种可能的事件名称字段
        if let Some(event_name) = json
            .get("event")
            .or_else(|| json.get("type"))
            .or_else(|| json.get("eventType"))
            .or_else(|| json.get("event_type"))
            .or_else(|| json.get("messageType"))
            .or_else(|| json.get("message_type"))
            .and_then(|v| v.as_str())
        {
            return event_name.to_string();
        }

        // 如果有嵌套的事件结构，尝试解析
        if let Some(data) = json.get("data")
            && let Some(event_name) = data.get("event").or_else(|| data.get("type")).and_then(|v| v.as_str())
        {
            return format!("data.{}", event_name);
        }

        // 如果有 payload 字段，尝试解析
        if let Some(payload) = json.get("payload")
            && let Some(event_name) = payload.get("event").or_else(|| payload.get("type")).and_then(|v| v.as_str())
        {
            return format!("payload.{}", event_name);
        }

        // 检查是否为 OpenAI realtime API 格式
        if let Some(event_type) = json.get("type").and_then(|v| v.as_str()) {
            return event_type.to_string();
        }

        // 如果找不到标准字段，但有其他可识别字段，尝试推断
        if json.get("error").is_some() {
            return "error".to_string();
        }
        if json.get("session").is_some() {
            return "session_update".to_string();
        }
        if json.get("conversation").is_some() {
            return "conversation_update".to_string();
        }
        if json.get("response").is_some() {
            return "response".to_string();
        }
        if json.get("audio").is_some() {
            return "audio".to_string();
        }
        if json.get("transcript").is_some() {
            return "transcript".to_string();
        }

        // 如果是对象但找不到明确的事件类型，返回所有顶级字段
        if json.is_object() {
            let keys: Vec<String> = json
                .as_object()
                .unwrap()
                .keys()
                .take(3) // 只取前3个字段避免过长
                .map(|k| k.to_string())
                .collect();
            if !keys.is_empty() {
                return format!("object[{}]", keys.join(","));
            }
        }

        return "json_unknown".to_string();
    }

    // 如果不是JSON，尝试检查是否为其他格式
    if text.trim().is_empty() {
        return "empty".to_string();
    }

    if text.starts_with("data:") {
        return "sse_data".to_string();
    }

    if text.starts_with("event:") {
        return "sse_event".to_string();
    }

    // 检查是否为简单的错误消息
    if text.contains("error") || text.contains("Error") {
        return "text_error".to_string();
    }

    // 返回文本的前几个字符作为标识

    if text.chars().count() > 20 {
        let truncated: String = text.chars().take(20).collect();
        format!("text[{}...]", truncated)
    } else {
        format!("text[{}]", text)
    }
}

/// 构造一个标准的超时错误消息。
fn build_timeout_msg(session_id: &str) -> WsMessage {
    const TIMEOUT_ERROR_CODE: u16 = 4008; // 类似 HTTP 408 Request Timeout
    const TIMEOUT_MESSAGE: &str = "Session timed out due to inactivity.";

    let ws_msg = realtime_event::create_error_event_message(session_id.to_string(), TIMEOUT_ERROR_CODE, TIMEOUT_MESSAGE);

    // 序列化为JSON字符串
    match serde_json::to_string(&ws_msg) {
        Ok(json_str) => WsMessage::from(json_str),
        Err(e) => {
            // 序列化失败是一个严重错误，这里记录日志并发送一个备用的纯文本错误信息
            error!("Failed to serialize timeout message for session {}: {}", session_id, e);
            WsMessage::from(format!(r#"{{"error": "Failed to serialize timeout message: {}"}}"#, e))
        },
    }
}
