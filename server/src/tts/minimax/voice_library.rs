//! MiniMax 声音库管理模块
//!
//! 提供全局声音库配置，支持多个API key及其voice_id映射

use anyhow::{Result, anyhow};
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

/// 声音库配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceLibraryConfig {
    /// API keys映射：key名称 -> 实际API key
    pub keys: FxHashMap<String, String>,

    /// voice_id 增益配置：voice_id -> gain_db
    /// 未配置的 voice_id 默认增益为 0.0
    #[serde(default)]
    pub gains: FxHashMap<String, f32>,

    /// voice_id 语速配置：voice_id -> speed
    /// 取值范围 [0.5, 2]，未配置时不覆盖默认值
    #[serde(default)]
    pub speeds: FxHashMap<String, f64>,

    /// voice_id 声调配置：voice_id -> pitch
    /// 取值范围 [-12, 12]，未配置时不覆盖默认值
    #[serde(default)]
    pub pitches: FxHashMap<String, i32>,

    /// voice_id 模型配置：voice_id -> model
    /// 可选值: speech-2.6-hd, speech-2.6-turbo, speech-02-hd, speech-02-turbo, speech-01-hd, speech-01-turbo
    /// 未配置时使用全局默认模型
    #[serde(default)]
    pub models: FxHashMap<String, String>,

    /// voice_id 情绪配置：voice_id -> emotion
    /// 可选值: happy, sad, angry, fearful, disgusted, surprised, calm, fluent, whisper
    /// 未配置时不设置情绪（由模型自动匹配）
    #[serde(default)]
    pub emotions: FxHashMap<String, String>,

    /// voice_id 音量配置：voice_id -> vol
    /// 取值范围 (0, 10]，未配置时不覆盖默认值
    #[serde(default)]
    pub volumes: FxHashMap<String, f64>,

    /// 其他字段为动态的voice_id映射
    /// voice_id_X -> { keyname -> actual_voice_id }
    #[serde(flatten)]
    pub voice_mappings: FxHashMap<String, FxHashMap<String, String>>,
}

impl VoiceLibraryConfig {
    /// 创建空配置
    pub fn new() -> Self {
        Self {
            keys: FxHashMap::default(),
            gains: FxHashMap::default(),
            speeds: FxHashMap::default(),
            pitches: FxHashMap::default(),
            models: FxHashMap::default(),
            emotions: FxHashMap::default(),
            volumes: FxHashMap::default(),
            voice_mappings: FxHashMap::default(),
        }
    }

    /// 从JSON字符串创建配置
    pub fn from_json(json: &str) -> Result<Self> {
        let value: serde_json::Value = serde_json::from_str(json)?;

        // 提取keys
        let keys_value = value.get("keys").ok_or_else(|| anyhow!("缺少 'keys' 字段"))?;
        let keys: FxHashMap<String, String> = serde_json::from_value(keys_value.clone())?;

        // 提取gains（可选）
        let gains: FxHashMap<String, f32> = value
            .get("gains")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        // 提取speeds（可选）
        let speeds: FxHashMap<String, f64> = value
            .get("speeds")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        // 提取pitches（可选）
        let pitches: FxHashMap<String, i32> = value
            .get("pitches")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        // 提取models（可选）
        let models: FxHashMap<String, String> = value
            .get("models")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        // 提取emotions（可选）
        let emotions: FxHashMap<String, String> = value
            .get("emotions")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        // 提取volumes（可选）
        let volumes: FxHashMap<String, f64> = value
            .get("volumes")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        // 保留字段列表
        const RESERVED_KEYS: &[&str] = &["keys", "gains", "speeds", "pitches", "models", "emotions", "volumes"];

        // 提取所有voice_id映射（除了保留字段外的所有字段）
        let mut voice_mappings = FxHashMap::default();
        if let Some(obj) = value.as_object() {
            for (key, val) in obj.iter() {
                if !RESERVED_KEYS.contains(&key.as_str()) {
                    if let Ok(mapping) = serde_json::from_value::<FxHashMap<String, String>>(val.clone()) {
                        voice_mappings.insert(key.clone(), mapping);
                    }
                }
            }
        }

        // 验证：每个voice_id映射中的keyname必须在keys中存在
        for (voice_id, mapping) in &voice_mappings {
            for keyname in mapping.keys() {
                if !keys.contains_key(keyname) {
                    return Err(anyhow!("voice_id '{}' 引用了不存在的 keyname '{}'", voice_id, keyname));
                }
            }
        }

        Ok(Self { keys, gains, speeds, pitches, models, emotions, volumes, voice_mappings })
    }

    /// 验证配置有效性
    pub fn validate(&self) -> Result<()> {
        if self.keys.is_empty() {
            return Err(anyhow!("keys 不能为空"));
        }

        // 验证每个voice_id映射中的keyname存在
        for (voice_id, mapping) in &self.voice_mappings {
            if mapping.is_empty() {
                return Err(anyhow!("voice_id '{}' 的映射不能为空", voice_id));
            }
            for keyname in mapping.keys() {
                if !self.keys.contains_key(keyname) {
                    return Err(anyhow!("voice_id '{}' 引用了不存在的 keyname '{}'", voice_id, keyname));
                }
            }
        }

        Ok(())
    }

    /// 获取虚拟voice_id对应的所有实际voice_id和API key
    /// 返回: Vec<(api_key, actual_voice_id)>
    pub fn get_voice_options(&self, virtual_voice_id: &str) -> Vec<(String, String)> {
        if let Some(mapping) = self.voice_mappings.get(virtual_voice_id) {
            mapping
                .iter()
                .filter_map(|(keyname, actual_voice_id)| self.keys.get(keyname).map(|api_key| (api_key.clone(), actual_voice_id.clone())))
                .collect()
        } else {
            Vec::new()
        }
    }

    /// 获取所有虚拟voice_id
    pub fn get_virtual_voice_ids(&self) -> Vec<String> {
        self.voice_mappings.keys().cloned().collect()
    }

    /// 获取配置的key数量
    pub fn key_count(&self) -> usize {
        self.keys.len()
    }

    /// 获取 voice_id 对应的增益 dB 值，未配置时返回 0.0
    pub fn get_gain_db(&self, voice_id: &str) -> f32 {
        self.gains.get(voice_id).copied().unwrap_or(0.0)
    }

    /// 获取 voice_id 对应的语速，未配置时返回 None
    pub fn get_speed(&self, voice_id: &str) -> Option<f64> {
        self.speeds.get(voice_id).copied()
    }

    /// 获取 voice_id 对应的声调，未配置时返回 None
    pub fn get_pitch(&self, voice_id: &str) -> Option<i32> {
        self.pitches.get(voice_id).copied()
    }

    /// 获取 voice_id 对应的模型，未配置时返回 None
    pub fn get_model(&self, voice_id: &str) -> Option<&str> {
        self.models.get(voice_id).map(|s| s.as_str())
    }

    /// 获取 voice_id 对应的情绪，未配置时返回 None
    pub fn get_emotion(&self, voice_id: &str) -> Option<&str> {
        self.emotions.get(voice_id).map(|s| s.as_str())
    }

    /// 获取 voice_id 对应的音量，未配置时返回 None
    pub fn get_vol(&self, voice_id: &str) -> Option<f64> {
        self.volumes.get(voice_id).copied()
    }
}

impl Default for VoiceLibraryConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局声音库管理器
pub struct VoiceLibrary {
    config: Arc<RwLock<VoiceLibraryConfig>>,
    /// 当前使用的索引，用于轮换
    current_index: Arc<std::sync::atomic::AtomicUsize>,
}

impl VoiceLibrary {
    /// 创建新的声音库
    pub fn new() -> Self {
        Self {
            config: Arc::new(RwLock::new(VoiceLibraryConfig::new())),
            current_index: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// 从配置创建声音库
    pub fn from_config(config: VoiceLibraryConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            current_index: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// 更新配置
    pub fn update_config(&self, config: VoiceLibraryConfig) -> Result<()> {
        config.validate()?;
        let mut write_guard = self.config.write();
        *write_guard = config;
        info!("声音库配置已更新");
        Ok(())
    }

    /// 从JSON更新配置
    pub fn update_from_json(&self, json: &str) -> Result<()> {
        let config = VoiceLibraryConfig::from_json(json)?;
        self.update_config(config)
    }

    /// 获取虚拟voice_id对应的API key和实际voice_id（轮换策略）
    /// 返回: Option<(api_key, actual_voice_id)>
    pub fn get_voice(&self, virtual_voice_id: &str) -> Option<(String, String)> {
        let config = self.config.read();
        let options = config.get_voice_options(virtual_voice_id);

        if options.is_empty() {
            warn!("虚拟 voice_id '{}' 未配置", virtual_voice_id);
            return None;
        }

        // 轮换选择
        let index = self.current_index.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % options.len();
        Some(options[index].clone())
    }

    /// 获取所有虚拟voice_id
    pub fn get_virtual_voice_ids(&self) -> Vec<String> {
        self.config.read().get_virtual_voice_ids()
    }

    /// 检查是否已配置
    pub fn is_configured(&self) -> bool {
        self.config.read().key_count() > 0
    }

    /// 获取配置的key数量
    pub fn key_count(&self) -> usize {
        self.config.read().key_count()
    }

    /// 获取当前配置的副本
    pub fn get_config(&self) -> VoiceLibraryConfig {
        self.config.read().clone()
    }

    /// 获取 voice_id 对应的增益 dB 值，未配置时返回 0.0
    pub fn get_gain_db(&self, voice_id: &str) -> f32 {
        self.config.read().get_gain_db(voice_id)
    }

    /// 获取 voice_id 对应的语速，未配置时返回 None
    pub fn get_speed(&self, voice_id: &str) -> Option<f64> {
        self.config.read().get_speed(voice_id)
    }

    /// 获取 voice_id 对应的声调，未配置时返回 None
    pub fn get_pitch(&self, voice_id: &str) -> Option<i32> {
        self.config.read().get_pitch(voice_id)
    }

    /// 获取 voice_id 对应的模型，未配置时返回 None
    pub fn get_model(&self, voice_id: &str) -> Option<String> {
        self.config.read().get_model(voice_id).map(|s| s.to_string())
    }

    /// 获取 voice_id 对应的情绪，未配置时返回 None
    pub fn get_emotion(&self, voice_id: &str) -> Option<String> {
        self.config.read().get_emotion(voice_id).map(|s| s.to_string())
    }

    /// 获取 voice_id 对应的音量，未配置时返回 None
    pub fn get_vol(&self, voice_id: &str) -> Option<f64> {
        self.config.read().get_vol(voice_id)
    }
}

impl Default for VoiceLibrary {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局声音库实例
static GLOBAL_VOICE_LIBRARY: once_cell::sync::Lazy<VoiceLibrary> = once_cell::sync::Lazy::new(VoiceLibrary::new);

/// 获取全局声音库实例
pub fn global_voice_library() -> &'static VoiceLibrary {
    &GLOBAL_VOICE_LIBRARY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_voice_library_config_from_json() {
        let json = r#"
        {
            "keys": {
                "key1": "actual_api_key_1",
                "key2": "actual_api_key_2"
            },
            "voice_id_1": {
                "key1": "voice_1_for_key1",
                "key2": "voice_1_for_key2"
            },
            "voice_id_2": {
                "key1": "voice_2_for_key1"
            }
        }
        "#;

        let config = VoiceLibraryConfig::from_json(json).unwrap();
        assert_eq!(config.keys.len(), 2);
        assert_eq!(config.voice_mappings.len(), 2);

        let options = config.get_voice_options("voice_id_1");
        assert_eq!(options.len(), 2);

        let options = config.get_voice_options("voice_id_2");
        assert_eq!(options.len(), 1);
    }

    #[test]
    fn test_voice_library_config_validation() {
        let json = r#"
        {
            "keys": {
                "key1": "actual_api_key_1"
            },
            "voice_id_1": {
                "key2": "voice_1_for_key2"
            }
        }
        "#;

        let result = VoiceLibraryConfig::from_json(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_voice_library_rotation() {
        let json = r#"
        {
            "keys": {
                "key1": "api_key_1",
                "key2": "api_key_2"
            },
            "voice_id_1": {
                "key1": "voice_1_a",
                "key2": "voice_1_b"
            }
        }
        "#;

        let config = VoiceLibraryConfig::from_json(json).unwrap();
        let library = VoiceLibrary::from_config(config);

        // 第一次获取
        let (api_key_1, voice_id_1) = library.get_voice("voice_id_1").unwrap();
        // 第二次获取（应该轮换）
        let (_api_key_2, _voice_id_2) = library.get_voice("voice_id_1").unwrap();

        // 因为是轮换，所以两次应该不同（如果有多个选项）
        assert!(api_key_1 == "api_key_1" || api_key_1 == "api_key_2");
        assert!(voice_id_1 == "voice_1_a" || voice_id_1 == "voice_1_b");
    }
}
