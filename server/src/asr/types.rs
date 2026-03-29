use std::str::FromStr;

// ── ASR shared data types ───────────────────────────────────────────────────
// Canonical ASR result types used by all backends.

/// Supported languages for speech recognition.
#[derive(Debug, Copy, Clone)]
pub enum AsrLanguage {
    /// English
    En,
    /// Chinese (Mandarin)
    Zh,
    /// Cantonese
    Yue,
    /// Japanese
    Ja,
    /// Korean
    Ko,
    /// Auto detect or unknown language
    Auto,
}

impl FromStr for AsrLanguage {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "zh" | "zh-cn" | "zh-tw" | "zh-hans" | "zh-hant" | "chinese" | "mandarin" => Ok(AsrLanguage::Zh),
            "en" | "en-us" | "en-gb" | "english" => Ok(AsrLanguage::En),
            "yue" | "yue-hk" | "cantonese" => Ok(AsrLanguage::Yue),
            "ja" | "ja-jp" | "japanese" => Ok(AsrLanguage::Ja),
            "ko" | "ko-kr" | "korean" => Ok(AsrLanguage::Ko),
            "auto" | "autodetect" => Ok(AsrLanguage::Auto),
            _ => Err(format!("Unsupported language: {}", s)),
        }
    }
}

impl std::fmt::Display for AsrLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AsrLanguage::Zh => write!(f, "zh"),
            AsrLanguage::En => write!(f, "en"),
            AsrLanguage::Yue => write!(f, "yue"),
            AsrLanguage::Ja => write!(f, "ja"),
            AsrLanguage::Ko => write!(f, "ko"),
            AsrLanguage::Auto => write!(f, "auto"),
        }
    }
}

/// Possible emotions detected in speech.
#[derive(Debug, Copy, Clone)]
pub enum AsrEmotion {
    Happy,
    Sad,
    Angry,
    Neutral,
    Fearful,
    Disgusted,
    Surprised,
    Unknown,
}

/// Types of audio events detected in speech.
#[derive(Debug, Copy, Clone)]
pub enum AsrEvent {
    Bgm,
    Speech,
    Applause,
    Laughter,
    Cry,
    Sneeze,
    Breath,
    Cough,
    Unknown,
}

/// Whether punctuation is included in transcribed text.
#[derive(Debug, Copy, Clone)]
pub enum PunctuationMode {
    With,
    Without,
}

/// Token-level timestamp information
#[derive(Debug, Clone)]
pub struct TokenSpan {
    pub text: String,
    pub start_ms: f64,
    pub end_ms: f64,
    pub confidence: f32,
}

/// Word-level timestamp information (grouped from tokens)
#[derive(Debug, Clone)]
pub struct WordSpan {
    pub word: String,
    pub start_ms: f64,
    pub end_ms: f64,
    pub confidence: f32,
}

/// A segment of audio with its transcribed text and associated metadata.
#[derive(Debug)]
pub struct VoiceText {
    pub language: AsrLanguage,
    pub emotion: AsrEmotion,
    pub event: AsrEvent,
    pub punctuation_normalization: PunctuationMode,
    pub content: String,
    pub token_spans: Option<Vec<TokenSpan>>,
    pub word_spans: Option<Vec<WordSpan>>,
}

/// Create a plain VoiceText from final text content with sensible defaults.
pub fn voice_text_from_text(text: String) -> VoiceText {
    VoiceText {
        language: AsrLanguage::Auto,
        emotion: AsrEmotion::Neutral,
        event: AsrEvent::Speech,
        punctuation_normalization: PunctuationMode::With,
        content: text,
        token_spans: None,
        word_spans: None,
    }
}
