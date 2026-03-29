use anyhow::{Result, anyhow};
use async_trait::async_trait;
use std::sync::Arc;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, broadcast, mpsc, watch};
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

use crate::agents::turn_tracker::{self, ToolControlMode};
use crate::rpc::pipeline::{CleanupGuard, StreamingPipeline};
use crate::rpc::protocol::{BinaryMessage, CommandId};
use crate::rpc::session_router::SessionRouter;
use crate::tts::minimax::{MiniMaxConfig, VoiceSetting};

// 事件发射沿用 asr_llm_tts 的 EventEmitter
use super::super::asr_llm_tts::event_emitter::EventEmitter;
use super::super::asr_llm_tts::simple_interrupt_manager::{SimpleInterruptHandler, SimpleInterruptManager};
use super::super::tts_only::config::TtsProcessorConfig;

// 直接复用 TTS-only 的控制器以保证音频行为一致
use super::super::asr_llm_tts::LockfreeResponseId;
use super::super::asr_llm_tts::tts_task::TtsController;
use super::super::asr_llm_tts::types::{SharedFlags, TurnContext};

#[allow(clippy::type_complexity)]
pub struct VisionTtsPipeline {
    pub session_id: String,
    pub router: Arc<SessionRouter>,
    pub emitter: Arc<EventEmitter>,
    pub tts_controller: Arc<TtsController>,
    pub text_tx: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    pub input_timeout_tx: Arc<Mutex<Option<watch::Sender<bool>>>>,
    pub processor: Arc<Mutex<Option<()>>>,
    pub interrupt_manager: Arc<SimpleInterruptManager>,
    // 时区和位置信息已移除 - 现在动态从IP地理位置获取
    // 一次性销毁标志
    pub should_destroy: Arc<std::sync::atomic::AtomicBool>,
    // 🆕 继承自触发 session 的音频/节拍设置
    pub inherited_output_config: Arc<Mutex<Option<crate::audio::OutputAudioConfig>>>,
    pub inherited_pacing: Arc<Mutex<Option<(usize, u64, f64)>>>,
    // 🔧 当前轮次的response_id管理器
    pub current_turn_response_id: Arc<LockfreeResponseId>,
    // 🆕 繁简转换模式（支持运行时更新）
    pub tts_chinese_convert_mode: std::sync::RwLock<crate::text_filters::ConvertMode>,
    /// 🆕 统一TTS：LLM→TTS 广播发送端与任务句柄
    pub llm_to_tts_tx: Arc<Mutex<Option<broadcast::Sender<(TurnContext, String)>>>>,
    pub tts_task_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    pub shared_flags: Arc<SharedFlags>,
}

impl VisionTtsPipeline {
    /// 创建新的VisionTtsPipeline，带中断管理器（时区和位置信息将动态从IP地理位置获取）
    pub fn new(
        session_id: String,
        router: Arc<SessionRouter>,
        interrupt_manager: Option<Arc<super::super::asr_llm_tts::simple_interrupt_manager::SimpleInterruptManager>>,
        text_done_signal_only: Option<Arc<std::sync::atomic::AtomicBool>>,
        signal_only: Option<Arc<std::sync::atomic::AtomicBool>>,
        tts_config: Option<MiniMaxConfig>,
        voice_setting: Option<VoiceSetting>,
    ) -> Self {
        let emitter = Arc::new(EventEmitter::new(
            router.clone(),
            session_id.clone(),
            text_done_signal_only.unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicBool::new(false))),
            signal_only.unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicBool::new(false))),
        ));

        // 🆕 使用传入的会话级打断管理器；若无则创建新的
        let interrupt_manager = interrupt_manager.unwrap_or_else(|| Arc::new(SimpleInterruptManager::new()));
        // 🆕 继承 session 的 TTS 配置（voice_id 等）
        let mut tts_ctrl = TtsController::new(tts_config, voice_setting);
        tts_ctrl.set_interrupt_manager(interrupt_manager.clone());
        let tts_controller = Arc::new(tts_ctrl);

        Self {
            session_id,
            router,
            emitter,
            tts_controller,
            text_tx: Arc::new(Mutex::new(None)),
            input_timeout_tx: Arc::new(Mutex::new(None)),
            processor: Arc::new(Mutex::new(None)),
            interrupt_manager,
            // timezone和location字段已移除
            should_destroy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            inherited_output_config: Arc::new(Mutex::new(None)),
            inherited_pacing: Arc::new(Mutex::new(None)),
            current_turn_response_id: Arc::new(LockfreeResponseId::new()),
            tts_chinese_convert_mode: std::sync::RwLock::new(crate::text_filters::ConvertMode::None),
            llm_to_tts_tx: Arc::new(Mutex::new(None)),
            tts_task_handle: Arc::new(Mutex::new(None)),
            shared_flags: Arc::new(SharedFlags::new()),
        }
    }

    fn detect_mime(data: &[u8]) -> Option<&'static str> {
        if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
            return Some("image/jpeg");
        }
        if data.len() >= 8 && data[0..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
            return Some("image/png");
        }
        if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
            return Some("image/webp");
        }
        None
    }

    async fn start_workers(&self) -> Result<()> {
        // 🆕 继承 session 的输出与节拍设置
        let mut config = TtsProcessorConfig::default();
        if let Some(out_cfg) = self.inherited_output_config.lock().await.clone() {
            config.output_audio_config = Some(out_cfg);
        }
        if let Some((burst, delay, rate)) = *self.inherited_pacing.lock().await {
            config.initial_burst_count = burst;
            config.initial_burst_delay_ms = delay;
            config.send_rate_multiplier = rate;
        }
        // 🆕 统一 TtsTask：配置输出
        if let Some(ref audio_cfg) = config.output_audio_config {
            self.tts_controller.configure_output_config(audio_cfg.clone()).await?;
        }

        // 创建 LLM→TTS 广播通道
        let (llm_to_tts_tx, llm_to_tts_rx) = broadcast::channel::<(TurnContext, String)>(200);
        {
            let mut g = self.llm_to_tts_tx.lock().await;
            *g = Some(llm_to_tts_tx);
        }

        let initial_output_config = if let Some(ref cfg) = config.output_audio_config {
            cfg.clone()
        } else {
            crate::audio::OutputAudioConfig::default_pcm(20)
        };
        let (task_completion_tx, _rx_done) = mpsc::unbounded_channel();
        let text_splitter_first = Arc::new(AtomicBool::new(false));
        let text_splitter = Arc::new(Mutex::new(crate::text_splitter::SimplifiedStreamingSplitter::new(None)));
        let (next_sentence_tx, next_sentence_rx) = mpsc::unbounded_channel();
        let current_turn_response_id_reader = Arc::new(super::super::asr_llm_tts::lockfree_response_id::LockfreeResponseIdReader::from_writer(&self.current_turn_response_id));

        let tts_task = super::super::asr_llm_tts::tts_task::TtsTask::new(
            self.session_id.clone(),
            self.tts_controller.clone(),
            self.emitter.clone(),
            self.router.clone(),
            llm_to_tts_rx,
            Arc::new(AtomicBool::new(false)),
            self.shared_flags.clone(),
            task_completion_tx,
            self.interrupt_manager.clone(),
            Some(SimpleInterruptHandler::new(
                self.session_id.clone(),
                "VisionTTS-Main".to_string(),
                self.interrupt_manager.subscribe(),
            )),
            config.initial_burst_count,
            config.initial_burst_delay_ms,
            config.send_rate_multiplier,
            text_splitter_first,
            text_splitter,
            initial_output_config,
            Arc::new(Mutex::new(None)),
            current_turn_response_id_reader,
            next_sentence_tx,
            next_sentence_rx,
            false, // is_translation_mode: 非同传模式
        );

        let handle = tokio::spawn(async move {
            if let Err(e) = tts_task.run().await {
                error!("Vision TtsTask failed: {}", e);
            }
        });
        {
            let mut h = self.tts_task_handle.lock().await;
            *h = Some(handle);
        }

        Ok(())
    }

    async fn handle_image(&self, image: Vec<u8>, user_prompt: Option<String>) -> Result<()> {
        // 获取时区和位置信息
        let (second_level_time, timezone_offset, user_location) = crate::llm::llm::get_timezone_and_location_info_from_ip(&self.session_id).await;

        // 解析时间组件
        let mut parts = second_level_time.split_whitespace();
        let current_date = parts.next().unwrap_or("");
        let current_time_only = parts.next().unwrap_or("");
        let current_weekday = parts.next().unwrap_or("");

        // 检查是否需要跳过 location 注入（从 turn_tracker 获取 device_code）
        let skip_location = {
            let tracker = crate::agents::turn_tracker::get_or_create_tracker(&self.session_id).await;
            let guard = tracker.read().await;
            guard.should_skip_location_injection()
        };

        // 构建时间/位置信息
        // - 7720 设备：time_prefix 在前面
        // - 其他设备：User Information 块（但对于视觉 LLM，也放在前面）
        let info_block = if skip_location {
            // 7720 设备：只使用英文时间前缀
            crate::agents::runtime::build_time_prefix(&second_level_time, &timezone_offset, current_weekday)
        } else {
            // 其他设备：使用完整的 User Information 块
            format!(
                "User Information:\n    USER_LOCATION: {}\n    CURRENT_DATE: {}\n    CURRENT_DATETIME: {}\n    CURRENT_TIME: {}\n    CURRENT_TIMEZONE: {}\n    CURRENT_WEEKDAY: {}\n\n",
                user_location, current_date, second_level_time, current_time_only, timezone_offset, current_weekday
            )
        };

        // 构建system prompt：时间/位置信息 + 环境变量prompt或本地化prompt
        let env_prompt = std::env::var("VISUAL_LLM_PROMPT").ok().unwrap_or_default();
        let base_prompt = if !env_prompt.is_empty() {
            env_prompt
        } else {
            // 从 turn_tracker 获取 asr_language 用于语言路由
            let asr_lang = {
                let tracker = crate::agents::turn_tracker::get_or_create_tracker(&self.session_id).await;
                let guard = tracker.read().await;
                guard.asr_language.clone()
            };
            let lang = asr_lang.as_deref().unwrap_or("zh");

            // 从注册表获取本地化的视觉问答提示词
            // 回退顺序：指定语言 -> 英文 -> 中文默认
            crate::agents::SystemPromptRegistry::global()
                .get("visual_qa", lang)
                .unwrap_or_else(|| {
                    // 硬编码的中文默认提示词（作为最终回退）
                    r#"你是一个视觉AI助手，用自然流畅的口语回答用户关于图片的问题。

回答示例：
用户问"这是什么"，好的回答是："这是一杯星巴克的拿铁咖啡。杯子是白色的纸杯，上面印着绿色的星巴克logo，杯盖是黑色塑料的。咖啡看起来刚做好，杯壁上还有一些奶泡。旁边放着一根绿色的吸管和一张小票。从背景看应该是在星巴克店内，光线很柔和。"

回答要求：
- 先直接回答问题，再描述观察到的细节
- 包含：主体是什么、外观特征、颜色材质、环境背景
- 纯文本，不用markdown格式
- 口语化表达，不重复"#.to_string()
                })
        };
        let system_prompt = format!("{}{}", info_block, base_prompt);

        // 🆕 用户请求：使用传入的提示词，如果没有则使用默认值
        let user_request = user_prompt.clone().unwrap_or_else(|| "请描述这张图片".to_string());

        info!(
            "🖼️ 视觉LLM请求: session={}, system_prompt_len={}, user_request_len={}",
            self.session_id,
            system_prompt.len(),
            user_request.len()
        );

        // 环境变量
        // http://localhost:19444/v2/chat/completions
        let url = std::env::var("VISUAL_LLM_STREAM_URL").unwrap_or("http://localhost:19444/v2/chat/completions".to_string());
        let api_key = std::env::var("VISUAL_LLM_API_KEY").ok();
        let model = std::env::var("VISUAL_LLM_MODEL").unwrap_or("vision".to_string());
        let timeout_secs = std::env::var("VISUAL_LLM_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30);

        // 组装data URL（优化：预分配并原位编码，避免中间分配与拷贝）
        use base64::{Engine as _, engine::general_purpose};
        let mime = Self::detect_mime(&image).unwrap_or("application/octet-stream");
        // 预估Base64容量：4 * ceil(n/3)
        let b64_capacity = 4 * image.len().div_ceil(3);
        let mut b64 = String::with_capacity(b64_capacity);
        general_purpose::STANDARD.encode_string(&image, &mut b64);
        let prefix = if mime.starts_with("image/") {
            format!("data:{};base64,", mime)
        } else {
            "data:application/octet-stream;base64,".to_string()
        };
        let mut image_data_url = String::with_capacity(prefix.len() + b64.len());
        image_data_url.push_str(&prefix);
        image_data_url.push_str(&b64);

        // 发送 response.created
        let response_id = format!("resp_{}", nanoid::nanoid!(8));
        let assistant_item_id = format!("asst_{}", nanoid::nanoid!(6));
        // 🎯 工具调用ID（用于上下文维护）
        let tool_call_id = format!("vision_call_{}", nanoid::nanoid!(6));

        // 🔧 关键修复：将response_id存储到Pipeline级别的管理器中
        self.current_turn_response_id.store(Some(response_id.clone()));
        info!("🔒 Vision TTS存储response_id到Pipeline管理器: {}", response_id);

        // 🆕 保存图片数据到存储系统（立即保存图片和prompt）
        let image_bytes = bytes::Bytes::from(image.clone());
        let mime = Self::detect_mime(&image).unwrap_or("application/octet-stream");
        let image_metadata = crate::storage::ImageMetadata {
            format: mime.split('/').nth(1).unwrap_or("unknown").to_string(),
            mime_type: mime.to_string(),
            size_bytes: image.len(),
            width: None,
            height: None,
        };

        // 异步保存图片和prompt（不阻塞主流程）
        let session_id_clone = self.session_id.clone();
        let response_id_clone = response_id.clone();
        let user_prompt_clone = user_prompt.clone();
        tokio::spawn(async move {
            if let Some(session_data_store) = crate::storage::GlobalSessionStoreManager::get() {
                match session_data_store
                    .save_vision_image_data(
                        &response_id_clone,
                        &session_id_clone,
                        image_bytes,
                        image_metadata,
                        user_prompt_clone,
                        None, // LLM响应稍后更新
                    )
                    .await
                {
                    Ok(_) => info!(
                        "💾 Vision图片和Prompt已保存: session={}, response_id={}",
                        session_id_clone, response_id_clone
                    ),
                    Err(e) => warn!(
                        "⚠️ 保存Vision图片数据失败: session={}, response_id={}, error={}",
                        session_id_clone, response_id_clone, e
                    ),
                }
            } else {
                debug!("💡 Session data store未初始化，跳过保存Vision图片数据");
            }
        });

        let ctx = TurnContext::new("user_".to_string(), assistant_item_id.clone(), response_id.clone(), Some(1));

        // 🎯 维护上下文：开始新轮次，记录用户的识图请求
        let user_text_for_context = user_prompt.clone().unwrap_or_else(|| "请描述这张图片".to_string());
        turn_tracker::start_turn(&self.session_id, &response_id, &user_text_for_context).await;
        turn_tracker::set_intent(&self.session_id, Some("agent.qa.visual".to_string())).await;
        turn_tracker::set_agent(&self.session_id, "VisionTtsPipeline").await;

        // 🎯 记录工具调用（模拟 get_visual_QA 工具被调用）
        let tool_args = serde_json::json!({ "question": user_text_for_context }).to_string();
        turn_tracker::record_tool_call(&self.session_id, &tool_call_id, "get_visual_QA", &tool_args).await;

        self.emitter.response_created(&ctx).await;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build()?;

        // 使用正确的API格式
        let body = serde_json::json!({
            "model": model,
            "stream": true,
            "chat_template_kwargs": {
                "enable_thinking": false
            },
            "messages": [
                {
                    "role": "system",
                    "content": system_prompt
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "text",
                            "text": user_request
                        },
                        {
                            "type": "image_url",
                            "image_url": {
                                "url": image_data_url
                            }
                        }
                    ]
                }
            ]
        });

        // 打印请求详情用于调试
        info!(
            "🔍 发送视觉LLM请求: url={}, model={}, system_prompt_len={}, user_request_len={}, image_size={} bytes",
            url,
            model,
            system_prompt.len(),
            user_request.len(),
            image.len()
        );

        // 打印请求体
        debug!("🔍 视觉LLM请求体: {}", serde_json::to_string_pretty(&body).unwrap_or_default());

        let mut req = client.post(&url).json(&body);
        if let Some(k) = api_key {
            req = req.header("Authorization", format!("Bearer {}", k));
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                error!("❌ 视觉LLM请求发送失败: {}", e);
                return Err(anyhow!("请求发送失败: {}", e));
            },
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let error_text = resp.text().await.unwrap_or_default();
            error!("❌ 视觉LLM请求失败: status={}, error={}", status, error_text);
            return Err(anyhow!("视觉LLM请求失败: {} - {}", status, error_text));
        }
        let mut stream = resp.bytes_stream();
        // 🆕 参考 LLM：使用简化打断处理器，统一处理全局与用户打断
        let mut interrupt_handler = SimpleInterruptHandler::new(
            self.session_id.clone(),
            "VisionTTS".to_string(),
            self.interrupt_manager.subscribe(),
        );
        let mut buf: Vec<u8> = Vec::new();
        let mut was_interrupted = false;

        // 🔧 统一：提前获取 LLM→TTS 发送端，避免循环内频繁加锁
        let llm_sender_opt = { self.llm_to_tts_tx.lock().await.clone() };

        // 🆕 收集完整的LLM响应文本
        let mut full_llm_response = String::new();

        use futures_util::StreamExt;
        loop {
            tokio::select! {
                // 参考 LLM：等待相关打断（包含 UserSpeaking/UserPtt 以及 SessionTimeout/ConnectionLost/SystemShutdown）
                interrupt_event = interrupt_handler.wait_for_interrupt() => {
                    if interrupt_event.is_some() {
                        info!("🛑 VisionTTS 接收到打断，立即停止并回收: session={}", self.session_id);
                        was_interrupted = true;
                        // 统一由音频侧在最终音频完成后发送 text.done（此处不提前发送）
                        // 通知文本处理器完全停止
                        if let Some(tx) = self.text_tx.lock().await.as_ref() { let _ = tx.send("__STOP__".to_string()); }
                        // 中断TTS并重置客户端
                        if let Err(e) = self.tts_controller.interrupt_session().await { warn!("⚠️ 中断TTS失败: {}", e); }
                        self.tts_controller.reset_client().await;
                        // 清空会话音频缓冲区，避免残留输出
                        {
                            let sender_guard = self.tts_controller.session_audio_sender.lock().await;
                            if let Err(e) = sender_guard.force_clear_buffer().await { warn!("⚠️ 清空音频缓冲区失败: {}", e); }
                        }
                        // 统一TtsTask：无需本地处理器停止
                        // 标记一次性销毁
                        self.should_destroy.store(true, std::sync::atomic::Ordering::Release);
                        // 显式终止HTTP流（尽快关闭底层连接）
                        drop(stream);
                        break;
                    }
                },
                chunk_opt = stream.next() => {
                    match chunk_opt {
                        Some(Ok(chunk)) => {
                            buf.extend_from_slice(&chunk);
                            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                                let line_bytes = buf.drain(..=pos).collect::<Vec<u8>>();
                                let line_str = match std::str::from_utf8(&line_bytes) {
                                    Ok(s) => s.trim(),
                                    Err(_) => { continue; }
                                };
                                if line_str.is_empty() { continue; }
                                let payload_str = if let Some(rest) = line_str.strip_prefix("data:") { rest.trim() } else { line_str };
                                if payload_str == "[DONE]" { continue; }
                                if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(payload_str) {
                                    // 支持OpenAI格式的流式响应
                                    let delta_text = json_val
                                        .get("choices")
                                        .and_then(|choices| choices.get(0))
                                        .and_then(|choice| choice.get("delta"))
                                        .and_then(|delta| delta.get("content"))
                                        .and_then(|content| content.as_str())
                                        .or_else(|| json_val.get("delta").and_then(|v| v.as_str()))
                                        .or_else(|| json_val.get("text").and_then(|v| v.as_str()))
                                        .unwrap_or("");

                                    if !delta_text.is_empty() {
                                        // 🆕 收集响应文本
                                        full_llm_response.push_str(delta_text);
                                        // 统一与 asr_llm_tts：不直接发 response_text_delta，由 TTS PacedSender 的 sentence_text 承担文字事件
                                        // 统一：直接将增量文本发送到 TtsTask
                                        if let Some(tx) = llm_sender_opt.as_ref() { let _ = tx.send((ctx.clone(), delta_text.to_owned())); }
                                    }
                                } else {
                                    // 如果JSON解析失败，打印原始内容用于调试
                                    debug!("⚠️ 无法解析响应JSON: {}", payload_str);
                                }
                            }
                        },
                        Some(Err(e)) => { warn!("视觉LLM流错误: {}", e); break; },
                        None => break,
                    }
                },
            }
        }

        if !was_interrupted {
            // 统一由音频侧在最终音频完成后发送 text.done（此处不发送）

            // 🆕 异步保存完整的LLM响应（更新已有记录）
            if !full_llm_response.is_empty() {
                let session_id_clone = self.session_id.clone();
                let response_id_clone = response_id.clone();
                let llm_response_clone = full_llm_response.clone();
                let response_len = llm_response_clone.len();
                let image_bytes_clone = bytes::Bytes::from(image.clone());
                let mime_clone = Self::detect_mime(&image).unwrap_or("application/octet-stream");
                let image_metadata_clone = crate::storage::ImageMetadata {
                    format: mime_clone.split('/').nth(1).unwrap_or("unknown").to_string(),
                    mime_type: mime_clone.to_string(),
                    size_bytes: image.len(),
                    width: None,
                    height: None,
                };

                tokio::spawn(async move {
                    if let Some(session_data_store) = crate::storage::GlobalSessionStoreManager::get() {
                        // 使用 ON CONFLICT DO UPDATE 更新完整响应
                        match session_data_store
                            .save_vision_image_data(
                                &response_id_clone,
                                &session_id_clone,
                                image_bytes_clone,
                                image_metadata_clone,
                                user_prompt,
                                Some(llm_response_clone),
                            )
                            .await
                        {
                            Ok(_) => info!(
                                "💾 Vision完整响应已保存: session={}, response_id={}, response_len={}",
                                session_id_clone, response_id_clone, response_len
                            ),
                            Err(e) => warn!(
                                "⚠️ 保存Vision完整响应失败: session={}, response_id={}, error={}",
                                session_id_clone, response_id_clone, e
                            ),
                        }
                    }
                });
            }

            // 🎯 维护上下文：完成工具调用，tool 返回结果中添加提示
            // 使用强引导提示，确保用户追问时 LLM 会再次调用工具
            if !full_llm_response.is_empty() {
                let tool_result_with_hint = format!(
                    "{}。[重要：用户后续任何关于图片的追问都必须再次调用 get_visual_QA 工具获取最新信息]",
                    full_llm_response
                );
                turn_tracker::complete_tool_call(
                    &self.session_id,
                    &tool_call_id,
                    "get_visual_QA", // 工具名
                    &tool_result_with_hint,
                    ToolControlMode::Tts,
                    Some(full_llm_response.clone()), // TTS 播报原始结果（不带提示）
                    true,
                )
                .await;
            }
            turn_tracker::finish_turn(&self.session_id).await;

            // 统一：通知 TtsTask 本轮完成，触发 turn-final 注入，由 PacedSender 发送 done/stopped
            if let Some(tx) = self.llm_to_tts_tx.lock().await.as_ref() {
                let _ = tx.send((ctx.clone(), "__TURN_COMPLETE__".to_string()));
            }
            info!("✅ Vision LLM流已结束，已通知 TtsTask 完成: session={}", self.session_id);
        } else {
            // 🎯 维护上下文：标记轮次被打断
            turn_tracker::interrupt_turn(&self.session_id).await;
            info!("🧹 视觉-tts已被打断，已立即停止输出并回收: session={}", self.session_id);
        }

        self.await_tts_shutdown().await;

        Ok(())
    }

    /// 监听TTS音频完成信号（SESSION_FINISHED/TURN_FINISHED），触发pipeline销毁
    async fn monitor_tts_completion(&self) {
        let should_destroy = self.should_destroy.clone();
        let tts_controller = self.tts_controller.clone();
        let session_id = self.session_id.clone();

        tokio::spawn(async move {
            // TTS 客户端初始化需要时间（约 1s），添加重试逻辑等待初始化完成
            let max_retries = 10;
            let retry_interval = std::time::Duration::from_millis(200);
            let mut audio_rx_opt = None;

            for attempt in 0..max_retries {
                match tts_controller.subscribe_audio().await {
                    Ok(rx) => {
                        if attempt > 0 {
                            info!("🎧 TTS音频订阅成功（重试{}次后）: session={}", attempt, session_id);
                        } else {
                            info!("🎧 TTS音频订阅成功: session={}", session_id);
                        }
                        audio_rx_opt = Some(rx);
                        break;
                    },
                    Err(e) => {
                        if attempt < max_retries - 1 {
                            debug!(
                                "⏳ TTS客户端尚未就绪，等待重试 {}/{}: session={}, error={}",
                                attempt + 1,
                                max_retries,
                                session_id,
                                e
                            );
                            tokio::time::sleep(retry_interval).await;
                        } else {
                            warn!(
                                "⚠️ 无法订阅TTS音频流（已重试{}次）: {}, 使用超时销毁: session={}",
                                max_retries, e, session_id
                            );
                        }
                    },
                }
            }

            match audio_rx_opt {
                Some(mut audio_rx) => {
                    info!("🎧 开始监听TTS音频完成信号: session={}", session_id);

                    while let Ok(chunk) = audio_rx.recv().await {
                        // 检查是否为完成信号（MiniMax：以 is_final 为准）
                        if chunk.is_final {
                            info!(
                                "🏁 收到TTS完成(is_final) 信号，标记VisionTtsPipeline销毁: session={}",
                                session_id
                            );
                            should_destroy.store(true, std::sync::atomic::Ordering::Release);
                            break;
                        }
                    }
                },
                None => {
                    // 回退到超时销毁
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    should_destroy.store(true, std::sync::atomic::Ordering::Release);
                },
            }

            info!("🗑️ TTS完成监听器结束: session={}", session_id);
        });
    }

    async fn await_tts_shutdown(&self) {
        // 关闭 LLM→TTS 文本通道，确保 TtsTask 能够检测到广播关闭并开始收尾
        {
            let mut llm_tx_guard = self.llm_to_tts_tx.lock().await;
            if let Some(sender) = llm_tx_guard.take() {
                drop(sender);
            }
        }

        // 释放文本输入/超时控制通道，防止后台任务持有引用导致 TtsTask 无法退出
        {
            let mut text_guard = self.text_tx.lock().await;
            if let Some(tx) = text_guard.take() {
                drop(tx);
            }
        }
        {
            let mut timeout_guard = self.input_timeout_tx.lock().await;
            if let Some(tx) = timeout_guard.take() {
                drop(tx);
            }
        }

        // 等待 TTS 任务结束；任务内部会在 PacedSender 完成后才返回
        let join_timeout = std::env::var("VISION_TTS_JOIN_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(120));

        let tts_handle = {
            let mut guard = self.tts_task_handle.lock().await;
            guard.take()
        };

        if let Some(handle) = tts_handle {
            match timeout(join_timeout, handle).await {
                Ok(join_result) => match join_result {
                    Ok(_) => info!("✅ VisionTTS TTS任务已结束（PacedSender 已完成）: session={}", self.session_id),
                    Err(join_err) if join_err.is_cancelled() => {
                        info!("✅ VisionTTS TTS任务已被取消: session={}", self.session_id);
                    },
                    Err(join_err) => {
                        warn!(
                            "⚠️ VisionTTS TTS任务结束时出现错误: session={}, error={}",
                            self.session_id, join_err
                        );
                    },
                },
                Err(_) => {
                    warn!(
                        "⚠️ VisionTTS 等待TTS任务结束超时({:?}): session={}",
                        join_timeout, self.session_id
                    );
                },
            }
        } else {
            debug!("ℹ️ VisionTTS 未发现活跃的TTS任务句柄: session={}", self.session_id);
        }

        // 标记任务已完成，允许 SessionManager 清理临时 Pipeline
        self.should_destroy.store(true, Ordering::Release);
    }
}

#[async_trait]
impl StreamingPipeline for VisionTtsPipeline {
    async fn start(&self) -> Result<CleanupGuard> {
        self.start_workers().await?;
        self.monitor_tts_completion().await;
        // 使用已拥有的克隆，避免捕获 &self
        let session_id_cleanup = self.session_id.clone();
        let tts_ctrl_cleanup = self.tts_controller.clone();
        Ok(CleanupGuard::new(move || {
            info!("🧹 清理 VisionTtsPipeline: {}", session_id_cleanup);
            let sid = session_id_cleanup.clone();
            let tts = tts_ctrl_cleanup.clone();
            tokio::spawn(async move {
                // 归还 TTS 客户端到全局池
                tts.return_client().await;
                info!("🔊 已归还TTS客户端（vision-tts）: {}", sid);
            });
        }))
    }

    async fn on_upstream(&self, payload: BinaryMessage) -> Result<()> {
        match payload.header.command_id {
            CommandId::ImageData => {
                // 打印处理ImageData的日志
                info!(
                    "🖼️ VisionTtsPipeline处理ImageData: session_id={}, payload_size={} bytes",
                    self.session_id,
                    payload.payload.len()
                );

                // 🆕 解析扩展的ImageData格式
                let (user_prompt, image_data) = payload
                    .parse_vision_image_data()
                    .map_err(|e| anyhow!("解析ImageData失败: {}", e))?;

                // 打印解析结果
                if !user_prompt.is_empty() {
                    info!(
                        "📝 从ImageData中解析到用户提示词: session_id={}, prompt_len={}, user_prompt=\"{}\"",
                        self.session_id,
                        user_prompt.len(),
                        user_prompt
                    );
                } else {
                    info!(
                        "📝 ImageData中无用户提示词，将使用默认或已设置的提示词: session_id={}",
                        self.session_id
                    );
                }

                // 尺寸限制（检查图像数据大小）
                let max_bytes = std::env::var("VISUAL_LLM_MAX_IMAGE_BYTES")
                    .ok()
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(5 * 1024 * 1024);
                if image_data.len() > max_bytes {
                    warn!("⚠️ 图像过大: {} bytes > {} bytes", image_data.len(), max_bytes);
                    return Err(anyhow!("图像过大"));
                }

                // 传递用户提示词（如果存在）
                let prompt_option = if user_prompt.is_empty() { None } else { Some(user_prompt) };
                self.handle_image(image_data, prompt_option).await
            },
            CommandId::TextData => {
                // 🆕 简化：只处理普通TTS输入，不再支持__VISION_PROMPT__标签
                if let Ok(text) = String::from_utf8(payload.payload)
                    && let Some(tx) = self.text_tx.lock().await.as_ref()
                {
                    let _ = tx.send(text);
                }
                Ok(())
            },
            _ => Ok(()),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn handle_tool_call_result(&self, _tool_result: super::super::asr_llm_tts::tool_call_manager::ToolCallResult) -> Result<()> {
        // Vision pipeline 不支持工具调用
        Ok(())
    }

    /// 统一会话配置热更新入口（VisionTTS 适配）
    async fn apply_session_config(&self, payload: &crate::rpc::protocol::MessagePayload) -> Result<()> {
        use crate::rpc::protocol::MessagePayload;

        if let MessagePayload::SessionConfig {
            voice_setting,
            output_audio_config,
            initial_burst_count,
            initial_burst_delay_ms,
            send_rate_multiplier,
            asr_language,
            text_done_signal_only,
            signal_only,
            tts_chinese_convert,
            ..
        } = payload
        {
            // 1) 语音设置
            if let Some(vs) = voice_setting {
                let setting: crate::tts::minimax::VoiceSetting = serde_json::from_value(vs.clone()).map_err(|e| anyhow!("解析语音设置失败: {}", e))?;
                self.tts_controller
                    .update_voice_setting(setting)
                    .await
                    .map_err(|e| anyhow!("更新语音设置失败: {}", e))?;
            }

            // 2) 输出音频配置
            if let Some(out_cfg_val) = output_audio_config {
                match serde_json::from_value::<crate::audio::OutputAudioConfig>(out_cfg_val.clone()) {
                    Ok(cfg) => {
                        self.tts_controller.configure_output_config(cfg).await?;
                    },
                    Err(e) => {
                        warn!("⚠️ 解析 output_audio_config 失败: {}", e);
                    },
                }
            }

            // 3) PacedSender 节拍参数
            if initial_burst_count.is_some() || initial_burst_delay_ms.is_some() || send_rate_multiplier.is_some() {
                let burst = initial_burst_count.unwrap_or(0) as usize;
                let delay = initial_burst_delay_ms.unwrap_or(5) as u64;
                let rate = send_rate_multiplier.unwrap_or(1.0);
                self.tts_controller.update_pacing(burst, delay, rate).await;
            }

            // 4) 语言（用于 TTS start_task 的 language_boost）
            if asr_language.is_some() {
                self.tts_controller.set_language(asr_language.clone()).await;
            }

            // 5) 文本/信令开关
            if let Some(only) = *text_done_signal_only {
                // 通过 emitter 拿到共享原子标志并设置
                self.emitter
                    .text_done_signal_only_flag()
                    .store(only, std::sync::atomic::Ordering::Release);
            }
            if let Some(only) = *signal_only {
                self.emitter
                    .signal_only_flag()
                    .store(only, std::sync::atomic::Ordering::Release);
            }

            // 6) TTS 繁简转换模式
            if let Some(mode_str) = tts_chinese_convert.clone() {
                let cmode = crate::text_filters::ConvertMode::from(mode_str.as_str());
                // 更新管线字段（线程安全）
                if let Ok(mut guard) = self.tts_chinese_convert_mode.write() {
                    *guard = cmode;
                }
                // 统一TtsTask：无需本地处理器参数更新
            }
        }

        Ok(())
    }
}
