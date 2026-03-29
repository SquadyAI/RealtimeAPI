//! Benchmarks for Silero VAD frame processing.
//!
//! Measures ONNX inference latency for 512-sample frames (32ms @16kHz).

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use ndarray::Array1;
use realtime::vad::VADEngine;

/// Benchmark VADIterator::process_chunk — the hot path in the audio pipeline.
/// Uses block_on since the VAD model's futures are not Send.
fn bench_vad_process_chunk(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let engine = VADEngine::new().expect("Failed to create VAD engine");
    let mut vad = engine.create_vad_iterator(0.55, 200, 80, 8000).unwrap();

    // Silence frame (512 zeros)
    let silence = Array1::<f32>::zeros(512);
    // Synthetic speech-like frame
    let speech: Array1<f32> = Array1::from_vec((0..512).map(|i| (i as f32 * 0.05).sin() * 0.5).collect());

    let mut group = c.benchmark_group("vad");
    group.sample_size(200);

    group.bench_function("process_chunk_silence_512", |b| {
        b.iter(|| rt.block_on(async { black_box(vad.process_chunk(&silence.view()).await.unwrap()) }));
    });

    group.bench_function("process_chunk_speech_512", |b| {
        b.iter(|| rt.block_on(async { black_box(vad.process_chunk(&speech.view()).await.unwrap()) }));
    });

    group.finish();
}

/// Benchmark VADIterator::reset overhead.
fn bench_vad_reset(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let engine = VADEngine::new().expect("Failed to create VAD engine");
    let mut vad = engine.create_vad_iterator(0.55, 200, 80, 8000).unwrap();

    c.bench_function("vad/reset", |b| {
        b.iter(|| rt.block_on(async { black_box(vad.reset().await) }));
    });
}

criterion_group!(benches, bench_vad_process_chunk, bench_vad_reset);
criterion_main!(benches);
