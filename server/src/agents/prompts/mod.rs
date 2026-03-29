//! Agent System Prompts - 每个 agent 的独立 prompt 文件
//!
//! 每个 agent 有独立的文件，文件中定义多语言版本的 system prompt

pub mod assistant;
pub mod device_control;
pub mod goodbye;
pub mod music;
pub mod navigation;
pub mod photo;
pub mod rejection;
pub mod reminder;
pub mod search;
pub mod visual_qa;
pub mod volume;

use rustc_hash::FxHashMap;

/// 获取所有 prompts 的映射
/// Key: (agent_id, language) -> prompt
pub fn all_prompts() -> FxHashMap<(String, String), String> {
    let mut prompts = FxHashMap::default();

    // Search Agent
    for (lang, prompt) in search::prompts() {
        prompts.insert(("agent.search.query".to_string(), lang), prompt);
    }

    // Volume Control Agent
    for (lang, prompt) in volume::prompts() {
        prompts.insert(("agent.volume.control".to_string(), lang), prompt);
    }

    // Music Agent
    for (lang, prompt) in music::prompts() {
        prompts.insert(("agent.media.playmusic".to_string(), lang), prompt);
    }

    // Reminder Agent
    for (lang, prompt) in reminder::prompts() {
        prompts.insert(("agent.reminder".to_string(), lang), prompt);
    }

    // Device Control Agent
    for (lang, prompt) in device_control::prompts() {
        prompts.insert(("agent.device.control".to_string(), lang), prompt);
    }

    // Navigation Agent
    for (lang, prompt) in navigation::prompts() {
        prompts.insert(("agent.navigation.direction".to_string(), lang), prompt);
    }

    // Photo Agent
    for (lang, prompt) in photo::prompts() {
        prompts.insert(("agent.qa.visual".to_string(), lang), prompt);
    }

    // Rejection Agent
    for (lang, prompt) in rejection::prompts() {
        prompts.insert(("agent.rejection.response".to_string(), lang), prompt);
    }

    // Goodbye Agent
    for (lang, prompt) in goodbye::prompts() {
        prompts.insert(("agent.conversation.end".to_string(), lang), prompt);
    }

    // Assistant (主系统提示词)
    for (lang, prompt) in assistant::prompts() {
        prompts.insert(("assistant".to_string(), lang), prompt);
    }

    // Visual QA (视觉问答提示词)
    for (lang, prompt) in visual_qa::prompts() {
        prompts.insert(("visual_qa".to_string(), lang), prompt);
    }

    prompts
}
