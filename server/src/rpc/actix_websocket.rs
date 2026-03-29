use actix_web::{HttpRequest, HttpResponse, Result as ActixResult, web};
use actix_ws::{AggregatedMessage, AggregatedMessageStream, Message as ActixMessage, Session};
use anyhow::Result;
use bytes::Bytes;
use futures_util::StreamExt;
use nanoid;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use super::{WebSocketSessionManager, message_adapter::WsMessage, protocol};

// Import IP geolocation service and connection metadata cache
use crate::ip_geolocation::types::IpAddress;
use crate::rpc::connection_metadata::{CONNECTION_METADATA_CACHE, ConnectionMetadata, ConnectionMetadataCleanupGuard};

/// Actix-Web应用状态
#[derive(Clone)]
pub struct ActixAppState {
    pub session_manager: Arc<WebSocketSessionManager>,
}

impl ActixAppState {
    pub fn new(session_manager: Arc<WebSocketSessionManager>) -> Self {
        Self { session_manager }
    }
}

/// 连接状态枚举
#[derive(Debug, Clone)]
enum ConnectionStatus {
    Healthy,      // 连接健康
    Disconnected, // 连接已断开
}

/// 获取客户端真实IP地址
fn get_client_ip(req: &HttpRequest) -> Option<std::net::SocketAddr> {
    // 首先尝试从 X-Forwarded-For 头部获取IP（通常用于反向代理环境）
    if let Some(x_forwarded_for) = req.headers().get("X-Forwarded-For")
        && let Ok(ip_str) = x_forwarded_for.to_str()
    {
        // X-Forwarded-For 格式通常是 "client, proxy1, proxy2"
        // 我们只需要第一个（最左边的）IP地址
        if let Some(client_ip) = ip_str.split(',').next() {
            let client_ip = client_ip.trim();
            // 构造一个假的 SocketAddr，因为我们只关心IP部分
            if let Ok(ip_addr) = client_ip.parse::<std::net::IpAddr>() {
                return Some(std::net::SocketAddr::new(ip_addr, 0));
            }
        }
    }

    // 其次尝试 X-Real-IP 头部
    if let Some(x_real_ip) = req.headers().get("X-Real-IP")
        && let Ok(ip_str) = x_real_ip.to_str()
        && let Ok(ip_addr) = ip_str.parse::<std::net::IpAddr>()
    {
        return Some(std::net::SocketAddr::new(ip_addr, 0));
    }

    // 最后回退到 peer_addr（适用于非代理环境）
    req.peer_addr()
}
/// actix-ws WebSocket连接处理函数
pub async fn actix_websocket_handler(req: HttpRequest, stream: web::Payload, data: web::Data<ActixAppState>) -> ActixResult<HttpResponse> {
    let (response, session, msg_stream) = actix_ws::handle(&req, stream)?;

    let connection_id = nanoid::nanoid!(16);

    // 获取客户端IP地址
    let client_ip = get_client_ip(&req);
    info!(
        "🔌 新的actix-ws WebSocket连接建立: connection_id={}, client_ip={:?}",
        connection_id, client_ip
    );

    // 🆕 添加详细的调试信息
    if let Some(ip) = &client_ip {
        let ip_str = ip.ip().to_string();
        if ip_str.starts_with("127.") || ip_str.starts_with("::1") {
            warn!("⚠️ 检测到本地IP地址 {}，无法获取真实地理位置", ip_str);
        } else if ip_str.starts_with("192.168.") || ip_str.starts_with("10.") || ip_str.starts_with("172.") {
            warn!("⚠️ 检测到内网IP地址 {}，无法获取真实地理位置", ip_str);
        } else {
            info!("✅ 检测到公网IP地址 {}，将查询地理位置", ip_str);
        }
    } else {
        warn!("⚠️ 无法获取客户端IP地址");
    }

    // Store basic connection metadata
    let ip_address = client_ip.map(|sa| IpAddress::from(sa.ip()));
    let metadata = ConnectionMetadata::new(connection_id.clone(), ip_address.clone());
    CONNECTION_METADATA_CACHE.insert(connection_id.clone(), metadata);

    // Create a cleanup guard instance
    let cleanup_guard = ConnectionMetadataCleanupGuard { connection_id: connection_id.clone() };

    // 启动连接处理任务
    let state = data.get_ref().clone();
    actix_web::rt::spawn(async move {
        // Spawn a separate task for IP geolocation lookup
        if let Some(_ip) = ip_address {
            // if let Some(geo_service) = get_ip_geolocation_service() {
            //     match geo_service.lookup(&ip) {
            //         Ok(geolocation) => {
            //             debug!(
            //                 "🌍 成功获取IP地理位置信息: ip={}, city={:?}",
            //                 ip,
            //                 geolocation.city.as_ref().unwrap_or(&"Unknown".to_string())
            //             );

            //             // Update connection metadata with geolocation data
            //             if let Some(mut entry) = CONNECTION_METADATA_CACHE.get_mut(&connection_id) {
            //                 entry.set_geolocation(geolocation);
            //             }
            //         },
            //         Err(e) => {
            //             warn!("⚠️ 无法解析IP地址 {} 的地理位置信息: {}", ip, e);
            //         },
            //     }
            // } else {
            //     debug!("🌐 IP地理位置服务未启用，跳过查询");
            // }
        }
        let max_size = 2_usize.pow(24);
        if let Err(e) = handle_actix_websocket_connection(
            session,
            msg_stream
                .max_frame_size(max_size)
                .aggregate_continuations()
                .max_continuation_size(max_size),
            state,
            connection_id,
        )
        .await
        {
            error!("❌ actix-ws WebSocket连接处理失败: {}", e);
        }

        // The cleanup_guard will be dropped here, triggering the cleanup
        drop(cleanup_guard);
    });

    Ok(response)
}

/// 处理actix-ws WebSocket连接的主要逻辑（简化版）
async fn handle_actix_websocket_connection(session: Session, mut msg_stream: AggregatedMessageStream, state: ActixAppState, connection_id: String) -> Result<()> {
    info!("🔧 开始WebSocket连接处理: connection_id={}", connection_id);

    // 使用panic保护
    let result = std::panic::AssertUnwindSafe(async {
        // 创建通道
        let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel::<WsMessage>();
        let (ws_tx, mut ws_rx) = mpsc::unbounded_channel::<WsMessage>();

        // 注册连接
        match tokio::time::timeout(
            Duration::from_secs(5),
            state.session_manager.register_connection(connection_id.clone(), ws_tx.clone()),
        )
        .await
        {
            Ok(_) => info!("✅ 连接注册成功: connection_id={}", connection_id),
            Err(_) => {
                error!("⏰ 连接注册超时: connection_id={}", connection_id);
                return Err(anyhow::anyhow!("连接注册超时"));
            },
        }

        // 事务性连接状态检测
        let (connection_status_tx, mut connection_status_rx) = mpsc::unbounded_channel::<ConnectionStatus>();

        // 心跳检测任务
        let heartbeat_task = {
            let connection_id = connection_id.clone();
            let ctrl_tx = ctrl_tx.clone();
            let status_tx = connection_status_tx.clone();
            tokio::spawn(async move {
                info!("💓 心跳任务启动: connection_id={}", connection_id);
                let mut interval = tokio::time::interval(Duration::from_secs(15));
                interval.tick().await;

                loop {
                    interval.tick().await;
                    let ping_data = Bytes::from(format!("heartbeat_{}", chrono::Utc::now().timestamp()));
                    // 🔧 修复：检查通道是否已关闭，而不是直接发送
                    if ctrl_tx.is_closed() {
                        warn!("💔 心跳通道已关闭，连接可能已断开: {}", connection_id);
                        status_tx.send(ConnectionStatus::Disconnected).ok();
                        break;
                    }

                    if ctrl_tx.send(WsMessage::Ping(ping_data)).is_err() {
                        warn!("💔 心跳发送失败，连接可能已断开: {}", connection_id);
                        status_tx.send(ConnectionStatus::Disconnected).ok();
                        break;
                    }

                    status_tx.send(ConnectionStatus::Healthy).ok();
                    // info!("💓 心跳发送成功: connection_id={}", connection_id);
                }
                info!("💓 心跳任务结束: connection_id={}", connection_id);
            })
        };

        // 发送任务
        let send_task = {
            let connection_id = connection_id.clone();
            let mut session_clone = session.clone();
            let status_tx = connection_status_tx.clone();
            tokio::spawn(async move {
                info!("📤 发送任务启动: connection_id={}", connection_id);

                loop {
                    tokio::select! {
                        Some(message) = ctrl_rx.recv() => {
                            let actix_msg: ActixMessage = message.into();
                            match actix_msg {
                                ActixMessage::Ping(data) => {
                                    if let Err(e) = session_clone.ping(&data).await {
                                        error!("❌ 发送Ping失败，判定连接断开: connection_id={}, error={}", connection_id, e);
                                        status_tx.send(ConnectionStatus::Disconnected).ok();
                                        break;
                                    }
                                },
                                ActixMessage::Pong(data) => {
                                    if let Err(e) = session_clone.pong(&data).await {
                                        error!("❌ 发送Pong失败，判定连接断开: connection_id={}, error={}", connection_id, e);
                                        status_tx.send(ConnectionStatus::Disconnected).ok();
                                        break;
                                    }
                                },
                                ActixMessage::Close(reason) => {
                                    if session_clone.close(reason).await.is_err() {
                                        error!("❌ 发送Close失败: connection_id={}", connection_id);
                                    }
                                    status_tx.send(ConnectionStatus::Disconnected).ok();
                                    break;
                                },
                                _ => {}
                            }
                        },
                        Some(message) = ws_rx.recv() => {
                            let actix_msg: ActixMessage = message.into();
                            match actix_msg {
                                ActixMessage::Text(text) => {
                                    if let Err(e) = session_clone.text(text).await {
                                        error!("❌ 发送文本失败，判定连接断开: connection_id={}, error={}", connection_id, e);
                                        status_tx.send(ConnectionStatus::Disconnected).ok();
                                        break;
                                    }
                                },
                                ActixMessage::Binary(data) => {
                                    if let Err(e) = session_clone.binary(data).await {
                                        error!("❌ 发送二进制失败，判定连接断开: connection_id={}, error={}", connection_id, e);
                                        status_tx.send(ConnectionStatus::Disconnected).ok();
                                        break;
                                    }
                                },
                                _ => {}
                            }
                        },
                        else => break,
                    }
                }
                info!("📤 发送任务结束: connection_id={}", connection_id);
            })
        };

        // 消息处理循环
        info!("📥 开始消息处理循环: connection_id={}", connection_id);
        let mut last_activity = std::time::Instant::now();
        let connection_timeout = Duration::from_secs(300);

        loop {
            tokio::select! {
                // 优先处理连接状态变化
                status = connection_status_rx.recv() => {
                    match status {
                        Some(ConnectionStatus::Disconnected) => {
                            warn!("🔌 事务性检测到连接断开: {}", connection_id);
                            break;
                        },
                        Some(ConnectionStatus::Healthy) => continue,
                        None => break,
                    }
                },
                // 其次处理WebSocket消息
                msg = msg_stream.next() => {
                    match msg {
                        Some(Ok(AggregatedMessage::Text(text))) => {
                            last_activity = std::time::Instant::now();
                            // 🪵 添加日志以捕获原始的JSON字符串，用于调试控制字符问题
                            tracing::debug!("📥 接收到原始WebSocket文本消息: {:?}", text);
                            match protocol::WebSocketMessage::from_json_safe(&text) {
                                Ok(ws_message) => {
                                    // 🔍 调试：记录解析后的payload中asr_language和system_prompt（若为SessionConfig）
                                    if let Some(protocol::MessagePayload::SessionConfig { asr_language, system_prompt, .. }) = &ws_message.payload {
                                        tracing::info!(
                                            "🔍 actix_ws: 解析到SessionConfig - asr_language={:?}, system_prompt长度={}, session_id={}",
                                            asr_language,
                                            system_prompt.as_ref().map(|s| s.len()).unwrap_or(0),
                                            ws_message.session_id
                                        );
                                    } else {
                                        tracing::info!(
                                            "🔍 actix_ws: 解析到的payload不是SessionConfig或为None, session_id={}",
                                            ws_message.session_id
                                        );
                                    }
                                    // 委托给session_manager处理事件
                                    if let Err(e) = state.session_manager.handle_websocket_message(ws_message, &connection_id, &ws_tx).await {
                                        error!("❌ 处理文本消息失败: {}", e);
                                        continue;
                                    }
                                },
                                Err(e) => {
                                    error!("❌ 解析WebSocket消息失败: {}", e);
                                    // 🪵 记录导致解析失败的原始JSON字符串
                                    tracing::error!("💥 导致解析失败的原始JSON字符串: {}", text.replace("\n", "\\n").replace("\r", "\\r").replace("\t", "\\t"));
                                },
                            }
                        },
                        Some(Ok(AggregatedMessage::Binary(data))) => {
                            last_activity = std::time::Instant::now();
                            // info!("📦 [TRACE-WS] 收到WebSocket二进制数据: size={} bytes, connection_id={}", data.len(), connection_id);

                            if data.len() < crate::rpc::protocol::BINARY_HEADER_SIZE {
                                warn!("收到的二进制包长度过小: {}", data.len());
                                continue;
                            }

                            let header = match crate::rpc::protocol::BinaryHeader::from_bytes(&data[..crate::rpc::protocol::BINARY_HEADER_SIZE]) {
                                Ok(h) => {
                                    // info!("📋 [TRACE-WS] 二进制消息头解析成功: protocol_id={:?}, command_id={}, session_id={}",
                                    //     h.protocol_id, h.command_id.as_u8(), h.session_id);
                                    h
                                },
                                Err(e) => {
                                    error!("❌ 二进制消息头解析失败: {}", e);
                                    // 追加调试：打印头部字节预览（最多24字节），便于定位协议不匹配
                                    let head_len = data.len().min(crate::rpc::protocol::BINARY_HEADER_SIZE);
                                    let head_bytes = &data[..head_len];
                                    let preview_len = head_bytes.len().min(24);
                                    let hex_preview = head_bytes[..preview_len]
                                        .iter()
                                        .map(|b| format!("{:02X}", b))
                                        .collect::<Vec<_>>()
                                        .join(" ");
                                    tracing::error!(
                                        "💥 二进制头预览[{} bytes]: {}",
                                        preview_len,
                                        hex_preview
                                    );
                                    continue;
                                },
                            };

                            if let Err(e) = state.session_manager.handle_binary_message(header, &data, &connection_id).await {
                                error!("❌ 处理二进制消息失败: {}", e);
                                continue;
                            }

                        },
                        Some(Ok(AggregatedMessage::Close(reason))) => {
                            if let Some(reason) = reason {
                                info!("🔌 连接关闭: connection_id={}, code={:?}, reason='{}'",
                                    connection_id, reason.code, reason.description.unwrap_or_default());
                            } else {
                                info!("🔌 连接关闭: connection_id={}", connection_id);
                            }
                            break;
                        },
                        Some(Ok(AggregatedMessage::Ping(data))) => {
                            last_activity = std::time::Instant::now();
                            if let Err(e) = ctrl_tx.send(WsMessage::Pong(data)) {
                                error!("❌ 发送Pong响应失败: connection_id={}, error={}", connection_id, e);
                                break;
                            }
                        },
                        Some(Ok(AggregatedMessage::Pong(_))) => {
                            last_activity = std::time::Instant::now();
                        },
                        Some(Err(e)) => {
                            error!("❌ WebSocket错误: connection_id={}, error={}", connection_id, e);
                            break;
                        },
                        None => {
                            info!("📡 WebSocket消息流已结束: connection_id={}", connection_id);
                            break;
                        }
                    }
                },
                // 备用超时检测
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    if last_activity.elapsed() > connection_timeout {
                        warn!("⏰ 备用超时检测：连接超时: connection_id={}, 无活动时间: {:?}",
                            connection_id, last_activity.elapsed());
                        break;
                    }
                    continue;
                }
            }
        }

        info!("📥 消息处理循环结束: connection_id={}", connection_id);

        // 取消心跳和发送任务
        heartbeat_task.abort();
        send_task.abort();

        Ok(())
    })
    .await;
    // 清理资源
    info!("🧹 开始清理连接资源: connection_id={}", connection_id);

    let cleanup_result = tokio::time::timeout(
        Duration::from_secs(2),
        state.session_manager.unregister_connection(&connection_id),
    )
    .await;

    let cleaned_count = match cleanup_result {
        Ok(count) => {
            info!("🗑️ 已清理 {} 个会话", count);
            count
        },
        Err(_) => {
            error!("⏰ 会话清理超时，强制执行清理: connection_id={}", connection_id);

            // 获取该连接下的所有会话ID
            let session_ids = state.session_manager.get_session_ids_for_connection(&connection_id).await;
            info!("🔧 强制清理 {} 个会话: {:?}", session_ids.len(), session_ids);

            for session_id in &session_ids {
                // Pipeline清理现在由SessionManager处理
                let _ = state.session_manager.destroy_session_completely(session_id).await;
                info!("🔧 强制移除Pipeline: {}", session_id);
            }

            tokio::spawn({
                let manager = state.session_manager.clone();
                let conn_id = connection_id.clone();
                async move {
                    let _ = manager.unregister_connection(&conn_id).await;
                    info!("🔧 延迟强制清理完成: {}", conn_id);
                }
            });

            session_ids.len()
        },
    };

    info!(
        "🔌 连接已完全断开: connection_id={}, cleaned_sessions={}",
        connection_id, cleaned_count
    );

    match result {
        Ok(_) => Ok(()),
        Err(panic_payload) => {
            error!(
                "💥 连接处理发生panic: connection_id={}, payload={:?}",
                connection_id, panic_payload
            );
            Err(anyhow::anyhow!("连接处理panic: {:?}", panic_payload))
        },
    }
}
