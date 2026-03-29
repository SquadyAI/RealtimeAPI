use emojis;
use lingua::Language;
use unicode_segmentation::UnicodeSegmentation;

use crate::rpc::tts_pool::TtsEngineKind;
use crate::tts::baidu::BAIDU_TTS_VOICE_ID_DUHANZHU_BRIGHT;

pub const TARGET_VOLC_VOICE_ID: &str = "zh_female_wanwanxiaohe_moon_bigtts";
const MAX_OTHER_LANGUAGE_CONFIDENCE: f64 = 0.2;
const MIN_ALLOWED_MARGIN: f64 = 0.15;

/// 语言检测结果的三态分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LanguageDetectionResult {
    /// 检测确定：中文或英文
    ConfidentCnOrEn,
    /// 检测确定：非中英文（如西班牙语、日语等）
    ConfidentOtherLanguage,
    /// 检测不确定（置信度低或各语言差距小）
    Uncertain,
}

/// 语言检测的最低置信度阈值（低于此值视为不确定）
const MIN_DETECTION_CONFIDENCE: f64 = 0.5;

fn classify_language_detection(cached_confidences: &[(Language, f64)]) -> LanguageDetectionResult {
    if cached_confidences.is_empty() {
        return LanguageDetectionResult::Uncertain;
    }

    let mut best_allowed: f64 = 0.0;
    let mut best_other: f64 = 0.0;
    for (language, confidence) in cached_confidences {
        if *confidence <= 0.0 {
            continue;
        }
        match language {
            Language::English | Language::Chinese => best_allowed = best_allowed.max(*confidence),
            _ => best_other = best_other.max(*confidence),
        }
    }

    // 检测不确定：所有语言置信度都很低
    if best_allowed < MIN_DETECTION_CONFIDENCE && best_other < MIN_DETECTION_CONFIDENCE {
        return LanguageDetectionResult::Uncertain;
    }

    // 检测确定为非中英文：其他语言置信度高于中/英文
    if best_other > best_allowed && best_other >= MIN_DETECTION_CONFIDENCE {
        return LanguageDetectionResult::ConfidentOtherLanguage;
    }

    // 检测确定为中/英文
    if best_allowed >= MIN_DETECTION_CONFIDENCE {
        // 额外检查：中/英文需要有足够的优势
        if best_other == 0.0 || (best_other <= MAX_OTHER_LANGUAGE_CONFIDENCE && best_allowed >= best_other + MIN_ALLOWED_MARGIN) {
            return LanguageDetectionResult::ConfidentCnOrEn;
        }
    }

    // 其他情况视为不确定（如置信度接近但都不够高）
    LanguageDetectionResult::Uncertain
}

/// 保留供其他模块使用（如 VolcEngine 路由）
#[allow(dead_code)]
fn confidences_indicate_cn_or_en(cached_confidences: &[(Language, f64)]) -> bool {
    matches!(
        classify_language_detection(cached_confidences),
        LanguageDetectionResult::ConfidentCnOrEn
    )
}

fn is_invisible_or_control(ch: char) -> bool {
    if ch.is_control() {
        return true;
    }
    let u = ch as u32;
    matches!(u,
        0x00AD
        | 0x200B
        | 0x200C
        | 0x200D
        | 0x200E
        | 0x200F
        | 0x202A..=0x202E
        | 0x2060..=0x206F
        | 0xFE00..=0xFE0F
        | 0xE0000..=0xE0FFF
    )
}

pub fn sanitize_visible_text(input: &str) -> String {
    let mut buf = String::new();
    for g in input.graphemes(true) {
        if emojis::get(g).is_some() {
            continue;
        }
        for ch in g.chars() {
            if !is_invisible_or_control(ch) {
                buf.push(ch);
            }
        }
    }

    let collapsed = buf.split_whitespace().collect::<Vec<_>>().join(" ");
    let collapsed = collapsed.trim();
    if collapsed.is_empty() {
        return String::new();
    }

    let mut graphemes = collapsed.graphemes(true);
    if let Some(first) = graphemes.next()
        && graphemes.next().is_none()
        && !first.chars().any(|ch| ch.is_alphanumeric())
    {
        tracing::debug!("🧹 过滤单符号文本，跳过发送到TTS: '{}'", first);
        return String::new();
    }

    // 移除 markdown 符号：* ` # >（引用）
    collapsed.replace(['*', '`', '#', '>'], "").replace('：', ":")
}

/// 判断是否路由到 VolcEngine（使用缓存的语言检测结果）
pub fn should_route_to_volc(text: &str, voice_id: Option<&str>, cached_confidences: &[(Language, f64)]) -> bool {
    if voice_id != Some(TARGET_VOLC_VOICE_ID) {
        return false;
    }

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    let start_time = std::time::Instant::now();

    if cached_confidences.is_empty() {
        let elapsed_ms = start_time.elapsed().as_millis();
        tracing::warn!("🌐 Lingua routing 检查耗时: {}ms (empty confidences)", elapsed_ms);
        return true;
    }

    let mut best_allowed: f64 = 0.0;
    let mut best_other: f64 = 0.0;

    for (language, confidence) in cached_confidences {
        if *confidence <= 0.0 {
            continue;
        }
        match language {
            Language::English | Language::Chinese => {
                best_allowed = best_allowed.max(*confidence);
            },
            _ => {
                best_other = best_other.max(*confidence);
            },
        }
    }

    let result = if best_allowed == 0.0 {
        false
    } else if best_other == 0.0 {
        true
    } else {
        best_other <= MAX_OTHER_LANGUAGE_CONFIDENCE && best_allowed >= best_other + MIN_ALLOWED_MARGIN
    };

    let elapsed_ms = start_time.elapsed().as_millis();
    tracing::debug!(
        "🌐 Lingua routing 检查耗时: {}ms (使用缓存, result={}, best_allowed={}, best_other={})",
        elapsed_ms,
        result,
        best_allowed,
        best_other
    );
    result
}

/// 选择 TTS 引擎的结果（包含是否为确定性路由的标志）
#[derive(Debug, Clone, Copy)]
pub struct TtsEngineSelection {
    pub engine: TtsEngineKind,
    /// 是否为确定性路由（语言检测通过），可用于轮次内继承
    pub is_confident: bool,
}

/// 选择 TTS 引擎。
///
/// - `force_engine`：强制使用的引擎（用于同声传译等场景），优先级最高
/// - 环境变量 `TTS_ENGINE` 可强制指定：`baidu` / `minimax` / `volc` / `edge` / `auto`
/// - 默认 `auto`：沿用现有策略（满足条件时走 VolcEngine，否则走 MiniMax）
/// - `inherited_engine`：轮次内已确认的引擎，用于在检测不确定时继承
pub fn select_tts_engine(
    text: &str,
    voice_id: Option<&str>,
    cached_confidences: &[(Language, f64)],
    inherited_engine: Option<TtsEngineKind>,
    force_engine: Option<TtsEngineKind>,
) -> TtsEngineSelection {
    // 最高优先级：强制引擎（同声传译模式使用）
    if let Some(engine) = force_engine {
        return TtsEngineSelection { engine, is_confident: true };
    }

    let override_engine = std::env::var("TTS_ENGINE").ok().map(|s| s.to_ascii_lowercase());
    match override_engine.as_deref() {
        Some("baidu") | Some("baidu_ws") | Some("baidutts") => {
            return TtsEngineSelection { engine: TtsEngineKind::Baidu, is_confident: true };
        },
        Some("minimax") => {
            return TtsEngineSelection { engine: TtsEngineKind::MiniMax, is_confident: true };
        },
        Some("volc") | Some("volcengine") | Some("volc_engine") => {
            return TtsEngineSelection { engine: TtsEngineKind::VolcEngine, is_confident: true };
        },
        Some("edge") | Some("edgetts") | Some("edge_tts") => {
            return TtsEngineSelection { engine: TtsEngineKind::EdgeTts, is_confident: true };
        },
        Some("azure") | Some("azuretts") | Some("azure_tts") => {
            return TtsEngineSelection { engine: TtsEngineKind::AzureTts, is_confident: true };
        },
        Some("auto") | None => {},
        Some(other) => {
            tracing::warn!("⚠️ 未识别的 TTS_ENGINE='{}'，回退到 auto 路由", other);
        },
    }

    if voice_id == Some(BAIDU_TTS_VOICE_ID_DUHANZHU_BRIGHT) {
        // 使用三态分类判断语言检测结果
        let detection_result = classify_language_detection(cached_confidences);

        match detection_result {
            LanguageDetectionResult::ConfidentCnOrEn => {
                // 检测确定为中/英文 → Baidu（确定性路由）
                return TtsEngineSelection { engine: TtsEngineKind::Baidu, is_confident: true };
            },
            LanguageDetectionResult::ConfidentOtherLanguage => {
                // 检测确定为非中英文 → MiniMax（确定性路由）
                tracing::debug!(
                    "🌐 检测确定为非中英文，路由到 MiniMax (text='{}')",
                    text.chars().take(30).collect::<String>()
                );
                return TtsEngineSelection { engine: TtsEngineKind::MiniMax, is_confident: true };
            },
            LanguageDetectionResult::Uncertain => {
                // 检测不确定时的处理
                // 方案A：优先使用轮次内已确认的引擎
                if let Some(inherited) = inherited_engine {
                    tracing::debug!(
                        "🔄 语言检测不确定，继承轮次内路由: {:?} (text='{}')",
                        inherited,
                        text.chars().take(30).collect::<String>()
                    );
                    return TtsEngineSelection { engine: inherited, is_confident: false };
                }

                // 方案C：没有可继承的引擎，默认 Baidu（因为该 voice_id 主要用于中英文）
                tracing::debug!(
                    "🔄 语言检测不确定且无轮次继承，默认 Baidu (text='{}')",
                    text.chars().take(30).collect::<String>()
                );
                return TtsEngineSelection { engine: TtsEngineKind::Baidu, is_confident: false };
            },
        }
    }

    if should_route_to_volc(text, voice_id, cached_confidences) {
        TtsEngineSelection { engine: TtsEngineKind::VolcEngine, is_confident: true }
    } else {
        TtsEngineSelection { engine: TtsEngineKind::MiniMax, is_confident: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_splitter::SimplifiedStreamingSplitter;
    use crate::tts::minimax::lang::lingua_language_confidences;

    #[test]
    fn routes_mixed_language_sentences() {
        let mut splitter = SimplifiedStreamingSplitter::new(None);

        let mut chunks = Vec::new();
        chunks.extend(splitter.found_first_sentence("你好，世界！Let's build something cool."));
        chunks.extend(splitter.found_first_sentence("接下来是一句英文。"));
        chunks.extend(splitter.found_first_sentence("Esto es una frase en"));
        chunks.extend(splitter.finalize());

        let mut routing = Vec::new();
        for chunk in chunks {
            let cleaned = sanitize_visible_text(&chunk.text);
            if cleaned.is_empty() {
                continue;
            }
            // 进行语言检测
            let confidences = lingua_language_confidences(&chunk.text);
            let engine = if should_route_to_volc(&cleaned, Some(TARGET_VOLC_VOICE_ID), &confidences) {
                TtsEngineKind::VolcEngine
            } else {
                TtsEngineKind::MiniMax
            };
            routing.push((cleaned, engine));
        }

        assert!(
            routing
                .iter()
                .any(|(text, engine)| text.contains("你好") && *engine == TtsEngineKind::VolcEngine),
            "中文片段应路由到 VolcEngine: {routing:?}"
        );

        // 由于 min_chunk_length=10 的限制，短句会合并，西班牙语文本可能与中文合并为一个 chunk，
        // 此时中文占主导，整体路由到 VolcEngine
        assert!(
            routing.iter().any(|(text, _engine)| text.contains("es una frase en")),
            "应存在包含西班牙语的片段: {routing:?}"
        );
    }

    #[test]
    fn baidu_only_for_cn_or_en_under_specific_voice_id() {
        // 这里用人工 confidence，避免依赖 lingua 模型细节
        let conf_cn = vec![(Language::Chinese, 0.95), (Language::Spanish, 0.02)];
        let conf_es = vec![(Language::Spanish, 0.95), (Language::Chinese, 0.01)];

        // 检测确定为中文 → Baidu
        assert_eq!(
            select_tts_engine("你好世界", Some(BAIDU_TTS_VOICE_ID_DUHANZHU_BRIGHT), &conf_cn, None, None).engine,
            TtsEngineKind::Baidu
        );
        // 检测确定为西班牙语（非中/英） → MiniMax
        assert_eq!(
            select_tts_engine("Hola mundo", Some(BAIDU_TTS_VOICE_ID_DUHANZHU_BRIGHT), &conf_es, None, None).engine,
            TtsEngineKind::MiniMax
        );
    }

    #[test]
    fn baidu_voice_uncertain_fallback_to_baidu() {
        // 测试方案C：检测不确定时默认 Baidu
        let conf_uncertain = vec![(Language::English, 0.3), (Language::Spanish, 0.25)];

        // 无继承引擎时，不确定应默认 Baidu（方案C）
        let selection = select_tts_engine(
            "What's up?",
            Some(BAIDU_TTS_VOICE_ID_DUHANZHU_BRIGHT),
            &conf_uncertain,
            None,
            None,
        );
        assert_eq!(selection.engine, TtsEngineKind::Baidu);
        assert!(!selection.is_confident, "检测不确定时 is_confident 应为 false");
    }

    #[test]
    fn baidu_voice_uncertain_inherit_from_turn() {
        // 测试方案A：检测不确定时继承轮次内的路由
        let conf_uncertain = vec![(Language::English, 0.3), (Language::Spanish, 0.25)];

        // 有继承引擎时，应继承（方案A）
        let selection = select_tts_engine(
            "What's up?",
            Some(BAIDU_TTS_VOICE_ID_DUHANZHU_BRIGHT),
            &conf_uncertain,
            Some(TtsEngineKind::Baidu),
            None,
        );
        assert_eq!(selection.engine, TtsEngineKind::Baidu);
        assert!(!selection.is_confident, "继承时 is_confident 应为 false");
    }

    #[test]
    fn baidu_voice_confident_other_language_to_minimax() {
        // 测试：检测确定为非中英文 → MiniMax（确定性路由）
        let conf_spanish = vec![(Language::Spanish, 0.85), (Language::English, 0.05)];

        let selection = select_tts_engine(
            "Hola mundo",
            Some(BAIDU_TTS_VOICE_ID_DUHANZHU_BRIGHT),
            &conf_spanish,
            None,
            None,
        );
        assert_eq!(selection.engine, TtsEngineKind::MiniMax);
        assert!(selection.is_confident, "检测确定为非中英文时 is_confident 应为 true");
    }

    #[test]
    fn force_engine_overrides_all() {
        // 测试：force_engine 参数优先级最高
        let conf_cn = vec![(Language::Chinese, 0.95), (Language::Spanish, 0.02)];

        // 即使检测为中文，force_engine 为 EdgeTts 时也应返回 EdgeTts
        let selection = select_tts_engine(
            "你好世界",
            Some(BAIDU_TTS_VOICE_ID_DUHANZHU_BRIGHT),
            &conf_cn,
            None,
            Some(TtsEngineKind::EdgeTts),
        );
        assert_eq!(selection.engine, TtsEngineKind::EdgeTts);
        assert!(selection.is_confident, "force_engine 时 is_confident 应为 true");
    }
}
