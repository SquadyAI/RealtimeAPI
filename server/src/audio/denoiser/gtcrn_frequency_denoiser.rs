//! Frequency-domain GTCRN denoiser with proper STFT/iSTFT processing
//!
//! This is the CORRECT implementation for GTCRN streaming inference:
//! - Input: 256 audio samples per frame (hop_length)
//! - Process: STFT -> GTCRN inference -> iSTFT
//! - STFT params: n_fft=512, hop_length=256, win_length=512

use ndarray::prelude::*;
use ort::execution_providers::CPUExecutionProvider;
use ort::io_binding::IoBinding;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;
use rustfft::num_complex::Complex32 as RComplex;
use rustfft::{Fft, FftPlanner};
use std::f32::consts::PI;
use std::fmt;
use std::sync::Arc;

// STFT parameters (must match Python implementation)
const N_FFT: usize = 512;
const HOP_LENGTH: usize = 256;
const WIN_LENGTH: usize = 512;
const N_FREQS: usize = N_FFT / 2 + 1; // 257

// Embed ONNX model for zero-deploy runtime dependency
pub static MODEL_DATA: &[u8] = include_bytes!("gtcrn_simple.onnx");

pub struct GtcrnFrequencyDenoiser {
    session: Session,
    // Model state caches
    conv_cache: Array5<f32>,  // (2, 1, 16, 16, 33)
    tra_cache: Array5<f32>,   // (2, 3, 1, 1, 16)
    inter_cache: Array4<f32>, // (2, 1, 33, 16)
    // STFT/iSTFT state
    window: Array1<f32>,        // Hann window
    audio_buffer: Vec<f32>,     // Input audio buffer
    stft_buffer: Vec<f32>,      // Previous audio for STFT overlap
    istft_buffer: Vec<Complex>, // Overlap-add buffer for iSTFT
    istft_norm: Vec<f32>,       // Window-sum-of-squares buffer for iSTFT normalization
    frame_count: usize,         // Track frame number for iSTFT
    // Performance optimization: reuse IoBinding across inferences
    // Reference: https://ort.pyke.io/perf/io-binding
    // "You'll generally want to create one binding per 'request'"
    // In our case, one audio stream = one request, so we reuse the binding
    io_binding: Option<IoBinding>,
    // FFT plans
    fft_forward: Arc<dyn Fft<f32> + Send + Sync>,
    fft_inverse: Arc<dyn Fft<f32> + Send + Sync>,
}

impl fmt::Debug for GtcrnFrequencyDenoiser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GtcrnFrequencyDenoiser")
            .field("session", &"onnx_session")
            .field("frame_count", &self.frame_count)
            .finish()
    }
}

#[derive(Clone, Copy, Debug)]
struct Complex {
    real: f32,
    imag: f32,
}

impl Complex {
    fn new(real: f32, imag: f32) -> Self {
        Self { real, imag }
    }

    fn zero() -> Self {
        Self { real: 0.0, imag: 0.0 }
    }
}

impl GtcrnFrequencyDenoiser {
    pub fn new() -> Result<Self, anyhow::Error> {
        // Initialize ONNX session
        let cpu_provider = CPUExecutionProvider::default();
        let providers = vec![cpu_provider.build()];

        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_log_level(ort::logging::LogLevel::Warning)?
            .with_execution_providers(providers)?
            .with_parallel_execution(false)?
            .with_intra_threads(1)?
            .with_memory_pattern(true)?
            .commit_from_memory(MODEL_DATA)?;

        // Initialize model state caches (all zeros initially)
        let conv_cache = Array5::<f32>::zeros((2, 1, 16, 16, 33));
        let tra_cache = Array5::<f32>::zeros((2, 3, 1, 1, 16));
        let inter_cache = Array4::<f32>::zeros((2, 1, 33, 16));

        // Create Hann window: w[n] = 0.5 * (1 - cos(2*pi*n/(N-1)))^0.5
        // This matches torch.hann_window(512).pow(0.5)
        let window = Array1::from_iter((0..WIN_LENGTH).map(|n| {
            let w = 0.5 * (1.0 - ((2.0 * PI * n as f32) / (WIN_LENGTH - 1) as f32).cos());
            w.sqrt()
        }));

        // Create a reusable IoBinding and pre-bind outputs to device
        let mut io_binding = session.create_binding()?;
        let mem_info = session.allocator().memory_info();
        io_binding.bind_output_to_device("enh", &mem_info)?;
        io_binding.bind_output_to_device("conv_cache_out", &mem_info)?;
        io_binding.bind_output_to_device("tra_cache_out", &mem_info)?;
        io_binding.bind_output_to_device("inter_cache_out", &mem_info)?;

        Ok(Self {
            session,
            conv_cache,
            tra_cache,
            inter_cache,
            window,
            audio_buffer: Vec::new(),
            stft_buffer: vec![0.0; WIN_LENGTH],
            istft_buffer: vec![Complex::zero(); WIN_LENGTH],
            istft_norm: vec![0.0; WIN_LENGTH],
            frame_count: 0,
            io_binding: Some(io_binding),
            fft_forward: {
                let mut planner = FftPlanner::<f32>::new();
                planner.plan_fft_forward(N_FFT)
            },
            fft_inverse: {
                let mut planner = FftPlanner::<f32>::new();
                planner.plan_fft_inverse(N_FFT)
            },
        })
    }

    /// Reset all streaming states
    pub fn reset(&mut self) {
        self.conv_cache.fill(0.0);
        self.tra_cache.fill(0.0);
        self.inter_cache.fill(0.0);
        self.audio_buffer.clear();
        self.stft_buffer.fill(0.0);
        self.istft_buffer.fill(Complex::zero());
        self.istft_norm.fill(0.0);
        self.frame_count = 0;
        // Keep io_binding for reuse (it will reset itself on next run)
    }

    /// Process audio frame in streaming mode
    ///
    /// Input: audio samples (will be accumulated)
    /// Output: denoised audio samples
    pub fn denoise_frame(&mut self, audio_frame: &[f32]) -> Result<Vec<f32>, anyhow::Error> {
        // Accumulate input
        self.audio_buffer.extend_from_slice(audio_frame);

        let mut output = Vec::new();

        // Process in hop_length chunks
        while self.audio_buffer.len() >= HOP_LENGTH {
            // Extract one hop
            let hop: Vec<f32> = self.audio_buffer.drain(0..HOP_LENGTH).collect();

            // Process this frame
            let denoised_hop = self.process_one_hop(&hop)?;
            output.extend_from_slice(&denoised_hop);
        }

        Ok(output)
    }

    /// Process one hop (256 samples) through STFT -> GTCRN -> iSTFT
    fn process_one_hop(&mut self, hop: &[f32]) -> Result<Vec<f32>, anyhow::Error> {
        assert_eq!(hop.len(), HOP_LENGTH);

        // 1. Update STFT buffer (shift left by hop_length and append new samples)
        self.stft_buffer.copy_within(HOP_LENGTH.., 0);
        self.stft_buffer[WIN_LENGTH - HOP_LENGTH..].copy_from_slice(hop);

        // 2. Compute STFT for this frame
        let spec = self.compute_stft(&self.stft_buffer)?;

        // 3. Run GTCRN inference
        let enhanced_spec = self.gtcrn_inference(&spec)?;

        // 4. Compute iSTFT
        let audio_out = self.compute_istft(&enhanced_spec)?;

        Ok(audio_out)
    }

    /// Compute STFT for one frame
    /// Input: audio window (512 samples)
    /// Output: complex spectrum (257, 1, 2) in format [B, F, T, 2]
    fn compute_stft(&self, audio: &[f32]) -> Result<Array4<f32>, anyhow::Error> {
        assert_eq!(audio.len(), WIN_LENGTH);

        // Apply window
        let windowed: Vec<f32> = audio.iter().zip(self.window.iter()).map(|(a, w)| a * w).collect();

        // Compute FFT
        let mut complex_input: Vec<Complex> = windowed.iter().map(|&x| Complex::new(x, 0.0)).collect();

        self.fft_512(&mut complex_input);

        // Extract positive frequencies (0 to N_FFT/2)
        // Format as (1, 257, 1, 2) - [batch, freq, time, complex]
        let mut spec = Array4::<f32>::zeros((1, N_FREQS, 1, 2));
        for i in 0..N_FREQS {
            spec[[0, i, 0, 0]] = complex_input[i].real;
            spec[[0, i, 0, 1]] = complex_input[i].imag;
        }

        Ok(spec)
    }

    /// Run GTCRN model inference
    ///
    /// Per https://ort.pyke.io/perf/io-binding: "create one binding per 'request'"
    /// For GTCRN, one audio stream = one request, so we reuse the IoBinding instance.
    /// Even though all inputs change each inference, reusing IoBinding reduces allocation overhead.
    fn gtcrn_inference(&mut self, spec: &Array4<f32>) -> Result<Array4<f32>, anyhow::Error> {
        // Reuse persistent IoBinding; only rebind inputs for this frame
        let binding = self
            .io_binding
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("io_binding not initialized"))?;
        binding.bind_input("mix", &Tensor::from_array(spec.view().to_owned())?)?;
        binding.bind_input("conv_cache", &Tensor::from_array(self.conv_cache.view().to_owned())?)?;
        binding.bind_input("tra_cache", &Tensor::from_array(self.tra_cache.view().to_owned())?)?;
        binding.bind_input("inter_cache", &Tensor::from_array(self.inter_cache.view().to_owned())?)?;

        let outputs = self.session.run_binding(binding)?;

        // Extract enhanced spectrum (minimize copies)
        let enh = outputs.get("enh").ok_or_else(|| anyhow::anyhow!("enh output not found"))?;
        let (_, enh_data) = enh.try_extract_tensor::<f32>()?;
        let enhanced = Array4::from_shape_vec((1, N_FREQS, 1, 2), enh_data.to_vec())?;

        // Update state caches (minimize copies)
        let conv_out = outputs
            .get("conv_cache_out")
            .ok_or_else(|| anyhow::anyhow!("conv_cache_out not found"))?;
        let (_, conv_data) = conv_out.try_extract_tensor::<f32>()?;
        self.conv_cache = Array5::from_shape_vec((2, 1, 16, 16, 33), conv_data.to_vec())?;

        let tra_out = outputs
            .get("tra_cache_out")
            .ok_or_else(|| anyhow::anyhow!("tra_cache_out not found"))?;
        let (_, tra_data) = tra_out.try_extract_tensor::<f32>()?;
        self.tra_cache = Array5::from_shape_vec((2, 3, 1, 1, 16), tra_data.to_vec())?;

        let inter_out = outputs
            .get("inter_cache_out")
            .ok_or_else(|| anyhow::anyhow!("inter_cache_out not found"))?;
        let (_, inter_data) = inter_out.try_extract_tensor::<f32>()?;
        self.inter_cache = Array4::from_shape_vec((2, 1, 33, 16), inter_data.to_vec())?;

        Ok(enhanced)
    }

    /// Compute iSTFT with overlap-add
    fn compute_istft(&mut self, spec: &Array4<f32>) -> Result<Vec<f32>, anyhow::Error> {
        // Extract complex spectrum
        let mut complex_spec = vec![Complex::zero(); N_FFT];
        for i in 0..N_FREQS {
            complex_spec[i] = Complex::new(spec[[0, i, 0, 0]], spec[[0, i, 0, 1]]);
        }

        // Mirror to get negative frequencies (conjugate symmetry)
        for i in 1..N_FREQS - 1 {
            let idx = N_FFT - i;
            complex_spec[idx] = Complex::new(complex_spec[i].real, -complex_spec[i].imag);
        }

        // IFFT
        self.ifft_512(&mut complex_spec);

        // Apply window
        let mut frame = vec![0.0; WIN_LENGTH];
        for (i, frame_val) in frame.iter_mut().enumerate().take(WIN_LENGTH) {
            *frame_val = complex_spec[i].real * self.window[i];
        }

        // Overlap-add (OLA) with window-sum-of-squares normalization (PyTorch/Librosa compatible)
        // 1) Add current frame to buffer and accumulate normalization weights
        for (i, frame_val) in frame.iter().enumerate().take(WIN_LENGTH) {
            self.istft_buffer[i].real += frame_val;
            // accumulate squared synthesis window to normalization buffer
            self.istft_norm[i] += self.window[i] * self.window[i];
        }

        // 2) Output first HOP samples, normalized by window-sum-of-squares
        let mut output = vec![0.0; HOP_LENGTH];
        for (i, output_val) in output.iter_mut().enumerate().take(HOP_LENGTH) {
            let denom = self.istft_norm[i];
            if denom > 1e-8 {
                *output_val = self.istft_buffer[i].real / denom;
            } else {
                *output_val = self.istft_buffer[i].real;
            }
        }

        // 3) Shift buffers left by HOP_LENGTH
        self.istft_buffer.copy_within(HOP_LENGTH.., 0);
        self.istft_buffer[WIN_LENGTH - HOP_LENGTH..].fill(Complex::zero());
        self.istft_norm.copy_within(HOP_LENGTH.., 0);
        self.istft_norm[WIN_LENGTH - HOP_LENGTH..].fill(0.0);

        self.frame_count += 1;
        Ok(output)
    }

    /// Simple in-place FFT (Cooley-Tukey, radix-2, decimation-in-time)
    fn fft_512(&self, data: &mut [Complex]) {
        assert_eq!(data.len(), N_FFT);
        // Convert to rustfft complex buffer
        let mut buffer: Vec<RComplex> = data.iter().map(|c| RComplex { re: c.real, im: c.imag }).collect();
        // Execute plan
        self.fft_forward.process(&mut buffer);
        // Copy back
        for (dst, src) in data.iter_mut().zip(buffer.iter()) {
            dst.real = src.re;
            dst.imag = src.im;
        }
    }

    /// Simple in-place IFFT
    fn ifft_512(&self, data: &mut [Complex]) {
        assert_eq!(data.len(), N_FFT);
        // Convert to rustfft complex buffer
        let mut buffer: Vec<RComplex> = data.iter().map(|c| RComplex { re: c.real, im: c.imag }).collect();
        // Execute inverse plan (includes 1/N scaling semantics we apply manually below)
        self.fft_inverse.process(&mut buffer);
        // Apply scaling (rustfft inverse does not scale by default)
        let scale = 1.0 / (N_FFT as f32);
        for (dst, src) in data.iter_mut().zip(buffer.iter()) {
            dst.real = src.re * scale;
            dst.imag = src.im * scale;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_denoiser() {
        let denoiser = GtcrnFrequencyDenoiser::new();
        assert!(denoiser.is_ok());
    }

    #[test]
    fn test_process_frame() {
        let mut denoiser = GtcrnFrequencyDenoiser::new().unwrap();

        // Create 512 samples of test audio
        let audio: Vec<f32> = (0..512).map(|i| (i as f32 * 0.01).sin()).collect();

        // Process
        let result = denoiser.denoise_frame(&audio);
        assert!(result.is_ok());

        let output = result.unwrap();
        // Should output 256 samples (one hop) when processing 512 samples
        assert!(output.len() >= 256);
    }
}
