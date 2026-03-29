use crate::AsrEngine;
use crate::llm::LlmClient;
use crate::rpc::{
    pipeline::StreamingPipeline,
    protocol::{self, ProtocolId},
    session_router::SessionRouter,
};

use crate::tts::minimax::{MiniMaxConfig, VoiceSetting};
use anyhow::Result;
use std::sync::Arc;
use tracing::{info, warn};

// 🆕 连接元数据（含IP地理位置）
// CONNECTION_METADATA_CACHE导入已移除 - 现在LLM直接动态获取IP地理位置信息

/// Pipeline工厂 - 负责创建不同类型的Pipeline实例
pub struct PipelineFactory {
    /// 会话路由器
    router: Arc<SessionRouter>,
    /// ASR 引擎
    asr_engine: Arc<AsrEngine>,
    /// LLM 客户端
    llm_client: Option<Arc<LlmClient>>,
    /// MCP 管理器
    mcp_manager: Arc<crate::mcp::McpManager>,
    /// 🔧 新增：MCP 提示词注册表
    mcp_prompt_registry: Arc<crate::llm::McpPromptRegistry>,
}

impl PipelineFactory {
    pub fn new(router: Arc<SessionRouter>, asr_engine: Arc<AsrEngine>, llm_client: Option<Arc<LlmClient>>, mcp_manager: Arc<crate::mcp::McpManager>) -> Self {
        info!("🚀 创建Pipeline工厂");

        // 🔧 新增：创建MCP提示词注册表
        let mcp_prompt_registry = Arc::new(crate::llm::McpPromptRegistry::new());

        Self { router, asr_engine, llm_client, mcp_manager, mcp_prompt_registry }
    }
}

#[async_trait::async_trait]
impl crate::rpc::session_manager::PipelineFactory for PipelineFactory {
    async fn create_pipeline(
        &self,
        session_id: &str,
        connection_id: &str,
        protocol_id: ProtocolId,
        speech_mode: crate::asr::SpeechMode,
        payload: Option<&protocol::MessagePayload>,
    ) -> Result<Arc<dyn StreamingPipeline + Send + Sync>, String> {
        match protocol_id {
            ProtocolId::Asr => {
                // ASR-only Pipeline - 仅语音识别
                self.create_asr_only_pipeline(session_id, connection_id, speech_mode, payload)
                    .await
            },
            ProtocolId::All => {
                self.create_asr_llm_tts_pipeline(session_id, connection_id, speech_mode, payload)
                    .await
            },
            ProtocolId::Llm => {
                // 文本-LLM-TTS Pipeline
                self.create_llm_tts_pipeline(session_id, connection_id, payload).await
            },
            ProtocolId::Tts => {
                // 保持 TTS-only 语义；Vision 由 ImageData 触发的临时管线承担
                self.create_tts_only_pipeline(session_id, connection_id, payload).await
            },
            ProtocolId::Translation => {
                // 同声传译 Pipeline
                self.create_translation_pipeline(session_id, connection_id, speech_mode, payload)
                    .await
            },
        }
    }
}

impl PipelineFactory {
    /// 创建 ASR-only Pipeline - 仅语音识别
    async fn create_asr_only_pipeline(
        &self,
        session_id: &str,
        _connection_id: &str,
        speech_mode: crate::asr::SpeechMode,
        payload: Option<&protocol::MessagePayload>,
    ) -> Result<Arc<dyn StreamingPipeline + Send + Sync>, String> {
        info!("🎤 创建 ASR-only Pipeline: {}", session_id);

        // 提取 ASR 语言设置
        let asr_language = if let Some(protocol::MessagePayload::SessionConfig { asr_language, .. }) = payload {
            asr_language.clone()
        } else {
            None
        };
        // 如果 asr_language 为 None，设置默认值为 "auto"
        let asr_language = asr_language.or(Some("auto".to_string()));
        info!("🔍 ASR-only: asr_language={:?}", asr_language);

        // 获取音频输入配置
        let audio_input_config = if let Some(protocol::MessagePayload::SessionConfig { input_audio_config, .. }) = payload
            && let Some(config_value) = input_audio_config
        {
            match serde_json::from_value::<crate::audio::input_processor::AudioInputConfig>(config_value.clone()) {
                Ok(mut cfg) => {
                    cfg.auto_correct();
                    if let Err(validation_error) = cfg.validate() {
                        warn!(
                            "⚠️ 输入音频配置验证失败: {}，使用默认配置, session_id={}",
                            validation_error, session_id
                        );
                        crate::audio::input_processor::AudioInputConfig::default()
                    } else {
                        info!("🎧 使用客户端提供的输入配置: {:?}, session_id={}", cfg, session_id);
                        cfg
                    }
                },
                Err(e) => {
                    warn!(
                        "⚠️ 解析 input_audio_config 失败，使用默认配置: {}, session_id={}",
                        e, session_id
                    );
                    crate::audio::input_processor::AudioInputConfig::default()
                },
            }
        } else {
            crate::audio::input_processor::AudioInputConfig::default()
        };

        // 创建 ASR-only Pipeline
        let pipeline = crate::rpc::pipeline::asr_only::AsrOnlyPipeline::new(
            session_id.to_string(),
            self.router.clone(),
            self.asr_engine.clone(),
            speech_mode,
            asr_language,
            audio_input_config,
        );

        info!("✅ ASR-only Pipeline 创建完成: {}", session_id);
        Ok(Arc::new(pipeline))
    }

    /// 创建ASR+LLM+TTS完整Pipeline
    async fn create_asr_llm_tts_pipeline(
        &self,
        session_id: &str,
        _connection_id: &str,
        speech_mode: crate::asr::SpeechMode,
        payload: Option<&protocol::MessagePayload>,
    ) -> Result<Arc<dyn StreamingPipeline + Send + Sync>, String> {
        // 获取LLM客户端
        let _llm_client = self.llm_client.clone().ok_or("LLM客户端未配置")?;

        // 获取语音设置
        let voice_setting = payload.and_then(|p| match p {
            protocol::MessagePayload::SessionConfig { voice_setting, .. } => voice_setting.as_ref().and_then(|v| serde_json::from_value(v.clone()).ok()),
            _ => None,
        });

        // 获取搜索配置
        let search_config = payload.and_then(|p| match p {
            protocol::MessagePayload::SessionConfig { search_config, .. } => search_config.clone(),
            _ => None,
        });

        // 🔧 关键修复：处理enable_search的情况
        let payload_search_config = if let Some(protocol::MessagePayload::SessionConfig { enable_search, search_config: _, .. }) = payload {
            if enable_search.unwrap_or(false) { search_config } else { None }
        } else {
            search_config
        };

        // 🆕 三合一远程配置获取
        let remote_config = if let Some(protocol::MessagePayload::SessionConfig { prompt_endpoint, .. }) = payload
            && let Some(endpoint) = prompt_endpoint
        {
            info!("🌐 检测到三合一配置端点: {}", endpoint);

            // 获取全局远程配置客户端
            use crate::rpc::remote_config::get_global_remote_config_client;
            let client = get_global_remote_config_client();

            // 尝试获取远程配置
            let remote_config_start = std::time::Instant::now();
            match client.get_config(endpoint).await {
                Ok(config) => {
                    info!(
                        "✅ 三合一配置获取成功: system_prompt={}, tools={}, mcp={}, search={} | ⏱️ 耗时: {:?}",
                        config.system_prompt.is_some(),
                        config.tools.as_ref().map(|t| t.len()).unwrap_or(0),
                        config.mcp_server_config.is_some(),
                        config.search_config.is_some(),
                        remote_config_start.elapsed()
                    );
                    Some(config)
                },
                Err(e) => {
                    warn!(
                        "⚠️ 远程配置获取失败: {}, 回退到本地配置 | ⏱️ 耗时: {:?}",
                        e,
                        remote_config_start.elapsed()
                    );
                    None
                },
            }
        } else {
            None
        };

        // 获取系统提示词（应用配置优先级：remote > payload > 服务端本地化）
        // 需要服务端本地化的语言列表（zh/en/ja/ko 由客户端提供）
        const LOCALIZED_LANGUAGES: &[&str] = &[
            "vi", "id", "th", "hi", "es", "fr", "de", "pt", "it", "ru", "tr", "uk", "pl", "nl", "el", "ro", "cs", "fi", "ar", "sv", "no", "da", "af",
        ];

        // 提取 asr_language 用于本地化判断
        let asr_lang_for_localization = if let Some(protocol::MessagePayload::SessionConfig { asr_language, .. }) = payload {
            asr_language.clone()
        } else {
            None
        };

        // 检查是否需要服务端本地化
        let needs_localization = asr_lang_for_localization
            .as_ref()
            .map(|lang| {
                let normalized = crate::agents::SystemPromptRegistry::normalize_language(lang);
                LOCALIZED_LANGUAGES.contains(&normalized)
            })
            .unwrap_or(false);

        let system_prompt = remote_config
            .as_ref()
            .and_then(|rc| rc.system_prompt.clone())
            .or_else(|| {
                payload.and_then(|p| match p {
                    protocol::MessagePayload::SessionConfig { system_prompt, .. } => {
                        info!("📝 从SessionConfig接收到的system_prompt: {:?}", system_prompt);
                        system_prompt.clone()
                    },
                    _ => None,
                })
            })
            .or_else(|| {
                // 如果客户端未提供 system_prompt 且语言需要本地化，使用服务端翻译版本
                if needs_localization {
                    if let Some(ref lang) = asr_lang_for_localization {
                        let localized = crate::agents::SystemPromptRegistry::global().get("assistant", lang);
                        if localized.is_some() {
                            info!("🌍 使用服务端本地化系统提示词: language={}", lang);
                        }
                        localized
                    } else {
                        None
                    }
                } else {
                    None
                }
            });

        // 🆕 提取表情选择提示词
        let emoji_prompt: Option<String> = remote_config.as_ref().and_then(|rc| rc.emoji_prompt.clone());

        // 🆕 提取音频输出配置和兼容性处理
        let output_audio_config = if let Some(protocol::MessagePayload::SessionConfig { output_audio_config, audio_chunk_size_kb, .. }) = payload {
            // 首先尝试使用新的 output_audio_config
            if let Some(cfg_val) = output_audio_config {
                match serde_json::from_value::<crate::audio::OutputAudioConfig>(cfg_val.clone()) {
                    Ok(mut cfg) => {
                        // 自动纠正并校验，避免不必要的回落
                        cfg.auto_correct();
                        if let Err(err) = cfg.validate() {
                            warn!("⚠️ 输出配置验证失败({}), 回落到默认PCM 20ms, session_id={}", err, session_id);
                            crate::audio::OutputAudioConfig::default_pcm(20)
                        } else {
                            info!("🎵 使用客户端提供的输出配置: {:?}, session_id={}", cfg, session_id);
                            cfg
                        }
                    },
                    Err(e) => {
                        warn!(
                            "⚠️ 解析 output_audio_config 失败，使用默认PCM: {}, session_id={}",
                            e, session_id
                        );
                        crate::audio::OutputAudioConfig::default_pcm(20)
                    },
                }
            } else if let Some(kb) = audio_chunk_size_kb {
                // 兼容旧的audio_chunk_size_kb参数：将KB转换为对应的毫秒数
                // 16kHz, 1ch, PCM S16LE: ms = bytes / 2 / 16000 * 1000
                let bytes = kb * 1024;
                let slice_ms = (bytes as f64 / 2.0 / 16000.0 * 1000.0) as u32;
                warn!(
                    "🔄 使用已弃用的 audio_chunk_size_kb ({} KB -> {} ms) 设置音频帧长，建议使用 output_audio_config, session_id={}",
                    kb, slice_ms, session_id
                );
                crate::audio::OutputAudioConfig::default_pcm(slice_ms)
            } else {
                // 使用默认配置
                crate::audio::OutputAudioConfig::default_pcm(20)
            }
        } else {
            // 使用默认配置
            crate::audio::OutputAudioConfig::default_pcm(20)
        };

        let initial_burst_count = payload
            .and_then(|p| match p {
                protocol::MessagePayload::SessionConfig { initial_burst_count, .. } => *initial_burst_count,
                _ => None,
            })
            .unwrap_or(3) as usize;

        let initial_burst_delay_ms = payload
            .and_then(|p| match p {
                protocol::MessagePayload::SessionConfig { initial_burst_delay_ms, .. } => *initial_burst_delay_ms,
                _ => None,
            })
            .unwrap_or(2) as u64;

        let send_rate_multiplier = payload
            .and_then(|p| match p {
                protocol::MessagePayload::SessionConfig { send_rate_multiplier, .. } => *send_rate_multiplier,
                _ => None,
            })
            .unwrap_or(1.001);

        // 🆕 是否仅在 response.text.done 发送信令
        let text_done_signal_only = payload
            .and_then(|p| match p {
                protocol::MessagePayload::SessionConfig { text_done_signal_only, .. } => *text_done_signal_only,
                _ => None,
            })
            .unwrap_or(false);

        // 🆕 是否仅发送语音和工具调用相关的信令
        let signal_only = payload
            .and_then(|p| match p {
                protocol::MessagePayload::SessionConfig { signal_only, .. } => *signal_only,
                _ => None,
            })
            .unwrap_or(false);

        // 🔧 移除 defer_llm_until_stop_input 处理，现在通过 SpeechMode::VadDeferred 来处理

        // 提取ASR语言设置
        let asr_language = if let Some(protocol::MessagePayload::SessionConfig { asr_language, .. }) = payload {
            asr_language.clone()
        } else {
            None
        };
        // 🆕 如果asr_language为None，设置默认值为"auto"
        let asr_language = asr_language.or(Some("auto".to_string()));
        info!("🔍 pipeline_factory: 从payload提取的asr_language: {:?}", asr_language);

        // 🔧 新增：打印payload的完整内容以帮助调试
        if let Some(p) = payload {
            info!("🔍 pipeline_factory: 完整payload内容: {:?}", p);
        } else {
            info!("🔍 pipeline_factory: payload为None");
        }

        // 时区和位置设置已移除 - 现在LLM动态从IP地理位置获取
        // 🆕 获取音频输入配置
        let audio_input_config = if let Some(protocol::MessagePayload::SessionConfig { input_audio_config, .. }) = payload
            && let Some(config_value) = input_audio_config
        {
            // 尝试直接反序列化为 AudioInputConfig
            match serde_json::from_value::<crate::audio::input_processor::AudioInputConfig>(config_value.clone()) {
                Ok(mut cfg) => {
                    // 验证并自动纠正配置
                    cfg.auto_correct();
                    if let Err(validation_error) = cfg.validate() {
                        warn!(
                            "⚠️ 输入音频配置验证失败: {}，使用默认配置, session_id={}",
                            validation_error, session_id
                        );
                        crate::audio::input_processor::AudioInputConfig::default()
                    } else {
                        info!("🎧 使用客户端提供的输入配置: {:?}, session_id={}", cfg, session_id);
                        cfg
                    }
                },
                Err(e) => {
                    warn!(
                        "⚠️ 解析 input_audio_config 失败，使用默认配置: {}, session_id={}",
                        e, session_id
                    );
                    crate::audio::input_processor::AudioInputConfig::default()
                },
            }
        } else {
            crate::audio::input_processor::AudioInputConfig::default()
        };

        // 提取enable_search设置
        let enable_search = if let Some(protocol::MessagePayload::SessionConfig { enable_search, .. }) = payload {
            enable_search.unwrap_or(false)
        } else {
            false
        };

        // 🆕 提取 ASR 繁简转换设置
        let asr_chinese_convert = if let Some(protocol::MessagePayload::SessionConfig { asr_chinese_convert, .. }) = payload {
            asr_chinese_convert.clone()
        } else {
            None
        };

        // 🆕 提取 TTS 繁简转换设置
        let tts_chinese_convert = if let Some(protocol::MessagePayload::SessionConfig { tts_chinese_convert, .. }) = payload {
            tts_chinese_convert.clone()
        } else {
            None
        };

        // 🆕 应用远程配置优先级：处理 search_config（在创建pipeline之前）
        let final_search_config = remote_config
            .as_ref()
            .and_then(|rc| rc.search_config.clone())
            .or(payload_search_config);

        // 🔧 创建模块化Pipeline（使用轮次级别的会话管理）
        let pipeline = crate::rpc::pipeline::asr_llm_tts::orchestrator::ModularPipeline::new(
            session_id,
            self.router.clone(),
            self.asr_engine.clone(),
            self.llm_client.clone().unwrap(),
            // 🆕 使用 MiniMax TTS 配置
            self.create_tts_config().await,
            speech_mode,
            voice_setting,
            final_search_config,
            enable_search, // 🔧 新增：传递enable_search参数
            system_prompt,
            initial_burst_count,
            initial_burst_delay_ms,
            send_rate_multiplier,
            asr_language,
            self.mcp_prompt_registry.clone(),
            audio_input_config,
            output_audio_config, // 🆕 新增：传递完整的音频输出配置（现在总是有效的配置）
            // timezone和location参数已移除 - 现在动态从IP地理位置获取
            text_done_signal_only,
            signal_only,
            asr_chinese_convert, // 🆕 ASR 繁简转换配置
            tts_chinese_convert, // 🆕 TTS 繁简转换配置
            emoji_prompt,        // 🆕 传递表情选择提示词
        );

        // 设置LLM参数
        let llm_params = Some(crate::llm::llm::ChatCompletionParams::default());
        let mut pipeline = if let Some(llm_params) = llm_params {
            pipeline.with_llm_params(llm_params)
        } else {
            pipeline
        };

        // 🔧 移除 defer_llm_until_stop_input 设置，现在通过 SpeechMode::VadDeferred 来处理

        // 🆕 应用远程配置优先级：处理工具配置
        // 优先级：remote > payload > default
        let remote_tools = remote_config.as_ref().and_then(|rc| rc.tools.clone());
        let remote_mcp_config = remote_config.as_ref().and_then(|rc| rc.mcp_server_config.clone());

        // 处理工具配置（应用远程配置优先级）
        if let Some(protocol::MessagePayload::SessionConfig { tools, tool_choice, mcp_server_config, tools_endpoint, .. }) = payload {
            let mut has_dynamic_source = false;

            // 优先级最高：tools_endpoint（异步加载）
            if let Some(endpoint) = tools_endpoint {
                has_dynamic_source = true;
                info!("🔧 启动异步工具端点加载: {} (优先级最高)", endpoint);

                use crate::mcp::get_global_async_tools_manager;
                let async_manager = get_global_async_tools_manager();

                if let Err(e) = async_manager
                    .start_async_tools_loading(endpoint.clone(), session_id.to_string())
                    .await
                {
                    warn!("⚠️ 启动异步工具加载失败: {} - {}", endpoint, e);
                } else {
                    info!("🚀 管道创建继续，tools_endpoint 工具将在后台异步加载");
                }
            }

            // 优先级第二：MCP 配置（远程 > payload）
            let final_mcp_config = remote_mcp_config.clone().or(mcp_server_config.clone());
            if let Some(mcp_config_val) = final_mcp_config {
                // has_dynamic_source = true;  // ← 移除：MCP 不应阻止 tools 处理
                match serde_json::from_value::<Vec<crate::mcp::McpServerConfig>>(mcp_config_val.clone()) {
                    Ok(mcp_configs) => {
                        info!(
                            "🔗 已接收 {} 个 MCP server 配置{}",
                            mcp_configs.len(),
                            if remote_mcp_config.is_some() { "（来自远程配置）" } else { "" }
                        );
                        pipeline = pipeline.with_mcp_servers(mcp_configs, self.mcp_manager.clone());
                    },
                    Err(e) => {
                        warn!("⚠️ 解析 MCP 配置失败，忽略: {}", e);
                    },
                }
            }

            // 优先级第三：传统 tools（远程 > payload）
            if !has_dynamic_source {
                let final_tools = remote_tools.clone().or(tools.clone());
                if let Some(tools_val) = final_tools {
                    // 解析工具列表
                    if let Ok(parsed_tools) = serde_json::from_value::<Vec<crate::llm::llm::Tool>>(serde_json::Value::Array(tools_val.clone())) {
                        // 🔧 修复：优先使用客户端提供的 tool_choice，否则默认为 auto
                        let final_choice = if let Some(choice_val) = tool_choice {
                            serde_json::from_value::<crate::llm::llm::ToolChoice>(choice_val.clone())
                                .ok()
                                .or(Some(crate::llm::llm::ToolChoice::auto()))
                        } else {
                            // 客户端未提供 tool_choice，使用默认 auto 策略
                            Some(crate::llm::llm::ToolChoice::auto())
                        };

                        info!(
                            "🧰 使用传统 Function Call 工具{}，共 {} 个，tool_choice={}",
                            if remote_tools.is_some() { "（来自远程配置）" } else { "" },
                            parsed_tools.len(),
                            if tool_choice.is_some() { "客户端提供" } else { "auto(默认)" }
                        );
                        pipeline = pipeline.with_function_calls(parsed_tools, final_choice);
                    }
                }
            } else {
                info!("🧩 已启用动态工具来源，最终合并策略：MCP > 内部工具 > tools_endpoint/传统tools");
            }
        } else {
            // payload 为 None，仅使用远程配置
            if let Some(mcp_config_val) = remote_mcp_config {
                match serde_json::from_value::<Vec<crate::mcp::McpServerConfig>>(mcp_config_val.clone()) {
                    Ok(mcp_configs) => {
                        info!("🔗 使用远程配置的 MCP 服务器，共 {} 个", mcp_configs.len());
                        pipeline = pipeline.with_mcp_servers(mcp_configs, self.mcp_manager.clone());
                    },
                    Err(e) => {
                        warn!("⚠️ 解析远程 MCP 配置失败: {}", e);
                    },
                }
            }

            if let Some(tools_val) = remote_tools
                && let Ok(mut parsed_tools) = serde_json::from_value::<Vec<crate::llm::llm::Tool>>(serde_json::Value::Array(tools_val.clone()))
            {
                // 修正非标准 JSON Schema 类型（如 "int" → "integer"）
                for tool in &mut parsed_tools {
                    crate::mcp::async_tools_manager::AsyncToolsManager::fix_schema_types(&mut tool.function.parameters);
                }
                info!("🧰 使用远程配置的工具，共 {} 个", parsed_tools.len());
                // 默认 tool_choice 为 auto
                let default_choice = serde_json::json!({"type": "auto"});
                if let Ok(choice) = serde_json::from_value::<crate::llm::llm::ToolChoice>(default_choice) {
                    pipeline = pipeline.with_function_calls(parsed_tools, Some(choice));
                }
            }
        }

        // 🆕 处理 offline_tools（离线工具列表）
        if let Some(protocol::MessagePayload::SessionConfig { offline_tools, .. }) = payload
            && let Some(tools_list) = offline_tools
        {
            info!("🔧 [pipeline_factory] 设置 offline_tools: {:?}", tools_list);
            crate::agents::turn_tracker::set_session_offline_tools(session_id, tools_list.clone()).await;
        }

        Ok(Arc::new(pipeline))
    }

    /// 创建文本-LLM-TTS Pipeline
    async fn create_llm_tts_pipeline(&self, session_id: &str, _connection_id: &str, payload: Option<&protocol::MessagePayload>) -> Result<Arc<dyn StreamingPipeline + Send + Sync>, String> {
        info!("🏗️ 创建文本-LLM-TTS Pipeline: {}", session_id);

        let llm_client = self.llm_client.clone().ok_or("LLM客户端未配置")?;

        // 提取配置（简化版本）
        let (system_prompt, voice_setting, enable_search, text_done_signal_only, signal_only) = match payload {
            Some(protocol::MessagePayload::SessionConfig {
                system_prompt, voice_setting, enable_search, text_done_signal_only, signal_only, ..
            }) => (
                system_prompt.clone(),
                voice_setting.as_ref().and_then(|v| serde_json::from_value(v.clone()).ok()),
                enable_search.unwrap_or(false),
                text_done_signal_only.unwrap_or(false),
                signal_only.unwrap_or(false),
            ),
            _ => (None, None, false, false, false),
        };

        // 创建 TTS 配置
        let tts_config = self.create_tts_config().await;

        // 创建 Pipeline（简化版本，不需要 MCP 客户端）
        let pipeline = crate::rpc::pipeline::llm_tts::LlmTtsPipeline::new(
            session_id.to_string(),
            self.router.clone(),
            llm_client,
            Vec::new(), // mcp_clients: 暂时为空，未来可扩展
            self.mcp_prompt_registry.clone(),
            None, // llm_params: 暂时为 None，使用默认
            system_prompt,
            voice_setting,
            tts_config,
            enable_search,
            text_done_signal_only,
            signal_only,
        );

        info!("✅ 文本-LLM-TTS Pipeline创建完成: {}", session_id);
        Ok(Arc::new(pipeline))
    }

    /// 创建TTS-only Pipeline（使用全局TTS池）
    async fn create_tts_only_pipeline(&self, session_id: &str, _connection_id: &str, payload: Option<&protocol::MessagePayload>) -> Result<Arc<dyn StreamingPipeline + Send + Sync>, String> {
        // 获取语音设置
        let voice_setting = payload.and_then(|p| match p {
            protocol::MessagePayload::SessionConfig { voice_setting, .. } => voice_setting
                .as_ref()
                .and_then(|v| serde_json::from_value::<VoiceSetting>(v.clone()).ok()),
            _ => None,
        });

        // 🆕 提取音频输出配置（与orchestrator.rs保持一致）
        let output_audio_config = payload
            .and_then(|p| match p {
                protocol::MessagePayload::SessionConfig { output_audio_config, .. } => output_audio_config
                    .as_ref()
                    .and_then(|v| serde_json::from_value::<crate::audio::OutputAudioConfig>(v.clone()).ok()),
                _ => None,
            })
            .unwrap_or_else(|| crate::audio::OutputAudioConfig::default_pcm(20));

        // 🆕 提取 TTS 繁简转换设置
        let tts_chinese_convert = payload.and_then(|p| match p {
            protocol::MessagePayload::SessionConfig { tts_chinese_convert, .. } => tts_chinese_convert.clone(),
            _ => None,
        });

        // 直接按需创建 MiniMax TTS-Only Pipeline（无全局池）
        let tts_config = self.create_tts_config().await;
        // 从 payload 提取 asr_language（用于 TTS 语言）
        let asr_language = payload.and_then(|p| match p {
            protocol::MessagePayload::SessionConfig { asr_language, .. } => asr_language.clone(),
            _ => None,
        });
        let pipeline = crate::rpc::pipeline::tts_only::enhanced_streaming_pipeline::create_enhanced_tts_only_pipeline_with_audio_format(
            session_id.to_string(),
            self.router.clone(),
            tts_config,
            voice_setting,
            output_audio_config,
            tts_chinese_convert,
            asr_language,
        );

        tracing::info!("✅ TTS-Only Pipeline创建完成: {}", session_id);
        Ok(Arc::new(pipeline))
    }

    /// 创建同声传译 Pipeline
    async fn create_translation_pipeline(
        &self,
        session_id: &str,
        _connection_id: &str,
        speech_mode: crate::asr::SpeechMode,
        payload: Option<&protocol::MessagePayload>,
    ) -> Result<Arc<dyn StreamingPipeline + Send + Sync>, String> {
        info!("🌍 创建同声传译 Pipeline: {}", session_id);

        // 获取 LLM 客户端
        let llm_client = self.llm_client.clone().ok_or("LLM客户端未配置")?;

        // 提取翻译语言配置
        let (from_language, to_language) = match payload {
            Some(protocol::MessagePayload::SessionConfig { from_language, to_language, .. }) => {
                let from = from_language.clone().ok_or("同声传译必须指定 from_language")?;
                let to = to_language.clone().ok_or("同声传译必须指定 to_language")?;
                (from, to)
            },
            _ => return Err("同声传译必须指定 from_language 和 to_language".to_string()),
        };

        // 获取语音设置
        let voice_setting = payload.and_then(|p| match p {
            protocol::MessagePayload::SessionConfig { voice_setting, .. } => voice_setting
                .as_ref()
                .and_then(|v| serde_json::from_value::<VoiceSetting>(v.clone()).ok()),
            _ => None,
        });

        // 获取音频输出配置
        let output_audio_config = payload
            .and_then(|p| match p {
                protocol::MessagePayload::SessionConfig { output_audio_config, .. } => output_audio_config
                    .as_ref()
                    .and_then(|v| serde_json::from_value::<crate::audio::OutputAudioConfig>(v.clone()).ok()),
                _ => None,
            })
            .unwrap_or_else(|| crate::audio::OutputAudioConfig::default_pcm(20));

        // 获取音频输入配置
        let input_audio_config = payload
            .and_then(|p| match p {
                protocol::MessagePayload::SessionConfig { input_audio_config, .. } => input_audio_config
                    .as_ref()
                    .and_then(|v| serde_json::from_value::<crate::audio::input_processor::AudioInputConfig>(v.clone()).ok()),
                _ => None,
            })
            .unwrap_or_default();

        // 创建 TTS 配置
        let tts_config = self.create_tts_config().await;

        // 创建同声传译 Pipeline
        let pipeline = crate::rpc::pipeline::translation::TranslationPipeline::new(
            session_id.to_string(),
            self.router.clone(),
            self.asr_engine.clone(),
            llm_client,
            tts_config,
            speech_mode,
            from_language,
            to_language,
            voice_setting,
            output_audio_config,
            input_audio_config,
        );

        info!("✅ 同声传译 Pipeline创建完成: {}", session_id);
        Ok(Arc::new(pipeline))
    }

    /// 创建 MiniMax TTS 配置
    async fn create_tts_config(&self) -> Option<MiniMaxConfig> {
        // 使用默认配置（从环境变量读取）
        let config = MiniMaxConfig::default();

        Some(config)
    }

    /// 异步加载工具端点配置（静态方法，不阻塞）
    async fn load_tools_from_endpoint_async(endpoint: &str, _tool_choice: &Option<serde_json::Value>) -> Result<Vec<crate::llm::llm::Tool>, String> {
        use crate::mcp::get_global_tools_endpoint_client;

        info!("🔧 异步处理工具端点: {}", endpoint);

        // 从工具端点获取工具列表
        let tools_values = get_global_tools_endpoint_client()
            .get_tools(endpoint)
            .await
            .map_err(|e| format!("获取工具端点失败: {}", e))?;

        // 转换为LLM工具格式
        let mut llm_tools = Vec::new();
        for tool_value in tools_values {
            match Self::convert_tool_value_to_llm_tool_static(tool_value) {
                Ok(llm_tool) => llm_tools.push(llm_tool),
                Err(e) => {
                    warn!("⚠️ 跳过无效工具: {}", e);
                },
            }
        }

        info!("✅ 异步成功转换 {} 个工具", llm_tools.len());
        Ok(llm_tools)
    }

    /// 处理工具端点配置（同步版本，已弃用）
    #[allow(dead_code)]
    async fn handle_tools_endpoint(&self, endpoint: &str, _tool_choice: &Option<serde_json::Value>) -> Result<Vec<crate::llm::llm::Tool>, String> {
        Self::load_tools_from_endpoint_async(endpoint, _tool_choice).await
    }

    /// 将工具值转换为LLM工具格式（静态版本）
    fn convert_tool_value_to_llm_tool_static(tool_value: serde_json::Value) -> Result<crate::llm::llm::Tool, String> {
        // 尝试直接解析为标准工具格式
        if let Ok(tool) = serde_json::from_value::<crate::llm::llm::Tool>(tool_value.clone()) {
            return Ok(tool);
        }

        // 尝试解析为MCP工具格式并转换
        if let Ok(mcp_tool) = serde_json::from_value::<crate::mcp::McpTool>(tool_value.clone()) {
            let llm_tool: crate::llm::llm::Tool = mcp_tool.into();
            return Ok(llm_tool);
        }

        // 尝试解析为嵌套格式 {type: "function", function: {...}}
        if let Some(tool_obj) = tool_value.as_object()
            && let (Some(tool_type), Some(function_obj)) = (tool_obj.get("type"), tool_obj.get("function"))
            && tool_type == "function"
            && function_obj.is_object()
            && let Ok(function) = serde_json::from_value::<crate::llm::llm::ToolFunction>(function_obj.clone())
        {
            return Ok(crate::llm::llm::Tool { tool_type: "function".to_string(), function });
        }

        Err(format!("无法解析工具格式: {}", tool_value))
    }

    /// 将工具值转换为LLM工具格式（实例版本）
    #[allow(dead_code)]
    fn convert_tool_value_to_llm_tool(&self, tool_value: serde_json::Value) -> Result<crate::llm::llm::Tool, String> {
        Self::convert_tool_value_to_llm_tool_static(tool_value)
    }
}
