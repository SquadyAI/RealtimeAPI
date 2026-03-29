//! ASR 稳定化模块
//!
//! 实现前缀一致性投票算法，用于稳定化 ASR 输出。
//! 当某前缀连续 N 次保持不变时，认为它是稳定的，可以发送翻译。

use std::collections::VecDeque;
use tracing::debug;

/// ASR 稳定化器
///
/// 基于前缀一致性投票算法，判断 ASR 输出文本的稳定部分。
/// 只有当某前缀连续 N 次（stability_threshold）保持不变时，才认为它是稳定的。
#[derive(Debug, Clone)]
pub struct AsrStabilizer {
    /// 历史 ASR 结果队列（保留最近 N 次）
    history: VecDeque<String>,
    /// 已确认稳定并发送翻译的文本长度（字节）
    confirmed_length: usize,
    /// 稳定性阈值（连续 N 次不变才算稳定）
    stability_threshold: usize,
    /// 最大历史记录数
    max_history: usize,
    /// 最小稳定长度（语义单位数）
    min_stable_units: usize,
    /// 是否启用语义单位级别稳定化
    use_semantic_units: bool,
}

impl AsrStabilizer {
    /// 创建新的稳定化器
    ///
    /// # Arguments
    /// * `stability_threshold` - 稳定性阈值（连续 N 次不变才算稳定）
    pub fn new(stability_threshold: usize) -> Self {
        Self {
            history: VecDeque::with_capacity(stability_threshold + 1),
            confirmed_length: 0,
            stability_threshold,
            max_history: stability_threshold + 1,
            min_stable_units: 3,
            use_semantic_units: true,
        }
    }

    /// 使用完整配置创建稳定化器
    pub fn with_config(config: StabilizerConfig) -> Self {
        Self {
            history: VecDeque::with_capacity(config.stability_threshold + 1),
            confirmed_length: 0,
            stability_threshold: config.stability_threshold,
            max_history: config.stability_threshold + 1,
            min_stable_units: config.min_stable_units,
            use_semantic_units: config.use_semantic_units,
        }
    }

    /// 处理新的 ASR 结果，返回可以发送翻译的稳定文本（如果有）
    ///
    /// # Arguments
    /// * `new_text` - 新的 ASR 累积文本
    ///
    /// # Returns
    /// * `Some(String)` - 新的稳定文本增量（尚未发送过的部分）
    /// * `None` - 没有新的稳定文本
    pub fn process(&mut self, new_text: &str) -> Option<String> {
        // 添加到历史
        self.history.push_back(new_text.to_string());
        if self.history.len() > self.max_history {
            self.history.pop_front();
        }

        // 需要足够的历史记录才能判断稳定性
        if self.history.len() < self.stability_threshold {
            debug!(
                "🔍 [Stabilizer] 历史记录不足: {}/{}, 等待更多结果",
                self.history.len(),
                self.stability_threshold
            );
            return None;
        }

        // 找到所有历史记录的公共前缀
        let stable_prefix = self.find_stable_prefix();

        // 检查是否满足最小稳定单位要求
        if self.use_semantic_units {
            let stable_units = Self::count_semantic_units(&stable_prefix);
            if stable_units < self.min_stable_units {
                debug!(
                    "🔍 [Stabilizer] 稳定前缀语义单位不足: {} < {}, 前缀: '{}'",
                    stable_units, self.min_stable_units, stable_prefix
                );
                return None;
            }
        }

        // 检查 ASR 是否修订了已确认的文本
        if stable_prefix.len() < self.confirmed_length {
            // ASR 修订了已确认的内容，需要重置
            debug!(
                "🔄 [Stabilizer] ASR 修订检测：稳定前缀变短 {} -> {}，重置状态",
                self.confirmed_length,
                stable_prefix.len()
            );
            self.confirmed_length = 0;
            self.history.clear();
            return None;
        }

        // 检查是否有新的稳定文本
        if stable_prefix.len() > self.confirmed_length {
            let new_stable = stable_prefix[self.confirmed_length..].to_string();
            debug!(
                "✅ [Stabilizer] 发现新稳定文本: '{}' (总长度: {} -> {})",
                new_stable,
                self.confirmed_length,
                stable_prefix.len()
            );
            self.confirmed_length = stable_prefix.len();
            Some(new_stable)
        } else {
            None
        }
    }

    /// 找到所有历史记录的公共前缀
    fn find_stable_prefix(&self) -> String {
        if self.history.is_empty() {
            return String::new();
        }

        let first = &self.history[0];
        let mut common_len = first.len();

        for text in self.history.iter().skip(1) {
            common_len = Self::common_prefix_length(first, text).min(common_len);
        }

        // 返回字符边界安全的前缀
        Self::safe_substring(first, common_len)
    }

    /// 计算两个字符串的公共前缀长度（字节）
    fn common_prefix_length(a: &str, b: &str) -> usize {
        a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
    }

    /// 安全截取字符串，确保在字符边界
    fn safe_substring(s: &str, max_bytes: usize) -> String {
        let mut end = max_bytes.min(s.len());
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        s[..end].to_string()
    }

    /// 检测字符是否为 CJK（中日韩）字符
    fn is_cjk(ch: char) -> bool {
        matches!(ch, '\u{4E00}'..='\u{9FFF}'   // CJK 基本区
               | '\u{3040}'..='\u{309F}'       // 平假名
               | '\u{30A0}'..='\u{30FF}'       // 片假名
               | '\u{AC00}'..='\u{D7AF}') // 韩文音节
    }

    /// 计算语义单位数量
    /// - CJK 字符：每个字符 = 1 单位
    /// - 拉丁语系：每个单词 = 1 单位（按空格分割）
    /// - 标点符号：不计入
    pub fn count_semantic_units(text: &str) -> usize {
        let mut count = 0;
        let mut in_word = false;

        for ch in text.chars() {
            if ch.is_whitespace() {
                in_word = false;
            } else if Self::is_cjk(ch) {
                count += 1;
                in_word = false;
            } else if ch.is_alphanumeric() {
                if !in_word {
                    count += 1;
                    in_word = true;
                }
            } else {
                // 标点符号不计入，但会结束当前单词
                in_word = false;
            }
        }
        count
    }

    /// 重置状态（新段落开始时调用）
    pub fn reset(&mut self) {
        self.history.clear();
        self.confirmed_length = 0;
        debug!("🔄 [Stabilizer] 状态已重置");
    }

    /// 获取已确认的文本长度（字节）
    pub fn confirmed_length(&self) -> usize {
        self.confirmed_length
    }

    /// 获取剩余未确认的文本
    ///
    /// 在段落结束时使用，返回最新 ASR 结果中尚未发送的部分
    pub fn get_remaining(&self, full_text: &str) -> Option<String> {
        if self.confirmed_length < full_text.len() {
            // 确保在字符边界上切片，避免 UTF-8 panic
            let mut start = self.confirmed_length;
            while start < full_text.len() && !full_text.is_char_boundary(start) {
                start += 1;
            }
            if start < full_text.len() {
                Some(full_text[start..].to_string())
            } else {
                None
            }
        } else {
            None
        }
    }
}

/// ASR 稳定化配置
#[derive(Debug, Clone)]
pub struct StabilizerConfig {
    /// 稳定性阈值：连续 N 次结果相同才算稳定
    /// 推荐值：2-3
    /// - N=2: 延迟约 3s（假设 ASR 1.5s 返回一次），但可能有误判
    /// - N=3: 延迟约 4.5s，更稳定
    pub stability_threshold: usize,

    /// 是否启用语义单位级别稳定化
    pub use_semantic_units: bool,

    /// 最小稳定长度（语义单位数）
    /// 避免频繁发送很短的片段
    pub min_stable_units: usize,
}

impl Default for StabilizerConfig {
    fn default() -> Self {
        Self {
            stability_threshold: 2, // 连续 2 次相同
            use_semantic_units: true,
            min_stable_units: 3, // 至少 3 个语义单位
        }
    }
}

/// 基于语义单位的稳定化器（可选增强版）
///
/// 与基于字符的稳定化器不同，这个版本在语义单位级别进行比较，
/// 对多语种（尤其是中文）更友好。
#[derive(Debug, Clone)]
pub struct SemanticStabilizer {
    /// 历史 ASR 结果（每个元素是语义单位列表）
    history: VecDeque<Vec<String>>,
    /// 已确认的语义单位数
    confirmed_units: usize,
    /// 稳定性阈值
    stability_threshold: usize,
    /// 最大历史记录数
    max_history: usize,
    /// 最小稳定单位数
    min_stable_units: usize,
}

impl SemanticStabilizer {
    /// 创建新的语义稳定化器
    pub fn new(stability_threshold: usize) -> Self {
        Self {
            history: VecDeque::with_capacity(stability_threshold + 1),
            confirmed_units: 0,
            stability_threshold,
            max_history: stability_threshold + 1,
            min_stable_units: 3,
        }
    }

    /// 将文本分割为语义单位
    pub fn tokenize_semantic_units(text: &str) -> Vec<String> {
        let mut units = Vec::new();
        let mut current_word = String::new();

        for ch in text.chars() {
            if Self::is_cjk(ch) {
                // CJK 字符：每个字符是一个单位
                if !current_word.is_empty() {
                    units.push(std::mem::take(&mut current_word));
                }
                units.push(ch.to_string());
            } else if ch.is_whitespace() {
                // 空格：结束当前单词
                if !current_word.is_empty() {
                    units.push(std::mem::take(&mut current_word));
                }
            } else if ch.is_alphanumeric() {
                // 拉丁字符：累积为单词
                current_word.push(ch);
            } else {
                // 标点符号：单独作为一个单位
                if !current_word.is_empty() {
                    units.push(std::mem::take(&mut current_word));
                }
                units.push(ch.to_string());
            }
        }

        if !current_word.is_empty() {
            units.push(current_word);
        }

        units
    }

    /// 检测字符是否为 CJK
    fn is_cjk(ch: char) -> bool {
        matches!(ch, '\u{4E00}'..='\u{9FFF}'
               | '\u{3040}'..='\u{309F}'
               | '\u{30A0}'..='\u{30FF}'
               | '\u{AC00}'..='\u{D7AF}')
    }

    /// 处理新的 ASR 结果
    pub fn process(&mut self, new_text: &str) -> Option<String> {
        let units = Self::tokenize_semantic_units(new_text);
        self.history.push_back(units);

        if self.history.len() > self.max_history {
            self.history.pop_front();
        }

        if self.history.len() < self.stability_threshold {
            return None;
        }

        // 找到公共前缀单位数
        let stable_count = self.find_stable_unit_count();

        if stable_count < self.min_stable_units {
            return None;
        }

        if stable_count > self.confirmed_units {
            // 取最新的历史记录来获取稳定部分的文本
            if let Some(latest) = self.history.back() {
                let new_stable: String = latest[self.confirmed_units..stable_count].iter().cloned().collect();
                self.confirmed_units = stable_count;
                return Some(new_stable);
            }
        }

        None
    }

    /// 找到所有历史记录的公共前缀单位数
    fn find_stable_unit_count(&self) -> usize {
        if self.history.is_empty() {
            return 0;
        }

        let first = &self.history[0];
        let mut common_count = first.len();

        for units in self.history.iter().skip(1) {
            let matching = first.iter().zip(units.iter()).take_while(|(a, b)| a == b).count();
            common_count = common_count.min(matching);
        }

        common_count
    }

    /// 重置状态
    pub fn reset(&mut self) {
        self.history.clear();
        self.confirmed_units = 0;
    }

    /// 获取剩余未确认的文本
    pub fn get_remaining(&self, full_text: &str) -> Option<String> {
        let units = Self::tokenize_semantic_units(full_text);
        if self.confirmed_units < units.len() {
            Some(units[self.confirmed_units..].join(""))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stabilizer_basic() {
        let mut stabilizer = AsrStabilizer::new(2);

        // 第一次结果
        assert_eq!(stabilizer.process("今天天气"), None);

        // 第二次结果，前缀稳定
        assert_eq!(stabilizer.process("今天天气很好"), Some("今天天气".to_string()));

        // 第三次结果，公共前缀仍为"今天天气"（已确认），无新稳定文本
        assert_eq!(stabilizer.process("今天天气很好啊"), None);
    }

    #[test]
    fn test_stabilizer_revision() {
        let mut stabilizer = AsrStabilizer::new(2);

        // ASR 修正了之前的文本
        stabilizer.process("你好世界");
        stabilizer.process("你好世界欢迎");

        // 重置
        stabilizer.reset();

        // 新段落
        assert_eq!(stabilizer.process("早上好"), None);
    }

    #[test]
    fn test_count_semantic_units() {
        // 中文
        assert_eq!(AsrStabilizer::count_semantic_units("今天天气很好"), 6);

        // 英文
        assert_eq!(AsrStabilizer::count_semantic_units("hello world"), 2);

        // 混合
        assert_eq!(AsrStabilizer::count_semantic_units("今天是Monday"), 4); // 今、天、是、Monday
    }

    #[test]
    fn test_semantic_tokenize() {
        let units = SemanticStabilizer::tokenize_semantic_units("你好world");
        assert_eq!(units, vec!["你", "好", "world"]);

        let units2 = SemanticStabilizer::tokenize_semantic_units("hello 世界！");
        assert_eq!(units2, vec!["hello", "世", "界", "！"]);
    }
}
