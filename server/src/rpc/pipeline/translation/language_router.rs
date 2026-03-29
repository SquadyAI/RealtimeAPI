//! 同声传译语言路由器
//!
//! 功能：
//! - TTS 语言路由（MiniMax 40种语言，不支持的自动 fallback 到英语）
//! - ISO 639 代码 → MiniMax language_boost 映射

use tracing::warn;

/// MiniMax 支持的语言列表（ISO 639-1 代码）
pub const MINIMAX_SUPPORTED_LANGUAGES: &[&str] = &[
    "zh", "yue", "en", "ar", "ru", "es", "fr", "pt", "de", "tr", "nl", "uk", "vi", "id", "ja", "it", "ko", "th", "pl", "ro", "el", "cs", "fi", "hi", "bg", "da", "he", "ms", "fa", "sk", "sv", "hr",
    "fil", "hu", "no", "sl", "ca", "nn", "ta", "af",
];

/// 获取实际使用的 TTS 语言（带 fallback）
///
/// 如果目标语言不支持，返回 "en"（英语）并记录监控指标
pub fn get_actual_tts_language(language: &str) -> String {
    let lang = language.to_lowercase();

    if MINIMAX_SUPPORTED_LANGUAGES.contains(&lang.as_str()) {
        lang
    } else {
        warn!("⚠️ 语言 '{}' 不支持，fallback 到英语", language);

        // 记录 fallback 指标
        use crate::monitoring::METRICS;
        METRICS.tts_fallback_total.with_label_values(&[&lang]).inc();

        "en".to_string()
    }
}

/// ISO 639 代码转 MiniMax language_boost 格式
///
/// 示例：
/// - "zh" → "Chinese"
/// - "en" → "English"
/// - "ja" → "Japanese"
pub fn to_minimax_language_boost(language: &str) -> Option<String> {
    let lang = language.to_lowercase();

    let boost = match lang.as_str() {
        "zh" | "zh-cn" | "cmn" => "Chinese",
        "yue" | "zh-hk" | "zh-tw" => "Chinese,Yue",
        "en" => "English",
        "ar" => "Arabic",
        "ja" => "Japanese",
        "ko" => "Korean",
        "es" => "Spanish",
        "fr" => "French",
        "de" => "German",
        "pt" => "Portuguese",
        "it" => "Italian",
        "ru" => "Russian",
        "tr" => "Turkish",
        "nl" => "Dutch",
        "uk" => "Ukrainian",
        "vi" => "Vietnamese",
        "id" => "Indonesian",
        "th" => "Thai",
        "pl" => "Polish",
        "ro" => "Romanian",
        "el" => "Greek",
        "cs" => "Czech",
        "fi" => "Finnish",
        "hi" => "Hindi",
        "bg" => "Bulgarian",
        "da" => "Danish",
        "he" => "Hebrew",
        "ms" => "Malay",
        "fa" => "Persian",
        "sk" => "Slovak",
        "sv" => "Swedish",
        "hr" => "Croatian",
        "fil" | "tl" => "Filipino",
        "hu" => "Hungarian",
        "no" | "nb" => "Norwegian",
        "sl" => "Slovenian",
        "ca" => "Catalan",
        "nn" => "Nynorsk",
        "ta" => "Tamil",
        "af" => "Afrikaans",
        _ => {
            warn!("❌ 未知语言代码: {}", language);
            return None;
        },
    };

    Some(boost.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_actual_tts_language() {
        // 支持的语言
        assert_eq!(get_actual_tts_language("zh"), "zh");
        assert_eq!(get_actual_tts_language("en"), "en");
        assert_eq!(get_actual_tts_language("ja"), "ja");
        assert_eq!(get_actual_tts_language("ko"), "ko");
        assert_eq!(get_actual_tts_language("fr"), "fr");

        // 不支持的语言 -> fallback 到英语
        assert_eq!(get_actual_tts_language("sw"), "en"); // 斯瓦希里语
        assert_eq!(get_actual_tts_language("xyz"), "en"); // 不存在的语言
        assert_eq!(get_actual_tts_language("bn"), "en"); // 孟加拉语
    }

    #[test]
    fn test_language_boost_mapping() {
        assert_eq!(to_minimax_language_boost("zh"), Some("Chinese".to_string()));
        assert_eq!(to_minimax_language_boost("yue"), Some("Chinese,Yue".to_string()));
        assert_eq!(to_minimax_language_boost("en"), Some("English".to_string()));
        assert_eq!(to_minimax_language_boost("ja"), Some("Japanese".to_string()));
        assert_eq!(to_minimax_language_boost("xyz"), None); // 不存在的语言
    }
}
