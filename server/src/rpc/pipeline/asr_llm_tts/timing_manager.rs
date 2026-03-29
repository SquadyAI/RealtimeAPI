//! 实时对话流水线计时管理器
//!
//! 记录从用户结束说话（VAD Speaking -> Silence）开始，到各个关键节点的时间差：
//! 1. ASR最终结果
//! 2. LLM发出请求
//! 3. LLM返回首分片文字
//! 4. text-splitter返回首分片
//! 5. TTS首个音频包
//! 6. pacedSender首个发出的音频包
//!
//! 同时收集Prometheus监控指标：
//! - ASR尾音-完成延迟
//! - TTS首字-首音延迟
//! - LLM post - 首字延迟

use crate::monitoring::METRICS;
use crate::storage::config::{LatencyRecord, enqueue_latency_report};
use chrono::Utc;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// 计时节点枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimingNode {
    VadTriggered,           // VAD Speaking -> Silence 触发（用户结束说话，开始处理）
    AsrFinalResult,         // ASR最终结果
    LlmRequestSent,         // LLM发出请求
    LlmFirstChunk,          // LLM返回首分片文字
    TextSplitterFirstChunk, // text-splitter返回首分片
    TtsFirstAudio,          // TTS首个音频包
    PacedSenderFirstAudio,  // pacedSender首个发出的音频包
}

impl TimingNode {
    pub fn as_str(&self) -> &'static str {
        match self {
            TimingNode::VadTriggered => "用户结束说话",
            TimingNode::AsrFinalResult => "ASR最终结果",
            TimingNode::LlmRequestSent => "LLM发出请求",
            TimingNode::LlmFirstChunk => "LLM返回首分片",
            TimingNode::TextSplitterFirstChunk => "Text-Splitter首分片",
            TimingNode::TtsFirstAudio => "TTS首个音频包",
            TimingNode::PacedSenderFirstAudio => "PacedSender首音频包",
        }
    }
}

/// 单个会话的计时数据
#[derive(Debug, Clone)]
pub struct SessionTiming {
    pub session_id: String,
    pub vad_trigger_time: Option<Instant>,
    pub node_times: FxHashMap<TimingNode, Instant>,
    pub completed: bool,
    pub turn_count: u32,        // 🆕 新增：轮次计数器
    pub report_generated: bool, // 🆕 新增：标记当前轮次是否已生成报告
}

impl SessionTiming {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            vad_trigger_time: None,
            node_times: FxHashMap::default(),
            completed: false,
            turn_count: 0,
            report_generated: false,
        }
    }

    /// 记录VAD触发时间（基准时间）- 现在在用户结束说话时记录
    pub fn record_vad_trigger(&mut self, time: Instant) {
        self.vad_trigger_time = Some(time);
        self.node_times.insert(TimingNode::VadTriggered, time);
        self.turn_count += 1; // 🆕 增加轮次计数
        info!(
            "⏱️ [{}] 基准时间已记录（用户结束说话） (轮次 {})",
            self.session_id, self.turn_count
        );
    }

    /// 记录节点时间
    pub fn record_node_time(&mut self, node: TimingNode, time: Instant) {
        // 🧯 报告已生成后停止累积，避免跨轮次混入
        if self.report_generated {
            debug!(
                "⏹️ [{}] 已生成报告(轮次 {}), 忽略后续节点: {}",
                self.session_id,
                self.turn_count,
                node.as_str()
            );
            return;
        }

        // 🆕 检查是否已经记录过该节点，避免重复记录
        if let std::collections::hash_map::Entry::Occupied(mut e) = self.node_times.entry(node) {
            // 如果已经记录过，只更新时间但不打印日志
            e.insert(time);
            return;
        }

        if let Some(vad_time) = self.vad_trigger_time {
            let duration = time.duration_since(vad_time);
            info!(
                "⏱️ [{}] {}: +{}ms (轮次 {})",
                self.session_id,
                node.as_str(),
                duration.as_millis(),
                self.turn_count
            );
        } else {
            warn!("⚠️ [{}] 尝试记录{}时间，但VAD触发时间未设置", self.session_id, node.as_str());
        }
        self.node_times.insert(node, time);

        // 🔧 添加Prometheus延迟指标更新
        if let Some(vad_time) = self.vad_trigger_time {
            let duration = time.duration_since(vad_time);
            let duration_secs = duration.as_secs_f64();

            match node {
                // ASR尾音-完成延迟 (ASR最终结果 - VAD触发)
                TimingNode::AsrFinalResult => {
                    METRICS.asr_trailing_delay.observe(duration_secs);
                },
                // LLM post - 首字延迟 (LLM返回首分片 - LLM发出请求)
                TimingNode::LlmFirstChunk => {
                    if let Some(llm_request_time) = self.node_times.get(&TimingNode::LlmRequestSent) {
                        let llm_duration = time.duration_since(*llm_request_time);
                        METRICS.llm_post_first_token_delay.observe(llm_duration.as_secs_f64());
                    }
                },
                // TTS首字-首音延迟 (TTS首个音频包 - LLM返回首分片)
                TimingNode::TtsFirstAudio => {
                    if let Some(llm_first_chunk_time) = self.node_times.get(&TimingNode::LlmFirstChunk) {
                        let tts_duration = time.duration_since(*llm_first_chunk_time);
                        METRICS.tts_first_byte_delay.observe(tts_duration.as_secs_f64());
                    }
                },
                _ => {},
            }
        }
    }

    /// 🆕 检查是否已经记录了关键里程碑，如果有则返回true，表示可以生成报告
    pub fn has_key_milestone(&self) -> bool {
        // 当记录了TTS首音或PacedSender首音时，认为达到了关键里程碑
        self.node_times.contains_key(&TimingNode::TtsFirstAudio) || self.node_times.contains_key(&TimingNode::PacedSenderFirstAudio)
    }

    /// 计算相对于VAD触发的时间差
    pub fn get_relative_time(&self, node: TimingNode) -> Option<Duration> {
        let node_time = self.node_times.get(&node)?;
        let vad_time = self.vad_trigger_time?;
        Some(node_time.duration_since(vad_time))
    }

    /// 生成计时报告
    pub fn generate_report(&self) -> Option<TimingReport> {
        let vad_time = self.vad_trigger_time?;

        let mut report = TimingReport {
            session_id: self.session_id.clone(),
            vad_trigger_time: vad_time,
            node_times: FxHashMap::default(),
            relative_times: FxHashMap::default(),
            turn_count: self.turn_count, // 🆕 添加轮次信息
        };

        for (node, time) in &self.node_times {
            report.node_times.insert(*node, *time);
            if *node != TimingNode::VadTriggered {
                report.relative_times.insert(*node, time.duration_since(vad_time));
            }
        }

        Some(report)
    }

    /// 🆕 新增：重置轮次数据（用于多轮对话）
    pub fn reset_for_new_turn(&mut self) {
        self.node_times.clear();
        self.vad_trigger_time = None;
        self.completed = false;
        self.report_generated = false; // 🆕 重置报告生成标志
        // 注意：不重置turn_count，因为它会在record_vad_trigger时自动递增
        info!(
            "🔄 [{}] 重置计时数据，准备新一轮对话 (当前轮次: {})",
            self.session_id, self.turn_count
        );
    }
}

/// 计时报告
#[derive(Debug, Clone)]
pub struct TimingReport {
    pub session_id: String,
    pub vad_trigger_time: Instant,
    pub node_times: FxHashMap<TimingNode, Instant>,
    pub relative_times: FxHashMap<TimingNode, Duration>,
    pub turn_count: u32, // 🆕 新增：轮次信息
}

impl TimingReport {
    /// 输出格式化的计时报告
    pub fn print_report(&self) {
        info!("📊 === 延迟分析报告 [{}] (轮次 {}) ===", self.session_id, self.turn_count);

        for node in [
            TimingNode::VadTriggered,
            TimingNode::AsrFinalResult,
            TimingNode::LlmRequestSent,
            TimingNode::LlmFirstChunk,
            TimingNode::TextSplitterFirstChunk,
            TimingNode::TtsFirstAudio,
            TimingNode::PacedSenderFirstAudio,
        ] {
            if let Some(relative_time) = self.relative_times.get(&node) {
                info!("⏱️ {}: +{}ms", node.as_str(), relative_time.as_millis());
            } else if self.node_times.contains_key(&node) {
                info!("⏱️ {}: +0ms (基准)", node.as_str());
            } else {
                info!("⏱️ {}: 未记录", node.as_str());
            }
        }

        // 计算关键延迟指标
        self.print_latency_analysis();
        info!("📊 === 延迟分析报告结束 (轮次 {}) ===", self.turn_count);
    }

    /// 计算并打印关键延迟指标
    fn print_latency_analysis(&self) {
        let mut analysis = Vec::new();

        // ASR延迟
        if let Some(asr_time) = self.relative_times.get(&TimingNode::AsrFinalResult) {
            analysis.push(format!("ASR识别延迟: {}ms", asr_time.as_millis()));
        }

        // LLM处理延迟
        if let (Some(llm_request), Some(llm_first)) = (
            self.relative_times.get(&TimingNode::LlmRequestSent),
            self.relative_times.get(&TimingNode::LlmFirstChunk),
        ) {
            let llm_processing = llm_first.as_millis() - llm_request.as_millis();
            analysis.push(format!("LLM处理延迟: {}ms", llm_processing));
        }

        // TTS生成延迟
        if let (Some(llm_first), Some(tts_first)) = (
            self.relative_times.get(&TimingNode::LlmFirstChunk),
            self.relative_times.get(&TimingNode::TtsFirstAudio),
        ) {
            let tts_generation = tts_first.as_millis() - llm_first.as_millis();
            analysis.push(format!("TTS生成延迟: {}ms", tts_generation));
        }

        // 端到端延迟
        if let Some(paced_first) = self.relative_times.get(&TimingNode::PacedSenderFirstAudio) {
            analysis.push(format!("端到端延迟: {}ms", paced_first.as_millis()));
        }

        if !analysis.is_empty() {
            info!("📈 关键延迟指标 (轮次 {}):", self.turn_count);
            for metric in analysis {
                info!("   {}", metric);
            }
        }
    }

    /// 转换为JSON格式
    pub fn to_json(&self) -> serde_json::Value {
        let mut relative_times_json = serde_json::Map::new();
        for (node, duration) in &self.relative_times {
            relative_times_json.insert(
                node.as_str().to_string(),
                serde_json::Value::Number(serde_json::Number::from(duration.as_millis() as i64)),
            );
        }

        serde_json::json!({
            "session_id": self.session_id,
            "turn_count": self.turn_count,
            "vad_trigger_time": self.vad_trigger_time.elapsed().as_millis(),
            "relative_times": relative_times_json,
        })
    }
}

/// 全局计时管理器
#[derive(Debug)]
pub struct TimingManager {
    sessions: Arc<RwLock<FxHashMap<String, SessionTiming>>>,
}

impl Default for TimingManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TimingManager {
    pub fn new() -> Self {
        Self { sessions: Arc::new(RwLock::new(FxHashMap::default())) }
    }

    /// 记录VAD触发时间
    pub async fn record_vad_trigger(&self, session_id: &str, time: Instant) {
        let mut sessions = self.sessions.write().await;
        let session_timing = sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionTiming::new(session_id.to_string()));
        session_timing.record_vad_trigger(time);
    }

    /// 记录节点时间
    pub async fn record_node_time(&self, session_id: &str, node: TimingNode, time: Instant) {
        let mut sessions = self.sessions.write().await;
        if let Some(session_timing) = sessions.get_mut(session_id) {
            session_timing.record_node_time(node, time);
        } else {
            // warn!("⚠️ 尝试为不存在的会话记录时间: {}", session_id);
        }
    }

    /// 🆕 记录节点时间并在达到关键里程碑时自动生成报告
    pub async fn record_node_time_and_try_report(&self, session_id: &str, node: TimingNode, time: Instant, response_id: Option<&str>) {
        let should_generate_report = {
            let mut sessions = self.sessions.write().await;
            if let Some(session_timing) = sessions.get_mut(session_id) {
                session_timing.record_node_time(node, time);

                // 检查是否达到关键里程碑且未生成报告
                let reached_milestone = matches!(node, TimingNode::TtsFirstAudio | TimingNode::PacedSenderFirstAudio);
                reached_milestone && !session_timing.report_generated && session_timing.has_key_milestone()
            } else {
                // warn!("⚠️ 尝试为不存在的会话记录时间: {}", session_id);
                false
            }
        };

        // 如果达到里程碑，生成报告
        if should_generate_report {
            if let Some(resp_id) = response_id {
                info!("📊 [{}] 达到关键里程碑 {:?}，自动生成延迟报告", session_id, node);
                self.generate_print_and_store(session_id, resp_id).await;
            } else {
                info!("📊 [{}] 达到关键里程碑 {:?}，但无response_id，仅打印报告", session_id, node);
                self.generate_and_print_report(session_id).await;
            }
        }
    }

    /// 获取会话计时数据
    pub async fn get_session_timing(&self, session_id: &str) -> Option<SessionTiming> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).cloned()
    }

    /// 🆕 新增：重置会话计时数据（用于多轮对话）
    pub async fn reset_session_timing(&self, session_id: &str) {
        let mut sessions = self.sessions.write().await;
        if let Some(session_timing) = sessions.get_mut(session_id) {
            session_timing.reset_for_new_turn();
        }
    }

    /// 生成并输出计时报告
    pub async fn generate_and_print_report(&self, session_id: &str) {
        if let Some(session_timing) = self.get_session_timing(session_id).await {
            if let Some(report) = session_timing.generate_report() {
                report.print_report();
            } else {
                warn!("⚠️ 无法生成计时报告，VAD触发时间未设置: {}", session_id);
            }
        } else {
            warn!("⚠️ 会话计时数据不存在: {}", session_id);
        }
    }

    /// 🆕 生成、打印并存储计时报告到PostgreSQL（按 session_id + response_id 为键）
    pub async fn generate_print_and_store(&self, session_id: &str, response_id: &str) {
        let mut sessions = self.sessions.write().await;
        if let Some(session_timing) = sessions.get_mut(session_id) {
            // 检查是否已经为当前轮次生成过报告
            if session_timing.report_generated {
                info!(
                    "📊 [{}] 当前轮次({})已生成报告，跳过重复生成",
                    session_id, session_timing.turn_count
                );
                return;
            }

            if let Some(report) = session_timing.generate_report() {
                // 标记已生成报告
                session_timing.report_generated = true;
                // 先打印
                report.print_report();

                // 计算各节点相对毫秒
                let asr_ms = report
                    .relative_times
                    .get(&TimingNode::AsrFinalResult)
                    .map(|d| d.as_millis() as i64);
                let llm_ms = report
                    .relative_times
                    .get(&TimingNode::LlmFirstChunk)
                    .map(|d| d.as_millis() as i64);
                let tts_ms = report
                    .relative_times
                    .get(&TimingNode::TtsFirstAudio)
                    .map(|d| d.as_millis() as i64);
                let paced_ms = report
                    .relative_times
                    .get(&TimingNode::PacedSenderFirstAudio)
                    .map(|d| d.as_millis() as i64);

                let record = LatencyRecord {
                    session_id: report.session_id.clone(),
                    response_id: response_id.to_string(),
                    turn_count: report.turn_count as i32,
                    asr_final_ms: asr_ms,
                    llm_first_token_ms: llm_ms,
                    tts_first_audio_ms: tts_ms,
                    paced_first_audio_ms: paced_ms,
                    created_at: Utc::now(),
                };

                // 非阻塞入队，失败则丢弃
                let _ = enqueue_latency_report(record.clone());

                // 🔧 同时更新Prometheus延迟指标
                crate::monitoring::record_latency_report(
                    record.asr_final_ms,
                    record.llm_first_token_ms,
                    record.tts_first_audio_ms,
                    record.paced_first_audio_ms,
                );
            } else {
                warn!("⚠️ 无法生成计时报告，VAD触发时间未设置: {}", session_id);
            }
        } else {
            warn!("⚠️ 会话计时数据不存在: {}", session_id);
        }
    }

    /// 清理会话计时数据
    pub async fn cleanup_session(&self, session_id: &str) {
        let mut sessions = self.sessions.write().await;

        if let Some(session_timing) = sessions.remove(session_id) {
            info!("🧹 清理会话计时数据: {}", session_id);
            if let Some(report) = session_timing.generate_report() {
                report.print_report();
            }
        }
    }
}

// 全局计时管理器实例
lazy_static::lazy_static! {
    pub static ref GLOBAL_TIMING_MANAGER: Arc<TimingManager> = Arc::new(TimingManager::new());
}

/// 便利函数：记录VAD触发时间
pub async fn record_vad_trigger(session_id: &str, time: Instant) {
    GLOBAL_TIMING_MANAGER.record_vad_trigger(session_id, time).await;
}

/// 便利函数：记录节点时间
pub async fn record_node_time(session_id: &str, node: TimingNode, time: Instant) {
    GLOBAL_TIMING_MANAGER.record_node_time(session_id, node, time).await;
}

/// 🆕 便利函数：记录节点时间并在达到关键里程碑时自动生成报告
pub async fn record_node_time_and_try_report(session_id: &str, node: TimingNode, time: Instant, response_id: Option<&str>) {
    GLOBAL_TIMING_MANAGER
        .record_node_time_and_try_report(session_id, node, time, response_id)
        .await;
}

/// 🆕 新增：重置会话计时数据（用于多轮对话）
pub async fn reset_session_timing(session_id: &str) {
    GLOBAL_TIMING_MANAGER.reset_session_timing(session_id).await;
}

/// 便利函数：生成并输出计时报告
pub async fn print_timing_report(session_id: &str) {
    GLOBAL_TIMING_MANAGER.generate_and_print_report(session_id).await;
}

/// 便利函数：清理会话计时数据
pub async fn cleanup_session_timing(session_id: &str) {
    GLOBAL_TIMING_MANAGER.cleanup_session(session_id).await;
}
