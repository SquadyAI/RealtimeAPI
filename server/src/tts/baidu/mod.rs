//! 百度 TTS 模块
//!
//! 提供百度智能云文本在线合成服务支持
//!
//! ## 功能特点
//!
//! - **HTTP REST API** (推荐): 短文本合成，无状态可并发
//! - WebSocket 流式合成 (已弃用)
//! - 支持多种发音人
//! - 支持语速、音调、音量调节
//!
//! ## 使用示例
//!
//! ```rust,ignore
//! use crate::tts::baidu::{BaiduHttpTtsClient, BaiduHttpTtsRequest};
//!
//! let client = BaiduHttpTtsClient::from_env()?;
//! let request = BaiduHttpTtsRequest::new("欢迎体验百度语音合成。");
//! let stream = client.synthesize(request)?;
//!
//! // 消费音频流...
//! ```
//!
//! ## 环境变量
//!
//! | 变量名 | 说明 | 必填 |
//! | --- | --- | --- |
//! | `BAIDU_TTS_API_KEY` | API Key | 是 |
//! | `BAIDU_TTS_SECRET_KEY` | Secret Key | 是 |
//! | `BAIDU_TTS_PER` | 发音人 ID（可被请求覆盖） | 否 |
//! | `BAIDU_TTS_SPD` | 语速 0-15 | 否 |
//! | `BAIDU_TTS_PIT` | 音调 0-15 | 否 |
//! | `BAIDU_TTS_VOL` | 音量 0-15 | 否 |
//! | `BAIDU_TTS_VOICE_HOT_UPDATE_ENABLED` | 是否允许热更新音色参数（默认 false） | 否 |
//! | `BAIDU_TTS_VOICE_PER` | 音色 per（覆盖常量，默认 4197） | 否 |
//! | `BAIDU_TTS_VOICE_SPD` | 音色语速 0-15（默认 7） | 否 |
//! | `BAIDU_TTS_VOICE_PIT` | 音色音调 0-15（默认 6） | 否 |
//! | `BAIDU_TTS_VOICE_VOL` | 音色音量 0-15（默认 5） | 否 |
//! | `BAIDU_TTS_VOICE_SPEED_FACTOR` | PCM 无级变速因子（默认 1.0） | 否 |

pub mod client;
pub mod config;
pub mod http_client;
pub mod types;

use std::sync::{OnceLock, RwLock};

/// 客户端可配置的"百度专用"音色（在系统里仍表现为一个 voice_id）。
///
/// 需求：当 voice_id 为该值且句子语言为中文/英文时，使用 Baidu TTS；
/// 否则 fallback 到 MiniMax（因为同名 voice_id 在 MiniMax 也存在）。
pub const BAIDU_TTS_VOICE_ID_DUHANZHU_BRIGHT: &str = "ttv-voice-2026012217272626-tk9kPJcw";

/// 百度音色 prosody 配置，支持运行时热更新。
///
/// | 环境变量 | 说明 | 默认值 |
/// |---|---|---|
/// | `BAIDU_TTS_VOICE_HOT_UPDATE_ENABLED` | 是否允许热更新音色参数 | false |
/// | `BAIDU_TTS_VOICE_PER` | 发音人 ID | 4197 |
/// | `BAIDU_TTS_VOICE_SPD` | 语速 0-15 | 7 |
/// | `BAIDU_TTS_VOICE_PIT` | 音调 0-15 | 6 |
/// | `BAIDU_TTS_VOICE_VOL` | 音量 0-15 | 5 |
/// | `BAIDU_TTS_VOICE_SPEED_FACTOR` | PCM 无级变速因子 (< 1.0 减速, > 1.0 加速) | 1.0 |
#[derive(Clone)]
struct BaiduVoiceParams {
    hot_update_enabled: bool,
    per: String,
    spd: u8,
    pit: u8,
    vol: u8,
    speed_factor: f64,
}

/// 可序列化的百度音色参数快照（供 API 返回）
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct BaiduVoiceParamsSnapshot {
    pub hot_update_enabled: bool,
    pub per: String,
    pub spd: u8,
    pub pit: u8,
    pub vol: u8,
    pub speed_factor: f64,
}

static VOICE_PARAMS: OnceLock<RwLock<BaiduVoiceParams>> = OnceLock::new();

fn voice_params_lock() -> &'static RwLock<BaiduVoiceParams> {
    VOICE_PARAMS.get_or_init(|| {
        let hot_update_enabled = std::env::var("BAIDU_TTS_VOICE_HOT_UPDATE_ENABLED")
            .ok()
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(false);
        let per = std::env::var("BAIDU_TTS_VOICE_PER").unwrap_or_else(|_| "4197".into());
        let spd = std::env::var("BAIDU_TTS_VOICE_SPD")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(7)
            .min(15);
        let pit = std::env::var("BAIDU_TTS_VOICE_PIT")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(6)
            .min(15);
        let vol = std::env::var("BAIDU_TTS_VOICE_VOL")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(5)
            .min(15);
        let speed_factor = std::env::var("BAIDU_TTS_VOICE_SPEED_FACTOR")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(1.0);

        tracing::info!(
            "🎙️ 百度音色参数: hot_update_enabled={}, per={}, spd={}, pit={}, vol={}, speed_factor={}",
            hot_update_enabled,
            per,
            spd,
            pit,
            vol,
            speed_factor
        );

        RwLock::new(BaiduVoiceParams { hot_update_enabled, per, spd, pit, vol, speed_factor })
    })
}

/// 获取当前百度音色参数快照（线程安全，用于 API 返回）
pub fn get_baidu_voice_params() -> BaiduVoiceParamsSnapshot {
    let params = voice_params_lock().read().unwrap();
    BaiduVoiceParamsSnapshot {
        hot_update_enabled: params.hot_update_enabled,
        per: params.per.clone(),
        spd: params.spd,
        pit: params.pit,
        vol: params.vol,
        speed_factor: params.speed_factor,
    }
}

/// 热更新百度音色参数（线程安全，后续新的 TTS 合成将使用新参数）
/// 仅当 `BAIDU_TTS_VOICE_HOT_UPDATE_ENABLED=true` 时才允许热更新
pub fn update_baidu_voice_params(snapshot: &BaiduVoiceParamsSnapshot) {
    let params = voice_params_lock().read().unwrap();
    if !params.hot_update_enabled {
        tracing::warn!("⚠️ 百度音色热更新被禁用（BAIDU_TTS_VOICE_HOT_UPDATE_ENABLED 未设置为 true），忽略热更新请求");
        return;
    }
    drop(params);

    let mut params = voice_params_lock().write().unwrap();
    params.hot_update_enabled = snapshot.hot_update_enabled;
    params.per = snapshot.per.clone();
    params.spd = snapshot.spd.min(15);
    params.pit = snapshot.pit.min(15);
    params.vol = snapshot.vol.min(15);
    params.speed_factor = snapshot.speed_factor;
    tracing::info!(
        "🔄 百度音色参数已热更新: hot_update_enabled={}, per={}, spd={}, pit={}, vol={}, speed_factor={}",
        params.hot_update_enabled,
        params.per,
        params.spd,
        params.pit,
        params.vol,
        params.speed_factor
    );
}

/// 根据 voice_id 返回百度 per 参数（若该 voice_id 不映射到百度则返回 None）。
pub fn baidu_per_for_voice_id(voice_id: &str) -> Option<String> {
    match voice_id {
        BAIDU_TTS_VOICE_ID_DUHANZHU_BRIGHT => {
            let params = voice_params_lock().read().unwrap();
            Some(params.per.clone())
        },
        _ => None,
    }
}

/// 根据 voice_id 返回自定义速度因子（若无特殊配置返回 None，表示 1.0 原速）。
pub fn baidu_speed_factor_for_voice_id(voice_id: &str) -> Option<f64> {
    match voice_id {
        BAIDU_TTS_VOICE_ID_DUHANZHU_BRIGHT => {
            let params = voice_params_lock().read().unwrap();
            Some(params.speed_factor)
        },
        _ => None,
    }
}

/// 根据 voice_id 返回百度 system.start payload 的覆盖（仅覆盖 spd/pit/vol，其余保持 base）。
pub fn baidu_payload_override_for_voice_id(voice_id: &str, mut base: SystemStartPayload) -> Option<SystemStartPayload> {
    match voice_id {
        BAIDU_TTS_VOICE_ID_DUHANZHU_BRIGHT => {
            let p = voice_params_lock().read().unwrap();
            base.spd = Some(p.spd);
            base.pit = Some(p.pit);
            base.vol = Some(p.vol);
            Some(base)
        },
        _ => None,
    }
}

/// PCM 16-bit LE 线性插值变速
///
/// 通过对 PCM 采样点做线性插值来拉伸/压缩音频时长，实现无级变速。
/// - `speed_factor` < 1.0：减速（音频变长，采样点变多）
/// - `speed_factor` > 1.0：加速（音频变短，采样点变少）
///
/// 注意：此方法会带来极轻微的音调变化，在 ±10% 范围内人耳几乎不可察觉。
pub fn pcm_speed_adjust(pcm_data: &[u8], speed_factor: f64) -> Vec<u8> {
    // 无需调整或数据太短
    if (speed_factor - 1.0).abs() < 0.001 || pcm_data.len() < 4 {
        return pcm_data.to_vec();
    }

    let sample_count = pcm_data.len() / 2; // 每个采样 2 字节 (16-bit LE)
    let output_count = (sample_count as f64 / speed_factor).ceil() as usize;

    let mut output = Vec::with_capacity(output_count * 2);

    for i in 0..output_count {
        let src_pos = i as f64 * speed_factor;
        let idx = src_pos.floor() as usize;
        let frac = src_pos - idx as f64;

        if idx + 1 < sample_count {
            // 线性插值：在两个相邻采样点之间插值
            let s0 = i16::from_le_bytes([pcm_data[idx * 2], pcm_data[idx * 2 + 1]]) as f64;
            let s1 = i16::from_le_bytes([pcm_data[(idx + 1) * 2], pcm_data[(idx + 1) * 2 + 1]]) as f64;
            let interpolated = (s0 * (1.0 - frac) + s1 * frac) as i16;
            output.extend_from_slice(&interpolated.to_le_bytes());
        } else if idx < sample_count {
            // 最后一个采样点，直接复制
            output.extend_from_slice(&[pcm_data[idx * 2], pcm_data[idx * 2 + 1]]);
        }
    }

    output
}

// WebSocket 客户端 (已弃用，保留兼容)
pub use client::{BaiduTtsClient, BaiduTtsRequest};
// HTTP REST API 客户端 (推荐)
pub use config::BaiduTtsConfig;
pub use http_client::{BaiduHttpTtsClient, BaiduHttpTtsRequest};
pub use types::{BaiduAudioFormat, BaiduTtsError, BaiduTtsErrorCode, SystemStartPayload};
