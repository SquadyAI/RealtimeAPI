//! Higher-level agents that orchestrate LLM prompts and return structured results.

pub mod audio_recorder_agent;
pub mod camera_photo_agent;
pub mod camera_video_agent;
pub mod device_control_agent;
pub mod fallback_agent;
pub mod goodbye_agent;
pub mod media_agent;
pub mod music_control_agent;
pub mod navigation_agent;
pub mod photo_agent;
pub mod rejection_agent;
pub mod reminder_agent;
pub mod role_extractor;
pub mod runtime;
pub mod search_agent;
pub mod stream_utils;
pub mod system_prompt_registry;
pub mod tool_utils;
pub mod translate_agent;
pub mod turn_tracker;
pub mod volume_agent;
pub mod volume_down_agent;
pub mod volume_up_agent;

// Prompts 模块，每个 agent 的 prompt 定义在独立的文件中
pub(crate) mod prompts;

pub use audio_recorder_agent::AudioRecorderAgent;
pub use camera_photo_agent::CameraPhotoAgent;
pub use camera_video_agent::CameraVideoAgent;
pub use device_control_agent::DeviceControlAgent;
pub use fallback_agent::FallbackAgent;
pub use goodbye_agent::GoodbyeAgent;
pub use media_agent::{MediaAgent, MediaAgentLock, MediaItem, MediaSource};
pub use music_control_agent::MusicControlAgent;
pub use navigation_agent::NavigationAgent;
pub use photo_agent::PhotoAgent;
pub use rejection_agent::RejectionAgent;
pub use reminder_agent::ReminderAgent;
pub use runtime::{
    Agent, AgentCancelToken, AgentCancellationToken, AgentContext, AgentExtra, AgentHandles, AgentRegistry, AgentToolClient, AgentTtsSink, BroadcastAgentTtsSink, RuntimeToolClient, ToolCallOutcome,
    ToolControl,
};
pub use search_agent::SearchAgent;
pub use system_prompt_registry::SystemPromptRegistry;
pub use translate_agent::TranslateAgent;
pub use turn_tracker::{
    SessionContext, ToolControlMode, TurnRecord, TurnStatus, TurnTracker, clear_session, get_llm_messages, get_or_create_tracker, has_session, init_session, remove_tracker, update_system_prompt,
};
pub use volume_agent::VolumeControlAgent;
pub use volume_down_agent::VolumeDownAgent;
pub use volume_up_agent::VolumeUpAgent;
