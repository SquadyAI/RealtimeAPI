//! MCP 服务器工具描述提示词注册表
//!
//! 根据 MCP 服务器地址，提供相应的工具描述文本，用于增强 system prompt

use rustc_hash::FxHashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

/// MCP 服务器工具描述注册表
#[derive(Debug, Clone)]
pub struct McpPromptRegistry {
    /// MCP 服务器地址 -> 工具描述文本
    prompts: Arc<RwLock<FxHashMap<String, String>>>,
}

impl Default for McpPromptRegistry {
    fn default() -> Self {
        let mut registry = Self { prompts: Arc::new(RwLock::new(FxHashMap::default())) };

        // 添加一些默认的工具描述
        registry.add_default_prompts();

        registry
    }
}

impl McpPromptRegistry {
    /// 创建新的注册表实例
    pub fn new() -> Self {
        Self::default()
    }

    /// 添加 MCP 服务器工具描述
    pub async fn add_prompt(&self, mcp_server: String, prompt: String) {
        debug!("🔧 添加 MCP 工具描述: {} -> {}", mcp_server, prompt);
        let mut prompts = self.prompts.write().await;
        prompts.insert(mcp_server, prompt);
    }

    /// 获取 MCP 服务器的工具描述
    pub async fn get_prompt(&self, mcp_server: &str) -> Option<String> {
        let prompts = self.prompts.read().await;
        prompts.get(mcp_server).cloned()
    }

    /// 检查是否有指定 MCP 服务器的工具描述
    pub async fn has_prompt(&self, mcp_server: &str) -> bool {
        let prompts = self.prompts.read().await;
        prompts.contains_key(mcp_server)
    }

    /// 获取所有可用的 MCP 服务器列表
    pub async fn get_available_servers(&self) -> Vec<String> {
        let prompts = self.prompts.read().await;
        prompts.keys().cloned().collect()
    }

    /// 移除 MCP 服务器工具描述
    pub async fn remove_prompt(&self, mcp_server: &str) -> bool {
        debug!("🗑️ 移除 MCP 工具描述: {}", mcp_server);
        let mut prompts = self.prompts.write().await;
        prompts.remove(mcp_server).is_some()
    }

    /// 清空所有工具描述
    pub async fn clear(&self) {
        debug!("🧹 清空所有 MCP 工具描述");
        let mut prompts = self.prompts.write().await;
        prompts.clear();
    }

    /// 添加默认的工具描述示例
    fn add_default_prompts(&mut self) {
        // 🆕 重新启用默认提示词，优化工具调用策略
        use tokio::runtime::Handle;
        if let Ok(handle) = Handle::try_current() {
            let registry = self.clone();
            handle.spawn(async move {
                registry
                    .add_prompt(
                        "https://example.com/gateway/machine/functionCall/mcp".to_string(),
                        "# Tool Calling Strategy
- Strictly follow tool definitions: Tool list and parameters are dynamically provided by the system, call strictly as defined
- Natural fluency: Integrate tool calls into conversation naturally, avoid mechanical feel
- Intelligent fallback: Provide valuable alternatives when tools fail
- Single-call principle: Only call the most relevant tool per conversation turn
- Context priority: Always prioritize user's actual location, time, and other context

## Guidelines
- For search needs: Find and call search-related tools
- For image-related questions: Must call image analysis tools
- For real-time information needs: Must call corresponding query tools
- Never answer based on guesses for information that requires tools
- Time queries: What time is it / current time → directly tell system time
- Simple greetings: Hello / thank you → natural conversation response
- Direct answerable: Basic knowledge that doesn't need real-time data
- Tool unavailable: Honestly explain when no matching tool exists"
                            .to_string(),
                    )
                    .await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mcp_prompt_registry() {
        let registry = McpPromptRegistry::new();

        // 测试添加和获取
        registry
            .add_prompt("http://localhost:3000".to_string(), "你可以使用搜索工具".to_string())
            .await;

        assert!(registry.has_prompt("http://localhost:3000").await);
        assert_eq!(
            registry.get_prompt("http://localhost:3000").await,
            Some("你可以使用搜索工具".to_string())
        );

        // 测试获取不存在的服务器
        assert_eq!(registry.get_prompt("http://localhost:9999").await, None);

        // 测试移除
        assert!(registry.remove_prompt("http://localhost:3000").await);
        assert!(!registry.has_prompt("http://localhost:3000").await);
    }
}
