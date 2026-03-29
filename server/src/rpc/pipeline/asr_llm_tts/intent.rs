use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, error, warn};

use crate::llm::llm::ChatMessage;
use crate::telemetry;

/// Wiki 上下文信息
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WikiContext {
    pub title: String,
    pub content: String,
    pub score: f64,
}

/// 意图识别响应的 payload 部分
#[derive(Debug, Deserialize)]
pub struct IntentPayload {
    wiki_context: Option<WikiContext>,
}

/// 意图识别响应
#[derive(Debug, Deserialize)]
pub struct IntentResponse {
    pub intent: Option<String>,
    pub payload: Option<IntentPayload>,
}

/// 意图识别结果（包含意图和 wiki 上下文）
#[derive(Debug, Clone)]
pub struct IntentResult {
    pub intent: Option<String>,
    pub wiki_context: Option<WikiContext>,
}

/// 意图识别请求
#[derive(Debug, Serialize)]
struct IntentRequest {
    conversation: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
}

/// 意图识别客户端
#[derive(Clone)]
pub struct IntentClient {
    client: Client,
    api_url: String,
}

impl IntentClient {
    pub fn new(api_url: &str) -> Self {
        debug!("🔧 初始化意图识别客户端: API_URL={}, 超时=800ms", api_url);
        Self {
            client: Client::builder()
                .timeout(Duration::from_millis(800)) // 设置较短的超时时间，保证响应速度
                .build()
                .unwrap_or_default(),
            api_url: api_url.to_string(),
        }
    }

    /// 识别意图（返回意图和可能的 wiki 上下文）
    pub async fn recognize(&self, history: &[ChatMessage], language: Option<&str>) -> IntentResult {
        debug!("🎯 开始意图识别: 原始历史消息数={}, language={:?}", history.len(), language);
        // 构造请求体，仅取最近的 N 条消息以减少 payload
        let request = IntentRequest { conversation: history.to_vec(), language: language.map(|s| s.to_string()) };

        let start = std::time::Instant::now();
        let request_json = serde_json::to_string_pretty(&request).unwrap_or_else(|_| "序列化失败".to_string());
        debug!("📤 意图识别请求体: {}", request_json);
        debug!("🌐 发送POST请求到: {}", self.api_url);
        match self.client.post(&self.api_url).json(&request).send().await {
            Ok(resp) => {
                let status = resp.status();
                debug!("📥 收到HTTP响应: status={}", status);
                if status.is_success() {
                    // 尝试获取响应文本用于调试
                    match resp.text().await {
                        Ok(response_text) => {
                            debug!("📄 原始响应体: {}", response_text);
                            // 尝试解析JSON
                            match serde_json::from_str::<IntentResponse>(&response_text) {
                                Ok(intent_resp) => {
                                    let wiki_context = intent_resp.payload.and_then(|p| p.wiki_context);
                                    debug!(
                                        "🧠 意图识别成功: intent={:?}, has_wiki={} (耗时: {:?})",
                                        intent_resp.intent,
                                        wiki_context.is_some(),
                                        start.elapsed()
                                    );
                                    if let Some(ctx) = telemetry::current_trace_context() {
                                        telemetry::emit(telemetry::TraceEvent::IntentRecognition {
                                            session_id: ctx.session_id,
                                            turn_id: ctx.turn_id,
                                            request_json: request_json.clone(),
                                            response_text: Some(response_text.clone()),
                                            intent: intent_resp.intent.clone(),
                                            latency_ms: start.elapsed().as_millis() as u64,
                                            status_code: Some(status.as_u16()),
                                        });
                                    }
                                    IntentResult { intent: intent_resp.intent, wiki_context }
                                },
                                Err(e) => {
                                    error!("❌ 意图识别解析失败: {} (耗时: {:?})", e, start.elapsed());
                                    error!("❌ 响应内容: {}", response_text);
                                    if let Some(ctx) = telemetry::current_trace_context() {
                                        telemetry::emit(telemetry::TraceEvent::IntentRecognition {
                                            session_id: ctx.session_id,
                                            turn_id: ctx.turn_id,
                                            request_json: request_json.clone(),
                                            response_text: Some(response_text.clone()),
                                            intent: None,
                                            latency_ms: start.elapsed().as_millis() as u64,
                                            status_code: Some(status.as_u16()),
                                        });
                                    }
                                    IntentResult { intent: None, wiki_context: None }
                                },
                            }
                        },
                        Err(e) => {
                            error!("❌ 读取响应体失败: {} (耗时: {:?})", e, start.elapsed());
                            if let Some(ctx) = telemetry::current_trace_context() {
                                telemetry::emit(telemetry::TraceEvent::IntentRecognition {
                                    session_id: ctx.session_id,
                                    turn_id: ctx.turn_id,
                                    request_json: request_json.clone(),
                                    response_text: None,
                                    intent: None,
                                    latency_ms: start.elapsed().as_millis() as u64,
                                    status_code: Some(status.as_u16()),
                                });
                            }
                            IntentResult { intent: None, wiki_context: None }
                        },
                    }
                } else {
                    // 尝试读取错误响应体
                    match resp.text().await {
                        Ok(error_text) => {
                            warn!("⚠️ 意图识别请求失败: status={} (耗时: {:?})", status, start.elapsed());
                            warn!("⚠️ 错误响应体: {}", error_text);
                            if let Some(ctx) = telemetry::current_trace_context() {
                                telemetry::emit(telemetry::TraceEvent::IntentRecognition {
                                    session_id: ctx.session_id,
                                    turn_id: ctx.turn_id,
                                    request_json: request_json.clone(),
                                    response_text: Some(error_text),
                                    intent: None,
                                    latency_ms: start.elapsed().as_millis() as u64,
                                    status_code: Some(status.as_u16()),
                                });
                            }
                        },
                        Err(_) => {
                            warn!("⚠️ 意图识别请求失败: status={} (耗时: {:?})", status, start.elapsed());
                            if let Some(ctx) = telemetry::current_trace_context() {
                                telemetry::emit(telemetry::TraceEvent::IntentRecognition {
                                    session_id: ctx.session_id,
                                    turn_id: ctx.turn_id,
                                    request_json: request_json.clone(),
                                    response_text: None,
                                    intent: None,
                                    latency_ms: start.elapsed().as_millis() as u64,
                                    status_code: Some(status.as_u16()),
                                });
                            }
                        },
                    }
                    IntentResult { intent: None, wiki_context: None }
                }
            },
            Err(e) => {
                warn!("⚠️ 意图识别请求错误: {} (耗时: {:?})", e, start.elapsed());
                warn!("⚠️ 请求URL: {}", self.api_url);
                if let Some(ctx) = telemetry::current_trace_context() {
                    telemetry::emit(telemetry::TraceEvent::IntentRecognition {
                        session_id: ctx.session_id,
                        turn_id: ctx.turn_id,
                        request_json,
                        response_text: Some(format!("request error: {}", e)),
                        intent: None,
                        latency_ms: start.elapsed().as_millis() as u64,
                        status_code: None,
                    });
                }
                IntentResult { intent: None, wiki_context: None }
            },
        }
    }
}
