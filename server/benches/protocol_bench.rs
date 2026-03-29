//! Benchmarks for WebSocket protocol message serialization/deserialization.
//!
//! Tests JSON and binary encoding/decoding for various message types and sizes.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use realtime::rpc::protocol::{BinaryHeader, BinaryMessage, CommandId, MessagePayload, ProtocolId, WebSocketMessage};

fn make_session_id() -> String {
    "1234567890123456".to_string() // 16 chars, valid nanoid size
}

/// Construct a realistic session config message from JSON (avoids listing all 30+ fields).
fn make_start_message_json() -> String {
    r#"{"protocol_id":100,"command_id":1,"session_id":"1234567890123456","payload":{"type":"session_config","mode":"vad","vad_threshold":0.55,"silence_duration_ms":200,"system_prompt":"You are a helpful assistant.","asr_language":"zh"}}"#.to_string()
}

fn make_text_delta_message() -> WebSocketMessage {
    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::Result,
        make_session_id(),
        Some(MessagePayload::ResponseTextDelta {
            event_id: "evt_001".to_string(),
            response_id: "resp_001".to_string(),
            item_id: "item_001".to_string(),
            output_index: 0,
            content_index: 0,
            delta: "Hello, how can I help you today?".to_string(),
        }),
    )
}

fn make_audio_chunk_message(audio_size: usize) -> WebSocketMessage {
    use base64::{Engine as _, engine::general_purpose};
    let audio_data = vec![0u8; audio_size];
    let encoded = general_purpose::STANDARD.encode(&audio_data);

    WebSocketMessage::new(
        ProtocolId::All,
        CommandId::AudioChunk,
        make_session_id(),
        Some(MessagePayload::AudioChunk { data: encoded, sample_rate: 16000, channels: 1 }),
    )
}

fn bench_json_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("protocol/json/serialize");

    // Session config: deserialize from JSON, then benchmark serialization
    let start_msg = WebSocketMessage::from_json(&make_start_message_json()).unwrap();
    group.bench_function("session_config", |b| {
        b.iter(|| black_box(start_msg.to_json().unwrap()));
    });

    let text_msg = make_text_delta_message();
    group.bench_function("text_delta", |b| {
        b.iter(|| black_box(text_msg.to_json().unwrap()));
    });

    // Small audio chunk (20ms @16kHz mono 16-bit = 640 bytes)
    let audio_small = make_audio_chunk_message(640);
    group.bench_function("audio_chunk_640B", |b| {
        b.iter(|| black_box(audio_small.to_json().unwrap()));
    });

    // Large audio chunk (1 second = 32000 bytes)
    let audio_large = make_audio_chunk_message(32000);
    group.bench_function("audio_chunk_32KB", |b| {
        b.iter(|| black_box(audio_large.to_json().unwrap()));
    });

    group.finish();
}

fn bench_json_deserialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("protocol/json/deserialize");

    let start_json = make_start_message_json();
    group.bench_function("session_config", |b| {
        b.iter(|| black_box(WebSocketMessage::from_json(&start_json).unwrap()));
    });

    let text_json = make_text_delta_message().to_json().unwrap();
    group.bench_function("text_delta", |b| {
        b.iter(|| black_box(WebSocketMessage::from_json(&text_json).unwrap()));
    });

    let audio_json = make_audio_chunk_message(640).to_json().unwrap();
    group.bench_function("audio_chunk_640B", |b| {
        b.iter(|| black_box(WebSocketMessage::from_json(&audio_json).unwrap()));
    });

    let audio_large_json = make_audio_chunk_message(32000).to_json().unwrap();
    group.bench_function("audio_chunk_32KB", |b| {
        b.iter(|| black_box(WebSocketMessage::from_json(&audio_large_json).unwrap()));
    });

    // Safe deserialization (with fallback)
    group.bench_function("session_config_safe", |b| {
        b.iter(|| black_box(WebSocketMessage::from_json_safe(&start_json).unwrap()));
    });

    group.finish();
}

fn bench_binary_header(c: &mut Criterion) {
    let mut group = c.benchmark_group("protocol/binary/header");

    let header = BinaryHeader::new(make_session_id(), ProtocolId::All, CommandId::AudioChunk).unwrap();

    group.bench_function("to_bytes", |b| {
        b.iter(|| black_box(header.to_bytes().unwrap()));
    });

    let header_bytes = header.to_bytes().unwrap();
    group.bench_function("from_bytes", |b| {
        b.iter(|| black_box(BinaryHeader::from_bytes(&header_bytes).unwrap()));
    });

    group.finish();
}

fn bench_binary_message(c: &mut Criterion) {
    let mut group = c.benchmark_group("protocol/binary/message");

    // Small audio payload (20ms frame)
    let header = BinaryHeader::new(make_session_id(), ProtocolId::All, CommandId::AudioChunk).unwrap();
    let small_payload = vec![0u8; 640];
    let small_msg = BinaryMessage::new(header.clone(), small_payload).unwrap();

    group.bench_function("to_bytes_640B", |b| {
        b.iter(|| black_box(small_msg.to_bytes().unwrap()));
    });

    let small_bytes = small_msg.to_bytes().unwrap();
    group.bench_function("from_bytes_640B", |b| {
        b.iter(|| black_box(BinaryMessage::from_bytes(&small_bytes).unwrap()));
    });

    // Large audio payload (1 second)
    let large_payload = vec![0u8; 32000];
    let large_msg = BinaryMessage::new(header, large_payload).unwrap();

    group.bench_function("to_bytes_32KB", |b| {
        b.iter(|| black_box(large_msg.to_bytes().unwrap()));
    });

    let large_bytes = large_msg.to_bytes().unwrap();
    group.bench_function("from_bytes_32KB", |b| {
        b.iter(|| black_box(BinaryMessage::from_bytes(&large_bytes).unwrap()));
    });

    group.finish();
}

fn bench_json_to_binary_conversion(c: &mut Criterion) {
    let mut group = c.benchmark_group("protocol/conversion");

    let audio_msg = make_audio_chunk_message(640);
    group.bench_function("json_to_binary_640B", |b| {
        b.iter(|| black_box(audio_msg.to_binary().unwrap()));
    });

    let audio_large = make_audio_chunk_message(32000);
    group.bench_function("json_to_binary_32KB", |b| {
        b.iter(|| black_box(audio_large.to_binary().unwrap()));
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_json_serialization,
    bench_json_deserialization,
    bench_binary_header,
    bench_binary_message,
    bench_json_to_binary_conversion,
);
criterion_main!(benches);
