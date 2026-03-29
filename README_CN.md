# Realtime API

**开源实时语音 AI 平台 —— 模块化 ASR、LLM、TTS 管线**
OpenAI Realtime API 的自托管替代方案 —— 端到端延迟 ≤450ms，支持 100+ 并发会话

[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE) [![Rust](https://img.shields.io/badge/rust-nightly-orange.svg)](https://www.rust-lang.org/) [![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](https://github.com/SquadyAI/RealtimeAPI/pulls)

快速开始 • [Playground](#playground-体验) • [API 文档](docs/Realtime_API_Guide.md) • [架构](#架构) • [贡献](#贡献)

[English](./README.md) | [简体中文](./README_CN.md)

---

## 为什么选择 Realtime API？

OpenAI 的 Realtime API 很强大，但**价格高**（$0.06+/分钟）、**无法私有部署**、**被供应商绑定**。我们做了一个可以跑在自己服务器上的实时语音 AI 平台，模型随便选。


|        | OpenAI Realtime | **本项目**                           |
| ------ | --------------- | --------------------------------- |
| 部署方式   | 仅云服务            | **自托管**                           |
| 数据安全   | 数据经第三方          | **完全私有**                          |
| LLM 选择 | 仅 GPT-4o        | **任意 OpenAI 兼容 API**              |
| TTS 选择 | 仅 OpenAI        | **Edge / MiniMax / Azure / 火山引擎** |
| ASR 选择 | 仅 Whisper       | **SenseVoice / WhisperLive**      |
| 成本     | 按分钟计费           | **固定服务器成本**                       |
| 延迟     | ~500ms          | **≤450ms**                        |


## 快速开始 (macOS / Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/SquadyAI/RealtimeAPI/main/server/install.sh | bash
realtime onboard
```

向导会引导你配置 LLM、TTS、ASR，然后自动构建并启动服务。打开 **[http://localhost:8080](http://localhost:8080)** 即可体验内置的 Playground。

> **前置条件：** 一个 OpenAI 兼容的 LLM API Key（OpenAI / DeepSeek / 通义千问 / Ollama 均可）。

**Docker 部署**

```bash
docker run -p 8080:8080 \
  -e LLM_BASE_URL=https://api.openai.com/v1 \
  -e LLM_API_KEY=sk-xxx \
  -e LLM_MODEL=gpt-4o-mini \
  ghcr.io/squadyai/realtimeapi:latest
```

**从源码安装**

```bash
git clone https://github.com/SquadyAI/RealtimeAPI.git && cd RealtimeAPI/server
cp .env.example .env   # 编辑 .env，至少设置 LLM_BASE_URL 和 LLM_MODEL
cargo build --release
realtime onboard
```

需要 [Rust nightly](https://rustup.rs/) 和 cmake。

完整配置参考 [.env.example](server/.env.example)。

<details>
<summary><strong>Windows 用户</strong></summary>

一键安装脚本需要 bash（macOS / Linux）。Windows 用户可以通过 WSL 或原生方式安装：

**方式 A — WSL（推荐）：**
```bash
wsl --install          # 如果还没有 WSL
# 在 WSL 中运行标准安装命令
curl -fsSL https://raw.githubusercontent.com/SquadyAI/RealtimeAPI/main/server/install.sh | bash
```

**方式 B — 原生 Windows：**
```powershell
git clone https://github.com/SquadyAI/RealtimeAPI.git
cd RealtimeAPI\server
copy .env.example .env
cargo build --release
.\target\release\realtime.exe
```

启动前编辑 `.env`，至少设置：
- `LLM_BASE_URL` — 如 `https://api.groq.com/openai/v1`（在 [console.groq.com](https://console.groq.com) 免费注册）
- `LLM_API_KEY` — 你的 API Key
- `LLM_MODEL` — 如 `llama-3.3-70b-versatile`
- `WHISPERLIVE_PATH` — 你的 WhisperLive WebSocket 地址

> 交互式 `realtime onboard` 向导仅支持 bash（macOS / Linux / WSL）。原生 Windows 请手动编辑 `.env`。

需要 [Rust](https://rustup.rs/)、cmake 和 Visual Studio Build Tools（C++ 工作负载）。

</details>

## Playground 体验

启动服务后，打开 **[http://localhost:8080](http://localhost:8080)** —— 内置的语音对话界面，无需额外配置。

点击 Squady 图标，开口说话，AI 实时回复。

> **在线演示：** [https://port2.luxhub.top:2097](https://port2.luxhub.top:2097)

## 你可以用它做什么

- **语音助手** —— 智能音箱、车载助手、客服机器人
- **实时翻译** —— 25+ 语言同声传译
- **智能设备控制** —— 语音操控 IoT，内置 27 个 Function Calling Agent
- **AI 教学** —— 实时语音反馈的互动式语言学习
- **无障碍工具** —— 为应用提供语音交互能力

## 架构

```
                          WebSocket (Opus 音频)
              ┌────────────────────────────────────────┐
              │                                        │
              ▼                                        │
┌───────────────────┐         ┌────────────────────────┴───────────────────────────┐
│                   │         │                Realtime API 服务端                  │
│      客户端       │         │                                                    │
│                   │  Opus   │  ┌───────┐   ┌─────┐   ┌─────┐   ┌─────┐          │
│   麦克风 ─────────┼────────▶│  │  VAD  │──▶│ ASR │──▶│ LLM │──▶│ TTS │──┐       │
│                   │         │  │Silero │   │Whis-│   │Open-│   │Edge/│  │       │
│                   │  Opus   │  │+Smart │   │per- │   │ AI  │   │Mini-│  │       │
│   扬声器 ◀────────┼────────◀│  │ Turn  │   │Live │   │兼容  │   │Max/ │  │       │
│                   │         │  └───────┘   └─────┘   └──┬──┘   │Azure│  │       │
│                   │         │                           │      └─────┘  │       │
└───────────────────┘         │                           ▼        ▲      │       │
                              │                    ┌────────────┐  │  Paced      │
                              │                    │   Agents   │  │  Sender     │
                              │                    │ + MCP 工具  │──┘  ◀──┘       │
                              │                    └────────────┘                 │
                              │                                                    │
                              └────────────────────────────────────────────────────┘
```

### 管线模式


| Protocol ID | 模式         | 管线              |
| ----------- | ---------- | --------------- |
| `100`       | 完整语音对话（默认） | ASR → LLM → TTS |
| `1`         | 仅语音识别      | 音频 → 文本         |
| `2`         | 仅文本对话      | 文本 → 文本         |
| `3`         | 仅语音合成      | 文本 → 音频         |
| `4`         | 同声传译       | 实时翻译            |


## 支持的服务


| 类别      | 服务商                | 状态  | 说明                              |
| ------- | ------------------ | --- | ------------------------------- |
| **ASR** | SenseVoice (ONNX)  | 默认  | 内置，25+ 语言，支持方言                  |
| **ASR** | WhisperLive        | 备选  | 外部服务，流式识别                       |
| **LLM** | 任意 OpenAI 兼容       | 默认  | GPT、DeepSeek、通义千问、Ollama、vLLM 等 |
| **TTS** | Edge TTS           | 默认  | 免费，100+ 语言                      |
| **TTS** | MiniMax            | 备选  | 中文优化，50+ 音色                     |
| **TTS** | Azure Speech       | 备选  | 高质量，多语言                         |
| **TTS** | 火山引擎               | 备选  | 中文音色                            |
| **TTS** | 百度 TTS             | 备选  | 中文音色                            |
| **VAD** | Silero + SmartTurn | 默认  | 双层检测：声学(32ms) + 语义              |
| **工具**  | MCP 协议             | 内置  | 动态扩展工具                          |
| **工具**  | 27 个 Agent         | 内置  | 搜索、翻译、导航、设备控制等                  |


## 性能指标


| 指标       | 数值       |
| -------- | -------- |
| 端到端延迟    | ≤ 450ms  |
| VAD 检测延迟 | 32ms/帧   |
| 并发会话数    | 100+（单机） |
| 基础内存占用   | ~200MB   |


**生产级特性：**

- 熔断器 —— 外部服务故障自动降级
- 连接池 —— 减少 LLM/TTS 连接开销
- 优雅关闭 —— 在途会话安全结束
- 结构化日志（tracing）+ Prometheus 指标 + Langfuse 集成
- TTS 参数热更新，无需重启

## WebSocket API

```javascript
const ws = new WebSocket('ws://localhost:8080/ws');

// 1. 配置会话
ws.send(JSON.stringify({
  protocol_id: 100,
  command_id: 1,
  session_id: 'my-session',
  payload: {
    type: 'session_config',
    mode: 'vad',
    system_prompt: '你是一个友好的助手',
    voice_setting: { voice_id: 'zh_female_wanwanxiaohe_moon_bigtts' }
  }
}));

// 2. 发送音频（二进制：32字节头 + PCM16 数据）
ws.send(audioBuffer);

// 3. 接收响应
ws.onmessage = (event) => {
  if (typeof event.data === 'string') {
    const msg = JSON.parse(event.data);
    // ASR 转写、LLM 文本增量、Function Call...
  } else {
    // TTS 音频片段 —— 直接播放
  }
};
```

完整协议参考：[Realtime_API_Guide.md](docs/Realtime_API_Guide.md)

## CLI 命令

```bash
realtime onboard     # 交互式设置向导
realtime onboard       # 启动服务（日志写入 logs/realtime.log）
realtime doctor      # 诊断配置和连接问题
```

## 配置

所有配置通过环境变量管理。设置向导（`realtime onboard`）会交互式地帮你完成。


| 变量                        | 必填  | 说明                                                   | 默认值            |
| ------------------------- | --- | ---------------------------------------------------- | -------------- |
| `LLM_BASE_URL`            | 是   | OpenAI 兼容 API 地址                                     | —              |
| `LLM_MODEL`               | 是   | 模型名称                                                 | —              |
| `LLM_API_KEY`             | 否   | API Key（自托管 LLM 可不填）                                 | —              |
| `ENABLE_TTS`              | 否   | 启用语音合成                                               | `true`         |
| `TTS_ENGINE`              | 否   | TTS 引擎 (`edge`, `minimax`, `azure`, `volc`, `baidu`) | `edge`         |
| `BIND_ADDR`               | 否   | 监听地址                                                 | `0.0.0.0:8080` |
| `VAD_THRESHOLD`           | 否   | VAD 灵敏度 (0.0–1.0)                                    | `0.6`          |
| `MAX_CONCURRENT_SESSIONS` | 否   | 最大并发会话数                                              | `100`          |


完整配置参考 [.env.example](server/.env.example)。

## 项目结构

```
src/
├── main.rs                 # 入口
├── rpc/                    # WebSocket 服务、会话管理、管线工厂
│   └── pipeline/           # ASR→LLM→TTS 编排、翻译等
├── asr/                    # SenseVoice (ONNX) + WhisperLive
├── llm/                    # OpenAI 兼容客户端、Function Calling、对话历史
├── tts/                    # Edge、MiniMax、Azure、火山引擎、百度
├── vad/                    # Silero VAD + SmartTurn 语义检测
├── agents/                 # 27 个 Function Calling Agent
├── mcp/                    # MCP 协议客户端
├── audio/                  # PCM 预处理、重采样、Opus 编解码
└── storage/                # PostgreSQL + 内存回退
```

## 文档


| 文档                                            | 说明             |
| --------------------------------------------- | -------------- |
| [API 指南](docs/Realtime_API_Guide.md)          | WebSocket 协议详解 |
| [架构设计](docs/architecture.md)                  | 系统架构概览         |
| [.env.example](server/.env.example)              | 完整配置参考         |


## 贡献

欢迎贡献！请阅读 [CONTRIBUTING.md](CONTRIBUTING.md)。

```bash
cargo build              # Debug 构建
cargo test               # 运行测试
realtime doctor          # 验证配置
```

## Roadmap

| 功能 | 说明 | 状态 |
|------|------|------|
| **长期记忆** | 跨会话记忆，用户偏好持久化 | 计划中 |
| **Agent 协作** | 多 Agent 编排与任务分发 | 计划中 |
| **多模态** | 视觉输入（摄像头/屏幕截图）+ 语音，接 GPT-4o 类模型 | 计划中 |
| **语音克隆** | Few-shot 声音克隆，用自己的声音对话 | 计划中 |
| **说话人识别** | 区分"谁在说话"，多人场景 | 计划中 |
| **声纹认证** | 基于声纹的身份验证 | 计划中 |
| **托管 ASR** | 零部署 ASR 服务，新用户开箱即用 | 计划中 |

有想法或想贡献？[提 Issue](https://github.com/SquadyAI/RealtimeAPI/issues) 或查看 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 许可证

[Apache License 2.0](LICENSE)