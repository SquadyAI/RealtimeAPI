use crate::rpc::pipeline::StreamingPipeline;
use crate::rpc::{
    message_adapter::WsMessage,
    protocol::{self, ProtocolId},
    realtime_event,
    request_normalizer::StartRequestNormalizer,
    session_manager::SessionManager,
};
use anyhow::Result;
use dashmap::DashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

const EARLY_PACKET_MAX_PER_SESSION: usize = 64;
const EARLY_PACKET_MAX_GLOBAL: usize = 4096;
const EARLY_PACKET_FLUSH_RETRY_DELAY_MS: u64 = 30;
const EARLY_PACKET_FLUSH_MAX_WAIT_MS: u64 = 10_000;

#[derive(Clone)]
struct PendingUpstreamPacket {
    connection_id: String,
    message: WsMessage,
}

#[derive(Default)]
struct AudioSegmentStats {
    packets: u64,
    bytes: u64,
}

/// 事件处理器 - 负责处理WebSocket消息和业务逻辑
pub struct EventHandler {
    session_manager: Arc<SessionManager>,
    pending_upstream_packets: Arc<DashMap<String, VecDeque<PendingUpstreamPacket>>>,
    pending_upstream_total: Arc<AtomicUsize>,
    active_flush_workers: Arc<DashMap<String, ()>>,
    audio_segment_stats: Arc<DashMap<String, AudioSegmentStats>>,
}

impl EventHandler {
    pub fn new(session_manager: Arc<SessionManager>) -> Self {
        Self {
            session_manager,
            pending_upstream_packets: Arc::new(DashMap::new()),
            pending_upstream_total: Arc::new(AtomicUsize::new(0)),
            active_flush_workers: Arc::new(DashMap::new()),
            audio_segment_stats: Arc::new(DashMap::new()),
        }
    }

    /// 处理WebSocket文本消息
    pub async fn handle_websocket_message(&self, ws_message: protocol::WebSocketMessage, connection_id: &str, ws_tx: &mpsc::UnboundedSender<WsMessage>) -> Result<(), String> {
        match ws_message.command_id {
            protocol::CommandId::Start => self.handle_start_command(&ws_message, connection_id, ws_tx).await,
            protocol::CommandId::StopInput => self.handle_stop_input_command(&ws_message, connection_id).await,
            protocol::CommandId::Stop => self.handle_stop_command(&ws_message, connection_id).await,
            protocol::CommandId::Interrupt => self.handle_interrupt_command(&ws_message, connection_id).await,
            protocol::CommandId::TextData => self.handle_text_data_command(&ws_message, connection_id).await,
            protocol::CommandId::Result => self.handle_result_command(&ws_message, ws_tx).await,
            _ => {
                error!("❌ 收到未知命令: {}", ws_message.command_id.as_u8());
                Ok(())
            },
        }
    }

    /// 处理二进制消息
    pub async fn handle_binary_message(&self, _header: protocol::BinaryHeader, data: &bytes::Bytes, connection_id: &str) -> Result<(), String> {
        // 解析完整的二进制消息
        let binary_message = match protocol::BinaryMessage::from_bytes(data) {
            Ok(msg) => msg,
            Err(e) => {
                error!("❌ 二进制消息解析失败: {}", e);
                return Err(format!("二进制消息解析失败: {}", e));
            },
        };

        // 验证会话ID
        if binary_message.session_id().is_empty() {
            return Err("无效的会话ID".to_string());
        }

        // 根据命令ID分发处理
        match binary_message.header.command_id {
            protocol::CommandId::Start => self.handle_binary_start_command(&binary_message, connection_id).await,
            protocol::CommandId::Stop => self.handle_binary_stop_command(&binary_message, connection_id).await,
            protocol::CommandId::AudioChunk => self.handle_binary_audio_chunk(&binary_message, connection_id).await,
            protocol::CommandId::TextData => self.handle_binary_text_data(&binary_message, connection_id).await,
            protocol::CommandId::StopInput => self.handle_binary_stop_input_command(&binary_message, connection_id).await,
            protocol::CommandId::ImageData => self.handle_binary_image_data(&binary_message, connection_id).await,
            protocol::CommandId::Interrupt => self.handle_binary_interrupt_command(&binary_message, connection_id).await,
            _ => {
                error!("❌ 收到未知的二进制命令: {}", binary_message.header.command_id.as_u8());
                Err(format!("未知的二进制命令: {}", binary_message.header.command_id.as_u8()))
            },
        }
    }

    // ── Start ─────────────────────────────────────────────────

    /// 处理Start命令
    async fn handle_start_command(&self, ws_message: &protocol::WebSocketMessage, connection_id: &str, ws_tx: &mpsc::UnboundedSender<WsMessage>) -> Result<(), String> {
        let handle_start_time = std::time::Instant::now();
        info!(
            "🚀 收到startSession请求: session_id={}, connection_id={}, protocol_id={:?}",
            ws_message.session_id, connection_id, ws_message.protocol_id
        );

        let normalized = StartRequestNormalizer::normalize(ws_message);

        match normalized.speech_mode {
            crate::asr::SpeechMode::PushToTalk => {
                info!("📱 启用PTT模式: session_id={}", normalized.session_id);
            },
            crate::asr::SpeechMode::VadDeferred => {
                info!("🔄 启用VAD延迟模式: session_id={}", normalized.session_id);
            },
            crate::asr::SpeechMode::Vad => {
                info!("🎤 使用默认VAD模式: session_id={}", normalized.session_id);
            },
        }

        let create_result = tokio::time::timeout(
            Duration::from_secs(10),
            self.session_manager.create_session(
                &normalized.session_id,
                connection_id,
                normalized.protocol_id,
                normalized.speech_mode,
                normalized.payload.as_ref(),
            ),
        )
        .await;

        match create_result {
            Ok(Ok(_)) => {
                info!(
                    "✅ 会话处理成功: session_id={} | ⏱️ handle_start_command 总耗时: {:?}",
                    ws_message.session_id,
                    handle_start_time.elapsed()
                );
                self.spawn_flush_worker_if_needed(&ws_message.session_id);

                self.send_session_created_event(ws_message, ws_tx).await;
            },
            Ok(Err(e)) => {
                error!("❌ 会话处理失败: session_id={}, error={}", ws_message.session_id, e);

                let error_response = crate::rpc::realtime_event::create_error_event_message(ws_message.session_id.clone(), 500, &format!("会话创建失败: {}", e));
                if let Ok(json) = error_response.to_json() {
                    let _ = ws_tx.send(WsMessage::Text(json));
                }
            },
            Err(_) => {
                error!("⏰ 会话处理超时: session_id={}", ws_message.session_id);

                let timeout_response = crate::rpc::realtime_event::create_error_event_message(ws_message.session_id.clone(), 408, "会话创建超时");
                if let Ok(json) = timeout_response.to_json() {
                    let _ = ws_tx.send(WsMessage::Text(json));
                }
            },
        }

        Ok(())
    }

    // ── StopInput / Interrupt / Stop ──────────────────────────

    /// 处理StopInput命令
    async fn handle_stop_input_command(&self, ws_message: &protocol::WebSocketMessage, connection_id: &str) -> Result<(), String> {
        info!("🛑 收到StopInput命令: {}", ws_message.session_id);
        if let Some((_, stats)) = self.audio_segment_stats.remove(&ws_message.session_id) {
            info!(
                "🎧 [AUDIO-SEGMENT] StopInput统计 | session={} | packets={} | bytes={}",
                ws_message.session_id, stats.packets, stats.bytes
            );
        }

        let binary_header = protocol::BinaryHeader::new(ws_message.session_id.clone(), ws_message.protocol_id, ws_message.command_id).map_err(|e| format!("创建二进制头失败: {}", e))?;

        let binary_data = binary_header.to_bytes().map_err(|e| format!("序列化二进制头失败: {}", e))?;

        self.session_manager
            .forward_message_from_connection(
                &ws_message.session_id,
                connection_id,
                WsMessage::Binary(bytes::Bytes::from(binary_data)),
            )
            .await
            .map_err(|e| format!("转发StopInput失败: {}", e))?;

        info!("✅ StopInput 已转发: session_id={}", ws_message.session_id);
        Ok(())
    }

    /// 用户按钮打断：转发给会话 pipeline，不销毁 session
    async fn handle_interrupt_command(&self, ws_message: &protocol::WebSocketMessage, connection_id: &str) -> Result<(), String> {
        info!("🛑 收到Interrupt命令(用户按钮打断): {}", ws_message.session_id);

        let binary_header = protocol::BinaryHeader::new(ws_message.session_id.clone(), ws_message.protocol_id, ws_message.command_id).map_err(|e| format!("创建二进制头失败: {}", e))?;

        let binary_data = binary_header.to_bytes().map_err(|e| format!("序列化二进制头失败: {}", e))?;

        self.session_manager
            .forward_message_from_connection(
                &ws_message.session_id,
                connection_id,
                WsMessage::Binary(bytes::Bytes::from(binary_data)),
            )
            .await
            .map_err(|e| format!("转发Interrupt失败: {}", e))?;

        info!("✅ Interrupt 已转发: session_id={}", ws_message.session_id);
        Ok(())
    }

    /// 处理Stop命令
    async fn handle_stop_command(&self, ws_message: &protocol::WebSocketMessage, _connection_id: &str) -> Result<(), String> {
        info!("🛑 开始销毁会话: session_id={}", ws_message.session_id);
        self.clear_pending_session_queue(&ws_message.session_id);
        self.audio_segment_stats.remove(&ws_message.session_id);

        let destroy_result = tokio::time::timeout(
            Duration::from_secs(8),
            self.session_manager.destroy_session(&ws_message.session_id),
        )
        .await;

        match destroy_result {
            Ok(Ok(_)) => {
                info!("✅ 会话销毁成功: session_id={}", ws_message.session_id);
            },
            Ok(Err(e)) => {
                error!("❌ 销毁会话失败: session_id={}, error={}", ws_message.session_id, e);
            },
            Err(_) => {
                error!("⏰ 销毁会话超时: session_id={}", ws_message.session_id);
            },
        };

        Ok(())
    }

    // ── TextData ──────────────────────────────────────────────

    /// 处理TextData命令
    async fn handle_text_data_command(&self, ws_message: &protocol::WebSocketMessage, connection_id: &str) -> Result<(), String> {
        // 支持 TTS-only、文本-LLM-TTS 和 混合模式的文本输入
        if ws_message.protocol_id == ProtocolId::Tts || ws_message.protocol_id == ProtocolId::Llm || ws_message.protocol_id == ProtocolId::All {
            if let Some(protocol::MessagePayload::TextData { text }) = &ws_message.payload {
                if !text.trim().is_empty() {
                    let preview = if text.chars().count() > 50 {
                        let truncated: String = text.chars().take(50).collect();
                        format!("{}...", truncated)
                    } else {
                        text.to_string()
                    };

                    let mode_name = match ws_message.protocol_id {
                        ProtocolId::Tts => "TTS-only",
                        ProtocolId::Llm => "文本-LLM-TTS",
                        ProtocolId::All => "混合模式",
                        _ => "未知模式",
                    };
                    info!("📝 {}收到JSON文本输入: {}", mode_name, preview);

                    let binary_header = protocol::BinaryHeader::new(ws_message.session_id.clone(), ws_message.protocol_id, ws_message.command_id).map_err(|e| format!("创建二进制头失败: {}", e))?;

                    let text_bytes = text.as_bytes();
                    let mut binary_data = binary_header.to_bytes().map_err(|e| format!("序列化二进制头失败: {}", e))?;
                    binary_data.extend_from_slice(text_bytes);

                    self.forward_or_buffer_upstream(
                        &ws_message.session_id,
                        connection_id,
                        WsMessage::Binary(bytes::Bytes::from(binary_data)),
                    )
                    .await
                    .map_err(|e| format!("转发文本数据失败: {}", e))?;
                    Ok(())
                } else {
                    warn!("⚠️ 收到空的文本数据: session_id={}", ws_message.session_id);
                    Ok(())
                }
            } else {
                error!(
                    "❌ 文本输入模式收到非文本数据类型的payload: session_id={}",
                    ws_message.session_id
                );
                Err("文本输入模式需要文本数据".to_string())
            }
        } else {
            info!("📝 收到非文本输入协议的TextData消息: protocol_id={:?}", ws_message.protocol_id);
            Ok(())
        }
    }

    // ── Result (tool call) ────────────────────────────────────

    /// 处理Result命令
    async fn handle_result_command(&self, ws_message: &protocol::WebSocketMessage, ws_tx: &mpsc::UnboundedSender<WsMessage>) -> Result<(), String> {
        // 处理客户端回传的消息（工具调用结果等）
        if let Some(protocol::MessagePayload::ConversationItemCreate { item, .. }) = &ws_message.payload {
            if let Some(item_type) = item.get("type").and_then(|v| v.as_str()) {
                if item_type == "function_call_output" {
                    let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or_default();
                    let output = item.get("output").and_then(|v| v.as_str()).unwrap_or_default();
                    let is_error = item.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);

                    if !call_id.is_empty() {
                        // 兼容扩展：支持可选 control 字段
                        let control_mode = item
                            .get("control")
                            .and_then(|v| v.get("mode"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        // 可选 tts_text 透传
                        let tts_text = item
                            .get("payload")
                            .and_then(|p| p.get("tts_text"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());

                        let tool_result = crate::rpc::pipeline::asr_llm_tts::tool_call_manager::ToolCallResult {
                            call_id: call_id.to_string(),
                            output: output.to_string(),
                            is_error,
                            control_mode,
                            tts_text,
                        };

                        info!(
                            "📥 收到客户端工具调用结果: call_id={}, output_size={}, is_error={}",
                            call_id,
                            output.len(),
                            is_error
                        );

                        // 发送确认消息
                        let confirm_msg = realtime_event::create_conversation_item_created_message(
                            ws_message.session_id.clone(),
                            item.get("id").and_then(|v| v.as_str()).unwrap_or_default(),
                            "function",
                            "completed",
                            None,
                        );
                        if let Ok(json) = confirm_msg.to_json() {
                            info!(
                                "🔧 [工具事件] session_id={}, event=conversation.item.created, payload_size={}bytes",
                                ws_message.session_id,
                                json.len()
                            );

                            let _ = ws_tx.send(WsMessage::Text(json));
                        }

                        // 转发工具调用结果到LLM
                        match self
                            .session_manager
                            .handle_tool_call_result(&ws_message.session_id, tool_result)
                            .await
                        {
                            Ok(_) => {
                                info!(
                                    "✅ 工具调用结果转发成功: session_id={}, call_id={}",
                                    ws_message.session_id, call_id
                                );
                            },
                            Err(e) => {
                                error!(
                                    "❌ 工具调用结果转发失败: session_id={}, call_id={}, error={}",
                                    ws_message.session_id, call_id, e
                                );
                                let error_response = realtime_event::create_error_event_message(ws_message.session_id.clone(), 500, &format!("工具调用结果处理失败: {}", e));
                                if let Ok(json) = error_response.to_json() {
                                    info!(
                                        "🔧 [工具事件] session_id={}, event=error, payload_size={}bytes",
                                        ws_message.session_id,
                                        json.len()
                                    );

                                    let _ = ws_tx.send(WsMessage::Text(json));
                                }
                            },
                        }
                    } else {
                        error!("❌ 工具调用结果缺少call_id: session_id={}", ws_message.session_id);
                    }
                } else {
                    info!("📝 收到非工具调用类型的conversation.item.create: type={}", item_type);
                }
            } else {
                error!("❌ conversation.item.create缺少type字段: session_id={}", ws_message.session_id);
            }
        }
        Ok(())
    }

    /// 发送会话创建事件
    async fn send_session_created_event(&self, ws_message: &protocol::WebSocketMessage, ws_tx: &mpsc::UnboundedSender<WsMessage>) {
        let conv_id = format!("sess_{}", ws_message.session_id);
        let session_created_event = realtime_event::create_session_created_message(ws_message.protocol_id, ws_message.session_id.clone(), &conv_id);

        if let Ok(json) = session_created_event.to_json() {
            info!(
                "🔧 [会话事件] session_id={}, event=session.created, payload_size={}bytes",
                ws_message.session_id,
                json.len()
            );

            let _ = ws_tx.send(WsMessage::Text(json));
        }
    }

    // ── Binary handlers ───────────────────────────────────────

    async fn handle_binary_start_command(&self, binary_message: &protocol::BinaryMessage, connection_id: &str) -> Result<(), String> {
        info!(
            "🚀 收到二进制Start命令: session_id={}, connection_id={}, protocol_id={:?}",
            binary_message.session_id(),
            connection_id,
            binary_message.header.protocol_id
        );

        let ws_message = protocol::WebSocketMessage::new(
            binary_message.header.protocol_id,
            binary_message.header.command_id,
            binary_message.session_id().to_string(),
            None,
        );

        let ws_sender = self
            .session_manager
            .get_connection_sender(connection_id)
            .ok_or_else(|| "无法获取WebSocket发送器".to_string())?;

        self.handle_start_command(&ws_message, connection_id, &ws_sender).await
    }

    async fn handle_binary_stop_command(&self, binary_message: &protocol::BinaryMessage, connection_id: &str) -> Result<(), String> {
        info!(
            "🛑 收到二进制Stop命令: session_id={}, connection_id={}",
            binary_message.session_id(),
            connection_id
        );

        let ws_message = protocol::WebSocketMessage::new(
            binary_message.header.protocol_id,
            binary_message.header.command_id,
            binary_message.session_id().to_string(),
            None,
        );

        self.handle_stop_command(&ws_message, connection_id).await
    }

    async fn handle_binary_audio_chunk(&self, binary_message: &protocol::BinaryMessage, connection_id: &str) -> Result<(), String> {
        if binary_message.payload.is_empty() {
            warn!("⚠️ 收到空的音频数据块: session_id={}", binary_message.session_id());
            return Ok(());
        }

        let expected_sample_count = binary_message.payload.len() / 2;
        if expected_sample_count == 0 {
            warn!(
                "⚠️ 音频数据格式异常: session_id={}, payload_size={}",
                binary_message.session_id(),
                binary_message.payload.len()
            );
        }

        {
            let mut stats = self
                .audio_segment_stats
                .entry(binary_message.session_id().to_string())
                .or_default();
            stats.packets = stats.packets.saturating_add(1);
            stats.bytes = stats.bytes.saturating_add(binary_message.payload.len() as u64);
        }

        let full_bytes = match binary_message.to_bytes() {
            Ok(b) => b,
            Err(e) => {
                error!("❌ BinaryMessage.to_bytes 失败: {}", e);
                return Err(format!("序列化音频二进制失败: {}", e));
            },
        };
        let message = crate::rpc::message_adapter::WsMessage::Binary(bytes::Bytes::from(full_bytes));
        match self
            .forward_or_buffer_upstream(binary_message.session_id(), connection_id, message)
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => {
                error!(
                    "🎵 [EventHandler] 音频数据转发失败: session_id={}, error={}",
                    binary_message.session_id(),
                    e
                );
                Err(e.to_string())
            },
        }
    }

    async fn handle_binary_text_data(&self, binary_message: &protocol::BinaryMessage, connection_id: &str) -> Result<(), String> {
        let text = match String::from_utf8(binary_message.payload.clone()) {
            Ok(t) => t,
            Err(e) => {
                error!("❌ 二进制文本数据UTF-8解析失败: {}", e);
                return Err(format!("文本数据编码错误: {}", e));
            },
        };

        if text.is_empty() {
            warn!("⚠️ 收到空的文本数据: session_id={}", binary_message.session_id());
            return Ok(());
        }

        info!(
            "📝 收到二进制文本数据: session_id={}, text_length={}",
            binary_message.session_id(),
            text.len()
        );

        let ws_message = protocol::WebSocketMessage::new(
            binary_message.header.protocol_id,
            binary_message.header.command_id,
            binary_message.session_id().to_string(),
            Some(protocol::MessagePayload::TextData { text }),
        );

        self.handle_text_data_command(&ws_message, connection_id).await
    }

    async fn handle_binary_stop_input_command(&self, binary_message: &protocol::BinaryMessage, connection_id: &str) -> Result<(), String> {
        info!(
            "⏹️ 收到二进制StopInput命令: session_id={}, connection_id={}",
            binary_message.session_id(),
            connection_id
        );

        let ws_message = protocol::WebSocketMessage::new(
            binary_message.header.protocol_id,
            binary_message.header.command_id,
            binary_message.session_id().to_string(),
            None,
        );

        self.handle_stop_input_command(&ws_message, connection_id).await
    }

    async fn handle_binary_interrupt_command(&self, binary_message: &protocol::BinaryMessage, connection_id: &str) -> Result<(), String> {
        info!(
            "🛑 收到二进制Interrupt命令(用户按钮打断): session_id={}, connection_id={}",
            binary_message.session_id(),
            connection_id
        );

        let full_bytes = match binary_message.to_bytes() {
            Ok(b) => b,
            Err(e) => {
                error!("❌ BinaryMessage.to_bytes 失败: {}", e);
                return Err(format!("序列化Interrupt二进制失败: {}", e));
            },
        };
        let message = crate::rpc::message_adapter::WsMessage::Binary(bytes::Bytes::from(full_bytes));
        self.session_manager
            .forward_message_from_connection(binary_message.session_id(), connection_id, message)
            .await
            .map_err(|e| format!("转发Interrupt失败: {}", e))?;

        Ok(())
    }

    async fn handle_binary_image_data(&self, binary_message: &protocol::BinaryMessage, connection_id: &str) -> Result<(), String> {
        let session_id = binary_message.session_id().to_string();

        info!(
            "📷 收到ImageData: session_id={}, connection_id={}, payload_size={} bytes",
            session_id,
            connection_id,
            binary_message.payload.len()
        );

        let router = self.session_manager.get_router();

        let (maybe_interrupt, maybe_text_done_only, maybe_signal_only, maybe_output, maybe_pacing, maybe_tts_config, maybe_voice_setting, maybe_chinese_convert) =
            self.session_manager.get_session_vision_inherit(&session_id).await;

        let vision = crate::rpc::pipeline::vision_tts::streaming_pipeline::VisionTtsPipeline::new(
            session_id.clone(),
            router.clone(),
            maybe_interrupt,
            maybe_text_done_only,
            maybe_signal_only,
            maybe_tts_config,
            maybe_voice_setting,
        );

        if let Some(out_cfg) = maybe_output {
            let mut g = vision.inherited_output_config.lock().await;
            *g = Some(out_cfg);
        }
        if let Some(pacing) = maybe_pacing {
            let mut g = vision.inherited_pacing.lock().await;
            *g = Some(pacing);
        }
        if let Some(mode) = maybe_chinese_convert {
            if let Ok(mut g) = vision.tts_chinese_convert_mode.write() {
                *g = mode;
            }
        }

        let image_bytes = binary_message.to_bytes().map_err(|e| format!("序列化二进制失败: {}", e))?;

        tokio::spawn(async move {
            if let Err(e) = async {
                vision
                    .start()
                    .await
                    .map_err(|e| anyhow::anyhow!("启动VisionTtsPipeline失败: {}", e))?;

                let bin = protocol::BinaryMessage::from_bytes(&image_bytes).map_err(|e| anyhow::anyhow!("重建二进制消息失败: {}", e))?;
                vision.on_upstream(bin).await.map_err(|e| anyhow::anyhow!(e.to_string()))?;

                Ok::<(), anyhow::Error>(())
            }
            .await
            {
                tracing::warn!("⚠️ VisionTtsPipeline 处理失败: {}", e);
            }
        });

        Ok(())
    }

    // ── Early-packet buffering ────────────────────────────────

    async fn forward_or_buffer_upstream(&self, session_id: &str, connection_id: &str, message: WsMessage) -> Result<(), String> {
        match self
            .session_manager
            .forward_message_from_connection(session_id, connection_id, message.clone())
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if e.contains("队列已满") => {
                self.buffer_early_packet(session_id, connection_id, message);
                self.spawn_flush_worker_if_needed(session_id);
                Ok(())
            },
            Err(e) => Err(e),
        }
    }

    fn buffer_early_packet(&self, session_id: &str, connection_id: &str, message: WsMessage) {
        if self
            .pending_upstream_total
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                if current >= EARLY_PACKET_MAX_GLOBAL { None } else { Some(current + 1) }
            })
            .is_err()
        {
            warn!(
                "⚠️ 全局早期缓冲已满，丢弃数据包: session_id={}, global_max={}",
                session_id, EARLY_PACKET_MAX_GLOBAL
            );
            return;
        }

        let mut queue = self.pending_upstream_packets.entry(session_id.to_string()).or_default();

        if queue.len() >= EARLY_PACKET_MAX_PER_SESSION {
            queue.pop_front();
            self.pending_upstream_total.fetch_sub(1, Ordering::AcqRel);
            warn!(
                "⚠️ 会话早期缓冲已满，丢弃最旧数据包: session_id={}, max={}",
                session_id, EARLY_PACKET_MAX_PER_SESSION
            );
        }

        queue.push_back(PendingUpstreamPacket { connection_id: connection_id.to_string(), message });

        info!(
            "🎵 已缓存早期数据包: session_id={}, buffered={}, global_buffered={}",
            session_id,
            queue.len(),
            self.pending_upstream_total.load(Ordering::Acquire)
        );
    }

    fn clear_pending_session_queue(&self, session_id: &str) {
        if let Some((_, queue)) = self.pending_upstream_packets.remove(session_id) {
            let removed = queue.len();
            if removed > 0 {
                self.pending_upstream_total.fetch_sub(removed, Ordering::AcqRel);
            }
        }
    }

    fn spawn_flush_worker_if_needed(&self, session_id: &str) {
        if self.active_flush_workers.insert(session_id.to_string(), ()).is_some() {
            return;
        }

        let sid = session_id.to_string();
        let session_manager = Arc::clone(&self.session_manager);
        let pending_packets = Arc::clone(&self.pending_upstream_packets);
        let pending_total = Arc::clone(&self.pending_upstream_total);
        let active_workers = Arc::clone(&self.active_flush_workers);

        tokio::spawn(async move {
            let started_at = Instant::now();
            loop {
                let next_packet = pending_packets.get(&sid).and_then(|queue| queue.front().cloned());

                let Some(packet) = next_packet else {
                    if let Some((_, queue)) = pending_packets.remove(&sid) {
                        let removed = queue.len();
                        if removed > 0 {
                            pending_total.fetch_sub(removed, Ordering::AcqRel);
                        }
                    }

                    active_workers.remove(&sid);

                    let has_pending = pending_packets.get(&sid).map(|queue| !queue.is_empty()).unwrap_or(false);
                    if has_pending && active_workers.insert(sid.clone(), ()).is_none() {
                        continue;
                    }
                    return;
                };

                match session_manager
                    .forward_message_from_connection(&sid, &packet.connection_id, packet.message.clone())
                    .await
                {
                    Ok(_) => {
                        if let Some(mut queue) = pending_packets.get_mut(&sid) {
                            if queue.pop_front().is_some() {
                                pending_total.fetch_sub(1, Ordering::AcqRel);
                            }
                            if queue.is_empty() {
                                drop(queue);
                                pending_packets.remove(&sid);
                            }
                        } else {
                            continue;
                        }
                    },
                    Err(e) if e.contains("队列已满") => {
                        if started_at.elapsed() >= Duration::from_millis(EARLY_PACKET_FLUSH_MAX_WAIT_MS) {
                            warn!(
                                "⚠️ 缓冲 flush 超时，丢弃剩余数据包: session_id={}, waited_ms={}",
                                sid, EARLY_PACKET_FLUSH_MAX_WAIT_MS
                            );
                            if let Some((_, queue)) = pending_packets.remove(&sid) {
                                let removed = queue.len();
                                if removed > 0 {
                                    pending_total.fetch_sub(removed, Ordering::AcqRel);
                                }
                            }
                            continue;
                        }
                        tokio::time::sleep(Duration::from_millis(EARLY_PACKET_FLUSH_RETRY_DELAY_MS)).await;
                    },
                    Err(e) => {
                        warn!("⚠️ 缓冲 flush 转发出错，跳过该包: session_id={}, error={}", sid, e);
                        if let Some(mut queue) = pending_packets.get_mut(&sid) {
                            if queue.pop_front().is_some() {
                                pending_total.fetch_sub(1, Ordering::AcqRel);
                            }
                            if queue.is_empty() {
                                drop(queue);
                                pending_packets.remove(&sid);
                            }
                        }
                    },
                }
            }
        });
    }
}
