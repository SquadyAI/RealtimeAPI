use tracing::{debug, warn};

/// 过滤 ASR 识别结果中的污染文本
fn filter_asr_pollution(text: &str) -> String {
    const POLLUTION_PATTERNS: &[&str] = &[
        "优优优独播剧场——YoYo Television Series Exclusive",
        "优优优独播剧场",
        "YoYo Television Series Exclusive",
    ];

    let mut result = text.to_string();
    for pattern in POLLUTION_PATTERNS {
        result = result.replace(pattern, "");
    }
    result.trim().to_string()
}

use super::types::TurnContext;
use crate::rpc::{ProtocolId, WsMessage, protocol::WebSocketMessage, realtime_event, session_router::SessionRouter};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub struct EventEmitter {
    router: Arc<SessionRouter>,
    session_id: String,
    /// 当为 true 时，response.text.done 仅发送信令（不包含完整文本）
    text_done_signal_only: Arc<AtomicBool>,
    /// 当为 true 时，除了语音和工具调用之外的所有事件都不发送
    signal_only: Arc<AtomicBool>,
}

impl EventEmitter {
    pub fn new(router: Arc<SessionRouter>, session_id: String, text_done_signal_only: Arc<AtomicBool>, signal_only: Arc<AtomicBool>) -> Self {
        Self { router, session_id, text_done_signal_only, signal_only }
    }

    /// 获取 signal_only 标志引用
    pub fn signal_only_flag(&self) -> Arc<AtomicBool> {
        self.signal_only.clone()
    }

    /// 获取 text_done_signal_only 标志引用
    pub fn text_done_signal_only_flag(&self) -> Arc<AtomicBool> {
        self.text_done_signal_only.clone()
    }

    /// 发送封装好的 WebSocketMessage
    async fn send_ws(&self, msg: WebSocketMessage) {
        debug!(
            "🎤 [TRACE-EVENT] 发送事件 | session_id={} | event={:?}",
            self.session_id, msg.payload
        );
        // 检查是否启用了 signal_only 模式
        let signal_only = self.signal_only.load(Ordering::Acquire);

        // 如果启用了 signal_only 模式，只发送语音和工具调用相关的事件
        if signal_only {
            match &msg.payload {
                // 语音输出相关事件（TTS）
                Some(crate::rpc::protocol::MessagePayload::ResponseAudioDelta { .. }) |
                Some(crate::rpc::protocol::MessagePayload::ResponseAudioDone { .. }) |
                Some(crate::rpc::protocol::MessagePayload::OutputAudioBufferStarted { .. }) |
                Some(crate::rpc::protocol::MessagePayload::OutputAudioBufferCleared { .. }) |
                Some(crate::rpc::protocol::MessagePayload::OutputAudioBufferStopped { .. }) |
                // 工具调用相关事件
                // Some(crate::rpc::protocol::MessagePayload::ResponseFunctionCallArgumentsDelta { .. }) |
                Some(crate::rpc::protocol::MessagePayload::ResponseFunctionCallArgumentsDone { .. }) |
                Some(crate::rpc::protocol::MessagePayload::ResponseFunctionCallResultDone { .. }) |
                // 语音输入/VAD 与 ASR 转录相关事件（必须允许，属于“信令”核心）
                Some(crate::rpc::protocol::MessagePayload::InputAudioSpeechStarted { .. }) |
                Some(crate::rpc::protocol::MessagePayload::InputAudioSpeechStopped { .. }) |
                Some(crate::rpc::protocol::MessagePayload::AsrTranscriptionCompleted { .. }) |
                Some(crate::rpc::protocol::MessagePayload::AsrTranscriptionFailed { .. }) |
                // 语言检测事件（同声传译）
                Some(crate::rpc::protocol::MessagePayload::ResponseLanguageDetected { .. }) |
                // 错误事件总是允许
                Some(crate::rpc::protocol::MessagePayload::ErrorEvent { .. }) => {
                // 允许发送这些事件
                },
                // 其他事件不发送
                _ => return
            }
        }

        if let Ok(json) = msg.to_json() {
            // 🔧 调试：打印语言检测事件的 JSON
            if json.contains("language.detected") {
                tracing::info!("🌐 [DEBUG] 语言检测事件 JSON: {}", json);
            }
            // 🔧 调试：打印实时事件发送
            // if let Ok(event_json) = serde_json::from_str::<serde_json::Value>(&json) {
            //     // let event_type = event_json.get("event").or_else(|| event_json.get("type"))
            //     //     .and_then(|v| v.as_str())
            //     //     .unwrap_or("unknown");
            //     // info!("📢 [实时事件] session_id={}, event={}, payload_size={}bytes",
            //     //       self.session_id, event_type, json.len());
            // } else {
            //     // info!("📢 [实时事件] session_id={}, payload_size={}bytes",
            //     //       self.session_id, json.len());
            // }

            // ⚠️ 注意：下行发送失败以前会被吞掉，debug-device 下输出错误用于定位“看起来像没VAD/没ASR事件”
            let send_res = self.router.send_to_client(&self.session_id, WsMessage::Text(json)).await;
            if let Err(e) = send_res {
                warn!("📤 EventEmitter 下行发送失败: session_id={}, error={}", self.session_id, e);
            }
        }
    }

    /// 会话创建
    pub async fn session_created(&self, protocol_id: ProtocolId) {
        let conv_id = format!("sess_{}", self.session_id);
        let msg = realtime_event::wrapped_session_created(protocol_id, self.session_id.clone(), &conv_id);
        self.send_ws(msg).await;
    }

    /// response.created
    pub async fn response_created(&self, ctx: &TurnContext) {
        let msg = realtime_event::wrapped_response_created(self.session_id.clone(), &ctx.response_id);
        self.send_ws(msg).await;
    }

    /// response.text.delta
    pub async fn response_text_delta(&self, ctx: &TurnContext, content_index: u32, delta: &str) {
        let msg = realtime_event::wrapped_response_text_delta(
            self.session_id.clone(),
            &ctx.response_id,
            &ctx.assistant_item_id,
            0, // output_index
            content_index,
            delta,
        );
        self.send_ws(msg).await;
    }

    /// response.text.done
    pub async fn response_text_done(&self, ctx: &TurnContext, content_index: u32, text: &str) {
        let only_signal = self.text_done_signal_only.load(Ordering::Acquire);
        let payload_text = if only_signal { "" } else { text };
        let msg = realtime_event::wrapped_response_text_done(
            self.session_id.clone(),
            &ctx.response_id,
            &ctx.assistant_item_id,
            0, // output_index
            content_index,
            payload_text,
        );
        self.send_ws(msg).await;
    }

    /// conversation.item.created
    pub async fn conversation_item_created(&self, item_id: &str, role: &str, status: &str, previous_item_id: Option<&str>) {
        let msg = realtime_event::wrapped_conversation_item_created(self.session_id.clone(), item_id, role, status, previous_item_id);
        self.send_ws(msg).await;
    }

    /// conversation.item.truncated
    pub async fn conversation_item_truncated(&self, item_id: &str, audio_end_ms: u32) {
        let msg = realtime_event::wrapped_conversation_item_truncated(
            self.session_id.clone(),
            item_id,
            0, // content_index
            audio_end_ms,
        );
        self.send_ws(msg).await;
    }

    /// output_audio_buffer.cleared
    pub async fn output_audio_buffer_cleared(&self, response_id: &str) {
        let msg = realtime_event::wrapped_output_audio_buffer_cleared(self.session_id.clone(), response_id);
        self.send_ws(msg).await;
    }

    /// 全局错误事件
    pub async fn error_event(&self, code: u16, message: &str) {
        let msg = realtime_event::wrapped_error_event(self.session_id.clone(), code, message);
        self.send_ws(msg).await;
    }

    /// ASR 转录失败事件
    pub async fn asr_transcription_failed(&self, item_id: &str, content_index: u32, code: &str, message: &str) {
        let msg = crate::rpc::realtime_event::create_asr_transcription_failed_message(self.session_id.clone(), item_id, content_index, code, message);
        self.send_ws(msg).await;
    }

    /// response.output_item.added
    pub async fn response_output_item_added(&self, ctx: &TurnContext) {
        let msg = realtime_event::wrapped_response_output_item_added(self.session_id.clone(), &ctx.response_id, 0, &ctx.assistant_item_id);
        self.send_ws(msg).await;
    }

    /// response.output_item.done
    pub async fn response_output_item_done(&self, ctx: &TurnContext) {
        let msg = crate::rpc::realtime_event::wrapped_response_output_item_done(self.session_id.clone(), &ctx.response_id, 0, &ctx.assistant_item_id);
        self.send_ws(msg).await;
    }

    /// response.done
    pub async fn response_done(&self, ctx: &TurnContext, usage: Option<serde_json::Value>) {
        let msg = realtime_event::wrapped_response_done(
            self.session_id.clone(),
            &ctx.response_id,
            vec![realtime_event::conversation_item_ref(&ctx.assistant_item_id)],
            usage,
        );
        self.send_ws(msg).await;
    }

    /// response.function_call_arguments.delta (OpenAI标准)
    pub async fn response_function_call_arguments_delta(&self, ctx: &TurnContext, call_id: &str, delta: &str) {
        let msg = realtime_event::wrapped_response_function_call_arguments_delta(
            self.session_id.clone(),
            &ctx.response_id,
            &ctx.assistant_item_id,
            call_id,
            delta,
        );
        self.send_ws(msg).await;
    }

    /// response.function_call_arguments.done (OpenAI标准)
    pub async fn response_function_call_arguments_done(&self, ctx: &TurnContext, call_id: &str, function_name: &str, arguments: &str) {
        let msg = realtime_event::wrapped_response_function_call_arguments_done(
            self.session_id.clone(),
            &ctx.response_id,
            &ctx.assistant_item_id,
            call_id,
            function_name,
            arguments,
        );
        self.send_ws(msg).await;
    }

    /// response.function_call_result.done
    pub async fn response_function_call_result_done(&self, ctx: &TurnContext, call_id: &str, result: &str) {
        let msg = realtime_event::wrapped_response_function_call_result_done(
            self.session_id.clone(),
            &ctx.response_id,
            &ctx.assistant_item_id,
            call_id,
            result,
        );
        self.send_ws(msg).await;
    }

    pub async fn output_audio_buffer_started(&self, response_id: &str) {
        let msg = crate::rpc::realtime_event::wrapped_output_audio_buffer_started(self.session_id.clone(), response_id);
        self.send_ws(msg).await;
    }

    pub async fn output_audio_buffer_stopped(&self, response_id: &str) {
        let msg = crate::rpc::realtime_event::wrapped_output_audio_buffer_stopped(self.session_id.clone(), response_id);
        self.send_ws(msg).await;
    }

    /// conversation.item.updated
    pub async fn conversation_item_updated(&self, item_id: &str, role: &str, status: &str) {
        let msg = realtime_event::wrapped_conversation_item_updated(self.session_id.clone(), item_id, role, status);
        self.send_ws(msg).await;
    }

    /// response.audio.done
    pub async fn response_audio_done(&self, response_id: &str, item_id: &str, output_index: u32, content_index: u32) {
        let msg = realtime_event::wrapped_response_audio_done(self.session_id.clone(), response_id, item_id, output_index, content_index);
        self.send_ws(msg).await;
    }

    /// input_audio_buffer.speech_started (OpenAI标准命名)
    pub async fn input_audio_buffer_speech_started(&self, item_id: &str, audio_start_ms: u32) {
        let msg = crate::rpc::realtime_event::wrapped_input_audio_speech_started(
            self.session_id.clone(),
            audio_start_ms, // 使用真实开始时间
            item_id,
        );
        self.send_ws(msg).await;
    }

    /// input_audio_buffer.speech_stopped (OpenAI标准命名)
    pub async fn input_audio_buffer_speech_stopped(&self, item_id: &str, audio_end_ms: u32) {
        let msg = crate::rpc::realtime_event::wrapped_input_audio_speech_stopped(
            self.session_id.clone(),
            audio_end_ms, // 使用真实结束时间
            item_id,
        );
        self.send_ws(msg).await;
    }

    /// conversation.item.input_audio_transcription.delta (OpenAI标准命名)
    pub async fn conversation_item_input_audio_transcription_delta(&self, item_id: &str, content_index: u32, text: &str) {
        let filtered_text = filter_asr_pollution(text);
        if filtered_text.is_empty() {
            return;
        }
        let msg = crate::rpc::realtime_event::wrapped_asr_transcription_delta(
            self.session_id.clone(),
            item_id,
            content_index, // 真实 index
            &filtered_text,
        );
        self.send_ws(msg).await;
    }

    /// conversation.item.input_audio_transcription.completed (OpenAI标准命名)
    pub async fn conversation_item_input_audio_transcription_completed(&self, item_id: &str, content_index: u32, text: &str) {
        let filtered_text = filter_asr_pollution(text);
        if filtered_text.is_empty() {
            return;
        }
        let msg = crate::rpc::realtime_event::wrapped_asr_transcription_completed(
            self.session_id.clone(),
            item_id,
            content_index, // 真实 index
            &filtered_text,
        );
        self.send_ws(msg).await;
    }

    /// response.language.detected (同声传译语言检测结果)
    pub async fn response_language_detected(&self, response_id: &str, code: &str) {
        let msg = crate::rpc::realtime_event::wrapped_response_language_detected(self.session_id.clone(), response_id, code);
        self.send_ws(msg).await;
    }
}
