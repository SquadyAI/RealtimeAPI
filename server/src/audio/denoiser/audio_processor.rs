//! Audio processing module for real-time denoising
//!
//! This module provides a denoiser that can be used in a streaming fashion,
//! similar to the AGC module.

use crate::audio::denoiser::GtcrnFrequencyDenoiser;
use serde::{Deserialize, Serialize};

/// Denoiser configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DenoiserConfig {
    /// Whether the denoiser is enabled
    pub enabled: bool,
}

impl Default for DenoiserConfig {
    fn default() -> Self {
        // 默认关闭降噪器以提高性能
        let enabled = std::env::var("DENOISER_ENABLED")
            .unwrap_or_else(|_| "false".to_string())
            .parse::<bool>()
            .unwrap_or(false);

        Self { enabled }
    }
}

/// Denoiser state
struct DenoiserState {
    /// The frequency-domain GTCRN denoiser
    denoiser: GtcrnFrequencyDenoiser,
}

impl DenoiserState {
    fn new(_config: &DenoiserConfig) -> Self {
        let denoiser = GtcrnFrequencyDenoiser::new().expect("Failed to initialize denoiser");

        Self { denoiser }
    }
}

/// Audio denoiser - designed for streaming audio processing like AGC
pub struct Denoiser {
    config: DenoiserConfig,
    state: DenoiserState,
}

impl Denoiser {
    /// Create a new denoiser instance
    pub fn new(config: DenoiserConfig) -> Self {
        let state = DenoiserState::new(&config);
        Self { config, state }
    }

    /// Process audio samples in-place - high performance version
    pub fn process_inplace(&mut self, audio: &mut [f32]) -> Result<(), anyhow::Error> {
        if audio.is_empty() {
            return Ok(());
        }

        // If the denoiser is not enabled, do nothing
        if !self.config.enabled {
            return Ok(());
        }

        // Apply denoising using frequency-domain GTCRN
        let denoised_audio = self.state.denoiser.denoise_frame(audio)?;

        // Copy the denoised audio back to the original buffer
        // Only copy the first audio.len() samples to match the input frame size
        let copy_length = std::cmp::min(denoised_audio.len(), audio.len());
        audio[..copy_length].copy_from_slice(&denoised_audio[..copy_length]);

        Ok(())
    }

    /// Reset the denoiser state
    pub fn reset(&mut self) {
        self.state.denoiser.reset();
    }

    /// Enable or disable the denoiser
    pub fn set_enabled(&mut self, enabled: bool) {
        self.config.enabled = enabled;
    }

    /// Check if the denoiser is enabled
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

/// Legacy AudioProcessor for backward compatibility
pub struct AudioProcessor;

impl Default for AudioProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioProcessor {
    pub fn new() -> Self {
        Self
    }
}
