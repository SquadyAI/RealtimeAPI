//! 翻译任务（参考 llm_task.rs 简化版）
//!
//! 功能：接收 ASR 文本 → 调用 LLM 流式翻译 → 输出到 TTS

use anyhow::Result;

/// 中英专有名词对照表（用于翻译 prompt 中的 glossary）
const ZH_EN_GLOSSARY: &str = "\
- 王嘉尔 = Jackson Wang
- Team Wang = Team Wang
- WHL = WHL
- 蔚汇来 = WHL Holdings Limited
- Magic AI = Magic AI
- Magic AI Glasses = Magic AI Glasses
- GOT7 = GOT7";
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

use crate::lang::minimax_name_matches_code;
use crate::llm::LlmClient;
use crate::monitoring::METRICS;
use crate::rpc::pipeline::asr_llm_tts::{event_emitter::EventEmitter, simple_interrupt_manager::SimpleInterruptHandler, types::TurnContext};
use crate::tts::minimax::lang::{LINGUA_MIN_CONFIDENCE, LINGUA_MIN_MARGIN, detect_language_boost};

/// 翻译任务
pub struct TranslationTask {
    session_id: String,
    llm_client: Arc<LlmClient>,
    from_language: String,
    to_language: String,
    /// 接收 ASR 输出的 channel
    asr_rx: mpsc::UnboundedReceiver<(TurnContext, String)>,
    /// 发送到 TTS 的 channel
    tts_tx: broadcast::Sender<(TurnContext, String)>,
    /// 中断处理器（监听用户打断）
    interrupt_handler: Option<SimpleInterruptHandler>,
    /// 事件发射器（发送 delta/done 事件到客户端）
    emitter: Arc<EventEmitter>,
}

impl TranslationTask {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: String,
        llm_client: Arc<LlmClient>,
        from_language: String,
        to_language: String,
        asr_rx: mpsc::UnboundedReceiver<(TurnContext, String)>,
        tts_tx: broadcast::Sender<(TurnContext, String)>,
        interrupt_handler: Option<SimpleInterruptHandler>,
        emitter: Arc<EventEmitter>,
    ) -> Self {
        info!("🌍 创建翻译任务: {} → {}", from_language, to_language);
        Self {
            session_id,
            llm_client,
            from_language,
            to_language,
            asr_rx,
            tts_tx,
            interrupt_handler,
            emitter,
        }
    }

    /// 运行翻译任务（监听 ASR 输出，流式翻译后发送到 TTS）
    pub async fn run(mut self) -> Result<()> {
        info!("▶️ 翻译任务开始: session={}", self.session_id);

        // 循环接收 ASR 识别结果
        while let Some((turn_ctx, asr_text)) = self.asr_rx.recv().await {
            info!("📥 收到 ASR 文本: '{}'", asr_text);

            // 检查是否被打断
            if let Some(ref handler) = self.interrupt_handler
                && handler.is_interrupted_immutable()
            {
                warn!("⚠️ 翻译任务被打断，跳过当前文本");
                continue;
            }

            // 记录翻译请求指标
            METRICS.translation_requests_total.inc();
            let start_time = std::time::Instant::now();

            // 流式翻译并发送到 TTS
            match self.translate_streaming(turn_ctx, &asr_text).await {
                Ok(_) => {
                    let elapsed = start_time.elapsed().as_secs_f64();
                    METRICS.translation_latency_seconds.observe(elapsed);
                    info!("✅ 翻译完成，耗时: {:.2}s", elapsed);
                },
                Err(e) => {
                    METRICS.translation_errors_total.inc();
                    error!("❌ 翻译失败: {}, 输入: '{}'", e, asr_text);
                    // 继续处理下一条，不中断
                },
            }
        }

        info!("⏹️ 翻译任务结束: session={}", self.session_id);
        Ok(())
    }

    /// 流式翻译单条文本（参考 llm_task.rs:1479-1510 的流式处理）
    async fn translate_streaming(&self, turn_ctx: TurnContext, asr_text: &str) -> Result<()> {
        if asr_text.trim().is_empty() {
            return Ok(());
        }

        info!("🌍 开始流式翻译: '{}'", asr_text);

        // 内置翻译 system prompt（不从外部配置读取）
        // 中英互译时加入专有名词对照表（glossary）
        let glossary_section = if Self::is_chinese_english_pair(&self.from_language, &self.to_language) {
            format!(
                "\n\nProper noun glossary (for translation reference only, NOT your identity):\n{}",
                ZH_EN_GLOSSARY
            )
        } else {
            String::new()
        };

        let system_prompt = format!(
            "You are a {}-{} translator.\n\
            Translate to the OTHER language. Output MUST be in a DIFFERENT language than input.\n\
            Output ONLY the translation, nothing else.{}",
            self.language_name(&self.from_language),
            self.language_name(&self.to_language),
            glossary_section
        );

        // 构建 LLM 请求消息（不包含历史，只有当前输入）
        let messages = vec![
            crate::llm::llm::ChatMessage {
                role: Some("system".to_string()),
                content: Some(system_prompt),
                tool_call_id: None,
                tool_calls: None,
            },
            crate::llm::llm::ChatMessage {
                role: Some("user".to_string()),
                content: Some(asr_text.to_string()),
                tool_call_id: None,
                tool_calls: None,
            },
        ];

        // 流式调用 LLM（参考 llm_task.rs:1479-1510）
        let stream = self
            .llm_client
            .chat_stream(messages, None) // 不传入 llm_params，使用默认配置
            .await
            .map_err(|e| anyhow::anyhow!("LLM 流式调用失败: {}", e))?;

        let mut stream = Box::pin(stream);

        // 累积当前句子的翻译文本（用于发送 done 事件）
        let mut sentence_text = String::new();

        // 关键：立即转发每个 delta chunk 到 TTS（不累积完整结果）
        // 这样 TTS Task 可以使用 SimplifiedStreamingSplitter 进行句子分割
        while let Some(item) = stream.next().await {
            match item {
                Ok(choice) => {
                    // 处理 delta 字段（流式响应）
                    if let Some(delta) = &choice.delta {
                        if let Some(content) = &delta.content
                            && !content.is_empty()
                        {
                            debug!("📝 翻译增量: '{}'", content);

                            // 检查是否被打断
                            if let Some(ref handler) = self.interrupt_handler
                                && handler.is_interrupted_immutable()
                            {
                                warn!("⚠️ 翻译流式处理被打断");
                                break;
                            }

                            // 累积翻译文本
                            sentence_text.push_str(content);

                            // 发送 response.text.delta 事件到客户端
                            self.emitter.response_text_delta(&turn_ctx, 0, content).await;

                            // 立即发送到 TTS（不等待完整翻译）
                            if let Err(e) = self.tts_tx.send((turn_ctx.clone(), content.clone())) {
                                error!("❌ 发送翻译片段到 TTS 失败: {}", e);
                                // 继续处理，不中断翻译
                            }
                        }
                    }
                    // 处理 message 字段（非流式响应）
                    else if let Some(message) = &choice.message
                        && let Some(content) = &message.content
                        && !content.is_empty()
                    {
                        debug!("📝 翻译完整消息: '{}'", content);

                        // 检查是否被打断
                        if let Some(ref handler) = self.interrupt_handler
                            && handler.is_interrupted_immutable()
                        {
                            warn!("⚠️ 翻译流式处理被打断");
                            break;
                        }

                        // 累积翻译文本
                        sentence_text.push_str(content);

                        // 发送 response.text.delta 事件到客户端
                        self.emitter.response_text_delta(&turn_ctx, 0, content).await;

                        // 立即发送到 TTS
                        if let Err(e) = self.tts_tx.send((turn_ctx.clone(), content.clone())) {
                            error!("❌ 发送翻译结果到 TTS 失败: {}", e);
                        }
                    }
                },
                Err(e) => {
                    error!("❌ 流式翻译错误: {}", e);
                    return Err(e);
                },
            }
        }

        info!("✅ 翻译流式处理完成: '{}'", sentence_text);

        // 发送 response.text.done 事件到客户端（每句话翻译完成后立即发送）
        if !sentence_text.is_empty() {
            self.emitter.response_text_done(&turn_ctx, 0, &sentence_text).await;
            info!("📤 已发送 response.text.done: '{}'", sentence_text);

            // 对翻译结果进行语言检测，发送 BCP-47 代码给客户端
            // 规则：结果必须是 from_language 或 to_language 之一，默认返回 to_language
            let detected_code = if let Some(detected_lang) = detect_language_boost(&sentence_text, LINGUA_MIN_CONFIDENCE, LINGUA_MIN_MARGIN) {
                // 检测成功，判断是 from 还是 to
                if minimax_name_matches_code(&detected_lang, &self.to_language) {
                    info!("🌐 语言检测匹配目标语言: {} -> {}", detected_lang, self.to_language);
                    self.to_language.clone()
                } else if minimax_name_matches_code(&detected_lang, &self.from_language) {
                    info!("🌐 语言检测匹配源语言: {} -> {}", detected_lang, self.from_language);
                    self.from_language.clone()
                } else {
                    // 检测到第三种语言，默认返回目标语言
                    info!(
                        "🌐 语言检测结果 '{}' 不匹配语言对，默认使用目标语言: {}",
                        detected_lang, self.to_language
                    );
                    self.to_language.clone()
                }
            } else {
                // 检测失败（置信度太低），默认返回目标语言
                info!("🌐 语言检测置信度不足，默认使用目标语言: {}", self.to_language);
                self.to_language.clone()
            };
            self.emitter
                .response_language_detected(&turn_ctx.response_id, &detected_code)
                .await;
            info!("🌐 已发送语言检测结果: code={}", detected_code);
        }

        // 发送 __TURN_COMPLETE__ 标记，通知 TTS 该轮次结束
        if let Err(e) = self.tts_tx.send((turn_ctx.clone(), "__TURN_COMPLETE__".to_string())) {
            error!("❌ 发送TURN_COMPLETE到TTS失败(翻译): {}", e);
        } else {
            info!("📤 已发送 __TURN_COMPLETE__ 到 TTS");
        }

        Ok(())
    }

    /// 语言代码转全名（用于 system prompt）
    /// 使用统一的 lang 模块，支持 32 种语言
    fn language_name(&self, code: &str) -> &'static str {
        crate::lang::get_english_name(code)
    }

    /// 判断是否为中英语言对
    fn is_chinese_english_pair(lang_a: &str, lang_b: &str) -> bool {
        let is_chinese = |code: &str| {
            let c = code.to_lowercase();
            c.starts_with("zh") || c == "yue"
        };
        let is_english = |code: &str| code.to_lowercase().starts_with("en");
        (is_chinese(lang_a) && is_english(lang_b)) || (is_english(lang_a) && is_chinese(lang_b))
    }
}
