//! Hot-update configuration methods for ModularPipeline.
//!
//! All `update_*` and `configure_*` methods live here.

use anyhow::{Result, anyhow};
use std::sync::atomic::Ordering;
use tracing::{error, info, warn};

use crate::mcp::client::McpClientWrapper;
use crate::tts::minimax::VoiceSetting;

use super::{ConfigUpdateEvent, ModularPipeline};

impl ModularPipeline {
    /// Update system prompt (re-initializes LLM session).
    pub async fn update_system_prompt(&self, new_prompt: String) -> Result<()> {
        info!("🔄 更新系统提示词: session_id={}", self.session_id);
        self.llm_client.init_session(&self.session_id, Some(new_prompt.clone())).await;
        info!("✅ 系统提示词更新完成: session_id={}", self.session_id);
        Ok(())
    }

    /// Compare and update MCP configuration — only rebuild when config actually changed.
    pub async fn compare_and_update_mcp_configuration(&self, new_mcp_config: serde_json::Value) -> Result<()> {
        info!("🔄 检查MCP服务器配置变化: session_id={}", self.session_id);

        let new_mcp_configs: Vec<crate::mcp::McpServerConfig> = serde_json::from_value(new_mcp_config).map_err(|e| anyhow!("解析MCP配置失败: {}", e))?;

        let current_endpoints: Vec<String> = self
            .mcp_clients
            .iter()
            .map(|client| match client {
                McpClientWrapper::Http { config, .. } => config.endpoint.clone(),
                McpClientWrapper::WebSocket { config, .. } => config.endpoint.clone(),
            })
            .collect();

        let new_endpoints: Vec<String> = new_mcp_configs.iter().map(|config| config.endpoint.clone()).collect();

        let configs_changed = current_endpoints.len() != new_endpoints.len() || !current_endpoints.iter().all(|endpoint| new_endpoints.contains(endpoint));

        if configs_changed {
            info!("🔄 检测到MCP服务器配置变化，进行重建: session_id={}", self.session_id);
            info!("   - 当前服务器: {:?}", current_endpoints);
            info!("   - 新服务器: {:?}", new_endpoints);

            let mcp_manager = crate::mcp::McpManager::new();

            if let Some(tx) = self.config_update_tx.lock().await.as_ref() {
                let event = ConfigUpdateEvent::UpdateMcpClients { mcp_configs: new_mcp_configs, mcp_manager: std::sync::Arc::new(mcp_manager) };
                tx.send(event).map_err(|e| anyhow!("发送MCP配置更新事件失败: {}", e))?;
            }

            info!("✅ MCP配置重建完成: session_id={}", self.session_id);
        } else {
            info!("✅ MCP服务器配置无变化，跳过重建: session_id={}", self.session_id);
        }

        Ok(())
    }

    /// Update search configuration.
    pub async fn update_search_configuration(&self, enable_search: bool, _search_config: Option<serde_json::Value>) -> Result<()> {
        info!(
            "🔄 更新搜索配置: session_id={}, enable_search={}",
            self.session_id, enable_search
        );
        self.enable_search.store(enable_search, Ordering::Release);

        if let Some(config_value) = _search_config {
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
            crate::function_callback::get_builtin_search_manager().set_default_options(options);
            info!("🔍 已应用默认搜索配置到内置搜索管理器");
        }

        info!("✅ 搜索配置更新完成: session_id={}", self.session_id);
        Ok(())
    }

    /// Update voice setting.
    pub async fn update_voice_setting(&self, voice_config: serde_json::Value) -> Result<()> {
        info!("🔄 更新语音设置: session_id={}", self.session_id);

        let voice_setting: VoiceSetting = serde_json::from_value(voice_config).map_err(|e| anyhow!("解析语音设置失败: {}", e))?;

        self.tts_controller
            .update_voice_setting(voice_setting)
            .await
            .map_err(|e| anyhow!("更新TTS语音设置失败: {}", e))?;

        info!("✅ 语音设置更新完成: session_id={}", self.session_id);
        Ok(())
    }

    /// Toggle response.text.done signal-only mode.
    pub async fn update_text_done_signal_only(&self, only_signal: bool) -> Result<()> {
        info!(
            "🔄 更新 text_done_signal_only: session_id={}, only_signal={}",
            self.session_id, only_signal
        );
        self.text_done_signal_only.store(only_signal, Ordering::Release);
        Ok(())
    }

    /// Toggle signal_only mode.
    pub async fn update_signal_only(&self, only: bool) -> Result<()> {
        info!("🔄 更新 signal_only: session_id={}, only={}", self.session_id, only);
        self.signal_only.store(only, Ordering::Release);
        Ok(())
    }

    /// Update ASR Chinese conversion mode.
    pub async fn update_asr_chinese_convert_mode(&self, mode: Option<String>) -> Result<()> {
        let new_mode = mode
            .as_deref()
            .map(crate::text_filters::ConvertMode::from)
            .unwrap_or(crate::text_filters::ConvertMode::None);
        info!("🔄 更新 ASR 繁简转换模式: session_id={}, mode={:?}", self.session_id, new_mode);
        let mut_flags = &self.shared_flags;
        if let Ok(mut guard) = mut_flags.asr_chinese_convert_mode.write() {
            *guard = new_mode;
        }
        Ok(())
    }

    /// Update TTS Chinese conversion mode.
    pub async fn update_tts_chinese_convert_mode(&self, mode: Option<String>) -> Result<()> {
        let new_mode = mode
            .as_deref()
            .map(crate::text_filters::ConvertMode::from)
            .unwrap_or(crate::text_filters::ConvertMode::None);
        info!("🔄 更新 TTS 繁简转换模式: session_id={}, mode={:?}", self.session_id, new_mode);
        let mut_flags = &self.shared_flags;
        if let Ok(mut guard) = mut_flags.tts_chinese_convert_mode.write() {
            *guard = new_mode;
        }
        Ok(())
    }

    /// Update ASR language preference (broadcasts via watch channel).
    pub async fn update_asr_language(&self, language: Option<String>) -> Result<()> {
        info!("🔄 更新 ASR 语言偏好: session_id={}, language={:?}", self.session_id, language);
        let _ = self.asr_language_tx.send(language);
        Ok(())
    }

    /// Update pacing config (burst/delay/rate).
    pub async fn update_pacing_config(&self, burst_count: usize, burst_delay_ms: u64, send_rate_multiplier: f64) -> Result<()> {
        info!(
            "🔄 更新PacedSender参数: session_id={}, rate={:.3}x, burst={}, delay={}ms",
            self.session_id, send_rate_multiplier, burst_count, burst_delay_ms
        );
        self.tts_controller
            .update_pacing(burst_count, burst_delay_ms, send_rate_multiplier)
            .await;
        Ok(())
    }

    /// Interrupt TTS session (delegates to TtsController).
    pub async fn interrupt_tts_session(&self) -> Result<()> {
        info!("🛑 管线级别打断 TTS 会话: session={}", self.session_id);
        self.tts_controller.interrupt_session().await?;
        info!("✅ TTS 会话已打断: session={}", self.session_id);
        Ok(())
    }

    /// Configure audio input processor format.
    pub async fn configure_audio_input_format(&self, format: crate::audio::AudioFormat) -> Result<()> {
        let mut input_processor_guard = self.input_processor.lock().await;
        let mut new_config = input_processor_guard.get_config().clone();
        new_config.format = format.clone();

        match input_processor_guard.update_config(new_config) {
            Ok(()) => {
                info!("✅ 音频输入处理器格式更新成功: {:?}", format);
                Ok(())
            },
            Err(e) => {
                error!("❌ 音频输入处理器格式更新失败: {}", e);
                Err(anyhow!("音频输入处理器格式更新失败: {}", e))
            },
        }
    }

    /// Get current audio input processor config.
    pub async fn get_audio_input_config(&self) -> crate::audio::AudioInputConfig {
        let input_processor_guard = self.input_processor.lock().await;
        input_processor_guard.get_config().clone()
    }

    /// Update full audio input config (including sample rate & resampling options).
    pub async fn configure_audio_input_config(&self, config: crate::audio::input_processor::AudioInputConfig) -> Result<()> {
        let mut guard = self.input_processor.lock().await;
        guard
            .update_config(config.clone())
            .map_err(|e| anyhow!("音频输入配置更新失败: {}", e))?;
        info!("✅ 音频输入配置更新成功: {:?}", config);
        Ok(())
    }

    /// Configure complete audio output config (with validation and auto-correction).
    pub async fn configure_output_config(&self, config: crate::audio::OutputAudioConfig) -> Result<()> {
        info!("🔄 ModularPipeline配置音频输出配置: {:?}", config);

        let mut corrected = config.clone();
        corrected.auto_correct();
        if let Err(e) = corrected.validate() {
            warn!("⚠️ 音频输出配置验证失败: {}，将回退到 PCM 20ms", e);
            corrected = crate::audio::OutputAudioConfig::default_pcm(20);
        }

        {
            let mut config_guard = self.audio_output_config.lock().await;
            *config_guard = corrected.clone();
        }

        self.tts_controller.configure_output_config(corrected).await?;

        info!("✅ ModularPipeline音频输出配置已更新");
        Ok(())
    }
}
