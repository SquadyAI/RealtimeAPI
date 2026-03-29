//! MP3 解码器
//!
//! 将 Edge TTS 返回的 MP3 音频解码为 PCM
//! 支持增量解码，减少延迟

use minimp3::{Decoder, Error as Mp3Error, Frame};
use std::io::Cursor;

use super::types::EdgeTtsError;

/// 增量解码的最小缓冲区大小（字节）
/// MP3 帧大小约 417 字节（24kHz），设置 1.5KB 可以确保有 3-4 个完整帧
const INCREMENTAL_DECODE_THRESHOLD: usize = 1536;

/// MP3 解码器
///
/// 支持增量解码：累积够一定量数据后立即解码，减少延迟
pub struct Mp3Decoder {
    /// 累积的 MP3 数据缓冲区
    buffer: Vec<u8>,
    /// 检测到的采样率
    detected_sample_rate: Option<i32>,
}

impl Mp3Decoder {
    /// 创建新的 MP3 解码器
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(16384), // 预分配 16KB
            detected_sample_rate: None,
        }
    }

    /// 获取检测到的采样率
    pub fn sample_rate(&self) -> Option<i32> {
        self.detected_sample_rate
    }

    /// 增量解码 MP3 数据
    ///
    /// 累积数据并在达到阈值时解码，减少延迟
    ///
    /// # Arguments
    /// * `mp3_data` - MP3 音频数据块
    ///
    /// # Returns
    /// * `Ok(Vec<u8>)` - 解码后的 PCM 数据（可能为空，如果数据不足）
    pub fn decode(&mut self, mp3_data: &[u8]) -> Result<Vec<u8>, EdgeTtsError> {
        self.buffer.extend_from_slice(mp3_data);

        // 当缓冲区达到阈值时进行增量解码
        if self.buffer.len() >= INCREMENTAL_DECODE_THRESHOLD {
            self.decode_available()
        } else {
            Ok(Vec::new())
        }
    }

    /// 解码缓冲区中所有可解码的帧，保留不完整的数据
    fn decode_available(&mut self) -> Result<Vec<u8>, EdgeTtsError> {
        if self.buffer.is_empty() {
            return Ok(Vec::new());
        }

        let mut pcm_output = Vec::new();
        let mut decoded_bytes = 0;

        // 创建解码器
        let cursor = Cursor::new(&self.buffer);
        let mut decoder = Decoder::new(cursor);

        loop {
            match decoder.next_frame() {
                Ok(Frame { data, sample_rate, channels, .. }) => {
                    // 记录检测到的采样率（仅第一帧）
                    if self.detected_sample_rate.is_none() {
                        self.detected_sample_rate = Some(sample_rate);
                        tracing::debug!("MP3 检测到采样率: {} Hz, channels: {}", sample_rate, channels);
                    }

                    // 转换为单声道 PCM
                    if channels == 2 {
                        for chunk in data.chunks(2) {
                            if chunk.len() == 2 {
                                let mono = ((chunk[0] as i32 + chunk[1] as i32) / 2) as i16;
                                pcm_output.extend_from_slice(&mono.to_le_bytes());
                            }
                        }
                    } else {
                        for sample in data {
                            pcm_output.extend_from_slice(&sample.to_le_bytes());
                        }
                    }

                    // 更新已解码的字节数（使用 Cursor 的位置）
                    decoded_bytes = decoder.reader().position() as usize;
                },
                Err(Mp3Error::Eof) => break,
                Err(Mp3Error::InsufficientData) => {
                    // 数据不足，保留未解码的部分
                    break;
                },
                Err(Mp3Error::SkippedData) => continue,
                Err(_) => continue, // 跳过其他错误
            }
        }

        // 移除已解码的数据，保留未解码的部分
        if decoded_bytes > 0 {
            self.buffer.drain(..decoded_bytes);
            tracing::debug!("增量解码: 已解码 {} 字节，剩余 {} 字节", decoded_bytes, self.buffer.len());
        }

        Ok(pcm_output)
    }

    /// 获取累积的 MP3 数据量
    pub fn buffer_len(&self) -> usize {
        self.buffer.len()
    }

    /// 重置解码器状态
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.detected_sample_rate = None;
    }

    /// 刷新解码器，解码所有剩余的 MP3 数据
    pub fn flush(&mut self) -> Result<Vec<u8>, EdgeTtsError> {
        if self.buffer.is_empty() {
            return Ok(Vec::new());
        }

        tracing::debug!("MP3 flush: 解码剩余 {} 字节", self.buffer.len());

        let mp3_data = std::mem::take(&mut self.buffer);

        // 一次性解码所有剩余数据
        let cursor = Cursor::new(&mp3_data);
        let mut decoder = Decoder::new(cursor);
        let mut pcm_output = Vec::new();

        loop {
            match decoder.next_frame() {
                Ok(Frame { data, sample_rate, channels, .. }) => {
                    if self.detected_sample_rate.is_none() {
                        self.detected_sample_rate = Some(sample_rate);
                    }

                    // 转换为单声道 PCM
                    if channels == 2 {
                        for chunk in data.chunks(2) {
                            if chunk.len() == 2 {
                                let mono = ((chunk[0] as i32 + chunk[1] as i32) / 2) as i16;
                                pcm_output.extend_from_slice(&mono.to_le_bytes());
                            }
                        }
                    } else {
                        for sample in data {
                            pcm_output.extend_from_slice(&sample.to_le_bytes());
                        }
                    }
                },
                Err(Mp3Error::Eof) => break,
                Err(Mp3Error::InsufficientData) => break,
                Err(Mp3Error::SkippedData) => continue,
                Err(e) => {
                    tracing::warn!("MP3 解码错误（跳过）: {:?}", e);
                    continue;
                },
            }
        }

        tracing::debug!("MP3 flush 完成: {} 字节 PCM", pcm_output.len());

        Ok(pcm_output)
    }
}

impl Default for Mp3Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// 通用 PCM 重采样到 16kHz
///
/// 使用线性插值实现重采样
pub fn resample_to_16k(input: &[u8], input_sample_rate: i32) -> Vec<u8> {
    if input_sample_rate == 16000 {
        // 无需重采样
        return input.to_vec();
    }

    // 解析输入样本
    let samples: Vec<i16> = input.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();

    if samples.is_empty() {
        return Vec::new();
    }

    // 计算重采样比率
    let ratio = input_sample_rate as f64 / 16000.0;
    let output_len = (samples.len() as f64 / ratio) as usize;
    let mut output = Vec::with_capacity(output_len * 2);

    // 线性插值重采样
    for i in 0..output_len {
        let src_pos = i as f64 * ratio;
        let src_idx = src_pos as usize;
        let frac = src_pos - src_idx as f64;

        let sample = if src_idx + 1 < samples.len() {
            let s0 = samples[src_idx] as f64;
            let s1 = samples[src_idx + 1] as f64;
            (s0 + (s1 - s0) * frac) as i16
        } else if src_idx < samples.len() {
            samples[src_idx]
        } else {
            0
        };

        output.extend_from_slice(&sample.to_le_bytes());
    }

    output
}

/// 24kHz PCM 重采样到 16kHz（兼容旧代码）
pub fn resample_24k_to_16k(input: &[u8]) -> Vec<u8> {
    resample_to_16k(input, 24000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resample_empty() {
        let result = resample_24k_to_16k(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_resample_ratio() {
        // 创建 300 个样本的测试数据（600 字节 @ 24kHz）
        let input: Vec<u8> = (0..300i16).flat_map(|s| s.to_le_bytes()).collect();

        let output = resample_24k_to_16k(&input);

        // 输出应该是 200 个样本（400 字节 @ 16kHz）
        // 300 * 2 / 3 = 200
        assert_eq!(output.len(), 400);
    }
}
