//! 音频增强模块 - 专门处理房间混响和音质改善
//!
//! 提供去混响、谱减法降噪、多带均衡器和动态范围压缩等功能

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use tracing::{debug, info};

/// 音频增强配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioEnhancementConfig {
    /// 是否启用音频增强
    pub enabled: bool,
    /// 去混响配置
    pub dereverberation: DereverberationConfig,
    /// 谱减法降噪配置
    pub spectral_subtraction: SpectralSubtractionConfig,
    /// 多带均衡器配置
    pub multiband_equalizer: MultibandEqualizerConfig,
    /// 动态范围压缩配置
    pub dynamic_range_compression: DynamicRangeCompressionConfig,
}

impl Default for AudioEnhancementConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dereverberation: DereverberationConfig::default(),
            spectral_subtraction: SpectralSubtractionConfig::default(),
            multiband_equalizer: MultibandEqualizerConfig::default(),
            dynamic_range_compression: DynamicRangeCompressionConfig::default(),
        }
    }
}

/// 去混响配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DereverberationConfig {
    /// 是否启用去混响
    pub enabled: bool,
    /// 混响时间估计 (秒)
    pub estimated_rt60: f32,
    /// 衰减因子 (0.0-1.0)
    pub decay_factor: f32,
    /// 延迟线长度 (样本数)
    pub delay_length: usize,
    /// 前向预测阶数
    pub prediction_order: usize,
}

impl Default for DereverberationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            estimated_rt60: 0.8,    // 估计混响时间 0.8秒
            decay_factor: 0.85,     // 衰减因子
            delay_length: 512,      // 延迟线长度
            prediction_order: 16,   // 前向预测阶数
        }
    }
}

/// 谱减法降噪配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectralSubtractionConfig {
    /// 是否启用谱减法
    pub enabled: bool,
    /// 过减因子 (1.0-3.0)
    pub over_subtraction_factor: f32,
    /// 谱底噪因子 (0.0-1.0)
    pub noise_floor_factor: f32,
    /// 噪声谱平滑因子 (0.0-1.0)
    pub noise_smoothing_factor: f32,
    /// 最小增益 (dB)
    pub min_gain_db: f32,
    /// 噪声估计帧数
    pub noise_estimation_frames: usize,
}

impl Default for SpectralSubtractionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            over_subtraction_factor: 2.0,      // 过减因子
            noise_floor_factor: 0.1,           // 谱底噪因子
            noise_smoothing_factor: 0.9,       // 噪声平滑因子
            min_gain_db: -20.0,                // 最小增益
            noise_estimation_frames: 20,       // 噪声估计帧数
        }
    }
}

/// 多带均衡器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultibandEqualizerConfig {
    /// 是否启用多带均衡器
    pub enabled: bool,
    /// 频带配置
    pub bands: Vec<EqualizerBand>,
}

/// 均衡器频带
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EqualizerBand {
    /// 中心频率 (Hz)
    pub center_freq: f32,
    /// 带宽 (Hz)
    pub bandwidth: f32,
    /// 增益 (dB)
    pub gain_db: f32,
    /// 滤波器类型
    pub filter_type: FilterType,
}

/// 滤波器类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FilterType {
    /// 低通滤波器
    Lowpass,
    /// 高通滤波器
    Highpass,
    /// 带通滤波器
    Bandpass,
    /// 带阻滤波器
    Bandstop,
    /// 峰值/陷波滤波器
    Peaking,
}

impl Default for MultibandEqualizerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bands: vec![
                // 低频增强 - 提升人声基频
                EqualizerBand {
                    center_freq: 100.0,
                    bandwidth: 50.0,
                    gain_db: 2.0,
                    filter_type: FilterType::Peaking,
                },
                // 中频清晰度 - 提升语音清晰度
                EqualizerBand {
                    center_freq: 1000.0,
                    bandwidth: 200.0,
                    gain_db: 3.0,
                    filter_type: FilterType::Peaking,
                },
                // 高频抑制 - 减少混响和噪声
                EqualizerBand {
                    center_freq: 4000.0,
                    bandwidth: 1000.0,
                    gain_db: -2.0,
                    filter_type: FilterType::Peaking,
                },
                // 超高频抑制 - 去除高频噪声
                EqualizerBand {
                    center_freq: 8000.0,
                    bandwidth: 2000.0,
                    gain_db: -4.0,
                    filter_type: FilterType::Peaking,
                },
            ],
        }
    }
}

/// 动态范围压缩配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicRangeCompressionConfig {
    /// 是否启用动态范围压缩
    pub enabled: bool,
    /// 压缩阈值 (dB)
    pub threshold_db: f32,
    /// 压缩比 (1.0表示不压缩)
    pub ratio: f32,
    /// 攻击时间 (秒)
    pub attack_time: f32,
    /// 释放时间 (秒)
    pub release_time: f32,
    /// 前瞻时间 (秒)
    pub lookahead_time: f32,
}

impl Default for DynamicRangeCompressionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_db: -18.0,    // 压缩阈值
            ratio: 3.0,             // 3:1 压缩比
            attack_time: 0.005,     // 5ms 攻击时间
            release_time: 0.1,      // 100ms 释放时间
            lookahead_time: 0.005,  // 5ms 前瞻时间
        }
    }
}

/// 音频增强器主结构
#[derive(Debug)]
pub struct AudioEnhancer {
    config: AudioEnhancementConfig,
    sample_rate: f32,

    // 去混响组件
    dereverberation: DereverberationProcessor,

    // 谱减法组件
    spectral_subtraction: SpectralSubtractionProcessor,

    // 多带均衡器组件
    multiband_equalizer: MultibandEqualizer,

    // 动态范围压缩组件
    dynamic_compressor: DynamicRangeCompressor,

    // 统计信息
    frame_count: u64,
}

impl AudioEnhancer {
    /// 创建新的音频增强器
    pub fn new(config: AudioEnhancementConfig, sample_rate: f32) -> Self {
        let dereverberation = DereverberationProcessor::new(&config.dereverberation, sample_rate);
        let spectral_subtraction = SpectralSubtractionProcessor::new(&config.spectral_subtraction, sample_rate);
        let multiband_equalizer = MultibandEqualizer::new(&config.multiband_equalizer, sample_rate);
        let dynamic_compressor = DynamicRangeCompressor::new(&config.dynamic_range_compression, sample_rate);

        Self {
            config,
            sample_rate,
            dereverberation,
            spectral_subtraction,
            multiband_equalizer,
            dynamic_compressor,
            frame_count: 0,
        }
    }

    /// 处理音频数据（就地处理）
    pub fn process_inplace(&mut self, audio: &mut [f32]) {
        if !self.config.enabled || audio.is_empty() {
            return;
        }

        self.frame_count += 1;

        // 1. 去混响处理
        if self.config.dereverberation.enabled {
            self.dereverberation.process_inplace(audio);
        }

        // 2. 谱减法降噪
        if self.config.spectral_subtraction.enabled {
            self.spectral_subtraction.process_inplace(audio);
        }

        // 3. 多带均衡器
        if self.config.multiband_equalizer.enabled {
            self.multiband_equalizer.process_inplace(audio);
        }

        // 4. 动态范围压缩
        if self.config.dynamic_range_compression.enabled {
            self.dynamic_compressor.process_inplace(audio);
        }

        // 定期输出统计信息
        if self.frame_count % 100 == 0 {
            debug!("🎚️ 音频增强处理帧 #{}: 长度={}样本", self.frame_count, audio.len());
        }
    }

    /// 获取增强统计信息
    pub fn get_enhancement_stats(&self) -> AudioEnhancementStats {
        AudioEnhancementStats {
            frames_processed: self.frame_count,
            dereverberation_enabled: self.config.dereverberation.enabled,
            spectral_subtraction_enabled: self.config.spectral_subtraction.enabled,
            multiband_eq_enabled: self.config.multiband_equalizer.enabled,
            dynamic_compression_enabled: self.config.dynamic_range_compression.enabled,
            current_noise_floor: self.spectral_subtraction.get_current_noise_floor(),
            current_compressor_gain: self.dynamic_compressor.get_current_gain_db(),
        }
    }

    /// 重置所有处理器状态
    pub fn reset(&mut self) {
        self.dereverberation.reset();
        self.spectral_subtraction.reset();
        self.multiband_equalizer.reset();
        self.dynamic_compressor.reset();
        self.frame_count = 0;
        info!("🔄 音频增强器已重置");
    }

    /// 更新配置
    pub fn update_config(&mut self, config: AudioEnhancementConfig) {
        self.config = config;
        self.dereverberation.update_config(&self.config.dereverberation);
        self.spectral_subtraction.update_config(&self.config.spectral_subtraction);
        self.multiband_equalizer.update_config(&self.config.multiband_equalizer);
        self.dynamic_compressor.update_config(&self.config.dynamic_range_compression);
    }
}

/// 音频增强统计信息
#[derive(Debug, Clone)]
pub struct AudioEnhancementStats {
    pub frames_processed: u64,
    pub dereverberation_enabled: bool,
    pub spectral_subtraction_enabled: bool,
    pub multiband_eq_enabled: bool,
    pub dynamic_compression_enabled: bool,
    pub current_noise_floor: f32,
    pub current_compressor_gain: f32,
}

// 以下是各个处理器的实现

/// 去混响处理器
#[derive(Debug)]
pub struct DereverberationProcessor {
    config: DereverberationConfig,
    sample_rate: f32,
    delay_line: VecDeque<f32>,
    prediction_coeffs: Vec<f32>,
    input_history: VecDeque<f32>,
}

impl DereverberationProcessor {
    pub fn new(config: &DereverberationConfig, sample_rate: f32) -> Self {
        let mut delay_line = VecDeque::with_capacity(config.delay_length);
        delay_line.resize(config.delay_length, 0.0);

        let mut input_history = VecDeque::with_capacity(config.prediction_order);
        input_history.resize(config.prediction_order, 0.0);

        // 初始化预测系数（简化的LPC系数）
        let mut prediction_coeffs = vec![0.0; config.prediction_order];
        for (i, coeff) in prediction_coeffs.iter_mut().enumerate().take(config.prediction_order) {
            *coeff = 0.8 * (-(i as f32) * 0.2).exp();
        }

        Self {
            config: config.clone(),
            sample_rate,
            delay_line,
            prediction_coeffs,
            input_history,
        }
    }

    pub fn process_inplace(&mut self, audio: &mut [f32]) {
        for sample in audio.iter_mut() {
            *sample = self.process_sample(*sample);
        }
    }

    fn process_sample(&mut self, input: f32) -> f32 {
        // 延迟线处理
        let delayed = self.delay_line.pop_front().unwrap_or(0.0);
        self.delay_line.push_back(input * self.config.decay_factor);

        // 前向预测
        let mut prediction = 0.0;
        for (i, &coeff) in self.prediction_coeffs.iter().enumerate() {
            if i < self.input_history.len() {
                prediction += coeff * self.input_history[i];
            }
        }

        // 更新历史
        self.input_history.pop_front();
        self.input_history.push_back(input);

        // 去混响输出
        let output = input - delayed * 0.5 - prediction * 0.3;

        // 限制输出范围
        output.clamp(-1.0, 1.0)
    }

    pub fn reset(&mut self) {
        self.delay_line.clear();
        self.delay_line.resize(self.config.delay_length, 0.0);
        self.input_history.clear();
        self.input_history.resize(self.config.prediction_order, 0.0);
    }

    pub fn update_config(&mut self, config: &DereverberationConfig) {
        self.config = config.clone();
        self.reset();
    }
}

/// 谱减法降噪处理器
#[derive(Debug)]
pub struct SpectralSubtractionProcessor {
    config: SpectralSubtractionConfig,
    sample_rate: f32,
    noise_spectrum: Vec<f32>,
    frame_count: usize,
    noise_floor: f32,
}

impl SpectralSubtractionProcessor {
    pub fn new(config: &SpectralSubtractionConfig, sample_rate: f32) -> Self {
        Self {
            config: config.clone(),
            sample_rate,
            noise_spectrum: vec![0.0; 512], // 简化的频谱大小
            frame_count: 0,
            noise_floor: 0.01,
        }
    }

    pub fn process_inplace(&mut self, audio: &mut [f32]) {
        // 简化的谱减法实现
        // 在实际应用中，这里应该使用FFT进行频域处理

        // 计算当前帧的能量
        let energy = audio.iter().map(|&x| x * x).sum::<f32>() / audio.len() as f32;

        // 更新噪声底噪估计
        if self.frame_count < self.config.noise_estimation_frames {
            self.noise_floor = (self.noise_floor * self.frame_count as f32 + energy) / (self.frame_count + 1) as f32;
        }

        // 计算增益
        let snr = if self.noise_floor > 0.0 {
            (energy / self.noise_floor).max(0.1)
        } else {
            1.0
        };

        let gain = if snr > 1.0 {
            1.0 - (1.0 / snr).powf(0.5)
        } else {
            self.config.noise_floor_factor
        };

        let final_gain = gain.max(10.0_f32.powf(self.config.min_gain_db / 20.0));

        // 应用增益
        for sample in audio.iter_mut() {
            *sample *= final_gain;
        }

        self.frame_count += 1;
    }

    pub fn get_current_noise_floor(&self) -> f32 {
        self.noise_floor
    }

    pub fn reset(&mut self) {
        self.noise_spectrum.fill(0.0);
        self.frame_count = 0;
        self.noise_floor = 0.01;
    }

    pub fn update_config(&mut self, config: &SpectralSubtractionConfig) {
        self.config = config.clone();
    }
}

/// 多带均衡器
#[derive(Debug)]
pub struct MultibandEqualizer {
    config: MultibandEqualizerConfig,
    sample_rate: f32,
    filters: Vec<BiquadFilter>,
}

impl MultibandEqualizer {
    pub fn new(config: &MultibandEqualizerConfig, sample_rate: f32) -> Self {
        let mut filters = Vec::new();

        for band in &config.bands {
            let filter = BiquadFilter::new_peaking(
                band.center_freq,
                band.bandwidth,
                band.gain_db,
                sample_rate,
            );
            filters.push(filter);
        }

        Self {
            config: config.clone(),
            sample_rate,
            filters,
        }
    }

    pub fn process_inplace(&mut self, audio: &mut [f32]) {
        // 串联所有滤波器
        for filter in &mut self.filters {
            filter.process_inplace(audio);
        }
    }

    pub fn reset(&mut self) {
        for filter in &mut self.filters {
            filter.reset();
        }
    }

    pub fn update_config(&mut self, config: &MultibandEqualizerConfig) {
        self.config = config.clone();
        self.filters.clear();

        for band in &config.bands {
            let filter = BiquadFilter::new_peaking(
                band.center_freq,
                band.bandwidth,
                band.gain_db,
                self.sample_rate,
            );
            self.filters.push(filter);
        }
    }
}

/// 动态范围压缩器
#[derive(Debug)]
pub struct DynamicRangeCompressor {
    config: DynamicRangeCompressionConfig,
    sample_rate: f32,
    envelope: f32,
    gain_reduction: f32,
    lookahead_buffer: VecDeque<f32>,
    lookahead_samples: usize,
}

impl DynamicRangeCompressor {
    pub fn new(config: &DynamicRangeCompressionConfig, sample_rate: f32) -> Self {
        let lookahead_samples = (config.lookahead_time * sample_rate) as usize;
        let mut lookahead_buffer = VecDeque::with_capacity(lookahead_samples);
        lookahead_buffer.resize(lookahead_samples, 0.0);

        Self {
            config: config.clone(),
            sample_rate,
            envelope: 0.0,
            gain_reduction: 0.0,
            lookahead_buffer,
            lookahead_samples,
        }
    }

    pub fn process_inplace(&mut self, audio: &mut [f32]) {
        let attack_coeff = (-1.0 / (self.config.attack_time * self.sample_rate)).exp();
        let release_coeff = (-1.0 / (self.config.release_time * self.sample_rate)).exp();
        let threshold_linear = 10.0_f32.powf(self.config.threshold_db / 20.0);

        for sample in audio.iter_mut() {
            // 前瞻处理
            let delayed_sample = self.lookahead_buffer.pop_front().unwrap_or(0.0);
            self.lookahead_buffer.push_back(*sample);

            // 包络跟踪
            let input_level = sample.abs();
            if input_level > self.envelope {
                self.envelope = input_level + attack_coeff * (self.envelope - input_level);
            } else {
                self.envelope = input_level + release_coeff * (self.envelope - input_level);
            }

            // 计算增益衰减
            let gain_reduction = if self.envelope > threshold_linear {
                let over_threshold = self.envelope / threshold_linear;
                let compressed = over_threshold.powf(1.0 / self.config.ratio);
                threshold_linear * compressed / self.envelope
            } else {
                1.0
            };

            self.gain_reduction = gain_reduction;

            // 应用压缩
            *sample = delayed_sample * gain_reduction;
        }
    }

    pub fn get_current_gain_db(&self) -> f32 {
        20.0 * self.gain_reduction.log10()
    }

    pub fn reset(&mut self) {
        self.envelope = 0.0;
        self.gain_reduction = 0.0;
        self.lookahead_buffer.clear();
        self.lookahead_buffer.resize(self.lookahead_samples, 0.0);
    }

    pub fn update_config(&mut self, config: &DynamicRangeCompressionConfig) {
        self.config = config.clone();
        let lookahead_samples = (config.lookahead_time * self.sample_rate) as usize;
        self.lookahead_buffer.clear();
        self.lookahead_buffer.resize(lookahead_samples, 0.0);
        self.lookahead_samples = lookahead_samples;
    }
}

/// 双二阶滤波器
#[derive(Debug)]
pub struct BiquadFilter {
    // 滤波器系数
    b0: f32, b1: f32, b2: f32,
    a1: f32, a2: f32,
    // 延迟单元
    x1: f32, x2: f32,
    y1: f32, y2: f32,
}

impl BiquadFilter {
    pub fn new_peaking(center_freq: f32, bandwidth: f32, gain_db: f32, sample_rate: f32) -> Self {
        let omega = 2.0 * std::f32::consts::PI * center_freq / sample_rate;
        let sin_omega = omega.sin();
        let cos_omega = omega.cos();
        let q = center_freq / bandwidth;
        let a = 10.0_f32.powf(gain_db / 40.0);
        let alpha = sin_omega / (2.0 * q);

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_omega;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_omega;
        let a2 = 1.0 - alpha / a;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            x1: 0.0, x2: 0.0,
            y1: 0.0, y2: 0.0,
        }
    }

    pub fn process_inplace(&mut self, audio: &mut [f32]) {
        for sample in audio.iter_mut() {
            *sample = self.process_sample(*sample);
        }
    }

    fn process_sample(&mut self, input: f32) -> f32 {
        let output = self.b0 * input + self.b1 * self.x1 + self.b2 * self.x2
                   - self.a1 * self.y1 - self.a2 * self.y2;

        self.x2 = self.x1;
        self.x1 = input;
        self.y2 = self.y1;
        self.y1 = output;

        output
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}
