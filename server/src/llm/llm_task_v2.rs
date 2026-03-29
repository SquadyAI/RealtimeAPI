//! Intent-based LLM streaming task (V2)
//!
//! This module implements the new two-layer dispatch system:
//! 1. Intent Recognition (via external API)
//! 2. Agent Dispatch (Reminder, Fallback, etc.)
//!
//! It retains full feature parity with the original `llm_task.rs` including:
//! - Token Aggregation for TTS
//! - Emoji Delivery Gate
//! - Simultaneous Interpretation Mode
//! - Robust Tool Execution (Built-in > MCP > External)
//! - Context Management & Rollback

use anyhow::Result;
use futures::StreamExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info};

use crate::agents::{
    Agent,
    AgentCancelToken,
    AgentCancellationToken,
    AgentContext,
    AgentExtra,
    AgentHandles,
    AgentRegistry,
    AudioRecorderAgent,
    BroadcastAgentTtsSink,
    CameraPhotoAgent,
    CameraVideoAgent,
    DeviceControlAgent,
    FallbackAgent,
    GoodbyeAgent,
    MusicControlAgent,
    NavigationAgent,
    PhotoAgent,
    RejectionAgent,
    ReminderAgent,
    RuntimeToolClient,
    SearchAgent,
    TranslateAgent,
    VolumeControlAgent,
    VolumeDownAgent,
    VolumeUpAgent,
    stream_utils::merge_tool_call_delta,
    // media_agent::MediaAgent,  // MediaAgent 暂时禁用
    turn_tracker,
};
use crate::llm::llm::{ChatCompletionParams, ChatMessage, LlmClient};
use crate::mcp::client::McpClientWrapper;
use crate::rpc::session_data_integration;
use crate::rpc::{EventEmitter, IntentClient, IntentResult, SharedFlags, TurnContext};
use crate::rpc::{SimpleInterruptHandler, SimpleInterruptManager};
use crate::tts::minimax::lang::{LINGUA_MIN_CONFIDENCE, LINGUA_MIN_MARGIN, detect_language_boost};

/// Default emoji prompt (copied from llm_task.rs)
const DEFAULT_EMOJI_PROMPT: &str = r#"<emojiSelector>
<role>表情选择助手，根据用户最新一句话从候选表情中选择最契合当前语义或情绪的emoji</role>

<output>
- 仅输出单个emoji字符
- 必须从候选列表中选择
- 不要输出任何文字、标点或解释
</output>

<matchingRules>
- 根据label语义匹配，不要根据表情外观判断
- 默认选择 Calm，仅在明确匹配下列情绪/场景时选择对应emoji：
  * Angry (😠): 用户表达愤怒、生气、不满情绪
  * Sad (😔): 用户表达悲伤、沮丧、失落情绪
  * Doubt (🤔): 用户表达疑惑、质疑、不确定
  * Happy (😊): 用户表达开心、高兴、兴奋情绪
  * Comfort (🤗): 用户需要安慰、鼓励时
  * Story (📖): 当且仅当用户明确说"讲故事"、"给我讲个故事"、"说个故事"等直接请求故事
  * Calm (😌): 所有其他情况（普通提问、闲聊、打招呼、识图请求、算数提问、功能调用等）
</matchingRules>

<allowedEmojis>
😌 : Calm
😠 : Angry
😔 : Sad
🤔 : Doubt
😊 : Happy
🤗 : Comfort
📖 : Story
</allowedEmojis>
</emojiSelector>"#;

/// 当 enable_search=false 时需要重路由到 FallbackAgent 的搜索相关 intent
/// 注意：agent.information.weather 不在此列表中，因为天气有专门的工具
const SEARCH_INTENTS_REROUTE_TO_FALLBACK: &[&str] = &[
    "agent.finance.stock",
    "agent.search.query",
    "agent.qa.domain",
    "agent.information.currency",
    "agent.information.date",
    "agent.information.legal",
    "agent.information.movie",
    "agent.information.news",
    "agent.datetime.query",
    "agent.information.time",
];

/// Token Aggregator for TTS streaming optimization
#[derive(Debug, Clone)]
pub struct TokenAggregator {
    buffer: String,
    token_count: usize,
    threshold: usize,
    content_index: u32,
}

impl TokenAggregator {
    pub fn new(threshold: usize) -> Self {
        Self { buffer: String::new(), token_count: 0, threshold, content_index: 0 }
    }

    pub fn add_token(&mut self, token: &str) -> Option<(u32, String)> {
        self.buffer.push_str(token);
        self.token_count += 1;
        if self.token_count >= self.threshold { self.flush_internal() } else { None }
    }

    pub fn flush(&mut self) -> Option<(u32, String)> {
        if !self.buffer.is_empty() { self.flush_internal() } else { None }
    }

    fn flush_internal(&mut self) -> Option<(u32, String)> {
        if self.buffer.is_empty() {
            return None;
        }
        let content = self.buffer.clone();
        let index = self.content_index;
        self.buffer.clear();
        self.token_count = 0;
        self.content_index += 1;
        Some((index, content))
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
        self.token_count = 0;
        self.content_index = 0;
    }
}

/// 将语言代码映射到 Lingua 检测返回的语言名（用于双向同传语言匹配）
/// 使用统一的 lang 模块，支持 32 种语言
fn lang_code_to_lingua_name(code: &str) -> String {
    crate::lang::get_lingua_name(code)
}

/// 根据语言A和语言B生成双向同传示例
/// 使用统一的 lang 模块，支持 32 种语言
fn get_simul_interpret_example(lang_a: &str, lang_b: &str) -> String {
    crate::lang::get_simul_interpret_example(lang_a, lang_b)
}

pub struct LlmTaskV2 {
    pub session_id: String,
    pub llm_client: Arc<LlmClient>,
    pub emitter: Arc<EventEmitter>,
    pub llm_params: Option<ChatCompletionParams>,
    pub rx: mpsc::UnboundedReceiver<(TurnContext, String)>,
    pub tts_tx: broadcast::Sender<(TurnContext, String)>,
    pub mcp_clients: Arc<Vec<McpClientWrapper>>,
    pub shared_flags: Arc<SharedFlags>,
    pub enable_search: Arc<AtomicBool>,
    pub simple_interrupt_manager: Arc<SimpleInterruptManager>,
    pub simple_interrupt_handler: Option<SimpleInterruptHandler>,

    // V2 Specific - 意图识别为核心特性
    pub intent_client: Arc<IntentClient>,
    pub agent_registry: Arc<AgentRegistry>,
}

impl LlmTaskV2 {
    fn build_agent_registry() -> Arc<AgentRegistry> {
        let mut registry = AgentRegistry::new();

        // Reminder Agent
        let reminder_agent = Arc::new(ReminderAgent::new());
        registry.register_with_intents(
            reminder_agent,
            &[
                "agent.reminder.set",
                "agent.reminder.query",
                "agent.reminder.remove",
                "agent.calendar.set",
            ],
        );

        // Media Agent: 暂时禁用，路由到 FallbackAgent
        // let media_agent = Arc::new(MediaAgent::new());
        // registry.register_with_intents(media_agent.clone(), media_agent.intents().as_slice());

        // Navigation Agent: agent.navigation.direction
        let navigation_agent = Arc::new(NavigationAgent::new());
        registry.register_with_intents(navigation_agent, &["agent.navigation.direction"]);

        // Rejection Agent
        let rejection_agent = Arc::new(RejectionAgent::new());
        registry.register_with_intents(
            rejection_agent,
            &["agent.rejection.response", "agent.list.remove", "agent.list.set"],
        );

        // Volume Control Agent
        let volume_agent = Arc::new(VolumeControlAgent::new());
        registry.register_with_intents(volume_agent, &["agent.volume.mute", "agent.volume.qa"]);

        // Volume Up Agent
        let volume_up_agent = Arc::new(VolumeUpAgent::new());
        registry.register_with_intents(volume_up_agent, &["agent.volume.up"]);

        // Volume Down Agent
        let volume_down_agent = Arc::new(VolumeDownAgent::new());
        registry.register_with_intents(volume_down_agent, &["agent.volume.down"]);

        // Photo Agent: 视觉问答（需要 LLM 判断）
        let photo_agent = Arc::new(PhotoAgent::new());
        registry.register_with_intents(photo_agent.clone(), photo_agent.intents().as_slice());

        // Camera Photo Agent: 拍照指令（直接调用工具，不需要 LLM）
        let camera_photo_agent = Arc::new(CameraPhotoAgent::new());
        registry.register_with_intents(camera_photo_agent, &["device.camera.photo"]);

        // Camera Video Agent: 录像指令
        let camera_video_agent = Arc::new(CameraVideoAgent::new());
        registry.register_with_intents(camera_video_agent, &["device.camera.video"]);

        // Audio Recorder Agent: 录音指令
        let audio_recorder_agent = Arc::new(AudioRecorderAgent::new());
        registry.register_with_intents(audio_recorder_agent, &["device.recorder.audio"]);

        // Music Control Agent: 音乐播放设置 (循环/随机等)
        let music_control_agent = Arc::new(MusicControlAgent::new());
        registry.register_with_intents(music_control_agent, &["agent.music.setting"]);

        // Goodbye Agent
        let goodbye_agent = Arc::new(GoodbyeAgent::new());
        registry.register_with_intents(goodbye_agent, &["agent.conversation.end"]);

        // Device Control Agent
        let device_control_agent = Arc::new(DeviceControlAgent::new());
        registry.register_with_intents(device_control_agent, &["agent.device.battery"]);

        // Search Agent: 搜索和信息查询相关意图
        let search_agent = Arc::new(SearchAgent::new());
        registry.register_with_intents(
            search_agent,
            &[
                "agent.finance.stock",
                "agent.search.query",
                "agent.qa.domain",
                "agent.information.currency",
                "agent.information.date",
                "agent.information.legal",
                "agent.information.movie",
                "agent.information.news",
                "agent.information.weather",
            ],
        );

        // Translate Agent: 翻译和同声传译
        let translate_agent = Arc::new(TranslateAgent::new());
        registry.register_with_intents(translate_agent, &["agent.language.translate"]);

        // 创建并注册 FallbackAgent 作为兜底机制
        let fallback_agent = Arc::new(FallbackAgent::new());

        // 注册明确路由到 FallbackAgent 的意图
        registry.register_with_intents(
            fallback_agent.clone(),
            &[
                "agent.datetime.convert",
                "agent.datetime.query",
                "agent.information.time",
                "agent.information.cooking",
                "agent.calculation.math",
                "agent.creative.content",
                "agent.general.greet",
                "agent.general.joke",
                "agent.daily.qa",
                // MediaAgent intents 暂时路由到 FallbackAgent
                "agent.media",
                "agent.media.music",
                "agent.music.play",
                "agent.music.query",
            ],
        );

        // 确保 FallbackAgent 在 registry 中作为兜底存在
        registry.ensure_fallback_agent(fallback_agent);

        Arc::new(registry)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: String,
        llm_client: Arc<LlmClient>,
        emitter: Arc<EventEmitter>,
        llm_params: Option<ChatCompletionParams>,
        rx: mpsc::UnboundedReceiver<(TurnContext, String)>,
        tts_tx: broadcast::Sender<(TurnContext, String)>,
        mcp_clients: Vec<McpClientWrapper>,
        shared_flags: Arc<SharedFlags>,
        enable_search: Arc<AtomicBool>,
        simple_interrupt_manager: Arc<SimpleInterruptManager>,
    ) -> Self {
        let intent_api_url = std::env::var("INTENT_API_URL").unwrap_or_else(|_| "http://localhost/intent".to_string());
        let intent_client = Arc::new(IntentClient::new(&intent_api_url));
        let mcp_clients = Arc::new(mcp_clients);
        let agent_registry = Self::build_agent_registry();

        Self {
            session_id,
            llm_client,
            emitter,
            llm_params,
            rx,
            tts_tx,
            mcp_clients,
            shared_flags,
            enable_search,
            simple_interrupt_manager,
            simple_interrupt_handler: None,
            intent_client,
            agent_registry,
        }
    }

    async fn wait_for_tts_receiver_ready(&self) {
        use std::time::Instant;
        let start = Instant::now();
        if self.tts_tx.receiver_count() > 0 {
            return;
        }
        while self.tts_tx.receiver_count() == 0 {
            if start.elapsed().as_millis() > 30 {
                break;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }

    fn spawn_emoji_response(&self, turn_ctx: TurnContext, user_text: String) {
        let llm_client = self.llm_client.clone();
        let emitter = self.emitter.clone();
        let shared_flags = self.shared_flags.clone();
        tokio::spawn(async move {
            let emoji_prompt_text = {
                let guard = shared_flags.emoji_prompt.lock().unwrap();
                match guard.as_ref() {
                    Some(prompt) if !prompt.trim().is_empty() => prompt.clone(),
                    _ => DEFAULT_EMOJI_PROMPT.to_string(),
                }
            };
            let choices = crate::extract_emojis_from_text(&emoji_prompt_text);
            if choices.is_empty() {
                return;
            }
            let selector_config = crate::rpc::SelectorConfig {
                api_key: llm_client.api_key.clone(),
                base_url: llm_client.base_url.clone(),
                model: llm_client.model.clone(),
                timeout_secs: llm_client.timeout_secs,
            };
            if let Ok(selector) = crate::rpc::GuidedChoiceSelector::new(selector_config) {
                let messages = vec![ChatMessage {
                    role: Some("user".to_string()),
                    content: Some(user_text),
                    tool_call_id: None,
                    tool_calls: None,
                }];
                if let Ok(emoji) = selector.select(&emoji_prompt_text, &choices, messages).await {
                    let delta = format!("({})", emoji);
                    emitter.response_text_delta(&turn_ctx, 0, &delta).await;
                }
            }
        });
    }

    /// 获取搜索不可用的多语言提示消息
    /// 要求 LLM 在回答前先提醒用户信息可能不是最新的，然后给出具体内容
    fn get_search_disabled_notice(language: Option<&str>) -> String {
        let lang = language.and_then(|l| l.split('-').next()).unwrap_or("en");
        let notice = match lang {
            "zh" => "回答前先说'信息可能不是最新的，如需最新信息请打开联网搜索'，然后给出你知道的大概数据，不要说不知道",
            "ja" => {
                "回答前にまず「情報が最新でない可能性があります。最新情報が必要な場合はオンライン検索を有効にしてください」と言い、その後あなたの知っている大まかなデータを提供してください。知らないとは言わないでください"
            },
            "ko" => "답변 전에 먼저 '정보가 최신이 아닐 수 있습니다. 최신 정보가 필요하면 온라인 검색을 켜주세요'라고 말하고, 그런 다음 당신이 아는 대략적인 데이터를 제공하세요. 모른다고 하지 마세요",
            "es" => {
                "Antes de responder, primero di 'la información puede no estar actualizada, para información actualizada activa la búsqueda en línea', luego da los datos aproximados que conoces. No digas que no sabes"
            },
            "it" => {
                "Prima di rispondere, dì 'le informazioni potrebbero non essere aggiornate, per informazioni aggiornate attiva la ricerca online', poi fornisci i dati approssimativi che conosci. Non dire che non sai"
            },
            "fr" => {
                "Avant de répondre, dites d'abord 'les informations peuvent ne pas être à jour, pour des informations actualisées activez la recherche en ligne', puis donnez les données approximatives que vous connaissez. Ne dites pas que vous ne savez pas"
            },
            "de" => {
                "Bevor Sie antworten, sagen Sie zuerst 'die Informationen sind möglicherweise nicht aktuell, für aktuelle Informationen aktivieren Sie die Online-Suche', dann geben Sie die ungefähren Daten an, die Sie kennen. Sagen Sie nicht, dass Sie es nicht wissen"
            },
            "ru" => {
                "Перед ответом сначала скажите 'информация может быть неактуальной, для актуальной информации включите онлайн-поиск', затем предоставьте примерные данные, которые вы знаете. Не говорите, что не знаете"
            },
            "th" => "ก่อนตอบ ให้บอกก่อนว่า 'ข้อมูลอาจไม่ใช่ล่าสุด หากต้องการข้อมูลล่าสุดกรุณาเปิดการค้นหาออนไลน์' แล้วให้ข้อมูลคร่าวๆ ที่คุณรู้ อย่าบอกว่าไม่รู้",
            _ => {
                "Before answering, first say 'the information may not be current, for the latest information please enable online search', then provide the approximate data you know. Do not say you don't know"
            },
        };
        notice.to_string()
    }

    async fn get_user_ip_and_city(&self) -> (Option<String>, Option<String>, Option<String>) {
        if let Some(session_manager) = crate::rpc::GlobalSessionManager::get()
            && let Some((_, user_location, asr_language, _, conn_metadata)) = session_manager.get_session_metadata(&self.session_id).await
        {
            let user_ip = conn_metadata.as_ref().and_then(|m| m.client_ip_str());
            let user_city = if let Some(loc) = user_location {
                Some(loc)
            } else if let Some(conn_meta) = &conn_metadata {
                if let (Some(ip), Some(locator)) = (conn_meta.client_ip.clone(), crate::ip_geolocation::get_ip_geolocation_service()) {
                    if let Ok(geo) = locator.lookup_with_language(&ip, asr_language.as_deref()) {
                        geo.city.or(geo.region).or(geo.country)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            return (user_ip, user_city, asr_language);
        }
        (None, None, None)
    }

    /// Returns (user_now_with_weekday, timezone_offset)
    /// user_now_with_weekday 格式: "2026-01-26 21:49:23 星期一"
    async fn compute_user_now_and_timezone(&self) -> (Option<String>, Option<String>) {
        let (time_with_weekday, timezone_offset, _) = crate::llm::llm::get_timezone_and_location_info_from_ip(&self.session_id).await;
        // 保留完整的时间字符串（包含星期几）
        (Some(time_with_weekday), Some(timezone_offset))
    }

    fn matches_goodbye_keywords(&self, text: &str) -> bool {
        let lower = text.to_lowercase();
        // 去除标点符号后匹配，支持 "Exit, please." 等带标点的变体
        let cleaned: String = lower.chars().map(|c| if c.is_ascii_punctuation() { ' ' } else { c }).collect();
        let normalized = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
        lower.contains("退下") || lower.contains("退一下") || normalized.contains("exit please")
    }

    /// 关键词匹配，返回 Intent 字符串（而非 Command）
    /// 匹配成功后直接路由到对应 Agent，跳过 Intent API
    fn match_simple_command_keywords(&self, text: &str) -> Option<&'static str> {
        let trimmed = text.trim();
        // 去除末尾标点符号，使 "录音." "录音。" 等能匹配 "录音"
        let cleaned = trimmed.trim_end_matches(['.', '。', '!', '！', '?', '？', ',', '，', '、', '~', '～']);
        let lower = cleaned.to_lowercase();
        match cleaned {
            // 拍照 → CameraPhotoAgent
            "拍照" | "帮我拍照" | "拍一张" | "拍张照" | "来一张" | "拍个照" | "拍张照片" | "帮我拍下来" | "拍下来" | "帮我拍一个" | "打开拍照" | "把他拍下来" | "把前面拍下来" | "拍一拍"
            | "拍一下这个" => Some("device.camera.photo"),

            // 录像 → CameraVideoAgent
            "录像"
            | "帮我录像"
            | "录个像"
            | "拍视频"
            | "录视频"
            | "打开录像"
            | "打开录像儿"
            | "启动录像功能"
            | "我要开始录像了"
            | "打开录像模式"
            | "切换到录像状态"
            | "开启视频录制"
            | "调用录像功能"
            | "进入录像界面"
            | "我想录像，帮我打开"
            | "准备录像，打开它"
            | "开始录像" => Some("device.camera.video"),

            // 录音 → AudioRecorderAgent
            "录音"
            | "帮我录音"
            | "录个音"
            | "打开录音"
            | "启动录音功能"
            | "开始录音"
            | "帮我打开录音机"
            | "进入录音模式"
            | "调用录音功能"
            | "我要录音了，打开它"
            | "开启录音模式"
            | "切换到录音状态"
            | "打开录音机应用" => Some("device.recorder.audio"),

            // 播放/停止/上一首/下一首 → MusicControlAgent
            "播放" | "放音乐" | "播放音乐" => Some("agent.music.setting"),
            "停止" | "停止播放" | "暂停" | "暂停音乐" => Some("agent.music.setting"),
            "上一首" | "上一曲" | "上一集" | "帮我播放上一首" | "前一首" | "前一曲" | "前一曲啊" => Some("agent.music.setting"),
            "下一首" | "下一曲" | "下一集" | "切歌" | "帮我切歌" | "换一首" | "换首歌" | "下一曲啊" | "切割" | "帮我切割" => Some("agent.music.setting"),

            // 调高音量 → VolumeUpAgent
            "帮我调高音量"
            | "声音大点"
            | "音量大点"
            | "调大声音"
            | "大声点"
            | "声音大点儿"
            | "声音大点儿嗯"
            | "声音大一点"
            | "音乐声大点"
            | "音量调大"
            | "声音放大点"
            | "调高音乐"
            | "音乐音量加一点"
            | "声音再大些"
            | "调大音乐声"
            | "音量往上调"
            | "音乐大点声"
            | "放大音量" => Some("agent.volume.up"),

            // 调低音量 → VolumeDownAgent
            "帮我调低音量"
            | "声音小点"
            | "音量小点"
            | "调小声音"
            | "小声点"
            | "声音小点儿"
            | "声音小点儿嗯"
            | "声音小一点"
            | "音量调小点儿"
            | "音乐声关小些"
            | "声音减小一点"
            | "调低音量"
            | "音乐音量往小了调"
            | "声音再小点儿"
            | "调小音乐声"
            | "音量降一级"
            | "减小当前音量"
            | "音乐声音太大，关小" => Some("agent.volume.down"),

            _ => match lower.as_str() {
                // 英文关键词（大小写不敏感）
                "take photo" | "take picture" | "take a photo" | "take a picture" => Some("device.camera.photo"),
                "record video" | "take video" | "take a video" => Some("device.camera.video"),
                "record audio" | "take a record" => Some("device.recorder.audio"),
                "play music" => Some("agent.music.setting"),
                "stop music" | "stop playing" => Some("agent.music.setting"),
                "previous song" | "last song" => Some("agent.music.setting"),
                "next song" | "skip song" => Some("agent.music.setting"),
                "volume up" | "turn up volume" | "increase volume" => Some("agent.volume.up"),
                "volume down" | "turn down volume" | "decrease volume" => Some("agent.volume.down"),
                _ => None,
            },
        }
    }

    async fn handle_keyword_triggered_goodbye(&self, ctx: &TurnContext, _user_text: String) -> Result<()> {
        // 🎯 中心化：轮次已在 handle_turn 开始时创建，这里只需设置意图和 agent
        turn_tracker::set_intent(&self.session_id, Some("agent.conversation.end".to_string())).await;
        turn_tracker::set_agent(&self.session_id, "keyword_goodbye").await;

        // 根据 asr_language 选择回复语言
        let (_, _, asr_language) = self.get_user_ip_and_city().await;
        let goodbye_text = match asr_language.as_deref() {
            Some(lang) if lang.starts_with("zh") => "😊 好的，再见！祝你有美好的一天！",
            Some(lang) if lang.starts_with("ja") => "😊 はい、さようなら！良い一日を！",
            Some(lang) if lang.starts_with("ko") => "😊 네, 안녕히 가세요! 좋은 하루 되세요!",
            Some(lang) if lang.starts_with("it") => "😊 Va bene, arrivederci! Buona giornata!",
            Some(lang) if lang.starts_with("es") => "😊 Está bien, ¡adiós! ¡Que tengas un buen día!",
            _ => "😊 Okay, goodbye! Have a great day!", // 英文及其他语言 fallback
        };

        self.emitter
            .conversation_item_created(&ctx.assistant_item_id, "assistant", "in_progress", Some(&ctx.user_item_id))
            .await;
        self.emitter.response_created(ctx).await;
        self.emitter.response_output_item_added(ctx).await;

        self.wait_for_tts_receiver_ready().await;
        let filtered = crate::text_filters::filter_for_tts(goodbye_text, *self.shared_flags.tts_chinese_convert_mode.read().unwrap());
        let _ = self.tts_tx.send((ctx.clone(), filtered));
        let _ = self.tts_tx.send((ctx.clone(), "__TURN_COMPLETE__".to_string()));

        // 🎯 中心化：添加回复消息并完成轮次
        turn_tracker::add_assistant_message(&self.session_id, goodbye_text).await;
        turn_tracker::finish_turn(&self.session_id).await;

        // 发送完整的 say_goodbye function call 事件序列，通知客户端关闭会话
        let call_id = format!("keyword_goodbye_{}", nanoid::nanoid!(6));
        self.emitter
            .response_function_call_arguments_done(ctx, &call_id, "say_goodbye", "{}")
            .await;
        self.emitter
            .response_function_call_result_done(ctx, &call_id, r#"{"ok": true}"#)
            .await;

        Ok(())
    }

    /// 同声传译：双向互译，自动检测用户输入语言并翻译成另一种语言
    async fn run_simul_interpret(&self, ctx: &TurnContext, user_text: String, cancel_token: Option<&AgentCancellationToken>) -> Result<()> {
        let (lang_a, lang_b) = {
            let a = self.shared_flags.simul_interpret_language_a.lock().unwrap().clone();
            let b = self.shared_flags.simul_interpret_language_b.lock().unwrap().clone();
            (a, b)
        };

        // 检测用户输入的语言，决定翻译方向
        // Lingua 返回的是语言名（如 "English", "Chinese"），需要与语言代码（如 "en", "zh"）匹配
        let detected_lang = detect_language_boost(&user_text, LINGUA_MIN_CONFIDENCE, LINGUA_MIN_MARGIN);
        let (source_lang, target_lang) = match detected_lang.as_deref() {
            Some(detected) => {
                let detected_lower = detected.to_lowercase();
                // 将用户配置的语言代码转换为 Lingua 语言名进行比较
                let lang_a_lingua = lang_code_to_lingua_name(&lang_a);
                let lang_b_lingua = lang_code_to_lingua_name(&lang_b);

                // 如果检测到的语言匹配 lang_a，则翻译成 lang_b；反之亦然
                if detected_lower == lang_a_lingua {
                    (lang_a.clone(), lang_b.clone())
                } else if detected_lower == lang_b_lingua {
                    (lang_b.clone(), lang_a.clone())
                } else {
                    // 检测到的语言不在配置的语言对中，默认 lang_a -> lang_b
                    info!(
                        "🌐 检测到语言 {} 不在配置的语言对 ({}/{}, {}/{}) 中，使用默认方向",
                        detected, lang_a, lang_a_lingua, lang_b, lang_b_lingua
                    );
                    (lang_a.clone(), lang_b.clone())
                }
            },
            None => {
                // 检测失败，默认 lang_a -> lang_b
                info!("🌐 语言检测失败，使用默认翻译方向: {} -> {}", lang_a, lang_b);
                (lang_a.clone(), lang_b.clone())
            },
        };

        info!(
            "🟣 SimulInterpret Loop: session={}, detected={:?}, src={}, tgt={}",
            self.session_id, detected_lang, source_lang, target_lang
        );

        // 打断检查
        if let Some(token) = cancel_token
            && token.is_cancelled()
        {
            debug!("SimulInterpret cancelled before start");
            return Ok(());
        }

        // Send standard events
        self.emitter
            .conversation_item_created(&ctx.assistant_item_id, "assistant", "in_progress", Some(&ctx.user_item_id))
            .await;
        self.emitter.response_created(ctx).await;
        self.emitter.response_output_item_added(ctx).await;

        self.spawn_emoji_response(ctx.clone(), user_text.clone());

        let example = get_simul_interpret_example(&source_lang, &target_lang);
        let simul_system_prompt = format!(
            "Simultaneous interpretation: {src} → {tgt}\n\nRULES:\n1. Translate EVERY input from {src} to {tgt}\n2. NEVER echo the original text - ALWAYS translate\n3. NEVER answer or respond - ONLY translate\n4. Keep the translation literal and complete\n\nExample:\n{example}\n\nExit: Call stop_simul_interpret when user says 退出/不要翻译/停止翻译/stop/don't translate",
            src = source_lang,
            tgt = target_lang,
            example = example
        );

        let messages = vec![
            crate::llm::llm::ChatMessage {
                role: Some("system".to_string()),
                content: Some(simul_system_prompt),
                tool_call_id: None,
                tool_calls: None,
            },
            crate::llm::llm::ChatMessage {
                role: Some("user".to_string()),
                content: Some(user_text.clone()),
                tool_call_id: None,
                tool_calls: None,
            },
        ];

        let mut bare_params = self.llm_params.clone().unwrap_or_default();
        // 同传模式下只保留 stop_simul_interpret 工具，移除所有其他工具
        let stop_tool: Vec<crate::llm::llm::Tool> = crate::function_callback::create_simul_tools()
            .into_iter()
            .filter(|t| t.function.name == "stop_simul_interpret")
            .collect();
        bare_params.tools = Some(stop_tool);
        bare_params.tool_choice = Some(crate::llm::llm::ToolChoice::auto());

        match self.llm_client.chat_stream(messages, Some(bare_params)).await {
            Ok(stream) => {
                let mut stream = Box::pin(stream);
                let mut accumulated_tool_calls: Vec<crate::llm::llm::ToolCall> = Vec::new();
                let mut stop_detected = false;
                let mut stop_tool_id: Option<String> = None;
                let mut accumulated_translation = String::new();
                let mut was_cancelled = false;

                // 使用 tokio::select! 实现真正的 next-token 打断
                loop {
                    // 创建打断 future（如果有 cancel_token）
                    let cancel_fut = async {
                        if let Some(token) = cancel_token {
                            token.cancelled().await;
                        } else {
                            // 没有 cancel_token 时，永远等待
                            std::future::pending::<()>().await;
                        }
                    };

                    tokio::select! {
                        biased; // 优先检查打断分支

                        // 打断分支：与 stream.next() 真正并发
                        _ = cancel_fut => {
                            debug!("🛑 SimulInterpret interrupted at token boundary");
                            was_cancelled = true;
                            break;
                        }

                        // Token 分支
                        maybe_item = stream.next() => {
                            match maybe_item {
                                Some(Ok(choice)) => {
                                    if let Some(delta) = &choice.delta {
                                        if let Some(text) = &delta.content
                                            && !text.is_empty() {
                                                accumulated_translation.push_str(text);
                                                let filtered = crate::text_filters::filter_for_tts(text, *self.shared_flags.tts_chinese_convert_mode.read().unwrap());
                                                let _ = self.tts_tx.send((ctx.clone(), filtered));
                                            }
                                        if let Some(tc_delta) = &delta.tool_calls {
                                            for d in tc_delta {
                                                merge_tool_call_delta(&mut accumulated_tool_calls, d);
                                                if d.function.name.as_deref() == Some("stop_simul_interpret") {
                                                    stop_detected = true;
                                                    if let Some(id) = d.id.as_deref() {
                                                        stop_tool_id = Some(id.to_string());
                                                    }
                                                }
                                            }
                                        }
                                    } else if let Some(message) = &choice.message {
                                        if let Some(text) = &message.content
                                            && !text.is_empty() {
                                                accumulated_translation.push_str(text);
                                                let filtered = crate::text_filters::filter_for_tts(text, *self.shared_flags.tts_chinese_convert_mode.read().unwrap());
                                                let _ = self.tts_tx.send((ctx.clone(), filtered));
                                            }
                                        if let Some(tcs) = &message.tool_calls {
                                            for tc in tcs {
                                                accumulated_tool_calls.push(tc.clone());
                                                if tc.function.name.as_deref() == Some("stop_simul_interpret") {
                                                    stop_detected = true;
                                                    if let Some(id) = tc.id.as_deref() {
                                                        stop_tool_id = Some(id.to_string());
                                                    }
                                                }
                                            }
                                        }
                                    }
                                },
                                Some(Err(e)) => {
                                    error!("SimulInterpret stream error: {}", e);
                                    break;
                                },
                                None => break, // Stream 结束
                            }
                        }
                    }
                }

                // 如果被打断，直接返回
                if was_cancelled {
                    return Ok(());
                }

                // Note: response.audio.done is now sent by TTS when audio generation is actually complete
                // Note: output_audio_buffer.stopped is now sent by PacedSender when audio playback is actually complete

                if stop_detected {
                    let mut args_json = "{}".to_string();
                    if let Some(tc) = accumulated_tool_calls
                        .iter()
                        .rev()
                        .find(|tc| tc.function.name.as_deref() == Some("stop_simul_interpret"))
                        && let Some(a) = tc.function.arguments.as_ref()
                    {
                        args_json = a.clone();
                    }
                    let tts_text = serde_json::from_str::<serde_json::Value>(&args_json)
                        .ok()
                        .and_then(|v| v.get("tts_text").and_then(|s| s.as_str()).map(|s| s.to_string()))
                        .unwrap_or_else(|| "Exiting simultaneous interpretation mode.".to_string());

                    if let Some(call_id) = stop_tool_id.as_deref() {
                        self.emitter
                            .response_function_call_arguments_done(ctx, call_id, "stop_simul_interpret", &args_json)
                            .await;
                    }

                    self.shared_flags.simul_interpret_enabled.store(false, Ordering::Release);

                    // 截断同传期间的所有 turns（清除同传上下文）
                    let start_count = self.shared_flags.simul_interpret_turn_start_count.load(Ordering::Acquire);
                    turn_tracker::truncate_turns_to(&self.session_id, start_count).await;
                    info!("🧹 同传结束，已截断 turns 到 {} 条", start_count);

                    // 切换回默认 ASR 引擎（同传结束后恢复）
                    {
                        let mut pref = self.shared_flags.preferred_asr_engine.lock().unwrap();
                        *pref = Some("whisperlive".to_string());
                    }
                    let _ = self.shared_flags.asr_engine_notify_tx.send(Some("whisperlive".to_string()));
                    info!("📢 同传结束，已发送 ASR 引擎变更通知: whisperlive");

                    // Direct TTS for exit message
                    self.wait_for_tts_receiver_ready().await;
                    let filtered = crate::text_filters::filter_for_tts(&tts_text, *self.shared_flags.tts_chinese_convert_mode.read().unwrap());
                    let _ = self.tts_tx.send((ctx.clone(), filtered));
                    let _ = self.tts_tx.send((ctx.clone(), "__TURN_COMPLETE__".to_string()));
                } else {
                    let _ = self.tts_tx.send((ctx.clone(), "__TURN_COMPLETE__".to_string()));
                }
            },
            Err(e) => error!("Failed to start simul interpret stream: {}", e),
        }
        Ok(())
    }

    async fn run_agent(&self, agent: Arc<dyn Agent>, agent_context: AgentContext, handles: AgentHandles<'_>) -> Result<()> {
        let turn_ctx = agent_context.turn_context.clone();

        // 🎯 统一打断检查：所有 Agent 入口统一处理
        if handles.cancel.is_cancelled() {
            turn_tracker::interrupt_turn(&self.session_id).await;
            return Ok(());
        }

        info!("🤖 Executing agent: {}", agent.id());

        self.emitter
            .conversation_item_created(
                &turn_ctx.assistant_item_id,
                "assistant",
                "in_progress",
                Some(&turn_ctx.user_item_id),
            )
            .await;
        self.emitter.response_created(&turn_ctx).await;
        self.emitter.response_output_item_added(&turn_ctx).await;

        match agent.run(agent_context, handles).await {
            Ok(()) => {
                // Agent 内部已处理：
                // - TTS 通过 tts_sink 发送
                // - assistant 消息通过 turn_tracker 添加
                // - 打断通过 interrupt_turn 处理

                // 🎯 中心化：完成轮次
                turn_tracker::finish_turn(&self.session_id).await;

                // 退出 agent 执行完成后清空历史，避免污染后续意图识别
                if agent.id() == "agent.conversation.end" {
                    turn_tracker::clear_session(&self.session_id).await;
                    info!("🧹 退出 agent 完成，已清空会话历史: session={}", self.session_id);
                }

                self.signal_turn_complete(&turn_ctx).await;
                Ok(())
            },
            Err(e) => {
                error!("❌ Agent {} failed: {}", agent.id(), e);
                // 🎯 中心化：标记轮次失败
                turn_tracker::interrupt_turn(&self.session_id).await;
                self.finalize_agent_error(&turn_ctx).await
            },
        }
    }

    #[allow(dead_code)]
    fn spawn_metadata_save(&self, response_id: &str, llm_text: &str) {
        if llm_text.trim().is_empty() {
            return;
        }
        let session_id = self.session_id.clone();
        let response_id = response_id.to_string();
        let llm_text = llm_text.to_string();
        tokio::spawn(async move {
            if let Err(e) = session_data_integration::save_conversation_metadata_globally(&session_id, &response_id, &llm_text, true).await {
                error!(
                    "❌ 保存LLM响应数据失败: session_id={}, response_id={}, error={}",
                    session_id, response_id, e
                );
            } else {
                debug!("💾 LLM响应数据保存成功: session_id={}, response_id={}", session_id, response_id);
            }
        });
    }

    async fn finalize_agent_response(&self, _ctx: &TurnContext, _text: &str) -> Result<()> {
        // Note: response.text.done is now sent by PacedSender when all text deltas are complete
        // Note: response.audio.done is now sent by TTS when audio generation is actually complete
        // Note: output_audio_buffer.stopped is now sent by PacedSender when audio playback is actually complete
        Ok(())
    }

    async fn finalize_agent_error(&self, ctx: &TurnContext) -> Result<()> {
        let apology = "抱歉，暂时无法处理该请求。";
        self.wait_for_tts_receiver_ready().await;
        let filtered = crate::text_filters::filter_for_tts(apology, *self.shared_flags.tts_chinese_convert_mode.read().unwrap());
        let _ = self.tts_tx.send((ctx.clone(), filtered));
        let _ = self.tts_tx.send((ctx.clone(), "__TURN_COMPLETE__".to_string()));
        self.finalize_agent_response(ctx, apology).await
    }

    async fn signal_turn_complete(&self, ctx: &TurnContext) {
        let _ = self.tts_tx.send((ctx.clone(), "__TURN_COMPLETE__".to_string()));
    }

    pub async fn run(mut self) -> Result<()> {
        info!("🎯 LlmTaskV2 Started: session={}", self.session_id);

        // 提取 deviceCode 并设置到 turn_tracker（用于判断是否跳过 location 注入等）
        let device_code = self.extract_device_code();
        turn_tracker::set_session_device_code(&self.session_id, device_code).await;

        while let Some((ctx, user_text)) = self.rx.recv().await {
            info!("LLM V2 received: '{}'", user_text);
            if let Err(e) = self.handle_turn(ctx, user_text).await {
                error!("Agent execution failed: {}", e);
            }
        }

        Ok(())
    }

    async fn handle_turn(&self, ctx: TurnContext, user_text: String) -> Result<()> {
        // 🎯 中心化：在 TurnTracker 开始新轮次
        turn_tracker::start_turn(&self.session_id, &ctx.response_id, &user_text).await;

        // 退出关键词（保持不变）
        if self.matches_goodbye_keywords(&user_text) {
            self.handle_keyword_triggered_goodbye(&ctx, user_text).await?;
            return Ok(());
        }

        // Media Agent 锁检查 - 暂时禁用，MediaAgent 已路由到 FallbackAgent
        // let is_media_locked = {
        //     let guard = self.shared_flags.media_agent_lock.lock().unwrap();
        //     guard.is_locked()
        // };
        //
        // if is_media_locked {
        //     info!("🔒 MediaAgent 锁生效，跳过 Intent 识别，直接路由到 MediaAgent");
        //     let interrupt_handler = self.acquire_interrupt_handler();
        //
        //     let media_agent = self
        //         .agent_registry
        //         .get_agent(Some("agent.media"));
        //
        //     // 🎯 中心化：设置意图和 Agent
        //     turn_tracker::set_intent(&self.session_id, Some("agent.media".to_string())).await;
        //     turn_tracker::set_agent(&self.session_id, media_agent.id()).await;
        //
        //     self.wait_for_tts_receiver_ready().await;
        //     let agent_context = self
        //         .build_agent_context(ctx.clone(), user_text.clone(), IntentResult {
        //             intent: Some("agent.media".to_string()),
        //             wiki_context: None,
        //         })
        //         .await;
        //
        //     return self.dispatch_agent(media_agent, agent_context, ctx, interrupt_handler).await;
        // }

        // 同声传译模式检查（保持不变）
        if self.shared_flags.simul_interpret_enabled.load(Ordering::Acquire) {
            // 同声传译模式也支持打断
            let cancel_token = AgentCancellationToken::new();
            let mut interrupt_handler = self.acquire_interrupt_handler();
            let cancel_token_clone = cancel_token.clone();
            let cancel_monitor = tokio::spawn(async move {
                if interrupt_handler.wait_for_interrupt().await.is_some() {
                    cancel_token_clone.cancel();
                }
            });
            let result = self.run_simul_interpret(&ctx, user_text, Some(&cancel_token)).await;
            cancel_monitor.abort();
            return result;
        }

        // ========== 关键词匹配优先 ==========
        let keyword_intent = self.match_simple_command_keywords(&user_text);

        // 决定最终使用的 Intent
        let intent_result = if let Some(intent_str) = keyword_intent {
            // 关键词匹配成功，跳过 Intent API
            info!("🔑 关键词匹配命中: '{}' → {}", user_text, intent_str);
            IntentResult { intent: Some(intent_str.to_string()), wiki_context: None }
        } else {
            // 关键词不匹配，走 Intent API
            // 🎯 中心化：从 TurnTracker 获取历史用于意图识别
            let history = turn_tracker::get_conversation_history(&self.session_id).await;

            // 获取 asr_language 用于 intent 识别（触发中文 wiki 检索）
            let (_, _, asr_language) = self.get_user_ip_and_city().await;
            let intent_future = self.intent_client.recognize(history.as_slice(), asr_language.as_deref());

            // 立即开始emoji请求，不等待intent（仅在非关键词匹配时触发）
            self.spawn_emoji_response(ctx.clone(), user_text.clone());

            intent_future.await
        };

        // 🎯 中心化：记录意图识别结果
        turn_tracker::set_intent(&self.session_id, intent_result.intent.clone()).await;

        let interrupt_handler = self.acquire_interrupt_handler();

        // 检查是否需要重路由搜索 intent 到 FallbackAgent（enable_search=false 时）
        let should_reroute_search = intent_result
            .intent
            .as_deref()
            .map(|i| SEARCH_INTENTS_REROUTE_TO_FALLBACK.contains(&i))
            .unwrap_or(false)
            && !self.enable_search.load(Ordering::Acquire);

        // 使用统一的 get_agent 方法，确保意图识别的必选特性
        // 这体现了 LlmTaskV2 的核心设计理念：所有请求都经过意图路由
        // get_agent 方法内部已包含兜底机制，始终返回有效的 agent
        let selected_agent = if should_reroute_search {
            // 搜索功能未开启，重路由到 FallbackAgent
            info!("🔄 搜索功能未开启，将 intent {:?} 重路由到 FallbackAgent", intent_result.intent);
            self.agent_registry.get_agent(Some("agent.fallback"))
        } else {
            self.agent_registry.get_agent(intent_result.intent.as_deref())
        };

        // 🎯 中心化：记录 Agent 路由结果
        turn_tracker::set_agent(&self.session_id, selected_agent.id()).await;

        self.wait_for_tts_receiver_ready().await;
        let agent_context = self
            .build_agent_context(ctx.clone(), user_text.clone(), intent_result, should_reroute_search)
            .await;

        self.dispatch_agent(selected_agent, agent_context, ctx, interrupt_handler).await
    }

    fn acquire_interrupt_handler(&self) -> SimpleInterruptHandler {
        if let Some(ref handler) = self.simple_interrupt_handler {
            handler.clone()
        } else {
            SimpleInterruptHandler::new(
                self.session_id.clone(),
                "LLM-V2".to_string(),
                self.simple_interrupt_manager.subscribe(),
            )
        }
    }

    async fn build_agent_context(&self, ctx: TurnContext, user_text: String, intent_result: IntentResult, search_disabled_notice: bool) -> AgentContext {
        let (user_ip, user_city, asr_language) = self.get_user_ip_and_city().await;
        let (user_now, user_timezone) = self.compute_user_now_and_timezone().await;
        let agent_extra = AgentExtra {
            user_ip: user_ip.clone(),
            user_city,
            user_timezone,
            asr_language: asr_language.clone(),
            intent_label: intent_result.intent.clone(),
        };

        // 🎯 中心化：从 TurnTracker 读取 system_prompt 和 offline_tools
        let (system_prompt, offline_tools) = {
            let tracker = turn_tracker::get_or_create_tracker(&self.session_id).await;
            let guard = tracker.read().await;
            (guard.system_prompt.clone(), guard.offline_tools.clone())
        };

        // 如果需要注入搜索不可用提示，修改 system_prompt 并同步更新 TurnTracker
        // 去重：检查是否已包含提示词，避免多次请求导致重复追加
        let system_prompt = if search_disabled_notice {
            let notice = Self::get_search_disabled_notice(asr_language.as_deref());
            if system_prompt.as_ref().map(|sp| sp.contains(&notice)).unwrap_or(false) {
                // 已有提示词，不重复追加
                system_prompt
            } else {
                let updated_prompt = system_prompt.map(|sp| format!("{}\n\n{}", sp, notice));
                if let Some(ref prompt) = updated_prompt {
                    turn_tracker::update_system_prompt(&self.session_id, prompt.clone()).await;
                }
                updated_prompt
            }
        } else {
            system_prompt
        };

        // 从 system_prompt 中提取 <role> 部分，用于注入专用 Agent
        let role_prompt = system_prompt
            .as_ref()
            .and_then(|sp| crate::agents::role_extractor::extract_role_from_system_prompt(sp));

        // 合并工具列表：内置工具优先，外部工具补充（去重同名）
        let tools = self.build_merged_tools(&user_text).await;

        AgentContext {
            session_id: self.session_id.clone(),
            user_text,
            user_now,
            turn_context: ctx,
            shared_flags: self.shared_flags.clone(),
            system_prompt,
            role_prompt,
            tools,
            offline_tools,
            extra: agent_extra,
            wiki_context: intent_result.wiki_context,
        }
    }

    /// 从 URL 中提取 deviceCode 参数值
    fn extract_device_code_from_url(url: &str) -> Option<String> {
        url::Url::parse(url).ok().and_then(|parsed| {
            parsed
                .query_pairs()
                .find(|(k, _)| k == "deviceCode")
                .map(|(_, v)| v.to_string())
        })
    }

    /// 从 mcp_clients 中提取 deviceCode（返回第一个找到的）
    fn extract_device_code(&self) -> Option<String> {
        for client in self.mcp_clients.iter() {
            if let Some(device_code) = Self::extract_device_code_from_url(client.endpoint()) {
                return Some(device_code);
            }
        }
        None
    }

    /// 检查是否需要跳过内置 math 和 world_clock 工具（基于 deviceCode）
    fn should_skip_math_and_clock_tools(&self) -> bool {
        // deviceCode=7720, 8105, 7981, 7943 的设备不注入 math 和 world_clock 工具
        const SKIP_BUILTIN_TOOLS_DEVICE_CODES: &[&str] = &["7720", "8105", "7981", "7943"];

        for client in self.mcp_clients.iter() {
            if let Some(device_code) = Self::extract_device_code_from_url(client.endpoint()) {
                if SKIP_BUILTIN_TOOLS_DEVICE_CODES.contains(&device_code.as_str()) {
                    return true;
                }
            }
        }
        false
    }

    /// 合并工具列表：内置工具优先，外部工具补充
    async fn build_merged_tools(&self, user_text: &str) -> Vec<crate::llm::llm::Tool> {
        use std::collections::HashSet;

        let mut tools = Vec::new();
        let mut tool_names: HashSet<String> = HashSet::new();

        // 检查是否需要跳过 math 和 world_clock 工具
        let skip_math_and_clock = self.should_skip_math_and_clock_tools();
        if skip_math_and_clock {
            info!("⏭️ 根据 deviceCode 跳过内置 math 和 world_clock 工具");
        }

        // 1. 内置工具（优先级最高）
        // 搜索工具：根据 enable_search 动态决定
        if self.enable_search.load(std::sync::atomic::Ordering::Acquire) {
            for t in crate::function_callback::create_search_tools() {
                tool_names.insert(t.function.name.clone());
                tools.push(t);
            }
        }

        // 数学计算工具
        if !skip_math_and_clock {
            for t in crate::function_callback::create_math_tools() {
                tool_names.insert(t.function.name.clone());
                tools.push(t);
            }
        }

        // 世界时钟工具
        if !skip_math_and_clock {
            for t in crate::function_callback::create_world_clock_tools() {
                tool_names.insert(t.function.name.clone());
                tools.push(t);
            }
        }

        // 同声传译工具：受标志位与关键词共同控制
        let simul_feature_enabled = crate::env_utils::env_bool_or_default("ENABLE_SIMUL_INTERPRET", true);
        if simul_feature_enabled {
            let simul_on = self
                .shared_flags
                .simul_interpret_enabled
                .load(std::sync::atomic::Ordering::Acquire);
            if simul_on {
                // 同传模式已开启：仅注入 stop_simul_interpret，移除 start
                for t in crate::function_callback::create_simul_tools() {
                    if t.function.name == "stop_simul_interpret" && !tool_names.contains(&t.function.name) {
                        tool_names.insert(t.function.name.clone());
                        tools.push(t);
                    }
                }
                // 确保未意外保留 start（来自外部注入）
                tools.retain(|t| t.function.name.as_str() != "start_simul_interpret");
            } else if Self::contains_translation_keyword(user_text) {
                // 正常模式且命中翻译关键词：仅注入 start_simul_interpret
                for t in crate::function_callback::create_simul_tools() {
                    if t.function.name == "start_simul_interpret" && !tool_names.contains(&t.function.name) {
                        tool_names.insert(t.function.name.clone());
                        tools.push(t);
                    }
                }
                // 确保未意外保留 stop（来自外部注入）
                tools.retain(|t| t.function.name.as_str() != "stop_simul_interpret");
            }
        }

        let builtin_count = tools.len();

        // 2. 外部工具（来自 mcp_server_config / prompt_endpoint）
        let mut llm_params_count = 0;
        if let Some(ref params) = self.llm_params {
            if let Some(ref external_tools) = params.tools {
                for t in external_tools {
                    if !tool_names.contains(&t.function.name) {
                        tool_names.insert(t.function.name.clone());
                        tools.push(t.clone());
                        llm_params_count += 1;
                    }
                }
            }
        }

        // 3. 异步加载的工具（来自 tools_endpoint，存储在 SESSION_TOOLS 缓存）
        let async_tools = crate::mcp::async_tools_manager::get_session_loaded_tools(&self.session_id).await;
        let mut async_tools_count = 0;
        for t in async_tools {
            if !tool_names.contains(&t.function.name) {
                tool_names.insert(t.function.name.clone());
                tools.push(t);
                async_tools_count += 1;
            }
        }

        info!(
            "🔧 合并后的工具列表: {} 个 (内置={}, llm_params={}, tools_endpoint={}) {:?}",
            tools.len(),
            builtin_count,
            llm_params_count,
            async_tools_count,
            tools.iter().map(|t| &t.function.name).collect::<Vec<_>>()
        );
        tools
    }

    /// 检测用户输入是否包含翻译关键词
    fn contains_translation_keyword(text: &str) -> bool {
        let lower = text.to_lowercase();
        lower.contains("翻译")
            || lower.contains("同传")
            || lower.contains("同声传译")
            || lower.contains("translate")
            || lower.contains("interpretation")
            || lower.contains("interpret")
            || lower.contains("翻訳")
            || lower.contains("ほんやく")
            || lower.contains("通訳")
            || lower.contains("つうやく")
            || lower.contains("번역")
            || lower.contains("통역")
            || lower.contains("traduire")
            || lower.contains("traduction")
            || lower.contains("interprétation")
            || lower.contains("übersetzen")
            || lower.contains("übersetzung")
            || lower.contains("dolmetschen")
            || lower.contains("traducir")
            || lower.contains("traducción")
            || lower.contains("interpretar")
    }

    async fn dispatch_agent(&self, agent: Arc<dyn Agent>, agent_context: AgentContext, ctx: TurnContext, interrupt_handler: SimpleInterruptHandler) -> Result<()> {
        let cancel_token = AgentCancellationToken::new();
        let tts_sink = BroadcastAgentTtsSink::new(Arc::new(self.tts_tx.clone()), ctx.clone(), self.shared_flags.clone());
        let tool_client = RuntimeToolClient::new(
            Arc::downgrade(&self.emitter),
            self.mcp_clients.clone(),
            self.session_id.clone(),
            ctx.clone(),
            self.shared_flags.tool_call_manager.clone(),
            cancel_token.clone(),
            agent_context.extra.asr_language.clone(),
        );
        let mut cancel_monitor_handler = interrupt_handler.clone();
        let cancel_token_clone = cancel_token.clone();
        let cancel_monitor = tokio::spawn(async move {
            if cancel_monitor_handler.wait_for_interrupt().await.is_some() {
                cancel_token_clone.cancel();
            }
        });

        let handles = AgentHandles {
            llm_client: Arc::downgrade(&self.llm_client),
            shared_flags: self.shared_flags.clone(),
            enable_search: self.enable_search.clone(),
            mcp_clients: self.mcp_clients.clone(),
            tts_sink: &tts_sink,
            tool_client: &tool_client,
            cancel: &cancel_token,
            interrupt_handler: Some(interrupt_handler),
            emitter: Arc::downgrade(&self.emitter),
            turn_context: ctx.clone(),
        };

        let run_result = self.run_agent(agent, agent_context, handles).await;
        cancel_monitor.abort();
        run_result
    }
}
