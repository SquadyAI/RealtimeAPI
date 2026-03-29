//! Real-time audio denoising library using GTCRN ONNX model

pub mod audio_processor;
pub mod gtcrn_frequency_denoiser;

// Export the frequency-domain GTCRN denoiser
pub use gtcrn_frequency_denoiser::GtcrnFrequencyDenoiser;
