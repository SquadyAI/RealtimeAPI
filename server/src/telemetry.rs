//! 异步 OpenTelemetry 追踪模块 (Langfuse)
//!
//! 设计：
//! - 主流程通过 channel 发送事件，不等待结果
//! - 后台 worker 负责转换为 OTel span 并发送
//! - 通道满了或发送失败都不影响主流程
//!
//! 环境变量（按 Langfuse 文档的 OTEL 接入方式）：
//! - LANGFUSE_BASE_URL: 例如 `https://cloud.langfuse.com` 或自建域名（代码会拼到 `${BASE}/api/public/otel`）
//! - LANGFUSE_PUBLIC_KEY: pk
//! - LANGFUSE_SECRET_KEY: sk

use base64::Engine;
use rustc_hash::FxHashMap;
use std::future::Future;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// 当前任务的 Trace 上下文（用来让 LLM / intent 等深层模块也能拿到 session/turn）
#[derive(Debug, Clone)]
pub struct TraceContext {
    pub session_id: String,
    pub turn_id: String,
}

tokio::task_local! {
    static TRACE_CONTEXT: TraceContext;
}

/// 在当前 async 任务作用域内设置 trace context（不需要改动大量函数签名）。
pub async fn with_trace_context<T>(session_id: impl Into<String>, turn_id: impl Into<String>, fut: impl Future<Output = T>) -> T {
    TRACE_CONTEXT
        .scope(TraceContext { session_id: session_id.into(), turn_id: turn_id.into() }, fut)
        .await
}

/// 获取当前任务的 trace context；如果不在 scope 内则返回 None。
pub fn current_trace_context() -> Option<TraceContext> {
    TRACE_CONTEXT.try_with(|c| c.clone()).ok()
}

/// 追踪事件
#[derive(Debug)]
pub enum TraceEvent {
    /// 轮次开始
    TurnStart {
        session_id: String,
        turn_id: String,
        user_text: String,
        intent: Option<String>,
        agent_id: Option<String>,
    },
    /// 工具调用开始
    ToolCallStart {
        session_id: String,
        turn_id: String,
        call_id: String,
        name: String,
        arguments: String,
    },
    /// 工具调用完成
    ToolCallEnd {
        session_id: String,
        turn_id: String,
        call_id: String,
        name: String, // 工具名（原子化：不依赖 ToolCallStart）
        result: String,
        success: bool,
        duration_ms: u64,
    },
    /// 轮次结束
    TurnEnd {
        session_id: String,
        turn_id: String,
        assistant_response: Option<String>,
        duration_ms: u64,
        status: String, // completed, interrupted
        intent: Option<String>,
        agent_id: Option<String>,
        /// 用于排查/复盘：本轮及历史 messages（JSON string，可能被裁剪）
        history_messages_json: Option<String>,
    },
    /// LLM 调用
    LlmGeneration {
        session_id: String,
        turn_id: String,
        model: String,
        input_messages: usize,
        /// LLM 请求体（post body），JSON string（可能被裁剪）
        request_json: Option<String>,
        output_text: Option<String>,
        input_tokens: Option<u32>,
        output_tokens: Option<u32>,
        latency_ms: u64,
    },
    /// 意图识别调用（外部 intent api）
    IntentRecognition {
        session_id: String,
        turn_id: String,
        request_json: String,
        response_text: Option<String>,
        intent: Option<String>,
        latency_ms: u64,
        status_code: Option<u16>,
    },
}

static TRACE_SENDER: OnceLock<mpsc::Sender<TraceEvent>> = OnceLock::new();

/// 初始化追踪系统（在 main 启动时调用）
pub fn init() {
    // 仅支持 Langfuse 标准三件套（你 env 里就是这种写法）
    let base = match std::env::var("LANGFUSE_BASE_URL") {
        Ok(v) if !v.trim().is_empty() => v.trim().trim_end_matches('/').to_string(),
        _ => {
            info!("📊 Langfuse 追踪已禁用 (LANGFUSE_BASE_URL 未设置)");
            return;
        },
    };

    let pk = match std::env::var("LANGFUSE_PUBLIC_KEY") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => {
            warn!("⚠️ Langfuse 追踪已禁用 (LANGFUSE_PUBLIC_KEY 未设置)");
            return;
        },
    };

    let sk = match std::env::var("LANGFUSE_SECRET_KEY") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => {
            warn!("⚠️ Langfuse 追踪已禁用 (LANGFUSE_SECRET_KEY 未设置)");
            return;
        },
    };

    // Langfuse OTEL endpoint: /api/public/otel
    let endpoint = if base.ends_with("/api/public/otel") {
        base
    } else {
        format!("{}/api/public/otel", base)
    };

    // Basic auth = base64("pk:sk")
    let auth = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", pk, sk));

    let (tx, rx) = mpsc::channel::<TraceEvent>(2000);
    if TRACE_SENDER.set(tx).is_err() {
        warn!("⚠️ Telemetry 已初始化，跳过重复初始化");
        return;
    }

    // 启动后台 worker
    tokio::spawn(async move {
        trace_worker(rx, endpoint, auth).await;
    });

    info!("✅ Langfuse 追踪已启用");
}

/// 非阻塞发送事件（失败静默丢弃）
#[inline]
pub fn emit(event: TraceEvent) {
    if let Some(tx) = TRACE_SENDER.get() {
        let _ = tx.try_send(event);
    }
}

/// 检查追踪是否已启用
#[inline]
pub fn is_enabled() -> bool {
    TRACE_SENDER.get().is_some()
}

// ============================================================================
// 辅助宏：简化 emit 调用
// ============================================================================

/// 轮次开始追踪
#[macro_export]
macro_rules! trace_turn_start {
    ($session_id:expr, $turn_id:expr, $user_text:expr) => {
        $crate::telemetry::emit($crate::telemetry::TraceEvent::TurnStart {
            session_id: $session_id.to_string(),
            turn_id: $turn_id.to_string(),
            user_text: $user_text.to_string(),
            intent: None,
            agent_id: None,
        });
    };
    ($session_id:expr, $turn_id:expr, $user_text:expr, $intent:expr, $agent_id:expr) => {
        $crate::telemetry::emit($crate::telemetry::TraceEvent::TurnStart {
            session_id: $session_id.to_string(),
            turn_id: $turn_id.to_string(),
            user_text: $user_text.to_string(),
            intent: $intent,
            agent_id: $agent_id,
        });
    };
}

/// 轮次结束追踪
#[macro_export]
macro_rules! trace_turn_end {
    ($session_id:expr, $turn_id:expr, $response:expr, $duration_ms:expr, $status:expr) => {
        $crate::telemetry::emit($crate::telemetry::TraceEvent::TurnEnd {
            session_id: $session_id.to_string(),
            turn_id: $turn_id.to_string(),
            assistant_response: $response,
            duration_ms: $duration_ms,
            status: $status.to_string(),
            intent: None,
            agent_id: None,
            history_messages_json: None,
        });
    };
}

/// 工具调用开始追踪
#[macro_export]
macro_rules! trace_tool_start {
    ($session_id:expr, $turn_id:expr, $call_id:expr, $name:expr, $args:expr) => {
        $crate::telemetry::emit($crate::telemetry::TraceEvent::ToolCallStart {
            session_id: $session_id.to_string(),
            turn_id: $turn_id.to_string(),
            call_id: $call_id.to_string(),
            name: $name.to_string(),
            arguments: $args.to_string(),
        });
    };
}

/// 工具调用结束追踪
#[macro_export]
macro_rules! trace_tool_end {
    ($session_id:expr, $turn_id:expr, $call_id:expr, $name:expr, $result:expr, $success:expr, $duration_ms:expr) => {
        $crate::telemetry::emit($crate::telemetry::TraceEvent::ToolCallEnd {
            session_id: $session_id.to_string(),
            turn_id: $turn_id.to_string(),
            call_id: $call_id.to_string(),
            name: $name.to_string(),
            result: $result.to_string(),
            success: $success,
            duration_ms: $duration_ms,
        });
    };
}

/// LLM 调用追踪
#[macro_export]
macro_rules! trace_llm {
    ($session_id:expr, $turn_id:expr, $model:expr, $input_msgs:expr, $output:expr, $latency_ms:expr) => {
        $crate::telemetry::emit($crate::telemetry::TraceEvent::LlmGeneration {
            session_id: $session_id.to_string(),
            turn_id: $turn_id.to_string(),
            model: $model.to_string(),
            input_messages: $input_msgs,
            request_json: None,
            output_text: $output,
            input_tokens: None,
            output_tokens: None,
            latency_ms: $latency_ms,
        });
    };
    ($session_id:expr, $turn_id:expr, $model:expr, $input_msgs:expr, $output:expr, $input_tokens:expr, $output_tokens:expr, $latency_ms:expr) => {
        $crate::telemetry::emit($crate::telemetry::TraceEvent::LlmGeneration {
            session_id: $session_id.to_string(),
            turn_id: $turn_id.to_string(),
            model: $model.to_string(),
            input_messages: $input_msgs,
            request_json: None,
            output_text: $output,
            input_tokens: $input_tokens,
            output_tokens: $output_tokens,
            latency_ms: $latency_ms,
        });
    };
}

// ============================================================================
// 后台 Worker
// ============================================================================

async fn trace_worker(mut rx: mpsc::Receiver<TraceEvent>, endpoint: String, auth: String) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("⚠️ Telemetry HTTP client 创建失败: {}", e);
            return;
        },
    };

    let traces_endpoint = format!("{}/v1/traces", endpoint.trim_end_matches('/'));
    info!("📤 Langfuse OTLP endpoint: {}", traces_endpoint);

    // 活跃的 span（用于关联 turn 和 tool_call）
    let mut active_turns: FxHashMap<String, SpanContext> = FxHashMap::default();
    let mut active_tools: FxHashMap<String, SpanContext> = FxHashMap::default();

    // 防止极端情况下 start/end 不成对导致 map 残留增长（固定 TTL，不可配置，保持行为唯一）
    const ACTIVE_CTX_TTL_NS: u64 = 10 * 60 * 1_000_000_000; // 10 minutes

    // 批量发送
    let mut batch: Vec<serde_json::Value> = Vec::with_capacity(100);
    let mut last_flush = Instant::now();
    let flush_interval = Duration::from_secs(2);
    let max_batch_size = 50;

    loop {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some(event)) => {
                if let Some(span) = process_event(event, &mut active_turns, &mut active_tools) {
                    batch.push(span);
                }

                if batch.len() >= max_batch_size || last_flush.elapsed() > flush_interval {
                    flush_batch(&client, &traces_endpoint, &auth, &mut batch).await;
                    last_flush = Instant::now();

                    // 周期性清理（避免 active map 长期残留）
                    purge_old_contexts(&mut active_turns, &mut active_tools, ACTIVE_CTX_TTL_NS);
                }
            },
            Ok(None) => break, // channel 关闭
            Err(_) => {
                // 超时，检查是否需要 flush
                if !batch.is_empty() && last_flush.elapsed() > flush_interval {
                    flush_batch(&client, &traces_endpoint, &auth, &mut batch).await;
                    last_flush = Instant::now();

                    purge_old_contexts(&mut active_turns, &mut active_tools, ACTIVE_CTX_TTL_NS);
                }
            },
        }
    }

    // 最后 flush
    if !batch.is_empty() {
        flush_batch(&client, &traces_endpoint, &auth, &mut batch).await;
    }

    info!("📊 Telemetry worker 已停止");
}

#[derive(Debug, Clone)]
struct SpanContext {
    trace_id: String,
    span_id: String,
    #[allow(dead_code)]
    parent_span_id: Option<String>,
    start_time_ns: u64,
    /// 工具名（仅 tool span 使用，用于 ToolCallEnd 时恢复 span name）
    #[allow(dead_code)]
    tool_name: Option<String>,
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out = String::with_capacity(max_chars + 16);
    for (i, ch) in s.chars().enumerate() {
        if i >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.push_str("...(truncated)");
    out
}

fn maybe_truncate(s: &str) -> String {
    // “最好不要 truncate”：默认不裁剪。
    // 但为了避免极端情况下单个 attribute 超大导致：
    // - OTLP HTTP 请求体过大被反向代理/后端拒绝
    // - 内存占用爆炸（history/messages 特别容易膨胀）
    // 这里保留一个硬保护上限。
    const HARD_MAX_CHARS: usize = 2_000_000;

    let chars = s.chars().count();
    if chars > HARD_MAX_CHARS {
        return truncate(s, HARD_MAX_CHARS);
    }
    s.to_string()
}

fn now_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64
}

fn purge_old_contexts(active_turns: &mut FxHashMap<String, SpanContext>, active_tools: &mut FxHashMap<String, SpanContext>, ttl_ns: u64) {
    let now = now_ns();
    active_turns.retain(|_, ctx| now.saturating_sub(ctx.start_time_ns) <= ttl_ns);
    active_tools.retain(|_, ctx| now.saturating_sub(ctx.start_time_ns) <= ttl_ns);
}

fn process_event(event: TraceEvent, active_turns: &mut FxHashMap<String, SpanContext>, active_tools: &mut FxHashMap<String, SpanContext>) -> Option<serde_json::Value> {
    match event {
        TraceEvent::TurnStart { session_id, turn_id, user_text, intent, agent_id } => {
            let trace_id = trace_id(&session_id);
            let span_id = span_id(&turn_id);
            let start_time = now_ns();

            active_turns.insert(
                turn_id.clone(),
                SpanContext {
                    trace_id: trace_id.clone(),
                    span_id: span_id.clone(),
                    parent_span_id: None, // turn 是根 span
                    start_time_ns: start_time,
                    tool_name: None,
                },
            );

            let mut attrs = vec![
                attr_str("langfuse.session.id", &session_id),
                attr_str("langfuse.trace.name", "conversation"),
                attr_str("langfuse.observation.type", "span"),
                attr_str("langfuse.observation.input", &user_text),
                attr_str("turn.id", &turn_id),
            ];
            if let Some(i) = intent {
                attrs.push(attr_str("turn.intent", &i));
            }
            if let Some(a) = agent_id {
                attrs.push(attr_str("turn.agent_id", &a));
            }

            Some(span_json(&trace_id, &span_id, None, "turn", start_time, start_time, attrs))
        },

        TraceEvent::TurnEnd {
            session_id,
            turn_id,
            assistant_response,
            duration_ms,
            status,
            intent,
            agent_id,
            history_messages_json,
        } => {
            let end_time = now_ns();

            // 原子化：所有信息都能从事件本身推导，不强依赖 active_turns
            // - trace_id: 由 session_id 确定性生成
            // - span_id: 由 turn_id 确定性生成（和 TurnStart 规则一致）
            // - start_time: 优先用 active_turns 里存的精确值，否则用 duration_ms 反推
            let tid = trace_id(&session_id);
            let sid = span_id(&turn_id);
            let start_time_ns = active_turns
                .remove(&turn_id)
                .map(|ctx| ctx.start_time_ns)
                .unwrap_or_else(|| end_time.saturating_sub(duration_ms * 1_000_000));

            let mut attrs = vec![
                attr_str("langfuse.session.id", &session_id),
                // 保证 trace name 稳定：即使 TurnStart 丢了，TurnEnd 也能把 trace 命名为 conversation
                attr_str("langfuse.trace.name", "conversation"),
                attr_str("turn.status", &status),
            ];
            if let Some(resp) = &assistant_response {
                attrs.push(attr_str("langfuse.observation.output", &maybe_truncate(resp)));
            }
            attrs.push(attr_int("turn.duration_ms", duration_ms as i64));
            if let Some(i) = intent {
                // 既保留 turn 级别，也提供 trace metadata 方便筛选
                attrs.push(attr_str("turn.intent", &i));
                attrs.push(attr_str("langfuse.trace.metadata.intent", &i));
            }
            if let Some(a) = agent_id {
                attrs.push(attr_str("turn.agent_id", &a));
                attrs.push(attr_str("langfuse.trace.metadata.agent_id", &a));
            }
            if let Some(h) = history_messages_json {
                attrs.push(attr_str("langfuse.trace.metadata.history_messages", &maybe_truncate(&h)));
            }

            Some(span_json(&tid, &sid, None, "turn", start_time_ns, end_time, attrs))
        },

        TraceEvent::ToolCallStart { session_id, turn_id, call_id, name, arguments: _arguments } => {
            let parent_ctx = active_turns.get(&turn_id);
            let tid = parent_ctx.map(|c| c.trace_id.clone()).unwrap_or_else(|| trace_id(&session_id));
            let parent_span_id = parent_ctx.map(|c| c.span_id.clone());
            let sid = span_id(&call_id);
            let start_time = now_ns();

            active_tools.insert(
                call_id.clone(),
                SpanContext {
                    trace_id: tid,
                    span_id: sid,
                    parent_span_id,
                    start_time_ns: start_time,
                    tool_name: Some(name), // 存储工具名，ToolCallEnd 时用来构建 span name
                },
            );

            // ToolCallStart 不发 span：只在 ToolCallEnd 时发一个完整的 span（带正确的 start/end 时间）
            // 这样每个工具只有一条 span，Langfuse 按 `tool.{name}` 分组统计 latency
            None
        },

        TraceEvent::ToolCallEnd { session_id, turn_id, call_id, name, result, success, duration_ms } => {
            let end_time = now_ns();

            // 原子化：所有信息都能从事件本身推导，不强依赖 active_tools
            // - trace_id: 由 session_id 确定性生成
            // - span_id: 由 call_id 确定性生成
            // - parent_span_id: 由 turn_id 确定性生成（和 TurnStart 规则一致）
            // - start_time: 优先用 active_tools 里存的精确值，否则用 duration_ms 反推

            let tid = trace_id(&session_id);
            let sid = span_id(&call_id);
            let parent_sid = span_id(&turn_id); // 确定性：和 TurnStart 的 span_id 规则一致

            // 如果 ToolCallStart 存过精确的 start_time，就用；否则用 duration_ms 反推
            let start_time_ns = active_tools
                .remove(&call_id)
                .map(|ctx| ctx.start_time_ns)
                .unwrap_or_else(|| end_time.saturating_sub(duration_ms * 1_000_000));

            let span_name = format!("tool.{}", name);

            let attrs = vec![
                attr_str("langfuse.observation.type", "span"),
                attr_str("langfuse.session.id", &session_id),
                attr_str("tool.name", &name),
                attr_str("langfuse.observation.output", &maybe_truncate(&result)),
                attr_bool("tool.success", success),
                attr_int("tool.duration_ms", duration_ms as i64),
            ];

            Some(span_json(
                &tid,
                &sid,
                Some(&parent_sid),
                &span_name,
                start_time_ns,
                end_time,
                attrs,
            ))
        },

        TraceEvent::LlmGeneration {
            session_id,
            turn_id,
            model,
            input_messages,
            request_json,
            output_text,
            input_tokens,
            output_tokens,
            latency_ms,
        } => {
            // 原子化：所有信息都能从事件本身推导，不强依赖 active_turns
            let tid = trace_id(&session_id);
            let parent_sid = span_id(&turn_id); // 确定性：和 TurnStart 的 span_id 规则一致
            let sid = span_id(&format!("llm_{}_{}", turn_id, now_ns()));
            let end_time = now_ns();
            let start_time = end_time.saturating_sub(latency_ms * 1_000_000);

            let mut attrs = vec![
                attr_str("langfuse.observation.type", "generation"),
                attr_str("langfuse.session.id", &session_id),
                attr_str("gen_ai.system", "openai-compatible"),
                attr_str("gen_ai.request.model", &model),
                attr_str("langfuse.observation.model.name", &model),
                attr_int("gen_ai.request.messages_count", input_messages as i64),
            ];
            if let Some(req) = request_json {
                attrs.push(attr_str("langfuse.observation.input", &maybe_truncate(&req)));
            }

            if let Some(out) = &output_text {
                attrs.push(attr_str("langfuse.observation.output", &maybe_truncate(out)));
            }
            if let Some(inp) = input_tokens {
                attrs.push(attr_int("gen_ai.usage.input_tokens", inp as i64));
            }
            if let Some(out) = output_tokens {
                attrs.push(attr_int("gen_ai.usage.output_tokens", out as i64));
            }
            attrs.push(attr_int("gen_ai.latency_ms", latency_ms as i64));

            Some(span_json(
                &tid,
                &sid,
                Some(&parent_sid),
                "llm.generation",
                start_time,
                end_time,
                attrs,
            ))
        },

        TraceEvent::IntentRecognition {
            session_id,
            turn_id,
            request_json,
            response_text,
            intent,
            latency_ms,
            status_code,
        } => {
            // 原子化：所有信息都能从事件本身推导，不强依赖 active_turns
            let tid = trace_id(&session_id);
            let parent_sid = span_id(&turn_id); // 确定性：和 TurnStart 的 span_id 规则一致
            let sid = span_id(&format!("intent_{}_{}", turn_id, now_ns()));
            let end_time = now_ns();
            let start_time = end_time.saturating_sub(latency_ms * 1_000_000);

            let mut attrs = vec![
                attr_str("langfuse.observation.type", "span"),
                attr_str("langfuse.session.id", &session_id),
                attr_str("intent.latency_ms", &latency_ms.to_string()),
            ];
            if let Some(code) = status_code {
                attrs.push(attr_int("http.status_code", code as i64));
            }
            if let Some(i) = &intent {
                attrs.push(attr_str("turn.intent", i));
                attrs.push(attr_str("langfuse.trace.metadata.intent", i));
            }
            attrs.push(attr_str("langfuse.observation.input", &maybe_truncate(&request_json)));
            if let Some(resp) = response_text {
                attrs.push(attr_str("langfuse.observation.output", &maybe_truncate(&resp)));
            }

            Some(span_json(
                &tid,
                &sid,
                Some(&parent_sid),
                "intent.recognize",
                start_time,
                end_time,
                attrs,
            ))
        },
    }
}

async fn flush_batch(client: &reqwest::Client, endpoint: &str, auth: &str, batch: &mut Vec<serde_json::Value>) {
    if batch.is_empty() {
        return;
    }

    let spans_count = batch.len();
    let payload = serde_json::json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [
                    {"key": "service.name", "value": {"stringValue": "realtime-llm"}},
                    {"key": "deployment.environment", "value": {"stringValue": std::env::var("ENVIRONMENT").unwrap_or_else(|_| "production".to_string())}}
                ]
            },
            "scopeSpans": [{
                "scope": {"name": "turn_tracker", "version": "1.0"},
                "spans": std::mem::take(batch)
            }]
        }]
    });

    let result = client
        .post(endpoint)
        .header("Authorization", format!("Basic {}", auth))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await;

    match result {
        Ok(resp) if resp.status().is_success() => {
            debug!("📤 Langfuse: 已发送 {} 个 span", spans_count);
        },
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            debug!("⚠️ Langfuse 响应 {}: {}", status, body);
        },
        Err(e) => {
            // builder error 通常是 endpoint URL 不合法（最常见：少了 http(s)://）
            debug!("⚠️ Langfuse 发送失败: endpoint={}, err={:?}", endpoint, e);
        },
    }
}

// ============================================================================
// JSON 构建辅助函数
// ============================================================================

fn span_json(trace_id: &str, span_id: &str, parent_span_id: Option<&str>, name: &str, start_time_ns: u64, end_time_ns: u64, attributes: Vec<serde_json::Value>) -> serde_json::Value {
    let mut span = serde_json::json!({
        "traceId": trace_id,
        "spanId": span_id,
        "name": name,
        "kind": 1, // SPAN_KIND_INTERNAL
        "startTimeUnixNano": start_time_ns.to_string(),
        "endTimeUnixNano": end_time_ns.to_string(),
        "attributes": attributes,
        "status": {"code": 1} // STATUS_CODE_OK
    });

    if let Some(parent) = parent_span_id {
        span["parentSpanId"] = serde_json::Value::String(parent.to_string());
    }

    span
}

fn attr_str(key: &str, value: &str) -> serde_json::Value {
    serde_json::json!({
        "key": key,
        "value": {"stringValue": value}
    })
}

fn attr_int(key: &str, value: i64) -> serde_json::Value {
    serde_json::json!({
        "key": key,
        "value": {"intValue": value.to_string()}
    })
}

fn attr_bool(key: &str, value: bool) -> serde_json::Value {
    serde_json::json!({
        "key": key,
        "value": {"boolValue": value}
    })
}

/// 生成 traceId: 32 hex chars (16 bytes)
fn trace_id(s: &str) -> String {
    use md5::{Digest, Md5};
    let hash = Md5::digest(s.as_bytes());
    hex::encode(hash) // MD5 = 32 hex chars
}

/// 生成 spanId: 16 hex chars (8 bytes)
fn span_id(s: &str) -> String {
    use md5::{Digest, Md5};
    let hash = Md5::digest(s.as_bytes());
    hex::encode(&hash[..8]) // 取前 8 bytes = 16 hex chars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_and_span_id() {
        // trace_id: 32 hex chars
        let tid1 = trace_id("session_123");
        let tid2 = trace_id("session_123");
        let tid3 = trace_id("session_456");
        assert_eq!(tid1, tid2);
        assert_ne!(tid1, tid3);
        assert_eq!(tid1.len(), 32); // 16 bytes = 32 hex chars

        // span_id: 16 hex chars
        let sid1 = span_id("turn_abc");
        let sid2 = span_id("turn_abc");
        let sid3 = span_id("turn_xyz");
        assert_eq!(sid1, sid2);
        assert_ne!(sid1, sid3);
        assert_eq!(sid1.len(), 16); // 8 bytes = 16 hex chars
    }

    #[test]
    fn test_emit_without_init() {
        // emit 在未初始化时不应 panic
        emit(TraceEvent::TurnStart {
            session_id: "test".into(),
            turn_id: "t1".into(),
            user_text: "hello".into(),
            intent: None,
            agent_id: None,
        });
    }
}
