//! Role Extractor - 从 XML 格式的 system_prompt 中提取 <role> 部分
//!
//! 用于将外部注入的人格设定（role）提取出来，注入到各专用 Agent 的 prompt 中。

use tracing::warn;

/// 从 XML 格式的 system_prompt 中提取 `<role>...</role>` 内容
///
/// # Arguments
/// * `system_prompt` - 完整的 system prompt 字符串（XML 格式）
///
/// # Returns
/// * `Some(String)` - 提取到的 role 部分（包含 `<role>` 标签）
/// * `None` - 未找到 role 部分或解析失败
///
/// # Example
/// ```
/// let prompt = "<assistantProfile><role><name>Alice</name></role></assistantProfile>";
/// let role = extract_role_from_system_prompt(prompt);
/// assert_eq!(role, Some("<role><name>Alice</name></role>".to_string()));
/// ```
pub fn extract_role_from_system_prompt(system_prompt: &str) -> Option<String> {
    let start_tag = "<role>";
    let end_tag = "</role>";

    // 1. 找开始标签
    let start_pos = match system_prompt.find(start_tag) {
        Some(pos) => pos,
        None => {
            // 没有 <role> 标签，这是正常情况（可能是旧格式的 prompt）
            return None;
        },
    };

    // 2. 在开始标签之后找闭合标签
    let after_start = start_pos + start_tag.len();
    let end_pos = match system_prompt[after_start..].find(end_tag) {
        Some(relative_pos) => after_start + relative_pos,
        None => {
            warn!("⚠️ role_extractor: 发现 <role> 但缺少 </role> 闭合标签，跳过 role 提取");
            return None;
        },
    };

    // 3. 验证内部内容非空
    let inner_content = system_prompt[after_start..end_pos].trim();
    if inner_content.is_empty() {
        warn!("⚠️ role_extractor: <role></role> 内容为空，跳过 role 提取");
        return None;
    }

    // 4. 提取并清理结果
    let role_content = &system_prompt[start_pos..end_pos + end_tag.len()];
    Some(role_content.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_role_basic() {
        let prompt = r#"<assistantProfile>
            <role>
                <name>Alice</name>
                <organization>SquadyAI</organization>
            </role>
            <Requirements>...</Requirements>
        </assistantProfile>"#;

        let role = extract_role_from_system_prompt(prompt);
        assert!(role.is_some());
        let role_str = role.unwrap();
        assert!(role_str.starts_with("<role>"));
        assert!(role_str.ends_with("</role>"));
        assert!(role_str.contains("<name>Alice</name>"));
        assert!(role_str.contains("<organization>SquadyAI</organization>"));
    }

    #[test]
    fn test_extract_role_full_xml() {
        let prompt = r#"<assistantProfile><role><name>Alice</name><organization>SquadyAI</organization><description>多语言语音AI助手</description><personality>机智幽默,简洁直接,谦虚真诚</personality></role><Requirements><requirement>始终使用用户的输入语言或要求的语言回复</requirement></Requirements></assistantProfile>"#;

        let role = extract_role_from_system_prompt(prompt);
        assert!(role.is_some());
        let role_str = role.unwrap();
        assert!(role_str.contains("<personality>"));
        assert!(role_str.contains("机智幽默"));
    }

    #[test]
    fn test_no_role() {
        let prompt = "<assistantProfile><Requirements>...</Requirements></assistantProfile>";
        assert!(extract_role_from_system_prompt(prompt).is_none());
    }

    #[test]
    fn test_empty_string() {
        assert!(extract_role_from_system_prompt("").is_none());
    }

    #[test]
    fn test_unclosed_tag() {
        // 标签不闭合：有 <role> 但没有 </role>
        let prompt = "<role><name>Test</name>";
        assert!(extract_role_from_system_prompt(prompt).is_none());
    }

    #[test]
    fn test_only_end_tag() {
        // 只有闭合标签，没有开始标签
        let prompt = "<name>Test</name></role>";
        assert!(extract_role_from_system_prompt(prompt).is_none());
    }

    #[test]
    fn test_empty_role_content() {
        // <role></role> 内容为空
        let prompt = "<assistantProfile><role></role></assistantProfile>";
        assert!(extract_role_from_system_prompt(prompt).is_none());
    }

    #[test]
    fn test_whitespace_only_role_content() {
        // <role> 内只有空白字符
        let prompt = "<assistantProfile><role>   \n\t  </role></assistantProfile>";
        assert!(extract_role_from_system_prompt(prompt).is_none());
    }

    #[test]
    fn test_trailing_garbage() {
        // 文档后有多余字符
        let prompt = "<role><name>Alice</name></role>\n\n一些垃圾字符@#$%";
        let role = extract_role_from_system_prompt(prompt);
        assert!(role.is_some());
        let role_str = role.unwrap();
        assert!(role_str.ends_with("</role>"));
        assert!(!role_str.contains("垃圾"));
    }

    #[test]
    fn test_result_is_trimmed() {
        // 结果应该被 trim
        let prompt = "  \n<role><name>Test</name></role>  \n";
        let role = extract_role_from_system_prompt(prompt);
        assert!(role.is_some());
        let role_str = role.unwrap();
        assert!(role_str.starts_with("<role>"));
        assert!(role_str.ends_with("</role>"));
    }
}
