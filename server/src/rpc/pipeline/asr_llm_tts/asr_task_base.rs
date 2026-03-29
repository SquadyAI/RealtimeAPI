//! Shared base config and helpers for ASR task variants (PTT, VAD, VAD-deferred).
//!
//! All three active ASR task types share the same 14 fields. This module extracts
//! them into `BaseAsrTaskConfig` so each variant only declares its unique fields.

use std::sync::Arc;
use tokio::sync::{mpsc, watch};

use crate::asr::AsrEngine;
use crate::rpc::session_router::SessionRouter;

use super::event_emitter::EventEmitter;
use super::lockfree_response_id::LockfreeResponseId;
use super::simple_interrupt_manager::{SimpleInterruptHandler, SimpleInterruptManager};
use super::types::{SharedFlags, TaskCompletion, TurnContext};

/// Re-export `is_only_punctuation` from the canonical location so callers
/// can use `asr_task_base::is_only_punctuation` without hunting for the path.
pub use crate::asr::punctuation::is_only_punctuation;

/// Shared configuration / channels that every ASR task variant needs.
pub struct BaseAsrTaskConfig {
    pub session_id: String,
    pub asr_engine: Arc<AsrEngine>,
    pub emitter: Arc<EventEmitter>,
    pub router: Arc<SessionRouter>,
    pub input_rx: mpsc::Receiver<super::asr_task_core::AsrInputMessage>,
    pub shared_flags: Arc<SharedFlags>,
    pub task_completion_tx: mpsc::UnboundedSender<TaskCompletion>,
    pub simple_interrupt_manager: Arc<SimpleInterruptManager>,
    pub simple_interrupt_handler: Option<SimpleInterruptHandler>,
    pub cleanup_rx: mpsc::UnboundedReceiver<()>,
    pub asr_language: Option<String>,
    pub asr_language_rx: Option<watch::Receiver<Option<String>>>,
    pub current_turn_response_id: Arc<LockfreeResponseId>,
    pub parallel_tts_tx: Option<mpsc::UnboundedSender<(TurnContext, String)>>,
}

/// Smart text merge: handle overlap and concatenation of ASR results.
#[allow(dead_code)]
pub fn smart_text_merge(existing: &str, new: &str) -> String {
    if existing.trim().is_empty() {
        return new.to_string();
    }
    if new.trim().is_empty() {
        return existing.to_string();
    }

    if new.contains(existing) {
        return new.to_string();
    }
    if existing.contains(new) {
        return existing.to_string();
    }

    let punct_set = "\u{ff0c}\u{3002}\u{ff01}\u{ff1f}\u{ff1b}\u{ff1a}\u{3001},.!?;:~\u{2026}";
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
        if new_core.starts_with(existing_core.as_str()) || new_core.contains(existing_core.as_str()) {
            return new.to_string();
        }
        if existing_core.starts_with(new_core.as_str()) || existing_core.contains(new_core.as_str()) {
            return existing.to_string();
        }
    }

    // Case-insensitive overlap concatenation
    let existing_lower: Vec<char> = existing.to_lowercase().chars().collect();
    let new_lower: Vec<char> = new.to_lowercase().chars().collect();
    let min_len = existing_lower.len().min(new_lower.len());
    let mut best_overlap = 0;
    for overlap_len in (1..=min_len).rev() {
        let existing_suffix = &existing_lower[existing_lower.len().saturating_sub(overlap_len)..];
        let new_prefix = &new_lower[..overlap_len];
        if existing_suffix == new_prefix {
            best_overlap = overlap_len;
            break;
        }
    }
    if best_overlap > 0 {
        let merged = format!("{}{}", existing, new.chars().skip(best_overlap).collect::<String>());
        return merged;
    }

    // Boundary space handling
    let existing_trimmed = existing.trim();
    let new_trimmed = new.trim();
    let needs_space = {
        let left = existing_trimmed.chars().rev().find(|c| !c.is_whitespace());
        let right = new_trimmed.chars().find(|c| !c.is_whitespace());
        match (left, right) {
            (Some(a), Some(b)) => a.is_alphanumeric() && b.is_alphanumeric(),
            _ => false,
        }
    };
    if needs_space {
        format!("{} {}", existing_trimmed, new_trimmed)
    } else {
        format!("{}{}", existing_trimmed, new_trimmed)
    }
}

/// Remove consecutive duplicate words/phrases (e.g. "Hello Hello" -> "Hello").
pub fn remove_consecutive_duplicates(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }

    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 2 {
        return text.to_string();
    }

    // Deduplicate consecutive identical words
    let mut result: Vec<&str> = Vec::new();
    for word in &words {
        if result.last() != Some(word) {
            result.push(word);
        }
    }

    if result.len() < words.len() {
        return result.join(" ");
    }

    // Deduplicate consecutive identical phrases (e.g. "Hello World Hello World")
    let half = words.len() / 2;
    for phrase_len in (1..=half).rev() {
        if words.len() >= phrase_len * 2 {
            let first_phrase = &words[..phrase_len];
            let second_phrase = &words[phrase_len..phrase_len * 2];
            if first_phrase == second_phrase {
                return first_phrase.join(" ");
            }
        }
    }

    text.to_string()
}
