# 系统架构文档

## 概述

实时语音对话系统采用 Pipeline-based 模块化架构，基于 Rust nightly (edition 2024) 和 Tokio 异步运行时构建。系统设计目标：

- **低延迟**: 端到端延迟控制在 450ms 以内
- **高并发**: 支持 100+ 用户同时会话
- **可扩展**: Protocol ID 驱动的 Pipeline 工厂，按需组合 ASR/LLM/TTS
- **高可用**: 分层错误处理，CleanupGuard 自动资源回收，孤儿会话超时清理

## 源码结构

```
server/src/
├── rpc/                    # WebSocket 服务器、会话管理、Pipeline 系统
│   ├── session_manager.rs  # 会话生命周期管理
│   ├── session_router.rs   # 消息路由
│   ├── actix_websocket.rs  # Actix-Web WebSocket 连接处理
│   ├── actix_rpc_system.rs # RPC 框架
│   ├── protocol.rs         # 二进制协议定义 (ProtocolId, CommandId)
│   ├── pipeline_factory.rs # Pipeline 工厂 (protocol_id → Pipeline)
│   ├── event_handler.rs    # 事件分发
│   ├── realtime_event.rs   # 事件类型定义
│   ├── tts_pool.rs         # TTS 连接池与 failover
│   └── pipeline/           # 各类 Pipeline 实现
│       ├── asr_llm_tts/    # 完整对话 Pipeline (protocol_id=100)
│       ├── asr_only/       # 纯 ASR Pipeline (protocol_id=1)
│       ├── llm_tts/        # 文本→LLM→TTS Pipeline (protocol_id=2)
│       ├── tts_only/       # 纯 TTS Pipeline (protocol_id=3)
│       ├── translation/    # 翻译 Pipeline (protocol_id=4)
│       ├── vision_tts/     # 图像→LLM→TTS Pipeline
│       └── paced_sender.rs # 音频定时发送控制
├── asr/                    # 语音识别 (SenseVoice ONNX + WhisperLive)
├── llm/                    # LLM 客户端 (OpenAI-compatible API)
├── tts/                    # 语音合成 (MiniMax, VolcEngine, Edge, Azure, Baidu)
├── vad/                    # 语音活动检测 (Silero 帧级 + SmartTurn 语义级)
├── agents/                 # 功能代理 (19 个 Agent)
├── function_callback/      # 内置工具调用 (搜索、计算、同传、世界时钟)
├── mcp/                    # Model Context Protocol 客户端
├── audio/                  # 音频处理 (PCM/Opus 编解码、AGC、降噪)
├── storage/                # 持久化存储 (PostgreSQL + 内存回退)
├── ip_geolocation/         # IP 地理定位 (MaxMindDB)
├── geodata/                # 城市数据库
├── lib.rs                  # 公共导出、SystemError 定义
├── main.rs                 # 服务启动与初始化
├── lang.rs                 # 语言检测 (lingua)
├── text_splitter.rs        # TTS 文本分句
├── telemetry.rs            # Prometheus 指标采集
└── monitoring.rs           # 系统监控
```

## 核心模块

### 1. Pipeline 系统

**位置**: `src/rpc/pipeline/`

Pipeline 系统是整个架构的核心。客户端通过 `protocol_id` 选择 Pipeline 类型，由 PipelineFactory 动态创建：

```rust
#[repr(u8)]
pub enum ProtocolId {
    Asr = 1,          // 纯语音识别
    Llm = 2,          // 文本→LLM→TTS
    Tts = 3,          // 纯语音合成
    Translation = 4,  // 翻译
    All = 100,        // 完整对话 (ASR→LLM→TTS)
}
```

所有 Pipeline 实现统一的 `StreamingPipeline` trait：

```rust
#[async_trait]
pub trait StreamingPipeline: Send + Sync + Any {
    async fn start(&self) -> Result<CleanupGuard>;
    async fn on_upstream(&self, payload: BinaryMessage) -> Result<()>;
    fn as_any(&self) -> &dyn Any;
    async fn handle_tool_call_result(&self, result: ToolCallResult) -> Result<()>;
    async fn apply_session_config(&self, payload: &MessagePayload) -> Result<()>;
}
```

**Pipeline 类型**：

| protocol_id | Pipeline | 说明 |
|-------------|----------|------|
| `100` (All) | `ModularPipeline` | 完整对话：音频→VAD→ASR→LLM→Agent→TTS→音频 |
| `1` (Asr) | `AsrOnlyPipeline` | 纯语音识别，返回文本 |
| `2` (Llm) | `LlmTtsPipeline` | 文本输入→LLM→TTS→音频 |
| `3` (Tts) | `EnhancedStreamingPipeline` | 纯文本→TTS→音频 |
| `4` (Translation) | `TranslationPipeline` | 语音→ASR→翻译→TTS |
| — | `VisionTtsPipeline` | 图像→LLM→TTS→音频 |

**PipelineFactory** 根据 protocol_id 路由创建：

```rust
pub struct PipelineFactory {
    router: Arc<SessionRouter>,
    asr_engine: Arc<AsrEngine>,
    llm_client: Option<Arc<LlmClient>>,
    mcp_manager: Arc<McpManager>,
    mcp_prompt_registry: Arc<McpPromptRegistry>,
}
```

**完整对话 Pipeline (protocol_id=100)** 内部组件：

```
asr_llm_tts/
├── orchestrator.rs              # ModularPipeline 主编排器
├── asr_task.rs                  # ASR 任务入口
├── asr_task_core.rs             # ASR 核心逻辑
├── asr_task_vad.rs              # VAD 驱动的 ASR
├── asr_task_vad_deferred.rs     # 延迟 VAD 模式
├── asr_task_ptt.rs              # Push-to-Talk 模式
├── tts_task.rs                  # TTS 处理与流控
├── session_audio_sender.rs      # 音频输出管理
├── simple_interrupt_manager.rs  # 用户打断处理
├── tool_call_manager.rs         # 工具调用路由
├── routing.rs                   # Function Calling 路由
├── guided_choice_selector.rs    # 约束生成选择
├── sentence_queue.rs            # TTS 句子缓冲队列
├── timing_manager.rs            # 延迟追踪
├── event_emitter.rs             # 事件发布
├── lockfree_response_id.rs      # 无锁响应 ID 生成
└── intent.rs                    # 意图识别集成
```

### 2. 会话管理 (Session Management)

**位置**: `src/rpc/session_manager.rs`

SessionManager 管理 Pipeline 生命周期，而非直接管理 Session 对象：

```rust
pub struct SessionManager {
    router: Arc<SessionRouter>,
    active_pipelines: Arc<DashMap<String, Arc<dyn StreamingPipeline + Send + Sync>>>,
    cleanup_guards: Arc<DashMap<String, CleanupGuard>>,
    orphan_since: Arc<DashMap<String, Instant>>,
    store: Arc<dyn ConversationStore>,
    pipeline_factory: Arc<dyn PipelineFactory + Send + Sync>,
    creation_notifiers: Arc<DashMap<String, Arc<Notify>>>,
}
```

**主要功能**:
- Pipeline 创建与销毁（基于 PipelineFactory）
- 孤儿会话检测与自动超时回收
- CleanupGuard RAII 模式确保资源释放
- 创建通知器避免并发创建竞争

### 3. 二进制协议

**位置**: `src/rpc/protocol.rs`

客户端与服务器之间使用二进制帧协议通信：

```rust
// 帧头结构：32 字节
pub const NANOID_SIZE: usize = 16;         // 会话 ID
pub const PROTOCOL_ID_OFFSET: usize = 16;  // Pipeline 类型
pub const COMMAND_ID_OFFSET: usize = 17;   // 命令类型
pub const BINARY_HEADER_SIZE: usize = 32;  // 总头部大小

#[repr(u8)]
pub enum CommandId {
    Start = 1,              // 启动会话
    Stop = 2,               // 停止会话
    AudioChunk = 3,         // 音频数据帧
    TextData = 4,           // 文本数据
    StopInput = 5,          // 停止输入（不停止会话）
    ImageData = 6,          // 图像数据（视觉输入）
    Interrupt = 7,          // 用户打断
    ResponseAudioDelta = 20,// 响应音频增量
    Result = 100,           // 结果
    Error = 255,            // 错误
}
```

### 4. 音频处理 (Audio Processing)

**位置**: `src/audio/`

```
audio/
├── mod.rs              # AudioFormat 定义、AudioError
├── input_processor.rs  # 输入验证与预处理
├── opus_proc.rs        # Opus 编解码
├── tts_frame.rs        # TTS 输出帧处理
├── enhancement.rs      # 音频增强
├── agc.rs              # 自动增益控制 (AGC)
├── saver.rs            # 音频片段保存
└── denoiser/           # 语音降噪
```

**音频格式**:

```rust
pub enum AudioFormat {
    PcmS16Le,     // 16-bit PCM 小端序 (默认)
    PcmS24Le,     // 24-bit PCM
    PcmS32Le,     // 32-bit PCM
    Opus,         // Opus 编码
    Other(u32),   // 其他格式
}
```

**内部标准格式**: f32 归一化 [-1.0, 1.0]，16kHz，单声道

**主要功能**:
- PCM/Opus 编解码
- 自动增益控制 (AGC)
- 语音降噪
- 音频格式转换与标准化

### 5. 语音识别 (ASR)

**位置**: `src/asr/`

```
asr/
├── mod.rs                          # AsrEngine、AsrResult、AsrError
├── backend.rs                      # AsrBackend trait
├── sensevoice/                     # SenseVoice (默认，本地 ONNX 推理)
│   ├── mod.rs                      # 模型加载与推理
│   ├── streaming_frontend.rs       # Mel 频谱图 + 特征提取
│   ├── ctc_decoder.rs              # CTC 解码器
│   └── wenet_aligned_decoder.rs    # WeNet 对齐解码器
├── whisperlive.rs                  # WhisperLive (WebSocket 远程调用)
├── parakeet/                       # Parakeet 后端
├── stabilizer.rs                   # 时序结果稳定器
└── punctuation.rs                  # 标点恢复
```

**AsrBackend trait** — 流式识别接口：

```rust
#[async_trait]
pub trait AsrBackend: Send + Sync {
    /// 增量流式识别：audio = 单声道 16kHz f32 PCM
    async fn streaming_recognition(
        &mut self, audio: &[f32], is_last: bool, enable_final_inference: bool,
    ) -> Result<Option<VoiceText>, Box<dyn Error + Send + Sync>>;

    fn reset_streaming(&mut self);
    fn session_reset(&mut self);              // 完全重置（含降噪器）
    fn soft_reset_streaming(&mut self);       // 软重置（保留部分上下文）
    fn intermediate_reset_streaming(&mut self);
    async fn intermediate_recognition(&mut self, min_features: u32) -> Result<Option<VoiceText>>;
}
```

**支持的后端**:
- **SenseVoice**: 本地 ONNX 推理，内置 Mel 频谱图前端、CTC 解码。默认后端
- **WhisperLive**: 通过 WebSocket 连接远程 WhisperLive 服务。回退方案
- **Parakeet**: 备选后端

**语音模式** (SpeechMode):
- `Streaming` — 持续流式识别
- `Ptt` — Push-to-Talk 按键说话
- `VadWithDeferred` — VAD 触发 + 延迟确认

### 6. 大语言模型 (LLM)

**位置**: `src/llm/`

```
llm/
├── llm.rs                   # LlmClient — OpenAI-compatible API 客户端
├── llm_task_v2.rs           # 流式 LLM 任务执行
└── mcp_prompt_registry.rs   # MCP 提示词注册表
```

LLM 客户端为统一的 OpenAI-compatible 实现，通过环境变量 `LLM_BASE_URL` / `LLM_API_KEY` / `LLM_MODEL` 配置，可对接任何兼容 OpenAI API 的服务：

**主要功能**:
- 流式响应 (SSE)
- Function Calling / Tool Use
- 对话历史管理
- 连接池与 HTTP 版本配置
- MCP 工具集成

### 7. 语音合成 (TTS)

**位置**: `src/tts/`

```
tts/
├── mod.rs         # 公共导出
├── minimax/       # MiniMax TTS (HTTP，120+ 声音，SSML)
├── volc_engine.rs # 火山引擎 TTS
├── edge/          # Edge TTS (微软免费)
├── azure/         # Azure TTS (微软企业级)
└── baidu/         # 百度 TTS
```

**TTS 连接池** (`src/rpc/tts_pool.rs`): 管理多 TTS 提供商的连接池、failover 和请求队列。

**支持的提供商**:

| 提供商 | 类型 | 说明 |
|--------|------|------|
| MiniMax | HTTP | 默认提供商，120+ 声音，SSML 支持 |
| VolcEngine | WebSocket | 火山引擎，中英文 |
| Edge TTS | WebSocket | 微软免费方案 |
| Azure TTS | HTTP | 微软企业级 |
| Baidu | HTTP + WebSocket | 百度 TTS |

### 8. 语音活动检测 (VAD)

**位置**: `src/vad/`

采用**双层 VAD 架构**，帧级检测与语义级判断协同工作：

```
vad/
├── mod.rs                # VADError 定义、公共导出
├── model.rs              # SileroVAD ONNX 模型加载
├── iterator.rs           # VAD 迭代循环、VadEvent / VadState
├── engine.rs             # VAD 引擎接口
├── config.rs             # VADConfig / VADPoolConfig
└── semantic_vad/         # SmartTurn 语义级 VAD
    ├── mod.rs            # SmartTurnPredictor / SmartTurnSession
    └── features.rs       # 声学特征提取
```

**第一层：Silero VAD (帧级，32ms)**
- 嵌入式 ONNX 模型 (`silero_vad_16k_op15.onnx`，1.8MB)
- 逐帧判断语音/静音，低延迟

**第二层：SmartTurn (语义级)**
- 嵌入式 ONNX 模型 (`smart-turn-v3.1-raw.onnx`，32MB)
- 检测自然对话话轮边界，减少误打断
- 可否决 Silero 的话轮结束判断

```rust
#[derive(Debug, Clone)]
pub struct VadEvent {
    pub audio: Vec<f32>,           // 语音数据
    pub is_first: bool,            // 是否为语音段起始
    pub is_last: bool,             // 是否为语音段结束
    pub smart_turn_vetoed: bool,   // SmartTurn 是否否决了话轮结束
}
```

### 9. 功能代理 (Agents)

**位置**: `src/agents/`

Agent 系统通过 Function Calling 实现，LLM 识别用户意图后路由到对应 Agent 执行：

```rust
#[async_trait]
pub trait Agent: Send + Sync {
    fn id(&self) -> &str;
    fn intents(&self) -> Vec<&str> { vec![self.id()] }
    async fn run(&self, ctx: AgentContext, handles: AgentHandles<'_>) -> anyhow::Result<()>;
}
```

**已实现的 Agent (19 个)**:

| Agent | 说明 |
|-------|------|
| `translate_agent` | 语言翻译与同声传译 |
| `search_agent` | 网络搜索 |
| `navigation_agent` | GPS 导航与路线规划 |
| `music_control_agent` | 音乐播放控制 |
| `media_agent` | 媒体播放（含喜马拉雅等 provider） |
| `reminder_agent` | 提醒设置 |
| `device_control_agent` | 设备设置控制 |
| `camera_photo_agent` | 拍照 |
| `camera_video_agent` | 录像 |
| `audio_recorder_agent` | 录音 |
| `photo_agent` | 相册管理 |
| `volume_agent` | 音量控制 |
| `volume_up_agent` | 增大音量 |
| `volume_down_agent` | 减小音量 |
| `goodbye_agent` | 结束会话 |
| `rejection_agent` | 拒绝处理 |
| `fallback_agent` | 兜底响应 |
| `role_extractor` | 用户角色/上下文提取 |
| `turn_tracker` | 对话状态追踪 |

**支撑模块**:
- `runtime.rs` — Agent 编排引擎
- `system_prompt_registry.rs` — 系统提示词管理
- `prompts/` — 各 Agent 专用提示词（assistant、device_control、goodbye、music、navigation、photo、rejection、reminder、search、volume、visual_qa）

### 10. 内置工具 (Function Callback)

**位置**: `src/function_callback/`

独立于 Agent 的轻量级内置工具调用系统：

```
function_callback/
├── mod.rs               # FunctionCallResult、FunctionCallbackError
├── searxng_client.rs    # SearXNG 搜索引擎客户端
├── math_calculator.rs   # 数学计算
└── tools/
    ├── search.rs        # 搜索工具
    ├── math.rs          # 计算工具
    ├── simul_interpret.rs # 同声传译
    ├── world_clock.rs   # 世界时钟
    └── tavily_client.rs # Tavily 搜索客户端
```

```rust
pub struct FunctionCallResult {
    pub function_name: String,
    pub parameters: FxHashMap<String, serde_json::Value>,
    pub result: CallResult,
    pub timestamp: SystemTime,
    pub latency_ms: u64,
    pub success: bool,
    pub error_message: Option<String>,
    pub metadata: FxHashMap<String, String>,
}
```

### 11. MCP 协议模块

**位置**: `src/mcp/`

Model Context Protocol 客户端，用于集成外部工具服务：

```
mcp/
├── mod.rs                    # 公共导出、McpError
├── client.rs                 # MCP 客户端
├── manager.rs                # McpManager 连接池管理
├── async_tools_manager.rs    # 异步工具执行
├── tool_cache.rs             # 工具定义缓存
├── tools_endpoint_client.rs  # HTTP 端点客户端
├── http_client.rs            # HTTP 传输
├── protocol.rs               # MCP 协议类型
└── error.rs                  # McpError 定义
```

```rust
pub struct McpManager {
    clients: Arc<RwLock<FxHashMap<String, McpClientEntry>>>,
    cleanup_interval: Duration,
    idle_timeout: Duration,
}
```

**功能**:
- 客户端连接池（endpoint → client），自动清理空闲连接
- 异步工具发现与执行
- 工具定义缓存
- HTTP / WebSocket MCP 端点支持

### 12. 存储层 (Storage)

**位置**: `src/storage/`

采用**双 Store 架构**，分离对话记录和原始会话数据：

**ConversationStore** — 对话记录与配置：

```rust
#[async_trait]
pub trait ConversationStore: Send + Sync {
    async fn load(&self, session_id: &str) -> Result<Option<ConversationRecord>>;
    async fn save(&self, record: &ConversationRecord) -> Result<()>;
    async fn delete(&self, session_id: &str) -> Result<()>;
    async fn list_sessions(&self) -> Result<Vec<String>>;
}
```

**SessionDataStore** — 原始音频/图像/元数据归档：

```rust
#[async_trait]
pub trait SessionDataStore: Send + Sync {
    async fn save_asr_audio_data(&self, response_id: &str, session_id: &str,
        user_audio_chunks: Option<Bytes>, metadata: Option<AudioMetadata>) -> Result<()>;
    async fn save_tts_audio_data(&self, response_id: &str, session_id: &str,
        tts_output_audio: Option<Bytes>, metadata: Option<AudioMetadata>) -> Result<()>;
    async fn save_conversation_metadata(&self, response_id: &str, session_id: &str,
        llm_to_tts_text: String, metadata: Option<HashMap<String, Value>>) -> Result<()>;
    async fn save_vision_image_data(&self, response_id: &str, session_id: &str,
        image_data: Bytes, metadata: ImageMetadata, prompt: Option<String>,
        llm_response: Option<String>) -> Result<()>;
}
```

**后端实现**:
- **PostgreSQL**: `PgStore` + `PgSessionDataStore`，生产级。表：`conversations`、`session_configs`、`asr_audio_data`、`tts_audio_data`、`conversation_metadata`、`vision_image_data`
- **内存存储**: `InMemoryStore` + `InMemorySessionDataStore`，LRU 缓存，开发测试用

## 数据流

### 完整对话流 (protocol_id=100)

```
客户端音频 → [二进制协议解帧]
    → VAD (Silero 帧级检测 → SmartTurn 语义确认)
    → ASR (SenseVoice ONNX 本地推理)
    → LLM (OpenAI-compatible API，流式响应)
    → [可选: Function Calling → Agent 执行 / 内置工具调用]
    → TTS (MiniMax/VolcEngine/Edge/Azure/Baidu)
    → [PacedSender 定时发送]
    → 客户端音频
```

### 事件流

```
SessionRouter ← 客户端二进制帧
    → SessionManager (Pipeline 查找/创建)
    → StreamingPipeline.on_upstream(BinaryMessage)
    → Pipeline 内部处理
    → SessionRouter → 客户端
```

### 打断流

```
客户端 Interrupt (CommandId=7)
    → SimpleInterruptManager
    → 停止当前 ASR/LLM/TTS 输出
    → 清空 SentenceQueue
    → 保持会话存活
```

## 并发模型

### 异步运行时

基于 Tokio 多线程运行时，Actix-Web 作为 HTTP/WebSocket 框架：

- 每个 WebSocket 连接在独立的 Actix actor 中处理
- Pipeline 内部通过 `tokio::spawn` 创建 ASR/LLM/TTS 异步任务
- 使用 `mpsc::channel` 在任务间传递数据

### 并发数据结构

- `DashMap` — 无锁并发哈希表，用于 session 和 pipeline 存储
- `Arc<AtomicBool>` — 共享标志位 (SharedFlags)，控制打断/信号状态
- `lockfree_response_id` — 无锁响应 ID 生成器
- `Arc<Notify>` — 异步通知器，避免 Pipeline 创建竞争

### 资源管理

```rust
// CleanupGuard 模式：Pipeline.start() 返回 CleanupGuard，
// Drop 时自动清理 ASR/LLM/TTS 相关资源
pub struct CleanupGuard { /* ... */ }

// 孤儿会话检测：SessionManager 追踪未绑定路由的会话，
// 超时后自动回收
orphan_since: Arc<DashMap<String, Instant>>
```

## 错误处理

系统采用分层错误类型，每个模块定义独立的错误枚举，通过 `thiserror` 派生，最终汇聚到 `SystemError`：

```rust
#[derive(Debug, thiserror::Error)]
pub enum SystemError {
    #[error("音频处理错误: {0}")]
    Audio(#[from] AudioError),
    #[error("语音识别错误: {0}")]
    Asr(#[from] AsrError),
    #[error("功能回调错误: {0}")]
    FunctionCallback(#[from] FunctionCallbackError),
    #[error("MCP客户端错误: {0}")]
    Mcp(#[from] McpError),
    #[error("RPC通信错误: {0}")]
    Rpc(String),
    #[error("配置错误: {0}")]
    Config(String),
    #[error("初始化错误: {0}")]
    Initialization(String),
    #[error("运行时错误: {0}")]
    Runtime(String),
}
```

**模块级错误**:
- `AudioError` — IO、配置、解码、网络
- `AsrError` — 识别失败、配置、IO、会话未启动
- `RpcError` — WebSocket、会话、音频、超时、资源耗尽、序列化
- `McpError` — 连接、协议、JSON-RPC、超时、工具未找到、认证
- `FunctionCallbackError` — 功能未找到、参数无效、API 调用失败、超时、权限
- `VADError` — 模型加载、输入无效、ONNX 运行时、资源不可用

## 构建配置

### Feature Flags

```toml
[features]
default = ["binary-audio"]
binary-audio = []   # 二进制协议，512B/帧，16kHz (生产)
text-audio = []     # JSON 协议 (兼容模式)
```

### Release Profile

```toml
[profile.release]
opt-level = "z"       # 体积优化
codegen-units = 1     # 最大优化
lto = "thin"          # Thin LTO
strip = "symbols"     # 去除符号
```

### 内存分配器

使用 MiMalloc 替代系统分配器，优化多线程场景性能。

### 关键依赖

| 依赖 | 版本 | 用途 |
|------|------|------|
| tokio | 1.46 | 异步运行时 |
| actix-web | 4.5 | WebSocket 服务器 |
| ort | 2.0-rc.10 | ONNX Runtime (Silero, SmartTurn, SenseVoice) |
| sqlx | 0.8 | PostgreSQL ORM |
| prometheus | 0.14 | 指标采集 |
| tracing | 0.1 | 结构化日志 |
| dashmap | 6.1 | 并发哈希表 |
| reqwest | — | HTTP 客户端 |
| tokio-tungstenite | — | WebSocket 客户端 |
| lingua | 1.7 | 语言检测 |

## 可观测性

### 日志 (tracing)

基于 `tracing` + `tracing-subscriber`，支持结构化日志和 `#[instrument]` 自动追踪：

```rust
#[instrument(skip(self))]
pub async fn process_audio(&self, audio: &[f32]) -> Result<AsrResult> {
    info!(samples = audio.len(), "开始处理音频数据");
    // ...
}
```

### 指标 (Prometheus)

**位置**: `src/telemetry.rs`

通过 `http://localhost:8080/metrics` 暴露 Prometheus 指标，覆盖 ASR/LLM/TTS 延迟、会话数等。

### 系统监控

**位置**: `src/monitoring.rs`

运行时系统状态监控。

## 部署

### 单机部署

```
客户端 → WebSocket (ws://host:8080/ws) → Realtime API (单进程)
                                              ↓
                                         PostgreSQL (可选)
```

### Docker 部署

```bash
# CPU 镜像
docker build -f Dockerfile.cpu -t realtime-api:latest .
# GPU 镜像 (ONNX Runtime CUDA)
docker build -f Dockerfile.gpu -t realtime-api:latest-gpu .
# 编排
docker-compose up -d
```

### 健康检查

```
GET  http://localhost:8080/health    # 健康状态
GET  http://localhost:8080/metrics   # Prometheus 指标
WS   ws://localhost:8080/ws          # WebSocket 入口
```

### 配置

所有配置通过环境变量驱动（参见 `.env.example`，116 项）。关键配置：

| 变量 | 说明 |
|------|------|
| `LLM_API_KEY` | LLM API 密钥 |
| `LLM_BASE_URL` | LLM API 地址 |
| `LLM_MODEL` | 模型名称 |
| `BIND_ADDR` | 监听地址 (默认 0.0.0.0:8080) |
| `ENABLE_TTS` | 是否启用 TTS |
| `VAD_THRESHOLD` | VAD 阈值 |
