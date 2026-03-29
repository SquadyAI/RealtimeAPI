use anyhow::anyhow;
use async_trait::async_trait;

use super::stream_utils::{StreamOptions, process_stream_with_interrupt};
use super::system_prompt_registry::SystemPromptRegistry;
use super::{Agent, AgentContext, AgentHandles};
use crate::llm::{ChatCompletionParams, ChatMessage};

pub struct RejectionAgent;

impl Default for RejectionAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl RejectionAgent {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Agent for RejectionAgent {
    fn id(&self) -> &str {
        "agent.rejection.response"
    }

    async fn run(&self, ctx: AgentContext, handles: AgentHandles<'_>) -> anyhow::Result<()> {
        let llm_client = handles.llm_client.upgrade().ok_or_else(|| anyhow!("llm client unavailable"))?;

        let structured_block = crate::agents::runtime::build_user_structured_block_async(&ctx).await;
        let language = ctx.extra.asr_language.as_deref().unwrap_or("zh");
        let agent_id = self.id();
        let base_prompt = SystemPromptRegistry::global()
            .get(agent_id, language)
            .unwrap_or_else(|| panic!("System prompt not found for agent {} with language {}", agent_id, language));
        let system_prompt = crate::agents::runtime::build_agent_system_prompt_with_time_async(
            &ctx.session_id,
            ctx.role_prompt.as_deref(),
            &base_prompt,
            &structured_block,
            ctx.user_now.as_deref(),
            ctx.extra.user_timezone.as_deref(),
        )
        .await;

        // 🎯 中心化：从 TurnTracker 获取对话历史
        let conversation = super::turn_tracker::get_history_messages(&ctx.session_id).await;

        let mut messages = vec![ChatMessage {
            role: Some("system".into()),
            content: Some(system_prompt),
            tool_call_id: None,
            tool_calls: None,
        }];
        messages.extend(conversation);

        let params = ChatCompletionParams { stream: Some(true), ..Default::default() };

        let stream = llm_client.chat_stream(messages, Some(params)).await?;

        // 使用通用流式处理函数，自动支持 next-token 打断
        let options = StreamOptions::new(handles.cancel)
            .with_tts(handles.tts_sink)
            .with_tag("RejectionAgent");

        let result = process_stream_with_interrupt(Box::pin(stream), options).await;

        if result.was_interrupted {
            super::turn_tracker::interrupt_turn(&ctx.session_id).await;
            return Ok(());
        }

        if !result.text.trim().is_empty() {
            // 🎯 中心化：添加 assistant 消息到 TurnTracker
            super::turn_tracker::add_assistant_message(&ctx.session_id, &result.text).await;
        }

        Ok(())
    }
}
