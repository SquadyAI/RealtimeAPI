//! 文本-LLM-TTS Pipeline Orchestrator
//!
//! 复用 ModularPipeline 的 LlmTaskV2 和 TtsTask，仅替换 ASR 为文本输入

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::{Mutex, broadcast, mpsc};
use tracing::{error, info, warn};

use crate::audio::OutputAudioConfig;
use crate::llm::LlmTaskV2;
use crate::llm::McpPromptRegistry;
use crate::llm::llm::{ChatCompletionParams, LlmClient};
use crate::mcp::client::McpClientWrapper;
use crate::rpc::{
    pipeline::{CleanupGuard, StreamingPipeline},
    protocol::{BinaryMessage, CommandId},
    session_router::SessionRouter,
};
use crate::tts::minimax::{MiniMaxConfig, VoiceSetting};

use super::text_input_task::TextInputTask;
use crate::rpc::pipeline::asr_llm_tts::tts_task::TtsController;
use crate::rpc::pipeline::asr_llm_tts::{EventEmitter, LockfreeResponseId, LockfreeResponseIdReader, SharedFlags, SimpleInterruptHandler, SimpleInterruptManager, TtsTask};

/// 文本-LLM-TTS Pipeline
pub struct LlmTtsPipeline {
    session_id: String,
    router: Arc<SessionRouter>,
    llm_client: Arc<LlmClient>,
    mcp_clients: Vec<McpClientWrapper>,
    #[allow(dead_code)]
    mcp_prompt_registry: Arc<McpPromptRegistry>,

    // 配置
    llm_params: Option<ChatCompletionParams>,
    system_prompt: Option<String>,
    #[allow(dead_code)]
    voice_setting: Option<VoiceSetting>,
    #[allow(dead_code)]
    tts_config: Option<MiniMaxConfig>,
    enable_search: Arc<AtomicBool>,

    // 通道
    text_input_tx: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,

    // 共享状态
    shared_flags: Arc<SharedFlags>,
    tts_session_created: Arc<AtomicBool>,
    simple_interrupt_manager: Arc<SimpleInterruptManager>,
    current_turn_response_id: Arc<LockfreeResponseId>,
    tts_controller: Arc<TtsController>,

    // 事件发送控制标志
    text_done_signal_only: Arc<AtomicBool>,
    signal_only: Arc<AtomicBool>,
}

impl LlmTtsPipeline {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: String,
        router: Arc<SessionRouter>,
        llm_client: Arc<LlmClient>,
        mcp_clients: Vec<McpClientWrapper>,
        mcp_prompt_registry: Arc<McpPromptRegistry>,
        llm_params: Option<ChatCompletionParams>,
        system_prompt: Option<String>,
        voice_setting: Option<VoiceSetting>,
        tts_config: Option<MiniMaxConfig>,
        enable_search: bool,
        text_done_signal_only: bool,
        signal_only: bool,
    ) -> Self {
        Self {
            session_id: session_id.clone(),
            router,
            llm_client,
            mcp_clients,
            mcp_prompt_registry,
            llm_params,
            system_prompt,
            voice_setting: voice_setting.clone(),
            tts_config: tts_config.clone(),
            enable_search: Arc::new(AtomicBool::new(enable_search)),
            text_input_tx: Arc::new(Mutex::new(None)),
            shared_flags: Arc::new(SharedFlags::new()),
            tts_session_created: Arc::new(AtomicBool::new(false)),
            simple_interrupt_manager: Arc::new(SimpleInterruptManager::new()),
            current_turn_response_id: Arc::new(LockfreeResponseId::new()),
            tts_controller: Arc::new(TtsController::new(tts_config, voice_setting)),
            text_done_signal_only: Arc::new(AtomicBool::new(text_done_signal_only)),
            signal_only: Arc::new(AtomicBool::new(signal_only)),
        }
    }
}

#[async_trait]
impl StreamingPipeline for LlmTtsPipeline {
    async fn start(&self) -> Result<CleanupGuard> {
        info!("🚀 启动文本-LLM-TTS Pipeline: {}", self.session_id);

        // 初始化 LLM 会话
        if let Some(ref prompt) = self.system_prompt {
            info!("🤖 初始化LLM会话，使用自定义系统提示词，长度: {}", prompt.len());
            self.llm_client.init_session(&self.session_id, Some(prompt.clone())).await;
        } else {
            warn!("⚠️ 初始化LLM会话时没有系统提示词");
            self.llm_client.init_session(&self.session_id, None).await;
        }

        // 配置 TTS 音频输出
        let output_config = OutputAudioConfig::default();
        info!("🎵 配置TTS音频输出配置: {:?}", output_config);
        self.tts_controller.configure_output_config(output_config.clone()).await?;

        // 创建通道
        let (text_input_tx, text_input_rx) = mpsc::unbounded_channel();
        let (asr_to_llm_tx, asr_to_llm_rx) = mpsc::unbounded_channel();
        let (llm_to_tts_tx, llm_to_tts_rx) = broadcast::channel(100);
        let (task_completion_tx, _task_completion_rx) = mpsc::unbounded_channel();
        let (next_sentence_tx, next_sentence_rx) = mpsc::unbounded_channel();

        // 保存 text_input_tx
        *self.text_input_tx.lock().await = Some(text_input_tx);

        // 创建事件发射器
        let emitter = Arc::new(EventEmitter::new(
            self.router.clone(),
            self.session_id.clone(),
            self.text_done_signal_only.clone(),
            self.signal_only.clone(),
        ));

        // 1. 启动 TextInputTask
        let text_task = TextInputTask::new(self.session_id.clone(), text_input_rx, asr_to_llm_tx);
        let text_handle = tokio::spawn(async move {
            if let Err(e) = text_task.run().await {
                error!("文本输入任务错误: {}", e);
            }
        });

        // 2. 启动 LlmTaskV2（使用 Agent 架构，system_prompt 直接从 llm_client.contexts 读取）
        let llm_task = LlmTaskV2::new(
            self.session_id.clone(),
            self.llm_client.clone(),
            emitter.clone(),
            self.llm_params.clone(),
            asr_to_llm_rx,
            llm_to_tts_tx.clone(),
            self.mcp_clients.clone(),
            self.shared_flags.clone(),
            self.enable_search.clone(),
            self.simple_interrupt_manager.clone(),
        );
        let llm_handle = tokio::spawn(async move {
            if let Err(e) = llm_task.run().await {
                error!("LLM任务错误: {}", e);
            }
        });

        // 3. 启动 TtsTask（完整复用）
        let tts_task = TtsTask::new(
            self.session_id.clone(),
            self.tts_controller.clone(),
            emitter.clone(),
            self.router.clone(),
            llm_to_tts_rx,
            self.tts_session_created.clone(),
            self.shared_flags.clone(),
            task_completion_tx,
            self.simple_interrupt_manager.clone(),
            Some(SimpleInterruptHandler::new(
                self.session_id.clone(),
                "TTS-Task".to_string(),
                self.simple_interrupt_manager.subscribe(),
            )),
            5,   // initial_burst_count
            1,   // initial_burst_delay_ms
            1.0, // send_rate_multiplier
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(crate::text_splitter::SimplifiedStreamingSplitter::new(None))),
            output_config,
            Arc::new(Mutex::new(None)),
            Arc::new(LockfreeResponseIdReader::from_writer(&self.current_turn_response_id)),
            next_sentence_tx,
            next_sentence_rx,
            false, // is_translation_mode: 非同传模式
        );
        let tts_handle = tokio::spawn(async move {
            if let Err(e) = tts_task.run().await {
                error!("TTS任务错误: {}", e);
            }
        });

        info!("✅ 文本-LLM-TTS Pipeline 启动完成: {}", self.session_id);

        // 清理资源
        // 预先克隆所需字段，避免在闭包中捕获 &self 导致生命周期问题
        let session_id_cleanup = self.session_id.clone();
        let llm_client_cleanup = self.llm_client.clone();
        let tts_controller_cleanup = self.tts_controller.clone();
        let mcp_clients_cleanup = self.mcp_clients.clone();

        let cleanup = CleanupGuard::new(move || {
            info!("🧹 清理文本-LLM-TTS Pipeline");

            // 任务级终止
            text_handle.abort();
            llm_handle.abort();
            tts_handle.abort();

            // 会话级资源释放（异步执行）
            // 在进入异步任务前克隆所需对象，避免在 Fn 闭包中移动捕获变量
            let sid = session_id_cleanup.clone();
            let llm = llm_client_cleanup.clone();
            let tts = tts_controller_cleanup.clone();
            let mcp = mcp_clients_cleanup.clone();
            tokio::spawn(async move {
                // 清理 LLM 会话上下文，避免累积
                llm.cleanup_session(&sid).await;
                info!("💬 LLM会话上下文已清理: {}", sid);

                // 归还/清理 TTS 资源
                tts.return_client().await;
                info!("🔊 已归还TTS客户端: {}", sid);

                // 释放 MCP 客户端资源
                for wrapper in mcp {
                    match wrapper {
                        crate::mcp::client::McpClientWrapper::Http { client: _, config } => {
                            // HTTP MCP 工具缓存采用自然失效 + LRU，不做主动释放
                            info!("ℹ️ HTTP MCP 工具缓存采用自然失效与LRU，不主动刷新: {}", config.endpoint);
                        },
                        crate::mcp::client::McpClientWrapper::WebSocket { manager, config } => {
                            manager.release_client(&config.endpoint).await;
                            info!("🧹 已释放WebSocket MCP客户端引用: {}", config.endpoint);
                        },
                    }
                }
            });
        });

        Ok(cleanup)
    }

    async fn on_upstream(&self, payload: BinaryMessage) -> Result<()> {
        match payload.header.command_id {
            CommandId::TextData => {
                if let Ok(text) = String::from_utf8(payload.payload) {
                    info!("📥 收到文本: {}", text);

                    if let Some(tx) = self.text_input_tx.lock().await.as_ref() {
                        tx.send(text)?;
                    } else {
                        error!("文本输入通道未初始化");
                    }
                }
            },
            CommandId::Interrupt => {
                // 🆕 用户按钮打断：停止当前 LLM/TTS 输出，但不销毁会话
                info!("🛑 收到 Interrupt（用户按钮打断）: session_id={}", self.session_id);

                // 广播打断：LLM-V2 会用该信号 cancel 当前 agent，TtsTask 会立即停音频
                let _ = self.simple_interrupt_manager.broadcast_global_interrupt(
                    self.session_id.clone(),
                    crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::InterruptReason::UserSpeaking,
                );

                // 额外即时清理：不依赖任务轮询，尽快停掉当前音频输出
                if let Err(e) = self.tts_controller.interrupt_session().await {
                    warn!("⚠️ Interrupt期间中断TTS失败: {}", e);
                }
                self.tts_controller.abort_finish_wait().await;
                {
                    let mut guard = self.tts_controller.finish_session_cleanup_rx.lock().await;
                    *guard = None;
                }
            },
            _ => {},
        }

        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    /// 🆕 会话配置热更新入口 - 复用 ASR-LLM-TTS 的同款架构
    async fn apply_session_config(&self, payload: &crate::rpc::protocol::MessagePayload) -> Result<()> {
        use crate::rpc::protocol::MessagePayload;

        info!(
            "🔧 [apply_session_config] 开始处理配置更新(文本-LLM-TTS): session_id={}",
            self.session_id
        );

        if let MessagePayload::SessionConfig { system_prompt, enable_search, search_config, .. } = payload {
            info!("✅ [apply_session_config] Payload 匹配成功: SessionConfig");

            // 1) 系统提示词
            if let Some(new_prompt) = system_prompt {
                info!("🔧 [apply_session_config] 检测到 system_prompt 更新");
                self.update_system_prompt(new_prompt.clone()).await?;
            }

            // 2) 搜索配置
            info!(
                "🔍 [apply_session_config] 检查搜索配置: enable_search={:?}, has_search_config={}",
                enable_search,
                search_config.is_some()
            );
            if enable_search.is_some() || search_config.is_some() {
                let enable = enable_search.unwrap_or(false);
                info!(
                    "🔧 [apply_session_config] 准备调用 update_search_configuration: enable={}",
                    enable
                );
                self.update_search_configuration(enable, search_config.clone()).await?;
            } else {
                info!("ℹ️ [apply_session_config] 未检测到搜索配置变更");
            }

            info!("✅ [apply_session_config] 配置更新处理完成: session_id={}", self.session_id);
        } else {
            warn!(
                "⚠️ [apply_session_config] Payload 类型不匹配，期望 SessionConfig，实际收到其他类型: session_id={}",
                self.session_id
            );
        }

        Ok(())
    }
}

impl LlmTtsPipeline {
    /// 更新系统提示词 - 复用 ModularPipeline 的同款逻辑
    pub async fn update_system_prompt(&self, new_prompt: String) -> Result<()> {
        info!("🔄 更新系统提示词: session_id={}", self.session_id);

        // 重新初始化LLM会话以应用新的系统提示词
        self.llm_client.init_session(&self.session_id, Some(new_prompt.clone())).await;

        info!("✅ 系统提示词更新完成: session_id={}", self.session_id);
        Ok(())
    }

    /// 更新搜索配置 - 复用 ModularPipeline 的同款逻辑
    pub async fn update_search_configuration(&self, enable_search: bool, search_config: Option<serde_json::Value>) -> Result<()> {
        use std::sync::atomic::Ordering;

        info!(
            "🔄 更新搜索配置（文本-LLM-TTS）: session_id={}, enable_search={}",
            self.session_id, enable_search
        );

        // 写入原子开关，下一轮 LLM 将据此注入/移除内置搜索工具
        self.enable_search.store(enable_search, Ordering::Release);

        // 如果有具体的 search_config，解析并设置默认搜索选项
        if let Some(config_value) = search_config {
            let mut options = crate::function_callback::searxng_client::SearchOptions::default();

            if let Some(engines) = config_value.get("engines").and_then(|v| v.as_array()) {
                options.engines = Some(engines.iter().filter_map(|e| e.as_str().map(|s| s.to_string())).collect());
            }
            if let Some(lang) = config_value.get("language").and_then(|v| v.as_str()) {
                options.language = Some(lang.to_string());
            }
            if let Some(range) = config_value.get("time_range").and_then(|v| v.as_str()) {
                options.time_range = Some(range.to_string());
            }
            if let Some(safe) = config_value.get("safe_search").and_then(|v| v.as_u64()) {
                options.safe_search = Some(safe as u8);
            }
            if let Some(categories) = config_value.get("categories").and_then(|v| v.as_array()) {
                options.categories = Some(categories.iter().filter_map(|c| c.as_str().map(|s| s.to_string())).collect());
            }
            if let Some(rpp) = config_value.get("results_per_page").and_then(|v| v.as_u64()) {
                options.results_per_page = Some(rpp as usize);
            }

            // 应用到全局内置搜索管理器
            crate::function_callback::get_builtin_search_manager().set_default_options(options);
            info!("🔍 已应用默认搜索配置到内置搜索管理器（文本-LLM-TTS）");
        }

        info!("✅ 搜索配置更新完成（文本-LLM-TTS）: session_id={}", self.session_id);
        Ok(())
    }
}
