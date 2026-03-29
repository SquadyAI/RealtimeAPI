use tokio::sync::{broadcast, mpsc, watch};

use crate::asr::AsrSession;

use super::simple_interrupt_manager::SimpleInterruptEvent;

/// 统一的 ASR 输入消息（音频/PTT结束/直接文本）
#[derive(Debug)]
pub enum AsrInputMessage {
    Audio(Vec<f32>),
    PttEnd,
    /// 🆕 直接文本输入（用于文本-LLM-TTS混合模式）
    DirectText(String),
}

/// 统一的ASR任务事件
pub enum CoreEvent {
    InputAudio(Vec<f32>),
    PttEnd,
    Timeout,
    /// VAD 超时通道已关闭（发送端已停止/退出）
    TimeoutClosed,
    Interrupt(SimpleInterruptEvent),
    Cleanup,
    LanguageChanged(Option<String>),
    /// 🆕 VAD 运行时参数变化 (threshold, min_silence_ms, min_speech_ms)
    VadConfigChanged(Option<(Option<f32>, Option<u32>, Option<u32>)>),
    /// 🆕 ASR 引擎变更（用于同传模式动态切换 ASR 引擎）
    AsrEngineChanged(Option<String>),
    Closed,
}

/// 统一的select!事件轮询（一次返回一个事件）
#[allow(clippy::type_complexity)]
pub async fn poll_core_event(
    asr_session: &mut AsrSession,
    input_rx: &mut mpsc::Receiver<AsrInputMessage>,
    interrupt_rx: &mut broadcast::Receiver<SimpleInterruptEvent>,
    cleanup_rx: &mut mpsc::UnboundedReceiver<()>,
    asr_language_rx: &mut Option<watch::Receiver<Option<String>>>,
    vad_runtime_rx: &mut Option<watch::Receiver<Option<(Option<f32>, Option<u32>, Option<u32>)>>>,
    asr_engine_rx: &mut Option<watch::Receiver<Option<String>>>,
) -> CoreEvent {
    // 若存在VAD超时接收器，则包含timeout分支
    if let Some(timeout_rx) = asr_session.get_timeout_receiver_mut() {
        tokio::select! {
            maybe_msg = input_rx.recv() => {
                match maybe_msg {
                    Some(AsrInputMessage::Audio(chunk)) => CoreEvent::InputAudio(chunk),
                    Some(AsrInputMessage::PttEnd) => CoreEvent::PttEnd,
                    Some(AsrInputMessage::DirectText(_)) => {
                        // DirectText 不应该到达ASR任务，记录警告并忽略
                        tracing::warn!("⚠️ ASR任务收到DirectText消息，这不应该发生，已忽略");
                        // 递归调用继续等待下一个事件
                        return Box::pin(poll_core_event(
                            asr_session, input_rx, interrupt_rx, cleanup_rx, asr_language_rx, vad_runtime_rx, asr_engine_rx
                        )).await
                    },
                    None => CoreEvent::Closed,
                }
            }
            timeout_msg = timeout_rx.recv() => {
                if timeout_msg.is_some() {
                    CoreEvent::Timeout
                } else {
                    // 发送端已关闭；不要当作超时事件重复触发
                    CoreEvent::TimeoutClosed
                }
            },
            Ok(event) = interrupt_rx.recv() => CoreEvent::Interrupt(event),
            _ = cleanup_rx.recv() => CoreEvent::Cleanup,
            _ = async {
                if let Some(rx) = asr_language_rx.as_mut() {
                    let _ = rx.changed().await;
                }
            }, if asr_language_rx.is_some() => {
                let new_lang = asr_language_rx.as_ref().and_then(|rx| rx.borrow().clone());
                CoreEvent::LanguageChanged(new_lang)
            }
            _ = async {
                if let Some(rx) = vad_runtime_rx.as_mut() {
                    let _ = rx.changed().await;
                }
            }, if vad_runtime_rx.is_some() => {
                let cfg = vad_runtime_rx.as_ref().and_then(|rx| *rx.borrow());
                CoreEvent::VadConfigChanged(cfg)
            }
            _ = async {
                if let Some(rx) = asr_engine_rx.as_mut() {
                    let _ = rx.changed().await;
                }
            }, if asr_engine_rx.is_some() => {
                let new_engine = asr_engine_rx.as_ref().and_then(|rx| rx.borrow().clone());
                CoreEvent::AsrEngineChanged(new_engine)
            }
        }
    } else {
        tokio::select! {
            maybe_msg = input_rx.recv() => {
                match maybe_msg {
                    Some(AsrInputMessage::Audio(chunk)) => CoreEvent::InputAudio(chunk),
                    Some(AsrInputMessage::PttEnd) => CoreEvent::PttEnd,
                    Some(AsrInputMessage::DirectText(_)) => {
                        // DirectText 不应该到达ASR任务，记录警告并忽略
                        tracing::warn!("⚠️ ASR任务收到DirectText消息，这不应该发生，已忽略");
                        // 递归调用继续等待下一个事件
                        return Box::pin(poll_core_event(
                            asr_session, input_rx, interrupt_rx, cleanup_rx, asr_language_rx, vad_runtime_rx, asr_engine_rx
                        )).await
                    },
                    None => CoreEvent::Closed,
                }
            }
            Ok(event) = interrupt_rx.recv() => CoreEvent::Interrupt(event),
            _ = cleanup_rx.recv() => CoreEvent::Cleanup,
            _ = async {
                if let Some(rx) = asr_language_rx.as_mut() {
                    let _ = rx.changed().await;
                }
            }, if asr_language_rx.is_some() => {
                let new_lang = asr_language_rx.as_ref().and_then(|rx| rx.borrow().clone());
                CoreEvent::LanguageChanged(new_lang)
            }
            _ = async {
                if let Some(rx) = vad_runtime_rx.as_mut() {
                    let _ = rx.changed().await;
                }
            }, if vad_runtime_rx.is_some() => {
                let cfg = vad_runtime_rx.as_ref().and_then(|rx| *rx.borrow());
                CoreEvent::VadConfigChanged(cfg)
            }
            _ = async {
                if let Some(rx) = asr_engine_rx.as_mut() {
                    let _ = rx.changed().await;
                }
            }, if asr_engine_rx.is_some() => {
                let new_engine = asr_engine_rx.as_ref().and_then(|rx| rx.borrow().clone());
                CoreEvent::AsrEngineChanged(new_engine)
            }
        }
    }
}
