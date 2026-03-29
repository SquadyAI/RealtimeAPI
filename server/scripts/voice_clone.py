#!/usr/bin/env python3
"""
MiniMax 音色快速复刻脚本

功能：
1. 裁剪音频文件（根据开始时间、结束时间或时长）
2. 上传复刻音频到 MiniMax API
3. 可选：上传示例音频
4. 调用音色克隆接口

使用方法：
    python voice_clone.py --input audio.wav --start 10 --end 60 --voice-id my_voice_001
    python voice_clone.py --input audio.wav --start 10 --duration 50 --voice-id my_voice_001
    python voice_clone.py --input audio.wav --start 10 --end 60 --voice-id my_voice_001 --prompt-audio prompt.wav --prompt-text "示例文本"
"""

import os
import sys
import argparse
import requests
from pathlib import Path
from typing import Optional, Tuple
from pydub import AudioSegment


class VoiceCloneClient:
    """MiniMax 音色克隆客户端"""

    def __init__(self, api_key: Optional[str] = None):
        """
        初始化客户端

        :param api_key: MiniMax API Key，如果为None则从环境变量 MINIMAX_TTS_API_KEY 读取
        """
        self.api_key = api_key or os.environ.get("MINIMAX_TTS_API_KEY")
        if not self.api_key:
            raise ValueError(
                "未找到 API Key！请设置环境变量 MINIMAX_TTS_API_KEY 或通过参数传入"
            )

        self.upload_url = "https://api.minimaxi.com/v1/files/upload"
        self.clone_url = "https://api.minimaxi.com/v1/voice_clone"
        self.headers = {
            "Authorization": f"Bearer {self.api_key}"
        }

    def trim_audio(
        self,
        input_path: str,
        output_path: str,
        start_time: float = 0.0,
        end_time: Optional[float] = None,
        duration: Optional[float] = None
    ) -> Tuple[str, float]:
        """
        裁剪音频文件

        :param input_path: 输入音频文件路径
        :param output_path: 输出音频文件路径
        :param start_time: 开始时间（秒）
        :param end_time: 结束时间（秒），如果提供则忽略 duration
        :param duration: 时长（秒），仅在 end_time 未提供时使用
        :return: (输出文件路径, 实际时长)
        """
        print(f"📂 加载音频文件: {input_path}")
        audio = AudioSegment.from_file(input_path)

        # 转换为毫秒
        start_ms = int(start_time * 1000)

        if end_time is not None:
            end_ms = int(end_time * 1000)
            duration_ms = end_ms - start_ms
        elif duration is not None:
            duration_ms = int(duration * 1000)
            end_ms = start_ms + duration_ms
        else:
            # 如果没有指定结束时间或时长，使用整个文件
            end_ms = len(audio)
            duration_ms = end_ms - start_ms

        # 验证时间范围
        if start_ms < 0:
            raise ValueError(f"开始时间不能为负数: {start_time}")
        if start_ms >= len(audio):
            raise ValueError(f"开始时间 {start_time} 秒超出音频长度 {len(audio)/1000:.2f} 秒")
        if end_ms > len(audio):
            print(f"⚠️  警告: 结束时间 {end_time or (start_time + duration)} 秒超出音频长度，将使用文件末尾")
            end_ms = len(audio)
            duration_ms = end_ms - start_ms

        # 裁剪音频
        trimmed = audio[start_ms:end_ms]
        actual_duration = len(trimmed) / 1000.0

        print(f"✂️  裁剪音频: {start_time:.2f}s - {end_ms/1000:.2f}s (时长: {actual_duration:.2f}s)")

        # 确保输出目录存在
        output_path_obj = Path(output_path)
        output_path_obj.parent.mkdir(parents=True, exist_ok=True)

        # 导出音频（根据扩展名选择格式）
        output_ext = output_path_obj.suffix.lower()
        if output_ext == '.mp3':
            trimmed.export(output_path, format="mp3")
        elif output_ext == '.m4a':
            trimmed.export(output_path, format="ipod")
        elif output_ext in ['.wav', '.wave']:
            trimmed.export(output_path, format="wav")
        else:
            # 默认使用 wav 格式
            trimmed.export(output_path, format="wav")

        # 检查文件大小
        file_size_mb = os.path.getsize(output_path) / (1024 * 1024)
        print(f"💾 输出文件: {output_path} ({file_size_mb:.2f} MB)")

        if file_size_mb > 20:
            raise ValueError(f"文件大小 {file_size_mb:.2f} MB 超过限制 20 MB")

        return output_path, actual_duration

    def upload_file(self, file_path: str, purpose: str) -> str:
        """
        上传文件到 MiniMax API

        :param file_path: 文件路径
        :param purpose: 上传目的 ("voice_clone" 或 "prompt_audio")
        :return: file_id
        """
        print(f"📤 上传文件: {file_path} (purpose: {purpose})")

        with open(file_path, "rb") as f:
            files = {"file": (os.path.basename(file_path), f)}
            data = {"purpose": purpose}

            response = requests.post(
                self.upload_url,
                headers=self.headers,
                data=data,
                files=files
            )

        response.raise_for_status()
        result = response.json()
        file_id = result.get("file", {}).get("file_id")

        if not file_id:
            raise ValueError(f"上传失败，未返回 file_id: {result}")

        print(f"✅ 上传成功，file_id: {file_id}")
        return file_id

    def clone_voice(
        self,
        file_id: str,
        voice_id: str,
        text: str = "大兄弟，听您口音不是本地人吧，头回来天津卫，啊，待会您可甭跟着导航走，那玩意儿净给您往大马路上绕。",
        model: str = "speech-2.6-turbo",
        prompt_audio: Optional[str] = None,
        prompt_text: Optional[str] = None
    ) -> dict:
        """
        调用音色克隆接口

        :param file_id: 复刻音频的 file_id
        :param voice_id: 自定义音色 ID
        :param text: 试听文本
        :param model: 使用的模型
        :param prompt_audio: 示例音频的 file_id（可选）
        :param prompt_text: 示例音频对应的文本（可选）
        :return: API 响应结果
        """
        print(f"🎭 开始音色克隆: voice_id={voice_id}, model={model}")

        payload = {
            "file_id": file_id,
            "voice_id": voice_id,
            "text": text,
            "model": model
        }

        # 如果提供了示例音频，添加到 payload
        if prompt_audio:
            payload["clone_prompt"] = {}
            payload["clone_prompt"]["prompt_audio"] = prompt_audio
            if prompt_text:
                payload["clone_prompt"]["prompt_text"] = prompt_text

        clone_headers = {
            **self.headers,
            "Content-Type": "application/json"
        }

        response = requests.post(
            self.clone_url,
            headers=clone_headers,
            json=payload
        )

        response.raise_for_status()
        result = response.json()

        print(f"✅ 音色克隆成功")
        return result


def main():
    parser = argparse.ArgumentParser(
        description="MiniMax 音色快速复刻工具",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
示例:
  # 基本用法：裁剪并上传
  python voice_clone.py --input audio.wav --start 10 --end 60 --voice-id my_voice_001

  # 使用时长参数
  python voice_clone.py --input audio.wav --start 10 --duration 50 --voice-id my_voice_001

  # 带示例音频
  python voice_clone.py --input audio.wav --start 10 --end 60 --voice-id my_voice_001 \\
      --prompt-audio prompt.wav --prompt-text "示例文本"

  # 指定输出文件
  python voice_clone.py --input audio.wav --start 10 --end 60 --voice-id my_voice_001 \\
      --output trimmed.wav

  # 指定试听文本和模型
  python voice_clone.py --input audio.wav --start 10 --end 60 --voice-id my_voice_001 \\
      --text "这是试听文本" --model speech-2.6-turbo
        """
    )

    # 必需参数
    parser.add_argument(
        "--input", "-i",
        required=True,
        help="输入音频文件路径 (wav/mp3/m4a)"
    )
    parser.add_argument(
        "--voice-id", "-v",
        required=True,
        help="自定义音色 ID"
    )

    # 剪辑参数（至少需要 start）
    parser.add_argument(
        "--start", "-s",
        type=float,
        default=0.0,
        help="开始时间（秒），默认 0.0"
    )
    parser.add_argument(
        "--end", "-e",
        type=float,
        help="结束时间（秒），与 --duration 二选一"
    )
    parser.add_argument(
        "--duration", "-d",
        type=float,
        help="时长（秒），与 --end 二选一"
    )

    # 可选参数
    parser.add_argument(
        "--output", "-o",
        help="输出文件路径（默认：输入文件名_trimmed.wav）"
    )
    parser.add_argument(
        "--prompt-audio", "-p",
        help="示例音频文件路径（可选，用于增强克隆效果）"
    )
    parser.add_argument(
        "--prompt-text",
        help="示例音频对应的文本（与 --prompt-audio 一起使用）"
    )
    parser.add_argument(
        "--text", "-t",
        default="大兄弟，听您口音不是本地人吧，头回来天津卫，啊，待会您可甭跟着导航走，那玩意儿净给您往大马路上绕。",
        help="试听文本（默认：示例文本）"
    )
    parser.add_argument(
        "--model", "-m",
        default="speech-2.6-turbo",
        help="使用的模型（默认：speech-2.6-turbo）"
    )
    parser.add_argument(
        "--api-key",
        help="MiniMax API Key（默认：从环境变量 MINIMAX_TTS_API_KEY 读取）"
    )
    parser.add_argument(
        "--skip-upload",
        action="store_true",
        help="跳过上传，仅裁剪音频"
    )
    parser.add_argument(
        "--skip-clone",
        action="store_true",
        help="跳过克隆，仅上传文件"
    )

    args = parser.parse_args()

    # 验证参数
    if args.end is not None and args.duration is not None:
        parser.error("--end 和 --duration 不能同时指定")

    if not os.path.exists(args.input):
        parser.error(f"输入文件不存在: {args.input}")

    # 确定输出文件路径
    if args.output:
        output_path = args.output
    else:
        input_path = Path(args.input)
        output_path = str(input_path.parent / f"{input_path.stem}_trimmed{input_path.suffix}")

    try:
        # 初始化客户端
        client = VoiceCloneClient(api_key=args.api_key)

        # 判断是否需要裁剪：只有当用户明确指定了裁剪参数时才裁剪
        # 如果 start=0.0（默认值）且没有指定 end 或 duration，则不裁剪
        need_trim = (args.start != 0.0) or (args.end is not None) or (args.duration is not None)

        if need_trim:
            # 裁剪音频
            trimmed_path, duration = client.trim_audio(
                input_path=args.input,
                output_path=output_path,
                start_time=args.start,
                end_time=args.end,
                duration=args.duration
            )
        else:
            # 不需要裁剪，直接使用原文件
            trimmed_path = args.input
            audio = AudioSegment.from_file(args.input)
            duration = len(audio) / 1000.0
            print(f"📂 使用原始音频文件: {args.input} (时长: {duration:.2f}s)")

        # 验证时长要求
        if duration < 10:
            print(f"⚠️  警告: 音频时长 {duration:.2f} 秒小于推荐最小值 10 秒")
        if duration > 300:
            print(f"⚠️  警告: 音频时长 {duration:.2f} 秒超过推荐最大值 300 秒（5分钟）")

        if args.skip_upload:
            print("⏭️  跳过上传步骤")
            return

        # 上传复刻音频
        file_id = client.upload_file(trimmed_path, purpose="voice_clone")

        # 上传示例音频（如果提供）
        prompt_file_id = None
        if args.prompt_audio:
            if not os.path.exists(args.prompt_audio):
                print(f"⚠️  警告: 示例音频文件不存在: {args.prompt_audio}")
            else:
                # 检查示例音频时长
                prompt_audio_seg = AudioSegment.from_file(args.prompt_audio)
                prompt_duration = len(prompt_audio_seg) / 1000.0
                if prompt_duration >= 8:
                    print(f"⚠️  警告: 示例音频时长 {prompt_duration:.2f} 秒超过推荐最大值 8 秒")

                prompt_file_id = client.upload_file(args.prompt_audio, purpose="prompt_audio")

        if args.skip_clone:
            print("⏭️  跳过克隆步骤")
            print(f"\n📋 文件 ID:")
            print(f"  复刻音频: {file_id}")
            if prompt_file_id:
                print(f"  示例音频: {prompt_file_id}")
            return

        # 调用克隆接口
        result = client.clone_voice(
            file_id=file_id,
            voice_id=args.voice_id,
            text=args.text,
            model=args.model,
            prompt_audio=prompt_file_id,
            prompt_text=args.prompt_text
        )

        print("\n" + "="*50)
        print("✅ 音色克隆完成！")
        print("="*50)
        print(f"音色 ID: {args.voice_id}")
        print(f"模型: {args.model}")
        print(f"\n响应结果:")
        print(result)

    except Exception as e:
        print(f"\n❌ 错误: {e}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
