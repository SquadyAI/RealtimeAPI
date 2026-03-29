//! Guided Choice Selector - LLM约束选择工具
//!
//! 该模块提供了一个基于vLLM的guided generation功能的选择器工具。
//! 它能够强制LLM从预定义的选项列表中选择一个输出，即使输入内容与选项无关。
//!
//! # 主要功能
//! - 接受自定义system prompt和choices列表
//! - 自动清洗历史消息中的system角色
//! - 使用guided_choice确保输出在指定范围内
//! - 适用于意图识别、情感分析、分类任务等场景
//!
//! # 使用示例
//! ```rust
//! use crate::rpc::pipeline::asr_llm_tts::guided_choice_selector::{GuidedChoiceSelector, SelectorConfig};
//!
//! let config = SelectorConfig {
//!     api_key: "your-api-key".to_string(),
//!     base_url: "http://localhost:8000/v1".to_string(),
//!     model: "Qwen/Qwen3-4B-AWQ".to_string(),
//!     timeout_secs: 30,
//! };
//!
//! let selector = GuidedChoiceSelector::new(config);
//!
//! let system_prompt = "你是一个情感分析助手，根据对话判断用户的情感状态。";
//! let choices = vec!["开心", "难过", "生气", "平静"];
//! let messages = vec![
//!     ChatMessage { role: Some("user".to_string()), content: Some("今天天气真好！".to_string()), ..Default::default() },
//! ];
//!
//! let result = selector.select(system_prompt, &choices, messages).await?;
//! println!("选择结果: {}", result); // 输出: "开心"
//! ```

use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, error, info, warn};

/// 选择器错误类型
#[derive(Debug, Error)]
pub enum SelectorError {
    #[error("配置错误: {0}")]
    ConfigError(String),
    #[error("API请求失败: {0}")]
    ApiError(String),
    #[error("响应解析失败: {0}")]
    ParseError(String),
    #[error("结果不在允许范围内: {0}")]
    InvalidChoice(String),
    #[error("HTTP客户端创建失败: {0}")]
    ClientError(String),
}

/// 选择器配置
#[derive(Debug, Clone)]
pub struct SelectorConfig {
    /// LLM API密钥
    pub api_key: String,
    /// LLM API基础URL
    pub base_url: String,
    /// 使用的模型名称
    pub model: String,
    /// 请求超时时间（秒）
    pub timeout_secs: u64,
}

impl SelectorConfig {
    /// 验证配置的有效性
    pub fn validate(&self) -> Result<(), SelectorError> {
        if self.api_key.is_empty() {
            return Err(SelectorError::ConfigError("API key不能为空".to_string()));
        }
        if !self.base_url.starts_with("http") {
            return Err(SelectorError::ConfigError("base_url格式无效，必须以http开头".to_string()));
        }
        if self.model.is_empty() {
            return Err(SelectorError::ConfigError("model不能为空".to_string()));
        }
        if self.timeout_secs == 0 {
            return Err(SelectorError::ConfigError("timeout_secs必须大于0".to_string()));
        }
        Ok(())
    }
}

impl Default for SelectorConfig {
    fn default() -> Self {
        Self {
            api_key: std::env::var("LLM_API_KEY").unwrap_or_else(|_| "EMPTY".to_string()),
            base_url: std::env::var("LLM_BASE_URL").unwrap_or_else(|_| "http://localhost:8000/v1".to_string()),
            model: std::env::var("LLM_MODEL").unwrap_or_else(|_| "Qwen/Qwen3-4B-AWQ".to_string()),
            timeout_secs: 30,
        }
    }
}

// 使用项目统一的ChatMessage类型
use crate::llm::llm::ChatMessage;

/// 聊天完成请求（支持guided_choice）
#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,

    // vLLM guided generation（直接字段）
    #[serde(skip_serializing_if = "Option::is_none")]
    guided_choice: Option<Vec<String>>,
}

/// 聊天完成响应
#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChatMessage,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

/// Guided Choice选择器
///
/// 该工具使用vLLM的guided generation功能，强制LLM从预定义的选项中选择输出。
pub struct GuidedChoiceSelector {
    config: SelectorConfig,
    client: Client,
}

impl GuidedChoiceSelector {
    /// 创建新的选择器实例
    pub fn new(config: SelectorConfig) -> Result<Self, SelectorError> {
        // 验证配置
        config.validate()?;

        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| SelectorError::ClientError(format!("HTTP客户端创建失败: {}", e)))?;

        Ok(Self { config, client })
    }

    /// 使用默认配置创建选择器
    pub fn new_default() -> Result<Self, SelectorError> {
        Self::new(SelectorConfig::default())
    }

    /// 执行选择操作
    ///
    /// # 参数
    /// - `system_prompt`: 系统提示词，定义选择器的行为
    /// - `choices`: 允许的选项列表（字符串数组）
    /// - `messages`: 输入的对话历史
    ///
    /// # 返回
    /// 返回choices中的一个选项
    ///
    /// # 错误
    /// - API请求失败
    /// - 响应解析失败
    /// - 返回的结果不在choices范围内（理论上不应发生）
    pub async fn select(&self, system_prompt: &str, choices: &[impl AsRef<str>], messages: Vec<ChatMessage>) -> Result<String, SelectorError> {
        // 转换choices为String Vec
        let choices_vec: Vec<String> = choices.iter().map(|s| s.as_ref().to_string()).collect();

        if choices_vec.is_empty() {
            return Err(SelectorError::ConfigError("choices不能为空".to_string()));
        }

        debug!(
            "🎯 Guided Choice Selector: system_prompt长度={}, choices数量={}, messages数量={}",
            system_prompt.len(),
            choices_vec.len(),
            messages.len()
        );

        // 清洗消息：移除所有role=system的消息
        let cleaned_messages = Self::clean_system_messages(messages);
        debug!("🧹 清洗后消息数量: {}", cleaned_messages.len());

        // 构建最终的消息列表：system prompt + 清洗后的历史
        let mut final_messages = vec![ChatMessage {
            role: Some("system".to_string()),
            content: Some(system_prompt.to_string()),
            tool_call_id: None,
            tool_calls: None,
        }];
        final_messages.extend(cleaned_messages);

        // 构建请求
        let request = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages: final_messages,
            stream: None,
            temperature: Some(0.0), // 使用0温度获得确定性输出
            max_tokens: Some(20),   // 足够输出一个选项
            guided_choice: Some(choices_vec.clone()),
        };

        // 发送请求
        let url = format!("{}/chat/completions", self.config.base_url);

        debug!("📤 发送请求到: {}", url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| SelectorError::ApiError(format!("HTTP请求失败: {}", e)))?;

        let status = response.status();

        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "无法读取错误信息".to_string());
            error!("❌ LLM API错误: status={}, body={}", status, error_text);
            return Err(SelectorError::ApiError(format!("LLM API返回错误: {}", error_text)));
        }

        // 解析响应
        let response_body: ChatCompletionResponse = response
            .json()
            .await
            .map_err(|e| SelectorError::ParseError(format!("响应解析失败: {}", e)))?;

        if let Some(choice) = response_body.choices.first() {
            let result = choice
                .message
                .content
                .as_ref()
                .ok_or_else(|| SelectorError::ParseError("响应中没有内容".to_string()))?
                .trim()
                .to_string();

            debug!("📥 LLM选择结果: {}", result);

            // 验证结果是否在choices范围内
            if !choices_vec.contains(&result) {
                warn!("⚠️ LLM返回的结果不在choices范围内: '{}', 可能guided_choice未生效", result);
                // 注意：理论上有guided_choice时这不应该发生
                return Err(SelectorError::InvalidChoice(format!(
                    "返回结果 '{}' 不在允许的选项范围内",
                    result
                )));
            }

            info!("✅ 成功选择: {} (从{}个选项中)", result, choices_vec.len());
            Ok(result)
        } else {
            Err(SelectorError::ParseError("响应中没有choices".to_string()))
        }
    }

    /// 清洗消息列表：移除所有role=system的消息
    fn clean_system_messages(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
        messages
            .into_iter()
            .filter(|msg| if let Some(role) = &msg.role { role != "system" } else { true })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_system_messages() {
        let messages = vec![
            ChatMessage {
                role: Some("system".to_string()),
                content: Some("系统消息1".to_string()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: Some("user".to_string()),
                content: Some("用户消息".to_string()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: Some("system".to_string()),
                content: Some("系统消息2".to_string()),
                tool_call_id: None,
                tool_calls: None,
            },
            ChatMessage {
                role: Some("assistant".to_string()),
                content: Some("助手消息".to_string()),
                tool_call_id: None,
                tool_calls: None,
            },
        ];

        let cleaned = GuidedChoiceSelector::clean_system_messages(messages);

        assert_eq!(cleaned.len(), 2);
        assert_eq!(cleaned[0].role.as_ref().unwrap(), "user");
        assert_eq!(cleaned[1].role.as_ref().unwrap(), "assistant");
    }

    #[tokio::test]
    #[ignore] // 需要真实的LLM服务器才能运行
    async fn test_selector_basic() {
        let config = SelectorConfig::default();
        let selector = GuidedChoiceSelector::new(config).unwrap();

        let system_prompt = "你是一个天气分析助手，根据描述选择天气类型。";
        let choices = vec!["晴天", "多云", "阴天", "雨天", "雪天"];
        let messages = vec![ChatMessage {
            role: Some("user".to_string()),
            content: Some("今天阳光明媚，万里无云。".to_string()),
            tool_call_id: None,
            tool_calls: None,
        }];

        let result = selector.select(system_prompt, &choices, messages).await;
        assert!(result.is_ok());

        let choice = result.unwrap();
        assert!(choices.contains(&choice.as_str()));
    }
}
