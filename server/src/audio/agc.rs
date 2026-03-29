//! 自动增益控制(AGC)模块
//!
//! 专为归一化音频范围 [-1.0, 1.0] 优化的高性能AGC实现

use std::collections::VecDeque;
use crate::audio::enhancement::{AudioEnhancer, AudioEnhancementConfig};

/// AGC配置 - 专为归一化音频范围 [-1.0, 1.0] 优化
#[derive(Debug, Clone)]
pub struct AgcConfig {
    /// 是否启用AGC
    pub enabled: bool,
    /// 目标RMS电平 (归一化范围 0.0-1.0)
    pub target_rms: f32,
    /// 最大增益 (dB)
    pub max_gain_db: f32,
    /// 最小增益 (dB)
    pub min_gain_db: f32,
    /// 攻击时间常数 (秒)
    pub attack_time: f32,
    /// 释放时间常数 (秒)
    pub release_time: f32,
    /// 噪声门限 (RMS，归一化范围 0.0-1.0)
    pub noise_gate: f32,
    /// 是否启用高通滤波器
    pub highpass_enabled: bool,
    /// 高通滤波器截止频率 (Hz)
    pub highpass_freq: f32,
    /// 音频增强配置
    pub audio_enhancement: Option<AudioEnhancementConfig>,
}

impl Default for AgcConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            target_rms: 0.1,    // f32范围：约10%满量程
            max_gain_db: 12.0,  // 最大增益12dB
            min_gain_db: -20.0, // 最小增益-20dB
            attack_time: 0.1,   // 攻击 100ms
            release_time: 0.3,  // 释放 300ms
            noise_gate: 0.001,  // f32范围：0.1%噪声门限
            highpass_enabled: true, // 启用高通滤波
            highpass_freq: 100.0,    // 100Hz高通
            audio_enhancement: None, // 不启用音频增强
        }
    }
}

/// AGC状态 - 高性能版本
#[derive(Debug)]
struct AgcState {
    /// 当前增益 (线性)
    current_gain: f32,
    /// 平滑的RMS估计
    smoothed_rms: f32,
    /// 历史音频样本（用于RMS计算）
    rms_window: VecDeque<f32>,
    /// RMS窗口大小（样本数）
    rms_window_size: usize,
    /// 处理的帧计数，用于优化日志输出
    frame_count: u32,
    /// 预计算的时间常数（性能优化）
    attack_alpha_base: f32,
    release_alpha_base: f32,
    gain_alpha_base: f32,
    /// 预计算的增益范围
    max_gain_linear: f32,
    min_gain_linear: f32,
    /// 高通滤波器
    highpass_filter: HighpassFilter,
    /// 增益平滑因子（独立的平滑时间常数）
    gain_smooth_alpha: f32,
    /// 音频增强器
    audio_enhancer: Option<AudioEnhancer>,
}

impl AgcState {
    fn new(sample_rate: i32, config: &AgcConfig) -> Self {
        // RMS计算窗口 125 ms，适中平滑度
        let rms_window_size = (sample_rate as f32 * 0.125) as usize;
        let sample_rate_f = sample_rate as f32;

        // 初始化音频增强器
        let audio_enhancer = config.audio_enhancement.as_ref().map(|enhancement_config| AudioEnhancer::new(enhancement_config.clone(), sample_rate_f));

        Self {
            current_gain: 1.0,
            smoothed_rms: 0.0,
            rms_window: VecDeque::with_capacity(rms_window_size),
            rms_window_size,
            frame_count: 0,
            // 预计算时间常数基础值（避免运行时除法）
            attack_alpha_base: 1.0 / (sample_rate_f * config.attack_time),
            release_alpha_base: 1.0 / (sample_rate_f * config.release_time),
            gain_alpha_base: 1.0 / (sample_rate_f * config.attack_time),
            // 预计算增益范围（避免运行时指数运算）
            max_gain_linear: 10.0_f32.powf(config.max_gain_db * 0.05),
            min_gain_linear: 10.0_f32.powf(config.min_gain_db * 0.05),
            highpass_filter: HighpassFilter::new(config.highpass_freq, sample_rate as f32),
            // 独立的增益平滑因子，比攻击/释放时间更保守
            gain_smooth_alpha: 0.95, // 非常平滑的增益变化
            audio_enhancer,
        }
    }

    /// 重置AGC状态
    fn reset(&mut self) {
        self.current_gain = 1.0;
        self.smoothed_rms = 0.0;
        self.rms_window.clear();
        self.frame_count = 0;
        self.highpass_filter.reset();
        self.gain_smooth_alpha = 0.95; // 重置增益平滑因子

        // 重置音频增强器
        if let Some(ref mut enhancer) = self.audio_enhancer {
            enhancer.reset();
        }
    }

    /// 更新预计算值（当配置改变时）
    fn update_precomputed(&mut self, sample_rate: i32, config: &AgcConfig) {
        let sample_rate_f = sample_rate as f32;
        self.attack_alpha_base = 1.0 / (sample_rate_f * config.attack_time);
        self.release_alpha_base = 1.0 / (sample_rate_f * config.release_time);
        self.gain_alpha_base = 1.0 / (sample_rate_f * config.attack_time);
        self.max_gain_linear = 10.0_f32.powf(config.max_gain_db * 0.05);
        self.min_gain_linear = 10.0_f32.powf(config.min_gain_db * 0.05);
        self.highpass_filter.update_cutoff(config.highpass_freq, sample_rate as f32);

        // 更新音频增强器配置
        match (&mut self.audio_enhancer, &config.audio_enhancement) {
            (Some(enhancer), Some(enhancement_config)) => {
                enhancer.update_config(enhancement_config.clone());
            }
            (None, Some(enhancement_config)) => {
                self.audio_enhancer = Some(AudioEnhancer::new(enhancement_config.clone(), sample_rate_f));
            }
            (Some(_), None) => {
                self.audio_enhancer = None;
            }
            (None, None) => {
                // 无需操作
            }
        }
    }
}

/// 高通滤波器状态 - 一阶IIR滤波器
#[derive(Debug, Clone)]
struct HighpassFilter {
    /// 前一个输入样本
    prev_input: f32,
    /// 前一个输出样本
    prev_output: f32,
    /// 滤波器系数
    alpha: f32,
}

impl HighpassFilter {
    fn new(cutoff_freq: f32, sample_rate: f32) -> Self {
        // 改进的一阶高通滤波器系数计算
        // 使用双线性变换方法，更准确
        let omega = 2.0 * std::f32::consts::PI * cutoff_freq / sample_rate;
        let alpha = 1.0 / (1.0 + omega);

        Self { prev_input: 0.0, prev_output: 0.0, alpha }
    }

    /// 处理单个样本
    #[inline]
    fn process_sample(&mut self, input: f32) -> f32 {
        // 改进的一阶高通滤波器：y[n] = α * (y[n-1] + x[n] - x[n-1])
        let output = self.alpha * (self.prev_output + input - self.prev_input);

        self.prev_input = input;
        self.prev_output = output;

        output
    }

    /// 批量处理音频样本（高性能版本）
    fn process_inplace(&mut self, audio: &mut [f32]) {
        for sample in audio.iter_mut() {
            *sample = self.process_sample(*sample);
        }
    }

    /// 重置滤波器状态
    fn reset(&mut self) {
        self.prev_input = 0.0;
        self.prev_output = 0.0;
    }

    /// 更新滤波器参数
    fn update_cutoff(&mut self, cutoff_freq: f32, sample_rate: f32) {
        let omega = 2.0 * std::f32::consts::PI * cutoff_freq / sample_rate;
        self.alpha = 1.0 / (1.0 + omega);
    }
}

/// 自动增益控制器 - 专为归一化音频范围 [-1.0, 1.0] 优化
pub struct Agc {
    config: AgcConfig,
    state: AgcState,
    sample_rate: i32,
}

impl Agc {
    /// 创建新的AGC实例
    pub fn new(config: AgcConfig, sample_rate: i32) -> Self {
        let state = AgcState::new(sample_rate, &config);
        Self { config, state, sample_rate }
    }

    /// 就地处理音频样本，返回应用的增益值 - 高性能版本
    pub fn process_inplace(&mut self, audio: &mut [f32]) -> f32 {
        if audio.is_empty() {
            return self.state.current_gain;
        }

        // === 音频处理流水线 ===
        // 1. 先高通滤波，去除 DC 和超低频噪声
        if self.config.highpass_enabled {
            self.state.highpass_filter.process_inplace(audio);
        }

        // 2. 音频增强处理（去混响、降噪、均衡、压缩）
        if let Some(ref mut enhancer) = self.state.audio_enhancer {
            enhancer.process_inplace(audio);
        }

        // 3. 如果未启用 AGC，仅做前面的处理、不做增益控制
        if !self.config.enabled {
            return 1.0; // 单位增益
        }

        // 高性能RMS计算：直接计算当前块的RMS
        let mut sum_sq: f32 = 0.0f32;
        for &sample in audio.iter() {
            sum_sq += sample * sample;
        }
        let current_rms = (sum_sq / audio.len() as f32).sqrt();

        // 快速路径：噪声门限检查
        if current_rms < self.config.noise_gate {
            // 🔧 修复：低于噪声门限时不应用任何增益，保持原样
            // 只是跳过增益计算，不修改音频
            return 1.0; // 返回单位增益表示没有处理
        }

        // 更新RMS历史窗口（简化版本）
        self.state.rms_window.push_back(current_rms);
        if self.state.rms_window.len() > self.state.rms_window_size {
            self.state.rms_window.pop_front();
        }

        // 计算窗口平均RMS（更稳定）
        let window_rms = if !self.state.rms_window.is_empty() {
            self.state.rms_window.iter().sum::<f32>() / self.state.rms_window.len() as f32
        } else {
            current_rms
        };

        // 平滑RMS估计 - 使用预计算的时间常数
        let frame_samples = audio.len() as f32;
        let alpha = if self.state.smoothed_rms > window_rms {
            // 快速攻击
            let time_constant = frame_samples * self.state.attack_alpha_base;
            1.0 - (-time_constant).exp()
        } else {
            // 慢速释放
            let time_constant = frame_samples * self.state.release_alpha_base;
            1.0 - (-time_constant).exp()
        };

        self.state.smoothed_rms = alpha * window_rms + (1.0 - alpha) * self.state.smoothed_rms;

        // 计算目标增益
        let target_gain = if self.state.smoothed_rms > 0.0 {
            self.config.target_rms / self.state.smoothed_rms
        } else {
            1.0
        };

        // 限制增益范围 - 使用预计算值
        let clamped_gain = target_gain.clamp(self.state.min_gain_linear, self.state.max_gain_linear);

        // 改进的平滑增益变化 - 使用独立的平滑因子
        // 使用更保守的平滑算法，确保增益变化更加平滑
        let gain_diff = (clamped_gain - self.state.current_gain).abs();

        // 根据增益变化幅度动态调整平滑度
        let adaptive_alpha = if gain_diff > 0.1 {
            // 大的增益变化需要更平滑
            0.98
        } else {
            // 小的变化可以稍快一些
            self.state.gain_smooth_alpha
        };

        self.state.current_gain = adaptive_alpha * self.state.current_gain + (1.0 - adaptive_alpha) * clamped_gain;

        // 批量应用增益 - SIMD友好的循环
        let gain = self.state.current_gain;
        for sample in audio.iter_mut() {
            *sample *= gain;
        }

        // 性能优化：大幅降低日志输出频率
        self.state.frame_count += 1;
        // if self.state.frame_count % 50 == 0 {
        //     let enhancement_status = if let Some(ref enhancer) = self.state.audio_enhancer {
        //         let stats = enhancer.get_enhancement_stats();
        //         format!("🎚️增强[去混响:{} 降噪:{} 均衡:{} 压缩:{}]",
        //                if stats.dereverberation_enabled { "✓" } else { "✗" },
        //                if stats.spectral_subtraction_enabled { "✓" } else { "✗" },
        //                if stats.multiband_eq_enabled { "✓" } else { "✗" },
        //                if stats.dynamic_compression_enabled { "✓" } else { "✗" })
        //     } else {
        //         "无增强".to_string()
        //     };

        //     info!(
        //         "🔊 AGC[{}]: 增益={:.1}dB, RMS {:.1}% (目标{:.1}%) {} {}",
        //         self.state.frame_count,
        //         20.0 * gain.log10(),
        //         self.state.smoothed_rms * 100.0,
        //         self.config.target_rms * 100.0,
        //         if self.config.highpass_enabled {
        //             format!("HPF@{:.0}Hz", self.config.highpass_freq)
        //         } else {
        //             "无滤波".to_string()
        //         },
        //         enhancement_status
        //     );
        // }

        gain
    }

    /// 重置AGC状态
    pub fn reset(&mut self) {
        self.state.reset();
    }

    /// 获取当前增益(dB)
    pub fn get_current_gain_db(&self) -> f32 {
        20.0 * self.state.current_gain.log10()
    }

    /// 获取当前RMS
    pub fn get_current_rms(&self) -> f32 {
        self.state.smoothed_rms
    }

    /// 设置是否启用
    pub fn set_enabled(&mut self, enabled: bool) {
        self.config.enabled = enabled;
        if !enabled {
            self.state.current_gain = 1.0;
        }
    }

    /// 设置目标RMS（归一化范围 0.0-1.0）
    pub fn set_target_rms(&mut self, target_rms: f32) {
        self.config.target_rms = target_rms.clamp(0.003, 1.0);
    }

    /// 设置目标RMS（百分比）
    pub fn set_target_rms_percent(&mut self, percent: f32) {
        let target = percent / 100.0;
        self.config.target_rms = target.clamp(0.003, 1.0);
    }

    /// 设置噪声门限（归一化范围 0.0-1.0）
    pub fn set_noise_gate(&mut self, noise_gate: f32) {
        self.config.noise_gate = noise_gate.clamp(0.00003, 0.03);
    }

    /// 设置噪声门限（百分比）
    pub fn set_noise_gate_percent(&mut self, percent: f32) {
        let gate = percent / 100.0;
        self.config.noise_gate = gate.clamp(0.00003, 0.03);
    }

    /// 设置增益范围
    pub fn set_gain_range(&mut self, min_db: f32, max_db: f32) {
        self.config.min_gain_db = min_db.clamp(-40.0, 0.0);
        self.config.max_gain_db = max_db.clamp(0.0, 3.0); // 限制最大增益为3dB
        // 更新预计算值
        self.state.update_precomputed(self.sample_rate, &self.config);
    }

    /// 设置时间常数
    pub fn set_time_constants(&mut self, attack_time: f32, release_time: f32) {
        self.config.attack_time = attack_time.clamp(0.001, 1.0);
        self.config.release_time = release_time.clamp(0.001, 1.0);
        // 更新预计算值
        self.state.update_precomputed(self.sample_rate, &self.config);
    }

    /// 设置高通滤波器开关
    pub fn set_highpass_enabled(&mut self, enabled: bool) {
        self.config.highpass_enabled = enabled;
        if !enabled {
            // 重置滤波器状态避免残留
            self.state.highpass_filter.reset();
        }
    }

    /// 设置高通滤波器截止频率
    pub fn set_highpass_frequency(&mut self, freq: f32) {
        self.config.highpass_freq = freq.clamp(20.0, 1000.0); // 20Hz - 1kHz范围
        self.state
            .highpass_filter
            .update_cutoff(self.config.highpass_freq, self.sample_rate as f32);
    }

    /// 获取高通滤波器状态
    pub fn is_highpass_enabled(&self) -> bool {
        self.config.highpass_enabled
    }

    /// 获取高通滤波器频率
    pub fn get_highpass_frequency(&self) -> f32 {
        self.config.highpass_freq
    }

    /// 启用或禁用音频增强
    pub fn set_audio_enhancement_enabled(&mut self, enabled: bool) {
        if enabled && self.config.audio_enhancement.is_none() {
            self.config.audio_enhancement = Some(AudioEnhancementConfig::default());
            self.state.audio_enhancer = Some(AudioEnhancer::new(
                AudioEnhancementConfig::default(),
                self.sample_rate as f32
            ));
        } else if !enabled {
            self.config.audio_enhancement = None;
            self.state.audio_enhancer = None;
        }
    }

    /// 获取音频增强是否启用
    pub fn is_audio_enhancement_enabled(&self) -> bool {
        self.config.audio_enhancement.is_some()
    }

    /// 更新音频增强配置
    pub fn update_audio_enhancement_config(&mut self, config: AudioEnhancementConfig) {
        self.config.audio_enhancement = Some(config.clone());
        if let Some(ref mut enhancer) = self.state.audio_enhancer {
            enhancer.update_config(config);
        } else {
            self.state.audio_enhancer = Some(AudioEnhancer::new(config, self.sample_rate as f32));
        }
    }

    /// 获取音频增强统计信息
    pub fn get_audio_enhancement_stats(&self) -> Option<crate::audio::enhancement::AudioEnhancementStats> {
        self.state.audio_enhancer.as_ref().map(|enhancer| enhancer.get_enhancement_stats())
    }
}
