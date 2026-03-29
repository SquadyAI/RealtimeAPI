//! ASR streaming task with VAD and transcription event handling
//!
//! 🚀 关键修复：防止VAD模式下的重复completed事件导致客户端麦克风锁定
//!
//! 根本问题：在VAD模式下，由于网络延迟，客户端在收到第一个completed事件后仍会
//! 发送音频数据，如果服务端的VAD超时处理不当重置了has_sent_completed标志，
//! 就会导致这些延迟的音频数据触发第二个completed事件，造成客户端麦克风锁定。
//!
//! 修复策略：
//! 1. VAD超时处理时，检查是否已发送completed，如已发送则完全跳过超时处理
//! 2. has_sent_completed标志只有在确实没有发送completed的情况下才允许重置
//! 3. 保护期间跳过finalize、错误事件发送、会话重置等可能产生副作用的操作
//!
#![allow(dead_code)]

use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};
use unicode_segmentation::UnicodeSegmentation;

use crate::asr::{AsrEngine, AsrResult, SpeechMode};
use crate::rpc::session_router::SessionRouter;
use crate::vad::VadState;

use super::asr_task_core::AsrInputMessage;
use super::event_emitter::EventEmitter;
use super::simple_interrupt_manager::{InterruptReason as SimpleInterruptReason, SimpleInterruptHandler, SimpleInterruptManager}; // 🆕 简化打断管理器
use super::timing_manager::{TimingNode, record_node_time, record_vad_trigger, reset_session_timing}; // 🆕 导入计时管理器
use super::types::{TaskCompletion, TurnContext};

/// Push-To-Talk 事件
#[derive(Debug, Clone, Copy)]
pub enum PttEvent {
    End,
}

// 已迁移至 asr_task_core::AsrInputMessage

/// 接收原始音频数据 Vec<f32> 和 PTT事件（保证顺序）
pub struct AsrTask {
    pub session_id: String,
    pub asr_engine: Arc<AsrEngine>,
    pub emitter: Arc<EventEmitter>,
    pub router: Arc<SessionRouter>,
    pub speech_mode: SpeechMode,
    pub input_rx: mpsc::Receiver<AsrInputMessage>,
    /// 共享标志，用于获取当前 assistant 响应上下文
    pub shared_flags: Arc<super::types::SharedFlags>,
    pub task_completion_tx: mpsc::UnboundedSender<TaskCompletion>,
    /// 🆕 简化的打断管理器
    pub simple_interrupt_manager: Arc<SimpleInterruptManager>,

    /// 🆕 简化的打断处理器
    pub simple_interrupt_handler: Option<SimpleInterruptHandler>,
    /// 🆕 音频段保存通道
    // 🔧 已删除audio_segment_tx，使用新的会话数据持久化系统
    /// 🆕 LLM客户端引用，用于预热连接
    /// 🔧 新增：清理信号接收器
    pub cleanup_rx: mpsc::UnboundedReceiver<()>,
    /// 🆕 ASR语言设置
    pub asr_language: Option<String>,
    /// 🆕 ASR语言热更新接收器
    pub asr_language_rx: Option<watch::Receiver<Option<String>>>,
    /// 🔧 时序修复：Pipeline级别的当前轮次ID引用，用于打断信号
    pub current_turn_response_id: Arc<super::lockfree_response_id::LockfreeResponseId>,
    /// 🆕 全局轮次序列号，用于过滤延迟的VAD超时事件
    pub turn_sequence_number: Arc<std::sync::atomic::AtomicU64>,
    /// 🆕 当前轮次序列号，用于VAD超时检查
    pub current_turn_sequence: Arc<std::sync::atomic::AtomicU64>,
    /// 🚀 并行TTS会话创建通道
    pub parallel_tts_tx: Option<mpsc::UnboundedSender<(TurnContext, String)>>,
}

impl AsrTask {
    /// 🆕 使用简化打断机制开始新轮次
    pub fn start_new_turn_simplified(&mut self) -> Option<u64> {
        if let Some(ref mut handler) = self.simple_interrupt_handler {
            let turn_id = self.simple_interrupt_manager.start_new_turn();
            handler.bind_to_turn(turn_id);
            // 🔧 关键修复：同步更新本地轮次序列号
            self.current_turn_sequence.store(turn_id, std::sync::atomic::Ordering::Release);
            info!("🆕 ASR开始新轮次（简化机制）: turn={}", turn_id);
            Some(turn_id)
        } else {
            warn!("🚨 简化打断处理器未初始化");
            None
        }
    }

    /// 🆕 使用简化机制发送用户说话打断信号
    pub fn send_user_speaking_interrupt_simplified(&self) -> Result<(), String> {
        self.simple_interrupt_manager
            .broadcast_global_interrupt(self.session_id.clone(), SimpleInterruptReason::UserSpeaking)
    }

    /// 🆕 使用简化机制检查打断事件
    pub fn check_interrupt_simplified(&mut self) -> Option<super::simple_interrupt_manager::SimpleInterruptEvent> {
        if let Some(ref mut handler) = self.simple_interrupt_handler {
            handler.check_interrupt()
        } else {
            None
        }
    }
    /// 检测文本是否仅包含标点或空白
    fn is_only_punctuation(text: &str) -> bool {
        // 去除空白字符
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return true;
        }

        // 使用 Unicode 分割成标志性"字素"(grapheme)，检查每一个是否为字母/数字
        // 若都不是字母数字(即全部为标点、符号)，则过滤。
        trimmed.graphemes(true).all(|g| !g.chars().any(|c| c.is_alphanumeric()))
    }

    /// 智能文本合并：处理ASR结果的重叠和拼接问题
    fn smart_text_merge(existing: &str, new: &str) -> String {
        debug!("🔍 ASR任务smart_text_merge开始: existing='{}', new='{}'", existing, new);

        // 如果现有文本为空，直接返回新文本
        if existing.trim().is_empty() {
            debug!("🔍 existing为空，返回new: '{}'", new);
            return new.to_string();
        }

        // 如果新文本为空，返回现有文本
        if new.trim().is_empty() {
            debug!("🔍 new为空，返回existing: '{}'", existing);
            return existing.to_string();
        }

        // 情况1: 新文本完全包含现有文本（流式 ASR 常见情况）
        if new.contains(existing) {
            debug!("🔍 情况1: new包含existing，返回new: '{}'", new);
            return new.to_string();
        }

        // 情况2: 现有文本完全包含新文本（退化情况）
        if existing.contains(new) {
            debug!("🔍 情况2: existing包含new，返回existing: '{}'", existing);
            return existing.to_string();
        }

        // 情况3: 查找重叠部分并智能合并
        // 从现有文本的后缀与新文本的前缀开始匹配
        // 优化：使用迭代器直接操作，避免创建Vec<char>分配

        // 先进行一次基于"忽略结尾标点"的宽松包含判断，防止因标点差异导致重复拼接
        let punct_set = "，。！？；：、,.!?;:~…";
        let existing_core = {
            let trimmed = existing.trim();
            let stripped = trimmed.trim_end_matches(|c: char| punct_set.contains(c) || c.is_ascii_punctuation());
            stripped.to_string()
        };
        let new_core = {
            let trimmed = new.trim();
            let stripped = trimmed.trim_end_matches(|c: char| punct_set.contains(c) || c.is_ascii_punctuation());
            stripped.to_string()
        };
        if !existing_core.is_empty() && !new_core.is_empty() {
            // 如果新文本（去除结尾标点）以旧文本（去除结尾标点）为前缀/包含，则直接采用新文本
            if new_core.starts_with(existing_core.as_str()) || new_core.contains(existing_core.as_str()) {
                debug!("🔍 宽松匹配: new_core包含existing_core，返回new");
                return new.to_string();
            }
            // 反之亦然：旧文本包含新文本则保持旧文本
            if existing_core.starts_with(new_core.as_str()) || existing_core.contains(new_core.as_str()) {
                debug!("🔍 宽松匹配: existing_core包含new_core，返回existing");
                return existing.to_string();
            }
        }

        // 寻找最长的重叠部分（大小写不敏感）
        // 优化：使用迭代器直接比较，避免创建小写字符向量
        let existing_lower: String = existing.to_lowercase();
        let new_lower: String = new.to_lowercase();
        let mut best_overlap = 0;
        let min_len = existing_lower.chars().count().min(new_lower.chars().count());

        debug!(
            "🔍 情况3: 查找重叠，existing_len={}, new_len={}, min_len={}",
            existing.chars().count(),
            new.chars().count(),
            min_len
        );

        // 从最大可能重叠开始，逐步减少
        // 优化：使用chars()迭代器直接获取前后缀，避免Vec<char>分配
        for overlap_len in (1..=min_len).rev() {
            // 获取existing_lower的后overlap_len个字符
            let existing_suffix: String = existing_lower.chars().rev().take(overlap_len).collect();
            // 获取new_lower的前overlap_len个字符
            let new_prefix: String = new_lower.chars().take(overlap_len).collect();

            debug!(
                "🔍 检查重叠长度{}: existing_suffix={:?}, new_prefix={:?}",
                overlap_len, existing_suffix, new_prefix
            );

            if existing_suffix == new_prefix {
                best_overlap = overlap_len;
                debug!("🔍 找到最佳重叠长度: {}", best_overlap);
                break;
            }
        }

        if best_overlap > 0 {
            // 找到重叠，合并文本。注意：保留new的原始大小写，从new中跳过重叠部分
            // 优化：使用chars().skip()避免中间字符串分配
            let remaining: String = new.chars().skip(best_overlap).collect();
            let merged = format!("{}{}", existing, remaining);
            debug!("🔍 情况3a: 找到重叠，合并结果: '{}'", merged);
            return merged;
        }

        // 情况4: 没有找到重叠，但可能是连续的语音片段
        // 检查是否可以自然连接（通过检查空格或标点）
        let existing_trimmed = existing.trim();
        let new_trimmed = new.trim();

        // 检查现有文本是否以标点结尾
        let existing_ends_with_punctuation = existing_trimmed
            .chars()
            .last()
            .map(|c| "，。！？；：、".contains(c) || c.is_ascii_punctuation())
            .unwrap_or(false);

        debug!(
            "🔍 情况4: 检查标点连接，existing_ends_with_punctuation={}",
            existing_ends_with_punctuation
        );

        // 判断是否需要插入空格（字母数字紧邻）
        let needs_space = {
            let left = existing_trimmed.chars().rev().find(|c| !c.is_whitespace());
            let right = new_trimmed.chars().find(|c| !c.is_whitespace());
            match (left, right) {
                (Some(a), Some(b)) => a.is_alphanumeric() && b.is_alphanumeric(),
                _ => false,
            }
        };

        let result = if needs_space {
            format!("{} {}", existing_trimmed, new_trimmed)
        } else {
            format!("{}{}", existing_trimmed, new_trimmed)
        };
        debug!("🔍 情况4: 边界空格合并: '{}'", result);
        result
    }

    pub async fn run(mut self) -> Result<()> {
        info!("🎤 ASR task starting for session {}", self.session_id);

        // 🔧 添加错误计数器，用于改进错误处理
        let consecutive_errors = Arc::new(std::sync::atomic::AtomicU32::new(0));

        // 🔧 移除错误的启动时保存逻辑 - 这个时机不对，应该在即将发送新轮次前保存

        // 记录任务启动时间，用于计算相对毫秒
        let task_start_instant = Instant::now();

        // 创建 ASR 会话 (🆕 支持音频段保存和语言设置)
        info!("🔍 ASR任务创建会话，语言设置: {:?}", self.asr_language);
        let mut asr_session = self
            .asr_engine
            .create_session_with_auto_model_selection(
                self.session_id.clone(),
                self.speech_mode,
                self.asr_language.clone(),
                self.current_turn_response_id.clone(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("创建 ASR 会话失败: {}", e))?;

        // 🔧 修复：初始化时不获取VAD超时接收器，等待首次音频输入时检查

        // 用于控制超时计时器：只有在收到首个音频分片后才开始计时
        let mut first_audio_chunk_received = false;
        // 🆕 PTT自动开始标志
        let mut ptt_started = false;
        let _ptt_end_received = false;

        // 🔧 关键修复：在PTT模式下，任务启动时立即调用begin_speech()
        // 因为PTT模式意味着用户已经准备开始说话
        if self.speech_mode == crate::asr::SpeechMode::PushToTalk {
            asr_session.begin_speech();
            ptt_started = true;
            info!("🎙️ PTT begin_speech 自动触发 (任务启动)");
        }

        // 保存当前的用户item_id 与共享VAD状态
        // 🔧 PTT模式修复：在任务启动时就设置user_item_id，避免TranscriptionEvent被丢弃
        let mut current_user_item_id = if self.speech_mode == crate::asr::SpeechMode::PushToTalk {
            let item_id = format!("msg_{}", nanoid::nanoid!(6));
            info!("🎙️ PTT模式：任务启动时设置 user_item_id: {}", item_id);
            Some(item_id)
        } else {
            None::<String>
        };
        let mut current_user_content_index: u32 = 0;
        // 🔧 PTT模式修复：同时初始化语音段开始时间
        let mut speech_segment_start: Option<Instant> = if self.speech_mode == crate::asr::SpeechMode::PushToTalk {
            Some(Instant::now())
        } else {
            None
        };
        let has_sent_completed_atomic = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let last_vad_state_shared = Arc::new(std::sync::Mutex::new(VadState::Silence));

        // 🆕 新增：ASR任务层面的文本累积缓冲区
        let mut accumulated_text_buffer = String::new();
        let mut has_intermediate_results = false;

        // 🆕 新增：VAD触发状态跟踪（使用原子类型避免借用冲突）
        let vad_has_triggered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let vad_trigger_count = Arc::new(std::sync::atomic::AtomicU32::new(0));

        let mut conversation_item_created_sent = false; // 🔧 新增：标记是否已发送 conversation.item.created

        // 🆕 新增：只在Silence->Speaking时发送一次打断信号
        let mut has_sent_interrupt_this_turn = false;

        // 🚀 架构重构：学习LLM任务的事务驱动模式，完全消除无限循环轮询
        // 只在真正有事件时才被唤醒，而不是持续轮询
        info!(
            "🔄 ASR任务进入事件循环: session={}, speech_mode={:?}",
            self.session_id, self.speech_mode
        );
        info!("🔧 ASR任务通道状态检查: input_rx准备就绪（音频数据+PTT事件统一通道）");

        // 🆕 订阅简化打断管理器，用于监听 ConnectionLost 事件
        let mut interrupt_rx = self.simple_interrupt_manager.subscribe();
        // 🆕 语言更新订阅（若存在）
        let mut asr_language_rx = self.asr_language_rx.clone();
        info!("🔧 ASR任务已订阅简化打断管理器，监听 ConnectionLost 事件");
        loop {
            // 🔧 每次循环都重新克隆，避免移动问题
            let has_sent_completed_atomic_clone = has_sent_completed_atomic.clone();

            // 创建回调函数，处理ASR结果
            let session_id_callback = self.session_id.clone();
            let emitter_callback = self.emitter.clone();
            let simple_interrupt_manager_callback = self.simple_interrupt_manager.clone(); // 🆕 简化打断管理器
            let _is_responding_rx_callback = self.shared_flags.is_responding_rx.clone();
            let speech_mode_callback = self.speech_mode;
            let current_user_item_id_ref = &mut current_user_item_id;
            let current_user_content_index_ref = &mut current_user_content_index;
            let speech_segment_start_ref = &mut speech_segment_start;
            let last_vad_state_shared_cl = last_vad_state_shared.clone();
            let shared_flags_callback = self.shared_flags.clone();

            // 🆕 新增：传递累积缓冲区的引用
            let accumulated_text_buffer_ref = &mut accumulated_text_buffer;
            let has_intermediate_results_ref = &mut has_intermediate_results;

            // 🆕 新增：传递VAD触发状态的克隆引用
            let vad_has_triggered_clone = vad_has_triggered.clone();
            let vad_trigger_count_clone = vad_trigger_count.clone();

            let conversation_item_created_sent_ref = &mut conversation_item_created_sent;

            // 🔧 时序修复：传递response_id引用给回调，让回调动态获取最新值
            let current_turn_response_id_for_callback = self.current_turn_response_id.clone();
            // 🆕 传递序列号相关引用给回调
            let _turn_sequence_number_callback = self.turn_sequence_number.clone();
            let current_turn_sequence_callback = self.current_turn_sequence.clone();
            // 🔧 克隆shared_flags避免移动冲突
            let shared_flags_for_callback = shared_flags_callback.clone();
            // 🚀 克隆并行TTS发送器避免移动冲突
            let parallel_tts_tx_for_callback = self.parallel_tts_tx.clone();

            let mut callback = move |asr_result: AsrResult| {
                // 🆕 ASR 繁简转换：在处理文本前进行转换
                let asr_result = {
                    let convert_mode = *shared_flags_for_callback.asr_chinese_convert_mode.read().unwrap();
                    AsrResult {
                        text: crate::text_filters::convert_text(&asr_result.text, convert_mode),
                        ..asr_result
                    }
                };

                // 🔧 简化：只处理文本累积，不处理打断逻辑
                // 打断逻辑已移动到VAD状态变化处理中

                // 🔧 修复：VAD Silence 时重置打断flag，但需考虑保护状态
                if asr_result.vad_state == VadState::Silence {
                    // 🛡️ 保护逻辑：如果当前会话已完成，不重置打断标志，保持保护状态
                    let has_sent_completed_now = has_sent_completed_atomic_clone.load(std::sync::atomic::Ordering::Acquire);
                    if !has_sent_completed_now && has_sent_interrupt_this_turn {
                        debug!("[VAD] Silence, 重置has_sent_interrupt_this_turn（正常情况）");
                        has_sent_interrupt_this_turn = false;
                    } else if has_sent_completed_now {
                        debug!("[VAD] Silence, 保持打断标志状态（保护期间），has_sent_completed=true");
                    }
                }

                // VAD 状态变化处理（仅在 VAD 模式下）- 不依赖文本内容是否为空
                if matches!(speech_mode_callback, SpeechMode::Vad) {
                    let last_state = {
                        let guard = last_vad_state_shared_cl.lock().unwrap();
                        *guard
                    };

                    let state_changed = last_state != asr_result.vad_state;

                    // 🔧 关键修复：只有在状态实际发生变化时才更新
                    if state_changed {
                        let mut guard = last_vad_state_shared_cl.lock().unwrap();
                        *guard = asr_result.vad_state;
                    }

                    if state_changed {
                        info!(
                            "🔄 VAD状态变化: {:?} -> {:?}, text='{}'",
                            last_state, asr_result.vad_state, asr_result.text
                        );

                        match asr_result.vad_state {
                            // 🟢 静音 -> 说话：创建新 item 并发送 speech_started / item_created
                            VadState::Speaking => {
                                // 🔧 关键修复：防止已完成会话的重复打断
                                // 如果当前会话已发送completed，说明本轮对话已结束，不应再发送打断信号
                                let mut has_sent_completed_now = has_sent_completed_atomic_clone.load(std::sync::atomic::Ordering::Acquire);

                                // 🆕 VAD模式：在判断是否发送打断信号之前，先根据VAD新语音开始释放保护标志
                                if matches!(self.speech_mode, crate::asr::SpeechMode::Vad) && has_sent_completed_now {
                                    has_sent_completed_atomic_clone.store(false, std::sync::atomic::Ordering::Release);
                                    info!("🔄 [VAD] Silence->Speaking：释放 has_sent_completed（新轮次开始，已过VAD冷却）");
                                    // 立即更新本地快照，允许本次Speaking就触发打断
                                    has_sent_completed_now = false;
                                }
                                if has_sent_completed_now {
                                    info!(
                                        "🛡️ [VAD] 当前会话已发送completed，跳过打断信号发送，避免重复打断: session={}",
                                        session_id_callback
                                    );
                                    // 仍需创建item_id和发送speech_started，但跳过打断逻辑
                                } else if !has_sent_interrupt_this_turn {
                                    info!(
                                        "🗣️ [VAD] Silence->Speaking, 立即发送打断信号 (session: {})",
                                        session_id_callback
                                    );

                                    // 🔧 关键修复：只有在存在活跃轮次时才发送打断信号
                                    let current_turn = simple_interrupt_manager_callback.current_turn();
                                    let current_turn_for_interrupt = current_turn;
                                    info!(
                                        "🔄 轮次一致性检查: session={}, local_turn={}, global_turn={}",
                                        session_id_callback, current_turn_for_interrupt, current_turn
                                    );

                                    // 🚀 核心修复：轮次0表示初始状态，没有活跃的TTS任务需要被打断
                                    if current_turn_for_interrupt > 0 {
                                        info!("🔄 检测到活跃轮次({}), 准备发送打断信号", current_turn_for_interrupt);

                                        // 🔧 关键修复：先创建新轮次，然后使用新轮次发送打断信号
                                        // 这样确保"只处理新轮次"的逻辑能够正确工作
                                        let new_turn_sequence = simple_interrupt_manager_callback.start_new_turn();
                                        current_turn_sequence_callback.store(new_turn_sequence, std::sync::atomic::Ordering::Release);

                                        // 🔧 使用新轮次发送打断信号，确保下游组件能正确处理
                                        let start_time = std::time::Instant::now();
                                        let broadcast_result = simple_interrupt_manager_callback.broadcast_global_interrupt_with_turn(
                                            session_id_callback.clone(),
                                            SimpleInterruptReason::UserSpeaking,
                                            new_turn_sequence,
                                        );

                                        match broadcast_result {
                                            Ok(()) => {
                                                let elapsed = start_time.elapsed();
                                                info!(
                                                    "✅ 简化机制打断信号已广播 (耗时: {:?}) - 使用新轮次: {} 打断旧轮次: {}",
                                                    elapsed, new_turn_sequence, current_turn_for_interrupt
                                                );
                                            },
                                            Err(e) => {
                                                error!("简化机制广播用户说话打断失败: {} - 目标轮次: {}", e, new_turn_sequence);
                                            },
                                        }

                                        info!(
                                            "🆕 [VAD] Silence->Speaking时更新轮次序列号: session={}, new_turn_sequence={} (打断旧轮次: {})",
                                            session_id_callback, new_turn_sequence, current_turn_for_interrupt
                                        );
                                    } else {
                                        info!(
                                            "🆕 初始状态(轮次={}), 创建首个轮次但无需发送打断信号",
                                            current_turn_for_interrupt
                                        );
                                        // 初始状态仍需创建新轮次
                                        let new_turn_sequence = simple_interrupt_manager_callback.start_new_turn();
                                        current_turn_sequence_callback.store(new_turn_sequence, std::sync::atomic::Ordering::Release);
                                        info!(
                                            "🆕 [VAD] Silence->Speaking时创建首个轮次: session={}, new_turn_sequence={}",
                                            session_id_callback, new_turn_sequence
                                        );
                                    }

                                    // 🔧 修复：只有在实际发送了打断信号时才设置标志
                                    has_sent_interrupt_this_turn = true;

                                    // 🆕 立即输出上一轮的延迟报告（在重置之前），避免跨轮次累积
                                    {
                                        let session_id_for_seq = session_id_callback.clone();
                                        let response_id_for_seq = current_turn_response_id_for_callback.load();
                                        // 顺序执行：先生成并存储报告，再重置，防止跨轮次累积
                                        tokio::spawn(async move {
                                            if let Some(resp_id) = response_id_for_seq.as_ref() {
                                                crate::rpc::pipeline::asr_llm_tts::timing_manager::GLOBAL_TIMING_MANAGER
                                                    .generate_print_and_store(&session_id_for_seq, resp_id)
                                                    .await;
                                            } else {
                                                crate::rpc::pipeline::asr_llm_tts::timing_manager::print_timing_report(&session_id_for_seq).await;
                                            }
                                            // 报告输出后再重置本轮计时
                                            reset_session_timing(&session_id_for_seq).await;
                                        });
                                    }

                                    // 🔧 清理状态，准备新的语音轮次（与 asr_task_vad.rs 保持一致）
                                    accumulated_text_buffer_ref.clear();
                                    *current_user_item_id_ref = None;
                                    *conversation_item_created_sent_ref = false;
                                    *current_user_content_index_ref = 0;
                                    debug!("🆕 重置状态标志，开始新语音段");
                                } else {
                                    // 🛡️ 已完成会话的保护状态：不发送打断信号，不设置打断标志
                                    info!("🛡️ [VAD] 保护状态：跳过打断逻辑和状态重置，避免干扰已完成的会话");
                                }

                                // 🆕 标记VAD已触发
                                vad_has_triggered_clone.store(true, std::sync::atomic::Ordering::Release);
                                let count = vad_trigger_count_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                                debug!("🎤 VAD触发计数: {}", count);

                                // 保持原有逻辑（非VAD模式）
                                // 只有在实际需要启动新轮次时才重置has_sent_completed_atomic
                                // 如果当前会话已完成但仍在网络延迟期间，不应重置，以保护打断逻辑
                                // 🚀 额外保护：增加时间间隔检查，防止网络延迟期间的快速重复Speaking状态重置保护标志
                                if !matches!(self.speech_mode, crate::asr::SpeechMode::Vad) {
                                    let should_reset_completed_flag = !has_sent_completed_now;

                                    if should_reset_completed_flag {
                                        has_sent_completed_atomic_clone.store(false, std::sync::atomic::Ordering::Release);
                                        debug!("🔄 VAD状态变为Speaking，重置has_sent_completed_atomic为false（新轮次）");
                                    } else {
                                        debug!("🛡️ VAD状态变为Speaking，但当前会话已完成，保持has_sent_completed_atomic=true（保护期间）");
                                        debug!("🛡️ 这是防止网络延迟期间重复completed事件的关键保护机制");
                                    }
                                }

                                // 🔧 PTT模式修复：如果已有user_item_id（PTT模式下在启动时设置），则不重复设置
                                let item_id = if current_user_item_id_ref.is_none() {
                                    let new_item_id = format!("msg_{}", nanoid::nanoid!(6));
                                    *current_user_item_id_ref = Some(new_item_id.clone());
                                    info!("🎙️ 语音开始 - 设置 user_item_id: {}", new_item_id);
                                    new_item_id
                                } else {
                                    let existing_id = current_user_item_id_ref.as_ref().unwrap().clone();
                                    info!("🎙️ 语音开始 - user_item_id 已存在（PTT模式）: {}", existing_id);
                                    existing_id
                                };

                                *current_user_content_index_ref = 0;
                                *speech_segment_start_ref = Some(Instant::now());
                                // *has_sent_completed_ref = false; // 🔧 传递完成标志的引用

                                // 🔧 重置 conversation.item.created 发送标志
                                *conversation_item_created_sent_ref = false;

                                // 🆕 重置其他状态标志，准备新的语音段（累积缓冲区已在打断检查时提前清理）
                                *has_intermediate_results_ref = false;
                                debug!("🆕 重置状态标志，开始新语音段");

                                let emitter_start = emitter_callback.clone();
                                let start_ms = task_start_instant.elapsed().as_millis() as u32;
                                let id_clone = item_id.clone();
                                tokio::spawn(async move {
                                    emitter_start.input_audio_buffer_speech_started(&id_clone, start_ms).await;
                                });

                                // 🔧 修复：暂时移除立即发送conversation.item.created，等到有有效文本时再发送
                                // 这避免了VAD触发但ASR为空时创建多余的conversation item
                                // if !*conversation_item_created_sent_ref {
                                //     if let Some(item_id) = current_user_item_id_ref.as_ref() {
                                //         let emitter_item = emitter_callback.clone();
                                //         let item_id_clone = item_id.clone();
                                //         tokio::spawn(async move {
                                //             emitter_item
                                //                 .conversation_item_created(&item_id_clone, "user", "in_progress", None)
                                //                 .await;
                                //         });
                                //     }
                                //     *conversation_item_created_sent_ref = true;
                                // }
                            },
                            // 🔴 说话 -> 静音：发送 speech_stopped，保持当前 item_id，用于最终结果
                            VadState::Silence => {
                                if last_state == VadState::Speaking {
                                    // 🆕 记录VAD触发时间（基准时间）- 在speaking->silence时开始计时（用户结束说话）
                                    let vad_trigger_time = std::time::Instant::now();
                                    let session_id_for_timing = session_id_callback.clone();
                                    tokio::spawn(async move {
                                        record_vad_trigger(&session_id_for_timing, vad_trigger_time).await;
                                    });

                                    if let Some(ref item_id) = *current_user_item_id_ref {
                                        let end_ms = task_start_instant.elapsed().as_millis() as u32;
                                        info!("🛑 语音结束 - item_id: {}", item_id);
                                        let emitter_stop = emitter_callback.clone();
                                        let id_clone = item_id.clone();
                                        tokio::spawn(async move {
                                            emitter_stop.input_audio_buffer_speech_stopped(&id_clone, end_ms).await;
                                        });
                                    }
                                }
                                // 不创建新的 item_id，保留现有，直到下一次 Speaking 开始
                            },
                        }
                    }
                }

                // 🆕 文本累积处理：对所有有文本的ASR结果进行累积
                if !asr_result.text.trim().is_empty() {
                    debug!(
                        "🆕 处理ASR文本: '{}', is_partial: {}, 当前缓冲区: '{}'",
                        asr_result.text,
                        asr_result.is_partial,
                        accumulated_text_buffer_ref.chars().take(30).collect::<String>()
                    );

                    // 🔧 检查是否为首个分片且仅包含标点符号
                    let is_first_fragment = accumulated_text_buffer_ref.trim().is_empty();
                    let is_pure_punctuation = Self::is_only_punctuation(&asr_result.text);

                    if is_first_fragment && is_pure_punctuation {
                        debug!("🚫 过滤首个纯标点分片: '{}'", asr_result.text);
                        // 跳过此分片，不添加到累积缓冲区
                        return;
                    }

                    // 🆕 若尚未发送 conversation.item.created，则在此时发送（保证有有效文本）
                    if !*conversation_item_created_sent_ref {
                        if let Some(item_id) = current_user_item_id_ref.as_ref() {
                            let emitter_item = emitter_callback.clone();
                            let item_id_clone = item_id.clone();
                            tokio::spawn(async move {
                                emitter_item
                                    .conversation_item_created(&item_id_clone, "user", "in_progress", None)
                                    .await;
                            });
                        }
                        *conversation_item_created_sent_ref = true;
                    }

                    // 智能合并文本到累积缓冲区
                    let new_accumulated = Self::smart_text_merge(accumulated_text_buffer_ref, &asr_result.text);
                    *accumulated_text_buffer_ref = new_accumulated;

                    if asr_result.is_partial {
                        *has_intermediate_results_ref = true;
                    }

                    debug!("🆕 累积缓冲区更新: '{}'", accumulated_text_buffer_ref);
                }

                // LLM 处理请求（仅在最终结果时发送累积的完整文本）
                // 🆕 PTT模式下，不允许VAD状态变化触发最终输出，只有PttEvent::End才能触发
                if !asr_result.is_partial
                    && !accumulated_text_buffer_ref.trim().is_empty()
                    && !Self::is_only_punctuation(accumulated_text_buffer_ref)
                    && !has_sent_completed_atomic_clone.load(std::sync::atomic::Ordering::Acquire)
                    && self.speech_mode != crate::asr::SpeechMode::PushToTalk
                {
                    // 🔧 关键时机：在生成新轮次之前，保存当前应该被打断的response_id
                    let should_be_interrupted_response_id = {
                        let current_context = shared_flags_for_callback.assistant_response_context.get_context_copy();
                        current_context.map(|c| c.response_id)
                    };

                    // 保存到current_turn_response_id（仅保存真值），供后续打断信号使用
                    {
                        if let Some(ref id) = should_be_interrupted_response_id {
                            current_turn_response_id_for_callback.store(Some(id.clone()));
                            info!("🔒 保存应被打断的response_id: {} (即将生成新轮次)", id);
                        } else {
                            info!("🔒 当前无活跃轮次，无需保存打断目标");
                        }
                    }

                    // 生成新的对话上下文
                    let user_item_id = current_user_item_id_ref
                        .clone()
                        .unwrap_or_else(|| format!("msg_{}", nanoid::nanoid!(6)));
                    let user_item_id_for_ctx = user_item_id.clone(); // 为TurnContext准备副本
                    let assistant_item_id = format!("asst_{}", nanoid::nanoid!(6));
                    let response_id = format!("resp_{}", nanoid::nanoid!(8));

                    // 🔧 关键修复：使用已更新的轮次序列号创建TurnContext
                    let current_turn_sequence = current_turn_sequence_callback.load(std::sync::atomic::Ordering::Acquire);
                    let ctx = TurnContext::new(
                        user_item_id_for_ctx,
                        assistant_item_id,
                        response_id.clone(),
                        Some(current_turn_sequence),
                    );
                    info!("🔄 创建TurnContext with turn_sequence={}", current_turn_sequence);

                    // 🔧 关键修复：将新的response_id存储到LockfreeResponseId中
                    current_turn_response_id_for_callback.store(Some(response_id.clone()));
                    info!("🔒 存储新的response_id到LockfreeResponseId: {}", response_id);

                    // 🔧 UserSpeaking已提前更新轮次并打断旧TTS任务，无需额外的NewUserRequest信号
                    info!("✅ UserSpeaking已处理打断，开始LLM处理");

                    // 🔧 修复：确保只有有效用户输入才发送到LLM，防止空输入导致资源浪费
                    if !accumulated_text_buffer_ref.trim().is_empty() {
                        // 🆕 TODO 1: 记录ASR最终结果时间
                        let asr_final_time = Instant::now();
                        let session_id_for_asr_timing = session_id_callback.clone();
                        tokio::spawn(async move {
                            record_node_time(&session_id_for_asr_timing, TimingNode::AsrFinalResult, asr_final_time).await;
                        });

                        // 🚀 架构修正：ASR任务只发送到并行处理任务，由并行处理任务负责转发到LLM
                        if let Some(parallel_tts_tx) = &parallel_tts_tx_for_callback {
                            info!(
                                "🔍 ASR准备发送到并行处理任务: '{}'",
                                accumulated_text_buffer_ref.chars().take(50).collect::<String>()
                            );
                            if parallel_tts_tx
                                .send((ctx.clone(), accumulated_text_buffer_ref.clone()))
                                .is_err()
                            {
                                error!("⚠️ 并行处理任务通道已关闭");
                            } else {
                                info!(
                                    "✅ ASR任务已成功发送用户消息到并行处理任务: '{}'",
                                    accumulated_text_buffer_ref.chars().take(50).collect::<String>()
                                );
                                info!("🔄 并行处理任务将负责转发到LLM和创建TTS会话");
                            }
                        } else {
                            error!("❌ 并行处理任务通道未配置");
                        }

                        // 🔧 标记文本是否包含了中间结果（用于调试）
                        if *has_intermediate_results_ref {
                            info!("✅ 包含了中间结果的完整累积文本");
                        }

                        // 🔧 关键修复：标记已发送LLM消息，避免VAD超时竞态条件
                        info!("🛑 已发送LLM消息，标记以避免VAD超时竞态条件");
                        has_sent_completed_atomic_clone.store(true, std::sync::atomic::Ordering::Release);
                    }

                    // 🔧 标记已发送完成状态，避免重复发送（即使后续还有partial结果）
                    // *has_sent_completed_ref = true; // 移除此行，由上面的标记控制

                    // 更新用户消息状态
                    debug!(
                        "📤 用户消息状态更新为完成: item_id={}, text={}",
                        user_item_id,
                        accumulated_text_buffer_ref.chars().take(50).collect::<String>()
                    );

                    // 🔧 核心修复：在清空缓冲区前先clone文本内容，确保转录事件有正确内容
                    let user_text_for_update = accumulated_text_buffer_ref.clone();
                    info!(
                        "📋 为转录事件保存文本内容: '{}'",
                        user_text_for_update.chars().take(50).collect::<String>()
                    );

                    // 🔧 核心修复：clone文本后立即清空缓冲区，避免超时重发
                    info!(
                        "🧹 清空累积文本缓冲区，防止超时重复发送: '{}'",
                        accumulated_text_buffer_ref.chars().take(50).collect::<String>()
                    );
                    accumulated_text_buffer_ref.clear();

                    // 🔐 无条件标记已发送completed，防止随后VAD超时路径误发 failed
                    has_sent_completed_atomic_clone.store(true, std::sync::atomic::Ordering::Release);

                    // 使用异步发送，避免阻塞主循环
                    let emitter_update = emitter_callback.clone();
                    tokio::spawn(async move {
                        // 发送 conversation.item.input_audio_transcription.completed
                        emitter_update
                            .conversation_item_input_audio_transcription_completed(&user_item_id, 0, &user_text_for_update)
                            .await;

                        // 更新 conversation.item 状态为 completed
                        emitter_update
                            .conversation_item_updated(&user_item_id, "user", "completed")
                            .await;
                    });
                }

                // 🆕 打断后ASR无输出时，发送ASR转录失败信令并重置状态
                //    但若本轮已发送 completed（例如在 finalize 回调中），则不应再发送 failed
                if has_sent_interrupt_this_turn
                    && asr_result.vad_state == VadState::Silence
                    && accumulated_text_buffer_ref.trim().is_empty()
                    && !has_sent_completed_atomic_clone.load(std::sync::atomic::Ordering::Acquire)
                {
                    warn!(
                        "[VAD] 打断后ASR无输出, 发送AsrTranscriptionFailed信令并重置状态, session={}",
                        session_id_callback
                    );
                    let emitter_clone = emitter_callback.clone();
                    let session_id_clone = session_id_callback.clone();

                    // 如果有item_id，发送ASR转录失败信令
                    if let Some(item_id) = current_user_item_id_ref.as_ref() {
                        let item_id_clone = item_id.clone();
                        tokio::spawn(async move {
                            emitter_clone
                                .asr_transcription_failed(&item_id_clone, 0, "no_output", "检测到打断后无有效语音输入，ASR转录失败")
                                .await;
                            info!(
                                "🔔 已发送ASR转录失败信令: session={}, item_id={}",
                                session_id_clone, item_id_clone
                            );
                        });
                    } else {
                        // 如果没有item_id，发送通用错误事件
                        tokio::spawn(async move {
                            emitter_clone
                                .error_event(1002, "检测到打断后无有效语音输入，本轮对话已终止")
                                .await;
                            info!("🔔 已发送无输出信令: session={}", session_id_clone);
                        });
                    }

                    // 重置本轮状态，允许继续下一轮
                    has_sent_interrupt_this_turn = false;
                    *conversation_item_created_sent_ref = false;
                    accumulated_text_buffer_ref.clear();
                }

                // 转录事件处理 - 现在即使当前没有设置item_id也要尝试处理
                if let Some(item_id) = current_user_item_id_ref.as_ref() {
                    // 仅在已发送 conversation_item_created 时才发送 delta/completed
                    if *conversation_item_created_sent_ref {
                        let emitter = emitter_callback.clone();
                        let item_id = item_id.clone();
                        let text = asr_result.text.clone();
                        let is_partial = asr_result.is_partial;
                        let content_index = *current_user_content_index_ref;

                        // 🔧 只对有实际文本内容的结果发送事件
                        if !text.trim().is_empty() {
                            // 发送后自增索引
                            *current_user_content_index_ref += 1;

                            tokio::spawn(async move {
                                if is_partial {
                                    emitter
                                        .conversation_item_input_audio_transcription_delta(&item_id, content_index, &text)
                                        .await;
                                }
                                // else 分支已移除，不再发送分片级 completed
                            });
                        }
                    }
                } else {
                    // 如果没有item_id但有文本内容，说明VAD状态变化检测有问题，记录详细信息
                    if !asr_result.text.is_empty() {
                        warn!(
                            "TranscriptionEvent: user_item_id not set, dropping event for text: '{}', vad_state: {:?}, is_partial: {}",
                            asr_result.text, asr_result.vad_state, asr_result.is_partial
                        );
                    }
                }
            };

            // 🔧 修复：直接使用ASR会话中的VAD超时接收器
            if let Some(timeout_rx) = asr_session.get_timeout_receiver_mut() {
                // 有VAD超时接收器时，包含VAD超时分支
                tokio::select! {
                    // 统一输入消息：音频数据或PTT事件（保证顺序）
                    Some(input_msg) = self.input_rx.recv() => {
                        match input_msg {
                            AsrInputMessage::PttEnd => {
                                info!("🛑 收到PTT End 事件，模拟VAD结束，立即调用end_speech");

                                if let Err(e) = asr_session.end_speech(&mut callback).await {
                                    error!("PTT end_speech 失败: {}", e);
                                    // 发送ASR end_speech失败事件
                                    let emitter = self.emitter.clone();
                                    let item_id = current_user_item_id.clone().unwrap_or_else(|| "unknown".to_string());
                                    let error_msg = e.to_string();
                                    tokio::spawn(async move {
                                        emitter.asr_transcription_failed(&item_id, 0, "END_SPEECH_ERROR", &error_msg).await;
                                    });
                                } else {
                                    info!("✅ PTT end_speech 调用成功，ASR最终结果已通过callback处理");
                                    // PTT End成功后，ASR已通过callback返回最终结果
                                    // 现在可以结束当前轮次，准备下一轮

                                    // 🚀 PTT模式专门处理：检查累积文本并发送到并行处理任务
                                    if !accumulated_text_buffer.trim().is_empty() && !Self::is_only_punctuation(&accumulated_text_buffer) && !has_sent_completed_atomic.load(std::sync::atomic::Ordering::Acquire) {
                                        info!("🔍 PTT End后检查累积文本: '{}'", accumulated_text_buffer.chars().take(50).collect::<String>());

                                        // 生成新的对话上下文（类似callback中的逻辑）
                                        let user_item_id = current_user_item_id
                                            .clone()
                                            .unwrap_or_else(|| format!("msg_{}", nanoid::nanoid!(6)));
                                        let assistant_item_id = format!("asst_{}", nanoid::nanoid!(6));
                                        let response_id = format!("resp_{}", nanoid::nanoid!(8));

                                        // 🔧 修复：PTT模式下发送缺失的ASR转录信令
                                        info!("📤 PTT End发送ASR转录信令: item_id={}", user_item_id);
                                        let emitter_asr = self.emitter.clone();
                                        let user_item_id_for_asr = user_item_id.clone();
                                        let accumulated_text_for_asr = accumulated_text_buffer.clone();
                                        tokio::spawn(async move {
                                            // 发送 conversation.item.input_audio_transcription.completed
                                            emitter_asr
                                                .conversation_item_input_audio_transcription_completed(&user_item_id_for_asr, 0, &accumulated_text_for_asr)
                                                .await;

                                            // 更新 conversation.item 状态为 completed
                                            emitter_asr
                                                .conversation_item_updated(&user_item_id_for_asr, "user", "completed")
                                                .await;
                                        });

                                        // 🔐 无条件标记已发送completed（即使并行处理通道不可用）
                                        has_sent_completed_atomic.store(true, std::sync::atomic::Ordering::Release);

                                        // 🔧 关键修复：PTT End时预先开始新轮次，获取正确的轮次序列号
                                        let new_turn_sequence = self.simple_interrupt_manager.start_new_turn();
                                        self.current_turn_sequence.store(new_turn_sequence, std::sync::atomic::Ordering::Release);
                                        let ctx = TurnContext::new(user_item_id.clone(), assistant_item_id, response_id.clone(), Some(new_turn_sequence));
                                        info!("🔄 PTT End创建TurnContext with turn_sequence={} (新轮次)", new_turn_sequence);

                                        // 🔧 关键修复：将新的response_id存储到LockfreeResponseId中
                                        self.current_turn_response_id.store(Some(response_id.clone()));
                                        info!("🔒 PTT End存储新的response_id到LockfreeResponseId: {}", response_id);

                                        // 发送到并行处理任务
                                        if let Some(ref parallel_tts_tx) = self.parallel_tts_tx {
                                            info!("🔍 PTT End准备发送到并行处理任务: '{}'", accumulated_text_buffer.chars().take(50).collect::<String>());
                                            if parallel_tts_tx.send((ctx.clone(), accumulated_text_buffer.clone())).is_err() {
                                                error!("⚠️ PTT End发送到并行处理任务失败：通道已关闭");
                                            } else {
                                                info!("✅ PTT End已成功发送用户消息到并行处理任务: '{}'", accumulated_text_buffer.chars().take(50).collect::<String>());
                                                info!("🔄 并行处理任务将负责转发到LLM和创建TTS会话");
                                                // 标记已发送，避免重复
                                                has_sent_completed_atomic.store(true, std::sync::atomic::Ordering::Release);

                                                // 🧹 PTT模式专门清理：发送成功后清理文本缓冲区和相关状态
                                                accumulated_text_buffer.clear();
                                                has_intermediate_results = false;
                                                conversation_item_created_sent = false;
                                                info!("🧹 PTT End发送成功后已清理：文本缓冲区、中间结果标志、对话项创建标志");
                                            }
                                        } else {
                                            error!("❌ PTT End时并行处理任务通道未配置");
                                        }
                                    } else {
                                        info!("🔍 PTT End时无有效文本或已发送，跳过并行处理任务");
                                    }

                                    // 🔧 重置状态，准备下一轮对话
                                    ptt_started = false;
                                    first_audio_chunk_received = false;
                                    consecutive_errors.store(0, std::sync::atomic::Ordering::SeqCst);

                                    // 🔧 修复：PTT轮次结束，新轮次已在发送到并行处理任务时开始
                                    info!("🔄 PTT轮次结束，新轮次已在并行处理任务发送时开始: sequence={}", self.current_turn_sequence.load(std::sync::atomic::Ordering::Acquire));
                                }
                            }
                            AsrInputMessage::DirectText(_) => {
                                // DirectText 不应该到达ASR任务，记录警告并忽略
                                warn!("⚠️ ASR任务收到DirectText消息，这不应该发生，已忽略");
                                continue;
                            }
                            AsrInputMessage::Audio(audio_chunk) => {
                        // 🎯 TRACE: ASR任务收到音频数据
                        // info!("🎤 [TRACE-AUDIO] ASR任务收到音频数据 | session_id={} | f32_samples={}",
                        //       self.session_id, audio_chunk.len());

                        // 🆕 添加调试日志：确认音频数据接收

                                                // 🆕 自动开始PTT语音段: 如果处于PushToTalk模式且尚未开始，自动调用begin_speech()
                        if self.speech_mode == crate::asr::SpeechMode::PushToTalk {
                            // 尝试在第一次音频到达时自动开始PTT会话
                            if !ptt_started {
                                ptt_started = true;
                                asr_session.begin_speech();
                                info!("🎙️ 自动触发 PTT begin_speech (首个音频分片)");

                                // 🔧 关键修复：在PTT新轮次开始时重置has_sent_completed_atomic
                                // 这确保多轮PTT对话场景下，每次新的语音段都能正确处理
                                has_sent_completed_atomic.store(false, std::sync::atomic::Ordering::Release);
                                debug!("🔄 PTT新轮次开始，重置has_sent_completed_atomic为false");

                                // 🚀 PTT模式打断逻辑：发送UserPtt打断信号，目标指向"当前轮次"停止正在播放的TTS
                                let current_turn_for_interrupt = self.simple_interrupt_manager.current_turn();
                                if current_turn_for_interrupt > 0 {
                                    let target_turn = current_turn_for_interrupt;
                                    info!(
                                        "🔄 PTT检测到活跃轮次({}), 发送UserPtt打断信号 -> 目标轮次({})",
                                        current_turn_for_interrupt,
                                        target_turn
                                    );
                                    let simple_interrupt_manager_for_ptt = self.simple_interrupt_manager.clone();
                                    let session_id_for_ptt_interrupt = self.session_id.clone();
                                    tokio::spawn(async move {
                                        let start_time = std::time::Instant::now();
                                        let result = simple_interrupt_manager_for_ptt.broadcast_global_interrupt_with_turn(
                                            session_id_for_ptt_interrupt,
                                            crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::InterruptReason::UserPtt,
                                            target_turn
                                        );
                                        let elapsed = start_time.elapsed();

                                        match result {
                                            Ok(_) => {
                                                info!("✅ PTT打断信号发送成功: target_turn={} (当前轮次), elapsed={:?}", target_turn, elapsed);
                                            }
                                            Err(e) => {
                                                error!("❌ PTT打断信号发送失败: target_turn={} (当前轮次), error={}, elapsed={:?}", target_turn, e, elapsed);
                                            }
                                        }
                                    });

                                    // 🆕 立即输出上一轮延迟报告并重置计时，避免跨轮次累积
                                    let session_id_for_report = self.session_id.clone();
                                    let current_turn_response_id_for_ptt = self.current_turn_response_id.clone();
                                    tokio::spawn(async move {
                                        if let Some(resp_id) = current_turn_response_id_for_ptt.load().as_ref() {
                                            crate::rpc::pipeline::asr_llm_tts::timing_manager::GLOBAL_TIMING_MANAGER
                                                .generate_print_and_store(&session_id_for_report, resp_id)
                                                .await;
                                        } else {
                                            crate::rpc::pipeline::asr_llm_tts::timing_manager::print_timing_report(&session_id_for_report).await;
                                        }
                                        crate::rpc::pipeline::asr_llm_tts::timing_manager::reset_session_timing(&session_id_for_report).await;
                                    });
                                } else {
                                    info!("🔄 PTT无活跃轮次，跳过打断信号");
                                }
                            }
                        }

                        // PTT End现在立即处理，不再需要防御性检查

                        // 标记已收到首个音频分片，后续才启动超时计时器
                        if !first_audio_chunk_received {
                            first_audio_chunk_received = true;
                            info!("🎵 [PTT] 首个音频分片已接收: session={}", self.session_id);
                        }

                        // 🎯 TRACE: 调用ASR引擎处理音频
                        // info!("🎤 [TRACE-AUDIO] 调用ASR引擎处理音频 | session_id={} | samples={}",
                        //       self.session_id, audio_chunk.len());

                        match asr_session
                            .process_audio_chunk(audio_chunk, &mut callback)
                            .await
                        {
                            Ok(()) => {
                                // 🔧 成功处理音频块时重置错误计数器
                                consecutive_errors.store(0, std::sync::atomic::Ordering::SeqCst);
                            },
                            Err(e) => {
                                error!("处理音频块失败: {}", e);

                                // 🔧 添加ASR失败监控指标
                                crate::monitoring::record_asr_failure();

                                // 🔧 改进错误处理：统计连续错误次数，避免因单次错误退出
                                let consecutive_count = consecutive_errors.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                                const MAX_CONSECUTIVE_ERRORS: u32 = 5;

                                if consecutive_count >= MAX_CONSECUTIVE_ERRORS {
                                    error!("❌ ASR任务连续{}次错误，退出任务: session={}", consecutive_count, self.session_id);

                                    // 发送ASR失败事件
                                    let emitter_err = self.emitter.clone();
                                    let item_id_err = current_user_item_id.clone().unwrap_or_else(|| "unknown".to_string());
                                    let error_msg = format!("连续{}次处理音频块失败: {}", consecutive_count, e);
                                    tokio::spawn(async move {
                                        emitter_err.asr_transcription_failed(&item_id_err, 0, "PROCESS_ERROR", &error_msg).await;
                                    });
                                    break;
                                } else {
                                    warn!("⚠️ ASR处理音频块失败({}/{}次)，继续尝试: session={}, error={}",
                                          consecutive_count, MAX_CONSECUTIVE_ERRORS, self.session_id, e);

                                    // 继续处理下一个音频块，不退出
                                    continue;
                                }
                            }
                        }
                    }
                        }
                    }

                    // 🆕 ConnectionLost事件：WebSocket重连时重置has_sent_completed标志
                    Ok(event) = interrupt_rx.recv() => {
                        if event.session_id == self.session_id &&
                           matches!(event.reason, super::simple_interrupt_manager::InterruptReason::ConnectionLost) {
                            if self.speech_mode == crate::asr::SpeechMode::Vad {
                                // 在VAD模式下，保持已发送完成标志为true，避免后续VAD超时路径触发重复finalize并产生二次LLM请求
                                info!(
                                    "🔄 ASR收到ConnectionLost事件（VAD模式），保持has_sent_completed=true以避免超时重复finalize: session={}, event_id={}",
                                    self.session_id, event.event_id
                                );
                                // 清空状态，为新连接做准备（与 asr_task_vad.rs 保持一致）
                                accumulated_text_buffer.clear();
                                current_user_item_id = None;
                                conversation_item_created_sent = false;
                                current_user_content_index = 0;
                                info!("✅ ConnectionLost处理完成（VAD），等待新语音开始以开启新一轮处理");
                            } else {
                                info!(
                                    "🔄 ASR收到ConnectionLost事件，重置has_sent_completed标志: session={}, event_id={}",
                                    self.session_id, event.event_id
                                );
                                has_sent_completed_atomic.store(false, std::sync::atomic::Ordering::Release);
                                // 清空状态，为新连接做准备（与 asr_task_vad.rs 保持一致）
                                accumulated_text_buffer.clear();
                                current_user_item_id = None;
                                conversation_item_created_sent = false;
                                current_user_content_index = 0;
                                info!("✅ ConnectionLost处理完成，ASR已准备好处理新的语音输入");
                            }
                        } else {
                            debug!("🔄 ASR忽略不相关的打断事件: reason={:?}, session={}", event.reason, event.session_id);
                        }
                    }

                    // VAD超时事件：语音活动检测超时
                    Some(_timeout_event) = timeout_rx.recv() => {
                        info!("⏰ 收到VAD超时事件: {}", self.session_id);

                        // 🆕 PTT模式检查：在PTT模式下忽略VAD超时事件，只响应PttEvent::End
                        if self.speech_mode == crate::asr::SpeechMode::PushToTalk {
                            info!("🎙️ PTT模式：忽略VAD超时事件，只响应stopInput命令");
                            continue;
                        }

                        // 🔧 修复：获取当前状态
                        let is_responding_now = *self.shared_flags.is_responding_rx.borrow();
                        let has_sent_completed = has_sent_completed_atomic.load(std::sync::atomic::Ordering::Acquire);

                        // 🎯 VAD模式关键修复：在VAD模式下，一旦发送了completed就应该完全跳过后续的finalize
                        // 不应该重置has_sent_completed标志，因为这会破坏保护机制
                        let skip_finalize_in_vad = has_sent_completed && matches!(self.speech_mode, crate::asr::SpeechMode::Vad);

                        // 🔧 关键修复：VAD模式下已发送completed后，直接跳过整个超时处理，避免任何可能的二次finalize
                        // 这是解决锁定问题的核心修复：防止网络延迟期间收到的音频触发第二次transcription.completed
                        if skip_finalize_in_vad {
                            info!(
                                "🧊 VAD模式已发送transcription.completed，直接跳过VAD超时处理，避免二次finalize: session={}, has_sent_completed={}",
                                self.session_id, has_sent_completed
                            );
                            info!("🛡️ 这是防止客户端麦克风锁定的关键保护机制：避免网络延迟期间的重复finalize");
                            continue;
                        }

                        // 🔧 仅在非VAD模式下才考虑重置标志（保持原有逻辑用于PTT模式）
                        if !is_responding_now && has_sent_completed && !matches!(self.speech_mode, crate::asr::SpeechMode::Vad) {
                            has_sent_completed_atomic.store(false, std::sync::atomic::Ordering::Release);
                            info!("🔄 重置has_sent_completed标志，允许新语音输入（非VAD模式）");
                        }

                        // 🔧 系统正在响应时跳过超时处理
                        if is_responding_now {
                            info!(
                                "🛑 VAD超时但系统正在响应(TTS/LLM)，跳过超时处理以避免自打断: is_responding={}, has_sent_completed={}",
                                is_responding_now,
                                has_sent_completed_atomic.load(std::sync::atomic::Ordering::Acquire)
                            );
                            // 不在此处重置 has_sent_completed_atomic，等待TTS完成后由TTS侧复位
                            continue;
                        }

                        // 🔧 注意：VAD模式下的has_sent_completed检查已在上面提前处理，这里只处理其他模式
                        if has_sent_completed_atomic.load(std::sync::atomic::Ordering::Acquire) && !matches!(self.speech_mode, crate::asr::SpeechMode::Vad) {
                            info!(
                                "🛑 非VAD模式下已发送completed，跳过超时处理: speech_mode={:?}",
                                self.speech_mode
                            );
                            continue;
                        }

                        // 🆕 序列号检查：忽略延迟的超时事件
                        let current_sequence = self.current_turn_sequence.load(std::sync::atomic::Ordering::SeqCst);
                        let global_sequence = self.turn_sequence_number.load(std::sync::atomic::Ordering::SeqCst);
                        if current_sequence > 0 && current_sequence < global_sequence {
                            info!("🛑 忽略延迟的VAD超时事件: 当前轮次={}, 全局序列号={}", current_sequence, global_sequence);
                            continue;
                        }

                        // 🔧 时序修复：使用ASR任务启动时保存的response_id，确保时序安全
                        // 使用 load 进行无锁只读访问，不消耗数据
                        let _target_response_id_for_timeout = self.current_turn_response_id.load();

                        // 🆕 检查VAD是否曾经触发过
                        let vad_ever_triggered = vad_has_triggered.load(std::sync::atomic::Ordering::Acquire);

                        // 🆕 使用简化的打断机制处理VAD超时
                        let session_id_for_timeout = self.session_id.clone();
                        let start_time = std::time::Instant::now();
                        match self
                            .simple_interrupt_manager
                            .broadcast_global_interrupt(session_id_for_timeout.clone(), SimpleInterruptReason::SessionTimeout)
                        {
                            Ok(()) => {
                                let elapsed = start_time.elapsed();
                                info!("✅ 简化机制VAD超时打断信号已广播 (耗时: {:?})", elapsed);
                            },
                            Err(e) => {
                                error!("简化机制VAD超时打断信号广播失败: {}", e);
                            },
                        }

                        // 🔧 关键修复：处理超时事件（触发finalize）
                        // 🚀 重要修复：VAD模式下如果已发送completed，完全跳过finalize，防止重复处理
                        if !skip_finalize_in_vad {
                            if let Err(e) = asr_session.finalize(&mut callback).await {
                                error!("VAD超时finalize失败: {}", e);
                            } else {
                                info!("✅ VAD超时finalize成功: {}", self.session_id);
                            }
                        } else {
                            info!("🛡️ VAD模式已发送completed，跳过finalize避免重复处理");
                        }

                        // 🆕 在 finalize 之后重新检查是否已发送 completed（避免先completed后又failed）
                        let has_sent_completed_after = has_sent_completed_atomic.load(std::sync::atomic::Ordering::Acquire);

                        // 🆕 如果VAD从未触发，向前端发送提示事件（仅在未发送completed的情况下）
                        if !vad_ever_triggered && !skip_finalize_in_vad && !has_sent_completed_after {
                            info!("🔇 VAD超时且从未检测到语音，发送提示事件给前端");
                            let emitter_timeout = self.emitter.clone();
                            tokio::spawn(async move {
                                emitter_timeout.error_event(1001, "没有检测到语音输入，请检查麦克风或重新尝试").await;
                            });
                        } else if vad_ever_triggered && !skip_finalize_in_vad {
                            // 🆕 VAD已触发但ASR为空时，发送ASR转录失败信令（仅在未发送completed的情况下）
                            if accumulated_text_buffer.trim().is_empty() && !has_sent_completed_after {
                                info!("🔇 VAD超时且ASR为空，发送ASR转录失败信令");
                                let emitter_timeout = self.emitter.clone();
                                let current_user_item_id = current_user_item_id.clone();
                                tokio::spawn(async move {
                                    if let Some(item_id) = current_user_item_id {
                                        emitter_timeout.asr_transcription_failed(&item_id, 0, "timeout_no_output", "VAD超时且ASR转录为空").await;
                                    } else {
                                        emitter_timeout.error_event(1003, "VAD超时且ASR转录为空").await;
                                    }
                                });
                            } else if has_sent_completed_after {
                                info!("🛡️ 已在finalize回调中发送completed，跳过VAD超时的failed事件");
                            }
                        } else if skip_finalize_in_vad {
                            info!("🛡️ VAD模式已发送completed，跳过超时错误事件发送，避免干扰正常流程");
                        }

                        // 🔧 重置VAD触发标志，为下一轮对话做准备（仅在未发送completed的情况下）
                        if !skip_finalize_in_vad {
                            vad_has_triggered.store(false, std::sync::atomic::Ordering::Release);
                            info!("🔄 已重置VAD触发标志，为下一轮对话做准备");

                            // 🔧 超时事件处理后，重置ASR会话状态，允许下次音频输入时重新启动监控
                            asr_session.reset().await;
                            info!("🔄 VAD超时事件已处理，已清空接收器，下次音频输入时将重新启动监控");
                        } else {
                            info!("🛡️ VAD模式已发送completed，跳过会话重置，保持当前状态直到新语音开始");
                        }

                        // 🔧 VAD模式专用：超时处理完成后重置has_sent_completed标志，允许下一轮对话
                        // 🚀 关键修复：只有在当前轮次确实没有发送completed的情况下才重置标志
                        // 如果已经发送了completed，保持标志为true，防止网络延迟期间的音频数据触发第二次completed
                        if matches!(self.speech_mode, crate::asr::SpeechMode::Vad) {
                            let current_has_sent = has_sent_completed_atomic.load(std::sync::atomic::Ordering::Acquire);
                            if !current_has_sent {
                                has_sent_completed_atomic.store(false, std::sync::atomic::Ordering::Release);
                                info!("🔄 VAD模式：重置has_sent_completed标志，为下一轮语音输入做准备");
                            } else {
                                info!("🛡️ VAD模式：当前轮次已发送completed，保持保护状态，防止网络延迟期间的重复事件");
                                // 🔧 重要说明：保持has_sent_completed=true的状态，直到下一次VAD Speaking->Silence正常流程重置
                                // 这是防止客户端麦克风锁定的关键保护机制
                            }
                        }
                    }

                    // 清理信号事件：会话销毁时的清理
                    _ = self.cleanup_rx.recv() => {
                        info!("🧹 ASR任务收到清理信号，开始清理AsrSession: {}", self.session_id);

                        // 🔧 使用cleanup而不是reset，确保VAD超时监控任务被正确停止
                        asr_session.cleanup().await;
                        info!("✅ AsrSession已彻底清理，VAD超时监控任务已停止: {}", self.session_id);
                        break;
                    }
                    // 🆕 ASR 语言更新（非阻塞），使用智能模型选择
                    Ok(_) = asr_language_rx.as_mut().unwrap().changed(), if asr_language_rx.is_some() => {
                        if let Some(ref rx) = asr_language_rx {
                            let new_lang = rx.borrow().clone();
                            if new_lang != self.asr_language {
                                info!("🔄 检测到 ASR 语言变更，立即重建会话以使用智能模型选择: {:?} -> {:?}", self.asr_language, new_lang);
                                self.asr_language = new_lang.clone();

                                // 立即重建ASR会话，使用智能模型选择
                                match self.asr_engine.create_session_with_auto_model_selection(
                                    self.session_id.clone(),
                                    self.speech_mode,
                                    new_lang,
                                    self.current_turn_response_id.clone(),
                                ).await {
                                    Ok(new_session) => {
                                        info!("✅ ASR会话已根据新语言重建并使用智能模型选择");
                                        // 清理旧会话
                                        asr_session.cleanup().await;
                                        // 替换为新会话
                                        asr_session = new_session;

                                        // 如果是PTT模式且已经开始，需要在新会话中调用begin_speech
                                        if self.speech_mode == crate::asr::SpeechMode::PushToTalk && ptt_started {
                                            asr_session.begin_speech();
                                            info!("🎙️ 新ASR会话PTT begin_speech 已调用");
                                        }
                                    },
                                    Err(e) => {
                                        error!("❌ 重建ASR会话失败: {}", e);
                                        // 保持使用旧会话
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
                // 没有VAD超时接收器时，不包含VAD超时分支，避免无意义轮询
                tokio::select! {
                    // 统一输入消息：音频数据或PTT事件（保证顺序）
                    Some(input_msg) = self.input_rx.recv() => {
                        match input_msg {
                            AsrInputMessage::PttEnd => {
                                info!("🛑 收到PTT End 事件，模拟VAD结束，立即调用end_speech");

                                if let Err(e) = asr_session.end_speech(&mut callback).await {
                                    error!("PTT end_speech 失败: {}", e);
                                    // 发送ASR end_speech失败事件
                                    let emitter = self.emitter.clone();
                                    let item_id = current_user_item_id.clone().unwrap_or_else(|| "unknown".to_string());
                                    let error_msg = e.to_string();
                                    tokio::spawn(async move {
                                        emitter.asr_transcription_failed(&item_id, 0, "END_SPEECH_ERROR", &error_msg).await;
                                    });
                                } else {
                                    info!("✅ PTT end_speech 调用成功，ASR最终结果已通过callback处理");
                                    // PTT End成功后，ASR已通过callback返回最终结果
                                    // 现在可以结束当前轮次，准备下一轮

                                    // 🚀 PTT模式专门处理：检查累积文本并发送到并行处理任务
                                    if !accumulated_text_buffer.trim().is_empty() && !Self::is_only_punctuation(&accumulated_text_buffer) && !has_sent_completed_atomic.load(std::sync::atomic::Ordering::Acquire) {
                                        info!("🔍 PTT End后检查累积文本: '{}'", accumulated_text_buffer.chars().take(50).collect::<String>());

                                        // 生成新的对话上下文（类似callback中的逻辑）
                                        let user_item_id = current_user_item_id
                                            .clone()
                                            .unwrap_or_else(|| format!("msg_{}", nanoid::nanoid!(6)));
                                        let assistant_item_id = format!("asst_{}", nanoid::nanoid!(6));
                                        let response_id = format!("resp_{}", nanoid::nanoid!(8));

                                        // 🔧 修复：PTT模式下发送缺失的ASR转录信令
                                        info!("📤 PTT End发送ASR转录信令: item_id={}", user_item_id);
                                        let emitter_asr = self.emitter.clone();
                                        let user_item_id_for_asr = user_item_id.clone();
                                        let accumulated_text_for_asr = accumulated_text_buffer.clone();
                                        tokio::spawn(async move {
                                            // 发送 conversation.item.input_audio_transcription.completed
                                            emitter_asr
                                                .conversation_item_input_audio_transcription_completed(&user_item_id_for_asr, 0, &accumulated_text_for_asr)
                                                .await;

                                            // 更新 conversation.item 状态为 completed
                                            emitter_asr
                                                .conversation_item_updated(&user_item_id_for_asr, "user", "completed")
                                                .await;
                                        });

                                        // 🔧 关键修复：PTT End时预先开始新轮次，获取正确的轮次序列号
                                        let new_turn_sequence = self.simple_interrupt_manager.start_new_turn();
                                        self.current_turn_sequence.store(new_turn_sequence, std::sync::atomic::Ordering::Release);
                                        let ctx = TurnContext::new(user_item_id.clone(), assistant_item_id, response_id.clone(), Some(new_turn_sequence));
                                        info!("🔄 PTT End创建TurnContext with turn_sequence={} (新轮次)", new_turn_sequence);

                                        // 🔧 关键修复：将新的response_id存储到LockfreeResponseId中
                                        self.current_turn_response_id.store(Some(response_id.clone()));
                                        info!("🔒 PTT End存储新的response_id到LockfreeResponseId: {}", response_id);

                                        // 发送到并行处理任务
                                        if let Some(ref parallel_tts_tx) = self.parallel_tts_tx {
                                            info!("🔍 PTT End准备发送到并行处理任务: '{}'", accumulated_text_buffer.chars().take(50).collect::<String>());
                                            if parallel_tts_tx.send((ctx.clone(), accumulated_text_buffer.clone())).is_err() {
                                                error!("⚠️ PTT End发送到并行处理任务失败：通道已关闭");
                                            } else {
                                                info!("✅ PTT End已成功发送用户消息到并行处理任务: '{}'", accumulated_text_buffer.chars().take(50).collect::<String>());
                                                info!("🔄 并行处理任务将负责转发到LLM和创建TTS会话");
                                                // 标记已发送，避免重复
                                                has_sent_completed_atomic.store(true, std::sync::atomic::Ordering::Release);

                                                // 🧹 PTT模式专门清理：发送成功后清理文本缓冲区和相关状态
                                                accumulated_text_buffer.clear();
                                                has_intermediate_results = false;
                                                conversation_item_created_sent = false;
                                                info!("🧹 PTT End发送成功后已清理：文本缓冲区、中间结果标志、对话项创建标志");
                                            }
                                        } else {
                                            error!("❌ PTT End时并行处理任务通道未配置");
                                        }
                                    } else {
                                        info!("🔍 PTT End时无有效文本或已发送，跳过并行处理任务");
                                    }

                                    // 🔧 重置状态，准备下一轮对话
                                    ptt_started = false;
                                    first_audio_chunk_received = false;
                                    consecutive_errors.store(0, std::sync::atomic::Ordering::SeqCst);

                                    // 🔧 修复：PTT轮次结束，新轮次已在发送到并行处理任务时开始
                                    info!("🔄 PTT轮次结束，新轮次已在并行处理任务发送时开始: sequence={}", self.current_turn_sequence.load(std::sync::atomic::Ordering::Acquire));
                                }
                            }
                            AsrInputMessage::DirectText(_) => {
                                // DirectText 不应该到达ASR任务，记录警告并忽略
                                warn!("⚠️ ASR任务收到DirectText消息，这不应该发生，已忽略");
                                continue;
                            }
                            AsrInputMessage::Audio(audio_chunk) => {
                        // 🆕 添加调试日志：确认音频数据接收

                        // 🆕 自动开始PTT语音段: 如果处于PushToTalk模式且尚未开始，自动调用begin_speech()
                        if self.speech_mode == crate::asr::SpeechMode::PushToTalk && !ptt_started {
                            ptt_started = true;
                            asr_session.begin_speech();
                            info!("🎙️ 自动触发 PTT begin_speech (首个音频分片)");

                            // 🔧 关键修复：在PTT新轮次开始时重置has_sent_completed_atomic
                            // 这确保多轮PTT对话场景下，每次新的语音段都能正确处理
                            has_sent_completed_atomic.store(false, std::sync::atomic::Ordering::Release);
                            debug!("🔄 PTT新轮次开始，重置has_sent_completed_atomic为false");

                            // 🚀 PTT模式打断逻辑：发送UserPtt打断信号，目标指向"当前轮次"停止正在播放的TTS
                            let current_turn_for_interrupt = self.simple_interrupt_manager.current_turn();
                            if current_turn_for_interrupt > 0 {
                                let target_turn = current_turn_for_interrupt;
                                info!(
                                    "🔄 PTT检测到活跃轮次({}), 发送UserPtt打断信号 -> 目标轮次({})",
                                    current_turn_for_interrupt,
                                    target_turn
                                );
                                let simple_interrupt_manager_for_ptt = self.simple_interrupt_manager.clone();
                                let session_id_for_ptt_interrupt = self.session_id.clone();
                                tokio::spawn(async move {
                                    let start_time = std::time::Instant::now();
                                    let result = simple_interrupt_manager_for_ptt.broadcast_global_interrupt_with_turn(
                                        session_id_for_ptt_interrupt,
                                        crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::InterruptReason::UserPtt,
                                        target_turn
                                    );
                                    let elapsed = start_time.elapsed();

                                    match result {
                                        Ok(_) => {
                                            info!("✅ PTT打断信号发送成功: target_turn={} (当前轮次), elapsed={:?}", target_turn, elapsed);
                                        }
                                        Err(e) => {
                                            error!("❌ PTT打断信号发送失败: target_turn={} (当前轮次), error={}, elapsed={:?}", target_turn, e, elapsed);
                                        }
                                    }
                                });
                            } else {
                                info!("🔄 PTT无活跃轮次，跳过打断信号");
                            }
                        }

                        // PTT End现在立即处理，不再需要防御性检查

                        // 标记已收到首个音频分片，后续才启动超时计时器
                        if !first_audio_chunk_received {
                            first_audio_chunk_received = true;
                        }

                        // 🎯 TRACE: 调用ASR引擎处理音频
                        // info!("🎤 [TRACE-AUDIO] 调用ASR引擎处理音频 | session_id={} | samples={}",
                        //       self.session_id, audio_chunk.len());

                        match asr_session
                            .process_audio_chunk(audio_chunk, &mut callback)
                            .await
                        {
                            Ok(()) => {
                                // 🔧 成功处理音频块时重置错误计数器
                                consecutive_errors.store(0, std::sync::atomic::Ordering::SeqCst);

                                // PTT End现在立即处理，不再需要延迟处理逻辑
                            },
                            Err(e) => {
                                error!("处理音频块失败: {}", e);

                                // 🔧 添加ASR失败监控指标
                                crate::monitoring::record_asr_failure();

                                // 🔧 改进错误处理：统计连续错误次数，避免因单次错误退出
                                let consecutive_count = consecutive_errors.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                                const MAX_CONSECUTIVE_ERRORS: u32 = 5;

                                if consecutive_count >= MAX_CONSECUTIVE_ERRORS {
                                    error!("❌ ASR任务连续{}次错误，退出任务: session={}", consecutive_count, self.session_id);

                                    // 发送ASR失败事件
                                    let emitter_err = self.emitter.clone();
                                    let item_id_err = current_user_item_id.clone().unwrap_or_else(|| "unknown".to_string());
                                    let error_msg = format!("连续{}次处理音频块失败: {}", consecutive_count, e);
                                    tokio::spawn(async move {
                                        emitter_err.asr_transcription_failed(&item_id_err, 0, "PROCESS_ERROR", &error_msg).await;
                                    });
                                    break;
                                } else {
                                    warn!("⚠️ ASR处理音频块失败({}/{}次)，继续尝试: session={}, error={}",
                                          consecutive_count, MAX_CONSECUTIVE_ERRORS, self.session_id, e);

                                    // 继续处理下一个音频块，不退出
                                    continue;
                                }
                            }
                        }
                    }
                        }
                    }

                    // 🆕 ConnectionLost事件：WebSocket重连时重置has_sent_completed标志
                    Ok(event) = interrupt_rx.recv() => {
                        if event.session_id == self.session_id &&
                           matches!(event.reason, super::simple_interrupt_manager::InterruptReason::ConnectionLost) {
                            info!("🔄 ASR收到ConnectionLost事件，重置has_sent_completed标志: session={}, event_id={}",
                                  self.session_id, event.event_id);
                            has_sent_completed_atomic.store(false, std::sync::atomic::Ordering::Release);
                            // 清空状态，为新连接做准备（与 asr_task_vad.rs 保持一致）
                            accumulated_text_buffer.clear();
                            current_user_item_id = None;
                            conversation_item_created_sent = false;
                            current_user_content_index = 0;
                            info!("✅ ConnectionLost处理完成，ASR已准备好处理新的语音输入");
                        } else {
                            debug!("🔄 ASR忽略不相关的打断事件: reason={:?}, session={}", event.reason, event.session_id);
                        }
                    }

                    // 清理信号事件：会话销毁时的清理
                    _ = self.cleanup_rx.recv() => {
                        info!("🧹 ASR任务收到清理信号，开始清理AsrSession: {}", self.session_id);

                        // 🔧 使用cleanup而不是reset，确保VAD超时监控任务被正确停止
                        asr_session.cleanup().await;
                        info!("✅ AsrSession已彻底清理，VAD超时监控任务已停止: {}", self.session_id);
                        break;
                    }

                }
                info!("🛑 ASR任务退出: session={}, 原因=输入通道关闭", self.session_id);
            }
        }

        info!("ASR task for session {} finished.", self.session_id);
        let _ = self.task_completion_tx.send(TaskCompletion::Asr);
        Ok(())
    }
}
