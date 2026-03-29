//! Public types for the paced audio sender.

use bytes::Bytes;

#[derive(Clone)]
pub struct PacedAudioChunk {
    pub audio_data: Bytes,
    pub is_final: bool,
    pub realtime_metadata: Option<RealtimeAudioMetadata>,
    pub sentence_text: Option<String>,
    pub turn_final: bool,
}

#[derive(Debug, Clone)]
pub struct RealtimeAudioMetadata {
    pub response_id: String,
    pub assistant_item_id: String,
    pub output_index: u32,
    pub content_index: u32,
}

/// Pacing parameter configuration (supports runtime hot-update).
#[derive(Debug, Clone)]
pub struct PacingConfig {
    pub send_rate_multiplier: f64,
    pub initial_burst_count: usize,
    pub initial_burst_delay_ms: u64,
}

/// Precision timing configuration.
#[derive(Debug, Clone)]
pub struct PrecisionTimingConfig {
    pub error_threshold_us: i64,
    pub max_processing_delay_us: u64,
}

impl Default for PrecisionTimingConfig {
    fn default() -> Self {
        Self {
            error_threshold_us: 1000,        // 1ms
            max_processing_delay_us: 50_000, // 50ms
        }
    }
}
