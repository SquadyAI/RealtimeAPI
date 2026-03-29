# Architecture Overview

## System Architecture

```mermaid
graph TB
    subgraph Clients
        WEB[Web App<br/>realtime-ts-demo]
        SDK[Java SDK<br/>javasdk]
        PY[Python Client<br/>scripts/]
    end

    subgraph "Realtime API Server (Rust)"
        WS[WebSocket Server<br/>actix-web :8080]
        SM[Session Manager]
        PF[Pipeline Factory]
        SR[Session Router]

        subgraph "Pipeline Instances"
            P100[ModularPipeline<br/>ASR→LLM→TTS]
            P1[AsrOnlyPipeline]
            P2[LlmTtsPipeline]
            P3[TtsOnlyPipeline]
            P4[TranslationPipeline]
        end

        subgraph "Core Modules"
            VAD[VAD Engine<br/>Silero + SmartTurn]
            ASR[ASR Engine<br/>SenseVoice ONNX]
            LLM[LLM Client<br/>OpenAI-compatible]
            TTS[TTS Controller<br/>MiniMax / VolcEngine]
            PS[Paced Sender<br/>Jitter-free audio]
        end

        subgraph "Support"
            AG[Agents<br/>27 function-calling tools]
            MCP[MCP Client<br/>External tools]
            MON[Metrics<br/>Prometheus :8080/metrics]
            STORE[Storage<br/>PostgreSQL / InMemory]
        end
    end

    subgraph "External Services"
        LLM_API[LLM API<br/>OpenAI / DeepSeek / etc.]
        TTS_API[TTS API<br/>MiniMax / VolcEngine]
        SEARCH[Search API<br/>searchAPI service]
        GEO[Geocoding<br/>location / nominatim]
    end

    WEB & SDK & PY -->|WebSocket| WS
    WS --> SM --> PF
    PF --> P100 & P1 & P2 & P3 & P4
    P100 --> VAD & ASR & LLM & TTS & PS
    P100 --> AG & MCP
    SR -->|Route audio to client| WS
    LLM -->|HTTP/SSE| LLM_API
    TTS -->|WebSocket/HTTP| TTS_API
    AG -->|HTTP| SEARCH & GEO
```

## Data Flow (protocol_id=100)

The primary pipeline for voice conversations: audio in → intelligence → audio out.

```mermaid
sequenceDiagram
    participant C as Client
    participant WS as WebSocket
    participant VAD as VAD<br/>(Silero+SmartTurn)
    participant ASR as ASR<br/>(SenseVoice)
    participant LLM as LLM<br/>(OpenAI-compat)
    participant AG as Agents
    participant TTS as TTS<br/>(MiniMax)
    participant PS as PacedSender

    C->>WS: Connect + Start (session_config)
    WS-->>C: session.created

    loop Voice Turn
        C->>WS: Binary audio frames (640B/20ms)
        WS->>VAD: PCM samples (512/frame)

        Note over VAD: Silero: speech probability<br/>SmartTurn: turn-end detection

        VAD->>ASR: Speech segment (is_first → is_last)
        ASR-->>C: transcription.delta (streaming)
        ASR->>LLM: Final transcript

        Note over LLM: Streaming response<br/>+ function calling

        opt Tool Calls
            LLM->>AG: Function call (search, navigate, ...)
            AG-->>LLM: Tool result
        end

        LLM-->>C: response.text.delta (streaming)
        LLM->>TTS: Text chunks (sentence-split)
        TTS->>PS: Audio chunks
        PS-->>C: Binary audio (paced at realtime)
    end

    C->>WS: Stop
```

## Pipeline Selection

```mermaid
flowchart LR
    START([Client connects]) --> CMD{command_id?}

    CMD -->|Start| PID{protocol_id?}
    CMD -->|AudioChunk| ROUTE[Route to active pipeline]
    CMD -->|Interrupt| INT[Abort current response]
    CMD -->|Stop| CLOSE[Close session]

    PID -->|100 - All| FULL[ModularPipeline<br/>ASR→LLM→TTS<br/>Full voice conversation]
    PID -->|1 - ASR| ASRP[AsrOnlyPipeline<br/>Speech-to-text only]
    PID -->|2 - LLM| LLMP[LlmTtsPipeline<br/>Text→LLM→TTS]
    PID -->|3 - TTS| TTSP[TtsOnlyPipeline<br/>Text-to-speech only]
    PID -->|4 - Translation| TRANSP[TranslationPipeline<br/>Simultaneous interpretation]

    FULL --> MODE{speech_mode?}
    MODE -->|vad| VADM[VAD auto-detect<br/>+ SmartTurn filtering]
    MODE -->|vad_deferred| DEFM[VAD with deferred<br/>turn-end decision]
    MODE -->|ptt| PTTM[Push-to-talk<br/>manual control]
```

## Module Dependency Map

```mermaid
graph LR
    subgraph "rpc/"
        WS[actix_websocket] --> EH[event_handler]
        EH --> SM[session_manager]
        SM --> PF[pipeline_factory]
        PF --> PIPE[pipeline/]
    end

    subgraph "pipeline/"
        ORCH[orchestrator/] --> ASR_T[asr_task*]
        ORCH --> LLM_T[llm_task_v2]
        ORCH --> TTS_T[tts_task]
        TTS_T --> TTS_C[tts_controller/]
        TTS_T --> PSEND[paced_sender/]
    end

    subgraph "Core"
        ASR_T --> VAD_M[vad/]
        ASR_T --> ASR_M[asr/]
        LLM_T --> LLM_M[llm/]
        TTS_C --> TTS_M[tts/]
        LLM_T --> AGENTS[agents/]
        AGENTS --> MCP_M[mcp/]
    end

    ORCH --> TYPES[types.rs<br/>SharedFlags, TurnContext]
    PSEND --> AUDIO[audio/<br/>PCM, Opus, AGC]
```

## Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| **Silero + SmartTurn two-layer VAD** | Silero alone has false positives on pauses; SmartTurn's semantic model detects actual turn-ends |
| **PacedSender with Welford's variance** | Jitter-free audio delivery; sub-microsecond scheduling overhead |
| **Binary protocol for audio** | 10-30x faster than JSON+Base64; critical for 100+ concurrent sessions |
| **Pipeline factory pattern** | 6 pipeline types share the same WebSocket transport; protocol_id selects behavior |
| **Modular orchestrator** | orchestrator/ split into mod.rs + config.rs + state.rs; each <500 lines |
| **MiMalloc allocator** | Optimized for concurrent small allocations in async Rust |
