//! System Prompt Registry - 管理每个 agent 每种语言的 system prompt
//!
//! 支持多语言 system prompt，每个 agent 可以为不同语言配置独立的 prompt
//! Prompts 定义在 prompts/ 文件夹中，每个 agent 有独立的文件

use once_cell::sync::Lazy;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::sync::RwLock;

/// System Prompt Registry
///
/// 管理每个 agent 每种语言的 system prompt
/// Key: (agent_id, language) -> prompt
#[derive(Clone)]
pub struct SystemPromptRegistry {
    prompts: Arc<RwLock<FxHashMap<(String, String), String>>>,
}

/// 全局 System Prompt Registry 单例
static GLOBAL_REGISTRY: Lazy<SystemPromptRegistry> = Lazy::new(SystemPromptRegistry::new);

impl SystemPromptRegistry {
    pub fn new() -> Self {
        let registry = Self { prompts: Arc::new(RwLock::new(FxHashMap::default())) };
        registry.register_defaults();
        registry
    }

    /// 注册或更新某个 agent 某个语言的 system prompt
    pub fn register(&self, agent_id: &str, language: &str, prompt: String) {
        let key = (agent_id.to_string(), language.to_string());
        let mut prompts = self.prompts.write().unwrap();
        prompts.insert(key, prompt);
    }

    /// 规范化语言代码
    ///
    /// 将各种语言变体标准化为 prompt key：
    /// - 简体中文: "zh", "zh-CN", "zh-Hans" -> "zh"
    /// - 繁体中文: "zh-TW", "zh-HK", "zh-Hant", "yue" 及其他 zh 变体 -> "zh-TW"
    /// - "en-US", "en-GB", "en-AU" -> "en"
    /// - "ja-JP" -> "ja"
    /// - 等等
    pub fn normalize_language(language: &str) -> &str {
        let lang = language.to_lowercase();

        // 粤语 -> 繁体中文
        if lang == "yue" {
            return "zh-TW";
        }

        // 中文变体处理
        if lang.starts_with("zh") {
            // 简体中文: zh, zh-cn, zh-hans, zh-sg（新加坡使用简体）
            if lang == "zh" || lang == "zh-cn" || lang == "zh-hans" || lang == "zh-sg" {
                "zh"
            } else {
                // 其他所有中文变体（zh-tw, zh-hk, zh-hant, zh-mo 等）-> 繁体
                "zh-TW"
            }
        } else if lang.starts_with("en") {
            "en"
        } else if lang.starts_with("ja") || lang.starts_with("jp") {
            "ja"
        } else if lang.starts_with("ko") {
            "ko"
        } else if lang.starts_with("fr") {
            "fr"
        } else if lang.starts_with("de") {
            "de"
        } else if lang.starts_with("es") {
            "es"
        } else if lang.starts_with("pt") {
            "pt"
        } else if lang.starts_with("ru") {
            "ru"
        } else if lang.starts_with("th") {
            "th"
        } else if lang.starts_with("it") {
            "it"
        } else if lang.starts_with("vi") {
            "vi"
        } else if lang.starts_with("id") {
            "id"
        } else if lang.starts_with("hi") {
            "hi"
        } else if lang.starts_with("tr") {
            "tr"
        } else if lang.starts_with("uk") {
            "uk"
        } else if lang.starts_with("pl") {
            "pl"
        } else if lang.starts_with("nl") {
            "nl"
        } else if lang.starts_with("el") {
            "el"
        } else if lang.starts_with("ro") {
            "ro"
        } else if lang.starts_with("cs") {
            "cs"
        } else if lang.starts_with("fi") {
            "fi"
        } else if lang.starts_with("ar") {
            "ar"
        } else if lang.starts_with("sv") {
            "sv"
        } else if lang.starts_with("no") || lang.starts_with("nb") || lang.starts_with("nn") {
            "no"
        } else if lang.starts_with("da") {
            "da"
        } else if lang.starts_with("af") {
            "af"
        } else {
            // 如果无法识别，返回原始值的前两个字符（如果存在）
            language
                .split('-')
                .next()
                .unwrap_or(language)
                .split('_')
                .next()
                .unwrap_or(language)
        }
    }

    /// 获取某个 agent 某个语言的 system prompt
    ///
    /// # 参数
    /// - `agent_id`: agent 的 ID
    /// - `language`: 语言代码（必须提供，支持多种标准格式，如 "zh-CN", "en-US" 等）
    ///
    /// # 回退逻辑
    /// 1. 先尝试获取指定语言的 prompt（精确匹配）
    /// 2. 如果找不到，规范化语言代码后再次尝试（如 "zh-CN" -> "zh"）
    /// 3. 如果还是找不到，回退到 "en"（英文）
    /// 4. 如果连英文都没有，返回 None（这种情况不应该发生，因为所有 agent 都应该有默认 prompt）
    pub fn get(&self, agent_id: &str, language: &str) -> Option<String> {
        let prompts = self.prompts.read().unwrap();
        let agent_id = agent_id.to_string();

        // 1. 先尝试精确匹配
        if let Some(prompt) = prompts.get(&(agent_id.clone(), language.to_string())) {
            return Some(prompt.clone());
        }

        // 2. 规范化语言代码后再次尝试
        let normalized = Self::normalize_language(language);
        if normalized != language {
            if let Some(prompt) = prompts.get(&(agent_id.clone(), normalized.to_string())) {
                return Some(prompt.clone());
            }
        }

        // 3. 回退到英文
        if normalized != "en" {
            if let Some(prompt) = prompts.get(&(agent_id.clone(), "en".to_string())) {
                return Some(prompt.clone());
            }
        }

        // 4. 如果连英文都没有，返回 None
        None
    }

    /// 从 prompts 模块加载所有 prompts
    fn register_defaults(&self) {
        // 使用 crate::agents::prompts，因为 prompts 模块在 agents/mod.rs 中定义
        let all_prompts = crate::agents::prompts::all_prompts();
        for ((agent_id, language), prompt) in all_prompts {
            self.register(&agent_id, &language, prompt);
        }
    }
}

impl Default for SystemPromptRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemPromptRegistry {
    /// 获取全局 registry 实例
    pub fn global() -> &'static SystemPromptRegistry {
        &GLOBAL_REGISTRY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_get() {
        let registry = SystemPromptRegistry::new();

        // 测试获取默认的中文 prompt
        assert!(registry.get("agent.search.query", "zh").is_some());
        assert!(registry.get("agent.volume.control", "zh").is_some());

        // 测试注册新语言
        registry.register("agent.search.query", "en", "You are a search assistant.".to_string());
        assert_eq!(
            registry.get("agent.search.query", "en"),
            Some("You are a search assistant.".to_string())
        );

        // 测试回退到英文
        assert!(registry.get("agent.search.query", "fr").is_some()); // 应该回退到英文（如果有英文版本）

        // 测试语言代码规范化
        assert!(registry.get("agent.search.query", "zh-CN").is_some()); // zh-CN 应该匹配 zh
        assert!(registry.get("agent.search.query", "en-US").is_some()); // en-US 应该匹配 en（如果有英文版本）
    }

    #[test]
    fn test_chinese_variants_normalization() {
        // 简体中文变体 -> zh
        assert_eq!(SystemPromptRegistry::normalize_language("zh"), "zh");
        assert_eq!(SystemPromptRegistry::normalize_language("zh-CN"), "zh");
        assert_eq!(SystemPromptRegistry::normalize_language("zh-Hans"), "zh");
        assert_eq!(SystemPromptRegistry::normalize_language("zh-SG"), "zh"); // 新加坡使用简体
        assert_eq!(SystemPromptRegistry::normalize_language("ZH-CN"), "zh"); // 大小写不敏感

        // 繁体中文变体 -> zh-TW
        assert_eq!(SystemPromptRegistry::normalize_language("zh-TW"), "zh-TW");
        assert_eq!(SystemPromptRegistry::normalize_language("zh-HK"), "zh-TW");
        assert_eq!(SystemPromptRegistry::normalize_language("zh-Hant"), "zh-TW");
        assert_eq!(SystemPromptRegistry::normalize_language("zh-MO"), "zh-TW"); // 澳门

        // 粤语 -> zh-TW（粤语地区使用繁体）
        assert_eq!(SystemPromptRegistry::normalize_language("yue"), "zh-TW");
        assert_eq!(SystemPromptRegistry::normalize_language("YUE"), "zh-TW");
    }

    #[test]
    fn test_other_languages_normalization() {
        assert_eq!(SystemPromptRegistry::normalize_language("en-US"), "en");
        assert_eq!(SystemPromptRegistry::normalize_language("en-GB"), "en");
        assert_eq!(SystemPromptRegistry::normalize_language("ja-JP"), "ja");
        assert_eq!(SystemPromptRegistry::normalize_language("th"), "th");
        assert_eq!(SystemPromptRegistry::normalize_language("it-IT"), "it");
    }
}
