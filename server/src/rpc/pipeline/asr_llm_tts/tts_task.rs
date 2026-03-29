use anyhow::{Result, anyhow};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, broadcast, mpsc};
use tracing::{debug, error, info, warn};

use super::routing::{sanitize_visible_text, select_tts_engine};
use crate::audio::OutputAudioConfig;
use crate::rpc::pipeline::paced_sender::{PacedAudioChunk, RealtimeAudioMetadata};
use crate::rpc::session_router::SessionRouter;
use crate::rpc::tts_pool::TtsEngineKind;
use crate::text_splitter::SimplifiedStreamingSplitter;
use crate::tts::minimax::lang::{LINGUA_MIN_CONFIDENCE, LINGUA_MIN_MARGIN, detect_language_boost, get_voice_for_language, lingua_language_confidences};
use crate::tts::minimax::{AudioChunk, AudioSetting, MiniMaxHttpOptions, MiniMaxHttpTtsClient};
use crate::tts::volc_engine::{VolcEngineRequest, VolcEngineTtsClient};
use bytes::Bytes;
use futures_util::StreamExt;

use super::event_emitter::EventEmitter;
use super::simple_interrupt_manager::{SimpleInterruptHandler, SimpleInterruptManager};
use super::timing_manager::{TimingNode, record_node_time, record_node_time_and_try_report};
use super::types::{TaskCompletion, TurnContext};

// 导入SIMD优化的TTS音频帧处理模块
use crate::audio::{TTS_SOURCE_SAMPLE_RATE, TtsAudioFrame};

// 导入拆分出去的模块
pub use super::sentence_queue::{NextSentenceTrigger, SentenceQueue, TextChunk, get_top_language_from_cache};
pub use super::session_audio_sender::SessionAudioSender;

// Re-export TtsController from its own module (backward compatibility)
pub use super::tts_controller::TtsController;

/// 接收来自 LLM 的 (TurnContext, text_chunk)
pub struct TtsTask {
    pub session_id: String,
    /// TTS 控制器（管线级别共享）
    pub tts_controller: Arc<TtsController>,
    pub emitter: Arc<EventEmitter>,
    pub router: Arc<SessionRouter>,
    pub rx: tokio::sync::broadcast::Receiver<(TurnContext, String)>,
    pub tts_session_created: Arc<AtomicBool>,
    pub shared_flags: Arc<super::types::SharedFlags>,
    pub task_completion_tx: mpsc::UnboundedSender<TaskCompletion>,
    /// 🆕 简化的打断管理器
    pub simple_interrupt_manager: Arc<SimpleInterruptManager>,
    /// 🆕 简化的打断处理器
    pub simple_interrupt_handler: Option<SimpleInterruptHandler>,
    /// 初始爆发发送的块数
    pub initial_burst_count: usize,
    /// 初始爆发发送的延迟(ms)
    pub initial_burst_delay_ms: u64,
    /// 发送速率
    pub send_rate_multiplier: f64,
    /// 🆕 初始音频输出配置（从Pipeline传递）
    pub initial_output_config: OutputAudioConfig,
    /// 🆕 新增：TextSplitter首分片记录标志
    pub text_splitter_first_chunk_recorded: Arc<AtomicBool>,
    /// 🆕 文本分句器（会话内复用，跨轮次重置）
    pub text_splitter: Arc<Mutex<SimplifiedStreamingSplitter>>,
    /// 🔧 修复：音频处理任务句柄跟踪，确保同时只有一个任务运行
    pub audio_handler_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// 🔧 时序修复：Pipeline级别的当前轮次ID引用，用于打断信号（只读）
    pub current_turn_response_id: Arc<super::lockfree_response_id::LockfreeResponseIdReader>,

    // ============================================================================
    // 🆕 按需 TTS 生成相关字段（仅通道传递，内部状态在 TtsTask 中管理）
    // ============================================================================
    /// 下一句触发发送器（传给 PacedSender，由 orchestrator 创建）
    pub next_sentence_trigger_tx: mpsc::UnboundedSender<NextSentenceTrigger>,
    /// 下一句触发接收器（从 orchestrator 接收，TtsTask 启动时取出）
    pub next_sentence_trigger_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<NextSentenceTrigger>>>>,

    // 🔧 以下字段为 TtsTask 内部状态，不应在 orchestrator 中初始化
    /// 句子队列（按需生成模式）
    sentence_queue: Arc<Mutex<SentenceQueue>>,
    /// Phase 3：打断时断开向 PacedSender 发送音频的通路
    audio_sending_stopped: Arc<AtomicBool>,
    /// 是否已由任一路径注入过turn-final（整轮次收尾）
    turn_final_injected: Arc<AtomicBool>,
    /// 🆕 预取音频缓冲接收器（句级别并发：最多1个inflight）
    inflight_audio_rx: Arc<Mutex<Option<mpsc::Receiver<AudioChunk>>>>,
    /// 🆕 预取任务句柄（便于打断时中止后台HTTP流）
    inflight_task_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// 🆕 同声传译模式：收到新 response_id 时不清空旧缓冲区，让多个翻译任务按队列顺序完成
    is_translation_mode: bool,
}

impl TtsTask {
    /// 判断会话级音频处理任务是否正在运行（基于 JoinHandle 状态）
    async fn is_audio_handler_running(&self) -> bool {
        let task_guard = self.audio_handler_task.lock().await;
        task_guard.as_ref().map(|h| !h.is_finished()).unwrap_or(false)
    }

    /// 🆕 创建新的 TtsTask 实例，初始化内部状态
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: String,
        tts_controller: Arc<TtsController>,
        emitter: Arc<EventEmitter>,
        router: Arc<SessionRouter>,
        rx: broadcast::Receiver<(TurnContext, String)>,
        tts_session_created: Arc<AtomicBool>,
        shared_flags: Arc<super::types::SharedFlags>,
        task_completion_tx: mpsc::UnboundedSender<TaskCompletion>,
        simple_interrupt_manager: Arc<SimpleInterruptManager>,
        simple_interrupt_handler: Option<SimpleInterruptHandler>,
        initial_burst_count: usize,
        initial_burst_delay_ms: u64,
        send_rate_multiplier: f64,
        text_splitter_first_chunk_recorded: Arc<AtomicBool>,
        text_splitter: Arc<Mutex<SimplifiedStreamingSplitter>>,
        initial_output_config: OutputAudioConfig,
        audio_handler_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
        current_turn_response_id: Arc<super::lockfree_response_id::LockfreeResponseIdReader>,
        next_sentence_trigger_tx: mpsc::UnboundedSender<NextSentenceTrigger>,
        next_sentence_trigger_rx: mpsc::UnboundedReceiver<NextSentenceTrigger>,
        is_translation_mode: bool,
    ) -> Self {
        Self {
            session_id,
            tts_controller,
            emitter,
            router,
            rx,
            tts_session_created,
            shared_flags,
            task_completion_tx,
            simple_interrupt_manager,
            simple_interrupt_handler,
            initial_burst_count,
            initial_burst_delay_ms,
            send_rate_multiplier,
            text_splitter_first_chunk_recorded,
            text_splitter,
            initial_output_config,
            audio_handler_task,
            current_turn_response_id,
            next_sentence_trigger_tx,
            next_sentence_trigger_rx: Arc::new(Mutex::new(Some(next_sentence_trigger_rx))),
            // 🔧 内部状态由 TtsTask 自己初始化
            sentence_queue: Arc::new(Mutex::new(SentenceQueue::new())),
            audio_sending_stopped: Arc::new(AtomicBool::new(false)),
            turn_final_injected: Arc::new(AtomicBool::new(false)),
            inflight_audio_rx: Arc::new(Mutex::new(None)),
            inflight_task_handle: Arc::new(Mutex::new(None)),
            is_translation_mode,
        }
    }

    // ============================================================================
    // 🆕 按需生成：处理下一句句子
    // ============================================================================

    /// 处理队列中的下一句（发送到当前选定的TTS引擎）
    async fn process_next_sentence(&self) -> Result<()> {
        // 从队列中取出下一句
        let sentence_opt = {
            let mut queue = self.sentence_queue.lock().await;
            queue.mark_processing()
        };

        let sentence = match sentence_opt {
            Some(s) => s,
            None => {
                debug!("📭 句子队列为空，无需处理");
                return Ok(());
            },
        };

        let current_voice_id = self.tts_controller.current_voice_id().await;
        let sentence_text = &sentence.text;
        let cleaned = sanitize_visible_text(sentence_text);
        // 按需进行语言相关转换（如日语→ひらがな），使用缓存的语言检测结果
        if cleaned.is_empty() {
            info!("🧹 句子清洗后为空，跳过发送到TTS");
            return Ok(());
        }
        // 获取轮次内已确认的引擎（用于继承）
        let inherited_engine = { self.tts_controller.turn_confirmed_engine.lock().await.clone() };
        // 同声传译模式：强制使用 Edge TTS
        let force_engine = if self.is_translation_mode { Some(TtsEngineKind::EdgeTts) } else { None };
        // 使用缓存的语言检测结果进行路由判断（支持 TTS_ENGINE 覆盖 + 轮次继承）
        let selection = select_tts_engine(
            &cleaned,
            current_voice_id.as_deref(),
            &sentence.language_confidences,
            inherited_engine,
            force_engine,
        );
        let engine = selection.engine;
        // 如果是确定性路由，更新轮次缓存
        if selection.is_confident {
            let mut engine_guard = self.tts_controller.turn_confirmed_engine.lock().await;
            if engine_guard.is_none() {
                info!("🔒 首次确定路由，锁定轮次引擎: {:?}", engine);
                *engine_guard = Some(engine);
            }
        }

        info!(
            "🎯 按需处理句子 ({:?}): '{}'",
            engine,
            cleaned.chars().take(50).collect::<String>()
        );

        // 确保 TTS 客户端就绪
        let client_ready = {
            let pool_client_guard = self.tts_controller.pool_client.lock().await;
            pool_client_guard.is_some()
        };

        if !client_ready {
            // 尝试获取客户端
            match tokio::time::timeout(std::time::Duration::from_secs(2), self.tts_controller.get_or_create_client()).await {
                Ok(Ok(())) => {
                    info!("✅ 获取 HTTP TTS 客户端成功");
                },
                Ok(Err(e)) => {
                    error!("❌ 获取 HTTP TTS 客户端失败: {}", e);
                    return Err(anyhow!("TTS 客户端不可用: {}", e));
                },
                Err(_) => {
                    error!("⏳ 获取 HTTP TTS 客户端超时");
                    return Err(anyhow!("TTS 客户端获取超时"));
                },
            }
        }

        // 🚀 发送句子到目标 TTS（使用清洗后的文本）
        match self.tts_controller.synthesize_with_engine(engine, &cleaned).await {
            Ok(_) => {
                info!(
                    "✅ 句子已发送到 {:?} TTS: '{}'",
                    engine,
                    sentence_text.chars().take(30).collect::<String>()
                );

                // 🎯 标记本轮已经启动了第一句（用于禁止后续"首句自动触发"）
                self.text_splitter_first_chunk_recorded.store(true, Ordering::Release);

                // 🚀 关键优化：process_next_sentence 也要启动下一句预取（缓冲式）
                {
                    let mut queue = self.sentence_queue.lock().await;
                    if let Some(next_sentence) = queue.start_prefetch() {
                        let next_cleaned = sanitize_visible_text(&next_sentence.text);
                        if !next_cleaned.is_empty() {
                            let current_voice_id = self.tts_controller.current_voice_id().await;
                            // 获取轮次内已确认的引擎（用于继承）
                            let inherited_engine = { self.tts_controller.turn_confirmed_engine.lock().await.clone() };
                            // 同声传译模式：强制使用 Edge TTS
                            let force_engine = if self.is_translation_mode { Some(TtsEngineKind::EdgeTts) } else { None };
                            let selection = select_tts_engine(
                                &next_cleaned,
                                current_voice_id.as_deref(),
                                &next_sentence.language_confidences,
                                inherited_engine,
                                force_engine,
                            );
                            let next_engine = selection.engine;
                            // 如果是确定性路由且尚未锁定，更新轮次缓存
                            if selection.is_confident {
                                let mut engine_guard = self.tts_controller.turn_confirmed_engine.lock().await;
                                if engine_guard.is_none() {
                                    info!("🔒 预取路径确定路由，锁定轮次引擎: {:?}", next_engine);
                                    *engine_guard = Some(next_engine);
                                }
                            }
                            let baidu_per_override = current_voice_id
                                .as_deref()
                                .and_then(crate::tts::baidu::baidu_per_for_voice_id)
                                .map(|s| s.to_string());
                            // 关闭旧预取任务
                            {
                                let mut handle_guard = self.inflight_task_handle.lock().await;
                                if let Some(h) = handle_guard.take() {
                                    h.abort();
                                }
                            }
                            // 创建预取缓冲通道
                            let (tx, rx) = mpsc::channel::<AudioChunk>(4096);
                            {
                                let mut rx_guard = self.inflight_audio_rx.lock().await;
                                *rx_guard = Some(rx);
                            }
                            // 增益已嵌入 AudioChunk，无需设置共享状态
                            // 启动独立HTTP流
                            let tts_ctrl_for_prefetch = self.tts_controller.clone();
                            let handle = tokio::spawn(async move {
                                match next_engine {
                                    TtsEngineKind::MiniMax => {
                                        let cfg = tts_ctrl_for_prefetch.tts_config.clone().unwrap_or_default();
                                        let client = MiniMaxHttpTtsClient::new(cfg.clone());
                                        let voice_setting = { tts_ctrl_for_prefetch.voice_setting.lock().await.clone() };
                                        // 优先使用轮次缓存，缓存为空时进行语言检测并更新
                                        let (virtual_voice_id, lang) = {
                                            let mut turn_voice_guard = tts_ctrl_for_prefetch.turn_detected_voice_id.lock().await;
                                            let mut turn_lang_guard = tts_ctrl_for_prefetch.turn_detected_language.lock().await;

                                            if turn_voice_guard.is_some() {
                                                // 缓存已有值，直接使用
                                                (turn_voice_guard.clone().unwrap(), turn_lang_guard.clone())
                                            } else {
                                                // 获取用户配置的音色
                                                let configured_voice = tts_ctrl_for_prefetch.current_voice_id().await.unwrap_or_else(|| {
                                                    cfg.default_voice_id
                                                        .clone()
                                                        .unwrap_or_else(|| "zh_female_wanwanxiaohe_moon_bigtts".to_string())
                                                });
                                                let is_default_voice = configured_voice == "zh_female_wanwanxiaohe_moon_bigtts";

                                                // 缓存为空，进行语言检测
                                                if let Some(detected_lang) = detect_language_boost(&next_cleaned, LINGUA_MIN_CONFIDENCE, LINGUA_MIN_MARGIN) {
                                                    *turn_lang_guard = Some(detected_lang.clone());
                                                    // 只有默认音色时才自动切换
                                                    if is_default_voice {
                                                        if let Some(voice) = get_voice_for_language(&detected_lang) {
                                                            *turn_voice_guard = Some(voice.to_string());
                                                            info!("🎙️ [预取] 检测到语言并设置音色: lang={}, voice={}", detected_lang, voice);
                                                            (voice.to_string(), Some(detected_lang))
                                                        } else {
                                                            *turn_voice_guard = Some(configured_voice.clone());
                                                            info!(
                                                                "🎙️ [预取] 检测到语言但无映射，锁定fallback: lang={}, voice={}",
                                                                detected_lang, configured_voice
                                                            );
                                                            (configured_voice, Some(detected_lang))
                                                        }
                                                    } else {
                                                        // 用户设置了自定义音色，不自动切换
                                                        *turn_voice_guard = Some(configured_voice.clone());
                                                        info!(
                                                            "🎙️ [预取] 用户设置了自定义音色，跳过自动切换: voice={}, lang={}",
                                                            configured_voice, detected_lang
                                                        );
                                                        (configured_voice, Some(detected_lang))
                                                    }
                                                } else {
                                                    // 检测失败，使用客户端配置
                                                    (configured_voice, tts_ctrl_for_prefetch.language.lock().await.clone())
                                                }
                                            }
                                        };
                                        match client
                                            .synthesize_text(
                                                &virtual_voice_id,
                                                &next_cleaned,
                                                voice_setting,
                                                Some(AudioSetting::default()),
                                                None,
                                                None,
                                                lang,
                                                MiniMaxHttpOptions::default(),
                                            )
                                            .await
                                        {
                                            Ok(stream) => {
                                                tokio::pin!(stream);
                                                while let Some(item) = stream.next().await {
                                                    match item {
                                                        Ok(chunk) => {
                                                            if tx.send(chunk).await.is_err() {
                                                                break;
                                                            }
                                                        },
                                                        Err(e) => {
                                                            warn!("⚠️ 预取MiniMax流错误: {}", e);
                                                            break;
                                                        },
                                                    }
                                                }
                                            },
                                            Err(e) => warn!("⚠️ 启动MiniMax预取失败: {}", e),
                                        }
                                    },
                                    TtsEngineKind::VolcEngine => match VolcEngineTtsClient::from_env() {
                                        Ok(client) => {
                                            let mut req = VolcEngineRequest::from_text(next_cleaned.clone());
                                            req.emotion = Some("energetic".to_string());
                                            match client.stream_sentence(req) {
                                                Ok(stream) => {
                                                    tokio::pin!(stream);
                                                    while let Some(item) = stream.next().await {
                                                        match item {
                                                            Ok(chunk) => {
                                                                if tx.send(chunk).await.is_err() {
                                                                    break;
                                                                }
                                                            },
                                                            Err(e) => {
                                                                warn!("⚠️ 预取Volc流错误: {}", e);
                                                                break;
                                                            },
                                                        }
                                                    }
                                                },
                                                Err(e) => warn!("⚠️ 启动Volc预取失败: {}", e),
                                            }
                                        },
                                        Err(e) => warn!("⚠️ 构建Volc客户端失败: {}", e),
                                    },
                                    TtsEngineKind::Baidu => match crate::tts::baidu::BaiduHttpTtsClient::from_env() {
                                        Ok(client) => {
                                            let mut req = crate::tts::baidu::BaiduHttpTtsRequest::new(next_cleaned.clone());
                                            if let Some(ref per) = baidu_per_override {
                                                req = req.with_per(per.clone());
                                            }
                                            if let Some(voice_id) = current_voice_id.as_deref() {
                                                if let Some(payload) = crate::tts::baidu::baidu_payload_override_for_voice_id(voice_id, client.config().build_start_payload()) {
                                                    if let Some(spd) = payload.spd {
                                                        req = req.with_spd(spd);
                                                    }
                                                    if let Some(pit) = payload.pit {
                                                        req = req.with_pit(pit);
                                                    }
                                                    if let Some(vol) = payload.vol {
                                                        req = req.with_vol(vol);
                                                    }
                                                }
                                            }
                                            match client.synthesize(req) {
                                                Ok(stream) => {
                                                    tokio::pin!(stream);
                                                    while let Some(item) = stream.next().await {
                                                        match item {
                                                            Ok(chunk) => {
                                                                if tx.send(chunk).await.is_err() {
                                                                    break;
                                                                }
                                                            },
                                                            Err(e) => {
                                                                warn!("⚠️ 预取Baidu流错误: {}", e);
                                                                break;
                                                            },
                                                        }
                                                    }
                                                },
                                                Err(e) => warn!("⚠️ 启动Baidu预取失败: {}", e),
                                            }
                                        },
                                        Err(e) => warn!("⚠️ 构建Baidu客户端失败: {}", e),
                                    },
                                    TtsEngineKind::EdgeTts => {
                                        let client = crate::tts::edge::EdgeTtsClient::with_defaults();
                                        // 根据语言选择声音
                                        let voice = crate::tts::edge::get_voice_for_language("zh").unwrap_or("zh-CN-XiaoxiaoNeural");
                                        match client.synthesize(&next_cleaned, Some(voice)).await {
                                            Ok(stream) => {
                                                tokio::pin!(stream);
                                                while let Some(item) = stream.next().await {
                                                    match item {
                                                        Ok(chunk) => {
                                                            if tx.send(chunk).await.is_err() {
                                                                break;
                                                            }
                                                        },
                                                        Err(e) => {
                                                            warn!("⚠️ 预取Edge TTS流错误: {}", e);
                                                            break;
                                                        },
                                                    }
                                                }
                                            },
                                            Err(e) => warn!("⚠️ 启动Edge TTS预取失败: {}", e),
                                        }
                                    },
                                    TtsEngineKind::AzureTts => match crate::tts::azure::AzureTtsClient::from_env() {
                                        Ok(client) => {
                                            let voice = crate::tts::azure::get_voice_for_language("zh").unwrap_or("zh-CN-XiaoxiaoNeural");
                                            match client.synthesize(&next_cleaned, Some(voice)).await {
                                                Ok(stream) => {
                                                    tokio::pin!(stream);
                                                    while let Some(item) = stream.next().await {
                                                        match item {
                                                            Ok(chunk) => {
                                                                if tx.send(chunk).await.is_err() {
                                                                    break;
                                                                }
                                                            },
                                                            Err(e) => {
                                                                warn!("⚠️ 预取Azure TTS流错误: {}", e);
                                                                break;
                                                            },
                                                        }
                                                    }
                                                },
                                                Err(e) => warn!("⚠️ 启动Azure TTS预取失败: {}", e),
                                            }
                                        },
                                        Err(e) => warn!("⚠️ 创建Azure TTS客户端失败: {}", e),
                                    },
                                }
                            });
                            {
                                let mut handle_guard = self.inflight_task_handle.lock().await;
                                *handle_guard = Some(handle);
                            }
                        }
                    }
                }

                // ✅ 修复：检查音频处理任务是否需要重启（避免无条件重启导致状态丢失）
                let should_skip_spawn = {
                    let task_guard = self.audio_handler_task.lock().await;
                    task_guard.as_ref().map(|h| !h.is_finished()).unwrap_or(false)
                };

                if should_skip_spawn {
                    // 任务正在运行，跳过重启
                    info!("✅ 音频处理任务已运行，跳过重启: session={}", self.session_id);
                } else if self.tts_controller.has_audio_subscription().await {
                    // 任务不存在或已结束，需要重启
                    match self.tts_controller.subscribe_audio().await {
                        Ok(audio_rx) => {
                            info!("🔄 音频处理任务需要重启: session={}", self.session_id);
                            self.spawn_session_audio_handler(audio_rx).await;
                            info!("✅ 音频处理任务已重新启动: session={}", self.session_id);
                        },
                        Err(e) => {
                            warn!("⚠️ 订阅TTS音频广播失败: {}", e);
                        },
                    }
                }

                Ok(())
            },
            Err(e) => {
                error!("❌ 发送句子到 {:?} TTS 失败: {}", engine, e);
                // 重置客户端
                self.tts_controller.reset_client().await;
                Err(anyhow!("TTS 合成失败: {}", e))
            },
        }
    }

    // ============================================================================

    pub async fn run(mut self) -> Result<()> {
        info!("🚀 启动管线级别 HTTP TTS 任务: {}", self.session_id);
        info!("🔧 TTS任务初始化 - broadcast接收器已配置: session={}", self.session_id);

        // 🆕 启动按需句子处理任务（监听 PacedSender 的触发信号）
        let sentence_queue_clone = self.sentence_queue.clone();
        let tts_controller_clone = self.tts_controller.clone();
        let session_id_clone = self.session_id.clone();
        let current_turn_response_id_clone = self.current_turn_response_id.clone();
        let audio_sending_stopped_clone = self.audio_sending_stopped.clone();
        let assistant_context_clone = self.shared_flags.assistant_response_context.clone();
        // 🆕 标记LLM轮次完成
        let llm_turn_complete = Arc::new(AtomicBool::new(false));
        let llm_turn_complete_clone = llm_turn_complete.clone();
        // 🆕 预取音频缓冲/任务句柄克隆
        let inflight_audio_rx_clone = self.inflight_audio_rx.clone();
        let inflight_task_handle_clone = self.inflight_task_handle.clone();
        // 🆕 收尾幂等标志（供触发路径使用）
        let turn_final_injected_for_trigger = self.turn_final_injected.clone();
        // 🆕 同声传译模式：强制使用 Edge TTS
        let is_translation_mode_for_trigger = self.is_translation_mode;

        let trigger_rx = {
            let mut guard = self.next_sentence_trigger_rx.lock().await;
            guard.take()
        };

        if let Some(mut rx) = trigger_rx {
            tokio::spawn(async move {
                info!("🎯 按需句子处理任务已启动: session={}", session_id_clone);

                while let Some(_trigger) = rx.recv().await {
                    debug!("🎯 收到下一句触发信号: session={}", session_id_clone);

                    // 🆕 Phase 3: 若被打断，跳过任何触发处理，避免继续合成占用配额
                    if audio_sending_stopped_clone.load(Ordering::Acquire) {
                        debug!("🚫 音频发送已停止（打断），跳过触发的下一句处理: session={}", session_id_clone);
                        continue;
                    }

                    // 🚀 优化：优先使用预取的句子（已在后台合成中，无延迟）
                    let sentence_opt = {
                        let mut queue = sentence_queue_clone.lock().await;
                        queue.consume_inflight() // 先尝试消费预取句子
                    };

                    if let Some(sentence) = sentence_opt {
                        // ⚡ 使用预取句子：音频可能已在广播中或即将到达，无需等待TTS
                        info!("⚡ 触发路径使用预取句子（inflight），跳过TTS等待");

                        let sentence_text = &sentence.text;
                        let cleaned = sanitize_visible_text(sentence_text);
                        if cleaned.is_empty() {
                            info!("🧹 预取句清洗后为空，跳过");

                            // 🧭 若这是本轮的最后一句（队列已空且LLM轮次完成），发送控制final关闭本轮
                            let is_last_sentence_of_turn = {
                                let q = sentence_queue_clone.lock().await;
                                q.is_empty() && !q.has_inflight()
                            } && llm_turn_complete_clone.load(Ordering::Acquire);

                            if is_last_sentence_of_turn {
                                let response_id_for_final = current_turn_response_id_clone
                                    .load()
                                    .unwrap_or_else(|| format!("resp_{}", nanoid::nanoid!(8)));

                                let final_chunk = PacedAudioChunk {
                                    audio_data: Bytes::new(),
                                    is_final: true,
                                    realtime_metadata: Some(RealtimeAudioMetadata {
                                        response_id: response_id_for_final,
                                        assistant_item_id: assistant_context_clone
                                            .get_context_copy()
                                            .map(|c| c.assistant_item_id)
                                            .unwrap_or_else(|| format!("asst_{}", nanoid::nanoid!(6))),
                                        output_index: 0,
                                        content_index: 0,
                                    }),
                                    sentence_text: None,
                                    turn_final: true,
                                };

                                if !audio_sending_stopped_clone.load(Ordering::Acquire) {
                                    let sender_guard = tts_controller_clone.session_audio_sender.lock().await;
                                    if let Err(e) = sender_guard.send_audio(final_chunk).await {
                                        warn!("⚠️ 发送控制final失败: {}", e);
                                    } else {
                                        info!("🏁 预取句为空且为最后一句：已发送控制final以触发 text.done/stopped");
                                    }
                                } else {
                                    debug!("🚫 音频发送已停止（打断），跳过控制final");
                                }
                            } else {
                                // 非最后一句：注入句尾 empty final，以事务性触发下一句生成
                                let response_id_for_final = current_turn_response_id_clone
                                    .load()
                                    .unwrap_or_else(|| format!("resp_{}", nanoid::nanoid!(8)));

                                let empty_sentence_final = PacedAudioChunk {
                                    audio_data: Bytes::new(),
                                    is_final: true,
                                    realtime_metadata: Some(RealtimeAudioMetadata {
                                        response_id: response_id_for_final,
                                        assistant_item_id: assistant_context_clone
                                            .get_context_copy()
                                            .map(|c| c.assistant_item_id)
                                            .unwrap_or_else(|| format!("asst_{}", nanoid::nanoid!(6))),
                                        output_index: 0,
                                        content_index: 0,
                                    }),
                                    sentence_text: None,
                                    turn_final: false,
                                };

                                if !audio_sending_stopped_clone.load(Ordering::Acquire) {
                                    let sender_guard = tts_controller_clone.session_audio_sender.lock().await;
                                    if let Err(e) = sender_guard.send_audio(empty_sentence_final).await {
                                        warn!("⚠️ 发送句尾 empty final 失败: {}", e);
                                    } else {
                                        info!("🚀 预取句为空：已注入句尾 empty final 以触发下一句生成");
                                    }
                                } else {
                                    debug!("🚫 音频发送已停止（打断），跳过句尾 empty final 注入");
                                }

                                // 标记本句处理完成（无需进入预取回放的drain路径）
                                {
                                    let mut q = sentence_queue_clone.lock().await;
                                    q.mark_current_processing_complete();
                                }
                            }

                            // 🚀 尝试启动下一句预取（缓冲式，不通过全局广播）
                            {
                                let mut queue = sentence_queue_clone.lock().await;
                                if let Some(next_sentence) = queue.start_prefetch() {
                                    let next_cleaned = sanitize_visible_text(&next_sentence.text);
                                    if !next_cleaned.is_empty() {
                                        let current_voice_id = tts_controller_clone.current_voice_id().await;
                                        // 获取轮次内已确认的引擎（用于继承）
                                        let inherited_engine = { tts_controller_clone.turn_confirmed_engine.lock().await.clone() };
                                        // 同声传译模式：强制使用 Edge TTS
                                        let force_engine = if is_translation_mode_for_trigger {
                                            Some(TtsEngineKind::EdgeTts)
                                        } else {
                                            None
                                        };
                                        let selection = select_tts_engine(
                                            &next_cleaned,
                                            current_voice_id.as_deref(),
                                            &next_sentence.language_confidences,
                                            inherited_engine,
                                            force_engine,
                                        );
                                        let engine = selection.engine;
                                        // 如果是确定性路由且尚未锁定，更新轮次缓存
                                        if selection.is_confident {
                                            let mut engine_guard = tts_controller_clone.turn_confirmed_engine.lock().await;
                                            if engine_guard.is_none() {
                                                info!("🔒 预取路径确定路由，锁定轮次引擎: {:?}", engine);
                                                *engine_guard = Some(engine);
                                            }
                                        }
                                        // 关闭旧预取任务
                                        {
                                            let mut handle_guard = inflight_task_handle_clone.lock().await;
                                            if let Some(h) = handle_guard.take() {
                                                h.abort();
                                            }
                                        }
                                        // 创建预取缓冲通道
                                        let (tx, rx) = mpsc::channel::<AudioChunk>(4096);
                                        {
                                            let mut rx_guard = inflight_audio_rx_clone.lock().await;
                                            *rx_guard = Some(rx);
                                        }
                                        // 增益已嵌入 AudioChunk，无需设置共享状态
                                        // 启动独立HTTP流，写入缓冲通道
                                        let tts_ctrl_for_prefetch = tts_controller_clone.clone();
                                        let handle = tokio::spawn(async move {
                                            match engine {
                                                TtsEngineKind::MiniMax => {
                                                    let cfg = tts_ctrl_for_prefetch.tts_config.clone().unwrap_or_default();
                                                    let client = MiniMaxHttpTtsClient::new(cfg.clone());
                                                    let voice_setting = { tts_ctrl_for_prefetch.voice_setting.lock().await.clone() };
                                                    // 优先使用轮次缓存，缓存为空时进行语言检测并更新
                                                    let (virtual_voice_id, lang) = {
                                                        let mut turn_voice_guard = tts_ctrl_for_prefetch.turn_detected_voice_id.lock().await;
                                                        let mut turn_lang_guard = tts_ctrl_for_prefetch.turn_detected_language.lock().await;

                                                        if turn_voice_guard.is_some() {
                                                            (turn_voice_guard.clone().unwrap(), turn_lang_guard.clone())
                                                        } else {
                                                            // 获取用户配置的音色
                                                            let configured_voice = tts_ctrl_for_prefetch.current_voice_id().await.unwrap_or_else(|| {
                                                                cfg.default_voice_id
                                                                    .clone()
                                                                    .unwrap_or_else(|| "zh_female_wanwanxiaohe_moon_bigtts".to_string())
                                                            });
                                                            let is_default_voice = configured_voice == "zh_female_wanwanxiaohe_moon_bigtts";

                                                            if let Some(detected_lang) = detect_language_boost(&next_cleaned, LINGUA_MIN_CONFIDENCE, LINGUA_MIN_MARGIN) {
                                                                *turn_lang_guard = Some(detected_lang.clone());
                                                                // 只有默认音色时才自动切换
                                                                if is_default_voice {
                                                                    if let Some(voice) = get_voice_for_language(&detected_lang) {
                                                                        *turn_voice_guard = Some(voice.to_string());
                                                                        info!("🎙️ [预取] 检测到语言并设置音色: lang={}, voice={}", detected_lang, voice);
                                                                        (voice.to_string(), Some(detected_lang))
                                                                    } else {
                                                                        *turn_voice_guard = Some(configured_voice.clone());
                                                                        info!(
                                                                            "🎙️ [预取] 检测到语言但无映射，锁定fallback: lang={}, voice={}",
                                                                            detected_lang, configured_voice
                                                                        );
                                                                        (configured_voice, Some(detected_lang))
                                                                    }
                                                                } else {
                                                                    // 用户设置了自定义音色，不自动切换
                                                                    *turn_voice_guard = Some(configured_voice.clone());
                                                                    info!(
                                                                        "🎙️ [预取] 用户设置了自定义音色，跳过自动切换: voice={}, lang={}",
                                                                        configured_voice, detected_lang
                                                                    );
                                                                    (configured_voice, Some(detected_lang))
                                                                }
                                                            } else {
                                                                (configured_voice, tts_ctrl_for_prefetch.language.lock().await.clone())
                                                            }
                                                        }
                                                    };
                                                    match client
                                                        .synthesize_text(
                                                            &virtual_voice_id,
                                                            &next_cleaned,
                                                            voice_setting,
                                                            None,
                                                            None,
                                                            None,
                                                            lang,
                                                            MiniMaxHttpOptions::default(),
                                                        )
                                                        .await
                                                    {
                                                        Ok(stream) => {
                                                            tokio::pin!(stream);
                                                            while let Some(item) = stream.next().await {
                                                                match item {
                                                                    Ok(chunk) => {
                                                                        if tx.send(chunk).await.is_err() {
                                                                            break;
                                                                        }
                                                                    },
                                                                    Err(e) => {
                                                                        warn!("⚠️ 预取MiniMax流错误: {}", e);
                                                                        break;
                                                                    },
                                                                }
                                                            }
                                                        },
                                                        Err(e) => warn!("⚠️ 启动MiniMax预取失败: {}", e),
                                                    }
                                                },
                                                TtsEngineKind::VolcEngine => match VolcEngineTtsClient::from_env() {
                                                    Ok(client) => {
                                                        let mut req = VolcEngineRequest::from_text(next_cleaned.clone());
                                                        req.emotion = Some("energetic".to_string());
                                                        match client.stream_sentence(req) {
                                                            Ok(stream) => {
                                                                tokio::pin!(stream);
                                                                while let Some(item) = stream.next().await {
                                                                    match item {
                                                                        Ok(chunk) => {
                                                                            if tx.send(chunk).await.is_err() {
                                                                                break;
                                                                            }
                                                                        },
                                                                        Err(e) => {
                                                                            warn!("⚠️ 预取Volc流错误: {}", e);
                                                                            break;
                                                                        },
                                                                    }
                                                                }
                                                            },
                                                            Err(e) => warn!("⚠️ 启动Volc预取失败: {}", e),
                                                        }
                                                    },
                                                    Err(e) => warn!("⚠️ 构建Volc客户端失败: {}", e),
                                                },
                                                TtsEngineKind::Baidu => match crate::tts::baidu::BaiduHttpTtsClient::from_env() {
                                                    Ok(client) => {
                                                        let mut req = crate::tts::baidu::BaiduHttpTtsRequest::new(next_cleaned.clone());
                                                        if let Some(voice_id) = tts_ctrl_for_prefetch.current_voice_id().await {
                                                            if let Some(per) = crate::tts::baidu::baidu_per_for_voice_id(&voice_id) {
                                                                req = req.with_per(per);
                                                            }
                                                            if let Some(payload) = crate::tts::baidu::baidu_payload_override_for_voice_id(&voice_id, client.config().build_start_payload()) {
                                                                if let Some(spd) = payload.spd {
                                                                    req = req.with_spd(spd);
                                                                }
                                                                if let Some(pit) = payload.pit {
                                                                    req = req.with_pit(pit);
                                                                }
                                                                if let Some(vol) = payload.vol {
                                                                    req = req.with_vol(vol);
                                                                }
                                                            }
                                                        }
                                                        match client.synthesize(req) {
                                                            Ok(stream) => {
                                                                tokio::pin!(stream);
                                                                while let Some(item) = stream.next().await {
                                                                    match item {
                                                                        Ok(chunk) => {
                                                                            if tx.send(chunk).await.is_err() {
                                                                                break;
                                                                            }
                                                                        },
                                                                        Err(e) => {
                                                                            warn!("⚠️ 预取Baidu流错误: {}", e);
                                                                            break;
                                                                        },
                                                                    }
                                                                }
                                                            },
                                                            Err(e) => warn!("⚠️ 启动Baidu预取失败: {}", e),
                                                        }
                                                    },
                                                    Err(e) => warn!("⚠️ 构建Baidu客户端失败: {}", e),
                                                },
                                                TtsEngineKind::EdgeTts => {
                                                    let client = crate::tts::edge::EdgeTtsClient::with_defaults();
                                                    let voice = crate::tts::edge::get_voice_for_language("zh").unwrap_or("zh-CN-XiaoxiaoNeural");
                                                    match client.synthesize(&next_cleaned, Some(voice)).await {
                                                        Ok(stream) => {
                                                            tokio::pin!(stream);
                                                            while let Some(item) = stream.next().await {
                                                                match item {
                                                                    Ok(chunk) => {
                                                                        if tx.send(chunk).await.is_err() {
                                                                            break;
                                                                        }
                                                                    },
                                                                    Err(e) => {
                                                                        warn!("⚠️ 预取Edge TTS流错误: {}", e);
                                                                        break;
                                                                    },
                                                                }
                                                            }
                                                        },
                                                        Err(e) => warn!("⚠️ 启动Edge TTS预取失败: {}", e),
                                                    }
                                                },
                                                TtsEngineKind::AzureTts => match crate::tts::azure::AzureTtsClient::from_env() {
                                                    Ok(client) => {
                                                        let voice = crate::tts::azure::get_voice_for_language("zh").unwrap_or("zh-CN-XiaoxiaoNeural");
                                                        match client.synthesize(&next_cleaned, Some(voice)).await {
                                                            Ok(stream) => {
                                                                tokio::pin!(stream);
                                                                while let Some(item) = stream.next().await {
                                                                    match item {
                                                                        Ok(chunk) => {
                                                                            if tx.send(chunk).await.is_err() {
                                                                                break;
                                                                            }
                                                                        },
                                                                        Err(e) => {
                                                                            warn!("⚠️ 预取Azure TTS流错误: {}", e);
                                                                            break;
                                                                        },
                                                                    }
                                                                }
                                                            },
                                                            Err(e) => warn!("⚠️ 启动Azure TTS预取失败: {}", e),
                                                        }
                                                    },
                                                    Err(e) => warn!("⚠️ 创建Azure TTS客户端失败: {}", e),
                                                },
                                            }
                                        });
                                        {
                                            let mut handle_guard = inflight_task_handle_clone.lock().await;
                                            *handle_guard = Some(handle);
                                        }
                                    }
                                }
                            }
                            continue;
                        }

                        // 延后文字事件：在首个音频分片发送时由 PacedSender 发出
                        let cleaned_for_drain = cleaned.clone();

                        // 🎵 播放预取音频：从缓冲通道读取并送入 SessionAudioSender
                        {
                            // 🔧 关键：先取走当前预取任务句柄，避免下面启动下一句预取时把当前播放的HTTP流 abort 掉
                            {
                                let mut handle_guard = inflight_task_handle_clone.lock().await;
                                let _ = handle_guard.take(); // 不 abort，任其自然结束
                            }
                            let rx_opt = { inflight_audio_rx_clone.lock().await.take() };
                            if let Some(mut rx) = rx_opt {
                                let tts_controller_for_drain = tts_controller_clone.clone();
                                let session_audio_sender = tts_controller_for_drain.session_audio_sender.clone();
                                let audio_stop_flag = audio_sending_stopped_clone.clone();
                                let sentence_queue_for_drain = sentence_queue_clone.clone();
                                let current_turn_response_id_for_drain = current_turn_response_id_clone.clone();
                                let assistant_context_for_drain = assistant_context_clone.clone();
                                let turn_final_injected_flag = turn_final_injected_for_trigger.clone();
                                let session_id_for_drain = session_id_clone.clone();
                                tokio::spawn(async move {
                                    let mut first_sentence_chunk = true;
                                    let mut current_assistant_item_id = String::new();
                                    while let Some(chunk) = rx.recv().await {
                                        if audio_stop_flag.load(Ordering::Acquire) {
                                            break;
                                        }
                                        // 处理空final（句内）
                                        if chunk.data.is_empty() && chunk.is_final {
                                            // 若整句未产生任何非空音频（首分片标志仍为 true），在 final 之前补发带文字的空块，确保有 text.delta
                                            if first_sentence_chunk {
                                                let text_chunk = PacedAudioChunk {
                                                    audio_data: Bytes::new(),
                                                    is_final: false,
                                                    realtime_metadata: Some(RealtimeAudioMetadata {
                                                        response_id: current_turn_response_id_for_drain
                                                            .load()
                                                            .unwrap_or_else(|| format!("resp_{}", nanoid::nanoid!(8))),
                                                        assistant_item_id: if !current_assistant_item_id.is_empty() {
                                                            current_assistant_item_id.clone()
                                                        } else {
                                                            assistant_context_for_drain
                                                                .get_context_copy()
                                                                .map(|c| c.assistant_item_id)
                                                                .unwrap_or_else(|| format!("asst_{}", nanoid::nanoid!(6)))
                                                        },
                                                        output_index: 0,
                                                        content_index: 0,
                                                    }),
                                                    sentence_text: Some(cleaned_for_drain.clone()),
                                                    turn_final: false,
                                                };
                                                let sender_guard = session_audio_sender.lock().await;
                                                let _ = sender_guard.send_audio(text_chunk).await;
                                                drop(sender_guard);
                                            }

                                            let resp_id = current_turn_response_id_for_drain
                                                .load()
                                                .unwrap_or_else(|| format!("resp_{}", nanoid::nanoid!(8)));
                                            let final_chunk = PacedAudioChunk {
                                                audio_data: Bytes::new(),
                                                is_final: true,
                                                realtime_metadata: Some(RealtimeAudioMetadata {
                                                    response_id: resp_id,
                                                    assistant_item_id: if !current_assistant_item_id.is_empty() {
                                                        current_assistant_item_id.clone()
                                                    } else {
                                                        assistant_context_for_drain
                                                            .get_context_copy()
                                                            .map(|c| c.assistant_item_id)
                                                            .unwrap_or_else(|| format!("asst_{}", nanoid::nanoid!(6)))
                                                    },
                                                    output_index: 0,
                                                    content_index: 0,
                                                }),
                                                sentence_text: None,
                                                turn_final: false,
                                            };
                                            let sender_guard = session_audio_sender.lock().await;
                                            let _ = sender_guard.send_audio(final_chunk).await;
                                            drop(sender_guard);
                                            // 标记本句完成
                                            {
                                                let mut q = sentence_queue_for_drain.lock().await;
                                                q.mark_current_processing_complete();
                                            }
                                            // 如需收尾整轮
                                            let should_turn_final = {
                                                let q = sentence_queue_for_drain.lock().await;
                                                q.is_llm_complete() && !q.has_pending() && !q.has_inflight()
                                            };
                                            if should_turn_final && !turn_final_injected_flag.swap(true, Ordering::AcqRel) {
                                                let resp_id = current_turn_response_id_for_drain
                                                    .load()
                                                    .unwrap_or_else(|| format!("resp_{}", nanoid::nanoid!(8)));
                                                let final_ctrl = PacedAudioChunk {
                                                    audio_data: Bytes::new(),
                                                    is_final: true,
                                                    realtime_metadata: Some(RealtimeAudioMetadata {
                                                        response_id: resp_id,
                                                        assistant_item_id: if !current_assistant_item_id.is_empty() {
                                                            current_assistant_item_id.clone()
                                                        } else {
                                                            assistant_context_for_drain
                                                                .get_context_copy()
                                                                .map(|c| c.assistant_item_id)
                                                                .unwrap_or_else(|| format!("asst_{}", nanoid::nanoid!(6)))
                                                        },
                                                        output_index: 0,
                                                        content_index: 0,
                                                    }),
                                                    sentence_text: None,
                                                    turn_final: true,
                                                };
                                                let sender_guard = session_audio_sender.lock().await;
                                                let _ = sender_guard.send_audio(final_ctrl).await;
                                                drop(sender_guard);
                                                info!("🏁 预取路径：已注入turn-final收尾: session={}", session_id_for_drain);
                                            }
                                            break;
                                        }
                                        // 常规音频帧
                                        let resp_id = current_turn_response_id_for_drain
                                            .load()
                                            .unwrap_or_else(|| format!("resp_{}", nanoid::nanoid!(8)));
                                        if current_assistant_item_id.is_empty() {
                                            current_assistant_item_id = assistant_context_for_drain
                                                .get_context_copy()
                                                .map(|c| c.assistant_item_id)
                                                .unwrap_or_else(|| format!("asst_{}", nanoid::nanoid!(6)));
                                        }
                                        // 在首个非空音频分片发送前，先注入带文字的空块，触发 text.delta
                                        if first_sentence_chunk && !chunk.data.is_empty() {
                                            let text_chunk = PacedAudioChunk {
                                                audio_data: Bytes::new(),
                                                is_final: false,
                                                realtime_metadata: Some(RealtimeAudioMetadata {
                                                    response_id: resp_id.clone(),
                                                    assistant_item_id: current_assistant_item_id.clone(),
                                                    output_index: 0,
                                                    content_index: chunk.sequence_id as u32,
                                                }),
                                                sentence_text: Some(cleaned_for_drain.clone()),
                                                turn_final: false,
                                            };
                                            let sender_guard = session_audio_sender.lock().await;
                                            let _ = sender_guard.send_audio(text_chunk).await;
                                            drop(sender_guard);
                                            first_sentence_chunk = false;
                                        }
                                        // 🆕 将44100Hz原始音频降采样到16kHz用于播放（使用 chunk 自带的增益值）
                                        let tts_frame = TtsAudioFrame::from_audio_chunk(&chunk);
                                        let resampled_data = tts_frame.process_to_16k_with_db_gain(chunk.gain_db);
                                        let paced_chunk = PacedAudioChunk {
                                            audio_data: Bytes::from(resampled_data),
                                            is_final: chunk.is_final,
                                            realtime_metadata: Some(RealtimeAudioMetadata {
                                                response_id: resp_id.clone(),
                                                assistant_item_id: current_assistant_item_id.clone(),
                                                output_index: 0,
                                                content_index: chunk.sequence_id as u32,
                                            }),
                                            sentence_text: None,
                                            turn_final: false,
                                        };
                                        let sender_guard = session_audio_sender.lock().await;
                                        let _ = sender_guard.send_audio(paced_chunk).await;
                                        drop(sender_guard);
                                        if chunk.is_final {
                                            first_sentence_chunk = true;
                                        }
                                    }
                                });
                            } else {
                                debug!("📭 无预取音频可播放: session={}", session_id_clone);
                            }
                        }
                        // 🚀 播放当前inflight后，立即为下一句启动新的预取
                        {
                            let mut queue = sentence_queue_clone.lock().await;
                            if let Some(next_sentence) = queue.start_prefetch() {
                                let next_cleaned = sanitize_visible_text(&next_sentence.text);
                                if !next_cleaned.is_empty() {
                                    let current_voice_id = tts_controller_clone.current_voice_id().await;
                                    // 获取轮次内已确认的引擎（用于继承）
                                    let inherited_engine = { tts_controller_clone.turn_confirmed_engine.lock().await.clone() };
                                    // 同声传译模式：强制使用 Edge TTS
                                    let force_engine = if is_translation_mode_for_trigger {
                                        Some(TtsEngineKind::EdgeTts)
                                    } else {
                                        None
                                    };
                                    let selection = select_tts_engine(
                                        &next_cleaned,
                                        current_voice_id.as_deref(),
                                        &next_sentence.language_confidences,
                                        inherited_engine,
                                        force_engine,
                                    );
                                    let engine = selection.engine;
                                    // 如果是确定性路由且尚未锁定，更新轮次缓存
                                    if selection.is_confident {
                                        let mut engine_guard = tts_controller_clone.turn_confirmed_engine.lock().await;
                                        if engine_guard.is_none() {
                                            info!("🔒 预取路径确定路由，锁定轮次引擎: {:?}", engine);
                                            *engine_guard = Some(engine);
                                        }
                                    }
                                    // 关闭旧预取任务
                                    {
                                        let mut handle_guard = inflight_task_handle_clone.lock().await;
                                        if let Some(h) = handle_guard.take() {
                                            h.abort();
                                        }
                                    }
                                    // 创建预取缓冲通道
                                    let (tx, rx) = mpsc::channel::<AudioChunk>(4096);
                                    {
                                        let mut rx_guard = inflight_audio_rx_clone.lock().await;
                                        *rx_guard = Some(rx);
                                    }
                                    // 启动独立HTTP流
                                    let tts_ctrl_for_prefetch = tts_controller_clone.clone();
                                    let handle = tokio::spawn(async move {
                                        match engine {
                                            TtsEngineKind::MiniMax => {
                                                let cfg = tts_ctrl_for_prefetch.tts_config.clone().unwrap_or_default();
                                                let client = MiniMaxHttpTtsClient::new(cfg.clone());
                                                let voice_setting = { tts_ctrl_for_prefetch.voice_setting.lock().await.clone() };
                                                // 优先使用轮次缓存，缓存为空时进行语言检测并更新
                                                let (virtual_voice_id, lang) = {
                                                    let mut turn_voice_guard = tts_ctrl_for_prefetch.turn_detected_voice_id.lock().await;
                                                    let mut turn_lang_guard = tts_ctrl_for_prefetch.turn_detected_language.lock().await;

                                                    if turn_voice_guard.is_some() {
                                                        (turn_voice_guard.clone().unwrap(), turn_lang_guard.clone())
                                                    } else {
                                                        // 获取用户配置的音色
                                                        let configured_voice = tts_ctrl_for_prefetch.current_voice_id().await.unwrap_or_else(|| {
                                                            cfg.default_voice_id
                                                                .clone()
                                                                .unwrap_or_else(|| "zh_female_wanwanxiaohe_moon_bigtts".to_string())
                                                        });
                                                        let is_default_voice = configured_voice == "zh_female_wanwanxiaohe_moon_bigtts";

                                                        if let Some(detected_lang) = detect_language_boost(&next_cleaned, LINGUA_MIN_CONFIDENCE, LINGUA_MIN_MARGIN) {
                                                            *turn_lang_guard = Some(detected_lang.clone());
                                                            // 只有默认音色时才自动切换
                                                            if is_default_voice {
                                                                if let Some(voice) = get_voice_for_language(&detected_lang) {
                                                                    *turn_voice_guard = Some(voice.to_string());
                                                                    info!("🎙️ [预取] 检测到语言并设置音色: lang={}, voice={}", detected_lang, voice);
                                                                    (voice.to_string(), Some(detected_lang))
                                                                } else {
                                                                    *turn_voice_guard = Some(configured_voice.clone());
                                                                    info!(
                                                                        "🎙️ [预取] 检测到语言但无映射，锁定fallback: lang={}, voice={}",
                                                                        detected_lang, configured_voice
                                                                    );
                                                                    (configured_voice, Some(detected_lang))
                                                                }
                                                            } else {
                                                                // 用户设置了自定义音色，不自动切换
                                                                *turn_voice_guard = Some(configured_voice.clone());
                                                                info!(
                                                                    "🎙️ [预取] 用户设置了自定义音色，跳过自动切换: voice={}, lang={}",
                                                                    configured_voice, detected_lang
                                                                );
                                                                (configured_voice, Some(detected_lang))
                                                            }
                                                        } else {
                                                            (configured_voice, tts_ctrl_for_prefetch.language.lock().await.clone())
                                                        }
                                                    }
                                                };
                                                match client
                                                    .synthesize_text(
                                                        &virtual_voice_id,
                                                        &next_cleaned,
                                                        voice_setting,
                                                        Some(AudioSetting::default()),
                                                        None,
                                                        None,
                                                        lang,
                                                        MiniMaxHttpOptions::default(),
                                                    )
                                                    .await
                                                {
                                                    Ok(stream) => {
                                                        tokio::pin!(stream);
                                                        while let Some(item) = stream.next().await {
                                                            match item {
                                                                Ok(chunk) => {
                                                                    if tx.send(chunk).await.is_err() {
                                                                        break;
                                                                    }
                                                                },
                                                                Err(e) => {
                                                                    warn!("⚠️ 预取MiniMax流错误: {}", e);
                                                                    break;
                                                                },
                                                            }
                                                        }
                                                    },
                                                    Err(e) => warn!("⚠️ 启动MiniMax预取失败: {}", e),
                                                }
                                            },
                                            TtsEngineKind::VolcEngine => match VolcEngineTtsClient::from_env() {
                                                Ok(client) => {
                                                    let mut req = VolcEngineRequest::from_text(next_cleaned.clone());
                                                    req.emotion = Some("energetic".to_string());
                                                    match client.stream_sentence(req) {
                                                        Ok(stream) => {
                                                            tokio::pin!(stream);
                                                            while let Some(item) = stream.next().await {
                                                                match item {
                                                                    Ok(chunk) => {
                                                                        if tx.send(chunk).await.is_err() {
                                                                            break;
                                                                        }
                                                                    },
                                                                    Err(e) => {
                                                                        warn!("⚠️ 预取Volc流错误: {}", e);
                                                                        break;
                                                                    },
                                                                }
                                                            }
                                                        },
                                                        Err(e) => warn!("⚠️ 启动Volc预取失败: {}", e),
                                                    }
                                                },
                                                Err(e) => warn!("⚠️ 构建Volc客户端失败: {}", e),
                                            },
                                            TtsEngineKind::Baidu => match crate::tts::baidu::BaiduHttpTtsClient::from_env() {
                                                Ok(client) => {
                                                    let mut req = crate::tts::baidu::BaiduHttpTtsRequest::new(next_cleaned.clone());
                                                    if let Some(voice_id) = tts_ctrl_for_prefetch.current_voice_id().await {
                                                        if let Some(per) = crate::tts::baidu::baidu_per_for_voice_id(&voice_id) {
                                                            req = req.with_per(per);
                                                        }
                                                        if let Some(payload) = crate::tts::baidu::baidu_payload_override_for_voice_id(&voice_id, client.config().build_start_payload()) {
                                                            if let Some(spd) = payload.spd {
                                                                req = req.with_spd(spd);
                                                            }
                                                            if let Some(pit) = payload.pit {
                                                                req = req.with_pit(pit);
                                                            }
                                                            if let Some(vol) = payload.vol {
                                                                req = req.with_vol(vol);
                                                            }
                                                        }
                                                    }
                                                    match client.synthesize(req) {
                                                        Ok(stream) => {
                                                            tokio::pin!(stream);
                                                            while let Some(item) = stream.next().await {
                                                                match item {
                                                                    Ok(chunk) => {
                                                                        if tx.send(chunk).await.is_err() {
                                                                            break;
                                                                        }
                                                                    },
                                                                    Err(e) => {
                                                                        warn!("⚠️ 预取Baidu流错误: {}", e);
                                                                        break;
                                                                    },
                                                                }
                                                            }
                                                        },
                                                        Err(e) => warn!("⚠️ 启动Baidu预取失败: {}", e),
                                                    }
                                                },
                                                Err(e) => warn!("⚠️ 构建Baidu客户端失败: {}", e),
                                            },
                                            TtsEngineKind::EdgeTts => {
                                                let client = crate::tts::edge::EdgeTtsClient::with_defaults();
                                                let voice = crate::tts::edge::get_voice_for_language("zh").unwrap_or("zh-CN-XiaoxiaoNeural");
                                                match client.synthesize(&next_cleaned, Some(voice)).await {
                                                    Ok(stream) => {
                                                        tokio::pin!(stream);
                                                        while let Some(item) = stream.next().await {
                                                            match item {
                                                                Ok(chunk) => {
                                                                    if tx.send(chunk).await.is_err() {
                                                                        break;
                                                                    }
                                                                },
                                                                Err(e) => {
                                                                    warn!("⚠️ 预取Edge TTS流错误: {}", e);
                                                                    break;
                                                                },
                                                            }
                                                        }
                                                    },
                                                    Err(e) => warn!("⚠️ 启动Edge TTS预取失败: {}", e),
                                                }
                                            },
                                            TtsEngineKind::AzureTts => match crate::tts::azure::AzureTtsClient::from_env() {
                                                Ok(client) => {
                                                    let voice = crate::tts::azure::get_voice_for_language("zh").unwrap_or("zh-CN-XiaoxiaoNeural");
                                                    match client.synthesize(&next_cleaned, Some(voice)).await {
                                                        Ok(stream) => {
                                                            tokio::pin!(stream);
                                                            while let Some(item) = stream.next().await {
                                                                match item {
                                                                    Ok(chunk) => {
                                                                        if tx.send(chunk).await.is_err() {
                                                                            break;
                                                                        }
                                                                    },
                                                                    Err(e) => {
                                                                        warn!("⚠️ 预取Azure TTS流错误: {}", e);
                                                                        break;
                                                                    },
                                                                }
                                                            }
                                                        },
                                                        Err(e) => warn!("⚠️ 启动Azure TTS预取失败: {}", e),
                                                    }
                                                },
                                                Err(e) => warn!("⚠️ 创建Azure TTS客户端失败: {}", e),
                                            },
                                        }
                                    });
                                    {
                                        let mut handle_guard = inflight_task_handle_clone.lock().await;
                                        *handle_guard = Some(handle);
                                    }
                                }
                            }
                        }

                        continue; // 预取路径完成，继续等待下一个触发
                    }

                    // 🚫 没有预取句子：说明队列为空或预取逻辑出错
                    debug!("📭 触发时无预取句子可用，队列可能为空: session={}", session_id_clone);

                    // 🧹 检查是否为最后一句（LLM完成且队列已空）
                    let llm_complete = llm_turn_complete_clone.load(Ordering::Acquire);
                    let already_injected = turn_final_injected_for_trigger.load(Ordering::Acquire);
                    let audio_stopped = audio_sending_stopped_clone.load(Ordering::Acquire);

                    // 🔍 增强诊断：获取队列详细状态
                    let (queue_len, queue_empty, queue_llm_complete, has_inflight) = {
                        let q = sentence_queue_clone.lock().await;
                        (q.len(), q.is_empty(), q.is_llm_complete(), q.has_inflight())
                    };

                    let is_last_sentence_of_turn = queue_empty && !has_inflight && llm_complete;

                    if is_last_sentence_of_turn {
                        warn!(
                            "🔍 [队列空触发检查] session={}, llm_complete={}, turn_final_injected={}, audio_stopped={}, queue_len={}, queue_empty={}, queue_llm_complete={}, has_inflight={}（等待句内final后收尾）",
                            session_id_clone, llm_complete, already_injected, audio_stopped, queue_len, queue_empty, queue_llm_complete, has_inflight
                        );

                        // 🔍 诊断：如果LLM完成但队列始终为空，可能是splitter没有产出任何句子
                        if !already_injected {
                            warn!(
                                "🔍 [潜在问题] LLM已完成且队列为空但turn_final未注入，可能splitter未产出句子或音频处理任务的turn-final注入失败: session={}",
                                session_id_clone
                            );
                        }
                    }
                }

                info!("🔚 按需句子处理任务结束: session={}", session_id_clone);
            });
        }

        // 🔧 关键修复：在TTS任务启动时立即初始化音频处理，避免在接收文本时阻塞
        // 预先获取或创建TTS客户端
        info!("🔧 TTS任务开始获取或创建客户端: session={}", self.session_id);

        // 🚀 关键修复：添加超时保护和重试机制，避免TTS客户端获取无限阻塞
        let mut retry_count = 0;
        let max_retries = 2; // 最多重试2次

        loop {
            match tokio::time::timeout(
                std::time::Duration::from_secs(3), // 缩短单次获取超时时间
                self.tts_controller.get_or_create_client(),
            )
            .await
            {
                Ok(Ok(())) => {
                    if retry_count > 0 {
                        info!("✅ TTS客户端获取成功（重试{}次后）: session={}", retry_count, self.session_id);
                    } else {
                        info!("✅ TTS客户端获取成功: session={}", self.session_id);
                    }
                    break;
                },
                Ok(Err(e)) => {
                    if retry_count < max_retries {
                        retry_count += 1;
                        warn!(
                            "⚠️ TTS客户端获取失败，准备重试{}/{}: session={}, error={}",
                            retry_count, max_retries, self.session_id, e
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        continue;
                    } else {
                        error!(
                            "❌ TTS客户端获取失败（已重试{}次）: session={}, error={}",
                            max_retries, self.session_id, e
                        );
                        return Err(e);
                    }
                },
                Err(_) => {
                    if retry_count < max_retries {
                        retry_count += 1;
                        warn!(
                            "⚠️ TTS客户端获取超时(3s)，准备重试{}/{}: session={}",
                            retry_count, max_retries, self.session_id
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        continue;
                    } else {
                        let timeout_error = anyhow::anyhow!("TTS客户端获取超时(3s，已重试{}次)", max_retries);
                        error!("❌ TTS客户端获取超时（已重试{}次）: session={}", max_retries, self.session_id);
                        return Err(timeout_error);
                    }
                },
            }
        }

        // 🚀 预先初始化会话级别的音频发送器
        info!("🔧 TTS任务开始初始化音频发送器: session={}", self.session_id);
        {
            let mut sender_guard = self.tts_controller.session_audio_sender.lock().await;
            info!("🔧 TTS任务获取到音频发送器锁: session={}", self.session_id);
            if let Err(e) = sender_guard
                .initialize(
                    self.session_id.clone(),
                    self.router.clone(),
                    self.simple_interrupt_handler.clone().unwrap_or_else(|| {
                        SimpleInterruptHandler::new(
                            self.session_id.clone(),
                            "TTS-Default".to_string(),
                            self.simple_interrupt_manager.subscribe(),
                        )
                    }),
                    self.initial_burst_count,
                    self.initial_burst_delay_ms,
                    self.send_rate_multiplier,
                    None, // 使用最新的待应用配置（pending_output配置）
                    Some(self.emitter.signal_only_flag()),
                    Some(self.next_sentence_trigger_tx.clone()), // 🆕 按需 TTS 生成
                    self.is_translation_mode,                    // 🆕 同声传译模式
                )
                .await
            {
                error!("❌ 预初始化会话级别音频发送器失败: {}", e);
                // 防泄漏：初始化失败时立即归还已获取的TTS客户端
                self.tts_controller.return_client_with_mode(true).await;
                return Err(e);
            }
        }
        info!("✅ TTS音频发送器初始化成功: session={}", self.session_id);

        // 🚀 预先启动音频处理任务（会话级长驻，避免后续重复启动引入竞态）
        info!("🔧 TTS任务开始订阅音频流: session={}", self.session_id);
        match self.tts_controller.subscribe_audio().await {
            Ok(audio_rx) => {
                info!("🎵 预启动会话级别音频处理任务（广播模式）: {}", self.session_id);
                self.spawn_session_audio_handler(audio_rx).await;
                info!("✅ 会话级别音频处理任务已预启动（广播模式）: session={}", self.session_id);
            },
            Err(e) => {
                error!("❌ 无法订阅TTS音频流，音频处理任务启动失败: {}", e);
                // 防泄漏：订阅失败时立即归还已获取的TTS客户端
                self.tts_controller.return_client_with_mode(true).await;
                return Err(anyhow!("TTS音频订阅不可用: {}", e));
            },
        }

        // 🔧 关键修复：创建独立的打断信号接收器，用于立即响应打断
        info!("🔧 TTS任务开始创建打断信号接收器: session={}", self.session_id);
        let mut interrupt_receiver = self.simple_interrupt_handler.as_ref().map(|handler| handler.subscribe());
        info!("✅ TTS打断信号接收器创建完成: session={}", self.session_id);

        // 🔧 移除延迟初始化标志，因为已经预初始化完成
        let _first_send_recorded = Arc::new(AtomicBool::new(false));

        // 主循环变量
        let mut current_turn_id: Option<String> = None;
        let mut current_turn_sequence: Option<u64> = None; // 🔧 新增：跟踪当前轮次序列号

        info!("✅ TTS任务所有初始化步骤完成，开始监听LLM消息: session={}", self.session_id);
        loop {
            // 🚀 关键重构：使用biased select!确保打断信号优先级最高
            //
            // ⚡ 架构优势：
            // - biased select!按分支声明顺序优先检查，确保打断信号最先处理
            // - 即使synthesize_text等阻塞操作正在进行，下次循环打断信号仍有最高优先级
            // - 消除了之前933ms打断延迟的问题（由synthesize_text阻塞导致）
            tokio::select! {
                biased;

                // 🚀 最高优先级：立即处理打断信号，确保不被阻塞操作延迟
                interrupt_signal = async {
                    if let Some(ref mut rx) = interrupt_receiver {
                        rx.recv().await
                    } else {
                        // 如果没有打断接收器，永远等待（实际上不会被选中）
                        std::future::pending().await
                    }
                } => {
                    match interrupt_signal {
                        Ok(interrupt_event) => {
                            info!(
                                "🛑 TTS收到立即打断信号: reason={:?}, event_turn={}, current_bound_turn={:?}, session={}",
                                interrupt_event.reason,
                                interrupt_event.turn_sequence,
                                current_turn_sequence,
                                self.session_id
                            );

                            // 🔎 在处理前进行相关性过滤，避免新轮次开始时的自打断
                            let should_handle_interrupt = {
                                use crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::InterruptReason as SimpleInterruptReason;

                                // 只处理本会话的事件
                                let is_same_session = interrupt_event.session_id == self.session_id;
                                if !is_same_session {
                                    false
                                } else {
                                    // 🔧 验证当前轮次的response_id是否匹配（时序安全）
                                    // 使用 load 进行无锁只读访问，不消耗数据
                                    let current_response_id = self.current_turn_response_id.load();

                                    if let Some(ref current_id) = current_response_id
                                        && let Some(ref current_turn_id) = current_turn_id
                                            && current_id != current_turn_id {
                                                debug!("🔄 TTS任务忽略不匹配的打断信号: current_response_id={}, current_turn_id={}",
                                                       current_id, current_turn_id);
                                                continue; // 忽略不匹配的打断信号
                                            }
                                    match interrupt_event.reason {
                                        // 全局类事件总是处理
                                        SimpleInterruptReason::SessionTimeout
                                        | SimpleInterruptReason::SystemShutdown
                                        | SimpleInterruptReason::ConnectionLost => true,
                                        // 用户真实说话：需要立即停止播放
                                        // 🆕 同声传译模式：忽略 UserSpeaking 打断，保持队列式处理
                                        SimpleInterruptReason::UserSpeaking => {
                                            if self.is_translation_mode {
                                                // 同声传译模式：不清空缓冲区，让之前的翻译继续播放
                                                info!("🌍 同声传译模式：忽略 UserSpeaking 打断，继续队列式处理");
                                                false
                                            } else if current_turn_sequence.is_none() {
                                                // 若未绑定轮次，避免处理过期的旧轮次事件
                                                let current_global_turn = self.simple_interrupt_manager.current_turn();
                                                interrupt_event.turn_sequence >= current_global_turn
                                            } else {
                                                true
                                            }
                                        },
                                        // PTT/内部清理：仅当其指向更"新的"轮次时才处理
                                        // 未绑定轮次时忽略PTT，避免新轮次自打断
                                        SimpleInterruptReason::UserPtt => {
                                            if let Some(bound_turn) = current_turn_sequence {
                                                // 🔧 统一修复：PTT只处理新轮次（>），避免同轮次自打断
                                                // 这与LLM任务和SimpleInterruptHandler的逻辑保持一致
                                                interrupt_event.turn_sequence > bound_turn
                                            } else {
                                                false
                                            }
                                        }
                                    }
                                }
                            };

                            if !should_handle_interrupt {
                                debug!(
                                    "🔄 忽略非相关打断: reason={:?}, event_turn={}, current_bound_turn={:?}",
                                    interrupt_event.reason,
                                    interrupt_event.turn_sequence,
                                    current_turn_sequence
                                );
                                continue;
                            }

                            // 🔧 关键修复：对UserSpeaking的特殊处理
                            if matches!(interrupt_event.reason,
                                crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::InterruptReason::UserSpeaking) {

                                // 🚨 重要：真正的用户说话应该立即打断一切
                                // 只有在用户说话轮次大于当前轮次时，才表示这是新的用户输入
                                if let Some(current_seq) = current_turn_sequence
                                    && interrupt_event.turn_sequence <= current_seq {
                                        info!("🔄 检测到同轮次UserSpeaking，可能是内部清理信号: event_turn={}, current_turn={}",
                                              interrupt_event.turn_sequence, current_seq);

                                        // 但我们仍然执行清理操作，确保音频缓冲区被清理
                                        info!("🛑 执行音频缓冲区清理（即使是同轮次信号）");

                                        // 🆕 Phase 3: 清空句子队列和text_splitter
                                        {
                                            let mut queue = self.sentence_queue.lock().await;
                                            queue.clear();
                                        }
                                        {
                                            let mut splitter = self.text_splitter.lock().await;
                                            splitter.reset();
                                        }
                                        // 断开向 PacedSender 发送音频的通路
                                        self.audio_sending_stopped.store(true, Ordering::Release);
                                        info!("🧹 已清空句子队列和文本分句器，断开音频发送通路");
                                        // 🆕 中止预取任务并清空缓冲
                                        {
                                            let mut h = self.inflight_task_handle.lock().await;
                                            if let Some(handle) = h.take() { handle.abort(); }
                                        }
                                        {
                                            let mut rxg = self.inflight_audio_rx.lock().await;
                                            *rxg = None;
                                        }

                                        {
                                            let sender_guard = self.tts_controller.session_audio_sender.lock().await;
                                            if let Err(e) = sender_guard.force_clear_buffer().await {
                                                warn!("清空音频缓冲区失败: {}", e);
                                            }
                                        }

                                        // 🆕 打断后立即向 PacedSender 注入 turn-final（幂等），确保客户端收到 stopped/done
                                        {
                                            // 使用 turn_final_injected 保障只注入一次，避免依赖易错标志位
                                            if !self.turn_final_injected.swap(true, Ordering::AcqRel) {
                                                let response_id_for_final = self
                                                    .current_turn_response_id
                                                    .load()
                                                    .unwrap_or_else(|| format!("resp_{}", nanoid::nanoid!(8)));
                                                let final_chunk = PacedAudioChunk {
                                                    audio_data: Bytes::new(),
                                                    is_final: true,
                                                    realtime_metadata: Some(RealtimeAudioMetadata {
                                                        response_id: response_id_for_final,
                                                        assistant_item_id: self
                                                            .shared_flags
                                                            .assistant_response_context
                                                            .get_context_copy()
                                                            .map(|c| c.assistant_item_id)
                                                            .unwrap_or_else(|| format!("asst_{}", nanoid::nanoid!(6))),
                                                        output_index: 0,
                                                        content_index: 0,
                                                    }),
                                                    sentence_text: None,
                                                    turn_final: true,
                                                };
                                                let sender_guard = self.tts_controller.session_audio_sender.lock().await;
                                                if let Err(e) = sender_guard.send_audio(final_chunk).await {
                                                    warn!("⚠️ 打断后注入turn-final失败: {}", e);
                                                } else {
                                                    info!("🏁 打断后已注入turn-final，确保下游停止事件");
                                                }
                                            } else {
                                                info!("🛈 已注入过turn-final，跳过重复注入");
                                            }
                                        }

                                        // 🔧 重要修复：对于UserSpeaking，无论轮次如何，都应该立即中断TTS播放
                                        info!("🛑 UserSpeaking信号：立即中断TTS会话以停止音频播放");
                                        if let Err(e) = self.tts_controller.interrupt_session().await {
                                            warn!("中断TTS会话失败: {}", e);
                                        }

                                        // 🔧 先中断后台等待SessionFinished的任务（如果存在）
                                        info!("🛑 UserSpeaking打断：中断后台等待SessionFinished任务");
                                        self.tts_controller.abort_finish_wait().await;
                                        // 清理cleanup_rx，防止收到过期的清理通知
                                        {
                                            let mut guard = self.tts_controller.finish_session_cleanup_rx.lock().await;
                                            *guard = None;
                                        }

                                        // 🆕 Phase 3: 打断后不立即 reset_client，让旧任务在后台完成
                                        // audio_sending_stopped = true 已设置，音频不会发送到 PacedSender
                                        // 下一个轮次开始时会自然获取新的客户端
                                        info!("✅ 打断处理完成，旧TTS任务继续在后台运行（不发送音频）");
                                        // 不修改处理器状态布尔位；后续通过 JoinHandle 状态判断是否需要重挂

                                        info!("✅ UserSpeaking处理完成，TTS播放已停止（客户端未重置，后台等待完成）");

                                        // 🔧 pub-sub架构：清除本地打断状态，不影响其他subscriber
                                        if let Some(ref mut handler) = self.simple_interrupt_handler {
                                            handler.clear_interrupt_state();
                                        }
                                        continue;
                                    }

                                // 新轮次的用户说话：清空音频缓冲区，不关闭TTS会话
                                info!("🔄 新轮次UserSpeaking: 清空音频缓冲区，中断当前播放");

                                // 立即中断TTS播放
                                if let Err(e) = self.tts_controller.interrupt_session().await {
                                    warn!("中断TTS会话失败: {}", e);
                                }

                                // 🔧 先中断后台等待SessionFinished的任务（如果存在）
                                info!("🛑 新轮次UserSpeaking打断：中断后台等待SessionFinished任务");
                                self.tts_controller.abort_finish_wait().await;
                                // 清理cleanup_rx，防止收到过期的清理通知
                                {
                                    let mut guard = self.tts_controller.finish_session_cleanup_rx.lock().await;
                                    *guard = None;
                                }

                                // 🆕 Phase 3: 清空句子队列和text_splitter
                                {
                                    let mut queue = self.sentence_queue.lock().await;
                                    queue.clear();
                                }
                                {
                                    let mut splitter = self.text_splitter.lock().await;
                                    splitter.reset();
                                }
                                // 断开向 PacedSender 发送音频的通路
                                self.audio_sending_stopped.store(true, Ordering::Release);
                                info!("🧹 已清空句子队列和文本分句器，断开音频发送通路");
                                // 🆕 中止预取任务并清空缓冲
                                {
                                    let mut h = self.inflight_task_handle.lock().await;
                                    if let Some(handle) = h.take() { handle.abort(); }
                                }
                                {
                                    let mut rxg = self.inflight_audio_rx.lock().await;
                                    *rxg = None;
                                }

                                // 🆕 Phase 3: 打断后不立即 reset_client，让旧任务在后台完成
                                // 下一个轮次开始时会通过 prepare_new_turn_session() 获取新客户端
                                info!("✅ 新轮次打断处理完成，旧TTS任务继续在后台运行（不发送音频）");
                                // 不修改处理器状态布尔位；后续通过 JoinHandle 状态判断是否需要重挂

                                // 清空音频缓冲区
                                {
                                    let sender_guard = self.tts_controller.session_audio_sender.lock().await;
                                    if let Err(e) = sender_guard.force_clear_buffer().await {
                                        warn!("清空音频缓冲区失败: {}", e);
                                    }
                                }
                                // 🆕 打断后：幂等注入 turn-final（不依赖 audio_output_started）
                                {
                                    if !self.turn_final_injected.swap(true, Ordering::AcqRel) {
                                        let response_id_for_final = self
                                            .current_turn_response_id
                                            .load()
                                            .unwrap_or_else(|| format!("resp_{}", nanoid::nanoid!(8)));
                                        let final_chunk = PacedAudioChunk {
                                            audio_data: Bytes::new(),
                                            is_final: true,
                                            realtime_metadata: Some(RealtimeAudioMetadata {
                                                response_id: response_id_for_final,
                                                assistant_item_id: self
                                                    .shared_flags
                                                    .assistant_response_context
                                                    .get_context_copy()
                                                    .map(|c| c.assistant_item_id)
                                                    .unwrap_or_else(|| format!("asst_{}", nanoid::nanoid!(6))),
                                                output_index: 0,
                                                content_index: 0,
                                            }),
                                            sentence_text: None,
                                            turn_final: true,
                                        };
                                        let sender_guard = self.tts_controller.session_audio_sender.lock().await;
                                        if let Err(e) = sender_guard.send_audio(final_chunk).await {
                                            warn!("⚠️ 打断后注入turn-final失败: {}", e);
                                        } else {
                                            info!("🏁 打断后已注入turn-final，确保下游停止事件");
                                        }
                                    } else {
                                        info!("🛈 已注入过turn-final，跳过重复注入");
                                    }
                                }
                                info!("✅ 新轮次UserSpeaking处理完成，TTS播放已停止（客户端未重置，后台等待完成）");

                                // 🔧 pub-sub架构：清除本地打断状态，不影响其他subscriber
                                if let Some(ref mut handler) = self.simple_interrupt_handler {
                                    handler.clear_interrupt_state();
                                }
                                continue;
                            }

                            // 🔧 关键修复：立即处理其他类型的打断
                            info!("⚡ TTS立即执行打断操作: session={}", self.session_id);

                            // 🆕 Phase 3: 清空句子队列和text_splitter
                            {
                                let mut queue = self.sentence_queue.lock().await;
                                queue.clear();
                            }
                            {
                                let mut splitter = self.text_splitter.lock().await;
                                splitter.reset();
                            }
                            // 断开向 PacedSender 发送音频的通路
                            self.audio_sending_stopped.store(true, Ordering::Release);
                            info!("🧹 已清空句子队列和文本分句器，断开音频发送通路");
                            // 🆕 中止预取任务并清空缓冲
                            {
                                let mut h = self.inflight_task_handle.lock().await;
                                if let Some(handle) = h.take() { handle.abort(); }
                            }
                            {
                                let mut rxg = self.inflight_audio_rx.lock().await;
                                *rxg = None;
                            }

                            // 立即中断TTS会话
                            if let Err(e) = self.tts_controller.interrupt_session().await {
                                warn!("管线级别 TTS 打断失败: {}", e);
                            }

                            // 🚀 关键修复：只有SystemShutdown才是致命的，ConnectionLost应该可以恢复
                            let is_fatal_shutdown = matches!(interrupt_event.reason,
                                crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::InterruptReason::SystemShutdown);

                            // 🔧 ConnectionLost需要特殊处理：重置客户端但不退出任务
                            let is_connection_lost = matches!(interrupt_event.reason,
                                crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::InterruptReason::ConnectionLost);

                            // 🔧 关键修复：对于非致命打断，中断后台任务但不reset客户端
                            if !is_fatal_shutdown {
                                // 🔧 先中断后台等待SessionFinished的任务（如果存在）
                                info!("🛑 打断：中断后台等待SessionFinished任务");
                                self.tts_controller.abort_finish_wait().await;
                                // 清理cleanup_rx，防止收到过期的清理通知
                                {
                                    let mut guard = self.tts_controller.finish_session_cleanup_rx.lock().await;
                                    *guard = None;
                                }

                                // 🆕 Phase 3: 打断后不立即 reset_client，让旧任务在后台完成
                                info!("✅ 打断处理完成，旧TTS任务继续在后台运行（不发送音频）");
                                // 不修改处理器状态布尔位；后续通过 JoinHandle 状态判断是否需要重挂
                            }

                            // 清空音频缓冲区
                            {
                                let mut sender_guard = self.tts_controller.session_audio_sender.lock().await;
                                if is_fatal_shutdown {
                                    info!("🛑 强制清空SessionAudioSender缓冲区");
                                    sender_guard.force_cleanup().await
                                } else if is_connection_lost {
                                    info!("🔄 ConnectionLost: 彻底清理SessionAudioSender，下次使用时重建");
                                    sender_guard.force_cleanup().await;
                                    // 🚨 关键修复：ConnectionLost时彻底删除SessionAudioSender对象
                                    // 这样下次使用时会重新创建，而不是复用损坏的对象
                                    drop(sender_guard); // 先释放锁
                                    *self.tts_controller.session_audio_sender.lock().await = SessionAudioSender::new();
                                } else if let Err(e) = sender_guard.force_clear_buffer().await {
                                    warn!("强制清空音频缓冲区失败: {}", e);
                                }
                            }

                            // 🆕 对于非致命且非连接丢失的打断，注入 turn-final，确保客户端收到停止事件
                            if !is_fatal_shutdown && !is_connection_lost {
                                if let Some(response_id_for_final) = self.current_turn_response_id.load() {
                                    let final_chunk = PacedAudioChunk {
                                        audio_data: Bytes::new(),
                                        is_final: true,
                                        realtime_metadata: Some(RealtimeAudioMetadata {
                                            response_id: response_id_for_final,
                                            assistant_item_id: self
                                                .shared_flags
                                                .assistant_response_context
                                                .get_context_copy()
                                                .map(|c| c.assistant_item_id)
                                                .unwrap_or_else(|| format!("asst_{}", nanoid::nanoid!(6))),
                                            output_index: 0,
                                            content_index: 0,
                                        }),
                                        sentence_text: None,
                                        turn_final: true,
                                    };
                                    let sender_guard = self.tts_controller.session_audio_sender.lock().await;
                                    if let Err(e) = sender_guard.send_audio(final_chunk).await {
                                        warn!("⚠️ 打断后注入turn-final失败: {}", e);
                                    } else {
                                        info!("🏁 打断后已注入turn-final，确保下游停止事件");
                                    }
                                } else {
                                    info!("🛈 打断后未注入turn-final（无活跃response_id）");
                                }
                            }

                                                        if is_fatal_shutdown {
                                // 🔧 系统关闭时重置轮次绑定（清理操作）
                                current_turn_id.take();
                                // 清理轮次绑定状态，为系统关闭做准备
                                current_turn_sequence = None;
                                info!("🔄 系统关闭，已重置轮次绑定: current_turn_sequence={:?}", current_turn_sequence);
                                info!("✅ TTS立即打断处理完成，系统关闭中");
                                break; // 仅在 SystemShutdown 时才退出主循环
                            } else if is_connection_lost {
                                // 🔧 连接丢失时：清理并重置本地绑定（无全局池）
                                info!("🔄 ConnectionLost: 清理并重置本地绑定");

                                // 🔧 先中断后台等待SessionFinished的任务（如果存在）
                                info!("🛑 ConnectionLost：中断后台等待SessionFinished任务");
                                self.tts_controller.abort_finish_wait().await;
                                // 清理cleanup_rx，防止收到过期的清理通知
                                {
                                    let mut guard = self.tts_controller.finish_session_cleanup_rx.lock().await;
                                    *guard = None;
                                }
                                self.tts_controller.return_client_with_mode(true).await; // 打断清理模式
                                // 🔧 重置轮次绑定，为重连做准备
                                current_turn_id.take();
                                current_turn_sequence = None; // 重连时需要重新绑定
                                info!("🔄 连接丢失，已重置轮次绑定，等待重连");
                                info!("🔄 ConnectionLost处理完成，TTS任务继续运行等待重连");
                                // ConnectionLost不退出任务，继续等待下一轮
                            } else {
                                info!("✅ TTS立即打断处理完成，继续等待新的输入");
                                // 🔧 关键修复：对于用户打断（UserSpeaking、UserPtt等），不退出任务，而是继续等待下一轮
                                // 🔧 用户打断不重置轮次绑定，保持当前轮次信息用于后续处理

                                // 🔧 pub-sub架构：清除本地打断状态，不影响其他subscriber
                                if let Some(ref mut handler) = self.simple_interrupt_handler {
                                    handler.clear_interrupt_state();
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("⚠️ TTS打断信号接收滞后 {} 条消息", n);
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("📡 TTS打断信号通道已关闭");
                            break;
                        }
                    }
                }

                // 🆕 处理SessionFinished后的清理通知
                cleanup_result = async {
                    let mut guard = self.tts_controller.finish_session_cleanup_rx.lock().await;
                    if let Some(rx) = guard.take() {
                        rx.await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    match cleanup_result {
                        Ok(()) => {
                            info!("🧹 收到SessionFinished清理通知，执行清理操作: session={}", self.session_id);

                            // 1. 发送final空块触发PacedSender停止
                            if self.is_audio_handler_running().await
                                && let Some(resp_id) = self.current_turn_response_id.load() {
                                    let final_chunk = PacedAudioChunk {
                                        audio_data: Bytes::new(),
                                        is_final: true,
                                        realtime_metadata: Some(RealtimeAudioMetadata {
                                            response_id: resp_id,
                                            assistant_item_id: self
                                                .shared_flags
                                                .assistant_response_context
                                                .get_context_copy()
                                                .map(|c| c.assistant_item_id)
                                                .unwrap_or_else(|| format!("asst_{}", nanoid::nanoid!(6))),
                                            output_index: 0,
                                            content_index: 0,
                                        }),
                                        sentence_text: None,
                                        turn_final: true,
                                    };
                                    let sender_guard = self.tts_controller.session_audio_sender.lock().await;
                                    if sender_guard.try_send_audio(&final_chunk).await.is_err() {
                                        let _ = tokio::time::timeout(
                                            std::time::Duration::from_millis(10),
                                            sender_guard.send_audio(final_chunk)
                                        ).await;
                                    }
                                }

                            // 2. 归还TTS客户端到池
                            info!("🔙 SessionFinished后归还TTS客户端（正常清理）: session={}", self.session_id);
                            self.tts_controller.return_client_with_mode(false).await;
                            info!("✅ 已归还TTS客户端: session={}", self.session_id);

                            // 3. 解绑打断管理器
                            current_turn_id.take();
                            if let Some(ref mut handler) = self.simple_interrupt_handler {
                                handler.unbind_turn();
                                info!("🔓 SessionFinished后解绑TTS打断处理器轮次绑定");
                            }
                            current_turn_sequence = None;
                            info!("🔓 SessionFinished后解绑轮次绑定: 准备接收新轮次");
                            // 音频处理任务状态由 JoinHandle 决定，这里无需设置布尔位

                            // 4. 重置is_responding标志
                            let _ = self.shared_flags.is_responding_tx.send(false);
                            info!("🔄 SessionFinished后重置 is_responding 标志: session={}", self.session_id);
                        }
                        Err(_) => {
                            warn!("⚠️ 清理通知channel已关闭（可能是sender被drop）");
                        }
                    }
                }

                // 处理来自LLM的文本输入（broadcast receiver 返回 Result）
                tts_message = self.rx.recv() => {
                    match tts_message {
                        Ok((ctx, text)) => {
                            // info!("📨 TTS任务收到LLM消息: session={}, response_id={}, text='{}'",
                            //      self.session_id, ctx.response_id, text.chars().take(50).collect::<String>());
                            // 先基于 response_id 判断是否为新轮次（不再依赖非空文本）
                            let is_new_turn = current_turn_id.as_ref() != Some(&ctx.response_id);

                            if is_new_turn {
                                info!("🆕 [Early] 检测到新轮次: session={}, turn={}, is_translation_mode={}", self.session_id, ctx.response_id, self.is_translation_mode);

                                // 🆕 同声传译模式：不清空旧缓冲区，让多个翻译任务按队列顺序完成
                                if !self.is_translation_mode {
                                    // 提前完成新轮次初始化：清空旧缓冲、绑定轮次、更新sender绑定、设置当前轮次
                                    if let Some(prev_turn_seq) = current_turn_sequence {
                                        info!("🧹 [Early] 新轮次开始，清空旧轮次({})音频缓冲区（本地清理）", prev_turn_seq);
                                        let sender_guard = self.tts_controller.session_audio_sender.lock().await;
                                        if let Err(e) = sender_guard.force_clear_buffer().await {
                                            warn!("清空旧轮次音频缓冲区失败: {}", e);
                                        }
                                    }
                                } else {
                                    info!("🌍 [Early] 同声传译模式：保留旧缓冲区，队列式处理多个翻译任务");
                                    // 🔧 关键修复：重置 LLM 完成状态，避免新任务被误判为"最后一句"
                                    llm_turn_complete.store(false, Ordering::Release);
                                    {
                                        let mut queue = self.sentence_queue.lock().await;
                                        queue.reset_llm_complete();
                                    }
                                }

                                if let Some(ref mut simple_handler) = self.simple_interrupt_handler {
                                    let turn_to_bind = ctx.turn_sequence.unwrap_or_else(|| {
                                        warn!("⚠️ [Early] TurnContext 没有 turn_sequence，使用 current_turn() 作为兜底");
                                        self.simple_interrupt_manager.current_turn()
                                    });
                                    simple_handler.bind_to_turn(turn_to_bind);
                                    info!("🔗 [Early][TTS-Main-Simple] 绑定到轮次: {} (from TurnContext)", turn_to_bind);

                                    let mut sender_guard = self.tts_controller.session_audio_sender.lock().await;
                                    if let Err(e) = sender_guard.update_turn_binding(turn_to_bind).await {
                                        warn!("⚠️ [Early] 更新SessionAudioSender轮次绑定失败: {}", e);
                                    }
                                }

                                // 🆕 同声传译模式：不更新 sender 的 response_id，保持原有的音频流上下文
                                if !self.is_translation_mode {
                                    let mut sender_guard = self.tts_controller.session_audio_sender.lock().await;
                                    // 🔧 使用 Pipeline 级别的 current_turn_response_id 确保一致性
                                    // 使用 load 进行无锁只读访问，不消耗数据
                                    let current_response_id = self.current_turn_response_id.load();
                                    let assistant_id_for_turn = if !ctx.assistant_item_id.is_empty() {
                                        Some(ctx.assistant_item_id.clone())
                                    } else {
                                        None
                                    };
                                    if let Some(response_id) = current_response_id {
                                        let _ = sender_guard.start_new_turn(response_id, assistant_id_for_turn).await;
                                    } else {
                                        // 如果 Pipeline 级别没有 response_id，使用 ctx.response_id 作为兜底
                                        let _ = sender_guard.start_new_turn(ctx.response_id.clone(), assistant_id_for_turn).await;
                                    }
                                }

                                current_turn_id = Some(ctx.response_id.clone());
                                current_turn_sequence = ctx.turn_sequence;
                                self.tts_session_created.store(true, Ordering::Release);
                                self.text_splitter_first_chunk_recorded.store(false, Ordering::Release);

                                // 🆕 同声传译模式：不重置 splitter 和队列，保留正在处理的翻译任务
                                if !self.is_translation_mode {
                                    {
                                        // 重置文本分割器以开始新的轮次
                                        let mut splitter = self.text_splitter.lock().await;
                                        splitter.reset();
                                    }
                                    {
                                        // 🔧 关键修复：重置句子队列，确保新轮次从 idx=0 开始
                                        let mut queue = self.sentence_queue.lock().await;
                                        queue.reset();
                                    }
                                    // 🔧 关键修复：重置 llm_turn_complete 标志，避免新轮次被误判为"LLM已完成"
                                    // 此标志与 queue.is_llm_complete() 独立，必须同步重置
                                    llm_turn_complete.store(false, Ordering::Release);
                                    // 🌐 新轮次开始，重置轮次语言、音色和引擎缓存
                                    // 必须在 [Early] 阶段重置，因为工具调用可能先于文本到达
                                    {
                                        let mut lang_guard = self.tts_controller.turn_detected_language.lock().await;
                                        let mut voice_guard = self.tts_controller.turn_detected_voice_id.lock().await;
                                        let mut engine_guard = self.tts_controller.turn_confirmed_engine.lock().await;
                                        if lang_guard.is_some() || voice_guard.is_some() || engine_guard.is_some() {
                                            info!("🌐 [Early] 重置轮次缓存: prev_lang={:?}, prev_voice={:?}, prev_engine={:?}",
                                                lang_guard, voice_guard, engine_guard);
                                        }
                                        *lang_guard = None;
                                        *voice_guard = None;
                                        *engine_guard = None;
                                    }
                                }
                                // 🔓 新轮次开始，允许向 PacedSender 发送音频/文字控制块
                                self.audio_sending_stopped.store(false, Ordering::Release);
                                // 重置整轮注入幂等标志
                                self.turn_final_injected.store(false, Ordering::Release);
                                info!("🔓 新轮次允许音频发送: session={}", self.session_id);
                                info!("✅ [Early] 新轮次初始化完成: session={}, turn={}, seq={:?}, translation_mode={}", self.session_id, ctx.response_id, current_turn_sequence, self.is_translation_mode);
                            }

                            if text != "__TURN_COMPLETE__" && !text.is_empty() {
                                // ✂️ 关键重构：使用 text_splitter 将流式文本片段分割成完整句子
                                // MiniMax 不支持流式文本输入，必须发送完整句子
                                // info!("✂️ 收到LLM文本片段，送入splitter: '{}'", text.chars().take(30).collect::<String>());

                                let sentences = {
                                    let mut splitter = self.text_splitter.lock().await;
                                    splitter.found_first_sentence(&text)
                                };

                                if sentences.is_empty() {
                                    // info!("✂️ Splitter缓冲文本片段，等待完整句子");
                                    continue; // 继续接收更多文本，直到形成完整句子
                                }

                                info!("✂️ Splitter产出 {} 个完整句子，加入队列（按需生成模式）", sentences.len());

                                // 🆕 按需生成模式：将句子加入队列，而非立即发送
                                {
                                    let mut queue = self.sentence_queue.lock().await;
                                    for sentence in sentences {
                                        // 🌐 在分句完成时进行一次语言检测，缓存结果避免重复检测
                                        let mut confidences = lingua_language_confidences(&sentence.text);
                                        // 按置信度降序排列
                                        confidences.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

                                        // 将外部 splitter 的 TextChunk 映射为本地 TextChunk
                                        let mapped = TextChunk {
                                            text: sentence.text.clone(),
                                            can_synthesize_immediately: sentence.can_synthesize_immediately,
                                            requires_context: sentence.requires_context,
                                            language_confidences: confidences,
                                        };
                                        queue.push(mapped);
                                    }
                                }

                                // 🆕 若首句已启动且当前无inflight，立刻为下一句启动预取（缓冲式）
                                if self.text_splitter_first_chunk_recorded.load(Ordering::Acquire) {
                                    // 尝试开启预取（仅在没有inflight且队列有待处理时）
                                    let maybe_prefetch = {
                                        let mut queue = self.sentence_queue.lock().await;
                                        if !queue.has_inflight() && queue.has_pending() {
                                            queue.start_prefetch()
                                        } else {
                                            None
                                        }
                                    };
                                    if let Some(prefetch_sentence) = maybe_prefetch {
                                        let prefetch_text = sanitize_visible_text(&prefetch_sentence.text);
                                        if !prefetch_text.is_empty() {
                                            let current_voice_id = self.tts_controller.current_voice_id().await;
                                            // 获取轮次内已确认的引擎（用于继承）
                                            let inherited_engine = {
                                                self.tts_controller.turn_confirmed_engine.lock().await.clone()
                                            };
                                            // 同声传译模式：强制使用 Edge TTS
                                            let force_engine = if self.is_translation_mode { Some(TtsEngineKind::EdgeTts) } else { None };
                                            let selection = select_tts_engine(
                                                &prefetch_text,
                                                current_voice_id.as_deref(),
                                                &prefetch_sentence.language_confidences,
                                                inherited_engine,
                                                force_engine,
                                            );
                                            let engine = selection.engine;
                                            // 如果是确定性路由且尚未锁定，更新轮次缓存
                                            if selection.is_confident {
                                                let mut engine_guard = self.tts_controller.turn_confirmed_engine.lock().await;
                                                if engine_guard.is_none() {
                                                    info!("🔒 预取路径确定路由，锁定轮次引擎: {:?}", engine);
                                                    *engine_guard = Some(engine);
                                                }
                                            }
                                            // 关闭旧预取任务
                                            {
                                                let mut handle_guard = self.inflight_task_handle.lock().await;
                                                if let Some(h) = handle_guard.take() { h.abort(); }
                                            }
                                            // 创建预取缓冲通道
                                            let (tx, rx) = mpsc::channel::<AudioChunk>(512);
                                            {
                                                let mut rx_guard = self.inflight_audio_rx.lock().await;
                                                *rx_guard = Some(rx);
                                            }
                                            // 增益已嵌入 AudioChunk，无需设置共享状态
                                            // 启动独立HTTP流
                                            let tts_ctrl_for_prefetch = self.tts_controller.clone();
                                            let handle = tokio::spawn(async move {
                                                match engine {
                                                    TtsEngineKind::MiniMax => {
                                                        let cfg = tts_ctrl_for_prefetch.tts_config.clone().unwrap_or_default();
                                                        let client = MiniMaxHttpTtsClient::new(cfg.clone());
                                                        let voice_setting = { tts_ctrl_for_prefetch.voice_setting.lock().await.clone() };
                                                        // 优先使用轮次缓存，缓存为空时进行语言检测并更新
                                                        let (virtual_voice_id, lang) = {
                                                            let mut turn_voice_guard = tts_ctrl_for_prefetch.turn_detected_voice_id.lock().await;
                                                            let mut turn_lang_guard = tts_ctrl_for_prefetch.turn_detected_language.lock().await;

                                                            if turn_voice_guard.is_some() {
                                                                (turn_voice_guard.clone().unwrap(), turn_lang_guard.clone())
                                                            } else {
                                                                // 获取用户配置的音色
                                                                let configured_voice = tts_ctrl_for_prefetch.current_voice_id().await
                                                                    .unwrap_or_else(|| cfg.default_voice_id.clone().unwrap_or_else(|| "zh_female_wanwanxiaohe_moon_bigtts".to_string()));
                                                                let is_default_voice = configured_voice == "zh_female_wanwanxiaohe_moon_bigtts";

                                                                if let Some(detected_lang) = detect_language_boost(&prefetch_text, LINGUA_MIN_CONFIDENCE, LINGUA_MIN_MARGIN) {
                                                                    *turn_lang_guard = Some(detected_lang.clone());
                                                                    // 只有默认音色时才自动切换
                                                                    if is_default_voice {
                                                                        if let Some(voice) = get_voice_for_language(&detected_lang) {
                                                                            *turn_voice_guard = Some(voice.to_string());
                                                                            info!("🎙️ [预取] 检测到语言并设置音色: lang={}, voice={}", detected_lang, voice);
                                                                            (voice.to_string(), Some(detected_lang))
                                                                        } else {
                                                                            *turn_voice_guard = Some(configured_voice.clone());
                                                                            info!("🎙️ [预取] 检测到语言但无映射，锁定fallback: lang={}, voice={}", detected_lang, configured_voice);
                                                                            (configured_voice, Some(detected_lang))
                                                                        }
                                                                    } else {
                                                                        // 用户设置了自定义音色，不自动切换
                                                                        *turn_voice_guard = Some(configured_voice.clone());
                                                                        info!("🎙️ [预取] 用户设置了自定义音色，跳过自动切换: voice={}, lang={}", configured_voice, detected_lang);
                                                                        (configured_voice, Some(detected_lang))
                                                                    }
                                                                } else {
                                                                    (configured_voice, tts_ctrl_for_prefetch.language.lock().await.clone())
                                                                }
                                                            }
                                                        };
                                                        match client.synthesize_text(&virtual_voice_id, &prefetch_text, voice_setting, Some(AudioSetting::default()), None, None, lang, MiniMaxHttpOptions::default()).await {
                                                            Ok(stream) => {
                                                                tokio::pin!(stream);
                                                                while let Some(item) = stream.next().await {
                                                                    match item {
                                                                        Ok(chunk) => { if tx.send(chunk).await.is_err() { break; } }
                                                                        Err(e) => { warn!("⚠️ 预取MiniMax流错误: {}", e); break; }
                                                                    }
                                                                }
                                                            },
                                                            Err(e) => warn!("⚠️ 启动MiniMax预取失败: {}", e),
                                                        }
                                                    },
                                                    TtsEngineKind::VolcEngine => {
                                                        match VolcEngineTtsClient::from_env() {
                                                            Ok(client) => {
                                                                let mut req = VolcEngineRequest::from_text(prefetch_text.clone());
                                                                req.emotion = Some("energetic".to_string());
                                                                match client.stream_sentence(req) {
                                                                    Ok(stream) => {
                                                                        tokio::pin!(stream);
                                                                        while let Some(item) = stream.next().await {
                                                                            match item {
                                                                                Ok(chunk) => { if tx.send(chunk).await.is_err() { break; } }
                                                                                Err(e) => { warn!("⚠️ 预取Volc流错误: {}", e); break; }
                                                                            }
                                                                        }
                                                                    },
                                                                    Err(e) => warn!("⚠️ 启动Volc预取失败: {}", e),
                                                                }
                                                            },
                                                            Err(e) => warn!("⚠️ 构建Volc客户端失败: {}", e),
                                                        }
                                                    },
                                                    TtsEngineKind::Baidu => {
                                                        match crate::tts::baidu::BaiduHttpTtsClient::from_env() {
                                                            Ok(client) => {
                                                                let mut req = crate::tts::baidu::BaiduHttpTtsRequest::new(prefetch_text.clone());
                                                                if let Some(voice_id) = tts_ctrl_for_prefetch.current_voice_id().await {
                                                                    if let Some(per) = crate::tts::baidu::baidu_per_for_voice_id(&voice_id) {
                                                                        req = req.with_per(per);
                                                                    }
                                                                    if let Some(payload) = crate::tts::baidu::baidu_payload_override_for_voice_id(
                                                                        &voice_id,
                                                                        client.config().build_start_payload(),
                                                                    ) {
                                                                        if let Some(spd) = payload.spd {
                                                                            req = req.with_spd(spd);
                                                                        }
                                                                        if let Some(pit) = payload.pit {
                                                                            req = req.with_pit(pit);
                                                                        }
                                                                        if let Some(vol) = payload.vol {
                                                                            req = req.with_vol(vol);
                                                                        }
                                                                    }
                                                                }
                                                                match client.synthesize(req) {
                                                                    Ok(stream) => {
                                                                        tokio::pin!(stream);
                                                                        while let Some(item) = stream.next().await {
                                                                            match item {
                                                                                Ok(chunk) => { if tx.send(chunk).await.is_err() { break; } }
                                                                                Err(e) => { warn!("⚠️ 预取Baidu流错误: {}", e); break; }
                                                                            }
                                                                        }
                                                                    },
                                                                    Err(e) => warn!("⚠️ 启动Baidu预取失败: {}", e),
                                                                }
                                                            },
                                                            Err(e) => warn!("⚠️ 构建Baidu客户端失败: {}", e),
                                                        }
                                                    },
                                                    TtsEngineKind::EdgeTts => {
                                                        let client = crate::tts::edge::EdgeTtsClient::with_defaults();
                                                        let voice = crate::tts::edge::get_voice_for_language("zh")
                                                            .unwrap_or("zh-CN-XiaoxiaoNeural");
                                                        match client.synthesize(&prefetch_text, Some(voice)).await {
                                                            Ok(stream) => {
                                                                tokio::pin!(stream);
                                                                while let Some(item) = stream.next().await {
                                                                    match item {
                                                                        Ok(chunk) => {
                                                                            if tx.send(chunk).await.is_err() {
                                                                                break;
                                                                            }
                                                                        },
                                                                        Err(e) => {
                                                                            warn!("⚠️ 预取Edge TTS流错误: {}", e);
                                                                            break;
                                                                        },
                                                                    }
                                                                }
                                                            },
                                                            Err(e) => warn!("⚠️ 启动Edge TTS预取失败: {}", e),
                                                        }
                                                    },
                                                    TtsEngineKind::AzureTts => {
                                                        match crate::tts::azure::AzureTtsClient::from_env() {
                                                            Ok(client) => {
                                                                let voice = crate::tts::azure::get_voice_for_language("zh")
                                                                    .unwrap_or("zh-CN-XiaoxiaoNeural");
                                                                match client.synthesize(&prefetch_text, Some(voice)).await {
                                                                    Ok(stream) => {
                                                                        tokio::pin!(stream);
                                                                        while let Some(item) = stream.next().await {
                                                                            match item {
                                                                                Ok(chunk) => {
                                                                                    if tx.send(chunk).await.is_err() {
                                                                                        break;
                                                                                    }
                                                                                },
                                                                                Err(e) => {
                                                                                    warn!("⚠️ 预取Azure TTS流错误: {}", e);
                                                                                    break;
                                                                                },
                                                                            }
                                                                        }
                                                                    },
                                                                    Err(e) => warn!("⚠️ 启动Azure TTS预取失败: {}", e),
                                                                }
                                                            },
                                                            Err(e) => warn!("⚠️ 创建Azure TTS客户端失败: {}", e),
                                                        }
                                                    },
                                                }
                                            });
                                            {
                                                let mut handle_guard = self.inflight_task_handle.lock().await;
                                                *handle_guard = Some(handle);
                                            }
                                        }
                                    }
                                }

                                // 🚀 关键：检查是否需要触发第一句的处理
                                let should_process_first = {
                                    let first_not_started = !self.text_splitter_first_chunk_recorded.load(Ordering::Acquire);
                                    if !first_not_started {
                                        false
                                    } else {
                                        let queue = self.sentence_queue.lock().await;
                                        queue.peek_next().is_some()
                                    }
                                };

                                if should_process_first {
                                    let queue_debug = {
                                        let q = self.sentence_queue.lock().await;
                                        (q.len(), q.is_empty())
                                    };
                                    info!(
                                        "🔍 [第一句触发] 队列状态: len={}, is_empty={}, session={}",
                                        queue_debug.0, queue_debug.1, self.session_id
                                    );
                                    info!("🎯 这是第一句，立即触发处理");
                                    // 处理第一句（立即生成）
                                    if let Err(e) = self.process_next_sentence().await {
                                        error!("❌ 处理第一句失败: {}", e);
                                    }
                                    // 🚀 注意：process_next_sentence 内部已经启动了第二句的预取，这里无需重复
                                }

                                continue; // 🔧 修改：不再在这里批量处理，改为按需触发

                                // 🚀 关键修复：强制获取客户端，添加池状态诊断
                                // 避免与异步预热产生锁竞争
                                #[allow(unreachable_code)]
                                let client_ready = {
                                    let pool_client_guard = self.tts_controller.pool_client.lock().await;
                                    pool_client_guard.is_some()
                                };

                                if !client_ready {
                                    // 🔧 如果客户端不可用，尝试获取，增加更长的超时时间
                                    match tokio::time::timeout(
                                        std::time::Duration::from_secs(2),
                                        self.tts_controller.get_or_create_client()
                                    ).await {
                                        Ok(Ok(())) => {
                                            info!("✅ 快速获取TTS客户端成功: session={}", self.session_id);
                                        }
                                        Ok(Err(e)) => {
                                            warn!("⚠️ 快速获取TTS客户端失败: session={}, error={}", self.session_id, e);
                                            // 短延时重试一次
                                            info!("🔁 重试获取TTS客户端(50ms): session={}", self.session_id);
                                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                                            match tokio::time::timeout(
                                                std::time::Duration::from_millis(500),
                                                self.tts_controller.get_or_create_client()
                                            ).await {
                                                Ok(Ok(())) => info!("✅ 重试获取TTS客户端成功: session={}", self.session_id),
                                                _ => {
                                                    warn!("❌ 重试获取TTS客户端仍失败，跳过本批句子: session={}", self.session_id);

                                                    // 🆕 关键修复：TTS客户端获取彻底失败时发送错误事件通知客户端
                                                    warn!("🔚 TTS客户端获取彻底失败，发送错误事件通知客户端: session={}", self.session_id);
                                                    self.emitter.error_event(503, &format!("TTS客户端不可用: {}", e)).await;
                                                    info!("📤 已发送TTS客户端失败错误事件");

                                                    continue;
                                                }
                                            }
                                        }
                                        Err(_) => {
                                            warn!("⏳ 获取TTS客户端超时(2s): session={}", self.session_id);
                                            continue;
                                        }
                                    }
                                }

                                // 🚀 关键修复：新轮次开始时创建新session；若超时/失败则不复用旧会话并跳过本轮TTS
                                if is_new_turn {
                                    info!("🔄 新轮次开始，取消现有session并重建: session={}", self.session_id);
                                    // 🆕 Phase 3: 重置打断状态，允许新轮次发送音频
                                    self.audio_sending_stopped.store(false, Ordering::Release);
                                    info!("🔓 新轮次开始，重置audio_sending_stopped标志");

                                    match self.tts_controller.prepare_new_turn_session().await {
                                        Ok(()) => {
                                            info!("✅ 新轮次session准备成功: session={}", self.session_id);
                                        }
                                        Err(e) => {
                                            warn!("⚠️ 新轮次session准备失败(不可复用旧会话): {}", e);
                                            // 不允许继续使用可能处于不一致状态的旧会话
                                            self.tts_controller.reset_client().await;
                                            // 跳过当前文本，等待下一轮
                                            continue;
                                        }
                                    }
                                }

                                // 🆕 安全检查：如无活跃Session则补发StartSession，避免遗漏startSession
                                {
                                    // 先确保拿到客户端
                                    let has_client = {
                                        let guard = self.tts_controller.pool_client.lock().await;
                                        guard.is_some()
                                    };
                                    if !has_client {
                                        match tokio::time::timeout(
                                            std::time::Duration::from_millis(800),
                                            self.tts_controller.get_or_create_client()
                                        ).await {
                                            Ok(Ok(())) => {
                                                info!("✅ 获取TTS客户端用于补发StartSession: session={}", self.session_id);
                                            },
                                            _ => {
                                                warn!("⚠️ 获取TTS客户端失败，跳过本批句子: session={}", self.session_id);
                                                continue;
                                            }
                                        }
                                    }

                                    // 检查是否已存在活跃Session（SessionStarted且有session_id）
                                    let has_active_session = if let Some(pool_client_arc) = {
                                        let guard = self.tts_controller.pool_client.lock().await;
                                        guard.clone()
                                    } {
                                        let client_guard = pool_client_arc.lock().await;
                                        let connected = client_guard.is_connected();
                                        info!("🔍 [PreSend] 会话状态: connected={} (HTTP)", connected);
                                        connected
                                    } else {
                                        false
                                    };

                                    if !has_active_session {
                                        info!("🚀 [PreSend] 未检测到活跃Session，补发StartSession: session={}", self.session_id);
                                        if let Err(e) = self.tts_controller.prepare_new_turn_session().await {
                                            warn!("⚠️ [PreSend] StartSession失败: {}，跳过本批句子", e);
                                            // 重置以获取干净客户端，等待下一次文本
                                            self.tts_controller.reset_client().await;
                                            continue;
                                        }
                                        info!("✅ [PreSend] StartSession成功");
                                    }
                                }

                                // 🔧 新增：发送前检查音频处理任务状态，确保PacedSender能接收音频
                                if !self.is_audio_handler_running().await {
                                    warn!("⚠️ 检测到音频处理任务未运行，尝试重新启动");
                                    if self.tts_controller.has_audio_subscription().await {
                                        match self.tts_controller.subscribe_audio().await {
                                            Ok(audio_rx) => {
                                                info!("🔄 重新启动音频处理任务（主动检查）: {}", self.session_id);
                                                self.spawn_session_audio_handler(audio_rx).await;
                                                info!("✅ 音频处理任务已重启（主动检查）: session={}", self.session_id);
                                            }
                                            Err(e) => {
                                                warn!("⚠️ 主动检查后订阅音频流失败: {}", e);
                                            }
                                        }
                                    }
                                }

                                // ✂️ 循环处理 splitter 产出的所有完整句子
                                for sentence_chunk in sentences {
                                    // 🌐 在分句时进行语言检测
                                    let mut confidences = lingua_language_confidences(&sentence_chunk.text);
                                    confidences.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

                                    let sentence_text = &sentence_chunk.text;
                                    let mut cleaned = sanitize_visible_text(sentence_text);
                                    if cleaned.is_empty() {
                                        info!("🧹 finalize后首句清洗为空，跳过发送");
                                        continue;
                                    }
                                    let current_voice_id = self.tts_controller.current_voice_id().await;
                                    // 获取轮次内已确认的引擎（用于继承）
                                    let inherited_engine = {
                                        self.tts_controller.turn_confirmed_engine.lock().await.clone()
                                    };
                                    // 同声传译模式：强制使用 Edge TTS
                                    let force_engine = if self.is_translation_mode { Some(TtsEngineKind::EdgeTts) } else { None };
                                    let selection = select_tts_engine(&cleaned, current_voice_id.as_deref(), &confidences, inherited_engine, force_engine);
                                    let engine = selection.engine;
                                    // 如果是确定性路由且尚未锁定，更新轮次缓存
                                    if selection.is_confident {
                                        let mut engine_guard = self.tts_controller.turn_confirmed_engine.lock().await;
                                        if engine_guard.is_none() {
                                            info!("🔒 首次确定路由，锁定轮次引擎: {:?}", engine);
                                            *engine_guard = Some(engine);
                                        }
                                    }
                                    info!(
                                        "🎤 准备发送完整句子到 {:?}: '{}'",
                                        engine,
                                        cleaned.chars().take(50).collect::<String>()
                                    );

                                    // 🚀 发送完整句子到TTS
                                    match self.tts_controller.synthesize_with_engine(engine, &cleaned).await {
                                        Ok(_) => {

                                            // 延后文字事件：在首个音频分片发送时由 PacedSender 发出

                                            // 🆕 记录TextSplitter首分片时间（仅在首次文本发送时）
                                            if !self.text_splitter_first_chunk_recorded.load(Ordering::Acquire) {
                                                let text_splitter_time = std::time::Instant::now();
                                                record_node_time(&self.session_id, TimingNode::TextSplitterFirstChunk, text_splitter_time).await;
                                                self.text_splitter_first_chunk_recorded.store(true, Ordering::Release);
                                            }

                                            // 🔧 关键修复：在新轮次开始或音频处理任务未运行时启动
                                            // 确保PacedSender始终能接收到音频
                                            if !self.is_audio_handler_running().await && self.tts_controller.has_audio_subscription().await {
                                                match self.tts_controller.subscribe_audio().await {
                                                    Ok(audio_rx) => {
                                                        info!("🔄 新轮次开始，启动音频处理任务（广播模式）: {}", self.session_id);
                                                        self.spawn_session_audio_handler(audio_rx).await;
                                                        info!("✅ 新轮次音频处理任务已启动（广播模式）: session={}", self.session_id);
                                                    }
                                                    Err(e) => {
                                                        warn!("⚠️ 新轮次订阅音频流失败: {}", e);
                                                    }
                                                }
                                            } else if is_new_turn && self.is_audio_handler_running().await {
                                                info!("✅ 新轮次开始，但音频处理任务已运行，无需重复启动: {}", self.session_id);
                                            }
                                        },
                                        Err(e) => {
                                            error!(
                                                "❌ 发送句子到 {:?} TTS 失败: {} session={}",
                                                engine, e, self.session_id
                                            );

                                            // 🔧 正确的兜底机制：任何错误都重新获取client，让原client自己重连
                                            self.tts_controller.reset_client().await;
                                            warn!("🔄 已重新获取TTS客户端，原客户端将自动重连: session={}", self.session_id);

                                            // 🔧 重新获取客户端后，启动音频处理任务（错误恢复）
                                            if self.tts_controller.has_audio_subscription().await {
                                                match self.tts_controller.subscribe_audio().await {
                                                    Ok(audio_rx) => {
                                                        info!("🔄 客户端错误恢复，重新启动音频处理任务（广播模式）: {}", self.session_id);
                                                        self.spawn_session_audio_handler(audio_rx).await;
                                                        info!("✅ 音频处理任务已恢复（错误恢复，广播模式）: session={}", self.session_id);
                                                    }
                                                    Err(e) => {
                                                        warn!("⚠️ 错误恢复后订阅音频流失败: {}", e);
                                                    }
                                                }
                                            } else {
                                                warn!("⚠️ 错误恢复后无可用音频订阅，将在下次轮次开始时重试");
                                            }

                                            // 🔁 失败后短延时重试一次发送
                                            info!(
                                                "🔁 准备重试发送句子到 {:?} TTS (50ms): session={}",
                                                engine, self.session_id
                                            );
                                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                                            let mut should_send_completion_event = false; // 标记是否需要发送完成事件

                                            match tokio::time::timeout(
                                                std::time::Duration::from_secs(1),
                                                self.tts_controller.get_or_create_client()
                                            ).await {
                                                Ok(Ok(())) => {
                                                    match self.tts_controller.synthesize_with_engine(engine, &cleaned).await {
                                                        Ok(_) => {
                                                            info!(
                                                                "✅ 重试发送句子到 {:?} TTS 成功: session={}",
                                                                engine, self.session_id
                                                            );
                                                            if !self.is_audio_handler_running().await && self.tts_controller.has_audio_subscription().await {
                                                                match self.tts_controller.subscribe_audio().await {
                                                                    Ok(audio_rx) => {
                                                                        info!("🔄 重试成功后启动音频处理任务: {}", self.session_id);
                                                                        self.spawn_session_audio_handler(audio_rx).await;
                                                                        info!("✅ 音频处理任务已启动: session={}", self.session_id);
                                                                    }
                                                                    Err(e) => warn!("⚠️ 重试成功后订阅音频流失败: {}", e),
                                                                }
                                                            }
                                                        }
                                                        Err(e2) => {
                                                            warn!(
                                                                "❌ 重试发送句子到 {:?} TTS 仍失败: {} session={}",
                                                                engine, e2, self.session_id
                                                            );
                                                            should_send_completion_event = true; // 重试仍失败，标记需要发送完成事件
                                                        }
                                                    }
                                                }
                                                _ => {
                                                    warn!("❌ 重试获取TTS客户端失败，放弃本句: session={}", self.session_id);
                                                    should_send_completion_event = true; // 客户端获取失败，标记需要发送完成事件
                                                }
                                            }

                                            // 🆕 关键修复：TTS彻底失败时发送错误事件通知客户端
                                            if should_send_completion_event {
                                                warn!("🔚 TTS彻底失败，发送错误事件通知客户端: session={}", self.session_id);
                                                self.emitter.error_event(503, &format!("TTS服务不可用: {}", e)).await;
                                                info!("📤 已发送TTS失败错误事件");
                                            }

                                            // 失败后跳过本句，继续处理下一句
                                            continue;
                                        }
                                    }
                                } // end for sentence_chunk

                                continue; // 跳过后续的所有检查，直接处理下一个文本
                            }

                            // 🔧 处理特殊标记（仅对非普通文本）
                            if text == "__TURN_COMPLETE__" {
                                info!("🔚 收到轮次完成标记: session={}, turn={:?}", self.session_id, current_turn_id);

                                // 🔍 诊断：在finalize前检查splitter状态
                                let splitter_buffer_len = {
                                    let splitter = self.text_splitter.lock().await;
                                    splitter.get_buffer_status()
                                };
                                info!(
                                    "🔍 [TURN_COMPLETE] splitter缓冲区状态: buffer_len={}, session={}",
                                    splitter_buffer_len, self.session_id
                                );

                                // ✂️ 关键重构：调用 finalize() 处理缓冲区中的剩余文本
                                info!("✂️ 轮次完成，调用splitter.finalize()处理剩余文本");
                                let remaining_sentences = {
                                    let mut splitter = self.text_splitter.lock().await;
                                    splitter.finalize()
                                };

                                if !remaining_sentences.is_empty() {
                                    info!("✂️ Splitter finalize产出 {} 个剩余句子", remaining_sentences.len());

                                    // 🚀 不再立即发送到TTS：将剩余句子入队，按需由 pacedSender 的触发推进
                                    {
                                        let mut queue = self.sentence_queue.lock().await;
                                        for sentence_chunk in remaining_sentences {
                                            // 🌐 在分句完成时进行一次语言检测，缓存结果避免重复检测
                                            let mut confidences = lingua_language_confidences(&sentence_chunk.text);
                                            // 按置信度降序排列
                                            confidences.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

                                            let mapped = TextChunk {
                                                text: sentence_chunk.text.clone(),
                                                can_synthesize_immediately: sentence_chunk.can_synthesize_immediately,
                                                requires_context: sentence_chunk.requires_context,
                                                language_confidences: confidences,
                                            };
                                            queue.push(mapped);
                                        }
                                    }

                                    // 🆕 若首句已启动且当前无inflight，立刻为下一句启动预取（缓冲式）
                                    if self.text_splitter_first_chunk_recorded.load(Ordering::Acquire) {
                                        let maybe_prefetch = {
                                            let mut queue = self.sentence_queue.lock().await;
                                            if !queue.has_inflight() && queue.has_pending() {
                                                queue.start_prefetch()
                                            } else {
                                                None
                                            }
                                        };
                                        if let Some(prefetch_sentence) = maybe_prefetch {
                                            let prefetch_text = sanitize_visible_text(&prefetch_sentence.text);
                                            if !prefetch_text.is_empty() {
                                                let current_voice_id = self.tts_controller.current_voice_id().await;
                                                // 获取轮次内已确认的引擎（用于继承）
                                                let inherited_engine = {
                                                    self.tts_controller.turn_confirmed_engine.lock().await.clone()
                                                };
                                                // 同声传译模式：强制使用 Edge TTS
                                                let force_engine = if self.is_translation_mode { Some(TtsEngineKind::EdgeTts) } else { None };
                                                let selection = select_tts_engine(
                                                    &prefetch_text,
                                                    current_voice_id.as_deref(),
                                                    &prefetch_sentence.language_confidences,
                                                    inherited_engine,
                                                    force_engine,
                                                );
                                                let engine = selection.engine;
                                                // 如果是确定性路由且尚未锁定，更新轮次缓存
                                                if selection.is_confident {
                                                    let mut engine_guard = self.tts_controller.turn_confirmed_engine.lock().await;
                                                    if engine_guard.is_none() {
                                                        info!("🔒 预取路径确定路由，锁定轮次引擎: {:?}", engine);
                                                        *engine_guard = Some(engine);
                                                    }
                                                }
                                                // 关闭旧预取任务
                                                {
                                                    let mut handle_guard = self.inflight_task_handle.lock().await;
                                                    if let Some(h) = handle_guard.take() { h.abort(); }
                                                }
                                                // 创建预取缓冲通道
                                                let (tx, rx) = mpsc::channel::<AudioChunk>(512);
                                                {
                                                    let mut rx_guard = self.inflight_audio_rx.lock().await;
                                                    *rx_guard = Some(rx);
                                                }
                                                // 增益已嵌入 AudioChunk，无需设置共享状态
                                                // 启动独立HTTP流
                                                let tts_ctrl_for_prefetch = self.tts_controller.clone();
                                                let handle = tokio::spawn(async move {
                                                    match engine {
                                                        TtsEngineKind::MiniMax => {
                                                            let cfg = tts_ctrl_for_prefetch.tts_config.clone().unwrap_or_default();
                                                            let client = MiniMaxHttpTtsClient::new(cfg.clone());
                                                            let voice_setting = { tts_ctrl_for_prefetch.voice_setting.lock().await.clone() };
                                                            // 优先使用轮次缓存，缓存为空时进行语言检测并更新
                                                            let (virtual_voice_id, lang) = {
                                                                let mut turn_voice_guard = tts_ctrl_for_prefetch.turn_detected_voice_id.lock().await;
                                                                let mut turn_lang_guard = tts_ctrl_for_prefetch.turn_detected_language.lock().await;

                                                                if turn_voice_guard.is_some() {
                                                                    (turn_voice_guard.clone().unwrap(), turn_lang_guard.clone())
                                                                } else {
                                                                    // 获取用户配置的音色
                                                                    let configured_voice = tts_ctrl_for_prefetch.current_voice_id().await
                                                                        .unwrap_or_else(|| cfg.default_voice_id.clone().unwrap_or_else(|| "zh_female_wanwanxiaohe_moon_bigtts".to_string()));
                                                                    let is_default_voice = configured_voice == "zh_female_wanwanxiaohe_moon_bigtts";

                                                                    if let Some(detected_lang) = detect_language_boost(&prefetch_text, LINGUA_MIN_CONFIDENCE, LINGUA_MIN_MARGIN) {
                                                                        *turn_lang_guard = Some(detected_lang.clone());
                                                                        // 只有默认音色时才自动切换
                                                                        if is_default_voice {
                                                                            if let Some(voice) = get_voice_for_language(&detected_lang) {
                                                                                *turn_voice_guard = Some(voice.to_string());
                                                                                info!("🎙️ [预取] 检测到语言并设置音色: lang={}, voice={}", detected_lang, voice);
                                                                                (voice.to_string(), Some(detected_lang))
                                                                            } else {
                                                                                *turn_voice_guard = Some(configured_voice.clone());
                                                                                info!("🎙️ [预取] 检测到语言但无映射，锁定fallback: lang={}, voice={}", detected_lang, configured_voice);
                                                                                (configured_voice, Some(detected_lang))
                                                                            }
                                                                        } else {
                                                                            // 用户设置了自定义音色，不自动切换
                                                                            *turn_voice_guard = Some(configured_voice.clone());
                                                                            info!("🎙️ [预取] 用户设置了自定义音色，跳过自动切换: voice={}, lang={}", configured_voice, detected_lang);
                                                                            (configured_voice, Some(detected_lang))
                                                                        }
                                                                    } else {
                                                                        (configured_voice, tts_ctrl_for_prefetch.language.lock().await.clone())
                                                                    }
                                                                }
                                                            };
                                                            match client.synthesize_text(&virtual_voice_id, &prefetch_text, voice_setting, Some(AudioSetting::default()), None, None, lang, MiniMaxHttpOptions::default()).await {
                                                                Ok(stream) => {
                                                                    tokio::pin!(stream);
                                                                    while let Some(item) = stream.next().await {
                                                                        match item {
                                                                            Ok(chunk) => { if tx.send(chunk).await.is_err() { break; } }
                                                                            Err(e) => { warn!("⚠️ 预取MiniMax流错误: {}", e); break; }
                                                                        }
                                                                    }
                                                                },
                                                                Err(e) => warn!("⚠️ 启动MiniMax预取失败: {}", e),
                                                            }
                                                        },
                                                        TtsEngineKind::VolcEngine => {
                                                            match VolcEngineTtsClient::from_env() {
                                                                Ok(client) => {
                                                                    let mut req = VolcEngineRequest::from_text(prefetch_text.clone());
                                                                    req.emotion = Some("energetic".to_string());
                                                                    match client.stream_sentence(req) {
                                                                        Ok(stream) => {
                                                                            tokio::pin!(stream);
                                                                            while let Some(item) = stream.next().await {
                                                                                match item {
                                                                                    Ok(chunk) => { if tx.send(chunk).await.is_err() { break; } }
                                                                                    Err(e) => { warn!("⚠️ 预取Volc流错误: {}", e); break; }
                                                                                }
                                                                            }
                                                                        },
                                                                        Err(e) => warn!("⚠️ 启动Volc预取失败: {}", e),
                                                                    }
                                                                },
                                                                Err(e) => warn!("⚠️ 构建Volc客户端失败: {}", e),
                                                            }
                                                        },
                                                        TtsEngineKind::Baidu => {
                                                            match crate::tts::baidu::BaiduHttpTtsClient::from_env() {
                                                                Ok(client) => {
                                                                    let mut req = crate::tts::baidu::BaiduHttpTtsRequest::new(prefetch_text.clone());
                                                                    if let Some(voice_id) = tts_ctrl_for_prefetch.current_voice_id().await {
                                                                        if let Some(per) = crate::tts::baidu::baidu_per_for_voice_id(&voice_id) {
                                                                            req = req.with_per(per);
                                                                        }
                                                                        if let Some(payload) = crate::tts::baidu::baidu_payload_override_for_voice_id(
                                                                            &voice_id,
                                                                            client.config().build_start_payload(),
                                                                        ) {
                                                                            if let Some(spd) = payload.spd {
                                                                                req = req.with_spd(spd);
                                                                            }
                                                                            if let Some(pit) = payload.pit {
                                                                                req = req.with_pit(pit);
                                                                            }
                                                                            if let Some(vol) = payload.vol {
                                                                                req = req.with_vol(vol);
                                                                            }
                                                                        }
                                                                    }
                                                                    match client.synthesize(req) {
                                                                        Ok(stream) => {
                                                                            tokio::pin!(stream);
                                                                            while let Some(item) = stream.next().await {
                                                                                match item {
                                                                                    Ok(chunk) => { if tx.send(chunk).await.is_err() { break; } }
                                                                                    Err(e) => { warn!("⚠️ 预取Baidu流错误: {}", e); break; }
                                                                                }
                                                                            }
                                                                        },
                                                                        Err(e) => warn!("⚠️ 启动Baidu预取失败: {}", e),
                                                                    }
                                                                },
                                                                Err(e) => warn!("⚠️ 构建Baidu客户端失败: {}", e),
                                                            }
                                                        },
                                                        TtsEngineKind::EdgeTts => {
                                                            let client = crate::tts::edge::EdgeTtsClient::with_defaults();
                                                            let voice = crate::tts::edge::get_voice_for_language("zh")
                                                                .unwrap_or("zh-CN-XiaoxiaoNeural");
                                                            match client.synthesize(&prefetch_text, Some(voice)).await {
                                                                Ok(stream) => {
                                                                    tokio::pin!(stream);
                                                                    while let Some(item) = stream.next().await {
                                                                        match item {
                                                                            Ok(chunk) => { if tx.send(chunk).await.is_err() { break; } }
                                                                            Err(e) => { warn!("⚠️ 预取Edge TTS流错误: {}", e); break; }
                                                                        }
                                                                    }
                                                                },
                                                                Err(e) => warn!("⚠️ 启动Edge TTS预取失败: {}", e),
                                                            }
                                                        },
                                                        TtsEngineKind::AzureTts => {
                                                            match crate::tts::azure::AzureTtsClient::from_env() {
                                                                Ok(client) => {
                                                                    let voice = crate::tts::azure::get_voice_for_language("zh")
                                                                        .unwrap_or("zh-CN-XiaoxiaoNeural");
                                                                    match client.synthesize(&prefetch_text, Some(voice)).await {
                                                                        Ok(stream) => {
                                                                            tokio::pin!(stream);
                                                                            while let Some(item) = stream.next().await {
                                                                                match item {
                                                                                    Ok(chunk) => { if tx.send(chunk).await.is_err() { break; } }
                                                                                    Err(e) => { warn!("⚠️ 预取Azure TTS流错误: {}", e); break; }
                                                                                }
                                                                            }
                                                                        },
                                                                        Err(e) => warn!("⚠️ 启动Azure TTS预取失败: {}", e),
                                                                    }
                                                                },
                                                                Err(e) => warn!("⚠️ 创建Azure TTS客户端失败: {}", e),
                                                            }
                                                        },
                                                    }
                                                });
                                                {
                                                    let mut handle_guard = self.inflight_task_handle.lock().await;
                                                    *handle_guard = Some(handle);
                                                }
                                            }
                                        }
                                    }

                                    // 🎯 若当前没有在处理的句子，这是该轮的第一句（队列由空变非空），立即处理第一句
                                    // 🔧 修复：必须检查 text_splitter_first_chunk_recorded 标志，确保只有第一句才会触发
                                    // 如果第一句已经开始处理了，finalize 产出的句子应该排队等待 PacedSender 触发
                                    let should_process_first = {
                                        let first_not_started = !self.text_splitter_first_chunk_recorded.load(Ordering::Acquire);
                                        if !first_not_started {
                                            false // 第一句已经开始处理，不要触发
                                        } else {
                                            let queue = self.sentence_queue.lock().await;
                                            queue.has_pending()
                                        }
                                    };
                                    if should_process_first {
                                        info!("🎯 finalize后队列首句（真正的第一句），立即触发处理");
                                        if let Err(e) = self.process_next_sentence().await {
                                            error!("❌ 处理首句失败（finalize后）: {}", e);
                                        }
                                    }
                                } else {
                                    // 🔍 诊断：finalize没有产出任何句子
                                    // 🔍 诊断：finalize没有产出任何句子
                                    // 🔧 修复：只有当确实没有正在处理的句子且总数为0时才认为是真的"无产出"
                                    let is_really_empty = {
                                        let queue = self.sentence_queue.lock().await;
                                        queue.is_empty() && !queue.is_processing() && queue.total_count() == 0
                                    };

                                    if is_really_empty {
                                        warn!(
                                            "🔍 [TURN_COMPLETE] Splitter finalize: 无剩余文本且无正在处理句子，整轮无产出，splitter_buffer_len={}, session={}",
                                            splitter_buffer_len, self.session_id
                                        );
                                        // 🆕 整轮无音频产出时，发送 output_audio_buffer.stopped 事件，让客户端知道轮次结束
                                        if let Some(ref response_id) = current_turn_id {
                                            self.emitter.output_audio_buffer_stopped(response_id).await;
                                            info!(
                                                "📤 [音频事件] session_id={}, event=output_audio_buffer.stopped, response_id={} (整轮无产出)",
                                                self.session_id, response_id
                                            );
                                        }
                                    } else {
                                        info!(
                                            "🔍 [TURN_COMPLETE] Splitter finalize: 无剩余文本，但有正在处理或已处理句子，非空轮次。splitter_buffer_len={}, session={}",
                                            splitter_buffer_len, self.session_id
                                        );
                                    }
                                }

                                // 🔧 新增：记录轮次完成事件，用于调试
                                info!("📊 轮次完成统计: session={}, turn_id={:?}, current_turn_sequence={:?}",
                                      self.session_id, current_turn_id, current_turn_sequence);

                                // ✅ 标记轮次完成，并将队列标记为 LLM 完成（供音频线程判定 turn_final）
                                llm_turn_complete.store(true, Ordering::Release);
                                let (queue_len, queue_empty, total_count, current_processing) = {
                                    let mut queue = self.sentence_queue.lock().await;
                                    queue.mark_llm_complete();
                                    (queue.len(), queue.is_empty(), queue.total_count(), queue.current_processing_idx())
                                };
                                info!(
                                    "🔍 [TURN_COMPLETE诊断] llm_turn_complete已设置=true, 队列状态: len={}, is_empty={}, total_count={}, current_processing={:?}, session={}",
                                    queue_len, queue_empty, total_count, current_processing, self.session_id
                                );
                                info!("⏳ 已标记轮次完成，等待播放完当前句子后再结束TTS: session={}", self.session_id);
                                continue;
                            }

                                                        // 🔧 移除快速打断检查：使用单一receiver避免重复处理
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!("⚠️ LLM→TTS 通道滞后 {} 条消息，丢弃过期文本，继续监听: session={}", n, self.session_id);
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            info!("📡 LLM→TTS 广播通道已关闭，TTS任务退出: session={}", self.session_id);
                            break;
                        }
                    }
                                }

                // 🔧 移除定期检查机制：通过单一receiver避免重复处理打断信号
            }
        }

        // 重置会话标志
        self.tts_session_created.store(false, Ordering::Release);
        // 音频处理任务状态将在主循环结束时自动重置

        // 🔧 关键修复：TTS任务结束时，优雅结束当前session（无锁）
        if let Some(pool_client_arc) = {
            let guard = self.tts_controller.pool_client.lock().await;
            guard.clone()
        } {
            let mut client = pool_client_arc.lock().await;
            client.abort_active_stream();
        }

        // 🔧 关键修复：TTS任务结束时，确保音频处理任务被正确停止
        info!("🛑 TTS任务结束，停止音频处理任务避免内存泄漏");
        {
            let mut task_guard = self.audio_handler_task.lock().await;
            if let Some(audio_task_handle) = task_guard.take() {
                if !audio_task_handle.is_finished() {
                    info!("🛑 强制停止音频处理任务: session={}", self.session_id);
                    audio_task_handle.abort();
                    // 等待任务完全停止，避免资源泄漏
                    match tokio::time::timeout(std::time::Duration::from_secs(2), audio_task_handle).await {
                        Ok(Ok(_)) => info!("✅ 音频处理任务已正常停止"),
                        Ok(Err(e)) if e.is_cancelled() => info!("✅ 音频处理任务已被取消"),
                        Ok(Err(e)) => warn!("⚠️ 音频处理任务停止时出现错误: {}", e),
                        Err(_) => warn!("⚠️ 音频处理任务停止超时，但已发送abort信号"),
                    }
                } else {
                    info!("✅ 音频处理任务已自然结束");
                }
            }
        }

        // 🔧 关键修复：TTS任务结束时，确保会话级别音频发送器被清理
        // 但给它时间先发送完剩余的音频数据
        info!("🔄 TTS任务结束，准备清理会话级别音频发送器（优雅清理模式）");
        {
            let mut sender_guard = self.tts_controller.session_audio_sender.lock().await;
            sender_guard.cleanup().await;
        }

        // 🆕 关键修复：TTS任务结束时，确保TTS客户端被归还到池中（如果还没有归还的话）
        info!("🔓 TTS任务完成，确保TTS客户端已归还到全局池: session={}", self.session_id);
        self.tts_controller.return_client().await;

        // 发送任务完成信号
        let _ = self.task_completion_tx.send(TaskCompletion::Tts);
        info!("✅ 管线级别 VolcEngine TTS 任务结束: {}", self.session_id);

        Ok(())
    }

    /// 🆕 启动会话级别音频处理任务（使用会话级别的PacedAudioSender）
    /// 🔧 修复：确保每次只有一个音频处理任务运行，避免重复打断处理
    /// 🔧 改进：使用广播接收器，支持多用户订阅
    async fn spawn_session_audio_handler(&self, audio_rx: tokio::sync::broadcast::Receiver<AudioChunk>) {
        // 🔧 关键修复：在创建新任务前，先停止并清理之前的音频处理任务
        {
            let mut task_guard = self.audio_handler_task.lock().await;
            if let Some(previous_handle) = task_guard.take() {
                info!("🛑 检测到已存在的音频处理任务，正在停止: session={}", self.session_id);
                previous_handle.abort();

                // 等待任务完全停止（忽略取消错误）
                match previous_handle.await {
                    Ok(_) => info!("✅ 之前的音频处理任务已正常停止: session={}", self.session_id),
                    Err(e) if e.is_cancelled() => {
                        info!("✅ 之前的音频处理任务已被取消: session={}", self.session_id);
                        // 🆕 给予额外的缓冲时间，确保资源完全释放
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    },
                    Err(e) => warn!("⚠️ 停止之前音频处理任务时出现错误: {} session={}", e, self.session_id),
                }
            }
        }

        // 🆕 再等待一小段时间，确保TTS broadcast channel完全ready
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let _emitter = self.emitter.clone();
        let session_id = self.session_id.clone();
        let interrupt_handler = if let Some(handler) = &self.simple_interrupt_handler {
            handler.clone()
        } else {
            SimpleInterruptHandler::new(
                self.session_id.clone(),
                "TTS-Session-Audio".to_string(),
                self.simple_interrupt_manager.subscribe(),
            )
        };

        // 🔧 修复：移除专用打断接收器，避免重复处理打断信号
        // let mut dedicated_interrupt_rx = interrupt_handler.subscribe();
        let tts_controller_weak = Arc::downgrade(&self.tts_controller);
        // 初始化/获取代次watch接收器
        let gen_rx = {
            let current = self.tts_controller.audio_subscription_gen.load(Ordering::Acquire);
            let mut guard = self.tts_controller.audio_gen_watch_tx.lock().await;
            if guard.is_none() {
                let (tx, rx) = tokio::sync::watch::channel(current);
                *guard = Some(tx);
                rx
            } else {
                guard.as_ref().unwrap().subscribe()
            }
        };
        let session_audio_sender = self.tts_controller.session_audio_sender.clone();

        // 🚨 关键修复：在创建任务前确保SessionAudioSender被正确初始化
        {
            let mut sender_guard = self.tts_controller.session_audio_sender.lock().await;
            if !sender_guard.is_initialized() {
                info!("🔧 检测到SessionAudioSender未初始化，进行初始化");
                if let Err(e) = sender_guard
                    .initialize(
                        self.session_id.clone(),
                        self.router.clone(),
                        self.simple_interrupt_handler.clone().unwrap_or_else(|| {
                            SimpleInterruptHandler::new(
                                self.session_id.clone(),
                                "SessionAudio".to_string(),
                                self.simple_interrupt_manager.subscribe(),
                            )
                        }),
                        self.initial_burst_count,
                        self.initial_burst_delay_ms,
                        self.send_rate_multiplier,
                        None, // 使用最新的待应用配置（pending_output配置）
                        Some(self.emitter.signal_only_flag()),
                        Some(self.next_sentence_trigger_tx.clone()), // 🆕 按需 TTS 生成
                        self.is_translation_mode,                    // 🆕 同声传译模式
                    )
                    .await
                {
                    error!("❌ SessionAudioSender初始化失败: {}", e);
                    // 初始化失败，继续执行但记录错误
                } else {
                    info!("✅ SessionAudioSender初始化成功");
                }
            } else {
                info!("✅ SessionAudioSender已正确初始化，继续使用");
            }
        }

        // 🔧 关键修复：在任务中绑定当前订阅代次，客户端切换时旧任务会观察到代次变化后退出
        let bind_gen = self.tts_controller.audio_subscription_gen.load(Ordering::Acquire);

        // 🔧 传递 Pipeline 级别的 current_turn_response_id 引用给音频处理任务
        let current_turn_response_id_for_audio_task = self.current_turn_response_id.clone();

        // 🔧 关键修复：创建任务并保存句柄（避免捕获 &self 到 'static 任务中）
        let handle = tokio::spawn(Self::audio_handler_task_loop(
            session_id,
            audio_rx,
            interrupt_handler,
            gen_rx,
            bind_gen,
            current_turn_response_id_for_audio_task,
            session_audio_sender,
            tts_controller_weak,
            self.audio_sending_stopped.clone(),
            self.shared_flags.assistant_response_context.clone(),
            self.sentence_queue.clone(),
            self.turn_final_injected.clone(),
        ));

        // 🔧 关键修复：保存任务句柄
        {
            let mut task_guard = self.audio_handler_task.lock().await;
            *task_guard = Some(handle);
        }
    }

    /// 会话级别音频处理任务主体
    #[allow(clippy::too_many_arguments)]
    async fn audio_handler_task_loop(
        session_id: String,
        mut audio_rx: tokio::sync::broadcast::Receiver<AudioChunk>,
        mut interrupt_handler: SimpleInterruptHandler,
        mut gen_rx: tokio::sync::watch::Receiver<u64>,
        bind_gen: u64,
        current_turn_response_id_for_audio_task: Arc<super::lockfree_response_id::LockfreeResponseIdReader>,
        session_audio_sender: Arc<Mutex<SessionAudioSender>>,
        tts_controller_weak: std::sync::Weak<TtsController>,
        audio_sending_stopped: Arc<AtomicBool>,
        assistant_context: Arc<super::types::OptimizedAssistantResponseContext>,
        sentence_queue: Arc<Mutex<SentenceQueue>>,
        turn_final_injected: Arc<AtomicBool>,
    ) {
        info!(
            "🎵 启动会话级别音频处理任务: session={}, tts_controller_available={}",
            session_id,
            tts_controller_weak.strong_count() > 0
        );

        let mut _audio_chunks_sent = 0u64;
        let mut first_chunk = true;
        let mut current_response_id = String::new();
        let mut current_assistant_item_id = String::new();
        let mut current_user_item_id = String::new();

        // 🆕 每个句子单独累积，句子完成时立即保存（防止打断丢失）
        let mut current_sentence_audio: Vec<u8> = Vec::new();
        let mut current_sentence_duration_ms = 0u32;
        #[allow(unused_assignments)]
        let mut current_sentence_seq_id: u64 = 0;

        info!("✅ SessionAudioSender准备就绪，开始处理音频");

        loop {
            tokio::select! {
                Ok(()) = gen_rx.changed() => {
                    let current_gen = *gen_rx.borrow();
                    if current_gen != bind_gen {
                        info!("🔄 检测到音频订阅代次变化（{} -> {}），退出旧音频处理任务: session={}", bind_gen, current_gen, session_id);
                        break;
                    }
                }
                audio_result = audio_rx.recv() => {
                    match audio_result {
                        Ok(chunk) => {
                            // 🆕 生产侧触发：检测带文字的空块（来自 TTS 客户端）
                            if chunk.data.is_empty() && !chunk.is_final && chunk.sentence_text.is_some() {
                                // 直接转发文字空块到 PacedSender
                                let text = chunk.sentence_text.clone().unwrap();
                                let cleaned = sanitize_visible_text(&text);
                                if !cleaned.is_empty() {
                                    let text_chunk = PacedAudioChunk {
                                        audio_data: Bytes::new(),
                                        is_final: false,
                                        realtime_metadata: Some(RealtimeAudioMetadata {
                                            response_id: current_turn_response_id_for_audio_task.load()
                                                .unwrap_or_else(|| {
                                                    if !current_response_id.is_empty() {
                                                        current_response_id.clone()
                                                    } else {
                                                        format!("resp_{}", nanoid::nanoid!(8))
                                                    }
                                                }),
                                            assistant_item_id: if current_assistant_item_id.is_empty() {
                                                format!("asst_{}", nanoid::nanoid!(6))
                                            } else {
                                                current_assistant_item_id.clone()
                                            },
                                            output_index: 0,
                                            content_index: chunk.sequence_id as u32,
                                        }),
                                        sentence_text: Some(cleaned),
                                        turn_final: false,
                                    };
                                    let sender_guard = session_audio_sender.lock().await;
                                    let _ = sender_guard.send_audio(text_chunk).await;
                                    drop(sender_guard);
                                    info!("📨 已转发生产侧文字空块: text='{}'", text.chars().take(30).collect::<String>());
                                }
                                continue;
                            }

                            if chunk.data.is_empty() && !chunk.is_final {
                                debug!("🗑️ 丢弃空音频块（非控制信号）: seq={}", chunk.sequence_id);
                                continue;
                            }

                            if chunk.data.is_empty() && chunk.is_final {
                                let is_turn_finish_expected = {
                                    let queue = sentence_queue.lock().await;
                                    queue.is_llm_complete() && !queue.has_pending() && !queue.has_inflight() && !queue.is_processing()
                                };
                                let is_real_task_finish = chunk.sequence_id == u64::MAX && is_turn_finish_expected;
                                if is_real_task_finish {
                                    info!("🔓 收到SESSION_FINISHED信号 (sequence_id=u64::MAX)，发送final chunk到PacedSender: session={}", session_id);
                                } else {
                                    info!("🏁 收到句尾final (sequence_id={})，发送final chunk到PacedSender: session={}", chunk.sequence_id, session_id);
                                }
                                // 🆕 生产侧触发：文字空块已由 TTS 客户端在首帧前广播，这里无需补发
                                // 修正：仅在cc时才标记 turn_final=true，
                                // 句尾空final不再通过“队列空+llm_complete”判断为整轮结束，避免提前 text.done/stopped
                                let final_chunk = PacedAudioChunk {
                                    audio_data: Bytes::new(),
                                    is_final: true,
                                    realtime_metadata: Some(RealtimeAudioMetadata {
                                        response_id: current_turn_response_id_for_audio_task.load()
                                            .unwrap_or_else(|| {
                                                if !current_response_id.is_empty() {
                                                    current_response_id.clone()
                                                } else {
                                                    format!("resp_{}", nanoid::nanoid!(8))
                                                }
                                            }),
                                        assistant_item_id: if current_assistant_item_id.is_empty() { format!("asst_{}", nanoid::nanoid!(6)) } else { current_assistant_item_id.clone() },
                                        output_index: 0,
                                        content_index: 0,
                                    }),
                                    sentence_text: None,
                                    turn_final: is_real_task_finish,
                                };

                                let sender_guard = session_audio_sender.lock().await;
                                let try_res = sender_guard.try_send_audio(&final_chunk).await;
                                let _ = match try_res {
                                    Ok(_) => Ok(()),
                                    Err(e) => {
                                        if e.to_string().contains("full") {
                                            sender_guard.send_audio(final_chunk).await
                                        } else {
                                            Err(e)
                                        }
                                    }
                                };
                                drop(sender_guard);

                                {
                                    let mut q = sentence_queue.lock().await;
                                    q.mark_current_processing_complete();
                                }

                                // 🧹 若这是最后一句的句内final（非SESSION_FINISHED路径），并且LLM已完成且队列已空，则此刻再收尾（幂等）
                                if !is_real_task_finish {
                                    // 🔍 增强诊断：获取队列详细状态并记录
                                    let (llm_complete_status, has_pending, has_inflight, is_turn_final_now) = {
                                        let q = sentence_queue.lock().await;
                                        let is_final = q.is_llm_complete() && !q.has_pending() && !q.has_inflight();
                                        (q.is_llm_complete(), q.has_pending(), q.has_inflight(), is_final)
                                    };
                                    let already_injected = turn_final_injected.load(Ordering::Acquire);

                                    debug!(
                                        "🔍 [句内final检查] session={}, is_real_task_finish={}, llm_complete={}, has_pending={}, has_inflight={}, is_turn_final_now={}, already_injected={}, sequence_id={}",
                                        session_id, is_real_task_finish, llm_complete_status, has_pending, has_inflight, is_turn_final_now, already_injected, chunk.sequence_id
                                    );

                                    if is_turn_final_now && !turn_final_injected.swap(true, Ordering::AcqRel) {
                                        info!(
                                            "🔍 [句内final注入] 条件满足，准备注入turn-final: session={}, sequence_id={}",
                                            session_id, chunk.sequence_id
                                        );
                                        let response_id_for_final = current_turn_response_id_for_audio_task
                                            .load()
                                            .unwrap_or_else(|| {
                                                if !current_response_id.is_empty() {
                                                    current_response_id.clone()
                                                } else {
                                                    format!("resp_{}", nanoid::nanoid!(8))
                                                }
                                            });
                                        let final_chunk = PacedAudioChunk {
                                            audio_data: Bytes::new(),
                                            is_final: true,
                                            realtime_metadata: Some(RealtimeAudioMetadata {
                                                response_id: response_id_for_final,
                                                assistant_item_id: current_assistant_item_id.clone(),
                                                output_index: 0,
                                                content_index: 0,
                                            }),
                                            sentence_text: None,
                                            turn_final: true,
                                        };
                                        let sender_guard = session_audio_sender.lock().await;
                                        let _ = sender_guard.send_audio(final_chunk).await;
                                        drop(sender_guard);
                                        info!("🏁 句内final后收尾：已注入turn-final，完成整轮 text.done/stopped: session={}", session_id);
                                        // 通知底层引擎结束（容错）
                                        info!("🔚 句内final后：发送 task_finish 并关闭连接");
                                        if let Some(tts_controller_arc) = tts_controller_weak.upgrade() {
                                            let _ = tts_controller_arc.finish_current_task().await;
                                        }
                                    } else {
                                        // 🔍 诊断：记录为什么没有注入turn-final
                                        if !is_turn_final_now {
                                            debug!(
                                                "🔍 [句内final跳过] 条件不满足: llm_complete={}, has_pending={}, has_inflight={}, session={}, sequence_id={}",
                                                llm_complete_status, has_pending, has_inflight, session_id, chunk.sequence_id
                                            );
                                        } else if already_injected {
                                            debug!(
                                                "🔍 [句内final跳过] turn-final已注入，跳过重复注入: session={}, sequence_id={}",
                                                session_id, chunk.sequence_id
                                            );
                                        }
                                    }
                                } else {
                                    debug!(
                                        "🔍 [句内final跳过] is_real_task_finish=true，跳过句内final检查: session={}, sequence_id={}",
                                        session_id, chunk.sequence_id
                                    );
                                }

                                if is_real_task_finish {
                                    info!("✅ SESSION_FINISHED处理完成，音频处理任务结束: session={}", session_id);
                                    break;
                                } else {
                                    debug!("✅ 句子完成（空final），保持订阅等待下一句: session={}", session_id);
                                    continue;
                                }
                            }

                            // 🆕 累积当前句子的音频数据
                            current_sentence_audio.extend_from_slice(&chunk.data);
                            current_sentence_duration_ms += (chunk.data.len() as f64 / (TTS_SOURCE_SAMPLE_RATE as f64 * 2.0) * 1000.0) as u32;
                            current_sentence_seq_id = chunk.sequence_id;

                            if current_response_id.is_empty() {
                                current_response_id = current_turn_response_id_for_audio_task.load()
                                    .unwrap_or_else(|| format!("resp_{}", nanoid::nanoid!(8)));
                            }
                            if current_assistant_item_id.is_empty() {
                                current_assistant_item_id = assistant_context
                                    .get_context_copy()
                                    .map(|c| c.assistant_item_id)
                                    .unwrap_or_else(|| format!("asst_{}", nanoid::nanoid!(6)));
                            }
                            if current_user_item_id.is_empty() {
                                current_user_item_id = format!("user_{}", nanoid::nanoid!(6));
                            }

                            if chunk.is_final {
                                if chunk.sequence_id == u64::MAX {
                                    debug!("🔚 收到任务结束标记 (sequence_id=u64::MAX)，跳过处理（由上面的空chunk分支处理）");
                                    continue;
                                }

                                // 🆕 句子完成时立即保存到数据库（即使被打断也不丢失）
                                if !current_sentence_audio.is_empty() {
                                    let session_id_for_save = session_id.clone();
                                    let response_id_base = current_turn_response_id_for_audio_task.load()
                                        .unwrap_or_else(|| current_response_id.clone());
                                    // 使用 response_id + _seq{N} 区分不同句子
                                    let response_id_for_save = format!("{}_seq{}", response_id_base, current_sentence_seq_id);
                                    let audio_data_for_save = std::mem::take(&mut current_sentence_audio);
                                    let duration_for_save = current_sentence_duration_ms;
                                    current_sentence_duration_ms = 0;
                                    tokio::spawn(async move {
                                        use super::session_data_integration::{save_tts_audio_globally, create_audio_metadata_from_pcm_s16le};
                                        let mut meta = create_audio_metadata_from_pcm_s16le(audio_data_for_save.len(), TTS_SOURCE_SAMPLE_RATE, 1);
                                        if duration_for_save > 0 { meta.duration_ms = duration_for_save; }
                                        let _ = save_tts_audio_globally(&session_id_for_save, &response_id_for_save, bytes::Bytes::from(audio_data_for_save), meta).await;
                                    });
                                }

                                debug!("📝 句子音频生成完成 (sequence_id={})，已保存到DB，继续等待下一句", chunk.sequence_id);
                                // 🆕 生产侧触发：文字空块已由 TTS 客户端在首帧前广播，这里无需补发
                                // 🆕 将44100Hz原始音频降采样到16kHz用于播放（使用 chunk 自带的增益值）
                                let tts_frame = TtsAudioFrame::from_audio_chunk(&chunk);
                                let resampled_data = tts_frame.process_to_16k_with_db_gain(chunk.gain_db);
                                let paced_chunk = PacedAudioChunk {
                                    audio_data: Bytes::from(resampled_data),
                                    is_final: true,
                                    realtime_metadata: Some(RealtimeAudioMetadata {
                                        response_id: current_response_id.clone(),
                                        assistant_item_id: current_assistant_item_id.clone(),
                                        output_index: 0,
                                        content_index: chunk.sequence_id as u32,
                                    }),
                                    sentence_text: chunk.sentence_text.clone(),
                                    // 句内 final，不是整轮
                                    turn_final: false,
                                };
                                if audio_sending_stopped.load(Ordering::Acquire) {
                                    debug!("🚫 音频发送已停止（打断），跳过发送音频块: session={}", session_id);
                                    continue;
                                }
                                let sender_guard = session_audio_sender.lock().await;
                                let try_res = sender_guard.try_send_audio(&paced_chunk).await;
                                let _ = match try_res {
                                    Ok(_) => Ok(()),
                                    Err(e) => {
                                        if e.to_string().contains("full") {
                                            sender_guard.send_audio(paced_chunk).await
                                        } else {
                                            Err(e)
                                        }
                                    }
                                };
                                drop(sender_guard);

                                // 已移除延迟收尾逻辑，整轮收尾在"队列空+LLM完成"处统一处理
                                // 🆕 关键修复：非空final块也需要标记当前处理完成，并检查是否需要注入turn-final
                                {
                                    let mut q = sentence_queue.lock().await;
                                    q.mark_current_processing_complete();
                                }

                                // 🧹 检查是否需要注入 turn-final（与空final逻辑一致）
                                let is_turn_finish_expected = {
                                    let queue = sentence_queue.lock().await;
                                    queue.is_llm_complete() && !queue.has_pending() && !queue.has_inflight() && !queue.is_processing()
                                };

                                if is_turn_finish_expected {
                                    // 🔍 增强诊断：获取队列详细状态并记录
                                    let (llm_complete_status, has_pending, has_inflight, is_processing) = {
                                        let q = sentence_queue.lock().await;
                                        (q.is_llm_complete(), q.has_pending(), q.has_inflight(), q.is_processing())
                                    };
                                    let already_injected = turn_final_injected.load(Ordering::Acquire);

                                    debug!(
                                        "🔍 [非空句内final检查] session={}, llm_complete={}, has_pending={}, has_inflight={}, is_processing={}, already_injected={}, sequence_id={}",
                                        session_id, llm_complete_status, has_pending, has_inflight, is_processing, already_injected, chunk.sequence_id
                                    );

                                    if !turn_final_injected.swap(true, Ordering::AcqRel) {
                                        // 🆕 关键修复：只有在未打断的情况下才注入turn-final
                                        if !audio_sending_stopped.load(Ordering::Acquire) {
                                            info!(
                                                "🔍 [非空句内final注入] 条件满足，准备注入turn-final: session={}, sequence_id={}",
                                                session_id, chunk.sequence_id
                                            );

                                            // 构造一个纯控制的 final chunk
                                            let response_id_for_final = current_response_id.clone();
                                            let final_control_chunk = PacedAudioChunk {
                                                audio_data: Bytes::new(),
                                                is_final: true,
                                                realtime_metadata: Some(RealtimeAudioMetadata {
                                                    response_id: response_id_for_final,
                                                    assistant_item_id: current_assistant_item_id.clone(),
                                                    output_index: 0,
                                                    content_index: 0,
                                                }),
                                                sentence_text: None,
                                                turn_final: true,
                                            };

                                            let sender_guard = session_audio_sender.lock().await;
                                            let _ = sender_guard.send_audio(final_control_chunk).await;
                                            drop(sender_guard);

                                            info!("🏁 非空句内final后收尾：已注入turn-final，完成整轮 text.done/stopped: session={}", session_id);
                                            // 通知底层引擎结束（容错）
                                            if let Some(tts_controller_arc) = tts_controller_weak.upgrade() {
                                                let _ = tts_controller_arc.finish_current_task().await;
                                            }
                                        } else {
                                            info!("🚫 [非空句内final注入] 此时已打断，跳过注入turn-final: session={}", session_id);
                                        }
                                    }
                                }

                                debug!("📝 句子音频发送完成，继续等待同一响应的下一句音频");
                                continue;
                            }

                            // 🆕 生产侧触发：文字空块已由 TTS 客户端在首帧前广播，这里无需注入
                            // 🆕 将44100Hz原始音频降采样到16kHz用于播放（使用 chunk 自带的增益值）
                            let tts_frame = TtsAudioFrame::from_audio_chunk(&chunk);
                            let resampled_data = tts_frame.process_to_16k_with_db_gain(chunk.gain_db);
                            let paced_chunk = PacedAudioChunk {
                                audio_data: Bytes::from(resampled_data),
                                is_final: false,
                                realtime_metadata: Some(RealtimeAudioMetadata {
                                    response_id: current_response_id.clone(),
                                    assistant_item_id: current_assistant_item_id.clone(),
                                    output_index: 0,
                                    content_index: chunk.sequence_id as u32,
                                }),
                                sentence_text: chunk.sentence_text.clone(),
                                turn_final: false,
                            };
                            if audio_sending_stopped.load(Ordering::Acquire) {
                                debug!("🚫 音频发送已停止（打断），跳过发送final chunk: session={}", session_id);
                                continue;
                            }
                            let sender_guard = session_audio_sender.lock().await;
                            let _ = sender_guard.send_audio(paced_chunk).await;
                            drop(sender_guard);

                            _audio_chunks_sent += 1;
                            if first_chunk {
                                info!("🎵 首个音频块已发送到SessionAudioSender");
                                let first_audio_time = std::time::Instant::now();
                                record_node_time_and_try_report(&session_id, TimingNode::TtsFirstAudio, first_audio_time, Some(&current_response_id)).await;
                                first_chunk = false;
                            }
                            if _audio_chunks_sent.is_multiple_of(100) {
                                info!("📊 已处理 {} 个音频块", _audio_chunks_sent);
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            info!("📪 TTS音频广播通道已关闭，音频处理任务退出: session={}", session_id);

                            // 仅当判断整轮确实应当结束时，才注入 turn-final；否则不提前收尾，交由后续音频继续
                            let is_turn_finish_expected = {
                                let q = sentence_queue.lock().await;
                                q.is_llm_complete() && !q.has_pending() && !q.has_inflight() && !q.is_processing()
                            };
                            if is_turn_finish_expected {
                                let response_id_for_final = current_turn_response_id_for_audio_task
                                    .load()
                                    .or_else(|| {
                                        if !current_response_id.is_empty() {
                                            Some(current_response_id.clone())
                                        } else {
                                            None
                                        }
                                    });
                                if !turn_final_injected.swap(true, Ordering::AcqRel) {
                                    // 🆕 关键修复：只有在未打断的情况下才注入turn-final
                                    if !audio_sending_stopped.load(Ordering::Acquire) {
                                        if let Some(resp_id) = response_id_for_final {
                                            let assistant_id_for_final = if !current_assistant_item_id.is_empty() {
                                                current_assistant_item_id.clone()
                                            } else {
                                                assistant_context
                                                    .get_context_copy()
                                                    .map(|c| c.assistant_item_id)
                                                    .unwrap_or_else(|| format!("asst_{}", nanoid::nanoid!(6)))
                                            };
                                            let final_chunk = PacedAudioChunk {
                                                audio_data: Bytes::new(),
                                                is_final: true,
                                                realtime_metadata: Some(RealtimeAudioMetadata {
                                                    response_id: resp_id,
                                                    assistant_item_id: assistant_id_for_final,
                                                    output_index: 0,
                                                    content_index: 0,
                                                }),
                                                sentence_text: None,
                                                turn_final: true,
                                            };
                                            let sender_guard = session_audio_sender.lock().await;
                                            let _ = sender_guard.send_audio(final_chunk).await;
                                            drop(sender_guard);
                                            info!("🏁 收到通道关闭信号，注入turn-final完成整轮: session={}", session_id);
                                        }
                                    } else {
                                        info!("🚫 [通道关闭收尾] 此时已打断，跳过注入turn-final: session={}", session_id);
                                    }
                                }
                            } else {
                                info!("ℹ️ 广播关闭但回合未完成，跳过提前收尾，等待后续音频继续: session={}", session_id);
                            }

                            break;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_n)) => {
                            continue;
                        }
                    }
                }
                interrupt_event = interrupt_handler.wait_for_interrupt() => {
                    if let Some(_event) = interrupt_event {
                        if let Some(tts_controller_arc) = tts_controller_weak.upgrade() {
                            let _ = tts_controller_arc.interrupt_session().await;
                        }
                        {
                            let sender_guard = session_audio_sender.lock().await;
                            let _ = sender_guard.force_clear_buffer().await;
                        }
                        let mut _drained = 0usize;
                        loop {
                            match audio_rx.try_recv() {
                                Ok(_stale) => { _drained += 1; continue; },
                                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_n)) => continue,
                                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
                            }
                        }
                        current_response_id.clear();
                        current_assistant_item_id.clear();
                        _audio_chunks_sent = 0;
                        first_chunk = true;
                        info!("✅ 音频处理任务打断处理完成，状态已重置");
                    }
                }
            }
        }

        info!("🔚 会话级别音频处理任务结束: session={}", session_id);
    }
}
