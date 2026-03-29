//! 多语言断句标点模块
//!
//! 提供多语言环境下的断句点检测功能，支持：
//! - 中文/日文/韩文标点
//! - 英文/西文标点
//! - 阿拉伯文/希伯来文标点
//! - 泰文/越南文标点

/// 强断句点：句号、问号、感叹号等句末标点
/// 遇到即断句，不管字数
pub fn is_sentence_terminal(c: char) -> bool {
    matches!(
        c,
        // 英文/西文
        '.' | '!' | '?' |
        // 中文/日文
        '。' | '！' | '？' | '…' |
        // 韩文
        // (韩文通常使用中文标点)
        // 阿拉伯文
        '؟' | '۔' |
        // 泰文
        '๏' |
        // 省略号变体
        '⋯'
    )
}

/// 弱断句点：逗号、分号、冒号等句中标点
/// 需要满足字数阈值才断句
pub fn is_weak_break_point(c: char) -> bool {
    matches!(
        c,
        // 英文/西文
        ',' | ';' | ':' |
        // 中文/日文
        '，' | '；' | '：' | '、' |
        // 韩文
        // (韩文通常使用中文标点)
        // 阿拉伯文
        '،' | '؛'
    )
}

/// 查找文本中最后一个强断句点的位置
///
/// # Returns
/// * `Some((byte_index, char_len_in_bytes))` - 断句点的字节位置和字符长度
/// * `None` - 未找到断句点
pub fn find_last_sentence_terminal(text: &str) -> Option<(usize, usize)> {
    for (byte_idx, ch) in text.char_indices().rev() {
        if is_sentence_terminal(ch) {
            return Some((byte_idx, ch.len_utf8()));
        }
    }
    None
}

/// 查找文本中最后一个弱断句点的位置
///
/// # Returns
/// * `Some((byte_index, char_len_in_bytes))` - 断句点的字节位置和字符长度
/// * `None` - 未找到断句点
pub fn find_last_weak_break(text: &str) -> Option<(usize, usize)> {
    for (byte_idx, ch) in text.char_indices().rev() {
        if is_weak_break_point(ch) {
            return Some((byte_idx, ch.len_utf8()));
        }
    }
    None
}

/// 检查文本是否只包含标点符号（无实际内容）
///
/// 用于过滤纯标点的断句结果
pub fn is_only_punctuation(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.is_empty() || trimmed.chars().all(|c| !c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sentence_terminal() {
        // 英文
        assert!(is_sentence_terminal('.'));
        assert!(is_sentence_terminal('!'));
        assert!(is_sentence_terminal('?'));

        // 中文
        assert!(is_sentence_terminal('。'));
        assert!(is_sentence_terminal('！'));
        assert!(is_sentence_terminal('？'));

        // 非句末标点
        assert!(!is_sentence_terminal(','));
        assert!(!is_sentence_terminal('，'));
    }

    #[test]
    fn test_weak_break() {
        // 英文
        assert!(is_weak_break_point(','));
        assert!(is_weak_break_point(';'));

        // 中文
        assert!(is_weak_break_point('，'));
        assert!(is_weak_break_point('、'));

        // 非弱断句点
        assert!(!is_weak_break_point('.'));
        assert!(!is_weak_break_point('。'));
    }

    #[test]
    fn test_find_last_sentence_terminal() {
        assert_eq!(find_last_sentence_terminal("你好。世界"), Some((6, 3)));
        assert_eq!(find_last_sentence_terminal("Hello!"), Some((5, 1)));
        assert_eq!(find_last_sentence_terminal("没有句号"), None);
    }

    #[test]
    fn test_find_last_weak_break() {
        assert_eq!(find_last_weak_break("你好，世界"), Some((6, 3)));
        assert_eq!(find_last_weak_break("Hello, world"), Some((5, 1)));
        assert_eq!(find_last_weak_break("没有逗号"), None);
    }

    #[test]
    fn test_is_only_punctuation() {
        assert!(is_only_punctuation(""));
        assert!(is_only_punctuation("   "));
        assert!(is_only_punctuation("。！？"));
        assert!(is_only_punctuation("..."));
        assert!(!is_only_punctuation("你好"));
        assert!(!is_only_punctuation("Hello"));
        assert!(!is_only_punctuation("你好。"));
    }
}
