//! Music Control Agent - 音乐控制指令 Agent
//!
//! 处理 agent.music.control intent，根据关键词判断具体动作：
//! - 播放音乐 → wk_play_music
//! - 停止/暂停 → wk_stop_music
//! - 上一首/上一曲 → wk_previous_song
//! - 下一首/下一曲/切歌 → wk_next_song

use async_trait::async_trait;
use serde_json::json;

use super::{Agent, AgentContext, AgentHandles, ToolControl};
use crate::llm::Tool;

/// 音乐控制动作
#[derive(Debug, Clone, Copy)]
enum MusicAction {
    Play,
    Stop,
    Previous,
    Next,
}

impl MusicAction {
    /// 返回对应的指令字符串
    fn command(&self) -> &'static str {
        match self {
            MusicAction::Play => "wk_play_music",
            MusicAction::Stop => "wk_stop_music",
            MusicAction::Previous => "wk_previous_song",
            MusicAction::Next => "wk_next_song",
        }
    }
}

pub struct MusicControlAgent;

impl Default for MusicControlAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl MusicControlAgent {
    pub fn new() -> Self {
        Self
    }

    /// 检查是否有音乐控制工具可用（tools 或 offline_tools 中有 music_control）
    fn has_music_capability(tools: &[Tool], offline_tools: &[String]) -> bool {
        // 检查 tools 中是否有 music_control
        let has_in_tools = tools.iter().any(|t| {
            let name = t.function.name.to_lowercase();
            name.contains("music_control") || name.contains("music")
        });

        // 检查 offline_tools 中是否有 music_control
        let has_in_offline = offline_tools.iter().any(|name| {
            let name_lower = name.to_lowercase();
            name_lower == "music_control"
        });

        has_in_tools || has_in_offline
    }

    /// 根据用户文本判断音乐控制动作
    fn detect_action(user_text: &str) -> MusicAction {
        let text = user_text.to_lowercase();

        if text.contains("停止") || text.contains("暂停") || text.contains("stop") || text.contains("pause") {
            return MusicAction::Stop;
        }

        if text.contains("上一首") || text.contains("上一曲") || text.contains("previous") {
            return MusicAction::Previous;
        }

        if text.contains("下一首") || text.contains("下一曲") || text.contains("切歌") || text.contains("next") {
            return MusicAction::Next;
        }

        MusicAction::Play
    }
}

#[async_trait]
impl Agent for MusicControlAgent {
    fn id(&self) -> &str {
        "agent.music.control"
    }

    fn intents(&self) -> Vec<&str> {
        vec!["agent.music.control"]
    }

    async fn run(&self, ctx: AgentContext, handles: AgentHandles<'_>) -> anyhow::Result<()> {
        // 检查是否有音乐控制能力（tools 或 offline_tools 中有 music_control）
        if !Self::has_music_capability(&ctx.tools, &ctx.offline_tools) {
            let language = ctx.extra.asr_language.as_deref().unwrap_or("zh");
            let reject_msg = match language.split('-').next().unwrap_or(language) {
                "zh" => "抱歉，音乐控制功能暂时不可用。",
                "ja" => "申し訳ございませんが、現在音楽コントロール機能はご利用いただけません。",
                "ko" => "죄송합니다. 현재 음악 컨트롤 기능을 사용할 수 없습니다.",
                "es" => "Lo siento, el control de música no está disponible en este momento.",
                "it" => "Mi dispiace, il controllo musicale non è disponibile al momento.",
                _ => "Sorry, music control is not available at the moment.",
            };
            handles.tts_sink.send(reject_msg).await;
            return Ok(());
        }

        let action = Self::detect_action(&ctx.user_text);
        let command = action.command();

        let command_args = json!({
            "command": command
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
