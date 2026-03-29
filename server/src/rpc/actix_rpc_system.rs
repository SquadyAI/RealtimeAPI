//! Actix-Web RPC系统
//! 提供基于actix-web的RPC服务器，替换axum版本

use crate::AsrEngine;
use crate::llm::LlmClient;
use crate::rpc::WebSocketSessionManager;
use crate::rpc::actix_websocket::{ActixAppState, actix_websocket_handler};
use actix_cors::Cors;
use actix_files::Files;
use actix_web::{App, HttpServer, middleware, web};
use anyhow::Result;
use std::sync::Arc;
use tracing::{error, info};

use super::RpcConfig;

/// 健康检查响应模型
#[derive(serde::Serialize)]
pub struct HealthResponse {
    /// 服务状态
    status: String,
    /// 服务名称
    service: String,
    /// 时间戳
    timestamp: String,
}

/// 状态检查响应模型
#[derive(serde::Serialize)]
pub struct StatusResponse {
    /// 服务状态
    status: String,
    /// 服务名称
    service: String,
    /// 活跃会话数
    active_sessions: usize,
    /// 时间戳
    timestamp: String,
    /// 框架名称
    framework: String,
}

/// MCP缓存调试响应模型
#[derive(serde::Serialize)]
pub struct McpCacheDebugResponse {
    /// MCP缓存统计信息
    mcp_cache_stats: serde_json::Value,
    /// 时间戳
    timestamp: String,
}

/// 声音库配置更新请求模型
#[derive(serde::Deserialize)]
pub struct VoiceLibraryConfigRequest {
    /// 配置JSON
    #[allow(dead_code)]
    config: serde_json::Value,
}

/// 声音库配置响应模型
#[derive(serde::Serialize)]
pub struct VoiceLibraryConfigResponse {
    /// 状态
    status: String,
    /// 消息
    message: String,
    /// 配置（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    #[allow(dead_code)]
    config: Option<serde_json::Value>,
    /// 时间戳
    timestamp: String,
}

/// 通用 API 响应模型
#[derive(serde::Serialize)]
pub struct ApiResponse<T: serde::Serialize> {
    status: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
    timestamp: String,
}
/// 基于Actix-Web的RPC系统
pub struct ActixRpcSystem {
    running: Arc<std::sync::atomic::AtomicBool>,
    #[allow(dead_code)]
    config: RpcConfig,
}

impl ActixRpcSystem {
    /// 创建新的Actix RPC系统实例
    pub async fn new(config: RpcConfig) -> Result<Self> {
        Ok(Self { running: Arc::new(std::sync::atomic::AtomicBool::new(true)), config })
    }

    /// 启动Actix-Web RPC系统
    pub async fn start(&self, addr: &str, asr_engine: Arc<AsrEngine>, llm_client: Option<Arc<LlmClient>>, store: Arc<dyn crate::storage::ConversationStore>) -> Result<()> {
        if !self.running.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }

        // 创建WebSocket会话管理器
        let session_manager = Arc::new(WebSocketSessionManager::new(asr_engine, llm_client, store).await);

        // 🆕 启动会话超时管理器
        // 超时管理器现在由SessionManager内部处理

        // 创建应用状态
        let app_state = ActixAppState::new(session_manager);

        info!("🎧 Actix-Web RPC服务器启动在 {}", addr);

        // 解析绑定地址
        let bind_addr = addr.to_string();

        // 启动服务器，自动重试绑定
        loop {
            match self.create_server(app_state.clone(), &bind_addr).await {
                Ok(_) => {
                    info!("✅ Actix-Web服务器正常关闭");
                    break; // 只有正常关闭时才退出循环
                },
                Err(e) => {
                    error!("❌ Actix-Web服务器错误: {}", e);
                    if e.to_string().contains("Address already in use") {
                        error!("⚠️ 地址已被占用，等待5秒后重试...");
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    } else {
                        // 对于其他错误，也等待一下再重试
                        error!("⏳ 5秒后将尝试重新启动服务器...");
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    }
                    // 继续循环，尝试重新启动
                },
            }
        }

        Ok(())
    }

    /// 创建并运行Actix-Web服务器
    async fn create_server(&self, app_state: ActixAppState, bind_addr: &str) -> Result<()> {
        // 🔧 优化worker配置，减少不必要的poll
        let worker_threads = std::env::var("ACTIX_WORKER_THREADS")
            .and_then(|s| s.parse().map_err(|_| std::env::VarError::NotPresent))
            .unwrap_or_else(|_| {
                // 默认使用CPU核心数的一半，避免过多worker
                let cpu_count = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
                std::cmp::max(2, cpu_count / 2)
            });

        info!("🔧 Actix-Web worker配置: threads={}", worker_threads);

        HttpServer::new(move || {
            App::new()
                .app_data(web::Data::new(app_state.clone()))
                .wrap(
                    middleware::Logger::default()
                        .exclude("/")
                        .exclude_regex("/assets/.*")
                        .exclude_regex("/fonts/.*")
                        .exclude("/favicon.ico")
                        .exclude("/manifest.webmanifest")
                        .exclude("/sw.js")
                        .exclude("/registerSW.js"),
                )
                .wrap(
                    Cors::default()
                        .allow_any_origin()
                        .allow_any_method()
                        .allow_any_header()
                        .supports_credentials(),
                )
                .route("/ws", web::get().to(actix_websocket_handler))
                .route("/health", web::get().to(health_check)) // 添加根路径健康检查
                .route("/metrics", web::get().to(metrics_endpoint))
                .route("/tts-settings", web::get().to(tts_settings_page))
                .service(
                    web::scope("/api")
                        .route("/health", web::get().to(health_check))
                        .route("/status", web::get().to(status_check))
                        .route("/debug/mcp-cache", web::get().to(mcp_cache_debug))
                        .route("/tts/baidu-voice-params", web::get().to(get_baidu_voice_params))
                        .route("/tts/baidu-voice-params", web::post().to(update_baidu_voice_params)),
                    // .route("/voice-library/config", web::post().to(update_voice_library_config))
                    // .route("/voice-library/config", web::get().to(get_voice_library_config)),
                )
                // Playground 静态文件服务：用户访问 http://host:8080/ 即可打开 Playground
                .service(
                    Files::new("/", Self::playground_dir())
                        .index_file("index.html")
                        .prefer_utf8(true),
                )
        })
        .workers(worker_threads) // 🔧 设置worker线程数
        .shutdown_timeout(5) // 🔧 设置关闭超时
        .bind(bind_addr)?
        .run()
        .await
        .map_err(|e| anyhow::anyhow!("Actix-Web服务器运行错误: {}", e))
    }

    /// 停止RPC系统
    pub async fn stop(&self) -> Result<()> {
        self.running.store(false, std::sync::atomic::Ordering::Release);
        tracing::info!("Actix RPC系统已停止");
        Ok(())
    }

    /// 获取 Playground 静态文件目录路径
    /// 优先级: PLAYGROUND_DIR 环境变量 > 可执行文件旁的 playground/ > 当前目录的 playground/
    fn playground_dir() -> String {
        let dir = if let Ok(dir) = std::env::var("PLAYGROUND_DIR") {
            dir
        } else if let Ok(exe) = std::env::current_exe() {
            let sibling = exe.parent().unwrap_or(std::path::Path::new(".")).join("playground");
            if sibling.is_dir() {
                sibling.to_string_lossy().into_owned()
            } else {
                "playground".to_string()
            }
        } else {
            "playground".to_string()
        };

        let path = std::path::Path::new(&dir);
        if path.is_dir() {
            info!(
                "Playground directory: {}",
                path.canonicalize().unwrap_or(path.to_path_buf()).display()
            );
        } else {
            tracing::warn!(
                "Playground directory not found: {} — visit http://host:port will return 404. Copy playground files or set PLAYGROUND_DIR.",
                dir
            );
        }
        dir
    }
}

/// Prometheus指标端点
async fn metrics_endpoint() -> actix_web::Result<impl actix_web::Responder> {
    use crate::monitoring::REGISTRY;
    use prometheus::{Encoder, TextEncoder};

    let encoder = TextEncoder::new();
    let mut buffer = Vec::new();

    // 收集自定义注册表中的指标
    let metric_families = REGISTRY.gather();
    encoder
        .encode(&metric_families, &mut buffer)
        .map_err(|e| actix_web::error::ErrorInternalServerError(format!("Failed to encode metrics: {}", e)))?;

    Ok(actix_web::HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4; charset=utf-8")
        .body(buffer))
}

/// 健康检查端点
async fn health_check() -> actix_web::Result<impl actix_web::Responder> {
    let response = HealthResponse {
        status: "healthy".to_string(),
        service: "realtime-rpc".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };
    Ok(actix_web::HttpResponse::Ok().json(response))
}

/// 状态检查端点
async fn status_check(data: web::Data<ActixAppState>) -> actix_web::Result<impl actix_web::Responder> {
    let active_sessions = data.session_manager.get_active_session_count().await;

    let response = StatusResponse {
        status: "running".to_string(),
        service: "realtime-rpc".to_string(),
        active_sessions,
        timestamp: chrono::Utc::now().to_rfc3339(),
        framework: "actix-web".to_string(),
    };
    Ok(actix_web::HttpResponse::Ok().json(response))
}

/// MCP缓存调试端点
async fn mcp_cache_debug() -> actix_web::Result<impl actix_web::Responder> {
    use crate::mcp::GLOBAL_MCP_TOOL_CACHE;

    let cache_stats = GLOBAL_MCP_TOOL_CACHE.get_cache_stats().await;

    let response = McpCacheDebugResponse {
        mcp_cache_stats: serde_json::to_value(&cache_stats).unwrap_or(serde_json::json!({})),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };
    Ok(actix_web::HttpResponse::Ok().json(response))
}

/// 更新声音库配置端点
#[allow(dead_code)]
async fn update_voice_library_config(req: web::Json<VoiceLibraryConfigRequest>) -> actix_web::Result<impl actix_web::Responder> {
    use crate::tts::minimax::global_voice_library;

    // 将JSON值转换为字符串
    let config_json = serde_json::to_string(&req.config).map_err(|e| actix_web::error::ErrorBadRequest(format!("无效的JSON格式: {}", e)))?;

    // 更新全局声音库配置
    match global_voice_library().update_from_json(&config_json) {
        Ok(_) => {
            info!("声音库配置已通过API更新");
            let response = VoiceLibraryConfigResponse {
                status: "success".to_string(),
                message: "声音库配置已更新".to_string(),
                config: None,
                timestamp: chrono::Utc::now().to_rfc3339(),
            };
            Ok(actix_web::HttpResponse::Ok().json(response))
        },
        Err(e) => {
            error!("更新声音库配置失败: {}", e);
            let response = VoiceLibraryConfigResponse {
                status: "error".to_string(),
                message: format!("配置更新失败: {}", e),
                config: None,
                timestamp: chrono::Utc::now().to_rfc3339(),
            };
            Ok(actix_web::HttpResponse::BadRequest().json(response))
        },
    }
}

/// 获取声音库配置端点
#[allow(dead_code)]
async fn get_voice_library_config() -> actix_web::Result<impl actix_web::Responder> {
    use crate::tts::minimax::global_voice_library;

    let config = global_voice_library().get_config();
    let config_json = serde_json::to_value(&config).map_err(|e| actix_web::error::ErrorInternalServerError(format!("序列化配置失败: {}", e)))?;

    let response = VoiceLibraryConfigResponse {
        status: "success".to_string(),
        message: "获取配置成功".to_string(),
        config: Some(config_json),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };
    Ok(actix_web::HttpResponse::Ok().json(response))
}

/// 获取百度 TTS 音色参数
async fn get_baidu_voice_params() -> actix_web::Result<impl actix_web::Responder> {
    let params = crate::tts::baidu::get_baidu_voice_params();
    let response = ApiResponse {
        status: "success".to_string(),
        message: "获取百度音色参数成功".to_string(),
        data: Some(params),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };
    Ok(actix_web::HttpResponse::Ok().json(response))
}

/// 更新百度 TTS 音色参数（热更新，无需重启）
async fn update_baidu_voice_params(body: web::Json<crate::tts::baidu::BaiduVoiceParamsSnapshot>) -> actix_web::Result<impl actix_web::Responder> {
    let snapshot = body.into_inner();

    // 校验参数范围
    if snapshot.spd > 15 || snapshot.pit > 15 || snapshot.vol > 15 {
        let response = ApiResponse::<()> {
            status: "error".to_string(),
            message: "spd/pit/vol 范围为 0-15".to_string(),
            data: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        return Ok(actix_web::HttpResponse::BadRequest().json(response));
    }
    if snapshot.speed_factor <= 0.0 || snapshot.speed_factor > 5.0 {
        let response = ApiResponse::<()> {
            status: "error".to_string(),
            message: "speed_factor 范围为 (0, 5.0]".to_string(),
            data: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        return Ok(actix_web::HttpResponse::BadRequest().json(response));
    }

    crate::tts::baidu::update_baidu_voice_params(&snapshot);

    let updated = crate::tts::baidu::get_baidu_voice_params();
    let response = ApiResponse {
        status: "success".to_string(),
        message: "百度音色参数已热更新，新的 TTS 合成将使用新参数".to_string(),
        data: Some(updated),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };
    Ok(actix_web::HttpResponse::Ok().json(response))
}

/// TTS 参数调节前端页面
async fn tts_settings_page() -> actix_web::Result<impl actix_web::Responder> {
    Ok(actix_web::HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(TTS_SETTINGS_HTML))
}

/// 内嵌的 TTS 设置页面 HTML
const TTS_SETTINGS_HTML: &str = r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>百度 TTS 音色参数调节</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: #0f172a; color: #e2e8f0; min-height: 100vh; display: flex; justify-content: center; align-items: flex-start; padding: 2rem; }
  .card { background: #1e293b; border-radius: 16px; padding: 2rem; width: 100%; max-width: 520px; box-shadow: 0 4px 24px rgba(0,0,0,0.4); }
  h1 { font-size: 1.4rem; margin-bottom: 1.5rem; color: #38bdf8; text-align: center; }
  .field { margin-bottom: 1.25rem; }
  label { display: flex; justify-content: space-between; align-items: center; font-size: 0.9rem; color: #94a3b8; margin-bottom: 0.4rem; }
  label span.val { font-weight: 600; color: #f1f5f9; font-variant-numeric: tabular-nums; min-width: 3em; text-align: right; }
  input[type=range] { -webkit-appearance: none; width: 100%; height: 6px; border-radius: 3px; background: #334155; outline: none; }
  input[type=range]::-webkit-slider-thumb { -webkit-appearance: none; width: 18px; height: 18px; border-radius: 50%; background: #38bdf8; cursor: pointer; }
  input[type=text] { width: 100%; padding: 0.5rem 0.75rem; background: #0f172a; border: 1px solid #334155; border-radius: 8px; color: #f1f5f9; font-size: 0.95rem; outline: none; }
  input[type=text]:focus { border-color: #38bdf8; }
  .actions { display: flex; gap: 0.75rem; margin-top: 1.5rem; }
  button { flex: 1; padding: 0.65rem; border: none; border-radius: 10px; font-size: 0.95rem; font-weight: 600; cursor: pointer; transition: opacity .15s; }
  button:hover { opacity: 0.85; }
  .btn-primary { background: #38bdf8; color: #0f172a; }
  .btn-secondary { background: #334155; color: #e2e8f0; }
  .toast { position: fixed; top: 1.5rem; right: 1.5rem; padding: 0.75rem 1.25rem; border-radius: 10px; font-size: 0.9rem; color: #fff; opacity: 0; transform: translateY(-8px); transition: all .3s ease; pointer-events: none; z-index: 999; }
  .toast.show { opacity: 1; transform: translateY(0); }
  .toast.success { background: #059669; }
  .toast.error { background: #dc2626; }
  .desc { font-size: 0.75rem; color: #64748b; margin-top: 0.2rem; }
</style>
</head>
<body>
<div class="card">
  <h1>百度 TTS 音色参数调节</h1>

  <div class="field">
    <label>发音人 ID (per) <span class="val" id="per-val"></span></label>
    <input type="text" id="per" placeholder="例如 4189, 4197">
    <div class="desc">百度发音人编号，常用: 4189(度小美-情感), 4197(度逍遥-情感)</div>
  </div>

  <div class="field">
    <label>语速 (spd) <span class="val" id="spd-val">5</span></label>
    <input type="range" id="spd" min="0" max="15" step="1" value="5">
    <div class="desc">范围 0-15，默认 7</div>
  </div>

  <div class="field">
    <label>音调 (pit) <span class="val" id="pit-val">6</span></label>
    <input type="range" id="pit" min="0" max="15" step="1" value="6">
    <div class="desc">范围 0-15，默认 6</div>
  </div>

  <div class="field">
    <label>音量 (vol) <span class="val" id="vol-val">5</span></label>
    <input type="range" id="vol" min="0" max="15" step="1" value="5">
    <div class="desc">范围 0-15，默认 5</div>
  </div>

  <div class="field">
    <label>变速因子 (speed_factor) <span class="val" id="sf-val">1.00</span></label>
    <input type="range" id="speed_factor" min="0.5" max="2.0" step="0.05" value="1.0">
    <div class="desc">PCM 无级变速, &lt;1.0 减速, &gt;1.0 加速, 范围 0.5-2.0</div>
  </div>

  <div class="actions">
    <button class="btn-secondary" onclick="loadParams()">刷新</button>
    <button class="btn-primary" onclick="saveParams()">保存并生效</button>
  </div>
</div>

<div class="toast" id="toast"></div>

<script>
const $ = id => document.getElementById(id);
const API = '/api/tts/baidu-voice-params';

function showToast(msg, type) {
  const t = $('toast');
  t.textContent = msg;
  t.className = 'toast show ' + type;
  setTimeout(() => t.className = 'toast', 2500);
}

function bindSlider(id) {
  const el = $(id);
  const valEl = $(id + '-val');
  if (!el || !valEl) return;
  el.addEventListener('input', () => {
    valEl.textContent = id === 'speed_factor' ? parseFloat(el.value).toFixed(2) : el.value;
  });
}

['spd', 'pit', 'vol', 'speed_factor'].forEach(bindSlider);

async function loadParams() {
  try {
    const res = await fetch(API);
    const json = await res.json();
    if (json.status === 'success' && json.data) {
      const d = json.data;
      $('per').value = d.per;
      $('per-val').textContent = d.per;
      $('spd').value = d.spd; $('spd-val').textContent = d.spd;
      $('pit').value = d.pit; $('pit-val').textContent = d.pit;
      $('vol').value = d.vol; $('vol-val').textContent = d.vol;
      $('speed_factor').value = d.speed_factor;
      $('sf-val').textContent = parseFloat(d.speed_factor).toFixed(2);
      showToast('参数已加载', 'success');
    }
  } catch (e) { showToast('加载失败: ' + e.message, 'error'); }
}

async function saveParams() {
  const body = {
    per: $('per').value,
    spd: parseInt($('spd').value),
    pit: parseInt($('pit').value),
    vol: parseInt($('vol').value),
    speed_factor: parseFloat($('speed_factor').value),
  };
  try {
    const res = await fetch(API, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    const json = await res.json();
    if (json.status === 'success') {
      showToast('参数已保存并生效!', 'success');
    } else {
      showToast('保存失败: ' + json.message, 'error');
    }
  } catch (e) { showToast('保存失败: ' + e.message, 'error'); }
}

loadParams();
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{App, test, web};

    #[actix_web::test]
    async fn test_health_check() {
        let app = test::init_service(App::new().route("/health", web::get().to(health_check))).await;

        let req = test::TestRequest::get().uri("/health").to_request();
        let resp = test::call_service(&app, req).await;
        assert!(resp.status().is_success());
    }

    #[tokio::test]
    async fn test_actix_rpc_system_creation() {
        let config = RpcConfig::default();
        let rpc_system = ActixRpcSystem::new(config).await;
        assert!(rpc_system.is_ok());
    }
}
