# Benchmark Results

Baseline performance data for critical path components.

## Test Environment

| Item | Value |
|------|-------|
| CPU | Apple M3 Pro |
| RAM | 18 GB |
| OS | macOS 26.3.1 (Darwin 25.3.0) |
| Rust | 1.89.0 nightly |
| Framework | Criterion 0.7.0 |
| Profile | dev (unoptimized) — numbers reflect worst-case; release builds are faster |

## How to Run

```bash
cd server/

# Run all benchmarks
cargo bench

# Run a specific benchmark suite
cargo bench --bench vad_bench
cargo bench --bench text_splitter_bench
cargo bench --bench paced_sender_bench
cargo bench --bench protocol_bench
```

---

## 1. VAD (Voice Activity Detection)

Silero VAD ONNX inference on 512-sample frames (32ms @16kHz).

| Benchmark | Median | Description |
|-----------|--------|-------------|
| `process_chunk_silence_512` | **82 us** | Silent frame — model returns low probability |
| `process_chunk_speech_512` | **81 us** | Speech-like frame — model returns high probability |
| `reset` | **263 ns** | Reset VAD state between utterances |

**Key takeaway**: ~82 us per 32ms frame = **2.6x realtime** headroom on CPU. Each frame costs ~0.26% of its audio duration, leaving ample room for the rest of the pipeline.

---

## 2. Text Splitter

Streaming sentence segmentation for TTS input — the bridge between LLM output and TTS synthesis.

### Punctuation Detection

| Benchmark | Median | Description |
|-----------|--------|-------------|
| `is_sentence_terminal` (period) | <1 ns | Single char match: `.` |
| `is_sentence_terminal` (Chinese) | <1 ns | Single char match: `。` |
| `is_weak_break_point` (comma) | <1 ns | Single char match: `,` |
| `is_only_punctuation` (true) | **66 ns** | Full string scan, all punctuation |
| `is_only_punctuation` (false) | **4.4 ns** | Early exit on first alphanumeric |
| `find_last_sentence_terminal` (Chinese 80-char) | **2.2 ns** | Reverse scan for `。！？` |
| `find_last_sentence_terminal` (English 120-char) | **1.7 ns** | Reverse scan for `.!?` |
| `find_last_weak_break` (Chinese) | **6.8 ns** | Reverse scan for `，、；` |

### ASR Text Merge & Deduplication

| Benchmark | Median | Description |
|-----------|--------|-------------|
| `smart_text_merge` (no overlap) | **298 ns** | Two distinct strings |
| `smart_text_merge` (full containment) | **41 ns** | "Hello" + "Hello World" |
| `smart_text_merge` (partial overlap) | **381 ns** | Overlap detection + merge |
| `smart_text_merge` (Chinese no overlap) | **357 ns** | CJK character handling |
| `smart_text_merge` (Chinese overlap) | **464 ns** | CJK overlap detection |
| `remove_consecutive_duplicates` (no dups) | **182 ns** | Clean text passthrough |
| `remove_consecutive_duplicates` (word dups) | **151 ns** | "hello hello" dedup |
| `remove_consecutive_duplicates` (phrase dups) | **207 ns** | "hello world hello world" dedup |

### Streaming Splitter (End-to-End)

| Benchmark | Median | Description |
|-----------|--------|-------------|
| `chinese_streaming` (13 chunks) | **73 us** | Typical LLM output, 3 sentences |
| `english_streaming` (8 chunks) | **70 us** | English text, 2 sentences |
| `chinese_bulk` (single input) | **66 ns** | Pre-formed multi-sentence text |
| `mixed_language` (6 chunks) | **72 us** | Chinese + English mixed input |

**Key takeaway**: Streaming splitter processes a full turn's text in ~73 us — negligible compared to LLM token latency (~50-200ms).

---

## 3. Paced Sender

Audio frame scheduling precision — the component that ensures smooth playback without jitter.

### Pacing Calculations

| Benchmark | Median | Description |
|-----------|--------|-------------|
| `poll_interval_ms` (1x) | <1 ns | 20ms / 1.0 = 20ms |
| `poll_interval_ms` (1.5x) | <1 ns | 20ms / 1.5 = 13ms |
| `poll_interval_ms` (2x) | <1 ns | 20ms / 2.0 = 10ms |
| `audio_duration_us` (640B 16kHz mono) | <1 ns | 20ms frame duration calc |
| `audio_duration_us` (960B 24kHz mono) | <1 ns | 20ms frame at higher sample rate |
| `audio_duration_us` (64KB 16kHz stereo) | <1 ns | 1 second bulk audio |
| `send_delay_us` (burst phase) | <1 ns | Initial burst: 10ms delay |
| `send_delay_us` (steady state) | <1 ns | Normal: 20ms pacing |
| `send_delay_us` (steady 2x) | <1 ns | Accelerated: 10ms pacing |

### Timing Statistics (Welford's Online Variance)

| Benchmark | Median | Description |
|-----------|--------|-------------|
| `update_processing_delay` (single) | **14 ns** | First sample into empty window |
| `update_processing_delay` (steady state) | **14 ns** | Window full (10 samples), rolling |
| `update_cumulative_error` | **3.2 ns** | Timing error accumulation + clamp |
| `get_stability_report` | **143 ns** | Format stats string for logging |
| `full_cycle` | **15 ns** | Delay update + error update combined |

**Key takeaway**: All pacing operations are sub-microsecond. The scheduling loop adds <20ns overhead per frame — well within the 20ms inter-frame budget.

---

## 4. Protocol (WebSocket Serialization)

JSON and binary message encoding/decoding for the WebSocket transport layer.

### JSON Serialization

| Benchmark | Median | Description |
|-----------|--------|-------------|
| `session_config` serialize | **620 ns** | Config message (~6 fields) |
| `text_delta` serialize | **229 ns** | LLM text response event |
| `audio_chunk_640B` serialize | **470 ns** | 20ms audio frame (base64) |
| `audio_chunk_32KB` serialize | **13 us** | 1 second audio (base64) |
| `session_config` deserialize | **800 ns** | Config message parsing |
| `text_delta` deserialize | **850 ns** | Event message parsing |
| `audio_chunk_640B` deserialize | **632 ns** | 20ms audio parsing |
| `audio_chunk_32KB` deserialize | **4.5 us** | 1 second audio parsing |
| `session_config_safe` deserialize | **785 ns** | With fallback recovery |

### Binary Protocol

| Benchmark | Median | Description |
|-----------|--------|-------------|
| `header to_bytes` | **19 ns** | Encode 32-byte header |
| `header from_bytes` | **20 ns** | Decode 32-byte header |
| `message to_bytes` (640B) | **68 ns** | Header + 20ms audio frame |
| `message from_bytes` (640B) | **51 ns** | Parse header + audio frame |
| `message to_bytes` (32KB) | **397 ns** | Header + 1 second audio |
| `message from_bytes` (32KB) | **374 ns** | Parse header + 1 second audio |

### JSON-to-Binary Conversion

| Benchmark | Median | Description |
|-----------|--------|-------------|
| `json_to_binary` (640B) | **222 ns** | Base64 decode + binary pack |
| `json_to_binary` (32KB) | **9.0 us** | Large audio conversion |

**Key takeaway**: Binary protocol is **10-30x faster** than JSON for audio frames. A 20ms audio frame encodes in 68ns (binary) vs 470ns (JSON). For high-throughput audio streaming, the binary path is critical.

---

## Summary: Latency Budget

For a single voice turn (Client audio → VAD → ASR → LLM → TTS → Client audio):

| Component | Latency | Budget Share |
|-----------|---------|--------------|
| VAD frame processing | ~82 us/frame | <0.1% |
| Text splitting (full turn) | ~73 us | <0.1% |
| Protocol serialization | ~0.5 us/msg | <0.1% |
| Pacing overhead | ~15 ns/frame | ~0% |
| **ASR inference** | ~100-300 ms | **~30%** |
| **LLM generation** | ~200-500 ms | **~50%** |
| **TTS synthesis** | ~50-200 ms | **~20%** |
| **Network RTT** | ~10-50 ms | **~5%** |

The Rust pipeline components (VAD, text splitting, protocol, pacing) together consume <0.5ms — **<0.1% of the end-to-end latency**. The bottleneck is entirely in the AI model inference (ASR/LLM/TTS), which is the expected and correct design.
