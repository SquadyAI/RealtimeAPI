#!/usr/bin/env python3
"""
Realtime WebSocket API 客户端
完整语音对话模式 (protocol_id=100)

使用方法:
    python realtime_client.py input.wav output.pcm
    python realtime_client.py input.pcm output.pcm --raw
"""

import asyncio
import json
import struct
import wave
import argparse
import time
from pathlib import Path
from dataclasses import dataclass
from typing import Optional

import websockets


# ============== 常量定义 ==============

# 服务端点
WS_ENDPOINT = "ws://localhost:8080/ws"

# Protocol IDs
PROTOCOL_ASR = 1
PROTOCOL_LLM = 2
PROTOCOL_TTS = 3
PROTOCOL_TRANSLATION = 4
PROTOCOL_ALL = 100

# Command IDs
CMD_START = 1
CMD_STOP = 2
CMD_AUDIO_CHUNK = 3
CMD_TEXT_DATA = 4
CMD_STOP_INPUT = 5
CMD_IMAGE_DATA = 6
CMD_RESPONSE_AUDIO_DELTA = 20
CMD_RESULT = 100
CMD_ERROR = 255

# 输入音频参数 (发送给 ASR)
SAMPLE_RATE = 16000
CHANNELS = 1
SAMPLE_WIDTH = 2  # 16-bit

# 输出音频参数 (TTS 返回 16kHz)
OUTPUT_SAMPLE_RATE = 16000

# 二进制消息头大小
HEADER_SIZE = 32

# 音频发送间隔 (毫秒)
AUDIO_CHUNK_MS = 20
AUDIO_CHUNK_SAMPLES = SAMPLE_RATE * AUDIO_CHUNK_MS // 1000  # 320 samples
AUDIO_CHUNK_BYTES = AUDIO_CHUNK_SAMPLES * SAMPLE_WIDTH  # 640 bytes


# ============== 数据类 ==============

@dataclass
class SessionConfig:
    """会话配置"""
    mode: str = "vad_deferred"  # vad / vad_deferred / ptt
    system_prompt: str = "你是一个友好的AI助手，用简短的语言回答问题"
    voice_id: str = "zh_female_wanwanxiaohe_moon_bigtts"
    vad_threshold: float = 0.75
    silence_duration_ms: int = 300
    enable_search: bool = True
    asr_language: Optional[str] = "zh-CN"  # ASR 语言
    # 同声传译配置
    from_language: Optional[str] = None  # 源语言，如 "en"
    to_language: Optional[str] = None    # 目标语言，如 "zh"
    # 工具配置
    prompt_endpoint: Optional[str] = None  # 工具定义端点
    # 音频发送配置
    initial_burst_delay_ms: int = 10
    initial_burst_count: int = 10
    send_rate_multiplier: float = 1.2
    # 输入音频配置
    input_audio_format: str = "opus"  # opus / pcm
    input_audio_sample_rate: int = 16000
    # 语音设置扩展
    voice_speed: float = 1.0
    voice_pitch: int = 0
    voice_vol: float = 1.0
    # 工具配置
    tools: Optional[list] = None  # 工具定义列表
    offline_tools: Optional[list] = None  # 离线工具列表
    tool_choice: str = "auto"  # auto / none / required
    # MCP 服务配置
    mcp_server_config: Optional[list] = None
    # 其他配置
    emoji_prompt: Optional[str] = None
    location: str = ""
    # 繁简转换: "none" | "t2s" (繁→简) | "s2t" (简→繁)
    chinese_convert: Optional[str] = None


@dataclass
class AudioDelta:
    """解析后的音频数据"""
    response_id: str
    item_id: str
    output_index: int
    content_index: int
    audio_data: bytes


# ============== 工具函数 ==============

def generate_session_id() -> str:
    """生成会话 ID (固定16字符)"""
    import random
    import string
    # 固定 16 字符: 字母+数字
    chars = string.ascii_lowercase + string.digits
    return ''.join(random.choice(chars) for _ in range(16))


def pad_session_id(session_id: str) -> bytes:
    """将 session_id 填充到 16 字节"""
    session_bytes = session_id.encode('utf-8')
    if len(session_bytes) > 16:
        return session_bytes[:16]
    return session_bytes.ljust(16, b'\x00')


def build_binary_header(session_id: str, protocol_id: int, command_id: int) -> bytes:
    """
    构建 32 字节二进制消息头
    格式: session_id(16) + protocol_id(1) + command_id(1) + reserved(14)
    """
    header = bytearray(HEADER_SIZE)

    # session_id (16 bytes)
    session_bytes = pad_session_id(session_id)
    header[0:16] = session_bytes

    # protocol_id (1 byte)
    header[16] = protocol_id

    # command_id (1 byte)
    header[17] = command_id

    # reserved (14 bytes) - 保持为 0

    return bytes(header)


def parse_audio_delta(binary_data: bytes) -> AudioDelta:
    """
    解析服务器返回的音频二进制数据
    格式:
        32字节头
        response_id_len (4字节, 小端) + response_id_bytes
        item_id_len (4字节, 小端) + item_id_bytes
        output_index (4字节, 小端)
        content_index (4字节, 小端)
        audio_data
    """
    offset = HEADER_SIZE  # 跳过 32 字节头

    # 解析 response_id
    response_id_len = struct.unpack_from('<I', binary_data, offset)[0]
    offset += 4
    response_id = binary_data[offset:offset + response_id_len].decode('utf-8')
    offset += response_id_len

    # 解析 item_id
    item_id_len = struct.unpack_from('<I', binary_data, offset)[0]
    offset += 4
    item_id = binary_data[offset:offset + item_id_len].decode('utf-8')
    offset += item_id_len

    # 解析索引
    output_index = struct.unpack_from('<I', binary_data, offset)[0]
    offset += 4
    content_index = struct.unpack_from('<I', binary_data, offset)[0]
    offset += 4

    # 剩余部分是音频数据
    audio_data = binary_data[offset:]

    return AudioDelta(
        response_id=response_id,
        item_id=item_id,
        output_index=output_index,
        content_index=content_index,
        audio_data=audio_data
    )


def load_audio_file(file_path: str, is_raw: bool = False) -> bytes:
    """
    加载音频文件，返回 PCM 数据

    Args:
        file_path: 音频文件路径
        is_raw: 是否为原始 PCM 文件

    Returns:
        PCM S16LE 数据
    """
    if is_raw:
        # 直接读取 PCM 文件
        with open(file_path, 'rb') as f:
            return f.read()
    else:
        # 读取 WAV 文件
        with wave.open(file_path, 'rb') as wf:
            # 检查格式
            if wf.getnchannels() != CHANNELS:
                raise ValueError(f"音频必须是单声道，当前: {wf.getnchannels()} 声道")
            if wf.getsampwidth() != SAMPLE_WIDTH:
                raise ValueError(f"音频必须是 16-bit，当前: {wf.getsampwidth() * 8}-bit")
            if wf.getframerate() != SAMPLE_RATE:
                print(f"警告: 音频采样率为 {wf.getframerate()}Hz，建议使用 {SAMPLE_RATE}Hz")

            return wf.readframes(wf.getnframes())


# ============== 客户端类 ==============

class RealtimeClient:
    """Realtime WebSocket API 客户端"""

    def __init__(self, endpoint: str = WS_ENDPOINT):
        self.endpoint = endpoint
        self.session_id: Optional[str] = None
        self.ws: Optional[websockets.WebSocketClientProtocol] = None
        self.received_audio: bytearray = bytearray()
        self.is_session_created = False
        self.is_response_done = False
        self.transcription_text = ""
        self.response_text = ""
        self.asr_only_mode = False  # ASR-only 模式标志

    async def connect(self):
        """建立 WebSocket 连接"""
        print(f"正在连接: {self.endpoint}")
        self.ws = await websockets.connect(self.endpoint)
        print("✅ WebSocket 连接成功")

    async def close(self):
        """关闭连接"""
        if self.ws:
            await self.ws.close()
            print("连接已关闭")

    async def send_json(self, data: dict):
        """发送 JSON 消息"""
        message = json.dumps(data, ensure_ascii=False)
        await self.ws.send(message)
        payload = data.get('payload')
        msg_type = payload.get('type', 'unknown') if payload else f"cmd_{data.get('command_id')}"
        print(f">>> 发送: {msg_type}")

    async def send_binary(self, data: bytes):
        """发送二进制消息"""
        await self.ws.send(data)

    async def start_session(self, config: SessionConfig):
        """创建会话"""
        self.session_id = generate_session_id()
        print(f"会话 ID: {self.session_id}")

        payload = {
            "type": "session_config",
            "mode": config.mode,
            "system_prompt": config.system_prompt,
            "vad_threshold": config.vad_threshold,
            "enable_search": config.enable_search,
            "location": config.location,
            # 音频发送配置
            "initial_burst_delay_ms": config.initial_burst_delay_ms,
            "initial_burst_count": config.initial_burst_count,
            "send_rate_multiplier": config.send_rate_multiplier,
            # 输入音频配置
            "input_audio_config": {
                "format": config.input_audio_format,
                "sample_rate": config.input_audio_sample_rate
            },
            # 语音设置
            "voice_setting": {
                "voice_id": config.voice_id,
                "speed": config.voice_speed,
                "pitch": config.voice_pitch,
                "vol": config.voice_vol
            },
            # 工具配置
            "tool_choice": config.tool_choice
        }

        if config.asr_language:
            payload["asr_language"] = config.asr_language

        if config.prompt_endpoint:
            payload["prompt_endpoint"] = config.prompt_endpoint

        if config.tools is not None:
            payload["tools"] = config.tools

        if config.offline_tools is not None:
            payload["offline_tools"] = config.offline_tools

        if config.mcp_server_config is not None:
            payload["mcp_server_config"] = config.mcp_server_config

        # emoji_prompt 可以为 None
        payload["emoji_prompt"] = config.emoji_prompt

        # 繁简转换
        if config.chinese_convert:
            payload["chinese_convert"] = config.chinese_convert

        await self.send_json({
            "protocol_id": PROTOCOL_ALL,
            "command_id": CMD_START,
            "session_id": self.session_id,
            "payload": payload
        })

    async def start_translation_session(self, config: SessionConfig):
        """创建同声传译会话 (protocol_id=4)"""
        self.session_id = generate_session_id()
        print(f"会话 ID: {self.session_id}")
        print(f"翻译模式: {config.from_language} → {config.to_language}")

        payload = {
            "type": "session_config",
            "from_language": config.from_language,
            "to_language": config.to_language,
            "mode": config.mode,
            "vad_threshold": config.vad_threshold,
            "silence_duration_ms": config.silence_duration_ms,
            "voice_setting": {
                "voice_id": config.voice_id
            }
        }

        await self.send_json({
            "protocol_id": PROTOCOL_TRANSLATION,
            "command_id": CMD_START,
            "session_id": self.session_id,
            "payload": payload
        })

    async def stop_input(self):
        """停止输入，触发 AI 回复"""
        await self.send_json({
            "protocol_id": PROTOCOL_ALL,
            "command_id": CMD_STOP_INPUT,
            "session_id": self.session_id,
            "payload": None
        })
        print(">>> 发送 StopInput，等待 AI 回复...")

    async def stop_session(self):
        """结束会话"""
        await self.send_json({
            "protocol_id": PROTOCOL_ALL,
            "command_id": CMD_STOP,
            "session_id": self.session_id,
            "payload": None
        })
        print(">>> 发送 Stop，结束会话")

    async def send_audio_chunk(self, audio_data: bytes):
        """
        发送音频数据块（二进制格式）
        """
        header = build_binary_header(self.session_id, PROTOCOL_ASR, CMD_AUDIO_CHUNK)
        message = header + audio_data
        await self.send_binary(message)

    async def send_audio_file(self, audio_data: bytes):
        """
        分块发送音频数据
        每 20ms 发送一次 (640 bytes @ 16kHz 16-bit mono)
        """
        total_chunks = len(audio_data) // AUDIO_CHUNK_BYTES
        print(f"音频大小: {len(audio_data)} bytes, 分 {total_chunks} 块发送")

        for i in range(0, len(audio_data), AUDIO_CHUNK_BYTES):
            chunk = audio_data[i:i + AUDIO_CHUNK_BYTES]
            await self.send_audio_chunk(chunk)
            # 模拟实时发送
            await asyncio.sleep(AUDIO_CHUNK_MS / 1000)

        print(f"✅ 音频发送完成，共 {total_chunks} 块")

    def handle_json_message(self, data: dict):
        """处理 JSON 消息"""
        payload = data.get("payload", {})
        msg_type = payload.get("type", "")

        if msg_type == "session.created":
            # 注意：服务器可能返回多次 session.created，只处理第一次
            if not self.is_session_created:
                self.is_session_created = True
                print("✅ 会话创建成功")

        elif msg_type == "input_audio_buffer.speech_started":
            print("🎤 检测到说话开始")

        elif msg_type == "input_audio_buffer.speech_stopped":
            print("🎤 检测到说话结束")

        elif msg_type == "conversation.item.input_audio_transcription.completed":
            transcript = payload.get("transcript", "")
            self.transcription_text = transcript
            print(f"📝 ASR 识别结果: {transcript}")
            # ASR-only 模式：收到识别结果即结束
            if self.asr_only_mode:
                self.is_response_done = True

        elif msg_type == "conversation.item.input_audio_transcription.delta":
            delta = payload.get("delta", "")
            print(f"📝 ASR 中间结果: {delta}")

        elif msg_type == "response.created":
            print("🤖 AI 开始生成回复")

        elif msg_type == "response.text.delta":
            delta = payload.get("delta", "")
            self.response_text += delta
            print(f"💬 {delta}", end="", flush=True)

        elif msg_type == "response.text.done":
            print()  # 换行
            print(f"💬 AI 文本完成: {self.response_text}")

        elif msg_type == "response.audio.done":
            # 如果没有 output_audio_buffer.stopped，用这个作为备用结束条件
            if not self.is_response_done:
                self.is_response_done = True
                print("✅ 音频发送完成，本轮对话结束")

        elif msg_type == "response.done":
            # response.done 表示本轮响应结束
            # 继续等待音频完成 (output_audio_buffer.stopped)
            print("✅ response.done，等待音频...")

        elif msg_type == "output_audio_buffer.stopped":
            # 只有在收到过响应内容后才算结束
            if self.response_text or self.received_audio:
                self.is_response_done = True
                print("✅ 音频播放完成，本轮对话结束")
            else:
                print("⚠️ 收到 output_audio_buffer.stopped (忽略，未收到响应)")

        elif msg_type == "output_audio_buffer.started":
            print("🔊 开始播放音频")

        elif msg_type == "output_audio_buffer.cleared":
            print("🔊 音频缓冲已清空")

        elif msg_type == "error" or msg_type == "error.event":
            # 兼容两种错误格式
            if "error" in payload:
                error = payload.get("error", {})
                print(f"❌ 错误: {error.get('message', 'Unknown error')}")
            else:
                # error.event 格式: {code, message}
                print(f"❌ 错误 [{payload.get('code')}]: {payload.get('message', 'Unknown error')}")
            self.is_response_done = True  # 标记结束，不再等待

        else:
            print(f"📨 收到: {msg_type}")

    def handle_binary_message(self, data: bytes):
        """处理二进制消息（音频数据）"""
        if len(data) < HEADER_SIZE:
            print(f"警告: 二进制数据太短 ({len(data)} bytes)")
            return

        # 解析头部获取 command_id
        command_id = data[17]

        if command_id == CMD_RESPONSE_AUDIO_DELTA:
            try:
                audio_delta = parse_audio_delta(data)
                self.received_audio.extend(audio_delta.audio_data)
                print(f"🔊 收到音频: {len(audio_delta.audio_data)} bytes (累计: {len(self.received_audio)} bytes)")
            except Exception as e:
                print(f"警告: 解析音频数据失败: {e}")
        else:
            print(f"📨 收到二进制消息, command_id={command_id}")

    async def send_text(self, text: str):
        """
        发送文本消息（用于文本对话模式）
        使用 protocol_id=2 (LLM)
        """
        await self.send_json({
            "protocol_id": PROTOCOL_LLM,
            "command_id": CMD_TEXT_DATA,
            "session_id": self.session_id,
            "payload": {
                "type": "text_data",
                "text": text
            }
        })
        print(f">>> 发送文本: {text}")

    async def receive_messages(self, timeout: float = 60.0):
        """接收并处理服务器消息"""
        try:
            start_time = asyncio.get_event_loop().time()
            async for message in self.ws:
                if isinstance(message, str):
                    # JSON 消息
                    data = json.loads(message)
                    self.handle_json_message(data)
                else:
                    # 二进制消息
                    self.handle_binary_message(message)

                # 如果对话完成，退出循环
                if self.is_response_done:
                    break

                # 超时检查
                if asyncio.get_event_loop().time() - start_time > timeout:
                    print(f"⚠️ 接收超时 ({timeout}s)")
                    break
        except websockets.exceptions.ConnectionClosed as e:
            print(f"连接关闭: {e}")

    def save_audio(self, output_path: str):
        """保存接收到的音频"""
        if not self.received_audio:
            print("没有收到音频数据")
            return

        with open(output_path, 'wb') as f:
            f.write(self.received_audio)

        duration_ms = len(self.received_audio) / SAMPLE_WIDTH / OUTPUT_SAMPLE_RATE * 1000
        print(f"✅ 音频已保存: {output_path} ({len(self.received_audio)} bytes, {duration_ms:.0f}ms)")

    def save_audio_as_wav(self, output_path: str):
        """保存为 WAV 格式 (TTS 返回 24kHz)"""
        if not self.received_audio:
            print("没有收到音频数据")
            return

        with wave.open(output_path, 'wb') as wf:
            wf.setnchannels(CHANNELS)
            wf.setsampwidth(SAMPLE_WIDTH)
            wf.setframerate(OUTPUT_SAMPLE_RATE)
            wf.writeframes(self.received_audio)

        duration_ms = len(self.received_audio) / SAMPLE_WIDTH / OUTPUT_SAMPLE_RATE * 1000
        print(f"✅ 音频已保存: {output_path} ({len(self.received_audio)} bytes, {duration_ms:.0f}ms)")


# ============== 主流程 ==============

async def run_text_conversation(
    text: str,
    output_file: Optional[str] = None,
    config: Optional[SessionConfig] = None
):
    """
    运行一次文本对话（用于测试 LLM+TTS）

    Args:
        text: 要发送的文本
        output_file: 输出音频文件路径（可选）
        config: 会话配置
    """
    if config is None:
        config = SessionConfig()

    client = RealtimeClient()

    try:
        # 1. 建立连接
        await client.connect()

        # 2. 创建会话
        await client.start_session(config)

        # 等待会话创建（处理多次 session.created）
        await asyncio.sleep(1.0)
        while True:
            try:
                msg = await asyncio.wait_for(client.ws.recv(), timeout=0.5)
                if isinstance(msg, str):
                    data = json.loads(msg)
                    client.handle_json_message(data)
            except asyncio.TimeoutError:
                break

        if not client.is_session_created:
            print("❌ 会话创建失败")
            return

        # 3. 发送文本
        await client.send_text(text)

        # 4. 发送 StopInput 触发 AI 回复
        await asyncio.sleep(0.2)
        await client.stop_input()

        # 5. 接收响应
        print("等待 AI 响应...")
        await client.receive_messages(timeout=30.0)

        # 6. 结束会话
        await client.stop_session()

        # 7. 保存音频
        if output_file and client.received_audio:
            if output_file.endswith('.wav'):
                client.save_audio_as_wav(output_file)
            else:
                client.save_audio(output_file)

        # 打印结果
        print("\n" + "=" * 50)
        print("对话结果:")
        print(f"  用户: {text}")
        print(f"  AI: {client.response_text}")
        print(f"  音频: {len(client.received_audio)} bytes")
        print("=" * 50)

    except Exception as e:
        print(f"错误: {e}")
        import traceback
        traceback.print_exc()
    finally:
        await client.close()


async def run_conversation(
    input_file: str,
    output_file: str,
    is_raw_input: bool = False,
    config: Optional[SessionConfig] = None
):
    """
    运行一次完整的语音对话

    Args:
        input_file: 输入音频文件路径
        output_file: 输出音频文件路径
        is_raw_input: 输入是否为原始 PCM 文件
        config: 会话配置
    """
    if config is None:
        config = SessionConfig()

    # 加载音频
    print(f"加载音频: {input_file}")
    audio_data = load_audio_file(input_file, is_raw_input)
    print(f"音频时长: {len(audio_data) / SAMPLE_WIDTH / SAMPLE_RATE * 1000:.0f}ms")

    client = RealtimeClient()

    try:
        # 1. 建立连接
        await client.connect()

        # 2. 创建会话
        await client.start_session(config)

        # 启动消息接收任务
        receive_task = asyncio.create_task(client.receive_messages())

        # 等待会话创建成功
        while not client.is_session_created:
            await asyncio.sleep(0.1)

        # 3. 发送音频
        await client.send_audio_file(audio_data)

        # 4. PTT/vad_deferred 模式: 发送 StopInput 触发 AI 回复
        # (VAD 模式会自动触发，但为了兼容性统一发送)
        await asyncio.sleep(0.1)
        await client.stop_input()

        # 6. 等待响应完成
        await receive_task

        # 7. 结束会话
        await client.stop_session()

        # 8. 保存音频
        if output_file.endswith('.wav'):
            client.save_audio_as_wav(output_file)
        else:
            client.save_audio(output_file)

        # 打印结果摘要
        print("\n" + "=" * 50)
        print("对话摘要:")
        print(f"  用户说: {client.transcription_text}")
        print(f"  AI回复: {client.response_text}")
        print("=" * 50)

    except Exception as e:
        print(f"错误: {e}")
        raise
    finally:
        await client.close()


async def run_asr_only(
    input_file: str,
    is_raw_input: bool = False,
    config: Optional[SessionConfig] = None
):
    """
    运行纯 ASR 模式 (protocol_id=1)
    只做语音识别，不经过 LLM 和 TTS

    Args:
        input_file: 输入音频文件路径
        is_raw_input: 输入是否为原始 PCM 文件
        config: 会话配置
    """
    if config is None:
        config = SessionConfig()

    # 加载音频
    print(f"加载音频: {input_file}")
    audio_data = load_audio_file(input_file, is_raw_input)
    print(f"音频时长: {len(audio_data) / SAMPLE_WIDTH / SAMPLE_RATE * 1000:.0f}ms")

    client = RealtimeClient()
    client.asr_only_mode = True  # 启用 ASR-only 模式

    try:
        # 1. 建立连接
        await client.connect()

        # 生成 session_id
        client.session_id = generate_session_id()
        print(f"会话 ID: {client.session_id}")

        # 2. 发送 Start 命令创建 ASR 会话 (protocol_id=1, command_id=1)
        start_payload = {
            "type": "session_config",
            "mode": config.mode,
            "vad_threshold": config.vad_threshold,
            "silence_duration_ms": config.silence_duration_ms
        }

        await client.send_json({
            "protocol_id": PROTOCOL_ASR,
            "command_id": CMD_START,
            "session_id": client.session_id,
            "payload": start_payload
        })
        print(">>> 发送 ASR Start 命令")

        # 等待会话创建
        await asyncio.sleep(0.5)
        while True:
            try:
                msg = await asyncio.wait_for(client.ws.recv(), timeout=0.5)
                if isinstance(msg, str):
                    data = json.loads(msg)
                    client.handle_json_message(data)
            except asyncio.TimeoutError:
                break

        if not client.is_session_created:
            print("⚠️ 未收到 session.created，继续尝试...")

        # 3. 发送音频
        await client.send_audio_file(audio_data)

        # 4. 发送 StopInput 触发识别结果
        await asyncio.sleep(0.1)
        await client.send_json({
            "protocol_id": PROTOCOL_ASR,
            "command_id": CMD_STOP_INPUT,
            "session_id": client.session_id,
            "payload": None
        })
        print(">>> 发送 StopInput，等待 ASR 结果")

        # 5. 接收识别结果
        print("等待 ASR 响应...")
        await client.receive_messages(timeout=30.0)

        # 6. 发送 Stop 结束会话
        await client.send_json({
            "protocol_id": PROTOCOL_ASR,
            "command_id": CMD_STOP,
            "session_id": client.session_id,
            "payload": None
        })
        print(">>> 发送 Stop，结束会话")

        # 打印结果
        print("\n" + "=" * 50)
        print("ASR 结果:")
        print(f"  识别文本: {client.transcription_text}")
        print("=" * 50)

    except Exception as e:
        print(f"错误: {e}")
        import traceback
        traceback.print_exc()
    finally:
        await client.close()


async def run_llm_only(
    text: str,
    config: Optional[SessionConfig] = None
):
    """
    运行纯 LLM 模式 (protocol_id=2)
    只做 AI 对话，不做 TTS 语音合成

    Args:
        text: 输入文本
        config: 会话配置
    """
    if config is None:
        config = SessionConfig()

    client = RealtimeClient()

    try:
        # 1. 建立连接
        await client.connect()

        # 生成 session_id
        client.session_id = generate_session_id()
        print(f"会话 ID: {client.session_id}")

        # 2. 发送 Start 命令创建 LLM 会话 (protocol_id=2, command_id=1)
        start_payload = {
            "type": "session_config",
            "system_prompt": config.system_prompt
        }
        if config.enable_search:
            start_payload["enable_search"] = True
        if config.asr_language:
            start_payload["asr_language"] = config.asr_language

        await client.send_json({
            "protocol_id": PROTOCOL_LLM,
            "command_id": CMD_START,
            "session_id": client.session_id,
            "payload": start_payload
        })
        print(">>> 发送 LLM Start 命令")

        # 等待会话创建
        await asyncio.sleep(0.5)
        while True:
            try:
                msg = await asyncio.wait_for(client.ws.recv(), timeout=0.5)
                if isinstance(msg, str):
                    data = json.loads(msg)
                    client.handle_json_message(data)
            except asyncio.TimeoutError:
                break

        if not client.is_session_created:
            print("⚠️ 未收到 session.created，继续尝试...")

        # 3. 发送 TextData
        await client.send_json({
            "protocol_id": PROTOCOL_LLM,
            "command_id": CMD_TEXT_DATA,
            "session_id": client.session_id,
            "payload": {
                "type": "text_data",
                "text": text
            }
        })
        print(f">>> 发送 LLM 文本: {text}")

        # 4. 发送 StopInput 触发 LLM 输出
        await asyncio.sleep(0.1)
        await client.send_json({
            "protocol_id": PROTOCOL_LLM,
            "command_id": CMD_STOP_INPUT,
            "session_id": client.session_id,
            "payload": None
        })
        print(">>> 发送 StopInput，等待 LLM 响应")

        # 5. 接收 LLM 响应
        print("等待 LLM 响应...")
        await client.receive_messages(timeout=60.0)

        # 6. 发送 Stop 结束会话
        await client.send_json({
            "protocol_id": PROTOCOL_LLM,
            "command_id": CMD_STOP,
            "session_id": client.session_id,
            "payload": None
        })
        print(">>> 发送 Stop，结束会话")

        # 打印结果
        print("\n" + "=" * 50)
        print("LLM 结果:")
        print(f"  用户输入: {text}")
        print(f"  AI 回复: {client.response_text}")
        print("=" * 50)

    except Exception as e:
        print(f"错误: {e}")
        import traceback
        traceback.print_exc()
    finally:
        await client.close()


async def run_tts_only(
    text: str,
    output_file: str,
    config: Optional[SessionConfig] = None
):
    """
    运行纯 TTS 模式 (protocol_id=3)
    直接将文字转换成语音，不经过 LLM

    Args:
        text: 要合成的文本
        output_file: 输出音频文件路径
        config: 会话配置（主要用于 voice_setting）
    """
    if config is None:
        config = SessionConfig()

    client = RealtimeClient()

    try:
        # 1. 建立连接
        await client.connect()

        # 生成 session_id
        client.session_id = generate_session_id()
        print(f"会话 ID: {client.session_id}")

        # 2. 发送 Start 命令创建 TTS 会话 (protocol_id=3, command_id=1)
        start_payload = {
            "type": "session_config"
        }
        if config.voice_id:
            start_payload["voice_setting"] = {
                "voice_id": config.voice_id
            }

        await client.send_json({
            "protocol_id": PROTOCOL_TTS,
            "command_id": CMD_START,
            "session_id": client.session_id,
            "payload": start_payload
        })
        print(">>> 发送 TTS Start 命令")

        # 等待会话创建
        await asyncio.sleep(0.5)
        while True:
            try:
                msg = await asyncio.wait_for(client.ws.recv(), timeout=0.5)
                if isinstance(msg, str):
                    data = json.loads(msg)
                    client.handle_json_message(data)
            except asyncio.TimeoutError:
                break

        if not client.is_session_created:
            print("⚠️ 未收到 session.created，继续尝试...")

        # 3. 发送 TextData (protocol_id=3, command_id=4)
        await client.send_json({
            "protocol_id": PROTOCOL_TTS,
            "command_id": CMD_TEXT_DATA,
            "session_id": client.session_id,
            "payload": {
                "type": "text_data",
                "text": text
            }
        })
        print(f">>> 发送 TTS 文本: {text[:50]}..." if len(text) > 50 else f">>> 发送 TTS 文本: {text}")

        # 4. 发送 StopInput 触发 TTS 输出 (protocol_id=3, command_id=5)
        await asyncio.sleep(0.1)
        await client.send_json({
            "protocol_id": PROTOCOL_TTS,
            "command_id": CMD_STOP_INPUT,
            "session_id": client.session_id,
            "payload": None
        })
        print(">>> 发送 StopInput，触发 TTS 输出")

        # 5. 接收音频响应
        print("等待 TTS 响应...")
        await client.receive_messages(timeout=60.0)

        # 6. 发送 Stop 结束会话
        await client.send_json({
            "protocol_id": PROTOCOL_TTS,
            "command_id": CMD_STOP,
            "session_id": client.session_id,
            "payload": None
        })
        print(">>> 发送 Stop，结束会话")

        # 7. 保存音频
        if client.received_audio:
            if output_file.endswith('.wav'):
                client.save_audio_as_wav(output_file)
            else:
                client.save_audio(output_file)

        # 打印结果
        print("\n" + "=" * 50)
        print("TTS 结果:")
        print(f"  输入文本: {text}")
        print(f"  音频大小: {len(client.received_audio)} bytes")
        if client.received_audio:
            duration_ms = len(client.received_audio) / SAMPLE_WIDTH / OUTPUT_SAMPLE_RATE * 1000
            print(f"  音频时长: {duration_ms:.0f}ms")
        print("=" * 50)

    except Exception as e:
        print(f"错误: {e}")
        import traceback
        traceback.print_exc()
    finally:
        await client.close()


async def run_translation(
    input_file: str,
    output_file: str,
    is_raw_input: bool = False,
    config: Optional[SessionConfig] = None
):
    """
    运行同声传译 (protocol_id=4)

    Args:
        input_file: 输入音频文件路径（源语言音频）
        output_file: 输出音频文件路径（目标语言音频）
        is_raw_input: 输入是否为原始 PCM 文件
        config: 会话配置（包含 from_language 和 to_language）
    """
    if config is None:
        config = SessionConfig()

    if not config.from_language or not config.to_language:
        print("错误: 同声传译模式需要指定 from_language 和 to_language")
        return

    # 加载音频
    print(f"加载音频: {input_file}")
    audio_data = load_audio_file(input_file, is_raw_input)
    print(f"音频时长: {len(audio_data) / SAMPLE_WIDTH / SAMPLE_RATE * 1000:.0f}ms")

    client = RealtimeClient()

    try:
        # 1. 建立连接
        await client.connect()

        # 2. 创建翻译会话 (protocol_id=4)
        await client.start_translation_session(config)

        # 启动消息接收任务
        receive_task = asyncio.create_task(client.receive_messages())

        # 等待会话创建成功（或错误）
        while not client.is_session_created and not client.is_response_done:
            await asyncio.sleep(0.1)

        if client.is_response_done and not client.is_session_created:
            print("❌ 翻译会话创建失败")
            return

        # 3. 发送源语言音频
        await client.send_audio_file(audio_data)

        # 4. 发送 StopInput 触发翻译
        await asyncio.sleep(0.1)
        await client.stop_input()

        # 5. 等待翻译完成
        await receive_task

        # 6. 结束会话
        await client.stop_session()

        # 7. 保存翻译后的音频
        if output_file.endswith('.wav'):
            client.save_audio_as_wav(output_file)
        else:
            client.save_audio(output_file)

        # 打印结果摘要
        print("\n" + "=" * 50)
        print("翻译结果:")
        print(f"  源语言 ({config.from_language}): {client.transcription_text}")
        print(f"  翻译后 ({config.to_language}): {client.response_text}")
        print(f"  音频: {len(client.received_audio)} bytes")
        print("=" * 50)

    except Exception as e:
        print(f"错误: {e}")
        raise
    finally:
        await client.close()


def main():
    parser = argparse.ArgumentParser(
        description="Realtime WebSocket API 客户端",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
示例:
  # 语音对话模式
  python realtime_client.py input.wav output.pcm
  python realtime_client.py input.wav output.wav
  python realtime_client.py input.pcm output.pcm --raw

  # 文本对话模式
  python realtime_client.py --text "你好，今天天气怎么样" --output response.wav

  # 纯 TTS 模式（文字转语音，不经过 LLM）
  python realtime_client.py --tts "你好，欢迎使用语音合成服务" tts_output.wav

  # 纯 ASR 模式（只做语音识别）
  python realtime_client.py --asr input.wav

  # 纯 LLM 模式（只做 AI 对话，无语音）
  python realtime_client.py --llm "你好，今天天气怎么样"

  # 同声传译模式 (英译中)
  python realtime_client.py english.wav output.wav --translate --from-lang en --to-lang zh

  # 同声传译模式 (中译英)
  python realtime_client.py chinese.wav output.wav --translate --from-lang zh --to-lang en

  # 自定义配置
  python realtime_client.py input.wav output.pcm --prompt "你是一个英语老师"
        """
    )

    parser.add_argument("input", nargs="?", help="输入音频文件 (WAV 或 PCM)")
    parser.add_argument("output", nargs="?", help="输出音频文件 (PCM 或 WAV)")
    parser.add_argument("--text", "-t", help="文本对话模式：直接发送文本（经过 LLM）")
    parser.add_argument("--tts", help="纯 TTS 模式：直接文字转语音（不经过 LLM）")
    parser.add_argument("--asr", action="store_true", help="纯 ASR 模式：只做语音识别")
    parser.add_argument("--llm", help="纯 LLM 模式：只做 AI 对话（无语音）")
    parser.add_argument("--raw", action="store_true", help="输入为原始 PCM 文件")
    parser.add_argument("--prompt", default="你是一个友好的AI助手，用简短的语言回答问题",
                        help="系统提示词")
    parser.add_argument("--voice", default="zh_female_wanwanxiaohe_moon_bigtts",
                        help="语音 ID")
    parser.add_argument("--mode", default="vad_deferred",
                        choices=["vad", "vad_deferred", "ptt"],
                        help="VAD 模式")
    parser.add_argument("--vad-threshold", type=float, default=0.55,
                        help="VAD 灵敏度 (0-1)")
    parser.add_argument("--silence-ms", type=int, default=300,
                        help="静音多久算说完 (毫秒)")
    parser.add_argument("--enable-search", action="store_true", default=True,
                        help="启用联网搜索（默认开启）")
    parser.add_argument("--disable-search", action="store_true",
                        help="禁用联网搜索")
    # 同声传译参数
    parser.add_argument("--translate", action="store_true",
                        help="启用同声传译模式 (protocol_id=4)")
    parser.add_argument("--from-lang", default=None,
                        help="源语言代码 (如 en, zh, ja)")
    parser.add_argument("--to-lang", default=None,
                        help="目标语言代码 (如 zh, en, ja)")
    # 工具配置参数
    parser.add_argument("--prompt-endpoint", default=None,
                        help="工具定义端点 URL (用于测试工具更新)")

    args = parser.parse_args()

    # 创建配置
    # enable_search: 默认开启，除非指定 --disable-search
    enable_search = not args.disable_search
    config = SessionConfig(
        mode=args.mode,
        system_prompt=args.prompt,
        voice_id=args.voice,
        vad_threshold=args.vad_threshold,
        silence_duration_ms=args.silence_ms,
        enable_search=enable_search,
        from_language=args.from_lang,
        to_language=args.to_lang,
        prompt_endpoint=args.prompt_endpoint
    )

    # 文本对话模式（经过 LLM）
    if args.text:
        asyncio.run(run_text_conversation(
            text=args.text,
            output_file=args.output,
            config=config
        ))
        return 0

    # 纯 TTS 模式（不经过 LLM）
    if args.tts:
        # TTS 模式：第一个位置参数是 output（因为不需要 input）
        output_file = args.output or args.input
        if not output_file:
            print("错误: TTS 模式需要指定输出文件路径")
            return 1
        asyncio.run(run_tts_only(
            text=args.tts,
            output_file=output_file,
            config=config
        ))
        return 0

    # 纯 ASR 模式（只做语音识别）
    if args.asr:
        if not args.input:
            print("错误: ASR 模式需要输入音频文件")
            return 1
        if not Path(args.input).exists():
            print(f"错误: 输入文件不存在: {args.input}")
            return 1
        asyncio.run(run_asr_only(
            input_file=args.input,
            is_raw_input=args.raw,
            config=config
        ))
        return 0

    # 纯 LLM 模式（只做 AI 对话）
    if args.llm:
        asyncio.run(run_llm_only(
            text=args.llm,
            config=config
        ))
        return 0

    # 同声传译模式
    if args.translate:
        if not args.from_lang or not args.to_lang:
            print("错误: 同声传译模式需要指定 --from-lang 和 --to-lang")
            return 1
        if not args.input:
            print("错误: 同声传译模式需要输入音频文件")
            return 1
        if not args.output:
            print("错误: 请提供输出音频文件路径")
            return 1
        if not Path(args.input).exists():
            print(f"错误: 输入文件不存在: {args.input}")
            return 1

        asyncio.run(run_translation(
            input_file=args.input,
            output_file=args.output,
            is_raw_input=args.raw,
            config=config
        ))
        return 0

    # 语音对话模式
    if not args.input:
        print("错误: 请提供输入音频文件，或使用 --text 参数进行文本对话")
        parser.print_help()
        return 1

    if not Path(args.input).exists():
        print(f"错误: 输入文件不存在: {args.input}")
        return 1

    if not args.output:
        print("错误: 请提供输出音频文件路径")
        return 1

    # 运行语音对话
    asyncio.run(run_conversation(
        input_file=args.input,
        output_file=args.output,
        is_raw_input=args.raw,
        config=config
    ))

    return 0


if __name__ == "__main__":
    exit(main())
