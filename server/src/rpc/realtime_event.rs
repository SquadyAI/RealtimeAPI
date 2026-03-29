use nanoid::nanoid;
use serde_json::{Value, json};

/// 为 Realtime 事件生成唯一 ID，形如 "event_xxxxx"。
fn gen_event_id() -> String {
    format!("event_{}", nanoid!(8))
}

/// session.created
pub fn session_created(conv_id: &str) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "session.created",
        "conversation": {
            "id": conv_id,
        },
    })
}

/// conversation.item.created（最精简版本，仅 message 类型）
/// role: "user" | "assistant"
pub fn conversation_item_created(prev_item: Option<&str>, item_id: &str, role: &str, status: &str) -> Value {
    let mut base = json!({
        "event_id": gen_event_id(),
        "type": "conversation.item.created",
        "item": {
            "id": item_id,
            "object": "realtime.item",
            "type": "message",
            "status": status,
            "role": role,
            "content": json!([]),
        }
    });
    if let Some(prev) = prev_item {
        base["previous_item_id"] = json!(prev);
    }
    base
}

/// conversation.item.input_audio_transcription.delta
pub fn asr_transcription_delta(item_id: &str, content_index: u32, delta: &str) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "conversation.item.input_audio_transcription.delta",
        "item_id": item_id,
        "content_index": content_index,
        "delta": delta,
    })
}

/// conversation.item.input_audio_transcription.completed
pub fn asr_transcription_completed(item_id: &str, content_index: u32, transcript: &str) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "conversation.item.input_audio_transcription.completed",
        "item_id": item_id,
        "content_index": content_index,
        "transcript": transcript,
    })
}

/// conversation.item.input_audio_transcription.failed
pub fn asr_transcription_failed(item_id: &str, content_index: u32, code: &str, message: &str) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "conversation.item.input_audio_transcription.failed",
        "item_id": item_id,
        "content_index": content_index,
        "error": {
            "type": "transcription_error",
            "code": code,
            "message": message,
        }
    })
}

/// response.created（初始 in_progress 状态）
pub fn response_created(resp_id: &str) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "response.created",
        "response": {
            "id": resp_id,
            "object": "realtime.response",
            "status": "in_progress",
            "status_details": Value::Null,
            "output": json!([]),
            "usage": Value::Null,
        }
    })
}

/// response.text.delta
pub fn response_text_delta(resp_id: &str, item_id: &str, output_index: u32, content_index: u32, delta: &str) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "response.text.delta",
        "response_id": resp_id,
        "item_id": item_id,
        "output_index": output_index,
        "content_index": content_index,
        "delta": delta,
    })
}

/// response.text.done
pub fn response_text_done(resp_id: &str, item_id: &str, output_index: u32, content_index: u32, text: &str) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "response.text.done",
        "response_id": resp_id,
        "item_id": item_id,
        "output_index": output_index,
        "content_index": content_index,
        "text": text,
    })
}

/// response.audio.delta
pub fn response_audio_delta(resp_id: &str, item_id: &str, output_index: u32, content_index: u32, delta_b64: &str) -> Value {
    json!({
        "type": "response.audio.delta",
        "response_id": resp_id,
        "item_id": item_id,
        "output_index": output_index,
        "content_index": content_index,
        "delta": delta_b64,
    })
}

/// response.audio.done
pub fn response_audio_done(resp_id: &str, item_id: &str, output_index: u32, content_index: u32) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "response.audio.done",
        "response_id": resp_id,
        "item_id": item_id,
        "output_index": output_index,
        "content_index": content_index,
    })
}

/// conversation.item.truncated
pub fn conversation_item_truncated(item_id: &str, content_index: u32, audio_end_ms: u32) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "conversation.item.truncated",
        "item_id": item_id,
        "content_index": content_index,
        "audio_end_ms": audio_end_ms,
    })
}

/// input_audio_buffer.speech_started
pub fn input_audio_speech_started(audio_start_ms: u32, item_id: &str) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "input_audio_buffer.speech_started",
        "audio_start_ms": audio_start_ms,
        "item_id": item_id,
    })
}

/// input_audio_buffer.speech_stopped
pub fn input_audio_speech_stopped(audio_end_ms: u32, item_id: &str) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "input_audio_buffer.speech_stopped",
        "audio_end_ms": audio_end_ms,
        "item_id": item_id,
    })
}

/// output_audio_buffer.started
pub fn output_audio_buffer_started(response_id: &str) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "output_audio_buffer.started",
        "response_id": response_id,
    })
}

/// output_audio_buffer.stopped
pub fn output_audio_buffer_stopped(response_id: &str) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "output_audio_buffer.stopped",
        "response_id": response_id,
    })
}

/// response.done
pub fn response_done(resp_id: &str, output_items: Vec<serde_json::Value>, usage: Option<serde_json::Value>) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "response.done",
        "response": {
            "id": resp_id,
            "object": "realtime.response",
            "status": "completed",
            "status_details": Value::Null,
            "output": output_items,
            "usage": usage.unwrap_or(Value::Null),
        }
    })
}

/// response.output_item.added
pub fn response_output_item_added(resp_id: &str, output_index: u32, item: serde_json::Value) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "response.output_item.added",
        "response_id": resp_id,
        "output_index": output_index,
        "item": item,
    })
}

/// response.output_item.done
pub fn response_output_item_done(resp_id: &str, output_index: u32, item: serde_json::Value) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "response.output_item.done",
        "response_id": resp_id,
        "output_index": output_index,
        "item": item,
    })
}

/// output_audio_buffer.cleared
pub fn output_audio_buffer_cleared(response_id: &str) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "output_audio_buffer.cleared",
        "response_id": response_id,
    })
}

/// error.event
pub fn error_event(code: u16, message: &str) -> Value {
    json!({
        "event_id": gen_event_id(),
        "type": "error",
        "error": {
            "type": "server_error",
            "code": code,
            "message": message,
        }
    })
}

// ===== WebSocketMessage包装函数 =====

use crate::rpc::protocol::{CommandId, MessagePayload, ProtocolId, WebSocketMessage};

/// 包装后的session.created消息
pub fn create_session_created_message(protocol_id: ProtocolId, session_id: String, conv_id: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    let session = session_created(conv_id);
    WebSocketMessage::new(
        protocol_id,
        CommandId::Result,
        session_id,
        Some(MessagePayload::SessionCreate { event_id, session }),
    )
}

/// 包装后的input_audio_buffer.speech_started消息
pub fn create_input_audio_speech_started_message(session_id: String, audio_start_ms: u32, item_id: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::InputAudioSpeechStarted { event_id, audio_start_ms, item_id: item_id.to_string() }),
    )
}

/// 包装后的input_audio_buffer.speech_stopped消息
pub fn create_input_audio_speech_stopped_message(session_id: String, audio_end_ms: u32, item_id: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::InputAudioSpeechStopped { event_id, audio_end_ms, item_id: item_id.to_string() }),
    )
}

/// 包装后的conversation.item.input_audio_transcription.delta消息
pub fn create_asr_transcription_delta_message(session_id: String, item_id: &str, content_index: u32, delta: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::AsrTranscriptionDelta { event_id, item_id: item_id.to_string(), content_index, delta: delta.to_string() }),
    )
}

/// 包装后的conversation.item.input_audio_transcription.completed消息
pub fn create_asr_transcription_completed_message(session_id: String, item_id: &str, content_index: u32, transcript: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::AsrTranscriptionCompleted {
            event_id,
            item_id: item_id.to_string(),
            content_index,
            transcript: transcript.to_string(),
        }),
    )
}

/// 包装后的conversation.item.truncated消息
pub fn create_conversation_item_truncated_message(session_id: String, item_id: &str, content_index: u32, audio_end_ms: u32) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ConversationItemTruncated { event_id, item_id: item_id.to_string(), content_index, audio_end_ms }),
    )
}

/// 包装后的response.text.delta消息
pub fn create_response_text_delta_message(session_id: String, resp_id: &str, item_id: &str, output_index: u32, content_index: u32, delta: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ResponseTextDelta {
            event_id,
            response_id: resp_id.to_string(),
            item_id: item_id.to_string(),
            output_index,
            content_index,
            delta: delta.to_string(),
        }),
    )
}

/// 包装后的response.text.done消息
pub fn create_response_text_done_message(session_id: String, resp_id: &str, item_id: &str, output_index: u32, content_index: u32, text: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ResponseTextDone {
            event_id,
            response_id: resp_id.to_string(),
            item_id: item_id.to_string(),
            output_index,
            content_index,
            text: text.to_string(),
        }),
    )
}

/// 包装后的response.audio.delta消息
pub fn create_response_audio_delta_message(session_id: String, resp_id: &str, item_id: &str, output_index: u32, content_index: u32, delta_b64: &str) -> WebSocketMessage {
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ResponseAudioDelta {
            response_id: resp_id.to_string(),
            item_id: item_id.to_string(),
            output_index,
            content_index,
            delta: delta_b64.to_string(),
        }),
    )
}

/// 🆕 创建二进制格式的response.audio.delta消息
/// 直接使用BinaryMessage，避免JSON序列化开销
pub fn create_response_audio_delta_binary_message(
    session_id: String,
    resp_id: &str,
    item_id: &str,
    output_index: u32,
    content_index: u32,
    audio_data: &[u8],
) -> Result<crate::rpc::protocol::BinaryMessage, crate::rpc::protocol::ProtocolError> {
    crate::rpc::protocol::BinaryMessage::response_audio_delta(session_id, resp_id, item_id, output_index, content_index, audio_data)
}

/// 包装后的response.audio.done消息
pub fn create_response_audio_done_message(session_id: String, resp_id: &str, item_id: &str, output_index: u32, content_index: u32) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ResponseAudioDone {
            event_id,
            response_id: resp_id.to_string(),
            item_id: item_id.to_string(),
            output_index,
            content_index,
        }),
    )
}

/// 包装后的output_audio_buffer.started消息
pub fn create_output_audio_buffer_started_message(session_id: String, response_id: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::OutputAudioBufferStarted { event_id, response_id: response_id.to_string() }),
    )
}

/// 包装后的output_audio_buffer.stopped消息
pub fn create_output_audio_buffer_stopped_message(session_id: String, response_id: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::OutputAudioBufferStopped { event_id, response_id: response_id.to_string() }),
    )
}

/// 包装后的output_audio_buffer.cleared消息
pub fn create_output_audio_buffer_cleared_message(session_id: String, response_id: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::OutputAudioBufferCleared { event_id, response_id: response_id.to_string() }),
    )
}

/// 包装后的error.event消息
pub fn create_error_event_message(session_id: String, code: u16, message: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ErrorEvent { event_id, code, message: message.to_string() }),
    )
}

/// 包装后的response.language.detected消息
pub fn wrapped_response_language_detected(session_id: String, response_id: &str, code: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::Translation,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ResponseLanguageDetected { event_id, response_id: response_id.to_string(), code: code.to_string() }),
    )
}

/// 包装后的transcription.failed
pub fn create_asr_transcription_failed_message(session_id: String, item_id: &str, content_index: u32, code: &str, message: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    let error_val: Value = serde_json::json!({
        "type": "transcription_error",
        "code": code,
        "message": message,
    });
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::AsrTranscriptionFailed { event_id, item_id: item_id.to_string(), content_index, error: error_val }),
    )
}

/// 会话项目引用
pub fn conversation_item_ref(item_id: &str) -> Value {
    json!({
        "id": item_id,
        "object": "realtime.item"
    })
}

/// 包装后的response.created消息
pub fn create_response_created_message(session_id: String, response_id: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    let response = json!({
        "id": response_id,
        "object": "realtime.response",
        "status": "in_progress",
        "status_details": Value::Null,
        "output": json!([]),
        "usage": Value::Null,
    });
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ResponseCreated { event_id, response }),
    )
}

/// 包装后的conversation.item.created消息
pub fn create_conversation_item_created_message(session_id: String, item_id: &str, role: &str, status: &str, previous_item_id: Option<&str>) -> WebSocketMessage {
    let event_id = gen_event_id();
    let item = json!({
        "id": item_id,
        "object": "realtime.item",
        "type": "message",
        "status": status,
        "role": role,
        "content": json!([]),
    });
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ConversationItemCreated { event_id, previous_item_id: previous_item_id.map(|s| s.to_string()), item }),
    )
}

/// 包装后的response.output_item.added消息
pub fn create_response_output_item_added_message(session_id: String, response_id: &str, output_index: u32, item_id: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    let item = json!({
        "id": item_id,
        "object": "realtime.item",
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": json!([]),
    });
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ResponseOutputItemAdded { event_id, response_id: response_id.to_string(), output_index, item }),
    )
}

/// 包装后的response.done消息
pub fn create_response_done_message(session_id: String, response_id: &str, output_items: Vec<serde_json::Value>, usage: Option<serde_json::Value>) -> WebSocketMessage {
    let event_id = gen_event_id();
    let response = json!({
        "id": response_id,
        "object": "realtime.response",
        "status": "completed",
        "status_details": Value::Null,
        "output": output_items,
        "usage": usage.unwrap_or(Value::Null),
    });
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ResponseDone { event_id, response }),
    )
}

/// 包装后的response.function_call_arguments.delta消息
pub fn create_response_function_call_delta_message(session_id: String, response_id: &str, item_id: &str, call_id: &str, delta: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ResponseFunctionCallDelta {
            event_id,
            response_id: response_id.to_string(),
            item_id: item_id.to_string(),
            call_id: call_id.to_string(),
            delta: delta.to_string(),
        }),
    )
}

/// 包装后的response.function_call_arguments.done消息
pub fn create_response_function_call_done_message(session_id: String, response_id: &str, item_id: &str, call_id: &str, arguments: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ResponseFunctionCallDone {
            event_id,
            response_id: response_id.to_string(),
            item_id: item_id.to_string(),
            call_id: call_id.to_string(),
            arguments: arguments.to_string(),
        }),
    )
}

/// 包装后的response.function_call_result.delta消息
pub fn create_response_function_call_result_delta_message(session_id: String, response_id: &str, item_id: &str, call_id: &str, delta: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ResponseFunctionCallResultDelta {
            event_id,
            response_id: response_id.to_string(),
            item_id: item_id.to_string(),
            call_id: call_id.to_string(),
            delta: delta.to_string(),
        }),
    )
}

/// 包装后的response.function_call_result.done消息
pub fn create_response_function_call_result_done_message(session_id: String, response_id: &str, item_id: &str, call_id: &str, result: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ResponseFunctionCallResultDone {
            event_id,
            response_id: response_id.to_string(),
            item_id: item_id.to_string(),
            call_id: call_id.to_string(),
            result: result.to_string(),
        }),
    )
}

/// 包装后的conversation.item.updated消息
pub fn create_conversation_item_updated_message(session_id: String, item_id: &str, role: &str, status: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    let item = json!({
        "id": item_id,
        "object": "realtime.item",
        "type": "message",
        "status": status,
        "role": role,
        "content": json!([]),
    });
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ConversationItemUpdated { event_id, item }),
    )
}

/// 包装后的response.output_item.done消息
pub fn create_response_output_item_done_message(session_id: String, response_id: &str, output_index: u32, item_id: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    let item = json!({
        "id": item_id,
        "object": "realtime.item",
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": json!([]),
    });
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ResponseOutputItemDone { event_id, response_id: response_id.to_string(), output_index, item }),
    )
}

pub fn create_function_call_arguments_delta_message(session_id: String, response_id: &str, item_id: &str, call_id: &str, delta: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ResponseFunctionCallArgumentsDelta {
            event_id,
            response_id: response_id.to_string(),
            item_id: item_id.to_string(),
            call_id: call_id.to_string(),
            delta: delta.to_string(),
        }),
    )
}

/// 包装后的response.function_call_arguments.done消息
pub fn create_function_call_arguments_done_message(session_id: String, response_id: &str, item_id: &str, call_id: &str, function_name: &str, arguments: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::ResponseFunctionCallArgumentsDone {
            event_id,
            response_id: response_id.to_string(),
            item_id: item_id.to_string(),
            call_id: call_id.to_string(),
            function_name: function_name.to_string(),
            arguments: arguments.to_string(),
        }),
    )
}

/// 创建服务器内置工具的结果消息（使用 function_call_output 类型）
/// 适用于：服务器内置的工具函数调用结果，如计算器、天气查询等
pub fn create_server_tool_result_message(session_id: String, response_id: &str, item_id: &str, call_id: &str, result: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    let _item_id = format!("item_{}", nanoid!(8));
    let _item = json!({
        "id": item_id,
        "object": "realtime.item",
        "type": "function_call_output",
        "status": "completed",
        "call_id": call_id,
        "result": result,
    });
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::TextData, // 客户端发送的消息使用 TextData 命令
        session_id,
        Some(MessagePayload::ResponseFunctionCallResultDone {
            event_id,
            response_id: response_id.to_string(),
            item_id: item_id.to_string(),
            call_id: call_id.to_string(),
            result: result.to_string(),
        }),
    )
}

/// 创建 session.update 消息
pub fn create_session_update_message(session_id: String, session_config: serde_json::Value) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        session_id,
        Some(MessagePayload::SessionUpdate { event_id, session: session_config }),
    )
}

/// 创建 response.cancel 消息
pub fn create_response_cancel_message(session_id: String, response_id: &str) -> WebSocketMessage {
    let event_id = gen_event_id();
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Stop, // 使用 Stop 命令表示取消
        session_id,
        Some(MessagePayload::ResponseCancel { event_id, response_id: response_id.to_string() }),
    )
}

// ===== 为了向后兼容，保留旧的函数名作为别名 =====

/// @deprecated 使用 create_session_created_message 替代
pub fn wrapped_session_created(protocol_id: ProtocolId, session_id: String, conv_id: &str) -> WebSocketMessage {
    create_session_created_message(protocol_id, session_id, conv_id)
}

/// @deprecated 使用 create_input_audio_speech_started_message 替代
pub fn wrapped_input_audio_speech_started(session_id: String, audio_start_ms: u32, item_id: &str) -> WebSocketMessage {
    create_input_audio_speech_started_message(session_id, audio_start_ms, item_id)
}

pub fn wrapped_input_audio_speech_stopped(session_id: String, audio_end_ms: u32, item_id: &str) -> WebSocketMessage {
    create_input_audio_speech_stopped_message(session_id, audio_end_ms, item_id)
}

/// @deprecated 使用 create_asr_transcription_delta_message 替代
pub fn wrapped_asr_transcription_delta(session_id: String, item_id: &str, content_index: u32, delta: &str) -> WebSocketMessage {
    create_asr_transcription_delta_message(session_id, item_id, content_index, delta)
}

/// @deprecated 使用 create_asr_transcription_completed_message 替代
pub fn wrapped_asr_transcription_completed(session_id: String, item_id: &str, content_index: u32, transcript: &str) -> WebSocketMessage {
    create_asr_transcription_completed_message(session_id, item_id, content_index, transcript)
}

/// @deprecated 使用 create_conversation_item_truncated_message 替代
pub fn wrapped_conversation_item_truncated(session_id: String, item_id: &str, content_index: u32, audio_end_ms: u32) -> WebSocketMessage {
    create_conversation_item_truncated_message(session_id, item_id, content_index, audio_end_ms)
}

/// @deprecated 使用 create_response_text_delta_message 替代
pub fn wrapped_response_text_delta(session_id: String, resp_id: &str, item_id: &str, output_index: u32, content_index: u32, delta: &str) -> WebSocketMessage {
    create_response_text_delta_message(session_id, resp_id, item_id, output_index, content_index, delta)
}

/// @deprecated 使用 create_response_text_done_message 替代
pub fn wrapped_response_text_done(session_id: String, resp_id: &str, item_id: &str, output_index: u32, content_index: u32, text: &str) -> WebSocketMessage {
    create_response_text_done_message(session_id, resp_id, item_id, output_index, content_index, text)
}

/// @deprecated 使用 create_response_audio_delta_message 替代
pub fn wrapped_response_audio_delta(session_id: String, resp_id: &str, item_id: &str, output_index: u32, content_index: u32, delta_b64: &str) -> WebSocketMessage {
    create_response_audio_delta_message(session_id, resp_id, item_id, output_index, content_index, delta_b64)
}

/// @deprecated 使用 create_response_audio_done_message 替代
pub fn wrapped_response_audio_done(session_id: String, resp_id: &str, item_id: &str, output_index: u32, content_index: u32) -> WebSocketMessage {
    create_response_audio_done_message(session_id, resp_id, item_id, output_index, content_index)
}

/// @deprecated 使用 create_output_audio_buffer_started_message 替代
pub fn wrapped_output_audio_buffer_started(session_id: String, response_id: &str) -> WebSocketMessage {
    create_output_audio_buffer_started_message(session_id, response_id)
}

/// @deprecated 使用 create_output_audio_buffer_stopped_message 替代
pub fn wrapped_output_audio_buffer_stopped(session_id: String, response_id: &str) -> WebSocketMessage {
    create_output_audio_buffer_stopped_message(session_id, response_id)
}

/// @deprecated 使用 create_output_audio_buffer_cleared_message 替代
pub fn wrapped_output_audio_buffer_cleared(session_id: String, response_id: &str) -> WebSocketMessage {
    create_output_audio_buffer_cleared_message(session_id, response_id)
}

/// @deprecated 使用 create_error_event_message 替代
pub fn wrapped_error_event(session_id: String, code: u16, message: &str) -> WebSocketMessage {
    create_error_event_message(session_id, code, message)
}

/// @deprecated 使用 create_asr_transcription_failed_message 替代
pub fn wrapped_asr_transcription_failed(session_id: String, item_id: &str, content_index: u32, code: &str, message: &str) -> WebSocketMessage {
    create_asr_transcription_failed_message(session_id, item_id, content_index, code, message)
}

/// @deprecated 使用 create_response_created_message 替代
pub fn wrapped_response_created(session_id: String, response_id: &str) -> WebSocketMessage {
    create_response_created_message(session_id, response_id)
}

/// @deprecated 使用 create_conversation_item_created_message 替代
pub fn wrapped_conversation_item_created(session_id: String, item_id: &str, role: &str, status: &str, previous_item_id: Option<&str>) -> WebSocketMessage {
    create_conversation_item_created_message(session_id, item_id, role, status, previous_item_id)
}

/// @deprecated 使用 create_response_output_item_added_message 替代
pub fn wrapped_response_output_item_added(session_id: String, response_id: &str, output_index: u32, item_id: &str) -> WebSocketMessage {
    create_response_output_item_added_message(session_id, response_id, output_index, item_id)
}

/// @deprecated 使用 create_response_done_message 替代
pub fn wrapped_response_done(session_id: String, response_id: &str, output_items: Vec<serde_json::Value>, usage: Option<serde_json::Value>) -> WebSocketMessage {
    create_response_done_message(session_id, response_id, output_items, usage)
}

/// @deprecated 使用 create_response_function_call_delta_message 替代
pub fn wrapped_response_function_call_delta(session_id: String, response_id: &str, item_id: &str, call_id: &str, delta: &str) -> WebSocketMessage {
    create_response_function_call_delta_message(session_id, response_id, item_id, call_id, delta)
}

/// @deprecated 使用 create_response_function_call_done_message 替代
pub fn wrapped_response_function_call_done(session_id: String, response_id: &str, item_id: &str, call_id: &str, arguments: &str) -> WebSocketMessage {
    create_response_function_call_done_message(session_id, response_id, item_id, call_id, arguments)
}

/// @deprecated 使用 create_response_function_call_result_delta_message 替代
pub fn wrapped_response_function_call_result_delta(session_id: String, response_id: &str, item_id: &str, call_id: &str, delta: &str) -> WebSocketMessage {
    create_response_function_call_result_delta_message(session_id, response_id, item_id, call_id, delta)
}

/// @deprecated 使用 create_response_function_call_result_done_message 替代
pub fn wrapped_response_function_call_result_done(session_id: String, response_id: &str, item_id: &str, call_id: &str, result: &str) -> WebSocketMessage {
    create_response_function_call_result_done_message(session_id, response_id, item_id, call_id, result)
}

/// @deprecated 使用 create_conversation_item_updated_message 替代
pub fn wrapped_conversation_item_updated(session_id: String, item_id: &str, role: &str, status: &str) -> WebSocketMessage {
    create_conversation_item_updated_message(session_id, item_id, role, status)
}

/// @deprecated 使用 create_response_output_item_done_message 替代
pub fn wrapped_response_output_item_done(session_id: String, response_id: &str, output_index: u32, item_id: &str) -> WebSocketMessage {
    create_response_output_item_done_message(session_id, response_id, output_index, item_id)
}

/// 包装后的response.function_call_arguments.delta消息 (OpenAI标准)
pub fn wrapped_response_function_call_arguments_delta(session_id: String, response_id: &str, item_id: &str, call_id: &str, delta: &str) -> WebSocketMessage {
    create_function_call_arguments_delta_message(session_id, response_id, item_id, call_id, delta)
}

/// 包装后的response.function_call_arguments.done消息 (OpenAI标准)
pub fn wrapped_response_function_call_arguments_done(session_id: String, response_id: &str, item_id: &str, call_id: &str, function_name: &str, arguments: &str) -> WebSocketMessage {
    create_function_call_arguments_done_message(session_id, response_id, item_id, call_id, function_name, arguments)
}

/// 包装后的 response.function_call_output 消息 (用于服务器内置工具结果)
pub fn wrapped_create_server_tool_result_message(session_id: String, response_id: &str, item_id: &str, call_id: &str, result: &str) -> WebSocketMessage {
    create_server_tool_result_message(session_id, response_id, item_id, call_id, result)
}

/// 包装后的 session.update 消息
pub fn wrapped_session_update(session_id: String, session_config: serde_json::Value) -> WebSocketMessage {
    create_session_update_message(session_id, session_config)
}

/// 包装后的 response.cancel 消息
pub fn wrapped_response_cancel(session_id: String, response_id: &str) -> WebSocketMessage {
    create_response_cancel_message(session_id, response_id)
}
