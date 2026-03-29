//! Volume Up Agent - 音量增大指令 Agent
//!
//! 处理 agent.volume.up intent，通过 tool_client 调用客户端音量增大指令

use async_trait::async_trait;
use serde_json::json;

use super::{Agent, AgentContext, AgentHandles, ToolControl};
use crate::llm::Tool;

pub struct VolumeUpAgent;

impl Default for VolumeUpAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl VolumeUpAgent {
    pub fn new() -> Self {
        Self
    }

    /// 检查是否有音量增大工具可用（tools 或 offline_tools 中有 increase_volume）
    fn has_volume_up_capability(tools: &[Tool], offline_tools: &[String]) -> bool {
        // 检查 tools 中是否有 increase_volume
        let has_in_tools = tools.iter().any(|t| {
            let name = t.function.name.to_lowercase();
            name.contains("increase_volume") || name.contains("volume_up")
        });

        // 检查 offline_tools 中是否有 increase_volume
        let has_in_offline = offline_tools.iter().any(|name| {
            let name_lower = name.to_lowercase();
            name_lower == "increase_volume"
        });

        has_in_tools || has_in_offline
    }
}

#[async_trait]
impl Agent for VolumeUpAgent {
    fn id(&self) -> &str {
        "agent.volume.up"
    }

    fn intents(&self) -> Vec<&str> {
        vec!["agent.volume.up"]
    }

    async fn run(&self, ctx: AgentContext, handles: AgentHandles<'_>) -> anyhow::Result<()> {
        // 检查是否有音量增大能力（tools 或 offline_tools 中有 increase_volume）
        if !Self::has_volume_up_capability(&ctx.tools, &ctx.offline_tools) {
            let language = ctx.extra.asr_language.as_deref().unwrap_or("zh");
            let reject_msg = match language.split('-').next().unwrap_or(language) {
                "zh" => "抱歉，音量调节功能暂时不可用。",
                "ja" => "申し訳ございませんが、現在音量調整機能はご利用いただけません。",
                "ko" => "죄송합니다. 현재 볼륨 조절 기능을 사용할 수 없습니다.",
                "es" => "Lo siento, el control de volumen no está disponible en este momento.",
                "it" => "Mi dispiace, il controllo del volume non è disponibile al momento.",
                _ => "Sorry, volume control is not available at the moment.",
            };
            handles.tts_sink.send(reject_msg).await;
            return Ok(());
        }

        let command_args = json!({
            "command": "wk_increase_volume"
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
