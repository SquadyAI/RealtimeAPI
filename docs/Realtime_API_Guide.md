# Realtime WebSocket API 快速接入指南

> 首次接入？直接看 [30秒快速体验](#12-30秒快速体验) 或 [模式一：完整语音对话](#4-模式一完整语音对话)

---

## 目录

| 章节 | 内容 | 阅读建议 |
|------|------|---------|
| [1. 概述](#1-概述与快速入门) | 系统介绍、模式选择 | 必读 |
| [2. 服务端点](#2-服务端点与连接) | 连接地址 | 必读 |
| [3. 核心概念](#3-核心概念解释) | 消息格式说明 | 必读 |
| [4. 完整语音对话](#4-模式一完整语音对话-protocol_id100) | 用户说话→AI语音回复 | **最常用** |
| [5. 纯ASR](#5-纯asr语音识别-protocol_id1) | 语音转文字 | 按需 |
| [6. 纯TTS](#6-纯tts语音合成-protocol_id3) | 文字转语音 | 按需 |
| [7. 文本对话+TTS](#7-文本对话tts-protocol_id2) | 打字→AI语音回复 | 按需 |
| [8. 同声传译](#8-同声传译-protocol_id4) | 实时语音翻译 | 按需 |
| [9. 视觉识别](#9-视觉识别tts) | 看图说话 | 按需 |
| [10. 高级功能](#10-高级功能可选) | 内置工具、自定义工具、打断 | 可选 |
| [11. 音频格式](#11-音频格式规范) | 输入/输出音频规范 | 查阅 |
| [12. 错误处理](#12-错误处理) | 错误码、错误消息格式 | 查阅 |
| [13. 附录](#13-附录) | 音色列表、语言代码 | 查阅 |

---

## 1. 概述与快速入门

### 1.1 这个系统是什么？

一句话：**让你的应用能"听懂人话、语音回答"**

```
用户说话 ──> 语音转文字(ASR) ──> AI思考(LLM) ──> 文字转语音(TTS) ──> AI回答
```

- **ASR** = 语音转文字（像讯飞输入法）
- **LLM** = AI大脑（像ChatGPT）
- **TTS** = 文字转语音（像导航播报）

### 1.2 30秒快速体验

复制以下代码即可运行：

```javascript
// 1. 连接（天才测试环境）
const ws = new WebSocket('ws://localhost:8080/ws');

// 2. 连接成功后，创建会话
ws.onopen = () => {
    ws.send(JSON.stringify({
        protocol_id: 100,
        command_id: 1,
        session_id: "sess000000000001",  // 固定16字符
        payload: {
            type: "session_config",
            mode: "vad_deferred",
            system_prompt: "你是一个友好的AI助手，用简短的语言回答问题"
        }
    }));
};

// 3. 接收消息
ws.onmessage = (event) => {
    const msg = JSON.parse(event.data);
    console.log('收到:', msg.payload?.type, msg);

    // 收到 session.created 后，就可以开始发送音频了
    if (msg.payload?.type === 'session.created') {
        console.log('✅ 会话创建成功！可以开始发送音频了');
    }
};
```

**验证成功的标志**：控制台打印 `✅ 会话创建成功！`

### 1.3 常见问题速查

| 问题 | 原因 | 解决 |
|------|------|------|
| 连接失败 | 端点地址错误 | 检查 [服务端点](#2-服务端点与连接) |
| 没收到 session.created | session_id 格式问题 | 用字母+数字，**固定16字符** |
| 发送音频无响应 | 音频格式错误 | 检查格式（Opus/PCM）和采样率（推荐16kHz） |
| AI 不说话 | 没配置 voice_setting | 添加 voice_id 配置 |
| 识别结果为空 | 说话声音太小/太短 | 调低 vad_threshold |

### 1.4 选择适合你的模式

根据你的使用场景，选择对应的接入模式：

```
你需要什么功能？
    │
    ├── 用户说话，AI语音回答 ──────────> 模式一：完整语音对话 (推荐)
    │
    ├── 把文字变成语音播报 ────────────> 模式二：纯TTS语音合成
    │
    ├── 用户打字，AI语音回答 ──────────> 模式三：文本对话+TTS
    │
    ├── 实时语音翻译（如英译中）────────> 模式四：同声传译
    │
    └── 让AI看图说话 ──────────────────> 模式五：视觉识别+TTS
```

### 1.5 快速接入 5 步流程

```
步骤1        步骤2         步骤3         步骤4         步骤5
┌─────┐     ┌──────┐      ┌──────┐      ┌──────┐      ┌──────┐
│建立  │ --> │发送   │ --> │发送   │ --> │接收   │ --> │结束   │
│连接  │     │Start │      │数据   │      │结果   │      │会话   │
└─────┘     └──────┘      └──────┘      └──────┘      └──────┘
WebSocket    创建会话       音频/文字      文字/音频      Stop
```

---

## 2. 服务端点与连接

### 2.1 服务端点列表

#### 本地开发环境

| 环境 | WebSocket端点 | HTTPS端点 |
|------|--------------|-----------|
| 测试 | `ws://localhost:8080/ws` | `http://localhost:19444/v2/chat/completions` |

### 2.2 建立 WebSocket 连接

```javascript
// 伪代码示例
const ws = new WebSocket('ws://localhost:8080/ws');

ws.onopen = function() {
    console.log('连接成功，可以开始发送消息了');
};

ws.onmessage = function(event) {
    // 处理服务器返回的消息
    const message = JSON.parse(event.data);
    console.log('收到消息:', message);
};

ws.onerror = function(error) {
    console.log('连接出错:', error);
};

ws.onclose = function() {
    console.log('连接已关闭');
};
```

---

## 3. 核心概念解释

### 3.1 消息结构

每条消息都包含 4 个基本字段：

```json
{
    "protocol_id": 100,              // 服务类型
    "command_id": 1,                 // 操作类型
    "session_id": "my_session_0001",  // 会话标识（固定16字符）
    "payload": { ... }               // 具体内容
}
```

### 3.2 字段说明

| 字段 | 作用 | 类比 |
|------|------|------|
| `session_id` | 会话的唯一标识 | 像电话通话的通话ID，用来区分不同的对话 |
| `protocol_id` | 选择使用哪种服务 | 像选择打电话还是发短信 |
| `command_id` | 告诉服务器要做什么操作 | 像说"开始通话"、"挂断电话" |
| `payload` | 具体的数据内容 | 像通话中说的具体内容 |

#### session_id 格式要求

| 要求 | 说明 |
|------|------|
| **长度** | 固定 **16 字符**（不能多也不能少） |
| **字符集** | 字母（a-z, A-Z）和数字（0-9），推荐使用 nanoid 生成 |
| **唯一性** | 同一时间内不同会话必须使用不同的 session_id |
| **格式示例** | `AI9myot94lOEZffQ`、`sess000000000001` |

> **生成建议**：
> - JavaScript: `import { nanoid } from 'nanoid'; const sessionId = nanoid(16);`
> - Python: `from nanoid import generate; session_id = generate(size=16)`
> - 也可以使用简单的字母数字组合，如 `"user001_sess0001"`（确保16字符）

### 3.3 protocol_id 服务类型表

| protocol_id | 服务类型 | 用途 | 状态 |
|-------------|---------|------|------|
| `1` | ASR | 纯语音识别（输入音频，输出文本） | ✅ 可用 |
| `2` | LLM+TTS | 文本对话+语音合成（输入文本，输出文本+音频） | ✅ 可用 |
| `3` | TTS | 纯语音合成（输入文本，输出音频） | ✅ 可用 |
| `4` | Translation | 同声传译服务 | ✅ 可用 |
| `100` | All | 完整流程（ASR+LLM+TTS） | ✅ 可用 |

### 3.4 command_id 操作类型表

| command_id | 操作名称 | 说明 |
|------------|---------|------|
| `1` | Start | 创建/开始会话 |
| `2` | Stop | 结束/销毁会话 |
| `3` | AudioChunk | 发送音频数据 |
| `4` | TextData | 发送文本数据 |
| `5` | StopInput | 停止输入（不销毁会话） |
| `6` | ImageData | 发送图片数据 |
| `7` | Interrupt | 打断当前 AI 回复（不销毁会话） |
| `20` | ResponseAudioDelta | 服务器返回的音频数据（二进制） |
| `100` | Result | 服务器返回的结果（JSON） |
| `255` | Error | 错误信息 |

---

## 4. 模式一：完整语音对话 (protocol_id=100)

> **最常用的场景**：用户说话 → AI理解 → AI语音回复

### 4.1 工作流程图（vad_deferred 模式）

```
客户端                              服务器
  │                                   │
  │──── 1. Start (创建会话) ─────────>│
  │<─── 2. session.created ───────────│
  │                                   │
  │════ 3. AudioChunk (发送语音) ════>│  ← 持续发送
  │<─── 4. speech_started ────────────│  ← 检测到说话
  │                                   │
  │     ... 用户说话中 ...             │
  │                                   │
  │<─── 5. speech_stopped ────────────│  ← 检测到停止
  │<─── 6. ASR识别结果 ───────────────│  ← "你好，今天天气怎么样"
  │                                   │
  │──── 7. StopInput ────────────────>│  ← 【关键】触发AI回复
  │                                   │
  │<─── 8. LLM文本流 ─────────────────│  ← "今天天气..."（流式）
  │<─── 9. TTS音频流 ─────────────────│  ← 音频数据（流式）
  │<─── 10. output_audio_buffer.stopped│  ← 本轮结束
  │                                   │
  │──── 11. Stop (结束会话) ──────────>│
  │                                   │
```

> **vad_deferred 模式**：收到 `speech_stopped` 后，需要客户端发送 `StopInput` 才会触发AI回复，这样更可控。

### 4.2 三种子模式

| 模式 | 说明 | 适用场景 |
|------|------|---------|
| `vad_deferred` | VAD检测语音结束后，等待客户端发送 StopInput 再触发LLM | **推荐使用**，更可控 |
| `vad` | 自动检测说话开始/结束，立即触发LLM | 纯自然对话 |
| `ptt` | 按住说话，松开结束 | 嘈杂环境、精确控制 |

> **推荐 `vad_deferred`**：收到 `speech_stopped` 后，客户端可以决定是否发送 `StopInput` 来触发AI回复，避免误触发。

### 4.3 接入步骤

#### 步骤 1：发送 Start 创建会话

```json
{
    "protocol_id": 100,
    "command_id": 1,
    "session_id": "你的会话ID",
    "payload": {
        "type": "session_config",
        "mode": "vad_deferred",
        "system_prompt": "你是一个友好的AI助手",
        "voice_setting": {
            "voice_id": "zh_female_wanwanxiaohe_moon_bigtts"
        }
    }
}
```

#### 步骤 2：等待 session.created 响应

```json
{
    "protocol_id": 100,
    "command_id": 100,
    "session_id": "你的会话ID",
    "payload": {
        "type": "session.created"
    }
}
```

> **注意**: 服务器可能返回多次 `session.created` 消息，客户端应只处理第一次。
>
> **为什么会收到多次？** 由于管线内部各组件（ASR、LLM、TTS）是异步初始化的，某些边界情况下可能触发重复的会话创建事件。这是正常行为，客户端只需忽略后续的 `session.created` 即可。

#### 步骤 3：发送音频数据（二进制格式）

```
┌──────────────────────────────────────────────────────────┐
│                    32字节消息头                           │
├──────────────┬────────────┬────────────┬────────────────┤
│ session_id   │ protocol_id│ command_id │   reserved     │
│  (16字节)    │  (1字节)   │  (1字节)   │   (14字节)     │
│              │   = 100    │    = 3     │                │
├──────────────┴────────────┴────────────┴────────────────┤
│                    音频数据                              │
│         Opus 16kHz 单声道（推荐）或 PCM S16LE            │
└──────────────────────────────────────────────────────────┘
```

> 音频数据必须使用**二进制格式**发送，不支持 JSON Base64 编码
>
> **推荐使用 Opus 格式**：带宽更低、延迟更小，详见 [音频格式规范](#11-音频格式规范)

#### 步骤 4：接收服务器响应并处理

服务器会依次返回以下事件：

```
收到: speech_started        ← AI检测到你开始说话
收到: speech_stopped        ← AI检测到你停止说话
收到: ASR识别结果           ← "你好，今天天气怎么样"

[vad_deferred模式] 此时需要发送 StopInput 触发AI回复 ↓

收到: response.text.delta   ← AI回复文字（流式）
收到: response.audio.delta  ← AI语音回复（流式）
收到: output_audio_buffer.stopped  ← 本轮对话结束
```

**vad_deferred 模式重要步骤**：收到 `speech_stopped` 后，发送 StopInput 触发AI回复：

```json
{
    "protocol_id": 100,
    "command_id": 5,
    "session_id": "你的会话ID",
    "payload": null
}
```

#### 步骤 5：结束会话

```json
{
    "protocol_id": 100,
    "command_id": 2,
    "session_id": "你的会话ID",
    "payload": null
}
```

### 4.4 配置项说明（session_config 完整参数）

#### 基础参数

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `mode` | string | 否 | `"vad"` | 语音检测模式：`"vad"` / `"vad_deferred"` / `"ptt"` |
| `system_prompt` | string | 否 | - | AI的角色设定 |
| `vad_threshold` | float | 否 | `0.55` | VAD灵敏度 (0-1)，越高越不灵敏 |
| `silence_duration_ms` | int | 否 | `300` | 静音多久算说完（毫秒） |
| `min_speech_duration_ms` | int | 否 | - | 认定语音开始所需的最小连续语音时长（毫秒） |

#### 语音设置

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `voice_setting` | object | 否 | TTS语音设置，详见 [语音配置](#语音配置示例) |
| `asr_language` | string | 否 | ASR语言偏好：`"zh"` / `"en"` / `"yue"` / `"ja"` / `"ko"` / `"auto"` |

#### 工具与配置

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `tools` | array | 否 | Function Call 工具定义 |
| `tool_choice` | string/object | 否 | 工具选择策略：`"auto"` / `"none"` / 指定工具 |
| `mcp_server_config` | array | 否 | MCP 服务器配置（数组） |
| `tools_endpoint` | string | 否 | 从 HTTP 端点获取工具配置 |
| `prompt_endpoint` | string | 否 | 三合一远程配置端点（优先级最高） |
| `enable_search` | bool | 否 | 启用内置搜索工具（推荐使用此参数） |
| `search_config` | object | 否 | 搜索引擎高级配置（一般不需要） |

#### 音频配置

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `output_audio_config` | object | 否 | 音频输出配置（PCM/Opus格式） |
| `input_audio_config` | object | 否 | 音频输入处理器配置，见下表 |

**`input_audio_config` 字段说明**：

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `format` | string | `"pcm_s16_le"` | 输入格式：`"opus"`（推荐）、`"pcm_s16_le"`、`"pcm_s24_le"`、`"pcm_s32_le"` |
| `sample_rate` | int | `16000` | 采样率 (Hz)，支持 8000-192000，推荐 16000 |

配置示例：
```json
{
    "input_audio_config": {
        "format": "opus",
        "sample_rate": 16000
    }
}
```

**`output_audio_config` 字段说明**：

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `format` | string | `"pcm_s16_le"` | 输出格式：`"opus"`（推荐）、`"pcm_s16_le"` |
| `slice_ms` | int | `20` | 音频分片时长（毫秒），Opus 支持 5/10/20/40/60 |
| `opus_config` | object | - | Opus 编码配置，仅当 format 为 opus 时有效 |

**`opus_config` 字段说明**（可选）：

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `bitrate` | int | `32000` | 比特率 (bps)，推荐 24000-64000 |
| `complexity` | int | `2` | 编码复杂度 (0-10)，越高质量越好但CPU占用更高 |
| `application` | string | `"voip"` | 应用类型：`"voip"` / `"audio"` / `"restricted_lowdelay"` |
| `variable_bitrate` | bool | `true` | 是否启用可变比特率 |
| `dtx` | bool | `false` | 是否启用 DTX（不连续传输，静音时节省带宽） |
| `fec` | bool | `false` | 是否启用 FEC（前向纠错） |

配置示例：
```json
{
    "output_audio_config": {
        "format": "opus",
        "slice_ms": 20,
        "opus_config": {
            "bitrate": 32000,
            "complexity": 2
        }
    }
}
```

#### 其他参数

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `timezone` | string | 否 | 用户时区，如 `"Asia/Shanghai"` |
| `location` | string | 否 | 用户位置信息，如 `"中国"` |
| `text_done_signal_only` | bool | 否 | 当为 true 时，response.text.done 仅发送信令不携带文本 |
| `signal_only` | bool | 否 | 当为 true 时，除语音和工具调用外的所有事件都不发送 |
| `chinese_convert` | string | 否 | 繁简转换：`"none"` / `"t2s"` (繁→简) / `"s2t"` (简→繁) |

#### 信令优化参数使用场景

**`text_done_signal_only`** - 适用于以下场景：

| 场景 | 说明 |
|------|------|
| **带宽受限环境** | 当客户端只需要知道文本生成完成，但已通过 `response.text.delta` 流式接收了完整文本时，设为 `true` 可避免 `response.text.done` 重复发送完整文本 |
| **纯语音应用** | 客户端只播放音频，不显示文字，设为 `true` 减少不必要的数据传输 |

**`signal_only`** - 适用于以下场景：

| 场景 | 说明 |
|------|------|
| **极简客户端** | 只需要音频输出和工具调用，不关心中间状态事件（如 `response.created`、`conversation.item.created` 等） |
| **嵌入式设备** | 处理能力有限，只需处理核心的语音数据和工具调用 |
| **低延迟要求** | 减少事件处理开销，专注于音频播放 |

> **注意**：设置 `signal_only: true` 后，客户端仍会收到：
> - `response.audio.delta` - 音频数据（必须）
> - `response.function_call_arguments.done` - 工具调用请求（如果有）
> - `error` - 错误事件（如果发生）

#### 完整配置示例

```json
{
    "type": "session_config",
    "mode": "vad_deferred",
    "system_prompt": "你是一个友好的AI助手",
    "vad_threshold": 0.55,
    "silence_duration_ms": 300,
    "voice_setting": {
        "voice_id": "zh_female_wanwanxiaohe_moon_bigtts",
        "speed": 1.0
    },
    "asr_language": "zh",
    "enable_search": true,
    "output_audio_config": {
        "format": "pcm_s16_le",
        "slice_ms": 20
    },
    "timezone": "Asia/Shanghai"
}
```

### 4.5 常见问题

**Q: VAD检测太灵敏/不灵敏怎么办？**

A: 调整 `vad_threshold` 参数：
- 环境嘈杂时，调高到 0.6-0.7
- 环境安静时，调低到 0.4-0.5

**Q: AI回复太快被打断怎么办？**

A: 增加 `silence_duration_ms` 的值，比如设为 500-800ms

**Q: 如何实现"按住说话"？**

A: 使用 PTT 模式：
1. 设置 `mode: "ptt"`
2. 用户按下按钮时开始发送音频
3. 用户松开按钮时发送 `command_id: 5` (StopInput) 触发AI回复

---

## 5. 纯ASR语音识别 (protocol_id=1)

> **场景**：只做语音识别，不需要 AI 对话和语音合成。如语音转文字、语音搜索等场景。

### 5.1 工作流程图

```
客户端                              服务器
  │                                   │
  │──── 1. Start (创建会话) ─────────>│
  │<─── 2. session.created ───────────│
  │                                   │
  │──── 3. AudioChunk (发送音频) ────>│  ← 流式发送
  │<─── 4. speech_started ────────────│  ← 检测到说话
  │<─── 5. speech_stopped ────────────│  ← 检测到停止
  │                                   │
  │──── 6. StopInput ────────────────>│  ← 触发识别
  │<─── 7. transcription.completed ───│  ← 识别结果
  │                                   │
  │──── 8. Stop (结束会话) ───────────>│
  │                                   │
```

### 5.2 接入步骤

#### 步骤 1：发送 Start 创建会话

```json
{
    "protocol_id": 1,
    "command_id": 1,
    "session_id": "你的会话ID",
    "payload": {
        "type": "session_config",
        "mode": "vad",
        "vad_threshold": 0.5,
        "silence_duration_ms": 500
    }
}
```

#### 步骤 2：等待 session.created

```json
{
    "protocol_id": 1,
    "command_id": 100,
    "session_id": "你的会话ID",
    "payload": {
        "type": "session.created"
    }
}
```

#### 步骤 3：发送音频数据（二进制格式）

音频格式：PCM S16LE, 16kHz, 单声道

```
┌──────────────────────────────────────────────────────────┐
│                    32字节消息头                           │
├──────────────┬────────────┬────────────┬────────────────┤
│ session_id   │ protocol_id│ command_id │   reserved     │
│  (16字节)    │  = 1       │  = 3       │   (14字节)     │
├──────────────┴────────────┴────────────┴────────────────┤
│ audio_data (PCM S16LE, 16kHz, 单声道)                    │
└──────────────────────────────────────────────────────────┘
```

#### 步骤 4：发送 StopInput 触发识别

```json
{
    "protocol_id": 1,
    "command_id": 5,
    "session_id": "你的会话ID",
    "payload": null
}
```

#### 步骤 5：接收识别结果

```json
{
    "protocol_id": 1,
    "command_id": 100,
    "session_id": "你的会话ID",
    "payload": {
        "type": "conversation.item.input_audio_transcription.completed",
        "transcript": "识别出的文字内容"
    }
}
```

#### 步骤 6：结束会话

```json
{
    "protocol_id": 1,
    "command_id": 2,
    "session_id": "你的会话ID",
    "payload": null
}
```

### 5.3 常见问题

**Q: 如何获取中间识别结果？**

A: 服务端会发送 `conversation.item.input_audio_transcription.delta` 事件，包含实时识别的中间结果。

**Q: 支持哪些语言的语音识别？**

A: 支持中文、英文等多种语言，系统会自动检测语言。

---

## 6. 纯TTS语音合成 (protocol_id=3)

> **场景**：把文字转换成语音，如播报通知、朗读文章。与模式三（文本对话+TTS）的区别是：纯TTS模式**不经过LLM**，直接将输入文本转为语音。

### 6.1 工作流程图

```
客户端                              服务器
  │                                   │
  │──── 1. Start (创建会话) ─────────>│
  │<─── 2. session.created ───────────│
  │                                   │
  │──── 3. TextData (发送文字) ──────>│
  │──── 4. StopInput ────────────────>│  ← 触发TTS输出
  │                                   │
  │<─── 5. response.created ──────────│
  │<─── 6. output_audio_buffer.started│
  │<─── 7. response.text.delta ───────│  ← 文本回显（流式）
  │<─── 8. response.audio.delta ──────│  ← 音频数据（流式）
  │<─── 9. response.text.done ────────│
  │<─── 10. output_audio_buffer.stopped│
  │                                   │
  │──── 11. Stop (结束会话) ──────────>│
  │                                   │
```

### 6.2 接入步骤

#### 步骤 1：发送 Start 创建会话

```json
{
    "protocol_id": 3,
    "command_id": 1,
    "session_id": "你的会话ID",
    "payload": {
        "type": "session_config",
        "voice_setting": {
            "voice_id": "zh_female_wanwanxiaohe_moon_bigtts"
        }
    }
}
```

#### 步骤 2：等待 session.created 响应

```json
{
    "protocol_id": 3,
    "command_id": 100,
    "session_id": "你的会话ID",
    "payload": {
        "type": "session.created"
    }
}
```

#### 步骤 3：发送 TextData 文字内容

```json
{
    "protocol_id": 3,
    "command_id": 4,
    "session_id": "你的会话ID",
    "payload": {
        "type": "text_data",
        "text": "你好，欢迎使用语音合成服务"
    }
}
```

#### 步骤 4：发送 StopInput 触发 TTS 输出

```json
{
    "protocol_id": 3,
    "command_id": 5,
    "session_id": "你的会话ID",
    "payload": null
}
```

> **重要**：必须发送 StopInput 才会触发 TTS 合成和音频输出。

#### 步骤 5：接收音频数据（二进制格式）

服务器通过 WebSocket 二进制消息返回音频：

```
┌──────────────────────────────────────────────────────────┐
│                    32字节消息头                           │
├──────────────┬────────────┬────────────┬────────────────┤
│ session_id   │ protocol_id│ command_id │   reserved     │
│  (16字节)    │  = 100     │  = 20      │   (14字节)     │
├──────────────┴────────────┴────────────┴────────────────┤
│ response_id_len (4字节) + response_id_bytes (变长)       │
│ item_id_len (4字节) + item_id_bytes (变长)               │
│ output_index (4字节) + content_index (4字节)             │
│ audio_data (PCM S16LE, 16kHz, 单声道)                    │
└──────────────────────────────────────────────────────────┘
```

#### 步骤 6：结束会话

```json
{
    "protocol_id": 3,
    "command_id": 2,
    "session_id": "你的会话ID",
    "payload": null
}
```

### 6.3 语音选择指南

#### 中文语音（推荐）

| 语音名称 | voice_id | 特点 |
|---------|----------|------|
| 温柔女声 | `zh_female_wanwanxiaohe_moon_bigtts` | 女声，温柔自然 |
| 湘小妹 | `zh_female_meituojieer_moon_bigtts` | 女声，活泼可爱 |
| 侃大山 | `zh_male_jingqiangkanye_emo_mars_bigtts` | 男声，沉稳大气 |

#### 英文语音

| 语音名称 | voice_id | 特点 |
|---------|----------|------|
| Lauren | `en_female_lauren_moon_bigtts` | 女声，美式英语 |
| Ethan | `ICL_en_male_aussie_v1_tob` | 男声，澳洲英语 |

#### 语音配置示例

```json
{
    "voice_setting": {
        "voice_id": "zh_female_wanwanxiaohe_moon_bigtts",
        "speed": 1.0,
        "volume": 1.0,
        "pitch": 0.0
    }
}
```

| 参数 | 范围 | 说明 |
|------|------|------|
| `speed` | 0.5-2.0 | 语速，1.0为正常 |
| `volume` | 0.0-2.0 | 音量，1.0为正常 |
| `pitch` | -1.0-1.0 | 音调，0为正常 |

### 6.4 常见问题

**Q: 如何让语音更快/更慢？**

A: 调整 `speed` 参数，1.2表示快20%，0.8表示慢20%

**Q: 支持哪些语言的TTS？**

A: 支持中文、英文、日语、韩语等40+种语言，详见附录

**Q: 发送 TextData 后没有收到音频怎么办？**

A: 必须发送 `StopInput` (command_id=5) 才会触发 TTS 合成。流程是：Start → TextData → StopInput → 接收音频

**Q: 纯TTS模式和文本对话+TTS模式有什么区别？**

A:
- **纯TTS模式** (protocol_id=3)：输入文本**直接转语音**，不经过LLM处理
- **文本对话+TTS模式** (protocol_id=2)：输入文本会**先经过LLM处理**，AI生成回复后再转语音

**Q: 可以在一个会话中多次发送文本吗？**

A: 可以。在同一个会话中，可以多次发送 TextData + StopInput 来合成多段语音。

---

## 7. 文本对话+TTS (protocol_id=2)

> **场景**：用户打字输入，AI用语音回答。与模式二（纯TTS）的区别是：文本会**经过LLM处理**，AI生成回复后再转语音。

### 7.1 工作流程图

```
客户端                              服务器
  │                                   │
  │──── 1. Start (创建会话) ─────────>│
  │<─── 2. session.created ───────────│
  │                                   │
  │──── 3. TextData (发送文字) ──────>│
  │──── 4. StopInput ────────────────>│  ← 触发AI回复
  │<─── 5. response.created ──────────│
  │<─── 6. response.text.delta ───────│  ← AI文字回复（流式）
  │<─── 7. response.audio.delta ──────│  ← AI语音回复（流式）
  │<─── 8. response.text.done ────────│
  │<─── 9. output_audio_buffer.stopped│
  │                                   │
  │──── 10. Stop (结束会话) ──────────>│
  │                                   │
```

### 7.2 接入步骤

#### 步骤 1：创建会话

```json
{
    "protocol_id": 2,
    "command_id": 1,
    "session_id": "你的会话ID",
    "payload": {
        "type": "session_config",
        "system_prompt": "你是一个友好的AI助手",
        "voice_setting": {
            "voice_id": "zh_female_wanwanxiaohe_moon_bigtts"
        }
    }
}
```

#### 步骤 2：等待 session.created

```json
{
    "protocol_id": 2,
    "command_id": 100,
    "session_id": "你的会话ID",
    "payload": {
        "type": "session.created"
    }
}
```

#### 步骤 3：发送文本消息

```json
{
    "protocol_id": 2,
    "command_id": 4,
    "session_id": "你的会话ID",
    "payload": {
        "type": "text_data",
        "text": "今天天气怎么样？"
    }
}
```

#### 步骤 4：发送 StopInput 触发 AI 回复

```json
{
    "protocol_id": 2,
    "command_id": 5,
    "session_id": "你的会话ID",
    "payload": null
}
```

#### 步骤 5：接收AI回复

服务器会返回：
- `response.text.delta`: AI的文字回复（流式）
- `response.audio.delta`: AI的语音回复（流式二进制）

#### 步骤 6：结束会话

```json
{
    "protocol_id": 2,
    "command_id": 2,
    "session_id": "你的会话ID",
    "payload": null
}
```

### 7.3 打断 AI 回复

在 AI 语音回复过程中，客户端可以发送 Interrupt 命令（command_id=7）立即停止当前回复：

```json
{
    "protocol_id": 2,
    "command_id": 7,
    "session_id": "你的会话ID",
    "payload": null
}
```

**使用场景**：
- 用户点击"停止"按钮时
- 需要立即停止 AI 说话，但不结束对话

**服务器响应**：
```
收到: output_audio_buffer.cleared  ← 清空音频缓冲
收到: output_audio_buffer.stopped  ← 停止播放
```

> 打断后会话保持，可继续发送新的文本消息。

### 7.4 常见问题

**Q: 只想要文字回复，不要语音怎么办？**

A: 不配置 `voice_setting`，或设置 `output_audio_config: null`

**Q: 如何实现多轮对话？**

A: 保持同一个 `session_id`，系统会自动维护对话历史

**Q: protocol_id=2 和 protocol_id=100 有什么区别？**

A:
- **protocol_id=2**：只能发送文本，输出文本+音频（不支持语音输入）
- **protocol_id=100**：支持语音输入+文本输入，输出文本+音频

---

## 8. 同声传译 (protocol_id=4)

> **场景**：实时语音翻译，如英语翻译成中文

### 8.1 工作流程图

```
客户端                              服务器
  │                                   │
  │──── 1. Start ────────────────────>│
  │      from_language: "en"          │
  │      to_language: "zh"            │
  │<─── 2. session.created ───────────│
  │                                   │
  │════ 3. AudioChunk (英语) ════════>│  ← 发送源语言音频
  │<─── 4. transcription.completed ───│  ← "Hello, how are you?"（源语言文本）
  │<─── 5. response.text.delta ───────│  ← "你好，你好吗？"（翻译后文本，流式）
  │<─── 6. response.audio.delta ──────│  ← 中文语音（流式）
  │<─── 7. response.text.done ────────│  ← 翻译文本完成
  │<─── 8. output_audio_buffer.stopped│  ← 本轮结束
  │                                   │
```

### 8.2 输入输出说明

| 方向 | 类型 | 事件 | 说明 |
|------|------|------|------|
| **输入** | 音频 | `AudioChunk` | 只支持音频输入，不支持文本输入 |
| **输出** | 源语言文本 | `transcription.completed` | ASR 识别的源语言文本 |
| **输出** | 翻译后文本 | `response.text.delta` | 翻译后的目标语言文本（流式） |
| **输出** | 目标语言音频 | `response.audio.delta` | 翻译后的目标语言语音（流式） |

### 8.3 支持的语言

#### 常用语言

| 语言 | 代码 | 语言 | 代码 |
|------|------|------|------|
| 中文（普通话） | `zh` | 中文（粤语） | `zh-HK` |
| 英语（美式） | `en-US` | 日语 | `ja` |
| 韩语 | `ko` | 法语 | `fr` |
| 德语 | `de` | 西班牙语 | `es` |

> 系统支持 32 种语言，完整列表见 [附录](#134-支持的语言代码)

### 8.4 接入步骤

#### 步骤 1：创建翻译会话

```json
{
    "protocol_id": 4,
    "command_id": 1,
    "session_id": "translation_001",
    "payload": {
        "type": "session_config",
        "from_language": "en",
        "to_language": "zh",
        "mode": "vad_deferred",
        "voice_setting": {
            "voice_id": "zh_female_wanwanxiaohe_moon_bigtts"
        }
    }
}
```

#### 步骤 2：发送源语言音频

与模式一相同，发送 AudioChunk

#### 步骤 3：接收翻译结果

```
收到: input_audio_transcription.completed  ← 源语言识别结果 "Hello"
收到: response.text.delta                  ← 翻译后文本 "你好"（流式）
收到: response.audio.delta                 ← 目标语言音频（中文语音，流式）
收到: response.text.done                   ← 翻译文本完成
收到: output_audio_buffer.stopped          ← 本轮结束
```

### 8.5 配置项说明

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `from_language` | string | **是** | 源语言代码，如 `"en"` |
| `to_language` | string | **是** | 目标语言代码，如 `"zh"` |
| `mode` | string | 否 | `"vad"` / `"vad_deferred"` / `"ptt"` |
| `voice_setting` | object | 否 | 目标语言的语音设置，详见 [语音配置](#语音配置示例) |

### 8.6 常见问题

**Q: 可以中途切换语言吗？**

A: 不可以。需要结束当前会话，创建新会话。

**Q: 同声传译支持文本输入吗？**

A: 不支持。同声传译目前只能输入音频，不支持直接输入文本进行翻译。如需文本翻译，可以使用 LLM+TTS 模式 (protocol_id=2)，在 system_prompt 中设定翻译角色。

**Q: 翻译结果有文本输出吗？**

A: 有。服务器会返回 `response.text.delta` 事件，包含翻译后的目标语言文本（流式），与音频同步发送。

---

## 9. 视觉识别+TTS

> **场景**：让AI看图说话，如识别图片内容（使用 protocol_id=100，command_id=6 发送图片）

### 9.1 工作流程图

```
客户端                              服务器
  │                                   │
  │──── 1. Start (创建会话) ─────────>│
  │<─── 2. session.created ───────────│
  │                                   │
  │──── 3. ImageData ────────────────>│  ← 发送图片 + 提示词
  │      prompt: "描述这张图片"        │
  │      image: [图片数据]             │
  │<─── 4. response.text.delta ───────│  ← "这是一张..."
  │<─── 5. response.audio.delta ──────│  ← 语音描述
  │<─── 6. output_audio_buffer.stopped│
  │                                   │
```

### 9.2 图片格式要求

| 项目 | 要求 |
|------|------|
| 格式 | JPG / PNG |
| 大小 | 建议 < 5MB |
| 分辨率 | 建议 < 4096x4096 |

### 9.3 接入步骤

#### 发送图片数据（二进制格式）

```
┌──────────────────────────────────────────────────────────┐
│                    32字节消息头                           │
├──────────────┬────────────┬────────────┬────────────────┤
│ session_id   │ protocol_id│ command_id │   reserved     │
│  (16字节)    │  = 100     │  = 6       │   (14字节)     │
├──────────────┴────────────┴────────────┴────────────────┤
│ prompt_length (4字节，小端)                              │
├──────────────────────────────────────────────────────────┤
│ prompt_utf8_bytes (变长，UTF-8编码的提示词)               │
├──────────────────────────────────────────────────────────┤
│ image_data (变长，JPG/PNG图片数据)                        │
└──────────────────────────────────────────────────────────┘
```

### 9.4 常见问题

**Q: 支持多张图片吗？**

A: 目前单次请求只支持一张图片

**Q: 图片太大怎么办？**

A: 建议先在客户端压缩到 1MB 以内

---

## 10. 高级功能（可选）

> 以下功能为进阶内容，首次接入可跳过

### 10.1 内置工具

系统内置了以下工具，AI 会根据用户意图自动调用：

| 工具名称 | 功能 | 是否需要配置 |
|---------|------|-------------|
| `search_web` | 联网搜索 | 需要开启 `enable_search` |
| `calculate` | 数学计算 | 默认可用 |
| `world_clock` | 世界时钟/时区查询 | 默认可用 |
| `reminder` | 提醒设置 | 默认可用 |

#### 开启联网搜索

在 Start 消息的 payload 中添加 `enable_search: true`：

```json
{
    "type": "session_config",
    "mode": "vad_deferred",
    "enable_search": true,
    "system_prompt": "你是一个AI助手"
}
```

#### 内置工具调用流程

```
用户: "今天深圳天气怎么样"
  │
  ↓
AI 自动调用 search_web
  │
  ↓
┌────────────────────────────────────┐
│ 服务端自动执行搜索，结果写入上下文   │
│ （search_web 不发送结果给客户端）    │
└────────────────────────────────────┘
  │
  ↓
AI 根据搜索结果回答: "今天深圳晴天，温度25度..."
```

#### 内置工具结果事件

除 `search_web` 外，其他内置工具会发送 `function_call_result.done` 事件：

```json
{
    "type": "response.function_call_result.done",
    "call_id": "xxx",
    "result": "计算表达式「2+3*4」，等于14"
}
```

> **注意**：`result` 字段是 **String 类型**（格式化文本），不是 JSON 对象

### 10.2 自定义工具（Function Calling）

> 让AI调用你自己定义的外部服务

#### 什么是自定义工具？

```
用户: "今天北京天气怎么样？"
  │
  ↓
AI决定调用天气工具
  │
  ↓
┌────────────────────────────────────┐
│ 工具调用请求                        │
│ function_name: "get_weather"       │
│ arguments: {"city": "北京"}         │
└────────────────────────────────────┘
  │
  ↓
客户端执行工具，返回结果
  │
  ↓
AI根据结果回答: "今天北京晴天，温度25度"
```

#### 工具调用流程

```
客户端                              服务器
  │                                   │
  │<─── function_call_arguments.done ─│  ← AI要调用工具
  │                                   │
  │  [客户端执行工具，获取结果]         │
  │                                   │
  │──── conversation.item.create ────>│  ← 返回工具结果
  │      call_id: "call_xxx"          │
  │      output: "{...}"              │
  │                                   │
  │<─── response.text.delta ──────────│  ← AI继续回复
  │                                   │
```

#### 配置工具

在 Start 消息的 payload 中添加 tools：

```json
{
    "type": "session_config",
    "tools": [
        {
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "查询指定城市的天气",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "city": {
                            "type": "string",
                            "description": "城市名称"
                        }
                    },
                    "required": ["city"]
                }
            }
        }
    ]
}
```

#### 返回工具结果

当收到 `response.function_call_arguments.done` 后，需要：
1. 从消息中提取 `call_id` 字段
2. 执行工具调用获取结果
3. 使用**相同的 call_id** 返回结果

```json
{
    "protocol_id": 100,
    "command_id": 100,
    "session_id": "你的会话ID",
    "payload": {
        "type": "conversation.item.create",
        "item": {
            "type": "function_call_output",
            "call_id": "call_abc123",  // ← 必须与请求中的 call_id 一致
            "output": "{\"temperature\": 25, \"weather\": \"晴天\"}",
            "is_error": false
        }
    }
}
```

> **重要**：`call_id` 必须与 `function_call_arguments.done` 消息中的 `call_id` 完全匹配，否则AI无法正确关联工具结果

### 10.3 打断机制

> 用户在AI说话时可以打断

系统支持两种打断方式：

#### 方式一：语音打断（自动检测）

用户开始说话时，VAD 自动检测并触发打断。

```
客户端                              服务器
  │                                   │
  │<─── response.audio.delta ─────────│  ← AI正在说话
  │<─── response.audio.delta ─────────│
  │                                   │
  │════ AudioChunk (用户开始说话) ═══>│  ← 用户打断
  │<─── speech_started ───────────────│
  │<─── output_audio_buffer.cleared ──│  ← 清空音频缓冲
  │<─── output_audio_buffer.stopped ──│  ← 停止播放
  │                                   │
  │     [处理用户新的语音输入]          │
  │                                   │
```

#### 方式二：按钮打断（Interrupt 命令）

客户端主动发送 Interrupt 命令（command_id=7）打断当前 AI 回复，不销毁会话。

**使用场景**：
- 用户点击"停止"按钮时
- 需要立即停止 AI 说话，但不结束对话
- PTT 模式下的手动打断

**发送格式**：

```json
{
    "protocol_id": 100,
    "command_id": 7,
    "session_id": "你的会话ID",
    "payload": null
}
```

或使用二进制格式（仅需 32 字节头）：

```
┌──────────────────────────────────────────────────────────┐
│                    32字节消息头                           │
├──────────────┬────────────┬────────────┬────────────────┤
│ session_id   │ protocol_id│ command_id │   reserved     │
│  (16字节)    │  = 100     │  = 7       │   (14字节)     │
└──────────────┴────────────┴────────────┴────────────────┘
```

**服务器响应**：

收到 Interrupt 后，服务器会：
1. 立即停止当前 TTS 输出
2. 清空待发送的音频缓冲
3. 发送 `output_audio_buffer.cleared` 和 `output_audio_buffer.stopped` 事件

```
客户端                              服务器
  │                                   │
  │<─── response.audio.delta ─────────│  ← AI正在说话
  │                                   │
  │──── Interrupt (command_id=7) ────>│  ← 用户按钮打断
  │<─── output_audio_buffer.cleared ──│  ← 清空音频缓冲
  │<─── output_audio_buffer.stopped ──│  ← 停止播放
  │                                   │
  │     [会话保持，可继续交互]          │
  │                                   │
```

#### 客户端处理建议

1. 收到 `speech_started` 时：立即停止播放当前音频
2. 收到 `output_audio_buffer.cleared` 时：清空待播放的音频队列
3. 继续接收和处理新的用户输入

> **Interrupt vs Stop 的区别**：
> - `Interrupt` (command_id=7)：仅停止当前 AI 回复，**保持会话**，可继续对话
> - `Stop` (command_id=2)：**销毁会话**，结束整个对话

### 10.4 MCP 服务器配置

> 连接外部 MCP (Model Context Protocol) 服务器执行工具调用

#### MCP 服务器配置

```json
{
    "mcp_server_config": [
        {
            "endpoint": "https://your-mcp-server.com/mcp",
            "authorization": "Bearer your-token",
            "timeout_secs": 30,
            "tool_cache_ttl_secs": 300
        }
    ]
}
```

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `endpoint` | string | **是** | MCP 服务器 URL (支持 http/https/ws/wss) |
| `authorization` | string | 否 | JWT 授权令牌，格式为 `Bearer xxx` |
| `timeout_secs` | int | 否 | 请求超时时间（秒），默认 30 |
| `reconnect_interval_secs` | int | 否 | WebSocket 重连间隔（秒），默认 5，仅 ws/wss 协议有效 |
| `max_reconnect_attempts` | int | 否 | WebSocket 最大重连次数，默认 3，仅 ws/wss 协议有效 |
| `tool_cache_ttl_secs` | int | 否 | 工具列表缓存 TTL（秒），默认 300 |

> **协议说明**：
> - **HTTP/HTTPS**: 使用 HTTP 短连接方式调用工具，每次请求独立
> - **WS/WSS**: 使用 WebSocket 长连接方式，支持自动重连

#### MCP Control Type（工具响应控制）

MCP 服务器返回结果时，通过 `control.mode` 字段指示后续处理方式：

| mode | 说明 |
|------|------|
| `llm` | 将工具结果返回给 LLM 继续对话（默认） |
| `tts` | 将 payload 内容直接发送到 TTS 合成 |
| `stop` | 停止当前对话，不再继续处理 |

**MCP 响应格式示例**：

```json
{
    "control": {
        "mode": "tts"
    },
    "payload": "今天天气晴朗，温度25度"
}
```

### 10.5 三合一远程配置（prompt_endpoint）

> 从远程 URL 一次性获取 system_prompt、tools、mcp_server_config、search_config

在 Start 消息的 payload 中设置 `prompt_endpoint`：

```json
{
    "type": "session_config",
    "prompt_endpoint": "https://your-server.com/api/prompt-config"
}
```

**远程配置响应格式**：

```json
{
    "system_prompt": "你是一个AI助手",
    "tools": [
        {
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "查询天气",
                "parameters": { ... }
            }
        }
    ],
    "mcp_server_config": [
        {
            "endpoint": "https://mcp.example.com/mcp"
        }
    ],
    "search_config": {
        "enabled": true
    }
}
```

> **优先级**：`prompt_endpoint` 的配置优先级最高，会覆盖其他同名字段

---

## 11. 音频格式规范

### 11.1 输入音频格式

服务器支持两种输入格式：

#### 推荐格式：Opus（带宽低、延迟小）

| 项目 | 要求 |
|------|------|
| 格式 | Opus 编码 |
| 采样率 | 16000 Hz (16kHz) |
| 声道 | 单声道 (Mono) |
| 帧时长 | 20ms（推荐） |

#### 备选格式：PCM

| 项目 | 要求 |
|------|------|
| 格式 | PCM S16LE (16位有符号小端) |
| 采样率 | 16000 Hz (16kHz) |
| 声道 | 单声道 (Mono) |
| 位深度 | 16 bit |

> **推荐使用 Opus**：相比 PCM，Opus 带宽节省约 90%，且具有更好的抗丢包能力。

#### 通过 input_audio_config 配置输入格式

可在 `session_config` 中通过 `input_audio_config` 指定输入音频格式：

| format 值 | 说明 |
|-----------|------|
| `opus` | Opus 编码（推荐） |
| `pcm_s16_le` | 16位 PCM（默认） |
| `pcm_s24_le` | 24位 PCM |
| `pcm_s32_le` | 32位 PCM |

### 11.2 输出音频格式

服务器支持两种输出格式，通过 `output_audio_config` 配置：

#### 推荐格式：Opus（带宽低、延迟小）

| 项目 | 要求 |
|------|------|
| 格式 | Opus 编码 |
| 采样率 | 16000 Hz (16kHz) |
| 声道 | 单声道 (Mono) |
| 帧时长 | 20ms（推荐），支持 5/10/20/40/60 ms |

#### 备选格式：PCM（默认）

| 项目 | 要求 |
|------|------|
| 格式 | PCM S16LE (16位有符号小端) |
| 采样率 | 16000 Hz (16kHz) |
| 声道 | 单声道 (Mono) |
| 位深度 | 16 bit |

> **推荐使用 Opus**：带宽节省约 90%，需在 `output_audio_config` 中配置。

配置示例：
```json
{
    "output_audio_config": {
        "format": "opus",
        "slice_ms": 20,
        "opus_config": {
            "bitrate": 32000,
            "complexity": 2
        }
    }
}
```

### 11.3 传输格式

音频数据**仅支持二进制格式**传输，不支持 JSON Base64 编码。

二进制格式优点：
- 效率高，节省带宽
- 延迟更低

### 11.3.1 接收音频解包指南

服务器返回的 `response.audio.delta` 是二进制消息，格式如下：

```
┌──────────────────────────────────────────────────────────┐
│                    32字节消息头                           │
├──────────────┬────────────┬────────────┬────────────────┤
│ session_id   │ protocol_id│ command_id │   reserved     │
│  (16字节)    │  = 100     │  = 20      │   (14字节)     │
├──────────────┴────────────┴────────────┴────────────────┤
│ response_id_len (4字节, 小端)                            │
│ response_id_bytes (变长)                                 │
│ item_id_len (4字节, 小端)                                │
│ item_id_bytes (变长)                                     │
│ output_index (4字节, 小端)                               │
│ content_index (4字节, 小端)                              │
│ audio_data (PCM S16LE 或 Opus, 取决于配置)              │
└──────────────────────────────────────────────────────────┘
```

**JavaScript 解包示例**：

```javascript
function parseAudioDelta(binaryData) {
    const view = new DataView(binaryData);
    let offset = 32; // 跳过32字节头

    // 解析 response_id
    const responseIdLen = view.getUint32(offset, true); // 小端序
    offset += 4;
    const responseId = new TextDecoder().decode(
        binaryData.slice(offset, offset + responseIdLen)
    );
    offset += responseIdLen;

    // 解析 item_id
    const itemIdLen = view.getUint32(offset, true);
    offset += 4;
    const itemId = new TextDecoder().decode(
        binaryData.slice(offset, offset + itemIdLen)
    );
    offset += itemIdLen;

    // 解析索引
    const outputIndex = view.getUint32(offset, true);
    offset += 4;
    const contentIndex = view.getUint32(offset, true);
    offset += 4;

    // 剩余部分是音频数据
    const audioData = binaryData.slice(offset);

    return { responseId, itemId, outputIndex, contentIndex, audioData };
}
```

**播放 PCM 音频示例**：

```javascript
// 将 PCM S16LE 数据播放到 AudioContext
function playPcmAudio(audioContext, pcmData) {
    const int16Array = new Int16Array(pcmData);
    const float32Array = new Float32Array(int16Array.length);

    // 转换为 [-1, 1] 范围
    for (let i = 0; i < int16Array.length; i++) {
        float32Array[i] = int16Array[i] / 32768;
    }

    const audioBuffer = audioContext.createBuffer(1, float32Array.length, 16000);
    audioBuffer.getChannelData(0).set(float32Array);

    const source = audioContext.createBufferSource();
    source.buffer = audioBuffer;
    source.connect(audioContext.destination);
    source.start();
}
```

### 11.4 音频发送建议

- 发送频率：每 20ms 发送一次 (320样本 @ 16kHz)，推荐值
- 缓冲大小：320-640 样本 (20-40ms)
- 也可以选择 100ms 发送一次 (1600样本)，但会增加延迟

---

## 12. 错误处理

### 12.1 错误码列表

#### 通用错误（`error` 事件）

| 错误码 | 说明 | 解决方案 |
|--------|------|---------|
| `400` | 请求格式错误 | 检查 JSON 格式和必填字段 |
| `408` | 会话创建超时 | 增加超时时间或重试 |
| `500` | 服务器内部错误（LLM失败等） | 稍后重试或联系技术支持 |
| `503` | TTS服务不可用 | 检查TTS服务状态，稍后重试 |
| `1001` | 没有检测到语音输入 | 检查麦克风权限和音频发送 |
| `1002` | 检测到打断后无有效语音输入 | 用户打断后未说话，可忽略 |
| `1003` | VAD超时且ASR转录为空 | 检查音频质量或调低 vad_threshold |

#### ASR转录失败（`conversation.item.input_audio_transcription.failed` 事件）

| code | 说明 | 解决方案 |
|------|------|---------|
| `no_output` | 打断后无有效语音输入 | 用户打断后未说话，可忽略 |
| `END_SPEECH_ERROR` | 语音结束处理错误 | 检查音频格式是否正确 |
| `PROCESS_ERROR` | 连续多次音频处理失败 | 检查音频数据完整性 |
| `timeout_no_output` | VAD超时且无转录结果 | 检查音频质量或调整VAD参数 |

> **注意**：错误详情请查看 `error.message` 字段，包含具体错误描述

### 12.2 错误消息格式

#### 通用错误（`error` 事件）

```json
{
    "protocol_id": 100,
    "command_id": 100,
    "session_id": "你的会话ID",
    "payload": {
        "type": "error",
        "error": {
            "type": "server_error",
            "code": 500,
            "message": "内部服务器错误"
        }
    }
}
```

#### ASR转录失败（`conversation.item.input_audio_transcription.failed` 事件）

```json
{
    "protocol_id": 100,
    "command_id": 100,
    "session_id": "你的会话ID",
    "payload": {
        "type": "conversation.item.input_audio_transcription.failed",
        "item_id": "item_xxxxxxxx",
        "content_index": 0,
        "error": {
            "type": "transcription_error",
            "code": "timeout_no_output",
            "message": "VAD超时且ASR转录为空"
        }
    }
}
```

### 12.3 重连策略

建议使用指数退避重连：

```
第1次重连：等待 1 秒
第2次重连：等待 2 秒
第3次重连：等待 4 秒
第4次重连：等待 8 秒
... 最大等待 30 秒
```

---

## 13. 附录

### 13.1 接入成功检查清单

在正式上线前，请确认以下几点：

| 检查项 | 验证方法 |
|--------|---------|
| ✅ WebSocket 连接成功 | 收到 `onopen` 回调 |
| ✅ 会话创建成功 | 收到 `session.created` 事件 |
| ✅ 能发送音频 | 发送 AudioChunk 无报错 |
| ✅ 能收到 ASR 结果 | 收到 `input_audio_transcription.completed` |
| ✅ 能收到 LLM 回复 | 收到 `response.text.delta` |
| ✅ 能收到 TTS 音频 | 收到 `response.audio.delta` |
| ✅ 能正常结束会话 | 发送 Stop 后连接正常关闭 |

### 13.2 完整消息类型速查表

#### 客户端 → 服务器

| 消息类型 | protocol_id | command_id | 说明 |
|---------|-------------|------------|------|
| Start | 1/2/3/4/100 | 1 | 创建会话 |
| Stop | 1/2/3/4/100 | 2 | 结束会话 |
| AudioChunk | 1/4/100 | 3 | 发送音频 |
| TextData | 2/3/100 | 4 | 发送文本 |
| StopInput | 1/2/3/4/100 | 5 | 停止输入，触发输出 |
| ImageData | 100 | 6 | 发送图片 |
| Interrupt | 1/2/3/4/100 | 7 | 打断当前 AI 回复 |
| conversation.item.create | 100 | 100 | 工具调用结果 |

#### 服务器 → 客户端

**JSON 消息** (command_id=100)：

| 事件类型 (payload.type) | 说明 |
|------------------------|------|
| `session.created` | 会话创建成功 |
| `session.update` | 会话配置已更新 |
| `conversation.item.created` | 对话项创建（用户/助手消息） |
| `conversation.item.updated` | 对话项更新 |
| `input_audio_buffer.speech_started` | 检测到说话开始 |
| `input_audio_buffer.speech_stopped` | 检测到说话结束 |
| `conversation.item.input_audio_transcription.delta` | ASR识别中间结果 |
| `conversation.item.input_audio_transcription.completed` | ASR识别完成 |
| `conversation.item.input_audio_transcription.failed` | ASR识别失败 |
| `response.created` | 开始生成回复 |
| `response.output_item.added` | 响应输出项添加 |
| `response.output_item.done` | 响应输出项完成 |
| `response.text.delta` | 文本回复（流式） |
| `response.text.done` | 文本回复完成 |
| `response.audio.done` | 音频回复完成 |
| `response.cancel` | 响应已取消 |
| `response.function_call_arguments.delta` | 工具调用参数（流式） |
| `response.function_call_arguments.done` | 工具调用参数完成 |
| `response.function_call_result.done` | 内置工具调用结果 |
| `output_audio_buffer.started` | 开始播放音频 |
| `output_audio_buffer.stopped` | 停止播放音频 |
| `output_audio_buffer.cleared` | 音频缓冲已清空 |
| `error` | 错误信息 |

> **注意**: 在 VAD 相关模式（`vad` 和 `vad_deferred`）下，首轮对话检测到 `speech_started` 时会发送 `output_audio_buffer.stopped`（此时实际没有音频在播放）。客户端不应将此事件作为对话结束的判断依据，而应检查是否已收到实际响应内容（文本或音频）后再结束对话。PTT 模式不受影响。

**二进制消息** (command_id=20)：

| 消息类型 | 说明 |
|---------|------|
| `response.audio.delta` | 音频回复（流式二进制数据，格式见 11.3.1） |

### 13.3 常用语音列表

#### 推荐语音（快速上手）

| 语言 | 推荐 voice_id | 说明 |
|------|--------------|------|
| 中文女声 | `zh_female_wanwanxiaohe_moon_bigtts` | 温柔自然，推荐 |
| 中文男声 | `zh_male_jingqiangkanye_emo_mars_bigtts` | 沉稳大气 |
| 英文女声 | `en_female_lauren_moon_bigtts` | 美式英语 |
| 英文男声 | `ICL_en_male_aussie_v1_tob` | 澳洲英语 |
| 日语女声 | `multi_female_gaolengyujie_moon_bigtts` | - |
| 粤语 | `zh-HK-HiuMaanNeural` | Azure语音 |

> 如需更多语音选项，请联系技术支持

### 13.4 支持的语言代码

系统支持 32 种语言，完整列表如下：

| # | 语言 | 代码 | # | 语言 | 代码 |
|---|------|------|---|------|------|
| 1 | 中文（普通话） | `zh` | 17 | 葡萄牙语（巴西） | `pt-BR` |
| 2 | 中文（粤语） | `zh-HK` | 18 | 意大利语 | `it` |
| 3 | 英语（美式） | `en-US` | 19 | 俄语 | `ru` |
| 4 | 英语（英式） | `en-UK` | 20 | 土耳其语 | `tr` |
| 5 | 英语（澳式） | `en-AU` | 21 | 乌克兰语 | `uk` |
| 6 | 英语（印式） | `en-IN` | 22 | 波兰语 | `pl` |
| 7 | 日语 | `ja` | 23 | 荷兰语 | `nl` |
| 8 | 韩语 | `ko` | 24 | 希腊语 | `el` |
| 9 | 越南语 | `vi` | 25 | 罗马尼亚语 | `ro` |
| 10 | 印尼语 | `id` | 26 | 捷克语 | `cs` |
| 11 | 泰语 | `th` | 27 | 芬兰语 | `fi` |
| 12 | 印地语 | `hi` | 28 | 阿拉伯语 | `ar` |
| 13 | 西班牙语 | `es` | 29 | 瑞典语 | `sv` |
| 14 | 法语 | `fr` | 30 | 挪威语 | `no` |
| 15 | 德语 | `de` | 31 | 丹麦语 | `da` |
| 16 | 葡萄牙语（欧洲） | `pt-PT` | 32 | 南非荷兰语 | `af` |

---

## 技术支持

如有问题，请联系：

**天才团队**: 詹添天
**邮箱**: tiantian.zhan@yale.edu

---

**文档版本**: v2.1
**最后更新**: 2025-12
