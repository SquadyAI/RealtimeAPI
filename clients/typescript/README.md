# Realtime API — TypeScript Client

Web-based reference client for the Realtime API, built with React + TypeScript + Vite.

## Features

- **Binary Audio Protocol** — 20-30% lower latency vs JSON
- **WebRTC Audio** — Echo cancellation, noise suppression, AGC
- **VAD** — Client-side voice activity detection
- **Opus Codec** — Efficient audio encoding/decoding via WASM
- **PWA Support** — Installable, offline-ready
- **Vision** — Camera capture and image transmission

## Quick Start

```bash
# Install dependencies
npm install

# Copy and edit environment config
cp .env.example .env

# Start dev server
npm run dev
```

Open `http://localhost:5173` in your browser.

## Project Structure

```
src/
├── sdk/
│   ├── core/           # VoiceChatCore — session lifecycle
│   ├── connection/     # WebSocket connection manager
│   ├── audio/          # Recording, playback, Opus codec, AGC
│   ├── protocol/       # Binary & JSON message protocol
│   ├── config/         # Runtime configuration
│   └── utils/          # EventEmitter, logger
├── components/         # UI components (reminder, vision, PWA)
├── hooks/              # React hooks (camera, reminder)
├── App.tsx             # Main application
└── RealTime.ts         # SDK facade
```

## Binary Audio Protocol

The client supports a binary audio protocol for lower latency. Messages use a compact binary header instead of JSON encoding for audio frames.

Key files:
- `src/sdk/protocol/ClientProtocol.ts` — Protocol ID, command ID enums, message types
- `src/sdk/protocol/BinaryAudioDecoder.ts` — Server binary message decoding
- `src/sdk/protocol/BinaryAudioAdapter.ts` — Auto-detection and fallback

## Build

```bash
npm run build    # Production build → dist/
```
