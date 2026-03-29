use anyhow::{Result, bail};
use ndarray::{Array2, Array3, Axis, s};
use once_cell::sync::Lazy;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub const SAMPLE_RATE: usize = 16_000;
const N_FFT: usize = 400;
const HOP_LENGTH: usize = 160;
const N_MELS: usize = 80;
const MEL_FLOOR: f32 = 1e-10;
const MAX_FREQUENCY: f32 = 8000.0;
const FREQ_BINS: usize = N_FFT / 2 + 1;

/// SmartTurn 模型要求固定 8 秒输入
const TARGET_DURATION_SECONDS: usize = 8;
const TARGET_SAMPLES: usize = TARGET_DURATION_SECONDS * SAMPLE_RATE; // 128000

static WINDOW: Lazy<Vec<f32>> = Lazy::new(|| hann_window(N_FFT));
static MEL_FILTERS: Lazy<Array2<f32>> = Lazy::new(build_mel_filters);

// 使用 RealFFT 替代复数FFT - 实数输入专用，约1.5-2倍性能提升
static REAL_FFT: Lazy<Arc<dyn RealToComplex<f32>>> = Lazy::new(|| {
    let mut planner = RealFftPlanner::<f32>::new();
    planner.plan_fft_forward(N_FFT)
});

/// 将音频截断或 padding 到固定 8 秒
/// - 如果 > 8秒：取最后 8 秒
/// - 如果 < 8秒：在开头 pad 零
fn truncate_or_pad_audio(audio: &[f32]) -> Vec<f32> {
    if audio.len() > TARGET_SAMPLES {
        // 取最后 8 秒
        audio[audio.len() - TARGET_SAMPLES..].to_vec()
    } else if audio.len() < TARGET_SAMPLES {
        // 在开头 pad 零
        let mut padded = vec![0.0f32; TARGET_SAMPLES - audio.len()];
        padded.extend_from_slice(audio);
        padded
    } else {
        audio.to_vec()
    }
}

pub fn log_mel_spectrogram(audio: &[f32]) -> Result<Array3<f32>> {
    // SmartTurn 模型要求固定 8 秒输入
    let audio = truncate_or_pad_audio(audio);

    let padded = reflect_pad(&audio, N_FFT / 2)?;
    let power_spec = stft_power(&padded);
    let mel_spec = MEL_FILTERS.dot(&power_spec);
    if mel_spec.len_of(Axis(1)) < 2 {
        bail!("Not enough frames to match Whisper's expectations");
    }
    let mut mel_spec = mel_spec.slice(s![.., ..-1]).to_owned();
    apply_dynamic_range(&mut mel_spec);
    Ok(mel_spec.insert_axis(Axis(0)))
}

fn stft_power(padded_audio: &[f32]) -> Array2<f32> {
    let num_frames = 1 + (padded_audio.len() - N_FFT) / HOP_LENGTH;
    let mut spec = Array2::<f32>::zeros((FREQ_BINS, num_frames));

    let fft = REAL_FFT.as_ref();
    // RealFFT 缓冲区：输入为实数，输出为 N/2+1 个复数
    let mut input_buffer = fft.make_input_vec();
    let mut output_buffer = fft.make_output_vec();

    for frame_idx in 0..num_frames {
        let offset = frame_idx * HOP_LENGTH;

        // 应用窗函数到输入缓冲区
        apply_window(&padded_audio[offset..], &mut input_buffer);

        // RealFFT: 只计算正频率部分，比复数FFT快约2倍
        fft.process(&mut input_buffer, &mut output_buffer).unwrap();

        // 计算功率谱并写入结果
        for (bin, complex) in output_buffer.iter().enumerate() {
            spec[(bin, frame_idx)] = complex.norm_sqr();
        }
    }

    spec
}

/// 窗函数应用 - 固定大小循环，帮助编译器向量化
#[inline]
fn apply_window(input: &[f32], output: &mut [f32]) {
    // 编译器更容易对固定大小循环进行SIMD优化
    for i in 0..N_FFT {
        output[i] = input[i] * WINDOW[i];
    }
}

/// 动态范围压缩 - 使用 f32::max 减少分支，利于SIMD
fn apply_dynamic_range(spec: &mut Array2<f32>) {
    // 第一遍: log变换 + 找最大值
    let mut max_val = f32::NEG_INFINITY;
    for value in spec.iter_mut() {
        let logged = value.max(MEL_FLOOR).log10();
        *value = logged;
        max_val = max_val.max(logged);
    }

    // 第二遍: 动态范围压缩（使用 f32::max 替代 if-else，更易向量化）
    let floor = max_val - 8.0;
    for value in spec.iter_mut() {
        *value = (value.max(floor) + 4.0) * 0.25;
    }
}

fn hann_window(length: usize) -> Vec<f32> {
    (0..length)
        .map(|n| (std::f32::consts::PI * n as f32 / (length - 1) as f32).sin().powi(2))
        .collect()
}

fn reflect_pad(signal: &[f32], pad: usize) -> Result<Vec<f32>> {
    if pad == 0 {
        return Ok(signal.to_vec());
    }
    if signal.len() <= pad {
        bail!("Signal must be longer than pad length");
    }

    let mut padded = Vec::with_capacity(signal.len() + 2 * pad);
    padded.extend(signal[1..=pad].iter().rev().copied());
    padded.extend_from_slice(signal);
    let tail_start = signal.len() - pad - 1;
    let tail_end = signal.len() - 1;
    padded.extend(signal[tail_start..tail_end].iter().rev().copied());
    Ok(padded)
}

fn build_mel_filters() -> Array2<f32> {
    let fft_freqs = linspace(0.0, (SAMPLE_RATE as f32) / 2.0, FREQ_BINS);
    let mel_points = linspace(0.0, hz_to_mel(MAX_FREQUENCY), N_MELS + 2);
    let filter_freqs: Vec<f32> = mel_points.iter().map(|&mel| mel_to_hz(mel)).collect();

    let mut filters = Array2::<f32>::zeros((N_MELS, FREQ_BINS));
    for mel_index in 0..N_MELS {
        let left = filter_freqs[mel_index];
        let center = filter_freqs[mel_index + 1];
        let right = filter_freqs[mel_index + 2];

        for (bin, &freq) in fft_freqs.iter().enumerate() {
            let weight = if freq >= left && freq <= center {
                (freq - left) / (center - left)
            } else if freq >= center && freq <= right {
                (right - freq) / (right - center)
            } else {
                0.0
            };
            filters[(mel_index, bin)] = weight.max(0.0);
        }
    }

    // 归一化
    for m in 0..N_MELS {
        let enorm = 2.0 / (filter_freqs[m + 2] - filter_freqs[m]);
        for bin in 0..FREQ_BINS {
            filters[(m, bin)] *= enorm;
        }
    }

    filters
}

fn linspace(start: f32, end: f32, points: usize) -> Vec<f32> {
    if points < 2 {
        return vec![start];
    }
    let step = (end - start) / (points as f32 - 1.0);
    (0..points).map(|i| start + i as f32 * step).collect()
}

fn hz_to_mel(freq: f32) -> f32 {
    const F_SP: f32 = 200.0 / 3.0;
    const MIN_LOG_HZ: f32 = 1000.0;
    const MIN_LOG_MEL: f32 = MIN_LOG_HZ / F_SP;
    let log_step = 6.4f32.ln() / 27.0;

    if freq < MIN_LOG_HZ {
        freq / F_SP
    } else {
        MIN_LOG_MEL + (freq / MIN_LOG_HZ).ln() / log_step
    }
}

fn mel_to_hz(mel: f32) -> f32 {
    const F_SP: f32 = 200.0 / 3.0;
    const MIN_LOG_HZ: f32 = 1000.0;
    const MIN_LOG_MEL: f32 = MIN_LOG_HZ / F_SP;
    let log_step = 6.4f32.ln() / 27.0;

    if mel < MIN_LOG_MEL {
        mel * F_SP
    } else {
        MIN_LOG_HZ * f32::exp(log_step * (mel - MIN_LOG_MEL))
    }
}
