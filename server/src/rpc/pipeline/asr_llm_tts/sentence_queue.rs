// 句子队列管理模块
// 从 tts_task.rs 拆分出来，负责 TTS 合成任务的句子排队和状态追踪

use lingua::Language;
use std::collections::VecDeque;
use tracing::{debug, info};

// ============================================================================
// TextChunk 定义
// ============================================================================

/// 文本块（本地定义，从 volcengine_tts 包复制）
#[derive(Debug, Clone)]
pub struct TextChunk {
    pub text: String,
    pub can_synthesize_immediately: bool,
    pub requires_context: bool,
    /// 缓存的语言检测结果 (language, confidence)，避免重复检测
    pub language_confidences: Vec<(Language, f64)>,
}

/// 从缓存的语言检测结果中获取主语言（取置信度最高者）
pub fn get_top_language_from_cache(confidences: &[(Language, f64)]) -> Option<Language> {
    if confidences.is_empty() {
        return None;
    }
    // 缓存的结果已经是按置信度排序的
    Some(confidences[0].0)
}

// ============================================================================
// SentenceQueue 定义
// ============================================================================

/// 句子队列（待 TTS 合成）
pub struct SentenceQueue {
    /// 待处理的句子队列
    pending: VecDeque<TextChunk>,
    /// 当前正在 TTS 处理的句子索引
    current_processing: Option<usize>,
    /// 当前正在处理的句子文本（用于在首个音频分片时发送文字事件）
    current_processing_text: Option<String>,
    /// 当前正在处理句子的语言置信度缓存（用于文字规范化/转换）
    current_processing_confs: Option<Vec<(Language, f64)>>,
    /// 正在预取（TTS合成中但尚未播放）的句子
    inflight_sentence: Option<(usize, TextChunk)>,
    /// 是否已接收完所有句子（LLM 流结束）
    llm_complete: bool,
    /// 总句子计数（用于索引）
    total_count: usize,
}

impl Default for SentenceQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl SentenceQueue {
    pub fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            current_processing: None,
            current_processing_text: None,
            current_processing_confs: None,
            inflight_sentence: None,
            llm_complete: false,
            total_count: 0,
        }
    }

    /// 添加句子到队列
    pub fn push(&mut self, sentence: TextChunk) {
        self.total_count += 1;

        // 加入待处理队列
        self.pending.push_back(sentence);
        // debug!("📝 句子入队: idx={}, text='{}', queue_len={}", idx, sentence.text.chars().take(20).collect::<String>(), self.pending.len());
    }

    /// 获取下一个待处理的句子（不移除）
    pub fn peek_next(&self) -> Option<&TextChunk> {
        self.pending.front()
    }

    /// 标记当前句子为"正在处理"
    pub fn mark_processing(&mut self) -> Option<TextChunk> {
        if let Some(sentence) = self.pending.pop_front() {
            let idx = self.total_count - self.pending.len() - 1;
            self.current_processing = Some(idx);
            self.current_processing_text = Some(sentence.text.clone());
            self.current_processing_confs = Some(sentence.language_confidences.clone());

            info!(
                "🎯 句子开始处理: idx={}, text='{}', remaining={}",
                idx,
                sentence.text.chars().take(20).collect::<String>(),
                self.pending.len()
            );
            Some(sentence)
        } else {
            None
        }
    }

    /// 标记 LLM 流结束
    pub fn mark_llm_complete(&mut self) {
        self.llm_complete = true;
        info!("🏁 LLM 流结束，总句子数: {}", self.total_count);
    }

    /// 重置 LLM 完成状态（用于同声传译模式的新翻译任务开始）
    pub fn reset_llm_complete(&mut self) {
        self.llm_complete = false;
        info!("🔄 重置 LLM 完成状态（同声传译模式新任务）");
    }

    /// 标记当前正在处理的句子已完成音频
    pub fn mark_current_processing_complete(&mut self) {
        self.current_processing = None;
        self.current_processing_text = None;
        self.current_processing_confs = None;
    }

    /// 清空队列（用于打断）
    pub fn clear(&mut self) {
        let cleared = self.pending.len();
        let had_inflight = self.inflight_sentence.is_some();
        self.pending.clear();
        self.current_processing = None;
        self.current_processing_text = None;
        self.current_processing_confs = None;
        self.inflight_sentence = None; // 清空预取句子
        self.llm_complete = false; // 关键修复：打断时重置LLM完成状态，避免误判为轮次结束
        info!("🧹 清空句子队列: 丢弃 {} 个未处理句子, inflight={}", cleared, had_inflight);
    }

    /// 完全重置队列（用于新轮次开始）
    pub fn reset(&mut self) {
        self.pending.clear();
        self.current_processing = None;
        self.current_processing_text = None;
        self.current_processing_confs = None;
        self.inflight_sentence = None;
        self.llm_complete = false;
        self.total_count = 0; // 关键：重置计数器，确保新轮次从 idx=0 开始
        info!("🔄 句子队列已完全重置（新轮次）");
    }

    /// 获取队列长度
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// 检查队列是否为空
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn is_llm_complete(&self) -> bool {
        self.llm_complete
    }

    /// 启动预取：从队列取出下一句开始合成（不等待播放触发）
    pub fn start_prefetch(&mut self) -> Option<TextChunk> {
        if self.inflight_sentence.is_some() {
            debug!("⏭️ 已有预取句子，跳过重复预取");
            return None; // 已有inflight，不重复预取
        }

        if let Some(sentence) = self.pending.pop_front() {
            let idx = self.total_count - self.pending.len() - 1;
            info!(
                "🚀 预取句子: idx={}, text='{}', remaining={}",
                idx,
                sentence.text.chars().take(20).collect::<String>(),
                self.pending.len()
            );
            self.inflight_sentence = Some((idx, sentence.clone()));
            Some(sentence)
        } else {
            debug!("📭 队列为空，无法预取");
            None
        }
    }

    /// 消费预取的句子（触发时直接使用已合成好的）
    pub fn consume_inflight(&mut self) -> Option<TextChunk> {
        if let Some((idx, sentence)) = self.inflight_sentence.take() {
            self.current_processing = Some(idx);
            self.current_processing_text = Some(sentence.text.clone());
            self.current_processing_confs = Some(sentence.language_confidences.clone());
            info!(
                "⚡ 使用预取句子: idx={}, text='{}' (无延迟)",
                idx,
                sentence.text.chars().take(20).collect::<String>()
            );
            Some(sentence)
        } else {
            None
        }
    }

    /// 检查是否有预取句子
    pub fn has_inflight(&self) -> bool {
        self.inflight_sentence.is_some()
    }

    /// 检查是否有正在处理的句子
    pub fn is_processing(&self) -> bool {
        self.current_processing.is_some()
    }

    /// 获取总句子计数
    pub fn total_count(&self) -> usize {
        self.total_count
    }

    /// 获取当前正在处理的句子索引
    pub fn current_processing_idx(&self) -> Option<usize> {
        self.current_processing
    }
}

// ============================================================================
// NextSentenceTrigger 定义
// ============================================================================

/// 下一句触发信号（完全事务性，无时间预测）
#[derive(Debug, Clone)]
pub struct NextSentenceTrigger {
    /// 当前正在播放的句子索引
    pub current_sentence_idx: usize,
}
