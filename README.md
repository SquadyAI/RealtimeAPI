<h1 align="center">Realtime API</h1>

<p align="center">
  <strong>Open-source real-time voice AI platform with modular ASR, LLM, and TTS pipelines.</strong><br>
  Self-hosted alternative to OpenAI Realtime API — sub-450ms latency, 100+ concurrent sessions.
</p>

<p align="center">
  <a href="https://github.com/SquadyAI/RealtimeAPI/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-Apache%202.0-blue.svg" alt="License"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/rust-nightly-orange.svg" alt="Rust"></a>
  <a href="https://github.com/SquadyAI/RealtimeAPI/pulls"><img src="https://img.shields.io/badge/PRs-welcome-brightgreen.svg" alt="PRs Welcome"></a>
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> &bull;
  <a href="#playground">Playground</a> &bull;
  <a href="docs/Realtime_API_Guide.md">API Docs</a> &bull;
  <a href="#architecture">Architecture</a> &bull;
  <a href="#contributing">Contributing</a>
</p>

<p align="center">
  <a href="./README.md">English</a> |
  <a href="./README_CN.md">简体中文</a>
</p>

---

## Why Realtime API?

OpenAI's Realtime API is powerful, but **expensive** ($0.06+/min), **not self-hostable**, and **vendor-locked**. We built this so you can run real-time voice AI on your own infrastructure, with any model you choose.

| | OpenAI Realtime | **This Project** |
|---|---|---|
| Deployment | Cloud only | **Self-hosted** |
| Data privacy | Third-party | **Fully private** |
| LLM | GPT-4o only | **Any OpenAI-compatible** |
| TTS | OpenAI only | **Edge / MiniMax / Azure / VolcEngine** |
| ASR | Whisper only | **WhisperLive** |
| Cost | Per-minute billing | **Fixed server cost** |
| Latency | ~500ms | **≤450ms** |

## Quick Start (macOS / Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/SquadyAI/RealtimeAPI/main/server/install.sh | bash
realtime onboard
```

The wizard walks you through LLM, TTS, and ASR configuration, creates `.env`, builds the binary, and starts the server. Open **http://localhost:8080** to try the built-in Playground.

<details>
<summary><strong>Prerequisites</strong></summary>

- An OpenAI-compatible LLM API key (OpenAI / DeepSeek / Qwen / Ollama)

</details>

<details>
<summary><strong>Install from source</strong></summary>

```bash
git clone https://github.com/SquadyAI/RealtimeAPI.git && cd RealtimeAPI/server
cp .env.example .env        # edit .env — set LLM_BASE_URL, LLM_MODEL at minimum
cargo build --release
realtime onboard
```

Requires [Rust nightly](https://rustup.rs/) and cmake.

See [server/.env.example](server/.env.example) for all configuration options.

</details>

<details>
<summary><strong>Windows</strong></summary>

The one-line installer requires bash (macOS / Linux). On Windows, install from source via WSL or natively:

**Option A — WSL (recommended):**
```bash
wsl --install          # if you don't have WSL yet
# then run the standard install command inside WSL
curl -fsSL https://raw.githubusercontent.com/SquadyAI/RealtimeAPI/main/server/install.sh | bash
```

**Option B — Native Windows:**
```powershell
git clone https://github.com/SquadyAI/RealtimeAPI.git
cd RealtimeAPI\server
copy .env.example .env
cargo build --release
.\target\release\realtime.exe
```

Edit `.env` before starting — set at minimum:
- `LLM_BASE_URL` — e.g. `https://api.groq.com/openai/v1` (free at [console.groq.com](https://console.groq.com))
- `LLM_API_KEY` — your API key
- `LLM_MODEL` — e.g. `llama-3.3-70b-versatile`
- `WHISPERLIVE_PATH` — your WhisperLive WebSocket URL

> The interactive `realtime onboard` wizard is bash-only (macOS / Linux / WSL). On native Windows, configure `.env` manually.

Requires [Rust](https://rustup.rs/), cmake, and Visual Studio Build Tools (C++ workload).

</details>

<details>
<summary><strong>Docker</strong></summary>

```bash
docker run -p 8080:8080 \
  -e LLM_BASE_URL=https://api.openai.com/v1 \
  -e LLM_API_KEY=sk-xxx \
  -e LLM_MODEL=gpt-4o-mini \
  ghcr.io/squadyai/realtime:latest
```

</details>

## Playground

**Try it online:** https://port2.luxhub.top:2097/ — no setup needed.

Or self-host: start the server and open **http://localhost:8080** — a fully functional voice conversation UI is built in.


## What You Can Build

- **Voice assistants** — Smart speakers, in-car assistants, customer service bots
- **Real-time translation** — Simultaneous interpretation across 25+ languages
- **Smart device control** — Voice-controlled IoT via built-in function-calling agents
- **AI tutoring** — Interactive language learning with real-time speech feedback
- **Accessibility tools** — Voice interfaces for applications

## Architecture

```
                          WebSocket (Opus audio)
              ┌────────────────────────────────────────┐
              │                                        │
              ▼                                        │
┌───────────────────┐         ┌────────────────────────┴───────────────────────────┐
│                   │         │                Realtime API Server                  │
│      Client       │         │                                                    │
│                   │  Opus   │  ┌───────┐   ┌─────┐   ┌─────┐   ┌─────┐          │
│   Microphone ─────┼────────▶│  │  VAD  │──▶│ ASR │──▶│ LLM │──▶│ TTS │──┐       │
│                   │         │  │Silero │   │Whis-│   │Open-│   │Edge/│  │       │
│                   │  Opus   │  │+Smart │   │per- │   │ AI  │   │Mini-│  │       │
│   Speaker ◀───────┼────────◀│  │ Turn  │   │Live │   │comp.│   │Max/ │  │       │
│                   │         │  └───────┘   └─────┘   └──┬──┘   │Azure│  │       │
│                   │         │                           │      └─────┘  │       │
└───────────────────┘         │                           ▼        ▲      │       │
                              │                    ┌────────────┐  │      │       │
                              │                    │   Agents   │  │  Paced      │
                              │                    │ + MCP Tools│──┘  Sender     │
                              │                    └────────────┘     ◀──┘       │
                              │                                                    │
                              └────────────────────────────────────────────────────┘
```

> Full architecture diagrams: [docs/architecture.md](server/docs/architecture.md)

### Pipeline Modes

| Protocol ID | Mode | Pipeline |
|---|---|---|
| `100` | Full conversation (default) | ASR → LLM → TTS |
| `1` | ASR only | Audio → Text |
| `2` | LLM only | Text → Text |
| `3` | TTS only | Text → Audio |
| `4` | Translation | Real-time interpretation |

## Supported Providers

| Category | Provider | Status | Notes |
|---|---|---|---|
| **ASR** | WhisperLive | Default | Streaming, multi-language |
| **LLM** | Any OpenAI-compatible | Default | GPT, DeepSeek, Qwen, Ollama, vLLM, etc. |
| **TTS** | Edge TTS | Default | Free, 100+ languages |
| **TTS** | MiniMax | Alternative | Chinese optimized, 50+ voices |
| **TTS** | Azure Speech | Alternative | High quality, multi-language |
| **TTS** | VolcEngine | Alternative | Chinese voices |
| **TTS** | Baidu TTS | Alternative | Chinese voices |
| **VAD** | Silero + SmartTurn | Default | Two-layer: acoustic (32ms) + semantic |
| **Tools** | MCP Protocol | Built-in | Dynamic tool extension |
| **Tools** | Function-calling Agents | Built-in | Search, translate, navigate, device control, etc. |

## Performance

| Metric | Value | Notes |
|---|---|---|
| End-to-end latency | ≤ 450ms | Dominated by AI model inference (ASR/LLM/TTS) |
| VAD inference | **82 us/frame** | 2.6x realtime headroom on 32ms frames |
| Text splitting | **73 us/turn** | Streaming sentence segmentation for TTS |
| Frame pacing | **<20 ns/frame** | Jitter-free audio delivery |
| Binary protocol | **68 ns/frame** | 10-30x faster than JSON for audio |
| Concurrent sessions | 100+ | Single node |
| Memory footprint | ~200MB | Base runtime |

The Rust pipeline (VAD → text splitting → protocol → pacing) adds **<0.5ms total** — less than 0.1% of end-to-end latency. The bottleneck is entirely in AI model inference, which is the correct design.

> Benchmark details: [docs/benchmarks.md](server/docs/benchmarks.md) | Run: `cd server && cargo bench`

**Production-grade features:**
- Timeout + fallback for external services
- Connection pooling for LLM/TTS
- Graceful shutdown with in-flight session draining
- Structured logging (tracing) + Prometheus metrics + Langfuse integration
- Hot-reload TTS voice parameters without restart

## WebSocket API

```javascript
const ws = new WebSocket('ws://localhost:8080/ws');

// 1. Configure session
ws.send(JSON.stringify({
  protocol_id: 100,
  command_id: 1,
  session_id: 'my-session',
  payload: {
    type: 'session_config',
    mode: 'vad',
    system_prompt: 'You are a helpful assistant.',
    voice_setting: { voice_id: 'zh_female_wanwanxiaohe_moon_bigtts' }
  }
}));

// 2. Send audio (binary: 32-byte header + PCM16 data)
ws.send(audioBuffer);

// 3. Receive responses
ws.onmessage = (event) => {
  if (typeof event.data === 'string') {
    const msg = JSON.parse(event.data);
    // ASR transcription, LLM text deltas, function calls...
  } else {
    // TTS audio chunks — play directly
  }
};
```

Full protocol reference: [Realtime_API_Guide.md](docs/Realtime_API_Guide.md)

## CLI Commands

```bash
realtime onboard     # Interactive setup wizard
realtime onboard       # Start the server (logs to logs/realtime.log)
realtime doctor      # Diagnose configuration and connectivity issues
```

## Configuration

All configuration is via environment variables. The setup wizard (`realtime onboard`) handles this interactively.

| Variable | Required | Description | Default |
|---|---|---|---|
| `LLM_BASE_URL` | Yes | OpenAI-compatible API endpoint | — |
| `LLM_MODEL` | Yes | Model name | — |
| `LLM_API_KEY` | No | API key (optional for self-hosted LLMs) | — |
| `ENABLE_TTS` | No | Enable voice synthesis | `true` |
| `TTS_ENGINE` | No | TTS engine (`edge`, `minimax`, `azure`, `volc`, `baidu`) | `edge` |
| `BIND_ADDR` | No | Listen address | `0.0.0.0:8080` |
| `VAD_THRESHOLD` | No | VAD sensitivity (0.0–1.0) | `0.6` |
| `MAX_CONCURRENT_SESSIONS` | No | Max concurrent sessions | `100` |

See [server/.env.example](server/.env.example) for all options.

## Project Structure

```
Realtime/
├── server/                 # Rust core server
│   ├── src/
│   │   ├── main.rs         # Entry point
│   │   ├── rpc/            # WebSocket server, session management, pipeline factory
│   │   │   └── pipeline/   # ASR→LLM→TTS orchestration, translation, etc.
│   │   ├── asr/            # WhisperLive streaming ASR
│   │   ├── llm/            # OpenAI-compatible client, function calling, history
│   │   ├── tts/            # Edge, MiniMax, Azure, VolcEngine, Baidu
│   │   ├── vad/            # Silero VAD + SmartTurn semantic detection
│   │   ├── agents/         # Function-calling agents
│   │   ├── mcp/            # Model Context Protocol client
│   │   ├── audio/          # PCM preprocessing, resampling, Opus codec
│   │   └── storage/        # PostgreSQL + in-memory fallback
│   └── Cargo.toml
└── clients/
    └── typescript/         # Web Playground (React)
```

## Documentation

| Document | Description |
|---|---|
| [API Guide](docs/Realtime_API_Guide.md) | WebSocket protocol reference |
| [Architecture](server/docs/architecture.md) | System design with Mermaid diagrams |
| [Benchmarks](server/docs/benchmarks.md) | Performance data with test conditions |
| [.env.example](server/.env.example) | Full configuration reference |

## Contributing

We welcome contributions! See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

```bash
# Development
cd server
cargo build              # Debug build
cargo test               # Run tests
realtime doctor          # Verify setup
```

## Roadmap

| Feature | Description | Status |
|---|---|---|
| **Long-term Memory** | Cross-session memory, user preference persistence | Planned |
| **Agent Collaboration** | Multi-agent orchestration and task delegation | Planned |
| **Multimodal** | Vision input (camera / screenshots) + voice, for GPT-4o class models | Planned |
| **Voice Cloning** | Few-shot voice cloning — talk in your own voice | Planned |
| **Speaker Identification** | Distinguish who is speaking in multi-person scenarios | Planned |
| **Voiceprint Authentication** | Speaker verification via voice biometrics | Planned |
| **Hosted ASR** | Zero-setup ASR — no self-hosting needed for new users | Planned |

Have ideas or want to contribute? [Open an issue](https://github.com/SquadyAI/RealtimeAPI/issues) or check [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[Apache License 2.0](LICENSE)
