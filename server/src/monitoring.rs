//! 监控指标模块
//!
//! 该模块负责收集和暴露系统的各项监控指标，包括：
//! - 系统内存在的session数量
//! - ASR/LLM/TTS成功率
//! - ASR/LLM/TTS资源池剩余量
//! - ASR尾音-完成延迟
//! - TTS首字-首音延迟
//! - LLM post - 首字延迟
//! - latency_reports 延迟报告指标

use once_cell::sync::Lazy;
use prometheus::{Gauge, Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, Opts, Registry};
use std::sync::Arc;
use tokio::sync::mpsc;

/// 全局Prometheus注册表
pub static REGISTRY: Lazy<Registry> = Lazy::new(Registry::new);

/// 全局监控指标集合
pub static METRICS: Lazy<Arc<Metrics>> = Lazy::new(|| {
    let metrics = Arc::new(Metrics::new(&REGISTRY));
    // 注册指标
    metrics.register(&REGISTRY);
    metrics
});

/// 非阻塞指标更新器
pub struct MetricsUpdater {
    tx: mpsc::UnboundedSender<MetricUpdate>,
}

/// 指标更新事件
#[derive(Debug, Clone)]
pub enum MetricUpdate {
    ActiveSessions(i64),
    AsrSuccess,
    AsrFailure,
    TtsPoolRemaining(i64),
    LatencyReport {
        asr_final_ms: Option<i64>,
        llm_first_token_ms: Option<i64>,
        tts_first_audio_ms: Option<i64>,
        paced_first_audio_ms: Option<i64>,
    },
}

/// 监控指标集合
pub struct Metrics {
    /// 活跃会话数
    pub active_sessions: IntGauge,

    // 成功率指标
    /// ASR成功率
    pub asr_success_rate: Gauge,
    /// LLM成功率
    pub llm_success_rate: Gauge,
    /// TTS成功率
    pub tts_success_rate: Gauge,

    // 资源池剩余量指标
    /// ASR资源池剩余量
    pub asr_pool_remaining: IntGauge,
    /// TTS资源池剩余量
    pub tts_pool_remaining: IntGauge,

    // 延迟指标
    /// ASR尾音-完成延迟直方图
    pub asr_trailing_delay: Histogram,
    /// TTS首字-首音延迟直方图
    pub tts_first_byte_delay: Histogram,
    /// LLM post - 首字延迟直方图
    pub llm_post_first_token_delay: Histogram,

    // 🆕 latency_reports 延迟指标
    /// ASR最终延迟直方图（基于数据库数据）
    pub latency_asr_final: Histogram,
    /// LLM首字延迟直方图（基于数据库数据）
    pub latency_llm_first_token: Histogram,
    /// TTS首音延迟直方图（基于数据库数据）
    pub latency_tts_first_audio: Histogram,
    /// 分音首音延迟直方图（基于数据库数据）
    pub latency_paced_first_audio: Histogram,

    // 成功率计数器（用于计算成功率）
    /// ASR成功次数
    pub asr_success_total: IntCounter,
    /// ASR失败次数
    pub asr_failure_total: IntCounter,
    /// LLM成功次数
    pub llm_success_total: IntCounter,
    /// LLM失败次数
    pub llm_failure_total: IntCounter,
    /// TTS成功次数
    pub tts_success_total: IntCounter,
    /// TTS失败次数
    pub tts_failure_total: IntCounter,

    // 翻译管线指标
    /// 翻译请求总数
    pub translation_requests_total: IntCounter,
    /// 翻译延迟直方图
    pub translation_latency_seconds: Histogram,
    /// 翻译错误总数
    pub translation_errors_total: IntCounter,
    /// TTS语言fallback计数（按语言分类）
    pub tts_fallback_total: IntCounterVec,
}

impl Metrics {
    /// 创建新的监控指标集合
    pub fn new(_registry: &Registry) -> Self {
        let active_sessions = IntGauge::new("active_sessions", "Number of active sessions").expect("Failed to create active_sessions metric");

        // 成功率指标
        let asr_success_rate = Gauge::new("asr_success_rate", "ASR success rate").expect("Failed to create asr_success_rate metric");
        let llm_success_rate = Gauge::new("llm_success_rate", "LLM success rate").expect("Failed to create llm_success_rate metric");
        let tts_success_rate = Gauge::new("tts_success_rate", "TTS success rate").expect("Failed to create tts_success_rate metric");

        // 资源池剩余量指标
        let asr_pool_remaining = IntGauge::new("asr_pool_remaining", "ASR resource pool remaining count").expect("Failed to create asr_pool_remaining metric");
        let tts_pool_remaining = IntGauge::new("tts_pool_remaining", "TTS resource pool remaining count").expect("Failed to create tts_pool_remaining metric");

        // 延迟指标（实时采集）
        let asr_trailing_delay = Histogram::with_opts(HistogramOpts::new(
            "asr_trailing_delay_seconds",
            "ASR trailing sound completion delay",
        ))
        .expect("Failed to create asr_trailing_delay_seconds metric");

        let tts_first_byte_delay = Histogram::with_opts(HistogramOpts::new(
            "tts_first_byte_delay_seconds",
            "TTS first byte to first sound delay",
        ))
        .expect("Failed to create tts_first_byte_delay_seconds metric");

        let llm_post_first_token_delay = Histogram::with_opts(HistogramOpts::new(
            "llm_post_first_token_delay_seconds",
            "LLM post to first token delay",
        ))
        .expect("Failed to create llm_post_first_token_delay_seconds metric");

        // 🆕 latency_reports 延迟指标（基于数据库数据）
        let latency_asr_final = Histogram::with_opts(HistogramOpts::new(
            "latency_asr_final_ms",
            "ASR final latency from database reports (milliseconds)",
        ))
        .expect("Failed to create latency_asr_final_ms metric");

        let latency_llm_first_token = Histogram::with_opts(HistogramOpts::new(
            "latency_llm_first_token_ms",
            "LLM first token latency from database reports (milliseconds)",
        ))
        .expect("Failed to create latency_llm_first_token_ms metric");

        let latency_tts_first_audio = Histogram::with_opts(HistogramOpts::new(
            "latency_tts_first_audio_ms",
            "TTS first audio latency from database reports (milliseconds)",
        ))
        .expect("Failed to create latency_tts_first_audio_ms metric");

        let latency_paced_first_audio = Histogram::with_opts(HistogramOpts::new(
            "latency_paced_first_audio_ms",
            "Paced first audio latency from database reports (milliseconds)",
        ))
        .expect("Failed to create latency_paced_first_audio_ms metric");

        // 成功率计数器
        let asr_success_total = IntCounter::new("asr_success_total", "Total number of ASR successes").expect("Failed to create asr_success_total metric");
        let asr_failure_total = IntCounter::new("asr_failure_total", "Total number of ASR failures").expect("Failed to create asr_failure_total metric");

        let llm_success_total = IntCounter::new("llm_success_total", "Total number of LLM successes").expect("Failed to create llm_success_total metric");
        let llm_failure_total = IntCounter::new("llm_failure_total", "Total number of LLM failures").expect("Failed to create llm_failure_total metric");

        let tts_success_total = IntCounter::new("tts_success_total", "Total number of TTS successes").expect("Failed to create tts_success_total metric");
        let tts_failure_total = IntCounter::new("tts_failure_total", "Total number of TTS failures").expect("Failed to create tts_failure_total metric");

        // 翻译管线指标
        let translation_requests_total = IntCounter::new("translation_requests_total", "Total number of translation requests").expect("Failed to create translation_requests_total metric");

        let translation_latency_seconds = Histogram::with_opts(HistogramOpts::new(
            "translation_latency_seconds",
            "Translation latency in seconds",
        ))
        .expect("Failed to create translation_latency_seconds metric");

        let translation_errors_total = IntCounter::new("translation_errors_total", "Total number of translation errors").expect("Failed to create translation_errors_total metric");

        let tts_fallback_total = IntCounterVec::new(
            Opts::new("tts_fallback_total", "Total TTS language fallback count by language"),
            &["language"],
        )
        .expect("Failed to create tts_fallback_total metric");

        Self {
            active_sessions,
            asr_success_rate,
            llm_success_rate,
            tts_success_rate,
            asr_pool_remaining,
            tts_pool_remaining,
            asr_trailing_delay,
            tts_first_byte_delay,
            llm_post_first_token_delay,
            latency_asr_final,
            latency_llm_first_token,
            latency_tts_first_audio,
            latency_paced_first_audio,
            asr_success_total,
            asr_failure_total,
            llm_success_total,
            llm_failure_total,
            tts_success_total,
            tts_failure_total,
            translation_requests_total,
            translation_latency_seconds,
            translation_errors_total,
            tts_fallback_total,
        }
    }

    /// 注册所有监控指标到Prometheus注册表
    pub fn register(&self, registry: &Registry) {
        registry.register(Box::new(self.active_sessions.clone())).ok();

        registry.register(Box::new(self.asr_success_rate.clone())).ok();
        registry.register(Box::new(self.llm_success_rate.clone())).ok();
        registry.register(Box::new(self.tts_success_rate.clone())).ok();

        registry.register(Box::new(self.asr_pool_remaining.clone())).ok();
        registry.register(Box::new(self.tts_pool_remaining.clone())).ok();

        registry.register(Box::new(self.asr_trailing_delay.clone())).ok();
        registry.register(Box::new(self.tts_first_byte_delay.clone())).ok();
        registry.register(Box::new(self.llm_post_first_token_delay.clone())).ok();

        registry.register(Box::new(self.latency_asr_final.clone())).ok();
        registry.register(Box::new(self.latency_llm_first_token.clone())).ok();
        registry.register(Box::new(self.latency_tts_first_audio.clone())).ok();
        registry.register(Box::new(self.latency_paced_first_audio.clone())).ok();

        registry.register(Box::new(self.asr_success_total.clone())).ok();
        registry.register(Box::new(self.asr_failure_total.clone())).ok();
        registry.register(Box::new(self.llm_success_total.clone())).ok();
        registry.register(Box::new(self.llm_failure_total.clone())).ok();
        registry.register(Box::new(self.tts_success_total.clone())).ok();
        registry.register(Box::new(self.tts_failure_total.clone())).ok();

        // 翻译管线指标
        registry.register(Box::new(self.translation_requests_total.clone())).ok();
        registry.register(Box::new(self.translation_latency_seconds.clone())).ok();
        registry.register(Box::new(self.translation_errors_total.clone())).ok();
        registry.register(Box::new(self.tts_fallback_total.clone())).ok();
    }

    /// 更新ASR成功率
    pub fn update_asr_success_rate(&self) {
        let success = self.asr_success_total.get();
        let failure = self.asr_failure_total.get();
        let total = success + failure;

        if total > 0 {
            let rate = success as f64 / total as f64;
            self.asr_success_rate.set(rate);
        }
    }

    /// 更新LLM成功率
    pub fn update_llm_success_rate(&self) {
        let success = self.llm_success_total.get();
        let failure = self.llm_failure_total.get();
        let total = success + failure;

        if total > 0 {
            let rate = success as f64 / total as f64;
            self.llm_success_rate.set(rate);
        }
    }

    /// 更新TTS成功率
    pub fn update_tts_success_rate(&self) {
        let success = self.tts_success_total.get();
        let failure = self.tts_failure_total.get();
        let total = success + failure;

        if total > 0 {
            let rate = success as f64 / total as f64;
            self.tts_success_rate.set(rate);
        }
    }

    /// 处理非阻塞指标更新
    fn process_update(&self, update: MetricUpdate) {
        match update {
            MetricUpdate::ActiveSessions(count) => {
                self.active_sessions.set(count);
            },
            MetricUpdate::AsrSuccess => {
                self.asr_success_total.inc();
                self.update_asr_success_rate();
            },
            MetricUpdate::AsrFailure => {
                self.asr_failure_total.inc();
                self.update_asr_success_rate();
            },
            MetricUpdate::TtsPoolRemaining(count) => {
                self.tts_pool_remaining.set(count);
            },
            MetricUpdate::LatencyReport { asr_final_ms, llm_first_token_ms, tts_first_audio_ms, paced_first_audio_ms } => {
                if let Some(ms) = asr_final_ms {
                    self.latency_asr_final.observe(ms as f64);
                }
                if let Some(ms) = llm_first_token_ms {
                    self.latency_llm_first_token.observe(ms as f64);
                }
                if let Some(ms) = tts_first_audio_ms {
                    self.latency_tts_first_audio.observe(ms as f64);
                }
                if let Some(ms) = paced_first_audio_ms {
                    self.latency_paced_first_audio.observe(ms as f64);
                }
            },
        }
    }
}

impl Default for MetricsUpdater {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsUpdater {
    /// 创建新的非阻塞指标更新器
    pub fn new() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel();

        // 启动后台指标更新任务
        tokio::spawn(async move {
            while let Some(update) = rx.recv().await {
                METRICS.process_update(update);
            }
        });

        Self { tx }
    }

    /// 非阻塞发送指标更新
    pub fn send_update(&self, update: MetricUpdate) {
        // 使用try_send避免阻塞，如果队列满了就丢弃更新
        let _ = self.tx.send(update);
    }
}

/// 全局非阻塞指标更新器
pub static METRICS_UPDATER: Lazy<MetricsUpdater> = Lazy::new(MetricsUpdater::new);

/// 非阻塞更新活跃会话数
pub fn update_active_sessions(count: i64) {
    METRICS_UPDATER.send_update(MetricUpdate::ActiveSessions(count));
}

/// 非阻塞记录ASR成功
pub fn record_asr_success() {
    METRICS_UPDATER.send_update(MetricUpdate::AsrSuccess);
}

/// 非阻塞记录ASR失败
pub fn record_asr_failure() {
    METRICS_UPDATER.send_update(MetricUpdate::AsrFailure);
}

/// 非阻塞更新TTS资源池剩余量
pub fn update_tts_pool_remaining(count: i64) {
    METRICS_UPDATER.send_update(MetricUpdate::TtsPoolRemaining(count));
}

/// 非阻塞记录延迟报告
pub fn record_latency_report(asr_final_ms: Option<i64>, llm_first_token_ms: Option<i64>, tts_first_audio_ms: Option<i64>, paced_first_audio_ms: Option<i64>) {
    METRICS_UPDATER.send_update(MetricUpdate::LatencyReport { asr_final_ms, llm_first_token_ms, tts_first_audio_ms, paced_first_audio_ms });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_creation() {
        let _metrics = Metrics::new(&REGISTRY);
        // 简单测试确保指标创建没有panic (reaching this point means success)
    }

    #[test]
    fn test_success_rate_calculation() {
        let metrics = Metrics::new(&REGISTRY);

        // 模拟一些成功和失败
        metrics.asr_success_total.inc_by(80);
        metrics.asr_failure_total.inc_by(20);
        metrics.update_asr_success_rate();

        let rate = metrics.asr_success_rate.get();
        assert!((rate - 0.8).abs() < 0.001);
    }
}
