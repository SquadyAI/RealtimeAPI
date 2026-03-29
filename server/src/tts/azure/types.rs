//! Azure TTS 类型定义

use std::fmt;

/// Azure TTS 错误
#[derive(Debug)]
pub enum AzureTtsError {
    /// 配置错误（缺少环境变量等）
    Config(String),
    /// HTTP 请求错误
    Http(String),
    /// 认证错误
    Auth(String),
    /// 音频解码错误
    Decode(String),
    /// 不支持的语言
    UnsupportedLanguage(String),
}

impl fmt::Display for AzureTtsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AzureTtsError::Config(msg) => write!(f, "Azure TTS 配置错误: {}", msg),
            AzureTtsError::Http(msg) => write!(f, "Azure TTS HTTP 错误: {}", msg),
            AzureTtsError::Auth(msg) => write!(f, "Azure TTS 认证错误: {}", msg),
            AzureTtsError::Decode(msg) => write!(f, "Azure TTS 解码错误: {}", msg),
            AzureTtsError::UnsupportedLanguage(lang) => {
                write!(f, "Azure TTS 不支持的语言: {}", lang)
            },
        }
    }
}

impl std::error::Error for AzureTtsError {}
