//! GTCRN频域降噪器 独立可执行程序
//!
//! 用法: denoise_wav.exe <input.wav> <output.wav>
//!
//! 使用正确的频域GTCRN流式推理（256样本/帧，STFT处理）

use anyhow::{Context, Result, bail};
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use std::env;
use std::path::Path;
use tracing::{error, info, warn};

use realtime::audio::denoiser::GtcrnFrequencyDenoiser;

// GTCRN 流式处理参数
const FRAME_SIZE: usize = 512; // 输入帧大小（将被降噪器内部处理为256样本/hop）
const TARGET_SR: u32 = 16000;

/// 简单的重采样函数（线性插值）- 与线上处理一致
fn resample_audio(input: &[f32], input_rate: u32, output_rate: u32) -> Vec<f32> {
    if input_rate == output_rate {
        return input.to_vec();
    }
    let ratio = input_rate as f32 / output_rate as f32;
    let output_len = (input.len() as f32 / ratio).ceil() as usize;
    let mut output = Vec::with_capacity(output_len);
    for i in 0..output_len {
        let input_pos = i as f32 * ratio;
        let idx = input_pos.floor() as usize;
        let next = (idx + 1).min(input.len().saturating_sub(1));
        let frac = input_pos - idx as f32;
        let sample = if idx < input.len().saturating_sub(1) {
            input[idx] * (1.0 - frac) + input[next] * frac
        } else {
            input[idx]
        };
        output.push(sample);
    }
    output
}

/// 加载WAV文件并转换为线上处理所需的格式
fn load_wav_16k_mono_f32(path: &str) -> Result<(Vec<f32>, u32)> {
    if !Path::new(path).exists() {
        bail!("输入文件不存在: {}", path);
    }

    let mut reader = WavReader::open(path).with_context(|| format!("无法读取WAV: {}", path))?;
    let spec = reader.spec();

    info!(
        "读取WAV: sr={}Hz, ch={}, bits={}, fmt={:?}",
        spec.sample_rate, spec.channels, spec.bits_per_sample, spec.sample_format
    );

    // 读取样本并转换为f32归一化格式
    let audio: Vec<f32> = match spec.sample_format {
        SampleFormat::Float => reader.samples::<f32>().collect::<std::result::Result<Vec<_>, _>>()?,
        SampleFormat::Int => {
            let ints = reader.samples::<i32>().collect::<std::result::Result<Vec<_>, _>>()?;
            let max_val = match spec.bits_per_sample {
                16 => 32768.0,
                24 => 8388608.0,
                32 => 2147483648.0,
                _ => 2147483648.0,
            };
            ints.into_iter().map(|s| (s as f32) / max_val).collect()
        },
    };

    // 多声道转单声道
    let mut audio = if spec.channels > 1 {
        let mut mono = Vec::with_capacity(audio.len() / spec.channels as usize + 1);
        for frame in audio.chunks(spec.channels as usize) {
            let sum: f32 = frame.iter().copied().sum();
            mono.push(sum / frame.len() as f32);
        }
        mono
    } else {
        audio
    };

    // 重采样到16kHz（如果需要）
    let mut sr = spec.sample_rate;
    if sr != TARGET_SR {
        warn!("采样率为 {}Hz，将重采样至 {}Hz", sr, TARGET_SR);
        audio = resample_audio(&audio, sr, TARGET_SR);
        sr = TARGET_SR;
    }

    // 峰值归一化到<=1.0（安全措施）
    let peak = audio.iter().map(|x| x.abs()).fold(0.0_f32, f32::max);
    if peak > 1.0 {
        for s in &mut audio {
            *s /= peak;
        }
    }

    Ok((audio, sr))
}

/// 保存f32音频数据为WAV文件
fn write_wav_f32_mono_16k(path: &str, audio: &[f32]) -> Result<()> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: TARGET_SR,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };

    let mut writer = WavWriter::create(path, spec).with_context(|| format!("无法创建输出WAV: {}", path))?;

    for &s in audio {
        writer.write_sample(s)?;
    }

    writer.finalize().with_context(|| format!("无法完成WAV文件写入: {}", path))?;

    Ok(())
}

fn main() -> Result<()> {
    // 初始化简单的日志输出
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();

    let args: Vec<String> = env::args().collect();

    if args.len() != 3 {
        eprintln!("用法: denoise_wav.exe <input.wav> <output.wav>");
        eprintln!("示例: denoise_wav.exe audio_resp_1762152506777293830.wav den.wav");
        std::process::exit(1);
    }

    let input_file = &args[1];
    let output_file = &args[2];

    info!("开始处理: {} -> {}", input_file, output_file);

    // 1. 加载并预处理音频（与线上处理完全一致）
    let (audio, sr) = load_wav_16k_mono_f32(input_file)?;
    if sr != TARGET_SR {
        warn!("内部处理采样率应为16k，当前: {}", sr);
    }

    info!("音频信息: {} 样本, {:.2}秒", audio.len(), audio.len() as f32 / TARGET_SR as f32);

    // 2. 创建频域GTCRN降噪器
    let mut denoiser = GtcrnFrequencyDenoiser::new().context("无法创建GtcrnFrequencyDenoiser")?;

    info!("降噪器配置:");
    info!("  - 类型: GTCRN频域流式降噪");
    info!("  - STFT参数: n_fft=512, hop_length=256");
    info!("  - 输入: 任意长度音频帧");
    info!("  - 输出: 256样本/hop（内部自动处理）");

    // 3. 流式处理音频（正确的GTCRN推理）
    info!("开始降噪处理...");
    let mut denoised: Vec<f32> = Vec::with_capacity(audio.len());
    let mut offset = 0;
    let mut chunk_count = 0;

    while offset < audio.len() {
        let end = (offset + FRAME_SIZE).min(audio.len());
        let frame = &audio[offset..end];

        // 使用频域GTCRN进行流式推理（内部STFT->推理->iSTFT）
        match denoiser.denoise_frame(frame) {
            Ok(denoised_frame) => {
                denoised.extend_from_slice(&denoised_frame);

                // 每100个块报告一次进度
                if chunk_count % 100 == 0 {
                    let progress = (end as f32 / audio.len() as f32) * 100.0;
                    info!(
                        "流式推理进度: {:.1}% ({} 块, 输出{}样本)",
                        progress,
                        chunk_count,
                        denoised_frame.len()
                    );
                }
            },
            Err(e) => {
                error!("流式推理出错: {}", e);
                // 出错时使用原始音频数据
                denoised.extend_from_slice(frame);
            },
        }

        chunk_count += 1;
        offset = end;
    }

    // 4. 保存降噪后的音频
    write_wav_f32_mono_16k(output_file, &denoised)?;

    info!("处理完成:");
    info!("  输入样本数: {}", audio.len());
    info!("  输出样本数: {}", denoised.len());
    info!("  处理块数: {}", chunk_count);
    info!("  输入时长: {:.2}秒", audio.len() as f32 / TARGET_SR as f32);
    info!("  输出时长: {:.2}秒", denoised.len() as f32 / TARGET_SR as f32);
    info!("  输出文件: {}", output_file);

    Ok(())
}
