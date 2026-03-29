//! 实时对话API系统
//!
//! 一个低延迟的实时语音对话系统，类似于OpenAI的Realtime API
//! 目标总延迟: ≤450ms
#![allow(clippy::collapsible_if)]
#![allow(clippy::clone_on_copy)]

pub mod agents;
pub mod asr;
pub mod audio;
pub mod function_callback;
pub mod geodata;
pub mod lang;
pub mod telemetry;
pub mod text_splitter;
pub mod tts;
#[allow(clippy::module_inception)]
pub mod llm {
    pub mod llm;
    pub mod llm_task_v2;
    pub mod mcp_prompt_registry;
    pub use llm::ChatCompletionParams;
    pub use llm::ChatCompletionRequest;
    pub use llm::ChatCompletionResponse;
    pub use llm::ChatMessage;
    pub use llm::Choice;
    pub use llm::LlmClient;
    // 新增的配置类型
    pub use llm::{HttpVersion, LlmConfig};
    // Function Call 相关类型
    pub use llm::{FunctionCall, Tool, ToolCall, ToolChoice, ToolFunction, ToolFunctionChoice};
    // MCP 提示词注册表
    pub use llm_task_v2::LlmTaskV2;
    pub use mcp_prompt_registry::McpPromptRegistry;
}
pub mod ip_geolocation;
pub mod mcp;
pub mod monitoring;
pub mod rpc;
pub mod storage;
pub mod vad;

pub use asr::{ASRModuleConfig, AsrEngine, AsrResult};
pub use function_callback::{CallResult, FunctionCallResult};
pub use ip_geolocation::{GeoLocation, IpAddress, LookupResult};
pub use mcp::{McpClient, McpError, McpServerConfig, McpTool, McpToolCall, McpToolResult};

/// 系统错误类型
#[derive(Debug, thiserror::Error)]
pub enum SystemError {
    #[error("音频处理错误: {0}")]
    Audio(#[from] audio::AudioError),

    #[error("语音识别错误: {0}")]
    Asr(#[from] asr::AsrError),

    #[error("功能回调错误: {0}")]
    FunctionCallback(#[from] function_callback::FunctionCallbackError),

    #[error("MCP客户端错误: {0}")]
    Mcp(#[from] mcp::McpError),

    #[error("RPC通信错误: {0}")]
    Rpc(String),

    #[error("配置错误: {0}")]
    Config(String),

    #[error("初始化错误: {0}")]
    Initialization(String),

    #[error("运行时错误: {0}")]
    Runtime(String),
}

/// 系统配置
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct SystemConfig {
    /// 语音识别配置
    pub asr: ASRModuleConfig,

    /// MCP服务器配置
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpServerConfig>,
    /// RPC通信配置
    pub rpc: rpc::config::RpcConfig,
}

/// GPU utilities module
pub mod gpu_utils {
    use tracing::{info, warn};

    /// 获取第一个可见的 CUDA 设备 ID
    ///
    /// 该函数检查 CUDA_VISIBLE_DEVICES 环境变量来确定哪些 GPU 是可见的，
    /// 并返回第一个可见设备的实际设备 ID。
    ///
    /// # 返回值
    /// - `Some(0)` - 当 CUDA_VISIBLE_DEVICES 设置时，始终返回 0（ONNX Runtime 中的第一个设备）
    /// - `Some(0)` - 当 CUDA_VISIBLE_DEVICES 未设置时，返回默认设备 0
    /// - `None` - 当 CUDA_VISIBLE_DEVICES 设置为空或无效时
    ///
    /// # 注意
    /// 当设置了 CUDA_VISIBLE_DEVICES 后，ONNX Runtime 看到的设备索引是从 0 开始的。
    /// 例如：CUDA_VISIBLE_DEVICES=2,3 时，物理设备 2 在 ONNX Runtime 中是设备 0。
    pub fn get_first_visible_cuda_device() -> Option<i32> {
        use std::env;

        match env::var("CUDA_VISIBLE_DEVICES") {
            Ok(devices_str) => {
                // 处理空字符串或只有空格的情况
                let devices_str = devices_str.trim();
                if devices_str.is_empty() {
                    info!("CUDA_VISIBLE_DEVICES 为空，无可用 GPU");
                    return None;
                }

                // 解析设备列表（格式如 "0,1,2" 或 "1" 或 "2,3"）
                let devices: Vec<&str> = devices_str.split(',').collect();
                if let Some(first_device) = devices.first() {
                    match first_device.trim().parse::<i32>() {
                        Ok(_) => {
                            info!(
                                "检测到 CUDA_VISIBLE_DEVICES={}，使用第一个可见设备（ONNX Runtime 设备 0）",
                                devices_str
                            );
                            // 当设置了 CUDA_VISIBLE_DEVICES 后，ONNX Runtime 看到的设备索引是从 0 开始的
                            Some(0) // 始终使用 ONNX Runtime 中的第一个设备
                        },
                        Err(_) => {
                            warn!("无法解析 CUDA_VISIBLE_DEVICES 中的设备 ID: {}", first_device);
                            None
                        },
                    }
                } else {
                    None
                }
            },
            Err(_) => {
                info!("CUDA_VISIBLE_DEVICES 未设置，使用默认 GPU 设备 0");
                Some(0)
            },
        }
    }

    /// 检查CUDA是否可用
    ///
    /// 该函数检查系统是否安装了CUDA和CUDNN库，以便判断是否可以使用GPU加速。
    /// 主要用于在初始化ONNX Runtime CUDA提供程序之前进行预检查。
    ///
    /// # 返回值
    /// - `true` - CUDA环境可用
    /// - `false` - CUDA环境不可用或缺少必要组件
    pub fn is_cuda_available() -> bool {
        // 检查CUDA_VISIBLE_DEVICES环境变量
        if let Ok(devices) = std::env::var("CUDA_VISIBLE_DEVICES")
            && devices.trim().is_empty()
        {
            info!("CUDA_VISIBLE_DEVICES 为空，禁用CUDA");
            return false;
        }

        // 在Windows上检查CUDNN库是否存在
        #[cfg(target_os = "windows")]
        {
            use std::path::Path;

            // 检查常见的CUDNN库文件
            let cudnn_libs = ["cudnn64_9.dll", "cudnn64_8.dll", "cudnn.dll"];

            // 1. 首先检查系统PATH中是否存在CUDNN库
            if let Ok(path_env) = std::env::var("PATH") {
                for path_dir in std::env::split_paths(&path_env) {
                    for lib_name in &cudnn_libs {
                        let lib_path = path_dir.join(lib_name);
                        if lib_path.exists() {
                            info!("在PATH中找到CUDNN库: {:?}", lib_path);
                            return true;
                        }
                    }
                }
            }

            // 2. 如果PATH中没有找到，检查标准CUDNN安装路径
            let standard_cudnn_paths = [
                r"C:\Program Files\NVIDIA\CUDNN\v9.12\bin\12.9",
                r"C:\Program Files\NVIDIA\CUDNN\v9.12\bin",
                r"C:\Program Files\NVIDIA\CUDNN\v9.11\bin\12.9",
                r"C:\Program Files\NVIDIA\CUDNN\v9.11\bin",
                r"C:\Program Files\NVIDIA\CUDNN\v9.10\bin\12.9",
                r"C:\Program Files\NVIDIA\CUDNN\v9.10\bin",
                r"C:\Program Files\NVIDIA\CUDNN\v8.9\bin",
                r"C:\Program Files\NVIDIA\CUDNN\v8.8\bin",
            ];

            for cudnn_dir in &standard_cudnn_paths {
                let cudnn_path = Path::new(cudnn_dir);
                if cudnn_path.exists() {
                    for lib_name in &cudnn_libs {
                        let lib_path = cudnn_path.join(lib_name);
                        if lib_path.exists() {
                            info!("在标准路径中找到CUDNN库: {:?}", lib_path);

                            // 动态添加CUDNN路径到当前进程的PATH环境变量
                            // 注意：此代码在初始化阶段执行，在异步 runtime 启动前是安全的
                            if let Ok(current_path) = std::env::var("PATH") {
                                if !current_path.contains(cudnn_dir) {
                                    let new_path = format!("{};{}", cudnn_dir, current_path);
                                    // unsafe 是必要的，因为 std::env::set_var 不是线程安全的
                                    // 此代码在初始化阶段执行，在异步 runtime 启动前是安全的
                                    unsafe {
                                        std::env::set_var("PATH", &new_path);
                                    }
                                    info!("✅ 已将CUDNN路径动态添加到进程PATH: {:?}", cudnn_dir);
                                } else {
                                    info!("ℹ️  CUDNN路径已在PATH中: {:?}", cudnn_dir);
                                }
                            }

                            // 在Windows上预加载CUDNN库以确保ONNX Runtime能找到它
                            #[cfg(target_os = "windows")]
                            {
                                if let Err(e) = preload_cudnn_libraries(cudnn_path) {
                                    warn!("预加载CUDNN库失败: {}", e);
                                    warn!("CUDA功能可能仍然不可用，建议将CUDNN路径添加到系统PATH");
                                } else {
                                    info!("✅ 成功预加载CUDNN库");
                                }
                            }

                            return true;
                        }
                    }
                }
            }

            warn!("未找到CUDNN库文件，CUDA功能不可用");
            warn!("请确保安装了CUDA和CUDNN，并将CUDNN的bin目录添加到系统PATH中");
            warn!("或者检查CUDNN是否安装在标准路径: C:\\Program Files\\NVIDIA\\CUDNN\\");
            false
        }

        // 在非Windows系统上，假设CUDA环境已正确配置
        #[cfg(not(target_os = "windows"))]
        {
            true
        }
    }

    /// Windows特定的CUDNN库预加载函数
    ///
    /// 这个函数尝试预加载CUDNN相关的DLL库，以确保ONNX Runtime能够找到它们。
    /// 这是必要的，因为即使CUDNN路径被添加到PATH中，已经加载的模块可能仍然找不到依赖项。
    #[cfg(target_os = "windows")]
    fn preload_cudnn_libraries(cudnn_path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        use std::ffi::CString;

        // CUDNN相关库文件列表（按依赖顺序排列）
        let cudnn_libraries = [
            "cudnn64_9.dll",
            "cudnn_ops64_9.dll",
            "cudnn_cnn64_9.dll",
            "cudnn_adv64_9.dll",
            "cudnn_graph64_9.dll",
            "cudnn_engines_precompiled64_9.dll",
            "cudnn_engines_runtime_compiled64_9.dll",
            "cudnn_heuristic64_9.dll",
        ];

        // Windows API声明 - 需要 unsafe 因为调用外部系统函数
        // 这是预加载 CUDNN 库所必需的，用于 ONNX Runtime GPU 加速
        unsafe extern "system" {
            fn LoadLibraryA(lpLibFileName: *const i8) -> *mut std::ffi::c_void;
            fn GetLastError() -> u32;
        }

        let mut loaded_count = 0;

        for lib_name in &cudnn_libraries {
            let lib_path = cudnn_path.join(lib_name);
            if lib_path.exists() {
                if let Some(lib_path_str) = lib_path.to_str()
                    && let Ok(c_path) = CString::new(lib_path_str)
                {
                    // 预加载动态库 - 调用 Windows API 需要 unsafe 块
                    let handle = unsafe { LoadLibraryA(c_path.as_ptr()) };
                    if handle.is_null() {
                        let error_code = unsafe { GetLastError() };
                        warn!("加载 {} 失败，错误代码: {}", lib_name, error_code);
                    } else {
                        info!("✅ 成功预加载: {}", lib_name);
                        loaded_count += 1;
                    }
                }
            } else {
                // 对于不存在的库文件，只记录调试信息，不视为错误
                info!("跳过不存在的库: {}", lib_name);
            }
        }

        if loaded_count > 0 {
            info!("预加载完成，成功加载 {} 个CUDNN库", loaded_count);
            Ok(())
        } else {
            Err("未能预加载任何CUDNN库".into())
        }
    }
}

/// 环境变量工具模块
pub mod env_utils {
    /// 从环境变量读取配置值的辅助函数
    pub fn env_or_default<T: std::str::FromStr>(key: &str, default: T) -> T {
        std::env::var(key).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
    }

    /// 从环境变量读取字符串配置的辅助函数
    pub fn env_string_or_default(key: &str, default: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| default.to_string())
    }

    /// 从环境变量读取布尔值的辅助函数
    pub fn env_bool_or_default(key: &str, default: bool) -> bool {
        std::env::var(key)
            .ok()
            .and_then(|s| match s.to_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => Some(true),
                "false" | "0" | "no" | "off" => Some(false),
                _ => None,
            })
            .unwrap_or(default)
    }

    /// 从环境变量读取可选值的辅助函数
    pub fn env_optional<T: std::str::FromStr>(key: &str) -> Option<T> {
        std::env::var(key).ok().and_then(|s| s.parse().ok())
    }
}

/// 文本过滤与规范化工具
pub mod text_filters {
    use zhconv::{Variant, zhconv};

    /// 繁简转换模式
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub enum ConvertMode {
        /// 不转换
        #[default]
        None,
        /// 繁体转简体
        T2S,
        /// 简体转繁体
        S2T,
    }

    impl ConvertMode {
        /// 转换为字符串
        pub fn as_str(&self) -> &'static str {
            match self {
                Self::None => "none",
                Self::T2S => "t2s",
                Self::S2T => "s2t",
            }
        }
    }

    impl From<&str> for ConvertMode {
        fn from(s: &str) -> Self {
            match s.to_lowercase().as_str() {
                "t2s" | "traditional_to_simplified" => Self::T2S,
                "s2t" | "simplified_to_traditional" => Self::S2T,
                "none" | "" => Self::None,
                _ => {
                    tracing::warn!("未知的繁简转换模式: {}, 使用 None", s);
                    Self::None
                },
            }
        }
    }

    /// 使用指定模式转换文本
    ///
    /// # Arguments
    /// * `input` - 输入文本
    /// * `mode` - 转换模式
    ///
    /// # Returns
    /// 转换后的文本，如果 mode 为 None 则返回原文本
    pub fn convert_text(input: &str, mode: ConvertMode) -> String {
        match mode {
            ConvertMode::None => input.to_string(),
            ConvertMode::T2S => zhconv(input, Variant::ZhCN), // 转为大陆简体
            ConvertMode::S2T => zhconv(input, Variant::ZhTW), // 转为台湾繁体
        }
    }

    /// 移除文本中的所有 emoji 字符
    ///
    /// 使用 `emojis` crate 进行精准识别，确保不误删其他字符
    ///
    /// # Arguments
    /// * `input` - 输入文本
    ///
    /// # Returns
    /// 移除所有 emoji 后的文本
    pub fn remove_emojis(input: &str) -> String {
        input
            .chars()
            .filter(|c| {
                let s = c.to_string();
                emojis::get(&s).is_none()
            })
            .collect()
    }

    /// 过滤 LLM 异常输出中的函数调用参数泄露
    ///
    /// 当小模型 function calling 不稳定时，可能输出类似：
    /// `translate_text({"text": "您好", "target_language": "ko"})`
    /// 的纯文本。此函数从中提取 text 字段值作为有效回复。
    ///
    /// # Arguments
    /// * `input` - 输入文本
    ///
    /// # Returns
    /// 过滤后的文本，如果匹配到函数调用模式则提取 text 字段，否则返回原文本
    pub fn filter_function_call_leak(input: &str) -> String {
        use regex::Regex;
        use std::sync::OnceLock;

        // 匹配 函数名({"text": "...", ...}) 模式
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| Regex::new(r#"\w+\s*\(\s*\{[^}]*"text"\s*:\s*"([^"]+)"[^}]*\}\s*\)"#).unwrap());

        if let Some(caps) = re.captures(input) {
            if let Some(text_match) = caps.get(1) {
                let extracted = text_match.as_str();
                tracing::warn!(
                    "⚠️ 检测到函数调用参数泄露，已提取 text 字段: '{}' -> '{}'",
                    input.chars().take(50).collect::<String>(),
                    extracted
                );
                return extracted.to_string();
            }
        }
        input.to_string()
    }

    /// 过滤 markdown 引用符号
    ///
    /// 移除行首的 `>` 和紧跟的空格，处理 LLM 输出 markdown 引用格式的情况。
    /// 例如：`> 海纳百川，有容乃大` -> `海纳百川，有容乃大`
    ///
    /// # Arguments
    /// * `input` - 输入文本
    ///
    /// # Returns
    /// 移除 markdown 引用符号后的文本
    pub fn filter_markdown_quote(input: &str) -> String {
        input
            .lines()
            .map(|line| {
                let trimmed = line.trim_start();
                if let Some(rest) = trimmed.strip_prefix('>') {
                    rest.trim_start()
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 应用所有 TTS 前置过滤器
    ///
    /// 统一入口，依次应用：
    /// 1. 函数调用参数泄露过滤（提取 text 字段）
    /// 2. 繁简转换（如有配置）
    ///
    /// 注：Markdown 符号（* ` # >）在 routing::sanitize_visible_text 中统一处理
    ///
    /// # Arguments
    /// * `input` - 输入文本
    /// * `mode` - 繁简转换模式
    ///
    /// # Returns
    /// 过滤后的文本
    pub fn filter_for_tts(input: &str, mode: ConvertMode) -> String {
        let step1 = filter_function_call_leak(input);
        let step2 = remove_emojis(&step1);
        convert_text(&step2, mode)
    }
}

/// 从文本中提取所有 emoji 字符（去重）
///
/// 该函数扫描输入文本，识别所有 emoji 符号，并返回去重后的列表。
/// 使用 `emojis` crate 进行精准识别。
///
/// # Arguments
/// * `text` - 输入文本
///
/// # Returns
/// 去重后的 emoji 符号列表，按出现顺序排列
///
/// # Example
/// ```
/// let text = "你好 😊 世界 😊 欢迎 🎉";
/// let emojis = extract_emojis_from_text(text);
/// assert_eq!(emojis, vec!["😊".to_string(), "🎉".to_string()]);
/// ```
pub fn extract_emojis_from_text(text: &str) -> Vec<String> {
    let mut emojis_found = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for ch in text.chars() {
        let s = ch.to_string();
        if emojis::get(&s).is_some() && !seen.contains(&s) {
            seen.insert(s.clone());
            emojis_found.push(s);
        }
    }

    emojis_found
}
