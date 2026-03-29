// 新版 ASR+LLM+TTS Pipeline 子模块骨架
// 目前仅包含类型定义与事件发送器，后续将引入 asr_task / llm_task / tts_task 等子模块。

pub mod asr_task;
pub mod asr_task_base;
pub mod asr_task_core;
pub mod asr_task_ptt;
pub mod asr_task_vad;
pub mod asr_task_vad_deferred;
pub mod audio_blocking_service;
pub mod event_emitter;
pub mod guided_choice_selector; // 🆕 Guided Choice选择器工具
pub mod intent;
pub mod interrupt_service;
pub mod lockfree_response_id; // 🔧 无锁 ResponseId 管理器
pub mod orchestrator;
pub mod routing;
pub mod sentence_queue; // 句子队列管理
pub mod session_audio_sender; // 会话级别音频发送器
pub mod session_data_integration; // 🆕 会话数据持久化集成
pub mod simple_interrupt_manager; // 简化的打断管理器
pub mod timing_manager;
pub mod tool_call_manager;
pub mod tts_controller;
pub mod tts_task;
pub mod types; // 🆕 计时管理器模块

pub use asr_task_ptt::AsrTaskPtt;
pub use asr_task_vad::AsrTaskVad;
pub use asr_task_vad_deferred::AsrTaskVadDeferred;
pub use audio_blocking_service::{AudioBlockingActivator, AudioBlockingChecker, AudioBlockingService};
pub use event_emitter::EventEmitter;
pub use guided_choice_selector::{GuidedChoiceSelector, SelectorConfig}; // 🆕 导出Guided Choice选择器
pub use interrupt_service::InterruptService;
pub use lockfree_response_id::{LockfreeResponseId, LockfreeResponseIdReader}; // 🔧 导出无锁 ResponseId 管理器
// 🆕 简化打断管理器的导出
// LlmTask (V1) 已弃用，所有管线已迁移到 LlmTaskV2
pub use orchestrator::ModularPipeline;
pub use sentence_queue::NextSentenceTrigger; // 导出下一句触发器（paced_sender 需要）
pub use session_data_integration::{save_tts_audio_globally, save_user_audio_globally}; // 🆕 会话数据持久化全局函数
pub use simple_interrupt_manager::{InterruptReason as SimpleInterruptReason, SimpleInterruptEvent, SimpleInterruptHandler, SimpleInterruptManager, TurnSequenceManager};
pub use timing_manager::{TimingNode, cleanup_session_timing, print_timing_report, record_node_time, record_node_time_and_try_report, record_vad_trigger, reset_session_timing};
pub use tool_call_manager::ToolCallManager;
pub use tts_task::TtsTask;
pub use types::{SharedFlags, TaskCompletion, TurnContext};
