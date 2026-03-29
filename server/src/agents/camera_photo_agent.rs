//! Camera Photo Agent - 拍照指令 Agent
//!
//! 处理 device.camera.photo intent，从工具列表中查找 take_photo 工具并直接调用，不需要 LLM 判断

use async_trait::async_trait;
use serde_json::json;
use tracing::debug;

use super::{Agent, AgentContext, AgentHandles, ToolControl};
use crate::llm::Tool;

pub struct CameraPhotoAgent;

impl Default for CameraPhotoAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl CameraPhotoAgent {
    pub fn new() -> Self {
        Self
    }

    /// 检查是否有拍照工具可用（tools 或 offline_tools 中有 take_photo）
    fn has_photo_capability(tools: &[Tool], offline_tools: &[String]) -> bool {
        // 检查 tools 中是否有 vision 或 take_photo
        let has_in_tools = tools.iter().any(|t| {
            let name = t.function.name.to_lowercase();
            name.contains("vision") || name.contains("take_photo")
        });

        // 检查 offline_tools 中是否有 take_photo
        let has_in_offline = offline_tools.iter().any(|name| {
            let name_lower = name.to_lowercase();
            name_lower == "take_photo" || name_lower == "take_picture"
        });

        has_in_tools || has_in_offline
    }
}

#[async_trait]
impl Agent for CameraPhotoAgent {
    fn id(&self) -> &str {
        "device.camera.photo"
    }

    fn intents(&self) -> Vec<&str> {
        vec!["device.camera.photo"]
    }

    async fn run(&self, ctx: AgentContext, handles: AgentHandles<'_>) -> anyhow::Result<()> {
        // 调试日志：打印 offline_tools 内容
        debug!(
            "CameraPhotoAgent: tools={:?}, offline_tools={:?}",
            ctx.tools.iter().map(|t| &t.function.name).collect::<Vec<_>>(),
            ctx.offline_tools
        );

        // 检查是否有拍照能力（tools 或 offline_tools 中有 take_photo）
        if !Self::has_photo_capability(&ctx.tools, &ctx.offline_tools) {
            // 工具不存在时返回拒绝消息（不保存到上下文）
            let language = ctx.extra.asr_language.as_deref().unwrap_or("zh");
            let reject_msg = match language.split('-').next().unwrap_or(language) {
                "zh" => "抱歉，拍照功能暂时不可用。",
                "ja" => "申し訳ございませんが、現在撮影機能はご利用いただけません。",
                "ko" => "죄송합니다. 현재 사진 촬영 기능을 사용할 수 없습니다.",
                "es" => "Lo siento, la captura de fotos no está disponible en este momento.",
                "it" => "Mi dispiace, la funzione fotografica non è disponibile al momento.",
                _ => "Sorry, photo capture is not available at the moment.",
            };
            handles.tts_sink.send(reject_msg).await;
            // 功能不可用：只播报，不保存到上下文
            return Ok(());
        }

        debug!("CameraPhotoAgent: calling device_command with wk_take_a_picture");

        let command_args = json!({
            "command": "wk_take_a_picture"
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
