# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.2] - 2026-03-30

### Fixed
- ort-wasm files now copied to playground root via `stripBase: true`, fixing 404 on `/ort-wasm-simd-threaded.jsep.mjs`
- Added `mjs` to workbox `globPatterns` so all WASM/MJS runtime files are precached by the service worker
- Removed duplicate ort-wasm copies from `assets/` and `node_modules/` in playground

## [1.0.1] - 2026-03-30

### Fixed
- PWA service worker no longer uses `clientsClaim()` on activation, preventing "Unsafe attempt to load URL from chrome-error://" errors after quickstart
- Playground URL is now printed only after the server is confirmed running

### Added
- Initial open source release
- Real-time voice conversation support (ASR → LLM → TTS pipeline)
- WebSocket API for bidirectional streaming
- Support for multiple LLM providers (OpenAI compatible)
- Support for multiple TTS providers (VolcEngine, MiniMax)
- SenseVoice-based ASR with multi-language support
- Voice Activity Detection (VAD) with semantic understanding
- Function calling and MCP protocol support
- Docker deployment support (CPU and GPU variants)

### Documentation
- Quick start guide
- API documentation
- Architecture overview
- Configuration guides

## [1.0.0] - 2026-02-03

### Added
- First public release
