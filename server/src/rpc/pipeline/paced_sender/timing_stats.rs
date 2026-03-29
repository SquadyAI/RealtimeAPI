//! TimingStats — pacing performance statistics.

use std::collections::VecDeque;
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct TimingStats {
    pub cumulative_error_us: i64,
    processing_delays: VecDeque<u64>,
    pub avg_processing_delay_us: u64,
    pub timing_variance: f64,
    pub last_actual_send_time: Option<Instant>,
    pub last_planned_send_time: Option<Instant>,
}

impl TimingStats {
    pub fn new() -> Self {
        Self {
            cumulative_error_us: 0,
            processing_delays: VecDeque::new(),
            avg_processing_delay_us: 0,
            timing_variance: 0.0,
            last_actual_send_time: None,
            last_planned_send_time: None,
        }
    }

    #[inline]
    pub fn update_processing_delay(&mut self, actual_delay_us: u64) {
        self.processing_delays.push_back(actual_delay_us);

        if self.processing_delays.len() > 10 {
            self.processing_delays.pop_front();
        }

        let n = self.processing_delays.len() as u64;
        if n == 1 {
            self.avg_processing_delay_us = actual_delay_us;
            self.timing_variance = 0.0;
        } else {
            // Welford's algorithm
            let old_avg = self.avg_processing_delay_us as f64;
            let new_avg = old_avg + (actual_delay_us as f64 - old_avg) / n as f64;
            self.avg_processing_delay_us = new_avg as u64;

            let old_var = self.timing_variance;
            let new_val = actual_delay_us as f64;
            let new_var = old_var + ((new_val - old_avg) * (new_val - new_avg) - old_var) / n as f64;
            self.timing_variance = new_var.max(0.0);
        }
    }

    #[inline]
    pub fn update_cumulative_error(&mut self, planned_time: Instant, actual_time: Instant) {
        let error_us = (actual_time - planned_time).as_micros() as i64;
        self.cumulative_error_us += error_us;
        self.cumulative_error_us = self.cumulative_error_us.clamp(-5_000, 5_000); // ±5ms

        self.last_planned_send_time = Some(planned_time);
        self.last_actual_send_time = Some(actual_time);
    }

    pub fn get_stability_report(&self) -> String {
        format!(
            "累积误差: {}μs, 平均处理延迟: {}μs, 方差: {:.1}μs",
            self.cumulative_error_us, self.avg_processing_delay_us, self.timing_variance
        )
    }
}
