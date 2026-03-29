#!/usr/bin/env python3
"""
WebSocket Load Test for Realtime Voice API

Spawns N concurrent WebSocket sessions, each sending audio and measuring
end-to-end latency. Reports P50/P95/P99, success rate, and error breakdown.

Usage:
    # Basic: 10 concurrent connections, 60 seconds
    python scripts/load_test.py --connections 10 --duration 60

    # With real audio file
    python scripts/load_test.py --connections 20 --duration 120 --audio-file test.wav

    # Custom endpoint
    python scripts/load_test.py --connections 5 --url ws://10.0.0.1:8080/ws

    # Dry-run: just test connectivity
    python scripts/load_test.py --connections 1 --duration 10 --no-audio

Requirements:
    pip install websockets
"""

import argparse
import asyncio
import json
import math
import random
import string
import struct
import sys
import time
import wave
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

try:
    import websockets
except ImportError:
    print("Error: websockets not installed. Run: pip install websockets")
    sys.exit(1)

# ============== Protocol Constants ==============

PROTOCOL_ALL = 100
PROTOCOL_ASR = 1
CMD_START = 1
CMD_STOP = 2
CMD_AUDIO_CHUNK = 3
CMD_STOP_INPUT = 5
HEADER_SIZE = 32

SAMPLE_RATE = 16000
CHANNELS = 1
SAMPLE_WIDTH = 2  # 16-bit PCM
CHUNK_MS = 20
CHUNK_BYTES = SAMPLE_RATE * CHANNELS * SAMPLE_WIDTH * CHUNK_MS // 1000  # 640


# ============== Protocol Helpers ==============


def generate_session_id() -> str:
    chars = string.ascii_lowercase + string.digits
    return "".join(random.choice(chars) for _ in range(16))


def build_binary_header(session_id: str, protocol_id: int, command_id: int) -> bytes:
    header = bytearray(HEADER_SIZE)
    sid = session_id.encode("utf-8")[:16].ljust(16, b"\x00")
    header[0:16] = sid
    header[16] = protocol_id
    header[17] = command_id
    return bytes(header)


def generate_synthetic_audio(duration_ms: int = 2000) -> bytes:
    """Generate synthetic 16kHz mono PCM silence with light noise for VAD triggering."""
    num_samples = SAMPLE_RATE * duration_ms // 1000
    # Low-amplitude noise so VAD doesn't hang waiting for speech
    samples = bytearray(num_samples * SAMPLE_WIDTH)
    for i in range(0, len(samples), 2):
        val = random.randint(-50, 50)
        struct.pack_into("<h", samples, i, val)
    return bytes(samples)


def load_audio_file(path: str) -> bytes:
    """Load a WAV or raw PCM file."""
    if path.endswith(".wav"):
        with wave.open(path, "rb") as wf:
            return wf.readframes(wf.getnframes())
    else:
        return Path(path).read_bytes()


# ============== Session Metrics ==============


@dataclass
class SessionResult:
    session_id: str
    success: bool = False
    error: Optional[str] = None
    connect_time_ms: float = 0.0
    session_created_time_ms: float = 0.0
    first_response_time_ms: float = 0.0
    full_response_time_ms: float = 0.0
    audio_chunks_sent: int = 0
    events_received: int = 0
    audio_bytes_received: int = 0
    transcription: str = ""
    response_text: str = ""


# ============== Single Session ==============


async def run_session(
    url: str,
    audio_data: bytes,
    session_timeout: float,
    no_audio: bool = False,
) -> SessionResult:
    """Run a single load test session: connect → start → send audio → receive → stop."""

    session_id = generate_session_id()
    result = SessionResult(session_id=session_id)
    t_start = time.monotonic()

    try:
        # 1. Connect
        ws = await asyncio.wait_for(
            websockets.connect(url, max_size=2**22, close_timeout=5),
            timeout=10,
        )
        result.connect_time_ms = (time.monotonic() - t_start) * 1000

        try:
            # 2. Send Start
            start_msg = json.dumps(
                {
                    "protocol_id": PROTOCOL_ALL,
                    "command_id": CMD_START,
                    "session_id": session_id,
                    "payload": {
                        "type": "session_config",
                        "mode": "vad",
                        "system_prompt": "Reply with one short sentence.",
                        "vad_threshold": 0.5,
                        "silence_duration_ms": 200,
                        "enable_search": False,
                    },
                }
            )
            await ws.send(start_msg)

            # Wait for session.created
            t_wait = time.monotonic()
            while time.monotonic() - t_wait < 10:
                msg = await asyncio.wait_for(ws.recv(), timeout=10)
                if isinstance(msg, str):
                    data = json.loads(msg)
                    payload = data.get("payload", {})
                    if isinstance(payload, dict) and payload.get("type") == "session.created":
                        result.session_created_time_ms = (time.monotonic() - t_start) * 1000
                        break

            if result.session_created_time_ms == 0:
                result.error = "session.created not received"
                return result

            # 3. Send audio chunks (binary format, paced at realtime)
            if not no_audio:
                chunks_sent = 0
                for i in range(0, len(audio_data), CHUNK_BYTES):
                    chunk = audio_data[i : i + CHUNK_BYTES]
                    if len(chunk) < CHUNK_BYTES:
                        chunk = chunk + b"\x00" * (CHUNK_BYTES - len(chunk))
                    header = build_binary_header(session_id, PROTOCOL_ASR, CMD_AUDIO_CHUNK)
                    await ws.send(header + chunk)
                    chunks_sent += 1
                    await asyncio.sleep(CHUNK_MS / 1000.0)
                result.audio_chunks_sent = chunks_sent

                # Send StopInput
                stop_input = json.dumps(
                    {
                        "protocol_id": PROTOCOL_ALL,
                        "command_id": CMD_STOP_INPUT,
                        "session_id": session_id,
                    }
                )
                await ws.send(stop_input)

            # 4. Receive responses
            t_recv_start = time.monotonic()
            got_first_response = False
            done = False

            while not done and (time.monotonic() - t_recv_start) < session_timeout:
                try:
                    msg = await asyncio.wait_for(ws.recv(), timeout=session_timeout)
                    result.events_received += 1

                    if isinstance(msg, bytes):
                        result.audio_bytes_received += len(msg)
                        if not got_first_response:
                            result.first_response_time_ms = (time.monotonic() - t_start) * 1000
                            got_first_response = True
                    elif isinstance(msg, str):
                        data = json.loads(msg)
                        payload = data.get("payload", {})
                        if not isinstance(payload, dict):
                            continue
                        msg_type = payload.get("type", "")

                        if "text.delta" in msg_type:
                            if not got_first_response:
                                result.first_response_time_ms = (time.monotonic() - t_start) * 1000
                                got_first_response = True
                            result.response_text += payload.get("delta", "")

                        elif msg_type == "conversation.item.input_audio_transcription.completed":
                            result.transcription = payload.get("transcript", "")

                        elif msg_type in (
                            "output_audio_buffer.stopped",
                            "response.done",
                        ):
                            done = True

                        elif msg_type == "error":
                            err = payload.get("error", {})
                            result.error = err.get("message", str(payload))
                            done = True

                except asyncio.TimeoutError:
                    if no_audio:
                        done = True  # no-audio mode: just tested connectivity
                    else:
                        result.error = "response timeout"
                        done = True

            result.full_response_time_ms = (time.monotonic() - t_start) * 1000

            # 5. Send Stop
            try:
                stop_msg = json.dumps(
                    {
                        "protocol_id": PROTOCOL_ALL,
                        "command_id": CMD_STOP,
                        "session_id": session_id,
                    }
                )
                await ws.send(stop_msg)
            except Exception:
                pass

            if result.error is None:
                result.success = True

        finally:
            await ws.close()

    except asyncio.TimeoutError:
        result.error = "connection timeout"
        result.full_response_time_ms = (time.monotonic() - t_start) * 1000
    except ConnectionRefusedError:
        result.error = "connection refused"
        result.full_response_time_ms = (time.monotonic() - t_start) * 1000
    except Exception as e:
        result.error = f"{type(e).__name__}: {e}"
        result.full_response_time_ms = (time.monotonic() - t_start) * 1000

    return result


# ============== Load Test Orchestrator ==============


def percentile(data: list[float], p: float) -> float:
    if not data:
        return 0.0
    k = (len(data) - 1) * (p / 100.0)
    f = math.floor(k)
    c = math.ceil(k)
    if f == c:
        return data[int(k)]
    return data[f] * (c - k) + data[c] * (k - f)


def print_report(results: list[SessionResult], duration: float, connections: int):
    total = len(results)
    successes = [r for r in results if r.success]
    failures = [r for r in results if not r.success]

    print("\n" + "=" * 70)
    print("LOAD TEST REPORT")
    print("=" * 70)

    print(f"\nConnections:     {connections} concurrent")
    print(f"Duration:        {duration:.1f}s")
    print(f"Total sessions:  {total}")
    print(f"Success:         {len(successes)} ({len(successes) * 100 // max(total, 1)}%)")
    print(f"Failed:          {len(failures)} ({len(failures) * 100 // max(total, 1)}%)")

    if successes:
        connect_times = sorted(r.connect_time_ms for r in successes)
        first_resp_times = sorted(r.first_response_time_ms for r in successes if r.first_response_time_ms > 0)
        full_resp_times = sorted(r.full_response_time_ms for r in successes)
        events = [r.events_received for r in successes]
        audio_out = [r.audio_bytes_received for r in successes]

        print("\n--- Latency (ms) ---")
        print(f"{'Metric':<30} {'P50':>10} {'P95':>10} {'P99':>10} {'Max':>10}")
        print("-" * 70)

        for name, data in [
            ("Connect", connect_times),
            ("First Response", first_resp_times),
            ("Full Response", full_resp_times),
        ]:
            if data:
                print(
                    f"{name:<30} {percentile(data, 50):>10.1f} {percentile(data, 95):>10.1f} "
                    f"{percentile(data, 99):>10.1f} {data[-1]:>10.1f}"
                )

        print(f"\n--- Throughput ---")
        print(f"Avg events/session:       {sum(events) / len(events):.1f}")
        print(f"Avg audio out/session:    {sum(audio_out) / len(audio_out) / 1024:.1f} KB")
        total_chunks = sum(r.audio_chunks_sent for r in successes)
        print(f"Total audio chunks sent:  {total_chunks}")

    if failures:
        print(f"\n--- Errors ---")
        error_counts: dict[str, int] = {}
        for r in failures:
            key = r.error or "unknown"
            # Truncate long errors
            if len(key) > 80:
                key = key[:77] + "..."
            error_counts[key] = error_counts.get(key, 0) + 1
        for err, count in sorted(error_counts.items(), key=lambda x: -x[1]):
            print(f"  {count:>4}x  {err}")

    print()


async def run_load_test(
    url: str,
    connections: int,
    duration: float,
    audio_data: bytes,
    no_audio: bool,
    session_timeout: float,
):
    """Run the load test: spawn sessions continuously for `duration` seconds."""

    results: list[SessionResult] = []
    semaphore = asyncio.Semaphore(connections)
    stop_event = asyncio.Event()

    async def bounded_session():
        async with semaphore:
            if stop_event.is_set():
                return None
            r = await run_session(url, audio_data, session_timeout, no_audio)
            results.append(r)
            # Print progress dot
            symbol = "." if r.success else "x"
            print(symbol, end="", flush=True)
            return r

    print(f"Starting load test: {connections} concurrent connections, {duration}s duration")
    print(f"Target: {url}")
    print(f"Audio: {'disabled' if no_audio else f'{len(audio_data)} bytes ({len(audio_data) * 1000 // (SAMPLE_RATE * SAMPLE_WIDTH)}ms)'}")
    print(f"\nProgress (. = success, x = fail):")

    t_start = time.monotonic()
    tasks: list[asyncio.Task] = []

    # Spawn sessions continuously until duration expires
    while (time.monotonic() - t_start) < duration:
        if len(tasks) - len(results) < connections:
            task = asyncio.create_task(bounded_session())
            tasks.append(task)
        else:
            # All slots full, wait a bit
            await asyncio.sleep(0.1)

        # Clean up completed tasks periodically
        tasks = [t for t in tasks if not t.done()]

    # Signal stop and wait for in-flight sessions
    stop_event.set()
    if tasks:
        print(f"\n\nWaiting for {len(tasks)} in-flight sessions...")
        await asyncio.gather(*tasks, return_exceptions=True)

    elapsed = time.monotonic() - t_start
    print_report(results, elapsed, connections)


# ============== CLI ==============


def main():
    parser = argparse.ArgumentParser(
        description="WebSocket load test for Realtime Voice API",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  %(prog)s --connections 10 --duration 60
  %(prog)s --connections 20 --duration 120 --audio-file test.wav
  %(prog)s --connections 5 --url ws://10.0.0.1:8080/ws
  %(prog)s --connections 1 --duration 10 --no-audio
        """,
    )
    parser.add_argument(
        "-c", "--connections", type=int, default=10, help="Number of concurrent connections (default: 10)"
    )
    parser.add_argument(
        "-d", "--duration", type=float, default=60, help="Test duration in seconds (default: 60)"
    )
    parser.add_argument(
        "-a", "--audio-file", type=str, default=None, help="WAV or raw PCM file to send (default: synthetic noise)"
    )
    parser.add_argument(
        "--audio-duration-ms",
        type=int,
        default=2000,
        help="Duration of synthetic audio in ms (default: 2000)",
    )
    parser.add_argument(
        "--url",
        type=str,
        default="ws://localhost:8080/ws",
        help="WebSocket endpoint URL (default: ws://localhost:8080/ws)",
    )
    parser.add_argument(
        "--no-audio", action="store_true", help="Skip audio sending, just test connectivity"
    )
    parser.add_argument(
        "--session-timeout",
        type=float,
        default=30,
        help="Per-session response timeout in seconds (default: 30)",
    )

    args = parser.parse_args()

    # Load or generate audio
    if args.audio_file:
        audio_data = load_audio_file(args.audio_file)
        print(f"Loaded audio: {args.audio_file} ({len(audio_data)} bytes)")
    else:
        audio_data = generate_synthetic_audio(args.audio_duration_ms)
        print(f"Generated synthetic audio: {args.audio_duration_ms}ms ({len(audio_data)} bytes)")

    asyncio.run(
        run_load_test(
            url=args.url,
            connections=args.connections,
            duration=args.duration,
            audio_data=audio_data,
            no_audio=args.no_audio,
            session_timeout=args.session_timeout,
        )
    )


if __name__ == "__main__":
    main()
