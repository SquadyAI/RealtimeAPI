// 会话级别音频发送器管理模块
// 从 tts_task.rs 拆分出来，负责 PacedAudioSender 的生命周期管理

use anyhow::{Result, anyhow};
use bytes::Bytes;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::audio::OutputAudioConfig;
use crate::rpc::pipeline::paced_sender::{PacedAudioChunk, PacedAudioSender, PacingConfig, RealtimeAudioMetadata};
use crate::rpc::session_router::SessionRouter;

use super::sentence_queue::NextSentenceTrigger;
use super::simple_interrupt_manager::SimpleInterruptHandler;

/// 会话级别的音频发送器管理器
pub struct SessionAudioSender {
    /// PacedAudioSender的发送通道
    sender_tx: Option<mpsc::Sender<PacedAudioChunk>>,
    /// 音频处理任务句柄
    handler_handle: Option<tokio::task::JoinHandle<()>>,
    /// 当前响应ID（用于打断控制）
    current_response_id: Arc<super::lockfree_response_id::LockfreeResponseId>,
    /// 简化打断处理器（接收所有打断信号）
    simple_interrupt_handler_ref: Option<SimpleInterruptHandler>,
    /// 待发送音频块计数器
    pending_counter: Option<Arc<std::sync::atomic::AtomicUsize>>,
    /// 待使用的音频格式配置（用于重新初始化）
    pending_output_config: Option<OutputAudioConfig>,
    /// 运行时节拍参数更新通道（发送端）
    pacing_tx: Option<tokio::sync::watch::Sender<PacingConfig>>,
}

impl Default for SessionAudioSender {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionAudioSender {
    pub fn new() -> Self {
        Self {
            sender_tx: None,
            handler_handle: None,
            current_response_id: Arc::new(super::lockfree_response_id::LockfreeResponseId::new()),
            simple_interrupt_handler_ref: None,
            pending_counter: None,
            pending_output_config: None,
            pacing_tx: None,
        }
    }

    /// 关键架构决策：初始化会话级别的音频发送器（整个会话生命周期只调用一次）
    ///
    /// **重要**：PacedSender在任何情况下都不应该被回收，因为：
    /// 1. 它负责音频缓冲区管理
    /// 2. 打断信号需要通过它来清空缓冲区
    /// 3. 如果重复创建会导致打断时找不到正确的PacedSender
    ///
    /// 因此，这个方法确保：
    /// - 每个会话只创建一个PacedSender
    /// - PacedSender生命周期 = 会话生命周期
    /// - 多轮次TTS复用同一个PacedSender
    #[allow(clippy::too_many_arguments)]
    pub async fn initialize(
        &mut self,
        session_id: String,
        router: Arc<SessionRouter>,
        interrupt_handler: SimpleInterruptHandler,
        burst_count: usize,
        burst_delay: u64,
        rate_multiplier: f64,
        output_config: Option<OutputAudioConfig>, // 完整的音频输出配置
        // 仅语音/工具信令模式标志
        signal_only_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
        // 按需 TTS 生成：下一句触发通道
        next_sentence_trigger_tx: Option<mpsc::UnboundedSender<NextSentenceTrigger>>,
        // 同声传译模式：文字事件由 TranslationTask 发送
        is_translation_mode: bool,
    ) -> Result<()> {
        if self.sender_tx.is_some() {
            // 检查是否有待应用的音频输出配置
            if self.pending_output_config.is_some() {
                info!("🔄 检测到待应用的音频输出配置，需要重新初始化PacedSender");
                // 清理现有的PacedSender以便重新创建
                if let Some(old_handle) = self.handler_handle.take()
                    && !old_handle.is_finished()
                {
                    info!("🛑 终止旧PacedSender以应用新音频格式");
                    old_handle.abort();
                }
                self.sender_tx = None;
                // 不再将 response_id 重置为 None，保持最后的真值
                self.pending_counter = None;
                // 继续下面的重新初始化流程
            } else {
                // 关键修复：已经初始化的PacedSender不重复创建，只更新打断处理器
                info!("🔄 SessionAudioSender已存在且有效，更新全局打断处理器用于新轮次");
                info!("✅ PacedSender架构正确：会话级别唯一，不重复创建");
                self.simple_interrupt_handler_ref = Some(interrupt_handler);
                return Ok(());
            }
        }

        // 关键修复：如果sender_tx为None（被清理过），重新初始化
        if self.sender_tx.is_none() {
            info!("🔧 SessionAudioSender存在但sender_tx已失效，重新初始化");

            // 内存泄漏修复：确保旧资源被完全清理
            if let Some(old_handle) = self.handler_handle.take()
                && !old_handle.is_finished()
            {
                warn!("🛑 发现未完成的旧音频任务，强制终止避免泄漏");
                old_handle.abort();
                // 不等待完成，避免阻塞重新初始化
            }

            // 清理其他潜在的旧资源引用
            // 不再将 response_id 重置为 None，保持最后的真值
            self.pending_counter = None;
            // simple_interrupt_handler_ref会被下面重新设置，无需清理
            // pending_output_config保留，用于重新初始化时使用正确的配置
        }

        info!("🎵 初始化会话级别音频发送器: session={}", session_id);
        info!("🔧 关键架构：创建会话级别唯一的PacedSender，整个会话生命周期复用");

        // 为PacedSender创建打断处理器，响应同会话的打断事件
        // 使用 derive_with_name 保留原 handler 的 ignore_user_speaking 设置
        let paced_sender_interrupt_handler = interrupt_handler.derive_with_name("PacedSender-Session".to_string());

        // 使用传入的完整输出配置，如果没有则检查pending_output_config，最后使用默认PCM配置
        let output_config = if let Some(provided_config) = output_config {
            // 使用调用方提供的完整配置
            provided_config
        } else if let Some(pending_config) = self.pending_output_config.clone() {
            // 使用之前设置的待应用配置
            pending_config
        } else {
            // 默认使用PCM配置，20ms时间片
            OutputAudioConfig::default_pcm(20)
        };

        info!("🎵 SessionAudioSender初始化音频配置: {:?}", output_config);

        // 创建节拍参数watch通道并传入PacedSender
        let (pacing_tx, pacing_rx) = tokio::sync::watch::channel(PacingConfig {
            send_rate_multiplier: rate_multiplier,
            initial_burst_count: burst_count,
            initial_burst_delay_ms: burst_delay,
        });

        let (paced_audio_tx, pending_counter, paced_sender_handle) = PacedAudioSender::new(
            session_id.clone(),
            router,
            16000, // sample_rate - TTS输出固定为16kHz
            1,     // channels - TTS输出固定为单声道
            output_config,
            50, // buffer_size
            Some(paced_sender_interrupt_handler),
            rate_multiplier,
            burst_count,
            burst_delay,
            pacing_rx,
            1, // 简化：直接使用毫秒值 (原值为25，根据用户反馈修改为1以减少首包延迟)
            Arc::new(super::lockfree_response_id::LockfreeResponseIdReader::from_writer(
                &self.current_response_id,
            )), // response_id - 会话级别，动态设置
            None, // assistant_item_id - 会话级别，动态设置
            None, // is_responding_tx
            true, // 启用受控生产模式，确保发送节拍准确
            signal_only_flag,
            next_sentence_trigger_tx, // 按需 TTS 生成
            is_translation_mode,      // 同声传译模式：文字事件由 TranslationTask 发送
        );

        self.sender_tx = Some(paced_audio_tx);
        self.handler_handle = Some(paced_sender_handle);
        self.simple_interrupt_handler_ref = Some(interrupt_handler);
        self.pending_counter = Some(pending_counter);
        self.pacing_tx = Some(pacing_tx);

        // 清理已应用的pending_output_config
        if self.pending_output_config.is_some() {
            info!("✅ 音频输出配置已应用: {:?}", self.pending_output_config);
            self.pending_output_config = None;
        }

        info!("✅ 会话级别音频发送器初始化完成: session={}", session_id);
        info!("🎯 PacedSender架构确认：会话级别唯一，支持多轮次复用，打断信号可正确清空缓冲区");
        Ok(())
    }

    /// 运行时更新节拍参数
    pub fn update_pacing(&mut self, burst_count: usize, burst_delay_ms: u64, rate_multiplier: f64) {
        if let Some(tx) = &self.pacing_tx {
            let _ = tx.send(PacingConfig {
                send_rate_multiplier: rate_multiplier,
                initial_burst_count: burst_count,
                initial_burst_delay_ms: burst_delay_ms,
            });
            info!(
                "🔄 [SessionAudioSender] 已下发新的节拍参数: rate={:.3}x, burst={}, delay={}ms",
                rate_multiplier, burst_count, burst_delay_ms
            );
        } else {
            warn!("⚠️ [SessionAudioSender] 节拍参数通道不存在，可能尚未初始化");
        }
    }

    /// 发送音频数据
    pub async fn send_audio(&self, chunk: PacedAudioChunk) -> Result<()> {
        if let Some(tx) = &self.sender_tx {
            tx.send(chunk).await.map_err(|e| anyhow!("发送音频失败: {}", e))?;
        } else {
            return Err(anyhow!("音频发送器未初始化"));
        }
        Ok(())
    }

    /// 尝试发送音频数据（非阻塞模式）
    pub async fn try_send_audio(&self, chunk: &PacedAudioChunk) -> Result<()> {
        if let Some(tx) = &self.sender_tx {
            match tx.try_send(chunk.clone()) {
                Ok(_) => Ok(()),
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => Err(anyhow!("音频缓冲区已满 - full")),
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Err(anyhow!("音频发送通道已关闭")),
            }
        } else {
            Err(anyhow!("音频发送器未初始化"))
        }
    }

    // 已移除：get_buffer_status / set_current_response_id（统一通过 start_new_turn + update_turn_binding 管理）

    /// 开始新轮次时更新响应ID
    pub async fn start_new_turn(&mut self, new_response_id: String, assistant_item_id: Option<String>) -> Result<()> {
        let old_response_id = self.current_response_id.load();
        info!(
            "🆕 开始新轮次，更新响应ID: old_response={:?}, new_response={}",
            old_response_id, new_response_id
        );

        // 更新当前响应ID
        self.current_response_id.store(Some(new_response_id));

        // 发送一次无副作用的控制块，携带新的 response_id，提示 PacedSender 复位本轮状态
        // 说明：该控制块不会产生 text.delta 或 audio.delta，仅用于让 PacedSender 观察到 response_id 切换
        if let Some(tx) = &self.sender_tx {
            let noop_chunk = PacedAudioChunk {
                audio_data: Bytes::new(),
                is_final: false,
                realtime_metadata: Some(RealtimeAudioMetadata {
                    response_id: self.current_response_id.load().unwrap_or_default(),
                    assistant_item_id: assistant_item_id.unwrap_or_default(),
                    output_index: 0,
                    content_index: 0,
                }),
                sentence_text: None,
                turn_final: false,
            };
            // 非阻塞发送；如满则忽略（后续首个有效chunk同样会触发复位）
            let _ = tx.try_send(noop_chunk);
        }
        Ok(())
    }

    /// 更新PacedSender的轮次绑定
    pub async fn update_turn_binding(&mut self, turn_sequence: u64) -> Result<()> {
        if let Some(ref mut handler) = self.simple_interrupt_handler_ref {
            handler.bind_to_turn(turn_sequence);
            info!("🔗 [SessionAudioSender] PacedSender已绑定到轮次: {}", turn_sequence);
        } else {
            warn!("⚠️ SessionAudioSender没有打断处理器，无法绑定轮次");
        }
        Ok(())
    }

    /// 强制清空音频缓冲区（立即模式，不发送final，不触发flush）
    pub async fn force_clear_buffer(&self) -> Result<()> {
        info!("🛑 强制清空SessionAudioSender缓冲区（不发送final）");

        // 不再发送任何"清理"音频块，完全依赖打断机制让 PacedSender 清空自身缓冲区，
        // 以避免产生 output_audio_buffer.started/stopped 等事件的错误序列。

        // 重置待发送计数器（本地侧统计归零）
        if let Some(ref counter) = self.pending_counter {
            let old_count = counter.load(std::sync::atomic::Ordering::Relaxed);
            counter.store(0, std::sync::atomic::Ordering::Relaxed);
            if old_count > 0 {
                info!("🔄 重置待发送音频块计数器: {} -> 0", old_count);
            }
        }

        // 检查简化打断处理器状态（用于诊断）
        if let Some(ref handler) = self.simple_interrupt_handler_ref {
            if handler.is_interrupted_immutable() {
                info!("✅ 简化打断处理器状态已激活，PacedSender将清理缓冲区");
            } else {
                info!("⚠️ 简化打断处理器状态未激活，若需清理请先广播打断事件");
            }
        } else {
            warn!("⚠️ SessionAudioSender没有配置简化打断处理器");
        }

        info!("✅ SessionAudioSender缓冲区清理完成");
        Ok(())
    }

    /// 检查SessionAudioSender是否已初始化
    pub fn is_initialized(&self) -> bool {
        self.sender_tx.is_some()
    }

    /// 配置完整的音频输出配置
    pub async fn configure_output_config(&mut self, config: OutputAudioConfig) -> Result<()> {
        info!("🔄 SessionAudioSender配置音频输出配置: {:?}", config);

        // 保存新配置用于后续初始化
        self.pending_output_config = Some(config.clone());

        // 检查是否需要重新创建PacedSender来应用新配置
        if self.sender_tx.is_some() {
            info!("🔄 检测到已有PacedSender，由于配置变更需要重新创建");

            // 清理现有的PacedSender
            if let Some(old_handle) = self.handler_handle.take()
                && !old_handle.is_finished()
            {
                info!("🛑 终止旧的PacedSender任务");
                old_handle.abort();
            }

            // 清理其他资源
            self.sender_tx = None;
            // 不清空 response_id，以确保后续使用的是真值
            self.pending_counter = None;

            info!("✅ 旧PacedSender已清理，新配置将在下次initialize时生效");
        } else {
            info!("✅ 没有现有PacedSender，新配置将在下次initialize时使用");
        }

        Ok(())
    }

    /// 清理音频发送器
    pub async fn cleanup(&mut self) {
        self.cleanup_with_force_mode(false).await;
    }

    /// 强制清理音频发送器（用于系统关闭）
    pub async fn force_cleanup(&mut self) {
        self.cleanup_with_force_mode(true).await;
    }

    /// 内部清理逻辑
    async fn cleanup_with_force_mode(&mut self, force_immediate: bool) {
        if force_immediate {
            info!("🔄 开始强制清理会话级别音频发送器（立即模式）");
        } else {
            info!("🔄 开始清理会话级别音频发送器");
        }

        // 关闭发送通道，让PacedSender知道不会再有新的音频数据
        if let Some(tx) = self.sender_tx.take() {
            drop(tx); // 关闭发送端，PacedSender会收到channel关闭信号
            info!("📪 已关闭音频发送通道，PacedSender将处理剩余缓冲数据");
        }

        // 给PacedSender一些时间来flush剩余的音频数据
        if let Some(_handle) = &self.handler_handle {
            if force_immediate {
                info!("⚡ 强制模式：跳过等待，立即继续清理");
            } else {
                info!("⏳ 等待PacedSender完成剩余音频发送...");

                // 先等待基本时间让PacedSender开始处理
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                // 然后检查是否还有待处理的音频数据
                if let Some(pending_counter) = &self.pending_counter {
                    let mut check_count = 0;
                    let mut consecutive_zero_count = 0;
                    const MAX_CONSECUTIVE_ZERO: u32 = 10;
                    const MAX_WAIT_CHECKS: u32 = 200; // 最多等待10秒 (200 * 50ms)

                    loop {
                        let pending_count = pending_counter.load(std::sync::atomic::Ordering::SeqCst);

                        if pending_count == 0 {
                            consecutive_zero_count += 1;
                            if consecutive_zero_count >= MAX_CONSECUTIVE_ZERO {
                                info!("✅ PacedSender缓冲区已确认清空，TTS任务可以安全结束");
                                break;
                            }
                        } else {
                            consecutive_zero_count = 0;
                        }

                        check_count += 1;
                        if check_count >= MAX_WAIT_CHECKS {
                            warn!("⏰ 等待PacedSender清空超时，强制继续清理");
                            break;
                        }

                        if check_count % 100 == 0 {
                            // 每5秒报告一次
                            info!(
                                "ℹ️ TTS任务等待音频播放完成：剩余{}个音频块，已等待{:.1}秒",
                                pending_count,
                                check_count as f64 * 0.05
                            );
                        }

                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                } else {
                    info!("ℹ️ 无pending_counter，TTS任务直接结束");
                }

                info!("✅ TTS任务音频播放等待完成");
            }
        }

        // 现在清理任务句柄
        if let Some(handle) = self.handler_handle.take() {
            if !handle.is_finished() {
                info!("🛑 强制停止音频发送器任务");
                handle.abort();
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
            } else {
                info!("✅ 音频发送器任务已自然结束");
            }
        }

        // 内存泄漏修复：清理所有资源引用（保留 response_id 真值，不清空）
        self.simple_interrupt_handler_ref = None; // 清理打断处理器，释放订阅
        self.pending_counter = None; // 清理计数器Arc引用

        info!("✅ 会话级别音频发送器清理完成");
    }
}
