//! Benchmarks for paced audio sender: scheduling precision and jitter measurement.
//!
//! Tests Welford's online variance algorithm, pacing calculations, and audio duration computation.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use realtime::rpc::pipeline::paced_sender::pacing_algorithm;
use realtime::rpc::pipeline::paced_sender::timing_stats::TimingStats;
use std::time::{Duration, Instant};

fn bench_timing_stats(c: &mut Criterion) {
    let mut group = c.benchmark_group("timing_stats");

    // Welford's algorithm: single update
    group.bench_function("update_processing_delay/single", |b| {
        let mut stats = TimingStats::new();
        b.iter(|| {
            stats.update_processing_delay(black_box(150));
        });
    });

    // Welford's algorithm: window full (10 samples) then steady state
    group.bench_function("update_processing_delay/steady_state", |b| {
        let mut stats = TimingStats::new();
        // Fill the window first
        for i in 0..10 {
            stats.update_processing_delay(100 + i * 10);
        }
        b.iter(|| {
            stats.update_processing_delay(black_box(150));
        });
    });

    // Cumulative error tracking
    group.bench_function("update_cumulative_error", |b| {
        let mut stats = TimingStats::new();
        let base_time = Instant::now();
        b.iter(|| {
            let planned = base_time;
            let actual = base_time + Duration::from_micros(50);
            stats.update_cumulative_error(black_box(planned), black_box(actual));
        });
    });

    // Stability report generation
    group.bench_function("get_stability_report", |b| {
        let mut stats = TimingStats::new();
        for i in 0..10 {
            stats.update_processing_delay(100 + i * 10);
        }
        b.iter(|| {
            black_box(stats.get_stability_report());
        });
    });

    // Combined: full cycle (delay update + error update)
    group.bench_function("full_cycle", |b| {
        let mut stats = TimingStats::new();
        for i in 0..10 {
            stats.update_processing_delay(100 + i * 10);
        }
        let base_time = Instant::now();
        b.iter(|| {
            stats.update_processing_delay(black_box(120));
            let planned = base_time;
            let actual = base_time + Duration::from_micros(30);
            stats.update_cumulative_error(black_box(planned), black_box(actual));
        });
    });

    group.finish();
}

fn bench_pacing_calculations(c: &mut Criterion) {
    let mut group = c.benchmark_group("pacing_algorithm");

    // Poll interval calculation
    group.bench_function("poll_interval_ms/1x", |b| {
        b.iter(|| black_box(pacing_algorithm::poll_interval_ms(20, 1.0)));
    });
    group.bench_function("poll_interval_ms/1.5x", |b| {
        b.iter(|| black_box(pacing_algorithm::poll_interval_ms(20, 1.5)));
    });
    group.bench_function("poll_interval_ms/2x", |b| {
        b.iter(|| black_box(pacing_algorithm::poll_interval_ms(20, 2.0)));
    });

    // Audio duration calculation: 20ms PCM frame @16kHz mono 16-bit = 640 bytes
    group.bench_function("audio_duration_us/640B_16kHz_mono", |b| {
        b.iter(|| {
            black_box(pacing_algorithm::audio_duration_us(640, 2, 1, 16000));
        });
    });
    // 20ms @24kHz mono 16-bit = 960 bytes
    group.bench_function("audio_duration_us/960B_24kHz_mono", |b| {
        b.iter(|| {
            black_box(pacing_algorithm::audio_duration_us(960, 2, 1, 24000));
        });
    });
    // Large buffer: 1 second @16kHz stereo 16-bit = 64000 bytes
    group.bench_function("audio_duration_us/64000B_16kHz_stereo", |b| {
        b.iter(|| {
            black_box(pacing_algorithm::audio_duration_us(64000, 2, 2, 16000));
        });
    });

    // Send delay: during burst phase
    group.bench_function("send_delay_us/burst_phase", |b| {
        b.iter(|| {
            black_box(pacing_algorithm::send_delay_us(0, 3, 10, 20, 1.0));
        });
    });
    // Send delay: steady state
    group.bench_function("send_delay_us/steady_state", |b| {
        b.iter(|| {
            black_box(pacing_algorithm::send_delay_us(10, 3, 10, 20, 1.0));
        });
    });
    // Send delay: accelerated (2x)
    group.bench_function("send_delay_us/steady_2x", |b| {
        b.iter(|| {
            black_box(pacing_algorithm::send_delay_us(10, 3, 10, 20, 2.0));
        });
    });

    group.finish();
}

criterion_group!(benches, bench_timing_stats, bench_pacing_calculations);
criterion_main!(benches);
