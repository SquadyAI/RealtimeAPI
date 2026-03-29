//! Language normalization for MiniMax `lang` / `language_boost`.
//!
//! Input can be a country code (e.g. "CN"), a language code (e.g. "zh", "en-US"),
//! or a language name. The output is normalized to the allowed set required by MiniMax:
//! [Chinese, Chinese,Yue, English, Arabic, Russian, Spanish, French, Portuguese, German,
//!  Turkish, Dutch, Ukrainian, Vietnamese, Indonesian, Japanese, Italian, Korean, Thai,
//!  Polish, Romanian, Greek, Czech, Finnish, Hindi, Bulgarian, Danish, Hebrew, Malay,
//!  Persian, Slovak, Swedish, Croatian, Filipino, Hungarian, Norwegian, Slovenian,
//!  Catalan, Nynorsk, Tamil, Afrikaans, auto]

use lazy_static::lazy_static;
use lingua::{Language, LanguageDetector, LanguageDetectorBuilder};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

lazy_static! {
    static ref LINGUA_DETECTOR: LanguageDetector = {
        use Language::*;
        let languages = vec![
            Chinese, English, Spanish, French, German, Portuguese, Italian, Japanese, Korean, Russian,
            Arabic, Turkish, Dutch, Ukrainian, Vietnamese, Indonesian, Thai, Polish, Romanian, Greek,
            Czech, Finnish, Hindi, Bulgarian, Danish, Hebrew, Malay, Persian, Slovak, Swedish, Croatian,
            Tagalog, Hungarian, Bokmal, Slovene, Catalan, Nynorsk, Tamil, Afrikaans,
        ];
        LanguageDetectorBuilder::from_languages(&languages)
            .with_minimum_relative_distance(0.0)
            .with_low_accuracy_mode()
            .with_preloaded_language_models()
            .build()
    };

    static ref ALLOWED: HashSet<&'static str> = HashSet::from([
        "Chinese",
        "Chinese,Yue",
        "English",
        "Arabic",
        "Russian",
        "Spanish",
        "French",
        "Portuguese",
        "German",
        "Turkish",
        "Dutch",
        "Ukrainian",
        "Vietnamese",
        "Indonesian",
        "Japanese",
        "Italian",
        "Korean",
        "Thai",
        "Polish",
        "Romanian",
        "Greek",
        "Czech",
        "Finnish",
        "Hindi",
        "Bulgarian",
        "Danish",
        "Hebrew",
        "Malay",
        "Persian",
        "Slovak",
        "Swedish",
        "Croatian",
        "Filipino",
        "Hungarian",
        "Norwegian",
        "Slovenian",
        "Catalan",
        "Nynorsk",
        "Tamil",
        "Afrikaans",
        "auto",
    ]);

    // ISO 3166 country code -> language name (allowed value)
    static ref COUNTRY_TO_LANG: HashMap<&'static str, &'static str> = {
        let mut m = HashMap::new();
        // Chinese / Cantonese
        m.insert("CN", "Chinese");
        m.insert("TW", "Chinese");
        m.insert("HK", "Chinese,Yue");
        m.insert("MO", "Chinese,Yue");
        // English-major countries
        for cc in ["US","GB","AU","NZ","IE","SG","CA"] { m.insert(cc, "English"); }
        // Arabic
        for cc in ["AE","BH","DZ","EG","IQ","JO","KW","LB","LY","MA","OM","PS","QA","SA","SD","SY","TN","YE"] { m.insert(cc, "Arabic"); }
        // Russian sphere (exclude UA)
        for cc in ["RU","BY","KZ","KG"] { m.insert(cc, "Russian"); }
        // Ukrainian
        m.insert("UA", "Ukrainian");
        // Spanish
        for cc in ["ES","MX","AR","CL","CO","PE","VE","EC","GT","CU","BO","DO","HN","PY","SV","NI","CR","UY","PA","PR","GQ"] { m.insert(cc, "Spanish"); }
        // French (select common countries; BE/CH are ambiguous, default to German/Dutch mapping elsewhere)
        for cc in ["FR","LU","MC","CI","SN","ML","BF","NE","TG","BJ","GN","CM","GA","MG","CD","CG","CF","TD","DJ","KM"] { m.insert(cc, "French"); }
        // Portuguese
        for cc in ["PT","BR","AO","MZ","GW","CV","ST","TL"] { m.insert(cc, "Portuguese"); }
        // German
        for cc in ["DE","AT","LI","LU"] { m.insert(cc, "German"); }
        // Dutch (BE ambiguous; choose Dutch)
        for cc in ["NL","BE","SR","AW","CW","SX"] { m.insert(cc, "Dutch"); }
        // Vietnamese / Indonesian / Japanese / Italian / Korean / Thai / Polish
        m.insert("VN", "Vietnamese");
        m.insert("ID", "Indonesian");
        m.insert("JP", "Japanese");
        m.insert("IT", "Italian");
        m.insert("KR", "Korean");
        m.insert("TH", "Thai");
        m.insert("PL", "Polish");
        // Romanian / Greek / Czech / Finnish / Hindi / Bulgarian / Danish / Hebrew / Malay / Persian / Slovak / Swedish / Croatian / Filipino / Hungarian / Norwegian / Slovenian / Catalan / Tamil / Afrikaans
        for (cc, lang) in [
            ("RO", "Romanian"), ("MD", "Romanian"),
            ("GR", "Greek"), ("CY", "Greek"),
            ("CZ", "Czech"), ("FI", "Finnish"),
            ("IN", "Hindi"),
            ("BG", "Bulgarian"), ("DK", "Danish"),
            ("IL", "Hebrew"),
            ("MY", "Malay"), ("BN", "Malay"),
            ("IR", "Persian"), ("AF", "Persian"),
            ("SK", "Slovak"), ("SE", "Swedish"), ("HR", "Croatian"),
            ("PH", "Filipino"), ("HU", "Hungarian"),
            ("NO", "Norwegian"), ("SI", "Slovenian"),
            ("AD", "Catalan"),
            ("LK", "Tamil"),
            ("ZA", "Afrikaans"), ("NA", "Afrikaans"),
        ] { m.insert(cc, lang); }
        // Switzerland (ambiguous): default to German
        m.insert("CH", "German");
        m
    };

    // ISO 639-1 language code -> language name (allowed value)
    static ref LANGCODE_TO_LANG: HashMap<&'static str, &'static str> = {
        let mut m = HashMap::new();
        m.insert("zh", "Chinese");
        m.insert("yue", "Chinese,Yue");
        m.insert("en", "English");
        m.insert("ar", "Arabic");
        m.insert("ru", "Russian");
        m.insert("es", "Spanish");
        m.insert("fr", "French");
        m.insert("pt", "Portuguese");
        m.insert("de", "German");
        m.insert("tr", "Turkish");
        m.insert("nl", "Dutch");
        m.insert("uk", "Ukrainian");
        m.insert("vi", "Vietnamese");
        m.insert("id", "Indonesian");
        m.insert("ja", "Japanese");
        m.insert("it", "Italian");
        m.insert("ko", "Korean");
        m.insert("th", "Thai");
        m.insert("pl", "Polish");
        m.insert("ro", "Romanian");
        m.insert("el", "Greek");
        m.insert("cs", "Czech");
        m.insert("fi", "Finnish");
        m.insert("hi", "Hindi");
        m.insert("bg", "Bulgarian");
        m.insert("da", "Danish");
        m.insert("he", "Hebrew");
        m.insert("iw", "Hebrew"); // legacy code
        m.insert("ms", "Malay");
        m.insert("fa", "Persian");
        m.insert("sk", "Slovak");
        m.insert("sv", "Swedish");
        m.insert("hr", "Croatian");
        m.insert("fil", "Filipino");
        m.insert("tl", "Filipino");
        m.insert("hu", "Hungarian");
        m.insert("no", "Norwegian");
        m.insert("nn", "Nynorsk");
        m.insert("sl", "Slovenian");
        m.insert("ca", "Catalan");
        m.insert("ta", "Tamil");
        m.insert("af", "Afrikaans");
        m
    };

    /// 语言到音色的映射表（用于动态音色切换）
    static ref LANGUAGE_VOICE_MAPPING: HashMap<&'static str, &'static str> = {
        let mut m = HashMap::new();
        m.insert("Chinese", "zh_female_wanwanxiaohe_moon_bigtts");
        m.insert("Chinese,Yue", "zh-HK-WanLungNeural");
        m.insert("English", "en_female_lauren_moon_bigtts");
        m.insert("Japanese", "multi_female_gaolengyujie_moon_bigtts");
        m.insert("Korean", "ko-KR-SunHiNeural");
        m.insert("Russian", "ru-RU-SvetlanaNeural");
        m.insert("Thai", "th-TH-PremwadeeNeural");
        m.insert("Spanish", "multi_female_shuangkuaisisi_moon_bigtts");
        m.insert("French", "fr-FR-VivienneMultilingualNeural");
        m
    };
}

/// Lingua 语言检测的最低置信度阈值
pub const LINGUA_MIN_CONFIDENCE: f64 = 0.6;
/// Lingua 语言检测的最低置信度差距（与次高语言）
pub const LINGUA_MIN_MARGIN: f64 = 0.2;

/// 根据语言获取对应的音色（用于动态音色切换）
pub fn get_voice_for_language(language: &str) -> Option<&'static str> {
    LANGUAGE_VOICE_MAPPING.get(language).copied()
}

fn capitalize_allowed(name: &str) -> Option<&'static str> {
    // Match case-insensitively to allowed names and return the canonical form
    let lower = name.trim().to_ascii_lowercase();
    ALLOWED.iter().find(|&v| v.to_ascii_lowercase() == lower).map(|v| v as _)
}

fn normalize_language_or_locale(s: &str) -> Option<&'static str> {
    let raw = s.trim();
    if raw.is_empty() {
        return None;
    }
    // direct allowed name
    if let Some(canon) = capitalize_allowed(raw) {
        return Some(canon);
    }

    let lower = raw.to_ascii_lowercase();
    if lower == "auto" {
        return Some("auto");
    }

    // locale like zh-CN, en_US, zh-HK -> prefer language code, but special-case zh-HK
    // Materialize the replaced string so the slices live long enough
    let normalized = lower.replace('_', "-");
    let parts: Vec<&str> = normalized.split('-').collect();
    if parts.is_empty() {
        return None;
    }

    // Special-case Cantonese
    if lower == "yue" || lower == "zh-hk" || lower == "zh-mo" || lower == "cantonese" {
        return Some("Chinese,Yue");
    }
    if lower == "zh" || lower == "zh-cn" || lower == "zh-tw" {
        return Some("Chinese");
    }

    // Try language code map (first token)
    if let Some(&lang) = LANGCODE_TO_LANG.get(parts[0]) {
        return Some(lang);
    }

    // If only region code present (or language unknown), try as country code
    let region = parts.last().unwrap_or(&parts[0]).to_ascii_uppercase();
    if let Some(&lang) = COUNTRY_TO_LANG.get(region.as_str()) {
        return Some(lang);
    }

    None
}

/// Normalize input (country code, language code, or name) to MiniMax allowed `lang`.
/// Any unknown or out-of-range value returns `auto`.
pub fn normalize_minimax_lang(input: Option<&str>) -> String {
    match input {
        None => "auto".to_string(),
        Some(s) => normalize_language_or_locale(s).unwrap_or("auto").to_string(),
    }
}

/// 将 Lingua 识别结果映射到 MiniMax 的 `language_boost` 值
pub fn lingua_language_to_minimax(language: Language) -> Option<&'static str> {
    match language {
        Language::Chinese => Some("Chinese"),
        Language::English => Some("English"),
        Language::Spanish => Some("Spanish"),
        Language::French => Some("French"),
        Language::German => Some("German"),
        Language::Portuguese => Some("Portuguese"),
        Language::Italian => Some("Italian"),
        Language::Japanese => Some("Japanese"),
        Language::Korean => Some("Korean"),
        Language::Russian => Some("Russian"),
        Language::Arabic => Some("Arabic"),
        Language::Turkish => Some("Turkish"),
        Language::Dutch => Some("Dutch"),
        Language::Ukrainian => Some("Ukrainian"),
        Language::Vietnamese => Some("Vietnamese"),
        Language::Indonesian => Some("Indonesian"),
        Language::Thai => Some("Thai"),
        Language::Polish => Some("Polish"),
        Language::Romanian => Some("Romanian"),
        Language::Greek => Some("Greek"),
        Language::Czech => Some("Czech"),
        Language::Finnish => Some("Finnish"),
        Language::Hindi => Some("Hindi"),
        Language::Bulgarian => Some("Bulgarian"),
        Language::Danish => Some("Danish"),
        Language::Hebrew => Some("Hebrew"),
        Language::Malay => Some("Malay"),
        Language::Persian => Some("Persian"),
        Language::Slovak => Some("Slovak"),
        Language::Swedish => Some("Swedish"),
        Language::Croatian => Some("Croatian"),
        Language::Tagalog => Some("Filipino"),
        Language::Hungarian => Some("Hungarian"),
        Language::Bokmal => Some("Norwegian"),
        Language::Slovene => Some("Slovenian"),
        Language::Catalan => Some("Catalan"),
        Language::Tamil => Some("Tamil"),
        Language::Afrikaans => Some("Afrikaans"),
        _ => None,
    }
}

/// 使用 Lingua 计算语言置信度（复用分句器的检测器）
pub fn lingua_language_confidences(text: &str) -> Vec<(Language, f64)> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let start_time = std::time::Instant::now();
    let result = LINGUA_DETECTOR.compute_language_confidence_values(text);
    let elapsed_ms = start_time.elapsed().as_millis();
    tracing::debug!("🌐 Lingua 语言检测耗时: {}ms (text_len={})", elapsed_ms, text.len());
    result
}

/// 根据置信度与差值阈值返回适用于 MiniMax 的语言增强值
pub fn detect_language_boost(text: &str, min_confidence: f64, min_margin: f64) -> Option<String> {
    let start_time = std::time::Instant::now();
    let mut confidences = lingua_language_confidences(text);
    if confidences.is_empty() {
        let elapsed_ms = start_time.elapsed().as_millis();
        tracing::debug!("🌐 Lingua detect_language_boost 总耗时: {}ms (empty confidences)", elapsed_ms);
        return None;
    }

    confidences.sort_by(|a, b| {
        b.1.partial_cmp(&a.1).unwrap_or_else(|| {
            if a.1 == b.1 {
                Ordering::Equal
            } else if a.1.is_nan() {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        })
    });

    let (best_lang, best_conf) = confidences[0];
    if best_conf < min_confidence {
        let elapsed_ms = start_time.elapsed().as_millis();
        tracing::debug!(
            "🌐 Lingua detect_language_boost 总耗时: {}ms (confidence too low: {})",
            elapsed_ms,
            best_conf
        );
        return None;
    }

    let second_conf = confidences.get(1).map(|(_, c)| *c).unwrap_or(0.0);
    if best_conf - second_conf < min_margin {
        let elapsed_ms = start_time.elapsed().as_millis();
        tracing::debug!(
            "🌐 Lingua detect_language_boost 总耗时: {}ms (margin too small: {} - {} = {})",
            elapsed_ms,
            best_conf,
            second_conf,
            best_conf - second_conf
        );
        return None;
    }

    let result = lingua_language_to_minimax(best_lang).map(|lang| lang.to_string());
    let elapsed_ms = start_time.elapsed().as_millis();
    if let Some(ref lang) = result {
        tracing::debug!(
            "🌐 Lingua detect_language_boost 总耗时: {}ms (detected: {}, confidence: {})",
            elapsed_ms,
            lang,
            best_conf
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{detect_language_boost, lingua_language_to_minimax, normalize_minimax_lang};
    use lingua::Language;

    #[test]
    fn test_country_codes() {
        assert_eq!(normalize_minimax_lang(Some("CN")), "Chinese");
        assert_eq!(normalize_minimax_lang(Some("HK")), "Chinese,Yue");
        assert_eq!(normalize_minimax_lang(Some("US")), "English");
        assert_eq!(normalize_minimax_lang(Some("BR")), "Portuguese");
        assert_eq!(normalize_minimax_lang(Some("UA")), "Ukrainian");
        assert_eq!(normalize_minimax_lang(Some("ZA")), "Afrikaans");
    }

    #[test]
    fn test_language_codes_and_names() {
        assert_eq!(normalize_minimax_lang(Some("zh")), "Chinese");
        assert_eq!(normalize_minimax_lang(Some("zh-HK")), "Chinese,Yue");
        assert_eq!(normalize_minimax_lang(Some("yue")), "Chinese,Yue");
        assert_eq!(normalize_minimax_lang(Some("en")), "English");
        assert_eq!(normalize_minimax_lang(Some("Portuguese")), "Portuguese");
        assert_eq!(normalize_minimax_lang(Some("nYnOrSk")), "Nynorsk");
    }

    #[test]
    fn test_unknown_defaults_to_auto() {
        assert_eq!(normalize_minimax_lang(Some("xx")), "auto");
        assert_eq!(normalize_minimax_lang(None), "auto");
    }

    #[test]
    fn test_lingua_language_mapping() {
        assert_eq!(lingua_language_to_minimax(Language::English), Some("English"));
        assert_eq!(lingua_language_to_minimax(Language::Chinese), Some("Chinese"));
        assert_eq!(lingua_language_to_minimax(Language::Esperanto), None);
    }

    #[test]
    fn test_detect_language_boost_confident_english() {
        let text = "This is a simple English sentence used for language detection.";
        let result = detect_language_boost(text, 0.5, 0.1);
        assert_eq!(result, Some("English".to_string()));
    }
}
