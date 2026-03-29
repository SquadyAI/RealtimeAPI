use crate::asr::SpeechMode;
use crate::rpc::protocol::{MessagePayload, ProtocolId, WebSocketMessage};

#[derive(Debug, Clone)]
pub struct NormalizedStartRequest {
    pub session_id: String,
    pub protocol_id: ProtocolId,
    pub speech_mode: SpeechMode,
    pub payload: Option<MessagePayload>,
}

pub struct StartRequestNormalizer;

impl StartRequestNormalizer {
    pub fn normalize(input: &WebSocketMessage) -> NormalizedStartRequest {
        let payload = normalize_payload(input);
        let speech_mode = resolve_speech_mode(payload.as_ref());

        NormalizedStartRequest {
            session_id: input.session_id.clone(),
            protocol_id: input.protocol_id,
            speech_mode,
            payload,
        }
    }
}

fn normalize_payload(input: &WebSocketMessage) -> Option<MessagePayload> {
    let top_timezone = input.timezone.clone();
    let top_location = input.location.clone();

    if top_timezone.is_none() && top_location.is_none() {
        return input.payload.clone();
    }

    tracing::info!(
        "🧭 取到顶层timezone/location，payload: tz={:?}, loc={:?}",
        top_timezone,
        top_location
    );

    match &input.payload {
        Some(MessagePayload::SessionConfig {
            mode,
            vad_threshold,
            silence_duration_ms,
            min_speech_duration_ms,
            system_prompt,
            mcp_server_config,
            tools_endpoint,
            prompt_endpoint,
            tools,
            tool_choice,
            enable_search,
            search_config,
            voice_setting,
            asr_language,
            timezone,
            location,
            audio_chunk_size_kb,
            initial_burst_count,
            initial_burst_delay_ms,
            send_rate_multiplier,
            output_audio_config,
            input_audio_config,
            text_done_signal_only,
            signal_only,
            asr_chinese_convert,
            tts_chinese_convert,
            from_language,
            to_language,
            offline_tools,
        }) => {
            let merged_timezone = preferred_string(timezone, &top_timezone);
            let merged_location = preferred_string(location, &top_location);

            Some(MessagePayload::SessionConfig {
                mode: mode.clone(),
                vad_threshold: *vad_threshold,
                silence_duration_ms: *silence_duration_ms,
                min_speech_duration_ms: *min_speech_duration_ms,
                system_prompt: system_prompt.clone(),
                mcp_server_config: mcp_server_config.clone(),
                tools_endpoint: tools_endpoint.clone(),
                prompt_endpoint: prompt_endpoint.clone(),
                tools: tools.clone(),
                tool_choice: tool_choice.clone(),
                enable_search: *enable_search,
                search_config: search_config.clone(),
                voice_setting: voice_setting.clone(),
                asr_language: asr_language.clone(),
                timezone: merged_timezone,
                location: merged_location,
                audio_chunk_size_kb: *audio_chunk_size_kb,
                initial_burst_count: *initial_burst_count,
                initial_burst_delay_ms: *initial_burst_delay_ms,
                send_rate_multiplier: *send_rate_multiplier,
                output_audio_config: output_audio_config.clone(),
                input_audio_config: input_audio_config.clone(),
                text_done_signal_only: *text_done_signal_only,
                signal_only: *signal_only,
                asr_chinese_convert: asr_chinese_convert.clone(),
                tts_chinese_convert: tts_chinese_convert.clone(),
                from_language: from_language.clone(),
                to_language: to_language.clone(),
                offline_tools: offline_tools.clone(),
            })
        },
        other => {
            tracing::warn!("⚠️ 收到没有SessionConfig的payload，tz/loc 字段被忽略");

            if top_timezone.is_some() || top_location.is_some() {
                Some(MessagePayload::SessionConfig {
                    mode: None,
                    vad_threshold: None,
                    silence_duration_ms: None,
                    min_speech_duration_ms: None,
                    system_prompt: None,
                    mcp_server_config: None,
                    tools_endpoint: None,
                    prompt_endpoint: None,
                    tools: None,
                    tool_choice: None,
                    enable_search: None,
                    search_config: None,
                    voice_setting: None,
                    asr_language: None,
                    timezone: top_timezone,
                    location: top_location,
                    audio_chunk_size_kb: None,
                    initial_burst_count: None,
                    initial_burst_delay_ms: None,
                    send_rate_multiplier: None,
                    output_audio_config: None,
                    input_audio_config: None,
                    text_done_signal_only: None,
                    signal_only: None,
                    asr_chinese_convert: None,
                    tts_chinese_convert: None,
                    from_language: None,
                    to_language: None,
                    offline_tools: None,
                })
            } else {
                other.clone()
            }
        },
    }
}

fn preferred_string(payload_value: &Option<String>, top_level_value: &Option<String>) -> Option<String> {
    if has_text(payload_value) {
        payload_value.clone()
    } else {
        top_level_value.clone().filter(|value| !value.trim().is_empty())
    }
}

fn has_text(value: &Option<String>) -> bool {
    value.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false)
}

fn resolve_speech_mode(payload: Option<&MessagePayload>) -> SpeechMode {
    if let Some(MessagePayload::SessionConfig { mode: Some(mode), .. }) = payload {
        match mode.as_str() {
            "ptt" => SpeechMode::PushToTalk,
            "vad_deferred" => SpeechMode::VadDeferred,
            _ => SpeechMode::Vad,
        }
    } else {
        SpeechMode::Vad
    }
}

#[cfg(test)]
mod tests {
    use super::StartRequestNormalizer;
    use crate::asr::SpeechMode;
    use crate::rpc::protocol::{CommandId, MessagePayload, ProtocolId, WebSocketMessage};

    #[test]
    fn top_level_timezone_and_location_fill_blank_payload_values() {
        let message = WebSocketMessage {
            protocol_id: ProtocolId::All,
            command_id: CommandId::Start,
            session_id: "session-1".to_string(),
            payload: Some(session_config(Some("vad"), Some(""), Some("   "))),
            timezone: Some("Asia/Shanghai".to_string()),
            location: Some("Shanghai".to_string()),
        };

        let normalized = StartRequestNormalizer::normalize(&message);

        match normalized.payload {
            Some(MessagePayload::SessionConfig { timezone, location, .. }) => {
                assert_eq!(timezone.as_deref(), Some("Asia/Shanghai"));
                assert_eq!(location.as_deref(), Some("Shanghai"));
            },
            other => panic!("expected session config, got {:?}", other),
        }
    }

    #[test]
    fn payload_values_take_precedence_when_present() {
        let message = WebSocketMessage {
            protocol_id: ProtocolId::All,
            command_id: CommandId::Start,
            session_id: "session-1".to_string(),
            payload: Some(session_config(Some("vad"), Some("America/New_York"), Some("New York"))),
            timezone: Some("Asia/Shanghai".to_string()),
            location: Some("Shanghai".to_string()),
        };

        let normalized = StartRequestNormalizer::normalize(&message);

        match normalized.payload {
            Some(MessagePayload::SessionConfig { timezone, location, .. }) => {
                assert_eq!(timezone.as_deref(), Some("America/New_York"));
                assert_eq!(location.as_deref(), Some("New York"));
            },
            other => panic!("expected session config, got {:?}", other),
        }
    }

    #[test]
    fn top_level_values_build_minimal_session_config_when_payload_missing() {
        let message = WebSocketMessage {
            protocol_id: ProtocolId::All,
            command_id: CommandId::Start,
            session_id: "session-1".to_string(),
            payload: None,
            timezone: Some("Asia/Shanghai".to_string()),
            location: Some("Shanghai".to_string()),
        };

        let normalized = StartRequestNormalizer::normalize(&message);

        match normalized.payload {
            Some(MessagePayload::SessionConfig { mode, timezone, location, system_prompt, .. }) => {
                assert!(mode.is_none());
                assert_eq!(timezone.as_deref(), Some("Asia/Shanghai"));
                assert_eq!(location.as_deref(), Some("Shanghai"));
                assert!(system_prompt.is_none());
            },
            other => panic!("expected session config, got {:?}", other),
        }
    }

    #[test]
    fn mode_maps_to_expected_speech_mode() {
        let ptt = WebSocketMessage {
            protocol_id: ProtocolId::All,
            command_id: CommandId::Start,
            session_id: "session-1".to_string(),
            payload: Some(session_config(Some("ptt"), None, None)),
            timezone: None,
            location: None,
        };
        let deferred = WebSocketMessage {
            protocol_id: ProtocolId::All,
            command_id: CommandId::Start,
            session_id: "session-2".to_string(),
            payload: Some(session_config(Some("vad_deferred"), None, None)),
            timezone: None,
            location: None,
        };
        let default_mode = WebSocketMessage {
            protocol_id: ProtocolId::All,
            command_id: CommandId::Start,
            session_id: "session-3".to_string(),
            payload: Some(session_config(Some("unknown"), None, None)),
            timezone: None,
            location: None,
        };

        assert_eq!(StartRequestNormalizer::normalize(&ptt).speech_mode, SpeechMode::PushToTalk);
        assert_eq!(
            StartRequestNormalizer::normalize(&deferred).speech_mode,
            SpeechMode::VadDeferred
        );
        assert_eq!(StartRequestNormalizer::normalize(&default_mode).speech_mode, SpeechMode::Vad);
    }

    #[test]
    fn empty_session_id_is_preserved() {
        let message = WebSocketMessage {
            protocol_id: ProtocolId::All,
            command_id: CommandId::Start,
            session_id: "    ".to_string(),
            payload: None,
            timezone: None,
            location: None,
        };

        let normalized = StartRequestNormalizer::normalize(&message);
        assert_eq!(normalized.session_id, "    ");
    }

    fn session_config(mode: Option<&str>, timezone: Option<&str>, location: Option<&str>) -> MessagePayload {
        MessagePayload::SessionConfig {
            mode: mode.map(str::to_string),
            vad_threshold: None,
            silence_duration_ms: None,
            min_speech_duration_ms: None,
            system_prompt: None,
            mcp_server_config: None,
            tools_endpoint: None,
            prompt_endpoint: None,
            tools: None,
            tool_choice: None,
            enable_search: None,
            search_config: None,
            voice_setting: None,
            asr_language: None,
            timezone: timezone.map(str::to_string),
            location: location.map(str::to_string),
            audio_chunk_size_kb: None,
            initial_burst_count: None,
            initial_burst_delay_ms: None,
            send_rate_multiplier: None,
            output_audio_config: None,
            input_audio_config: None,
            text_done_signal_only: None,
            signal_only: None,
            asr_chinese_convert: None,
            tts_chinese_convert: None,
            from_language: None,
            to_language: None,
            offline_tools: None,
        }
    }
}
