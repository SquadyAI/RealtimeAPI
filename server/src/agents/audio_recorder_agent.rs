//! Audio Recorder Agent - 录音指令 Agent
//!
//! 处理 device.recorder.audio intent，通过 tool_client 调用客户端录音指令

use async_trait::async_trait;
use serde_json::json;

use super::{Agent, AgentContext, AgentHandles, ToolControl};
use crate::llm::Tool;

pub struct AudioRecorderAgent;

impl Default for AudioRecorderAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioRecorderAgent {
    pub fn new() -> Self {
        Self
    }

    /// 检查是否有录音工具可用（tools 或 offline_tools 中有 take_record）
    fn has_record_capability(tools: &[Tool], offline_tools: &[String]) -> bool {
        // 检查 tools 中是否有 take_record
        let has_in_tools = tools.iter().any(|t| {
            let name = t.function.name.to_lowercase();
            name.contains("take_record") || name.contains("record")
        });

        // 检查 offline_tools 中是否有 take_record
        let has_in_offline = offline_tools.iter().any(|name| {
            let name_lower = name.to_lowercase();
            name_lower == "take_record"
        });

        has_in_tools || has_in_offline
    }
}

#[async_trait]
impl Agent for AudioRecorderAgent {
    fn id(&self) -> &str {
        "agent.recorder.audio"
    }

    fn intents(&self) -> Vec<&str> {
        vec!["device.recorder.audio"]
    }

    async fn run(&self, ctx: AgentContext, handles: AgentHandles<'_>) -> anyhow::Result<()> {
        // 检查是否有录音能力（tools 或 offline_tools 中有 take_record）
        if !Self::has_record_capability(&ctx.tools, &ctx.offline_tools) {
            let language = ctx.extra.asr_language.as_deref().unwrap_or("zh");
            let reject_msg = match language.split('-').next().unwrap_or(language) {
                "zh" => "抱歉，录音功能暂时不可用。",
                "ja" => "申し訳ございませんが、現在録音機能はご利用いただけません。",
                "ko" => "죄송합니다. 현재 녹음 기능을 사용할 수 없습니다.",
                "es" => "Lo siento, la grabación de audio no está disponible en este momento.",
                "it" => "Mi dispiace, la registrazione audio non è disponibile al momento.",
                _ => "Sorry, audio recording is not available at the moment.",
            };
            handles.tts_sink.send(reject_msg).await;
            return Ok(());
        }

        let command_args = json!({
            "command": "wk_take_a_record"
        })
        .to_string();

        let outcome = handles.tool_client.call("device_command", &command_args).await?;

        match outcome.control {
            ToolControl::Respond(tts_text) => {
                handles.tts_sink.send(&tts_text).await;
            },
            ToolControl::Interrupted => {
                super::turn_tracker::interrupt_turn(&ctx.session_id).await;
            },
            ToolControl::Stop | ToolControl::Continue => {},
        }

        Ok(())
    }
}
