//! TTS音频帧处理模块
//!
//! 提供高性能的44100Hz到16kHz降采样和响度调节功能。
//! 使用定点数插值、零拷贝和SIMD友好的代码结构来最大化性能。
//! 支持以dB为单位进行增益控制。

use crate::tts::minimax::AudioChunk;
use tracing::debug;

/// TTS原始采样率（44100Hz）
pub const TTS_SOURCE_SAMPLE_RATE: u32 = 44100;
/// TTS下游输出采样率（16000Hz）
pub const TTS_OUTPUT_SAMPLE_RATE: u32 = 16000;

// ============================================================================
// dB增益相关常量和函数
// ============================================================================

/// dB到线性增益的转换系数
/// 线性增益 = 10^(dB/20)
const DB_TO_LINEAR_CONVERSION_FACTOR: f32 = std::f32::consts::LN_10 / 20.0;

/// 将dB值转换为线性增益倍数
///
/// # 参数
/// * `db_gain` - dB增益值，正数表示放大，负数表示衰减
///
/// # 返回值
/// * 线性增益倍数（例如：0.5表示衰减6dB，2.0表示放大6dB）
///
/// # 示例
/// ```rust
/// let linear = db_to_linear(6.0); // 约等于 2.0
/// let linear = db_to_linear(-6.0); // 约等于 0.5
/// let linear = db_to_linear(0.0); // 等于 1.0
/// ```
#[inline]
pub fn db_to_linear(db_gain: f32) -> f32 {
    if db_gain == 0.0 {
        1.0
    } else {
        (db_gain * DB_TO_LINEAR_CONVERSION_FACTOR).exp()
    }
}

/// 将线性增益倍数转换为dB值
///
/// # 参数
/// * `linear_gain` - 线性增益倍数
///
/// # 返回值
/// * dB增益值
#[inline]
pub fn linear_to_db(linear_gain: f32) -> f32 {
    if linear_gain <= 0.0 {
        -f32::INFINITY
    } else if linear_gain == 1.0 {
        0.0
    } else {
        20.0 * linear_gain.log10()
    }
}

// ============================================================================
// 定点数常量（Q16.16格式）
// 44100/16000 = 2.75625 = 441/160
// 用Q16.16定点数：(441 << 16) / 160 = 180633.6
// ============================================================================

/// 定点数精度位数
const FRAC_BITS: u32 = 16;
/// 定点数1.0
const FRAC_ONE: u32 = 1 << FRAC_BITS;
/// 降采样步进（Q16.16定点数）：44100/16000 * 65536 = 180633.6 ≈ 180634
/// 使用精确分数 441/160 来避免累积误差
const RESAMPLE_STEP_NUM: u32 = 441;
const RESAMPLE_STEP_DEN: u32 = 160;
/// Q16.16格式的步进，用于快速累加采样位置
const RESAMPLE_STEP_Q16: u32 = ((RESAMPLE_STEP_NUM << FRAC_BITS) + (RESAMPLE_STEP_DEN / 2)) / RESAMPLE_STEP_DEN;
/// Q16.16的小数部分掩码
const FRAC_MASK_Q16: u32 = FRAC_ONE - 1;

/// TTS音频帧包装器
#[derive(Debug, Clone)]
pub struct TtsAudioFrame {
    /// 原始音频数据（16位PCM，小端序）
    pub raw_data: Vec<u8>,
    /// 序列ID
    pub sequence_id: u64,
    /// 是否为最后一个块
    pub is_final: bool,
    /// 句子文本
    pub sentence_text: Option<String>,
    /// 源采样率（Hz）- MiniMax=44100, Baidu=16000
    pub sample_rate: u32,
}

impl TtsAudioFrame {
    #[inline(always)]
    pub fn from_audio_chunk(chunk: &AudioChunk) -> Self {
        Self {
            raw_data: chunk.data.clone(),
            sequence_id: chunk.sequence_id,
            is_final: chunk.is_final,
            sentence_text: chunk.sentence_text.clone(),
            sample_rate: chunk.sample_rate,
        }
    }

    /// 使用dB增益进行16kHz降采样和音量调节（假设源采样率为 44100Hz）
    ///
    /// # 参数
    /// * `db_gain` - dB增益值
    ///   - 正数：音量放大（例如 +7.0 表示增益7dB）
    ///   - 负数：音量衰减（例如 -6.0 表示衰减6dB）
    ///   - 0.0：原始音量
    ///
    /// # 返回值
    /// 降采样后的音频数据（16000Hz，16位PCM）
    ///
    /// # 示例
    /// ```rust
    /// let frame = TtsAudioFrame { /* ... */ };
    /// let amplified = frame.resample_to_16k_with_db_gain(7.0); // +7dB增益
    /// let attenuated = frame.resample_to_16k_with_db_gain(-3.0); // -3dB衰减
    /// ```
    #[inline]
    pub fn resample_to_16k_with_db_gain(&self, db_gain: f32) -> Vec<u8> {
        adjust_and_resample_raw_db(&self.raw_data, db_gain)
    }

    /// 🆕 根据源采样率智能处理音频：
    /// - 如果源采样率 == 16000Hz：仅应用增益，不降采样
    /// - 如果源采样率 == 44100Hz：降采样到 16000Hz 并应用增益
    /// - 其他采样率：目前按 44100Hz 处理（可能需要扩展）
    ///
    /// # 参数
    /// * `db_gain` - dB增益值
    ///
    /// # 返回值
    /// 处理后的 16000Hz 16位 PCM 数据
    #[inline]
    pub fn process_to_16k_with_db_gain(&self, db_gain: f32) -> Vec<u8> {
        if self.sample_rate == TTS_OUTPUT_SAMPLE_RATE {
            // 源已经是 16kHz，只需应用增益
            apply_gain_only(&self.raw_data, db_gain)
        } else {
            // 需要降采样（44100 -> 16000）
            adjust_and_resample_raw_db(&self.raw_data, db_gain)
        }
    }
}

/// 仅应用增益（用于已经是 16kHz 的音频，如 Baidu TTS）
#[inline]
pub fn apply_gain_only(data: &[u8], db_gain: f32) -> Vec<u8> {
    let input_len = data.len();
    if input_len < 2 {
        return Vec::new();
    }

    let sample_count = input_len / 2;
    let linear_gain = db_to_linear(db_gain);

    debug!(
        "TTS音量增益处理(直通): db_gain={:.1}dB (linear={:.3}), input_samples={}, input_bytes={}",
        db_gain, linear_gain, sample_count, input_len
    );

    // 如果增益接近 1.0，直接返回原始数据
    if (linear_gain - 1.0).abs() < 0.001 {
        return data.to_vec();
    }

    let mut result = Vec::with_capacity(input_len);

    for i in 0..sample_count {
        let offset = i * 2;
        let sample = i16::from_le_bytes([data[offset], data[offset + 1]]);
        let with_gain = ((sample as f32) * linear_gain) as i32;
        let clamped = with_gain.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        result.extend_from_slice(&clamped.to_le_bytes());
    }

    result
}

// ============================================================================
// 高性能降采样：定点数线性插值 + 直接处理i16，无中间缓冲区
// ============================================================================

/// 融合操作：dB增益调节 + 降采样（单次遍历，无中间缓冲区）
#[inline]
pub fn adjust_and_resample_raw_db(data: &[u8], db_gain: f32) -> Vec<u8> {
    let input_len = data.len();
    if input_len < 2 {
        return Vec::new();
    }

    let input_sample_count = input_len / 2;
    let linear_gain = db_to_linear(db_gain);
    debug!(
        "TTS音量增益处理: db_gain={:.1}dB (linear={:.3}), input_samples={}, input_bytes={}",
        db_gain, linear_gain, input_sample_count, input_len
    );
    let output_len = (input_sample_count as u64 * RESAMPLE_STEP_DEN as u64 / RESAMPLE_STEP_NUM as u64) as usize + 1;

    let mut result = Vec::with_capacity(output_len * 2);
    let last_idx = input_sample_count.saturating_sub(1);

    // 预计算增益定点数（Q16.16格式）
    let gain_fixed = (linear_gain * FRAC_ONE as f32) as i32;
    let mut position_q16: u64 = 0;
    let step_q16 = RESAMPLE_STEP_Q16 as u64;
    let frac_mask = FRAC_MASK_Q16 as u64;

    unsafe {
        let ptr: *mut u8 = result.as_mut_ptr();
        let input_ptr = data.as_ptr();
        let mut write_idx = 0usize;
        let mut remaining = output_len;

        macro_rules! process_sample {
            () => {{
                let idx = (position_q16 >> FRAC_BITS) as usize;
                if idx >= input_sample_count {
                    false
                } else {
                    let frac = (position_q16 & frac_mask) as u32;
                    let curr_raw = read_i16_le_unchecked(input_ptr, idx);
                    let next_raw = if idx < last_idx {
                        read_i16_le_unchecked(input_ptr, idx + 1)
                    } else {
                        curr_raw
                    };

                    let curr = curr_raw as i32;
                    let next = next_raw as i32;
                    let diff = next.wrapping_sub(curr);
                    let interpolated = curr + ((diff * frac as i32) >> FRAC_BITS);

                    let with_gain = ((interpolated as i64 * gain_fixed as i64) >> FRAC_BITS) as i32;
                    // 直接转换，不限幅
                    let sample = with_gain as i16;

                    let sample_ptr = ptr.add(write_idx) as *mut i16;
                    std::ptr::write_unaligned(sample_ptr, sample.to_le());
                    write_idx += 2;

                    position_q16 += step_q16;
                    true
                }
            }};
        }

        while remaining >= 4 {
            if !process_sample!() {
                break;
            }
            if !process_sample!() {
                break;
            }
            if !process_sample!() {
                break;
            }
            if !process_sample!() {
                break;
            }
            remaining -= 4;
        }

        while remaining > 0 {
            if !process_sample!() {
                break;
            }
            remaining -= 1;
        }

        result.set_len(write_idx);
    }

    result
}

// ============================================================================
// 内联辅助函数
// ============================================================================

/// 无边界检查读取i16（通过样本索引）
/// 优化：使用unaligned读取，避免对齐要求，提升性能
#[inline(always)]
unsafe fn read_i16_le_unchecked(ptr: *const u8, sample_idx: usize) -> i16 {
    unsafe {
        let byte_ptr = ptr.add(sample_idx * 2) as *const i16;
        // 使用read_unaligned避免对齐检查，直接读取i16
        std::ptr::read_unaligned(byte_ptr).to_le()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resample_empty() {
        let frame = TtsAudioFrame {
            raw_data: Vec::new(),
            sequence_id: 0,
            is_final: false,
            sentence_text: None,
            sample_rate: 44100,
        };
        assert!(frame.resample_to_16k_with_db_gain(7.0).is_empty());
    }

    // ============================================================================
    // dB转换函数测试
    // ============================================================================

    #[test]
    fn test_db_to_linear_conversion() {
        // 测试0dB = 1.0
        assert!((db_to_linear(0.0) - 1.0).abs() < f32::EPSILON);

        // 测试+6dB ≈ 2.0
        let gain_6db = db_to_linear(6.0);
        assert!((gain_6db - 2.0).abs() < 0.01);

        // 测试-6dB ≈ 0.5
        let gain_minus_6db = db_to_linear(-6.0);
        assert!((gain_minus_6db - 0.5).abs() < 0.01);

        // 测试+20dB = 10.0
        let gain_20db = db_to_linear(20.0);
        assert!((gain_20db - 10.0).abs() < 0.01);

        // 测试-20dB = 0.1
        let gain_minus_20db = db_to_linear(-20.0);
        assert!((gain_minus_20db - 0.1).abs() < 0.01);

        // 测试+7dB（用户请求的值）
        let gain_7db = db_to_linear(7.0);
        assert!((gain_7db - 2.2387).abs() < 0.01); // 10^(7/20) ≈ 2.2387
    }

    #[test]
    fn test_linear_to_db_conversion() {
        // 测试1.0 = 0dB
        assert!((linear_to_db(1.0) - 0.0).abs() < f32::EPSILON);

        // 测试2.0 ≈ +6.02dB (20*log10(2) = 6.0206)
        let db_2 = linear_to_db(2.0);
        assert!((db_2 - 6.0206).abs() < 0.01);

        // 测试0.5 ≈ -6.02dB (20*log10(0.5) = -6.0206)
        let db_half = linear_to_db(0.5);
        assert!((db_half - (-6.0206)).abs() < 0.01);

        // 测试10.0 = +20dB
        let db_10 = linear_to_db(10.0);
        assert!((db_10 - 20.0).abs() < 0.01);

        // 测试0.1 = -20dB
        let db_tenth = linear_to_db(0.1);
        assert!((db_tenth - (-20.0)).abs() < 0.01);

        // 测试0或负数应该返回负无穷
        assert!(linear_to_db(0.0) == f32::NEG_INFINITY);
        assert!(linear_to_db(-1.0) == f32::NEG_INFINITY);
    }

    #[test]
    fn test_db_linear_round_trip() {
        let test_values = vec![-20.0, -10.0, -6.0, -3.0, 0.0, 3.0, 6.0, 10.0, 20.0];

        for db in test_values {
            let linear = db_to_linear(db);
            let db_back = linear_to_db(linear);
            assert!(
                (db - db_back).abs() < 0.1,
                "Round trip failed for {}dB: {}dB -> {} -> {}dB",
                db,
                db,
                linear,
                db_back
            );
        }
    }

    // ============================================================================
    // dB增益处理测试
    // ============================================================================

    #[test]
    fn test_resample_ratio_db() {
        // 创建1秒的44100Hz音频
        let sample_count = 44100;
        let mut raw_data = Vec::with_capacity(sample_count * 2);
        for i in 0..sample_count {
            let sample = ((i as f32 / sample_count as f32) * 2.0 - 1.0) * i16::MAX as f32;
            raw_data.extend_from_slice(&(sample as i16).to_le_bytes());
        }

        let frame = TtsAudioFrame {
            raw_data: raw_data.clone(),
            sequence_id: 0,
            is_final: false,
            sentence_text: None,
            sample_rate: 44100,
        };

        // 使用+7dB增益（用户请求的值）
        let resampled = frame.resample_to_16k_with_db_gain(7.0);

        // 期望输出约16000个样本
        let output_samples = resampled.len() / 2;
        assert!(
            (output_samples as i32 - 16000).abs() < 10,
            "Expected ~16000 samples, got {}",
            output_samples
        );
    }

    #[test]
    fn test_db_gain_7db_specific() {
        // 专门测试+7dB增益（用户请求的值）
        let sample_count = 4410; // 100ms
        let mut raw_data = Vec::with_capacity(sample_count * 2);
        for i in 0..sample_count {
            let sample = (i as f32 / sample_count as f32) * (i16::MAX / 4) as f32;
            raw_data.extend_from_slice(&(sample as i16).to_le_bytes());
        }

        let frame = TtsAudioFrame {
            raw_data,
            sequence_id: 0,
            is_final: false,
            sentence_text: None,
            sample_rate: 44100,
        };

        let result_7db = frame.resample_to_16k_with_db_gain(7.0);

        // 验证+7dB增益的线性倍数约为2.2387
        let expected_gain = db_to_linear(7.0);
        assert!((expected_gain - 2.2387).abs() < 0.001);

        // 检查增益效果：7dB增益后应该使音量约为原来的2.24倍
        let input_max = 32767 / 4; // 原始最大值
        let expected_output_max = (input_max as f32 * expected_gain) as i16;

        // 查找输出中的最大值
        let mut actual_max = 0i16;
        for i in 0..result_7db.len() / 2 {
            let sample = i16::from_le_bytes([result_7db[i * 2], result_7db[i * 2 + 1]]);
            actual_max = actual_max.max(sample.abs());
        }

        // 允许一定的误差范围
        assert!(
            (actual_max as f32 - expected_output_max as f32).abs() < expected_output_max as f32 * 0.1,
            "Expected max around {}, got {}",
            expected_output_max,
            actual_max
        );
    }

    #[test]
    fn test_interpolation_quality() {
        // 测试插值质量：生成正弦波，降采样后检查频率保持
        let sample_count = 44100; // 1秒
        let freq = 440.0; // 440Hz
        let mut raw_data = Vec::with_capacity(sample_count * 2);

        for i in 0..sample_count {
            let t = i as f32 / 44100.0;
            let sample = (t * freq * 2.0 * std::f32::consts::PI).sin() * (i16::MAX / 2) as f32;
            raw_data.extend_from_slice(&(sample as i16).to_le_bytes());
        }

        let frame = TtsAudioFrame {
            raw_data,
            sequence_id: 0,
            is_final: false,
            sentence_text: None,
            sample_rate: 44100,
        };

        let resampled = frame.resample_to_16k_with_db_gain(1.5); // +1.5dB增益
        let output_samples = resampled.len() / 2;

        // 检查输出采样数正确
        assert!(
            (output_samples as i32 - 16000).abs() < 10,
            "Expected ~16000 samples, got {}",
            output_samples
        );

        // 检查信号不是全零
        let mut has_nonzero = false;
        for i in 0..output_samples {
            let sample = i16::from_le_bytes([resampled[i * 2], resampled[i * 2 + 1]]);
            if sample.abs() > 100 {
                has_nonzero = true;
                break;
            }
        }
        assert!(has_nonzero, "Resampled signal should not be all zeros");
    }

    // ============================================================================
    // 性能测试
    // ============================================================================

    use std::time::Instant;

    fn create_test_audio(duration_ms: u32) -> Vec<u8> {
        let sample_count = (TTS_SOURCE_SAMPLE_RATE * duration_ms / 1000) as usize;
        let mut raw_data = Vec::with_capacity(sample_count * 2);
        for i in 0..sample_count {
            let t = i as f32 / TTS_SOURCE_SAMPLE_RATE as f32;
            let sample = (t * 440.0 * 2.0 * std::f32::consts::PI).sin() * i16::MAX as f32;
            raw_data.extend_from_slice(&(sample as i16).to_le_bytes());
        }
        raw_data
    }

    #[test]
    fn test_performance_resample_to_16k_with_gain() {
        println!("\n");
        println!("╔══════════════════════════════════════════════════════════════════════════════╗");
        println!("║                    resample_to_16k_with_gain 性能测试                        ║");
        println!("╚══════════════════════════════════════════════════════════════════════════════╝");

        let test_durations = vec![100, 500, 1000, 5000];
        let iterations = 100;

        for duration_ms in test_durations {
            let raw_data = create_test_audio(duration_ms);
            let frame = TtsAudioFrame {
                raw_data,
                sequence_id: 0,
                is_final: false,
                sentence_text: None,
                sample_rate: 44100,
            };

            let start = Instant::now();
            for _ in 0..iterations {
                let _ = frame.resample_to_16k_with_db_gain(7.0); // +7dB增益
            }
            let elapsed = start.elapsed();

            let input_samples = TTS_SOURCE_SAMPLE_RATE as u64 * duration_ms as u64 / 1000;
            let output_samples = (input_samples * RESAMPLE_STEP_DEN as u64 / RESAMPLE_STEP_NUM as u64) + 1;
            let total_input_samples = input_samples * iterations;
            let total_output_samples = output_samples * iterations;
            let input_samples_per_sec = total_input_samples as f64 / elapsed.as_secs_f64();
            let output_samples_per_sec = total_output_samples as f64 / elapsed.as_secs_f64();
            let avg_time_per_frame = elapsed.as_secs_f64() / iterations as f64 * 1000.0;
            let total_time_ms = elapsed.as_secs_f64() * 1000.0;

            println!("  ┌─────────────────────────────────────────────────────────────────────────┐");
            println!("  │ 测试音频长度: {} ms", duration_ms);
            println!(
                "  │ 输入: {} 样本 ({:.2} KB)",
                input_samples,
                (input_samples * 2) as f32 / 1024.0
            );
            println!(
                "  │ 输出: {} 样本 ({:.2} KB)",
                output_samples,
                (output_samples * 2) as f32 / 1024.0
            );
            println!("  │ 迭代次数: {}", iterations);
            println!("  │ 总耗时: {:.3} ms", total_time_ms);
            println!("  │ 平均每帧耗时: {:.3} ms", avg_time_per_frame);
            println!("  │ 输入处理速度: {:.2} M samples/s", input_samples_per_sec / 1_000_000.0);
            println!("  │ 输出生成速度: {:.2} M samples/s", output_samples_per_sec / 1_000_000.0);
            println!("  └─────────────────────────────────────────────────────────────────────────┘");
        }
        println!();
    }

    #[test]
    fn test_performance_comprehensive() {
        println!("\n");
        println!("╔══════════════════════════════════════════════════════════════════════════════╗");
        println!("║                          综合性能测试                                          ║");
        println!("╚══════════════════════════════════════════════════════════════════════════════╝");
        println!();
        println!("测试不同音频长度的处理性能（单次迭代）");
        println!();
        println!("┌──────────┬──────────────────────┬──────────────┐");
        println!("│ 时长(ms) │ resample_to_16k_with_gain(ms)  │ 数据量(KB)   │");
        println!("├──────────┼──────────────────────┼──────────────┤");

        let test_durations = vec![10, 50, 100, 500, 1000, 5000];

        for duration_ms in test_durations {
            let raw_data = create_test_audio(duration_ms);
            let frame = TtsAudioFrame {
                raw_data: raw_data.clone(),
                sequence_id: 0,
                is_final: false,
                sentence_text: None,
                sample_rate: 44100,
            };

            let data_size_kb = raw_data.len() as f64 / 1024.0;

            let start = Instant::now();
            let _ = frame.resample_to_16k_with_db_gain(7.0); // +7dB增益
            let resample_time = start.elapsed().as_secs_f64() * 1000.0;

            println!("│ {:8} │ {:20.3} │ {:12.2} │", duration_ms, resample_time, data_size_kb);
        }

        println!("└──────────┴──────────────────────┴──────────────┘");
        println!();
    }
}
