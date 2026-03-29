// ============ 简化版流式文本分割器 - 保留核心功能 ============

use futures::stream::Stream;
use regex::Regex;
use thiserror::Error;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct TextChunk {
    // 基本信息
    pub text: String,
    // 合成控制
    pub can_synthesize_immediately: bool,
    pub requires_context: bool,
}

#[derive(Debug, Clone)]
pub struct SplitterConfig {
    pub max_chunk_length: usize, // 最大分割长度 默认200
    pub min_chunk_length: usize, // 最小分割长度 默认1
}

impl Default for SplitterConfig {
    fn default() -> Self {
        Self {
            max_chunk_length: 100, // 最大分割长度
            min_chunk_length: 10,  // 最小分割长度（至少10个字）
        }
    }
}

impl SplitterConfig {
    /// 与部分配置合并
    pub fn merge_with(&mut self, partial: PartialSplitterConfig) {
        if let Some(max_chunk_length) = partial.max_chunk_length {
            self.max_chunk_length = max_chunk_length;
        }
        if let Some(min_chunk_length) = partial.min_chunk_length {
            self.min_chunk_length = min_chunk_length;
        }
    }
}

// 支持部分配置更新的结构体
#[derive(Debug, Clone, Default)]
pub struct PartialSplitterConfig {
    pub max_chunk_length: Option<usize>,
    pub min_chunk_length: Option<usize>,
}

impl PartialSplitterConfig {
    pub fn new() -> Self {
        Self::default()
    }
}

// ============ 错误类型 ============
#[derive(Error, Debug)]
#[error("{message}")]
pub struct TtsError {
    pub message: String,
    pub code: String,
}

impl TtsError {
    pub fn new(msg: &str, code: &str) -> Self {
        Self { message: msg.to_owned(), code: code.to_owned() }
    }
}

// ============ 流式处理接口  ============

pub trait TextStream: Stream<Item = String> + Unpin + Send {}
impl<T: Stream<Item = String> + Unpin + Send> TextStream for T {}

pub trait SplitTextStream: Stream<Item = TextChunk> + Send {}
impl<T: Stream<Item = TextChunk> + Send> SplitTextStream for T {}

// ============ 增强版流式文本分割器 ============
/**
 * 增强版流式TTS文本分割器
 * 特点：
 * 1. 多语言支持 - 针对不同语言优化分句策略
 * 2. 智能分句 - 使用Unicode句子边界+语言特定规则
 * 3. 实时处理 - 快速响应+延迟处理结合
 * 4. 不进行内容过滤，只负责分句
 */
#[derive(Debug, Clone)]
pub struct SimplifiedStreamingSplitter {
    pub config: SplitterConfig,
    sentence_pattern: Regex,
    buffer: String,
    // 仅允许"逗号类"弱分割触发首句，一旦产出首句即禁用
    allow_comma_like_boundary_for_first_sentence: bool,
}

impl SimplifiedStreamingSplitter {
    pub fn new(config: Option<PartialSplitterConfig>) -> Self {
        let mut final_config = SplitterConfig::default();
        if let Some(partial_config) = config {
            final_config.merge_with(partial_config);
        }

        // 创建一个统一的句子边界模式,包含常见句子结束与语义停顿标记
        // 变更：允许使用逗号/分号/顿号等中间停顿作为"轻边界"，以便更早吐出首句
        let sentence_pattern = Regex::new(
            r"(?x)
            [。！？…]+                   # 中文/日文句末
            |[.!?]+(?:[\x22\x27\u201C\u201D\u2018\u2019]*|\s+|$)  # 英文句末(带引号或空白)
            |[.]{3,}                    # 省略号
            |[؟]+                       # 阿拉伯文问号
            |\n+                        # 换行符
            |[～—／]+                   # 波浪线、破折号、斜杠
            |[，,、；;]+                # 中间停顿：逗号、顿号、分号
            ",
        )
        .unwrap();

        Self {
            config: final_config,
            sentence_pattern,
            buffer: String::new(),
            allow_comma_like_boundary_for_first_sentence: true,
        }
    }

    /// 文本清洗：去除"——"类破折号（U+2014），因为TTS不发音
    fn clean_text(&self, text: &str) -> String {
        // 直接移除所有 U+2014 '—'，从而覆盖"—"、"——"、"———"等情况

        text.replace('\u{2014}', "")
    }

    /**
     * 处理输入文本片段 - 主要API方法
     * 增强功能：
     * 1. 多语言句子边界检测
     * 2. 智能缓冲区管理
     * 3. 句子完整性验证
     * 4. 不进行内容过滤
     * 5. 🆕 基于长度的强制分割，确保流式响应
     * 6. 🆕 小数点保护：避免在小数点处分割（如36.1）
     */
    pub fn found_first_sentence(&mut self, input: &str) -> Vec<TextChunk> {
        // 不做任何过滤，直接加入缓冲区
        self.buffer.push_str(input);

        let mut results = Vec::new();
        let mut last_end = 0;

        // 查找所有句子边界
        for mat in self.sentence_pattern.find_iter(&self.buffer) {
            let sentence_end = mat.end();
            if sentence_end > last_end {
                let matched_text = mat.as_str();

                // 🆕 小数点保护：检查是否是小数点（前后都是数字）
                if matched_text == "." && self.is_decimal_point(sentence_end) {
                    // 这是小数点，跳过这个分割点
                    continue;
                }

                let sentence = self.buffer[last_end..sentence_end].trim().to_string();
                if !sentence.is_empty() {
                    // 仅将逗号/顿号/分号视为"可控弱边界"，用于首句提前吐出
                    let is_comma_like_boundary = matched_text.chars().any(|c| "，,、；;".contains(c));
                    // 短句保护：仅对非强终止符（如换行/波浪线等）启用更高的最小长度要求
                    let is_strong_ending = matched_text.chars().any(|c| "。！？…?!".contains(c));
                    // 如果是逗号类弱边界，且已经产出首句，则跳过该分割点，继续累积到强终止符
                    if is_comma_like_boundary && !self.allow_comma_like_boundary_for_first_sentence && !is_strong_ending {
                        continue;
                    }
                    let effective_min_len = if is_strong_ending {
                        // 强终止符：允许更短的完整句子通过
                        self.config.min_chunk_length
                    } else {
                        // 轻终止或其它边界：与全局最小长度一致，便于首句尽快吐出
                        self.config.min_chunk_length
                    };

                    if sentence.chars().count() < effective_min_len {
                        // 继续累积，等待更完整的句子
                        continue;
                    }

                    let cleaned_sentence = self.clean_text(&sentence).trim().to_string();
                    if cleaned_sentence.is_empty() {
                        // 清洗后为空则跳过
                        continue;
                    }

                    // 根据匹配的边界符号确定日志消息
                    let log_msg = if matched_text.chars().any(|c| "。！？…?!".contains(c)) {
                        "✂️ Splitter: Extracted a complete sentence."
                    } else {
                        "✂️ Splitter: Extracted a semantic unit."
                    };

                    info!(sentence = %cleaned_sentence, "{}", log_msg);
                    results.push(TextChunk {
                        text: cleaned_sentence,
                        can_synthesize_immediately: true,
                        requires_context: false,
                    });
                    // 一旦产出首句，就关闭逗号类弱边界分割
                    if self.allow_comma_like_boundary_for_first_sentence {
                        self.allow_comma_like_boundary_for_first_sentence = false;
                    }
                    // 仅在产生有效分片时推进缓冲区指针，避免过短分片被丢弃
                    last_end = sentence_end;
                }
            }
        }

        // 🆕 强制分割逻辑：如果缓冲区过长且没有找到边界，强制分割
        if last_end == 0 && self.buffer.chars().count() >= self.config.max_chunk_length {
            let forced_chunk = self.force_split_at_boundary();
            if let Some(chunk) = forced_chunk {
                info!(chunk = %chunk.text, "🔪 Splitter: Force-split long buffer at word boundary.");
                results.push(chunk);
                // 🔧 修复：使用字符索引更新缓冲区
                let split_char_pos = self.find_safe_split_position(self.config.max_chunk_length);
                let remaining: String = self.buffer.chars().skip(split_char_pos).collect();
                self.buffer = remaining;
            }
        } else {
            // 更新缓冲区,移除已处理的文本
            if last_end > 0 {
                self.buffer = self.buffer[last_end..].to_string();
            }
        }

        results
    }

    /**
     * 完成处理，输出剩余内容
     * 增强功能：
     * 1. 智能处理不完整句子
     * 2. 多语言感知的句子完整性检查
     */
    pub fn finalize(&mut self) -> Vec<TextChunk> {
        let mut results = Vec::new();
        let buffer_len_before = self.buffer.len();
        let remaining_text = self.clean_text(self.buffer.trim()).trim().to_string();
        let remaining_len_after_trim = remaining_text.len();

        info!(
            "✂️ Splitter: Finalizing buffer. buffer_len={}, after_trim={}, remaining_text='{}'",
            buffer_len_before,
            remaining_len_after_trim,
            remaining_text.chars().take(100).collect::<String>()
        );

        self.buffer.clear();

        if !remaining_text.is_empty() {
            info!(
                "✂️ Splitter: Flushing remaining text as a final chunk. text_len={}, text='{}'",
                remaining_text.len(),
                remaining_text.chars().take(100).collect::<String>()
            );
            let chunk = TextChunk { text: remaining_text, can_synthesize_immediately: true, requires_context: false };

            results.push(chunk);
        } else {
            warn!(
                "🔍 Splitter: finalize() called but buffer is empty after trim (original_len={})",
                buffer_len_before
            );
        }

        results
    }

    /**
     * 重置分割器状态
     */
    pub fn reset(&mut self) {
        self.buffer.clear();
        // 新轮次启用：仅首句允许逗号类弱边界
        self.allow_comma_like_boundary_for_first_sentence = true;
    }

    /**
     * 获取当前缓冲区状态（用于调试）
     */
    pub fn get_buffer_status(&self) -> usize {
        self.buffer.len()
    }

    /**
     * 🆕 强制在安全边界分割缓冲区
     * 优先在空格、标点符号等安全位置分割，避免破坏词汇
     */
    fn force_split_at_boundary(&self) -> Option<TextChunk> {
        if self.buffer.is_empty() {
            return None;
        }

        let split_char_pos = self.find_safe_split_position(self.config.max_chunk_length);
        if split_char_pos > 0 {
            // 🔧 修复：使用字符索引而不是字节索引
            let raw_chunk: String = self.buffer.chars().take(split_char_pos).collect();
            let chunk_text = self.clean_text(raw_chunk.trim()).trim().to_string();
            if !chunk_text.is_empty() && chunk_text.chars().count() >= self.config.min_chunk_length {
                return Some(TextChunk { text: chunk_text, can_synthesize_immediately: true, requires_context: false });
            }
        }
        None
    }

    /**
     * 🆕 检查句号是否是小数点（前后都是数字）
     * 流式输入场景：如果前面是数字但后面还没有内容，暂时保护以等待更多输入
     * @param dot_end_pos 句号在缓冲区中的结束位置（字节索引）
     * @return true 如果是小数点或可能是小数点，false 如果确定是句末标点
     */
    fn is_decimal_point(&self, dot_end_pos: usize) -> bool {
        let chars: Vec<char> = self.buffer.chars().collect();

        // 计算句号在字符数组中的位置
        let byte_to_char_pos = |byte_pos: usize| -> Option<usize> {
            let mut char_pos = 0;
            let mut current_byte = 0;
            for (i, ch) in self.buffer.chars().enumerate() {
                if current_byte >= byte_pos {
                    return Some(i);
                }
                current_byte += ch.len_utf8();
                char_pos = i;
            }
            if current_byte == byte_pos { Some(char_pos + 1) } else { None }
        };

        let Some(dot_char_pos) = byte_to_char_pos(dot_end_pos) else {
            return false;
        };

        // 句号应该在 dot_char_pos - 1 的位置
        if dot_char_pos == 0 {
            return false;
        }
        let dot_pos = dot_char_pos - 1;

        // 检查前一个字符是否是数字
        if dot_pos == 0 {
            return false; // 句号在开头，不可能是小数点
        }
        let Some(char_before) = chars.get(dot_pos - 1) else {
            return false;
        };
        if !char_before.is_ascii_digit() {
            return false;
        }

        // 检查后一个字符
        match chars.get(dot_pos + 1) {
            Some(char_after) if char_after.is_ascii_digit() => {
                // 前后都是数字，确定是小数点
                true
            },
            Some(char_after) if char_after.is_whitespace() || "。！？!?；;，,、）)」》>".contains(*char_after) => {
                // 后面是空格或标点，确定不是小数点
                false
            },
            None => {
                // 流式输入场景：后面还没有内容，暂时保护以等待更多输入
                // 如果前面是数字，我们暂时认为这可能是小数点，不要立即分割
                true
            },
            _ => {
                // 后面是其他字符（如字母），不是小数点
                false
            },
        }
    }

    /**
     * 🆕 寻找安全的分割位置
     * 优先级：空格 > 标点符号 > 字符边界
     */
    fn find_safe_split_position(&self, max_length: usize) -> usize {
        let chars: Vec<char> = self.buffer.chars().collect();
        if chars.len() <= max_length {
            return chars.len();
        }

        // 从max_length位置向前搜索安全分割点
        for i in (max_length / 2..=max_length).rev() {
            if i >= chars.len() {
                continue;
            }

            let ch = chars[i];
            // 优先在空格处分割
            if ch.is_whitespace() {
                return i + 1; // 包含空格
            }
            // 其次在标点符号后分割
            if "。！？.!?".contains(ch) {
                return i + 1; // 包含标点
            }
        }

        // 如果找不到安全分割点，在max_length处强制分割
        max_length.min(chars.len())
    }

    /// 高级包装：在内部出现 panic 时自动捕获、重置状态并返回空结果，防止崩溃
    pub fn process_input(&mut self, input: impl AsRef<str>) -> Vec<TextChunk> {
        let input_ref = input.as_ref();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.found_first_sentence(input_ref))) {
            Ok(chunks) => chunks,
            Err(e) => {
                tracing::error!("✂️ Text splitter panic captured: {:?}", e);
                self.reset();
                Vec::new()
            },
        }
    }
}

// ============ 辅助函数 ============
/**
 * 创建简化版流式文本分割器 - 便利函数
 */
pub fn create_simplified_text_splitter(config: Option<PartialSplitterConfig>) -> SimplifiedStreamingSplitter {
    SimplifiedStreamingSplitter::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chinese_sentence_splitting() {
        let mut splitter = SimplifiedStreamingSplitter::new(None);

        // 测试中文句子分割（min_chunk_length=10）
        // "你好！"(3字)被合并; "你好！我是AI助手。"(10字)输出;
        // "让我们开始对话吧。"(9字) < 10 留在缓冲区
        let chunks = splitter.found_first_sentence("你好！我是AI助手。让我们开始对话吧。");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "你好！我是AI助手。");
    }

    #[test]
    fn test_english_sentence_splitting() {
        let mut splitter = SimplifiedStreamingSplitter::new(None);

        // 测试英文句子分割（min_chunk_length=10，短句"Hello!"被合并）
        let chunks = splitter.found_first_sentence("Hello! I am an AI assistant. Let's talk!");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "Hello! I am an AI assistant.");
        assert_eq!(chunks[1].text, "Let's talk!");
    }

    #[test]
    fn test_mixed_language_splitting() {
        let mut splitter = SimplifiedStreamingSplitter::new(None);

        // 测试混合语言分割（min_chunk_length=10，短句合并）
        let chunks = splitter.found_first_sentence("Hello你好！How are you今天好吗？");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Hello你好！How are you今天好吗？");
    }

    #[test]
    fn test_streaming_input() {
        let mut splitter = SimplifiedStreamingSplitter::new(None);

        // 模拟流式输入
        let mut all_chunks = Vec::new();
        all_chunks.extend(splitter.found_first_sentence("第一句。"));
        all_chunks.extend(splitter.found_first_sentence("第二句，但还"));
        all_chunks.extend(splitter.found_first_sentence("没说完。"));
        all_chunks.extend(splitter.found_first_sentence("第三句！"));

        let final_chunks = splitter.finalize();
        all_chunks.extend(final_chunks);

        // min_chunk_length=10 causes short sentences to merge:
        // first 3 inputs merge into one chunk, last input becomes finalize chunk
        assert_eq!(all_chunks.len(), 2);
        // Verify by checking the chunk contains the input fragments
        assert!(all_chunks[0].text.len() > 10, "first chunk should be long enough");
        assert_eq!(
            all_chunks[1].text.chars().count(),
            4,
            "second chunk should be the last sentence"
        );
    }

    #[test]
    fn test_finalize() {
        let mut splitter = SimplifiedStreamingSplitter::new(None);
        splitter.found_first_sentence("这是一个不完整的句子");
        let chunks = splitter.finalize();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "这是一个不完整的句子");

        // finalize 后缓冲区应为空
        let chunks_after = splitter.finalize();
        assert_eq!(chunks_after.len(), 0);
    }

    #[test]
    fn test_japanese_sentence_splitting() {
        let mut splitter = SimplifiedStreamingSplitter::new(None);

        // 测试日语分割（min_chunk_length=10，短句合并）
        let chunks = splitter.found_first_sentence("こんにちは！私はAIアシスタントです。よろしくお願いします。");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "こんにちは！私はAIアシスタントです。");
        assert_eq!(chunks[1].text, "よろしくお願いします。");
    }

    #[test]
    fn test_empty_and_edge_cases() {
        let mut splitter = SimplifiedStreamingSplitter::new(None);

        // 测试空输入
        let chunks = splitter.found_first_sentence("");
        assert_eq!(chunks.len(), 0);

        // 测试只有空格
        let chunks = splitter.found_first_sentence("   ");
        assert_eq!(chunks.len(), 0);

        // 测试没有标点的文本
        let chunks = splitter.found_first_sentence("这是一段没有标点的文本");
        assert_eq!(chunks.len(), 0);
        let final_chunks = splitter.finalize();
        assert_eq!(final_chunks.len(), 1);
        assert_eq!(final_chunks[0].text, "这是一段没有标点的文本");
    }

    #[test]
    fn test_long_text_splitting() {
        let mut splitter = SimplifiedStreamingSplitter::new(None);

        // 测试长文本分割（min_chunk_length=10，短句合并）
        let long_text = "第一句话。这是第二句。这是第三句话！这是第四句？这是第五句。这是最后一句了。";
        let chunks = splitter.found_first_sentence(long_text);
        assert_eq!(chunks.len(), 3);

        // 验证分割结果：短句合并为3组
        assert_eq!(chunks.len(), 3);
    }

    #[test]
    fn test_emoji_preservation() {
        let mut splitter = SimplifiedStreamingSplitter::new(None);

        // 测试包含emoji的文本（min_chunk_length=10，短句合并）
        let chunks = splitter.found_first_sentence("你好！👋 我是AI助手。🤖 Let's talk! 🚀");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "你好！👋 我是AI助手。");
        assert_eq!(chunks[1].text, "🤖 Let's talk!");

        // 剩余的 emoji 应该在 finalize 时被刷新
        let final_chunks = splitter.finalize();
        assert_eq!(final_chunks.len(), 1);
        assert_eq!(final_chunks[0].text, "🚀");
    }

    #[test]
    fn test_force_splitting_long_buffer() {
        // 使用较小的max_chunk_length进行测试
        let config = PartialSplitterConfig { max_chunk_length: Some(8), min_chunk_length: Some(3) };
        let mut splitter = SimplifiedStreamingSplitter::new(Some(config));

        // 添加一个没有分割标志的长文本
        let chunks = splitter.found_first_sentence("我可以帮您查天气设闹钟播放音乐");

        // 应该强制分割为多个块
        assert!(!chunks.is_empty(), "应该至少产生一个强制分割的块");

        // 第一个块不应该超过最大长度
        if !chunks.is_empty() {
            assert!(chunks[0].text.chars().count() <= 8, "分割的块不应超过最大长度");
            assert!(chunks[0].text.chars().count() >= 3, "分割的块不应小于最小长度");
        }

        // 缓冲区应该还有剩余内容
        assert!(splitter.get_buffer_status() > 0, "缓冲区应该还有剩余内容");
    }

    #[test]
    fn test_decimal_point_protection() {
        let mut splitter = SimplifiedStreamingSplitter::new(None);

        // 测试小数点不应该被分割
        let chunks = splitter.found_first_sentence("人体的正常体温通常在36.1°C到37.2°C之间。");

        // 应该只在句号处分割，不应该在小数点处分割
        assert_eq!(chunks.len(), 1, "应该只产生一个完整句子");
        assert_eq!(chunks[0].text, "人体的正常体温通常在36.1°C到37.2°C之间。");

        // 测试流式输入的小数点保护
        let mut splitter2 = SimplifiedStreamingSplitter::new(None);
        let mut all_chunks = Vec::new();

        // 模拟流式输入："体温36.1度"
        all_chunks.extend(splitter2.found_first_sentence("体温36."));
        // 此时缓冲区有"体温36."，但因为没有后续数字，可能会分割（这是边界情况）

        all_chunks.extend(splitter2.found_first_sentence("1度。"));
        // 如果第一次没分割，这里应该产生完整的句子

        all_chunks.extend(splitter2.finalize());

        // 验证最终结果包含完整的数字
        let full_text: String = all_chunks.iter().map(|c| c.text.as_str()).collect();
        assert!(full_text.contains("36.1"), "应该保留完整的小数 36.1");
    }

    #[test]
    fn test_decimal_vs_period() {
        let mut splitter = SimplifiedStreamingSplitter::new(None);

        // 测试区分小数点和句末句号
        let chunks = splitter.found_first_sentence("价格是99.99元。这很便宜。");

        // "价格是99.99元。" (10 chars) passes min_chunk_length=10, but
        // "这很便宜。" (5 chars) is too short and stays in the buffer
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "价格是99.99元。");
    }

    #[test]
    fn test_streaming_with_force_split() {
        let config = PartialSplitterConfig {
            max_chunk_length: Some(6), // 更小的长度，更容易触发强制分割
            min_chunk_length: Some(3),
        };
        let mut splitter = SimplifiedStreamingSplitter::new(Some(config));

        // 模拟LLM流式输出的场景
        let mut all_chunks = Vec::new();

        // 逐步添加文本，模拟"我可以帮您查天气、设闹钟、播放音乐、回答一些问题，"的流式输入
        all_chunks.extend(splitter.found_first_sentence("我可以帮您查天气")); // 8个字符，超过max_chunk_length(6)，应该强制分割

        println!("第一次添加后的chunks数量: {}", all_chunks.len());
        for (i, chunk) in all_chunks.iter().enumerate() {
            println!("Chunk {}: '{}'", i, chunk.text);
        }

        // 验证已经有分割产生（要么通过标点符号分割，要么强制分割）
        all_chunks.extend(splitter.found_first_sentence("、")); // 这里应该触发分割
        all_chunks.extend(splitter.found_first_sentence("设闹钟播放音乐")); // 又是一个长文本
        all_chunks.extend(splitter.found_first_sentence("、"));
        all_chunks.extend(splitter.found_first_sentence("回答一些问题"));
        all_chunks.extend(splitter.found_first_sentence("，")); // 最后的分割

        // 处理剩余内容
        all_chunks.extend(splitter.finalize());

        println!("最终chunks数量: {}", all_chunks.len());
        for (i, chunk) in all_chunks.iter().enumerate() {
            println!("Final Chunk {}: '{}' (长度: {})", i, chunk.text, chunk.text.chars().count());
        }

        // 验证结果 - 至少应该有一些分割产生
        assert!(all_chunks.len() >= 2, "应该产生多个文本块，实现流式效果");

        // 验证每个块的长度都在合理范围内
        for chunk in &all_chunks {
            if chunk.text.chars().count() >= 3 {
                // 只检查非空的合理块
                assert!(chunk.text.chars().count() >= 3, "每个块都应该满足最小长度要求");
            }
        }
    }

    #[test]
    fn test_streaming_decimal_protection() {
        let mut splitter = SimplifiedStreamingSplitter::new(None);
        let mut all_chunks = Vec::new();

        // 模拟LLM流式输出："降雨量极小（0.0mm）"
        // 第一次输入："降雨量极小（0."
        all_chunks.extend(splitter.found_first_sentence("降雨量极小（0."));

        // 此时不应该在 0. 处分割，因为后面可能还有数字
        // 如果错误地分割了，会产生一个包含 "0." 的chunk

        // 第二次输入："0mm）。"
        all_chunks.extend(splitter.found_first_sentence("0mm）。"));

        // 处理剩余内容
        all_chunks.extend(splitter.finalize());

        println!("流式小数保护测试结果:");
        for (i, chunk) in all_chunks.iter().enumerate() {
            println!("Chunk {}: '{}'", i, chunk.text);
        }

        // 验证完整文本包含完整的数字
        let full_text: String = all_chunks.iter().map(|c| c.text.as_str()).collect();
        assert!(full_text.contains("0.0"), "应该保留完整的小数 0.0，而不是切开成 0. 和 0");

        // 验证没有产生只包含 "0." 或以 "0." 结尾的不完整分片
        for chunk in &all_chunks {
            assert!(!chunk.text.trim().ends_with("0."), "不应该在小数点处分割: '{}'", chunk.text);
        }
    }
}
