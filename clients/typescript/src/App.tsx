/* eslint-disable @typescript-eslint/no-unused-vars */
import React, { useEffect, useLayoutEffect, useRef, useState, useCallback, useMemo } from 'react'
import { useAtom } from 'jotai'
import ReactMarkdown from 'react-markdown'
import styles from './App.module.scss'
import squadyLogo from './assets/squady.png'
import { isUserSpeakingAtom, logsAtom, wsConnectedAtom, sessionCreatedAtom } from './state'
import { LiveAudioVisualizerComponent } from './component/audioVisualizer'
import { RealTime } from './RealTime'
import { StatusPoint } from './component/statusPoint'
import type { AudioManager } from './sdk/audio/AudioManager'
import type { AudioChunkPayload, ResponseAudioDeltaEvent, OutputAudioBufferStartedEvent, ResponseAudioDoneEvent, ResponseTextDeltaEvent, ResponseTextDoneEvent, InputAudioSpeechStartedEvent, InputAudioSpeechStoppedEvent } from './sdk/protocol/ClientProtocol';
import { ProtocolId } from './sdk/protocol/ClientProtocol';
import { AudioPlayer } from './sdk/audio/AudioPlayer';
import { audioProcessor } from './sdk/audio/AudioCodec';
import { base64ToUint8Array } from './sdk/audio/AudioUtils';
import type { VoiceChatCore } from './sdk';

import {
  decodeResponseAudioDeltaMessage
} from './sdk/protocol/BinaryAudioDecoder';

import type {
  DecodedAudioDeltaMessage
} from './sdk/protocol/BinaryAudioTypes';

import { VisionTestPanel } from './components/VisionTestPanel';
import { PWAInstallPrompt } from './components/PWAInstallPrompt';
import { useReminderHandler } from './hooks/useReminderHandler';
import { useCameraHandler } from './hooks/useCameraHandler';
import { ReminderPanel } from './components/ReminderPanel';
import { DEFAULT_WEBSOCKET_URL } from './sdk/types/protocol';
import { getRuntimeWebSocketUrl, setStoredWebSocketUrl } from './sdk/config/runtimeConfig';

// 趋势图组件
const TrendChart = React.memo(({ responseMetrics }: {
  responseMetrics: {
    audioResponseTimes: number[];
    averageAudioLatency: number;
    totalInteractions: number;
    lastAudioLatency: number;
    isTimingActive: boolean;
  }
}) => {
  const trendData = useMemo(() => {
    const times = responseMetrics.audioResponseTimes.slice(-10);
    return {
      times,
      minTime: Math.min(...responseMetrics.audioResponseTimes),
      maxTime: Math.max(...responseMetrics.audioResponseTimes)
    };
  }, [responseMetrics.audioResponseTimes]);

  return (
    <div className={styles.responseTrend}>
      <div className={styles.trendLabel}>音频响应延迟趋势（最近10次）</div>
      <div className={styles.trendBars}>
        {trendData.times.map((time: number, index: number) => (
          <div
            key={index}
            className={styles.trendBar}
            style={{
              height: `${Math.min(time / 20, 50)}px`,
              backgroundColor: time < 1000 ? '#4CAF50' : time < 2000 ? '#FF9800' : '#F44336'
            }}
            title={`第${responseMetrics.audioResponseTimes.length - 9 + index}次: ${time.toFixed(0)}ms`}
          />
        ))}
      </div>
      <div className={styles.trendStats}>
        <span>最快: {trendData.minTime.toFixed(1)}ms</span>
        <span>最慢: {trendData.maxTime.toFixed(1)}ms</span>
        <span>精度: 1ms</span>
      </div>
    </div>
  );
});

TrendChart.displayName = 'TrendChart';

// 定义下行事件常量（参考 ClientProtocol.ts 中的服务端事件）
const downstreamEvents = [
  'session.created',
  'conversation.item.created',
  'conversation.item.updated',
  'input_audio_buffer.speech_started',
  'input_audio_buffer.speech_stopped',
  'conversation.item.input_audio_transcription.delta',
  'conversation.item.input_audio_transcription.completed',
  'response.created',
  'response.text.delta',
  'response.text.done',
  'response.audio.delta',
  'response.audio.done',
  'response.output_item.added',
  'response.output_item.done',
  'response.done',
  'conversation.item.truncated',
  'output_audio_buffer.started',
  'output_audio_buffer.stopped',
  'error.event',
  'output_audio_buffer.cleared',
  'conversation.item.input_audio_transcription.failed',
  'response.function_call_arguments.delta',
  'response.function_call_arguments.done',
  'response.function_call_result.done',
  'session.update',
  'response.cancel',
  'response.function_call.delta',
  'response.function_call.done',
  'response.function_call_result.delta',
  // 新的二进制音频事件
  'binary_audio_delta',
  'binary_audio_error'
] as const;


export const App = () => {
  const [isUserSpeaking, setIsUserSpeaking] = useAtom(isUserSpeakingAtom)
  const [speechDetected, setSpeechDetected] = useState(false);
  const [speechSegmentCount, setSpeechSegmentCount] = useState(0);
  const [talkHint, setTalkHint] = useState<string>(''); // 移动端点击“开始对话”无反馈时用于提示原因

  // Voice (TTS) config
  const VOICE_PRESET_STORAGE_KEY = 'voice_preset';
  const LEGACY_USE_CUSTOM_VOICE_STORAGE_KEY = 'use_custom_voice';
  const CUSTOM_VOICE_ID = 'ttv-voice-2025120918423925-Ktg6miPT';
  type VoicePreset = 'default' | 'soft';
  const [voicePreset, setVoicePreset] = useState<VoicePreset>(() => {
    try {
      const preset = localStorage.getItem(VOICE_PRESET_STORAGE_KEY) as VoicePreset | null;
      if (preset === 'default' || preset === 'soft') return preset;
      const legacy = localStorage.getItem(LEGACY_USE_CUSTOM_VOICE_STORAGE_KEY);
      return (legacy === '1' || legacy === 'true') ? 'soft' : 'default';
    } catch {
      return 'default';
    }
  });

  const [vadThreshold, setVadThreshold] = useState(0.5);
  const [isPlayingDownstreamAudio, setIsPlayingDownstreamAudio] = useState(false);
  // 图像上传相关状态
  const [selectedImage, setSelectedImage] = useState<File | null>(null);
  const [imagePreview, setImagePreview] = useState<string | null>(null);
  const [isImageUploading, setIsImageUploading] = useState(false);
  const [imagePrompt, setImagePrompt] = useState<string>(''); // 图像提示词
  const [isImageInputExpanded, setIsImageInputExpanded] = useState(true); // 图片输入框展开状态
  const [isImageUploadSectionExpanded, setIsImageUploadSectionExpanded] = useState(false); // 默认展示响应显示
  // 会话状态
  const [sessionStatus, setSessionStatus] = useState<{ connected: boolean, sessionCreated: boolean }>({
    connected: false,
    sessionCreated: false
  });
  // 展示下行文本（WS返回的文本）
  const [currentText, setCurrentText] = useState('');
  const [finalTexts, setFinalTexts] = useState<string[]>([]);

  const realTime = useRef<any>()
  // take_photo/tool call 闭环去重：避免 arguments.done / function_call.done 两个分支重复回传
  const takePhotoAckedCallIdsRef = useRef<Set<string>>(new Set())

  const wsPresets = useMemo(() => {
    return [
      { id: 'default', label: 'Current Server', url: DEFAULT_WEBSOCKET_URL },
      { id: 'custom', label: 'Custom', url: '' },
    ] as const;
  }, []);

  type WsPresetId = typeof wsPresets[number]['id'];

  const [wsPresetId, setWsPresetId] = useState<WsPresetId>('default');
  const [wsUrl, setWsUrl] = useState<string>(() => getRuntimeWebSocketUrl(DEFAULT_WEBSOCKET_URL));
  const [wsInput, setWsInput] = useState<string>(() => getRuntimeWebSocketUrl(DEFAULT_WEBSOCKET_URL));
  const [isSwitchingWs, setIsSwitchingWs] = useState(false);
  const [currentSessionId, setCurrentSessionId] = useState<string>(''); // 展示当前 sessionId
  const [sessionIdInput, setSessionIdInput] = useState<string>(''); // 手动传入 sessionId

  // Reminder 处理 Hook
  const { handleReminderCall } = useReminderHandler();

  // Camera 处理 Hook
  const { openCamera, closeCamera, isCameraOpen } = useCameraHandler();

  useEffect(() => {
    realTime.current = new RealTime()

    // 页面打开时预加载 VAD 模型，避免用户点击时等待 WASM 加载
    const preloadVADModel = async () => {
      try {
        console.log('[APP] 页面打开，预加载 VAD 模型...');
        const audioManager = realTime.current?.getAudioManager();
        if (audioManager) {
          await audioManager.preloadModel();
          console.log('[APP] VAD 模型预加载完成');
        }
      } catch (error) {
        console.error('[APP] VAD 模型预加载失败:', error);
        // 预加载失败不阻塞，后续按需加载
      }
    };

    preloadVADModel();
  }, [])

  useEffect(() => {
    const current = getRuntimeWebSocketUrl(DEFAULT_WEBSOCKET_URL);
    setWsUrl(current);
    setWsInput(current);
    const matched = wsPresets.find(p => p.url && p.url === current);
    setWsPresetId((matched?.id ?? 'custom') as WsPresetId);

    // 启动时打印当前实际生效的环境（从 localStorage 覆盖 + 默认值综合后的结果）
    console.log('[WS CONFIG] boot', { presetId: matched?.id ?? 'custom', presetLabel: matched?.label ?? '自定义', url: current });
  }, [wsPresets]);

  // 响应速度监控相关状态
  const [responseMetrics, setResponseMetrics] = useState<{
    audioResponseTimes: number[];
    averageAudioLatency: number;
    totalInteractions: number;
    lastAudioLatency: number;
    isTimingActive: boolean;
  }>({
    audioResponseTimes: [],
    averageAudioLatency: 0,
    totalInteractions: 0,
    lastAudioLatency: 0,
    isTimingActive: false
  });


  // 性能监控开关状态
  const [isPerformanceMonitorEnabled, setIsPerformanceMonitorEnabled] = useState(true);

  // 用于跟踪请求时间的 Ref
  const speechStartTimeRef = useRef<number | null>(null);
  const hasRecordedResponse = useRef<boolean>(false);
  
  // 用于跟踪音频开始时间（用于计算 audio_end_ms）
  const audioStartTimeRef = useRef<number>(0);

  // 用于在useEffect中访问最新状态的ref
  const isPerformanceMonitorEnabledRef = useRef(isPerformanceMonitorEnabled);
  const isPlayingDownstreamAudioRef = useRef(isPlayingDownstreamAudio);

  // 使用useLayoutEffect更新ref值，避免重渲染
  useLayoutEffect(() => {
    isPerformanceMonitorEnabledRef.current = isPerformanceMonitorEnabled;
  }, [isPerformanceMonitorEnabled]);

  useLayoutEffect(() => {
    isPlayingDownstreamAudioRef.current = isPlayingDownstreamAudio;
  }, [isPlayingDownstreamAudio]);

  const resetPerformanceMonitor = useCallback((reason: string) => {
    console.log(`[PERF MONITOR] reset (${reason})`);

    setResponseMetrics({
      audioResponseTimes: [],
      averageAudioLatency: 0,
      totalInteractions: 0,
      lastAudioLatency: 0,
      isTimingActive: false
    });

    speechStartTimeRef.current = null;
    hasRecordedResponse.current = false;

    const voiceChatCore = realTime.current?.getVoiceChatCore();
    const connectionManager = voiceChatCore?.getConnectionManager();
    connectionManager?.clearPerformanceMetrics?.();
  }, []);

  const applyWebSocketUrl = useCallback(async (nextUrl: string, presetId?: WsPresetId) => {
    const trimmed = nextUrl.trim();
    if (!trimmed) return;

    const prevUrl = getRuntimeWebSocketUrl(DEFAULT_WEBSOCKET_URL);
    const presetLabel = presetId ? (wsPresets.find(p => p.id === presetId)?.label ?? presetId) : '(unknown)';

    console.log('[WS CONFIG] switch:start', { presetId: presetId ?? null, presetLabel, from: prevUrl, to: trimmed });

    // 切换环境时：重置性能监控（避免旧连接的数据污染新环境的统计）
    resetPerformanceMonitor('ws_switch');

    setStoredWebSocketUrl(trimmed);
    setWsUrl(trimmed);
    setWsInput(trimmed);

    // 切换环境：直接刷新页面（相当于重启应用）
    // 注意：环境变化通过 localStorage 写入保证，刷新后 connect() 会读到新值。
    setIsSwitchingWs(true);
    console.log('[WS CONFIG] switch:reload', { presetId: presetId ?? null, presetLabel, url: trimmed });
    window.location.reload();
  }, [resetPerformanceMonitor, wsPresets]);

  // 切换性能监控状态
  const togglePerformanceMonitor = useCallback(() => {
    setIsPerformanceMonitorEnabled(prev => {
      const newState = !prev;

      if (newState) {
        console.log('[PERF MONITOR] 性能监控已启用');
      } else {
        console.log('[PERF MONITOR] 性能监控已禁用，清理数据...');
        resetPerformanceMonitor('disabled');
      }

      return newState;
    });
  }, [resetPerformanceMonitor]);

  // 获取RealTime实例中的AudioManager
  const audioManagerRef = useRef<AudioManager | null>(null);
  const audioPlayerRef = useRef<AudioPlayer | null>(null);

  // 传输定时器
  const transmissionTimerRef = useRef<number | null>(null);
  const nextSendAtRef = useRef<number | null>(null);

  // 性能监控显示控制
  const [isPerformanceMonitorVisible, setIsPerformanceMonitorVisible] = useState(true);

  // 切换性能监控显示
  const togglePerformanceVisibility = useCallback(() => {
    setIsPerformanceMonitorVisible(prev => !prev);
  }, []);

  // 控制下行音频播放
  const toggleDownstreamAudioPlayback = useCallback(() => {
    setIsPlayingDownstreamAudio(prev => {
      const newState = !prev;

      // 初始化或清理AudioPlayer实例
      if (newState) {
        if (!audioPlayerRef.current) {
          audioPlayerRef.current = new AudioPlayer();
        }
        // console.debug('[APP] Downstream audio playback enabled');
      } else {
        if (audioPlayerRef.current) {
          audioPlayerRef.current.stopPlayback();
          audioPlayerRef.current = null;
        }
        // console.debug('[APP] Downstream audio playback disabled');
      }

      return newState;
    });
  }, []);


  const toggleVadProcessing = useCallback(async () => {
    // console.debug("toggleVadProcessing called, isUserSpeaking:", isUserSpeaking);
    if (!audioManagerRef.current) {
      // console.debug("No audioManagerRef, returning");
      setTalkHint('音频未就绪，请稍等 1s 再试');
      return;
    }

    if (!isUserSpeaking) {
      // console.debug("[APP] Starting VAD processing");

      // 批量重置状态，减少重渲染
      setSpeechSegmentCount(0);
      setSpeechDetected(false);
      // console.debug("[APP] Cleared previous speech segments");

      // 记录音频开始时间
      audioStartTimeRef.current = Date.now();
      console.log('[APP] 音频开始时间记录:', audioStartTimeRef.current);

      try {
        // 配置并启动AudioManager
        await audioManagerRef.current
          .configure({
            enabled: true,
            threshold: vadThreshold,
            loopback: false,
            uplink: {
              onChunkProcessed: async (chunk: Uint8Array) => {
                // console.debug('[APP UPLINK] Processed audio chunk for uplink, size:', chunk.length, 'bytes');

                // 检查数据是否为空
                if (!chunk || chunk.length === 0) {
                  // console.warn('[APP UPLINK] Warning: Empty audio chunk received, skipping send');
                  return;
                }

                // 验证数据是否符合最小大小要求
                if (chunk.length < 10) {
                  // console.warn('[APP UPLINK] Warning: Audio chunk too small, size:', chunk.length, 'bytes, skipping send');
                  return;
                }

                // 注意：计时器现在由 input_audio_buffer.speech_started 事件控制
                // 这里只负责发送音频数据，不再重置计时器

                // console.debug('[APP UPLINK] Sending audio chunk to server, size:', chunk.length, 'bytes');

                // 获取VoiceChatCore实例并发送编码后的音频数据
                const voiceChatCore = realTime.current?.getVoiceChatCore();
                if (voiceChatCore) {
                  const result = voiceChatCore.sendEncodedAudioData(chunk);
                  if (!result) {
                    // console.warn('[APP UPLINK] Warning: Failed to send audio chunk to server');
                  } else {
                    // console.debug('[APP UPLINK] Audio chunk sent to server successfully');
                  }
                } else {
                  // console.warn('[APP UPLINK] VoiceChatCore instance not available');
                }
              }
            }
          })
          .start();

        // console.debug("[APP] VAD audio processing started");
        setIsUserSpeaking(true);
        setTalkHint('');

      } catch (error) {
        console.error('[APP] 启动麦克风/对话失败:', error);
        setTalkHint('启动失败：请检查麦克风权限/HTTPS/静音模式');
      }
    } else {
      // console.debug("[APP] Stopping VAD processing");

      try {
        // 获取VoiceChatCore实例
        const voiceChatCore = realTime.current?.getVoiceChatCore();

        // 停止并禁用AudioManager，停止音频采集和发送
        await audioManagerRef.current
          .stop()
          .then(manager => manager.configure({ enabled: false }));

        // console.debug("[APP] VAD audio processing stopped");
        // 批量更新状态，减少重渲染次数
        setIsUserSpeaking(false);
        setSpeechDetected(false);
        setSpeechSegmentCount(0);
        setTalkHint('');

      } catch (error) {
        console.error('[APP] Error stopping VAD processing:', error);
        setTalkHint('停止失败：请刷新重试');
      }
    }
  }, [isUserSpeaking, vadThreshold, setIsUserSpeaking]);

  const record = useCallback(() => {
    // setIsUserSpeaking(!isUserSpeaking)
    toggleVadProcessing()
  }, [toggleVadProcessing]);

  // 处理图像选择
  const handleImageSelect = useCallback((event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (!file) return;

    // 验证文件类型
    const allowedTypes = ['image/jpeg', 'image/png', 'image/webp'];
    if (!allowedTypes.includes(file.type)) {
      alert('仅支持 JPEG、PNG、WebP 格式的图片');
      return;
    }

    // 验证文件大小（5MB限制）
    const maxSize = 5 * 1024 * 1024; // 5MB
    if (file.size > maxSize) {
      alert('图片文件大小不能超过 5MB');
      return;
    }

    setSelectedImage(file);

    // 创建预览
    const reader = new FileReader();
    reader.onload = (e) => {
      setImagePreview(e.target?.result as string);
    };
    reader.readAsDataURL(file);
  }, []);

  // 发送图像
  const handleSendImage = useCallback(async () => {
    if (!selectedImage || isImageUploading) return;

    // 检查连接状态
    const voiceChatCore = realTime.current?.getVoiceChatCore();
    if (!voiceChatCore?.isConnected) {
      alert('WebSocket未连接，请等待连接建立后再发送图像');
      return;
    }

    if (!voiceChatCore?.isSessionCreated) {
      alert('会话未创建，请先进行语音对话后再发送图像');
      return;
    }

    console.log('[IMAGE] 开始发送图像，会话ID:', voiceChatCore.currentSessionId);

    setIsImageUploading(true);
    try {
      // 将文件转换为 Uint8Array
      const arrayBuffer = await selectedImage.arrayBuffer();
      const uint8Array = new Uint8Array(arrayBuffer);

      console.log('[IMAGE] 图像数据准备完成，大小:', uint8Array.length, '字节');
      console.log('[IMAGE] 提示词:', imagePrompt || '(无提示词)');

      // 发送图像数据（使用协议ID 100 - All，包含提示词）
      const result = realTime.current?.sendImageData(uint8Array, imagePrompt, ProtocolId.All);

      if (result) {
        console.log(`[IMAGE] 图像发送成功: ${selectedImage.name}, 大小: ${selectedImage.size} 字节`);
        console.log('[IMAGE] 等待服务器视觉分析响应...');

        // 清理状态
        setSelectedImage(null);
        setImagePreview(null);
        setImagePrompt(''); // 清理提示词
        // 清理文件输入
        const fileInput = document.getElementById('imageInput') as HTMLInputElement;
        if (fileInput) {
          fileInput.value = '';
        }

        // 显示成功提示
        alert('图像已成功发送，请等待AI分析结果');
      } else {
        console.error('[IMAGE] 图像发送失败 - sendImageData返回false');
        alert('图像发送失败，请检查连接状态和会话状态');
      }
    } catch (error) {
      console.error('[IMAGE] 图像发送失败:', error);
      alert(`图像发送失败: ${error instanceof Error ? error.message : '未知错误'}`);
    } finally {
      setIsImageUploading(false);
    }
  }, [selectedImage, isImageUploading]);

  // 取消图像选择
  const handleCancelImage = useCallback(() => {
    setSelectedImage(null);
    setImagePreview(null);
    setImagePrompt(''); // 清理提示词
    const fileInput = document.getElementById('imageInput') as HTMLInputElement;
    if (fileInput) {
      fileInput.value = '';
    }
  }, []);

  // 切换图片输入框展开/折叠状态
  const toggleImageInputExpanded = useCallback(() => {
    setIsImageInputExpanded(prev => !prev);
  }, []);

  // 切换图片上传区域和响应文本显示区域的展开状态
  const toggleSecondarySections = useCallback(() => {
    setIsImageUploadSectionExpanded(prev => !prev);
  }, []);

  // // 音频传输控制
  // const startAudioTransmission = () => {
  //   if (transmissionTimerRef.current) return;

  //   const TICK_MS = 50; // 更高精度，带漂移校正
  //   nextSendAtRef.current = performance.now();

  //   const tick = async () => {
  //     // 注意：这里需要根据实际实现调整
  //     // await realTime.transmitAudioData(audioDataBufferRef.current);
  //     const now = performance.now();
  //     const next = (nextSendAtRef.current ?? now) + TICK_MS;
  //     nextSendAtRef.current = next;
  //     const delay = Math.max(0, next - now);
  //     transmissionTimerRef.current = window.setTimeout(tick, delay);
  //   };

  //   transmissionTimerRef.current = window.setTimeout(tick, 0);
  // };

  const stopAudioTransmission = () => {
    if (transmissionTimerRef.current) {
      window.clearTimeout(transmissionTimerRef.current);
      transmissionTimerRef.current = null;
    }
    nextSendAtRef.current = null;
  };



  useEffect(() => {
    realTime.current?.connectToServer()
    // 直接调用状态设置，避免依赖回调函数
    setIsPlayingDownstreamAudio(true);
    if (!audioPlayerRef.current) {
      audioPlayerRef.current = new AudioPlayer();
    }
  }, [])

  useEffect(() => {

    // 获取RealTime实例中的AudioManager
    audioManagerRef.current = realTime.current?.getAudioManager();

    // 设置事件监听器
    audioManagerRef.current?.on('speech:start', () => {
      // console.log('speech:start');
      setSpeechDetected(true);
    });

    audioManagerRef.current?.on('speech:end', (_speechAudio: Float32Array) => {
      // console.log('speech:end', speechAudio.length);
      setSpeechSegmentCount(prev => prev + 1); // 使用函数式更新避免闭包问题
      setSpeechDetected(false);

      // ========== VAD语音结束，发送 stopInput ==========
      const voiceChatCore = realTime.current?.getVoiceChatCore();
      if (voiceChatCore?.isConnected && voiceChatCore?.isSessionCreated) {
        // 计算 audio_end_ms
        const audioEndMs = Date.now() - audioStartTimeRef.current;
        const eventId = `event_${Date.now()}`;
        const itemId = `msg_${Date.now().toString(36)}`;

        console.log('[APP] VAD检测到语音结束，发送stopInput:', {
          eventId,
          audioEndMs,
          itemId,
          sessionId: voiceChatCore.currentSessionId
        });

        // 发送 stopInput 消息（JSON格式）
        voiceChatCore.sendStopInput(eventId, audioEndMs, itemId, false);
      } else {
        console.warn('[APP] 无法发送stopInput: 连接状态或会话状态不满足条件', {
          isConnected: voiceChatCore?.isConnected,
          isSessionCreated: voiceChatCore?.isSessionCreated
        });
      }
    });

    audioManagerRef.current?.on('audio:data', (_audioData: Float32Array) => {
      // suppressed
    });

    // 监听错误事件
    audioManagerRef.current?.on('error', (_error: Error) => {
      // suppressed
    });

    // 监听VAD配置变化
    audioManagerRef.current?.on('vad:enabled', (_enabled: boolean) => {
      // suppressed
    });

    audioManagerRef.current?.on('vad:threshold', (_threshold: number) => {
      // suppressed
    });

    // 监听VoiceChatCore的下行音频数据
    const voiceChatCore = realTime.current?.getVoiceChatCore();
    const handleAudioDataReceived = (payload: AudioChunkPayload) => {
      // payload 包含 type 和 data 字段
      // data 是 Base64 编码的音频数据
      // console.debug('[APP] Received downstream audio data:', payload.data.length, 'characters');
      // 播放下行音频数据
      if (audioPlayerRef.current && isPlayingDownstreamAudioRef.current) {
        // 需要将Base64字符串解码为Uint8Array
        const uint8Array = base64ToUint8Array(payload.data);
        audioPlayerRef.current.playAudioChunk(uint8Array);
      }
    };

    if (voiceChatCore) {
      voiceChatCore.on('audio_chunk', handleAudioDataReceived);

      // ========== 高精度性能监控监听器 ==========
      const connectionManager = voiceChatCore?.getConnectionManager();
      if (connectionManager) {
        console.log(`[DEBUG] 注册高精度性能监听器`);
        // 监听高精度延迟测量完成事件
        connectionManager.on('performance:latency:measured', (metric: {
          type: string;
          latency: number;
          startTime: number;
          endTime: number;
          timestamp: number;
        }) => {
          console.log(`[HIGH-PERF] 高精度延迟测量: ${metric.type} = ${metric.latency.toFixed(3)}ms`);

          // 更新UI显示 - 只有在性能监控开启时才处理
          // 优先处理 binary_audio_delta 事件，response.audio.delta 作为备用方案
          if ((metric.type === 'binary_audio_delta' || metric.type === 'response.audio.delta') && isPerformanceMonitorEnabledRef.current) {
            console.log(`[DEBUG] 高精度计时器触发，准备更新计数`);
            setResponseMetrics(prev => {
              console.log(`[DEBUG] 当前对话轮次: ${prev.totalInteractions} -> ${prev.totalInteractions + 1}`);
              const newAudioTimes = [...prev.audioResponseTimes, metric.latency].slice(-50);
              const avgAudio = newAudioTimes.reduce((sum: number, time: number) => sum + time, 0) / newAudioTimes.length;

              return {
                ...prev,
                audioResponseTimes: newAudioTimes,
                averageAudioLatency: avgAudio,
                lastAudioLatency: metric.latency,
                totalInteractions: prev.totalInteractions + 1,
                isTimingActive: false
              };
            });
          }
        });
      }

      const handlers = new Map<string, (payload: unknown) => void>();
      // 移除外部缓冲累积逻辑，直接逐块播放下行PCM
      // 创建统一的事件处理器
      const createEventHandler = (eventName: string) => (payload: unknown) => {
        // NOTE: noisy per-event log; uncomment only when debugging WS event stream ordering.
        // console.log('[type]:', eventName);

        switch (eventName) {
          case 'input_audio_buffer.speech_stopped': {
            // ========== 高精度响应延迟计时开始 ==========
            // 当收到 input_audio_buffer.speech_stopped 事件时，启动高精度计时器
            // 优先使用 binary_audio_delta 事件进行延迟计算，response.audio.delta 作为备用
            // 只有在性能监控开启时才进行计时
            if (!isPerformanceMonitorEnabledRef.current) return;

            const voiceChatCore = realTime.current?.getVoiceChatCore();
            const connectionManager = voiceChatCore?.getConnectionManager();

            if (connectionManager) {
              // 启动高精度计时器 - 优先针对二进制音频响应
              connectionManager.startHighPrecisionTimer('binary_audio_delta');
              console.log('[PERF MONITOR] 用户说话结束，高精度计时器已启动 - binary_audio_delta (优先)');
            }

            // 兼容性：保留原有计时方式
            speechStartTimeRef.current = performance.now();
            hasRecordedResponse.current = false;

            // 更新状态显示计时器已激活
            setResponseMetrics(prev => ({
              ...prev,
              isTimingActive: true
            }));
            break;
          }

          case 'response.created': {
            console.log('[TEXT] Response created, clearing current text');
            setCurrentText('');
            break;
          }

          case 'response.text.delta': {
            const textDeltaEvt = payload as ResponseTextDeltaEvent;
            if (!textDeltaEvt || typeof textDeltaEvt.delta !== 'string') return;
            console.log('[TEXT] Text delta received:', textDeltaEvt.delta);
            setCurrentText(prev => prev + textDeltaEvt.delta);
            break;
          }

          case 'response.text.done': {
            const textDoneEvt = payload as ResponseTextDoneEvent;
            if (!textDoneEvt || typeof textDoneEvt.text !== 'string') return;
            console.log('[TEXT] Text done received:', textDoneEvt.text);
            setFinalTexts(prev => [textDoneEvt.text, ...prev].slice(0, 20));
            setCurrentText('');
            // 收到服务器消息后折叠图片输入框
            setIsImageInputExpanded(false);
            break;
          }

          case 'binary_audio_delta': {
            // 处理新的二进制音频数据
            const binaryAudioData = payload as any;
            if (!binaryAudioData || !binaryAudioData.audioData) return;

            // NOTE: noisy per-chunk log; uncomment only when debugging downstream audio playback.
            // console.log('[APP] 🎵 收到二进制音频数据，准备播放');

            // ⚠️ 移除错误的自动打断逻辑
            // 流式音频应该连续推送到队列，而不是每次都清空
            // 只有在特定事件（如 output_audio_buffer.started）时才应该清空队列

            // ========== 响应延迟计算（二进制协议优先） ==========
            // 只有在性能监控开启时才执行
            // binary_audio_delta 事件优先于 response.audio.delta 进行延迟计算
            if (isPerformanceMonitorEnabledRef.current && speechStartTimeRef.current && !hasRecordedResponse.current) {
              const audioLatency = performance.now() - speechStartTimeRef.current;
              hasRecordedResponse.current = true; // 标记已记录响应，避免重复计算

              console.log(`[DEBUG] 二进制音频响应延迟 (binary_audio_delta): ${audioLatency.toFixed(2)}ms`);

              setResponseMetrics(prev => ({
                ...prev,
                isTimingActive: false // 确保计时状态正确
              }));
            }

            try {
              console.debug('[APP] 收到二进制音频数据:', {
                dataSize: binaryAudioData.audioData.length,
                responseId: binaryAudioData.responseId,
                itemId: binaryAudioData.itemId,
                outputIndex: binaryAudioData.outputIndex,
                contentIndex: binaryAudioData.contentIndex
              });

              // 二进制音频数据可能需要Opus解码
              const audioData = binaryAudioData.audioData;

              // 确保有音频数据
              if (!audioData || audioData.length === 0) {
                console.warn('[APP] 没有有效的音频数据，跳过播放');
                return;
              }

              // 调试：检查原始音频数据格式
              const audioSlice = Array.from(audioData.slice(0, 16)) as number[];
              const audioSlice100 = Array.from(audioData.slice(0, 100)) as number[];
              console.debug('[APP] 🔍 原始音频数据详情:', {
                dataSize: audioData.length,
                firstFewBytes: audioSlice,
                firstFewBytesHex: audioSlice.map(b => b.toString(16).padStart(2, '0')).join(' '),
                dataType: audioData.constructor.name,
                // 检查是否可能是Opus数据的特征
                possibleOpusSignature: (Array.from(audioData.slice(0, 8)) as number[]).map((b: number) => b.toString(16).padStart(2, '0')).join(' '),
                // 检查是否可能是PCM数据的特征（查看值范围）
                minByteValue: Math.min(...audioSlice100),
                maxByteValue: Math.max(...audioSlice100),
                avgByteValue: audioSlice100.reduce((a: number, b: number) => a + b, 0) / 100
              });

              let pcmBytes: Uint8Array;
              let decodingMethod = '';

              // 尝试Opus解码（二进制数据很可能仍然是Opus编码的）
              try {
                console.debug('[APP] 🎵 尝试对二进制音频数据进行Opus解码...');
                const decoded = audioProcessor.decodeOpus(audioData);
                if (decoded) {
                  pcmBytes = decoded;
                  decodingMethod = 'opus_decoded';
                  console.debug('[APP] ✅ 二进制Opus解码成功，PCM数据大小:', pcmBytes.length);

                  // 验证解码后的PCM数据特征
                  if (pcmBytes.length > 0) {
                    const dataView = new DataView(pcmBytes.buffer, pcmBytes.byteOffset, pcmBytes.byteLength);
                    const firstSample = dataView.getInt16(0, true); // 小端序
                    const secondSample = dataView.getInt16(2, true); // 小端序
                    console.debug('[APP] 🔊 解码后PCM样本值 (小端序):', {
                      firstSample,
                      secondSample,
                      firstSampleBigEndian: dataView.getInt16(0, false), // 大端序
                      secondSampleBigEndian: dataView.getInt16(2, false) // 大端序
                    });
                  }
                } else {
                  throw new Error('Opus解码返回null');
                }
              } catch (opusError) {
                console.warn('[APP] ❌ 二进制Opus解码失败，假设数据已经是PCM格式:', opusError);
                pcmBytes = audioData;
                decodingMethod = 'direct_pcm';

                // 验证直接作为PCM的数据特征
                if (pcmBytes.length >= 4) {
                  const dataView = new DataView(pcmBytes.buffer, pcmBytes.byteOffset, pcmBytes.byteLength);
                  const firstSampleLE = dataView.getInt16(0, true); // 小端序
                  const firstSampleBE = dataView.getInt16(0, false); // 大端序
                  console.debug('[APP] 🔊 直接PCM样本值对比:', {
                    firstSampleLittleEndian: firstSampleLE,
                    firstSampleBigEndian: firstSampleBE,
                    sampleDifference: Math.abs(firstSampleLE - firstSampleBE),
                    isLittleEndianReasonable: Math.abs(firstSampleLE) < 32768 && Math.abs(firstSampleLE) > 100,
                    isBigEndianReasonable: Math.abs(firstSampleBE) < 32768 && Math.abs(firstSampleBE) > 100
                  });
                }
              }

              // 调试：检查最终PCM数据格式
              console.debug('[APP] 📊 最终PCM数据详情:', {
                dataSize: pcmBytes.length,
                sampleCount: pcmBytes.length / 2, // PCM16 每个样本2字节
                firstFewBytes: Array.from(pcmBytes.slice(0, 16)),
                firstFewBytesHex: (Array.from(pcmBytes.slice(0, 16)) as number[]).map(b => b.toString(16).padStart(2, '0')).join(' '),
                isEvenLength: pcmBytes.length % 2 === 0,
                decodingMethod,
                // 音频数据质量检查
                audioQualityCheck: pcmBytes.length >= 4 ? (() => {
                  const dataView = new DataView(pcmBytes.buffer, pcmBytes.byteOffset, pcmBytes.byteLength);
                  const samples = [];
                  for (let i = 0; i < Math.min(10, pcmBytes.length / 2); i++) {
                    samples.push(dataView.getInt16(i * 2, true)); // 小端序
                  }
                  const maxSample = Math.max(...samples.map(Math.abs));
                  const avgSample = samples.reduce((a, b) => a + Math.abs(b), 0) / samples.length;
                  return {
                    maxAbsSample: maxSample,
                    avgAbsSample: avgSample,
                    isReasonableRange: maxSample <= 32767 && avgSample > 10,
                    sampleValues: samples.slice(0, 5)
                  };
                })() : null
              });

              // 播放音频数据
              if (audioPlayerRef.current && isPlayingDownstreamAudioRef.current) {
                console.debug('[APP] 开始播放二进制音频数据:', pcmBytes.length, '字节');
                audioPlayerRef.current.playAudioChunk(pcmBytes);
              } else {
                console.warn('[APP] 音频播放器未就绪:', {
                  hasPlayer: !!audioPlayerRef.current,
                  isPlaying: isPlayingDownstreamAudioRef.current
                });
              }
            } catch (e) {
              console.error('[APP] 二进制音频处理失败:', e);
            }
            break;
          }

          case 'binary_audio_error': {
            // 处理二进制音频错误
            const errorData = payload as any;
            console.error('[APP] 二进制音频解码错误:', errorData);
            break;
          }

          case 'response.audio.delta': {
            // 保留对传统JSON音频的支持（向后兼容）
            // 作为 binary_audio_delta 的备用方案，当二进制协议不可用时使用
            const audioDeltaEvt = payload as ResponseAudioDeltaEvent;
            if (!audioDeltaEvt || typeof audioDeltaEvt.delta !== 'string') return;

            // ========== 响应延迟计算（JSON协议备用） ==========
            // 只有在性能监控开启时才执行
            // response.audio.delta 作为备用方案，当 binary_audio_delta 不可用时使用
            if (isPerformanceMonitorEnabledRef.current && speechStartTimeRef.current && !hasRecordedResponse.current) {
              const audioLatency = performance.now() - speechStartTimeRef.current;
              hasRecordedResponse.current = true; // 标记已记录响应，避免重复计算

              console.log(`[DEBUG] JSON音频响应延迟 (response.audio.delta - 备用方案): ${audioLatency.toFixed(2)}ms`);

              setResponseMetrics(prev => ({
                ...prev,
                isTimingActive: false // 确保计时状态正确
              }));
            }

            try {
              // 直接使用传统的JSON协议处理音频数据
              const opusBytes = base64ToUint8Array(audioDeltaEvt.delta);

              // 调试：检查JSON协议的原始音频数据格式
              const opusSlice = Array.from(opusBytes.slice(0, 16)) as number[];
              const opusSlice100 = Array.from(opusBytes.slice(0, 100)) as number[];
              console.debug('[APP] 🔍 JSON协议音频数据详情:', {
                dataSize: opusBytes.length,
                responseId: audioDeltaEvt.response_id,
                itemId: audioDeltaEvt.item_id,
                firstFewBytes: opusSlice,
                firstFewBytesHex: opusSlice.map(b => b.toString(16).padStart(2, '0')).join(' '),
                // 检查是否可能是Opus数据的特征
                possibleOpusSignature: (Array.from(opusBytes.slice(0, 8)) as number[]).map((b: number) => b.toString(16).padStart(2, '0')).join(' '),
                // 检查是否可能是PCM数据的特征（查看值范围）
                minByteValue: Math.min(...opusSlice100),
                maxByteValue: Math.max(...opusSlice100),
                avgByteValue: opusSlice100.reduce((a: number, b: number) => a + b, 0) / 100
              });

              let pcmBytes: Uint8Array;
              let decodingMethod = '';

              // 尝试Opus解码
              try {
                console.debug('[APP] 🎵 尝试对JSON音频数据进行Opus解码...');
                const decoded = audioProcessor.decodeOpus(opusBytes);
                if (decoded) {
                  pcmBytes = decoded;
                  decodingMethod = 'opus_decoded';
                  console.debug('[APP] ✅ JSON Opus解码成功，PCM数据大小:', pcmBytes.length);

                  // 验证解码后的PCM数据特征
                  if (pcmBytes.length > 0) {
                    const dataView = new DataView(pcmBytes.buffer, pcmBytes.byteOffset, pcmBytes.byteLength);
                    const firstSample = dataView.getInt16(0, true); // 小端序
                    const secondSample = dataView.getInt16(2, true); // 小端序
                    console.debug('[APP] 🔊 JSON解码后PCM样本值 (小端序):', {
                      firstSample,
                      secondSample,
                      firstSampleBigEndian: dataView.getInt16(0, false), // 大端序
                      secondSampleBigEndian: dataView.getInt16(2, false) // 大端序
                    });
                  }
                } else {
                  throw new Error('Opus解码返回null');
                }
              } catch (opusError) {
                console.warn('[APP] ❌ JSON Opus解码失败，使用原始数据:', opusError);
                pcmBytes = opusBytes;
                decodingMethod = 'direct_pcm';

                // 验证直接作为PCM的数据特征
                if (pcmBytes.length >= 4) {
                  const dataView = new DataView(pcmBytes.buffer, pcmBytes.byteOffset, pcmBytes.byteLength);
                  const firstSampleLE = dataView.getInt16(0, true); // 小端序
                  const firstSampleBE = dataView.getInt16(0, false); // 大端序
                  console.debug('[APP] 🔊 JSON直接PCM样本值对比:', {
                    firstSampleLittleEndian: firstSampleLE,
                    firstSampleBigEndian: firstSampleBE,
                    sampleDifference: Math.abs(firstSampleLE - firstSampleBE),
                    isLittleEndianReasonable: Math.abs(firstSampleLE) < 32768 && Math.abs(firstSampleLE) > 100,
                    isBigEndianReasonable: Math.abs(firstSampleBE) < 32768 && Math.abs(firstSampleBE) > 100
                  });
                }
              }

              // 调试：检查JSON协议最终PCM数据格式
              console.debug('[APP] 📊 JSON最终PCM数据详情:', {
                dataSize: pcmBytes.length,
                sampleCount: pcmBytes.length / 2, // PCM16 每个样本2字节
                firstFewBytes: Array.from(pcmBytes.slice(0, 16)),
                firstFewBytesHex: (Array.from(pcmBytes.slice(0, 16)) as number[]).map(b => b.toString(16).padStart(2, '0')).join(' '),
                isEvenLength: pcmBytes.length % 2 === 0,
                decodingMethod,
                // 音频数据质量检查
                audioQualityCheck: pcmBytes.length >= 4 ? (() => {
                  const dataView = new DataView(pcmBytes.buffer, pcmBytes.byteOffset, pcmBytes.byteLength);
                  const samples = [];
                  for (let i = 0; i < Math.min(10, pcmBytes.length / 2); i++) {
                    samples.push(dataView.getInt16(i * 2, true)); // 小端序
                  }
                  const maxSample = Math.max(...samples.map(Math.abs));
                  const avgSample = samples.reduce((a, b) => a + Math.abs(b), 0) / samples.length;
                  return {
                    maxAbsSample: maxSample,
                    avgAbsSample: avgSample,
                    isReasonableRange: maxSample <= 32767 && avgSample > 10,
                    sampleValues: samples.slice(0, 5)
                  };
                })() : null
              });

              // 确保有音频数据
              if (!pcmBytes || pcmBytes.length === 0) {
                console.warn('[APP] 没有有效的音频数据，跳过播放');
                return;
              }

              // 播放音频数据
              if (audioPlayerRef.current && isPlayingDownstreamAudioRef.current) {
                console.debug('[APP] 开始播放JSON音频数据:', pcmBytes.length, '字节');
                audioPlayerRef.current.playAudioChunk(pcmBytes);
              } else {
                console.warn('[APP] 音频播放器未就绪:', {
                  hasPlayer: !!audioPlayerRef.current,
                  isPlaying: isPlayingDownstreamAudioRef.current
                });
              }
            } catch (e) {
              console.error('[APP] JSON音频处理失败:', e);
            }
            break;
          }

          // 删除 started/stopped 的本地缓冲管理，保持最小化

          case 'output_audio_buffer.started': {
            const now = performance.now();
            const startedPayload = payload as any;
            console.log('[APP] 🟢 信令时序诊断 - output_audio_buffer.started:', {
              timestamp: now,
              hasExistingPlayer: !!audioPlayerRef.current,
              playerStatus: audioPlayerRef.current?.getQueueStatus(),
              sessionId: startedPayload.session_id,
              item_id: startedPayload.item_id
            });

            // 确保AudioPlayer实例已准备好
            if (!audioPlayerRef.current) {
              audioPlayerRef.current = new AudioPlayer();
              console.log('[APP] 🟢 为新的音频缓冲区创建了新的AudioPlayer实例');
            } else {
              // 🔍 诊断：检查是否存在音频流重叠
              const queueStatus = audioPlayerRef.current.getQueueStatus();
              const potentialOverlap = queueStatus.isPlaying && (now - queueStatus.lastClearTime) < 50;

              console.log('[APP] 🔍 音频流重叠检测:', {
                isPlaying: queueStatus.isPlaying,
                timeSinceLastClear: now - queueStatus.lastClearTime,
                clearCount: queueStatus.clearCount,
                potentialOverlap,
                action: potentialOverlap ? 'force_reset' : 'clear_queue'
              });

              if (potentialOverlap) {
                // 如果检测到重叠，使用强制重置
                audioPlayerRef.current.forceReset();
                console.log('[APP] 🔴 检测到音频流重叠，执行强制重置');
              } else {
                // 清理之前的播放状态，准备新的音频流
                audioPlayerRef.current.clearQueue();
                console.log('[APP] 🟢 清理现有AudioPlayer状态，准备新的音频流');
              }
            }
            break;
          }

          case 'response.audio.done':
          case 'output_audio_buffer.stopped': {
            const now = performance.now();
            const stoppedPayload = payload as any;
            console.log('[APP] 🔴 信令时序诊断 - output_audio_buffer.stopped:', {
              timestamp: now,
              hasExistingPlayer: !!audioPlayerRef.current,
              playerStatus: audioPlayerRef.current?.getQueueStatus(),
              sessionId: stoppedPayload.session_id,
              item_id: stoppedPayload.item_id
            });

            // 停止处理：如果下行播放开关仍为开启，则不要把实例置空（否则后续音频会进入“未就绪”状态）
            if (audioPlayerRef.current) {
              console.log('[APP] 🔴 调用stopPlayback()停止音频播放');
              audioPlayerRef.current.stopPlayback();
              if (!isPlayingDownstreamAudioRef.current) {
                audioPlayerRef.current = null;
                console.log('[APP] 🔴 AudioPlayer实例已清理（下行播放已关闭）');
              } else {
                // 保持实例，等待下一段音频流
                audioPlayerRef.current.clearQueue();
                console.log('[APP] 🔴 AudioPlayer已停止并清空队列（保持实例以便后续继续播放）');
              }
            } else {
              console.log('[APP] 🔴 AudioPlayer实例已为空，无需清理');
            }
            break;
          }

          // 🔧 修复：这些事件现在由正确的 output_audio_buffer.started/stopped 机制处理
          // 保留日志记录但移除错误的打断逻辑
          case 'response.cancel': {
            console.log('[APP] 📝 收到response.cancel事件（已由正确的信令机制处理）');
            // 不再执行打断操作，由 output_audio_buffer.stopped 处理
            break;
          }

          case 'output_audio_buffer.cleared': {
            console.log('[APP] 📝 收到output_audio_buffer.cleared事件（已由正确的信令机制处理）');
            // 不再执行打断操作，由 output_audio_buffer.stopped 处理
            break;
          }

          case 'conversation.item.truncated': {
            console.log('[APP] 📝 收到conversation.item.truncated事件（已由正确的信令机制处理）');
            // 不再执行打断操作，由 output_audio_buffer.stopped 处理
            break;
          }

          case 'response.function_call_arguments.done': {
            // 处理 function_call_arguments.done 事件，识别工具调用
            const functionCallPayload = payload as any;
            console.log('[ToolCall] 收到事件:', eventName, functionCallPayload);

            const fnName =
              functionCallPayload?.function_name ||
              functionCallPayload?.function?.name ||
              functionCallPayload?.name;
            const callId = functionCallPayload?.call_id;
            let parsedArgs: any = undefined;
            try {
              parsedArgs = JSON.parse(functionCallPayload.arguments || '{}');
            } catch (_e) {
              // ignore
            }

            // 根据实际日志，字段名是 function_name 而不是 function.name
            if (functionCallPayload?.function_name === 'reminder' || functionCallPayload?.function?.name === 'reminder') {
              console.log('[Reminder] ✅ 识别到 reminder 工具调用:', functionCallPayload);
              try {
                const args = JSON.parse(functionCallPayload.arguments || '{}');
                console.log('[Reminder] ✅ 解析参数:', args);
                const reminder = handleReminderCall(args);

                // ✅ 本地工具执行完毕后回传 function_call_output（闭环）
                if (typeof callId === 'string' && callId.length > 0) {
                  const voiceChatCore = realTime.current?.getVoiceChatCore();
                  const connectionManager = voiceChatCore?.getConnectionManager() as any;
                  connectionManager?.sendFunctionCallOutput?.(callId, {
                    status: 1,
                    reminderId: reminder?.id,
                    content: reminder?.content,
                    startAt: reminder?.startAt,
                  });
                } else {
                  console.warn('[Reminder] call_id 为空，跳过 function_call_output 回传');
                }
              } catch (error) {
                console.error('[Reminder] ❌ 参数解析失败:', error);
              }
            }

            // 处理拍照意图：兼容不同tool命名（take_photo / device_command 等）
            const argsStr = parsedArgs ? JSON.stringify(parsedArgs) : (functionCallPayload.arguments || '');
            const isTakePhotoIntent =
              fnName === 'take_photo' ||
              // 显式路由：服务端如果用 device_command 发拍照指令，也当作 take_photo
              fnName === 'device_command' ||
              (typeof callId === 'string' && callId.includes('take_photo')) ||
              (
                // 兜底：device_command 类工具里带“拍照”指令
                ((fnName && String(fnName).includes('device')) || (typeof callId === 'string' && callId.includes('device'))) &&
                (argsStr.includes('take_photo') || argsStr.includes('拍照') || argsStr.includes('camera'))
              );

            if (isTakePhotoIntent) {
              console.log('[Camera] ✅ 识别到拍照意图，准备打开摄像头:', { fnName, callId, parsedArgs });
              void openCamera();
              console.log('[Camera] 摄像头打开请求已发出（若移动端拦截，将出现“点一下授权/打开摄像头”）');

              // ✅ take_photo：本地工具触发后回传 function_call_output（闭环，避免服务端等待超时）
              if (typeof callId === 'string' && callId.length > 0) {
                if (!takePhotoAckedCallIdsRef.current.has(callId)) {
                  takePhotoAckedCallIdsRef.current.add(callId)
                  const voiceChatCore = realTime.current?.getVoiceChatCore();
                  const connectionManager = voiceChatCore?.getConnectionManager() as any;
                  connectionManager?.sendFunctionCallOutput?.(callId, {
                    status: 1,
                    action: 'take_photo',
                    result: 'camera_open_requested',
                  });
                }
              } else {
                console.warn('[Camera] call_id 为空，跳过 function_call_output 回传');
              }
            }
            break;
          }

          case 'response.function_call.done': {
            // 处理 function_call.done 事件，作为备用方案
            const functionCallPayload = payload as any;
            console.log('[ToolCall] 收到事件:', eventName, functionCallPayload);

            const fnName =
              functionCallPayload?.function_name ||
              functionCallPayload?.function?.name ||
              functionCallPayload?.name;
            const callId = functionCallPayload?.call_id;
            let parsedArgs: any = undefined;
            try {
              parsedArgs = JSON.parse(functionCallPayload.arguments || '{}');
            } catch (_e) {
              // ignore
            }

            if (fnName === 'reminder') {
              console.log('[Reminder] ✅ 识别到 reminder 工具调用 (function_call.done):', functionCallPayload);
              try {
                const args = JSON.parse(functionCallPayload.arguments || '{}');
                console.log('[Reminder] ✅ 解析参数:', args);
                const reminder = handleReminderCall(args);

                // ✅ 本地工具执行完毕后回传 function_call_output（闭环）
                if (typeof callId === 'string' && callId.length > 0) {
                  const voiceChatCore = realTime.current?.getVoiceChatCore();
                  const connectionManager = voiceChatCore?.getConnectionManager() as any;
                  connectionManager?.sendFunctionCallOutput?.(callId, {
                    status: 1,
                    reminderId: reminder?.id,
                    content: reminder?.content,
                    startAt: reminder?.startAt,
                  });
                } else {
                  console.warn('[Reminder] call_id 为空，跳过 function_call_output 回传');
                }
              } catch (error) {
                console.error('[Reminder] ❌ 参数解析失败:', error);
              }
            }

            // 处理拍照意图（备用）：function_call.done 有时不带 function_name，只能靠 call_id / arguments 兜底
            const argsStr = parsedArgs ? JSON.stringify(parsedArgs) : (functionCallPayload.arguments || '');
            const isTakePhotoIntent =
              fnName === 'take_photo' ||
              // 显式路由：服务端如果用 device_command 发拍照指令，也当作 take_photo
              fnName === 'device_command' ||
              (typeof callId === 'string' && callId.includes('take_photo')) ||
              (
                ((fnName && String(fnName).includes('device')) || (typeof callId === 'string' && callId.includes('device'))) &&
                (argsStr.includes('take_photo') || argsStr.includes('拍照') || argsStr.includes('camera'))
              );
            if (isTakePhotoIntent) {
              console.log('[Camera] ✅ 识别到拍照意图(备用)，准备打开摄像头:', { fnName, callId, parsedArgs });
              void openCamera();
              console.log('[Camera] 摄像头打开请求已发出（若移动端拦截，将出现“点一下授权/打开摄像头”）');

              // ✅ take_photo：本地工具触发后回传 function_call_output（闭环，避免服务端等待超时）
              if (typeof callId === 'string' && callId.length > 0) {
                if (!takePhotoAckedCallIdsRef.current.has(callId)) {
                  takePhotoAckedCallIdsRef.current.add(callId)
                  const voiceChatCore = realTime.current?.getVoiceChatCore();
                  const connectionManager = voiceChatCore?.getConnectionManager() as any;
                  connectionManager?.sendFunctionCallOutput?.(callId, {
                    status: 1,
                    action: 'take_photo',
                    result: 'camera_open_requested',
                  });
                }
              } else {
                console.warn('[Camera] call_id 为空，跳过 function_call_output 回传');
              }
            }
            break;
          }

          default: {
            // 通用事件监听，用于调试
            if (eventName.includes('text') || eventName.includes('response.created') || eventName.includes('cancel') || eventName.includes('cleared') || eventName.includes('truncated')) {
              console.log('[DEBUG] Event received:', eventName, payload);
            }
            // 其他事件默认记录（保持可观察性）
            // console.log('[DOWNSTREAM]', eventName, payload);
            break;
          }
        }
      };

      // 注册所有事件处理器
      downstreamEvents.forEach((eventName) => {
        const handler = createEventHandler(eventName);
        handlers.set(eventName, handler);
        voiceChatCore.on(eventName, handler);
      });

      // 添加通用事件监听器，用于调试所有收到的事件
      voiceChatCore.on('*', (eventName: string, payload: any) => {
        if (eventName.includes('function_call') || eventName.includes('reminder')) {
          console.log('[DEBUG] 🔔 收到相关事件:', eventName, JSON.stringify(payload).substring(0, 200));
        }
      });


      // 清理
      return () => {
        console.log(`[DEBUG] 清理事件监听器`);
        audioManagerRef.current?.removeAllListeners?.();
        stopAudioTransmission();
        voiceChatCore.off('audio_chunk', handleAudioDataReceived);
        handlers.forEach((h, name) => voiceChatCore.off(name, h));

        // 清理高精度性能监听器
        if (connectionManager) {
          connectionManager.removeAllListeners('performance:latency:measured');
          console.log(`[DEBUG] 已清理高精度性能监听器`);
        }

        // 关闭摄像头
        closeCamera();
        console.log(`[DEBUG] 摄像头已关闭`);
      };
    }
    // 提供全局函数来添加日志

    return undefined;
  }, []); // 只在组件挂载时执行一次

  // 单独处理音频播放状态变化
  useEffect(() => {
    // 确保audioPlayer的状态正确初始化
    if (isPlayingDownstreamAudio) {
      if (!audioPlayerRef.current) {
        audioPlayerRef.current = new AudioPlayer();
        console.log('[AUDIO] AudioPlayer 初始化完成');
      }
    } else {
      if (audioPlayerRef.current) {
        audioPlayerRef.current.stopPlayback();
        audioPlayerRef.current = null;
        console.log('[AUDIO] AudioPlayer 已停止并清理');
      }
    }
  }, [isPlayingDownstreamAudio]);

  // 处理照片捕获事件
  useEffect(() => {
    const handlePhotoCaptured = async (event: Event) => {
      const blob = (event as CustomEvent).detail as Blob;
      if (!blob) {
        console.error('[CAMERA] 捕获的照片为空');
        return;
      }

      // 拍完照自动下载刚刚的图片（最佳努力：某些浏览器策略可能会拦截非用户手势下载）
      try {
        const ext = (() => {
          switch (blob.type) {
            case 'image/jpeg': return 'jpg';
            case 'image/png': return 'png';
            case 'image/webp': return 'webp';
            default: return 'jpg';
          }
        })();
        const filename = `photo-${Date.now()}.${ext}`;
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = filename;
        a.style.display = 'none';
        document.body.appendChild(a);
        a.click();
        a.remove();
        URL.revokeObjectURL(url);
      } catch {
        // ignore
      }

      console.log('[CAMERA] 收到捕获的照片，大小:', blob.size, '字节');

      // 检查连接状态
      const voiceChatCore = realTime.current?.getVoiceChatCore();
      if (!voiceChatCore?.isConnected) {
        console.error('[CAMERA] WebSocket未连接');
        alert('WebSocket未连接，无法发送照片');
        return;
      }

      if (!voiceChatCore?.isSessionCreated) {
        console.error('[CAMERA] 会话未创建');
        alert('会话未创建，无法发送照片');
        return;
      }

      console.log('[CAMERA] 连接状态正常，开始发送照片...');

      try {
        // 将 Blob 转换为 Uint8Array
        const arrayBuffer = await blob.arrayBuffer();
        const uint8Array = new Uint8Array(arrayBuffer);

        console.log('[CAMERA] 照片数据准备完成，大小:', uint8Array.length, '字节');

        // 发送照片数据（使用协议ID 100 - All，无提示词）
        const result = realTime.current?.sendImageData(uint8Array, '', ProtocolId.All);

        if (result) {
          console.log('[CAMERA] ✅ 照片发送成功，大小:', blob.size, '字节');
          console.log('[CAMERA] 📡 等待服务端响应...');
          // alert('照片已成功发送，请等待AI分析结果');
        } else {
          console.error('[CAMERA] ❌ 照片发送失败');
          alert('照片发送失败，请检查连接状态');
        }
      } catch (error) {
        console.error('[CAMERA] ❌ 照片处理失败:', error);
        alert(`照片处理失败: ${error instanceof Error ? error.message : '未知错误'}`);
      }
    };

    window.addEventListener('photoCaptured', handlePhotoCaptured);

    return () => {
      window.removeEventListener('photoCaptured', handlePhotoCaptured);
    };
  }, []);

  // 获取延迟颜色类名的函数 - 使用 useMemo 优化
  const getLatencyColor = useMemo(() =>
    (latency: number): string => {
      if (latency === 0) return '';
      if (latency < 1000) return styles.excellent;
      if (latency < 2000) return styles.good;
      return styles.poor;
    }, []);

  const [isWsConnected, setIsWsConnected] = useAtom(wsConnectedAtom);
  const [isSessionCreated, setIsSessionCreated] = useAtom(sessionCreatedAtom);

  // 更新会话状态
  const updateSessionStatus = useCallback(() => {
    const voiceChatCore = realTime.current?.getVoiceChatCore();
    if (voiceChatCore) {
      const newStatus = {
        connected: voiceChatCore.isConnected,
        sessionCreated: voiceChatCore.isSessionCreated
      };
      // console.log('[DEBUG] 会话状态更新:', newStatus);
      setSessionStatus(newStatus);
      setIsSessionCreated(voiceChatCore.isSessionCreated);
      setCurrentSessionId(voiceChatCore.currentSessionId ?? '');
    } else {
      console.log('[DEBUG] VoiceChatCore 实例不可用');
    }
  }, [setIsSessionCreated]);

  const applySessionId = useCallback(() => {
    const trimmed = sessionIdInput.trim();
    if (!trimmed) return;

    const voiceChatCore = realTime.current?.getVoiceChatCore();
    if (!voiceChatCore?.isConnected) {
      alert('WebSocket未连接，无法切换sessionId');
      return;
    }

    console.log('[SESSION] switch', { from: voiceChatCore.currentSessionId, to: trimmed });
    resetPerformanceMonitor('session_switch');
    voiceChatCore.getConnectionManager().recreateSession(trimmed);
    setSessionIdInput('');
  }, [resetPerformanceMonitor, sessionIdInput]);

  const applyVoicePreset = useCallback((nextPreset: VoicePreset) => {
    setVoicePreset(nextPreset);
    try {
      localStorage.setItem(VOICE_PRESET_STORAGE_KEY, nextPreset);
      // 兼容旧 key：保持同步，避免其它地方还在读
      localStorage.setItem(LEGACY_USE_CUSTOM_VOICE_STORAGE_KEY, nextPreset === 'soft' ? '1' : '0');
    } catch { /* ignore */ }

    const voice_setting = nextPreset === 'soft'
      ? { voice_id: CUSTOM_VOICE_ID, speed: 1.0, pitch: 0, vol: 1.0 }
      : undefined; // 默认：不发 voice_setting，让服务端走默认音色

    // 1) 更新默认 config（下次连接也会带上/不带 voice_setting）
    realTime.current?.setSessionConfig?.({ voice_setting });

    // 2) 已连接：强制新 sessionId 立即生效
    const voiceChatCore = realTime.current?.getVoiceChatCore?.();
    if (voiceChatCore?.isConnected) {
      voiceChatCore.startNewSession?.({ voice_setting });
    }
  }, []);

  useEffect(() => {
    const voiceChatCore: VoiceChatCore = realTime.current?.getVoiceChatCore();

    // 监听WebSocket原生事件
    const handleOpen = () => {
      // 连接成功时，connectionStatusChange事件应该已经更新了状态
      setIsWsConnected(true);
      updateSessionStatus();
    };

    const handleClose = () => {
      setIsWsConnected(false);
      updateSessionStatus();
    };

    const handleError = () => {
      updateSessionStatus();
    };

    // 监听会话创建事件
    const handleSessionCreated = () => {
      updateSessionStatus();
    };

    // 注册事件监听器
    // voiceChatCore.on('connectionStatusChange', handleConnectionStatusChange);
    voiceChatCore.on('open', handleOpen);
    voiceChatCore.on('close', handleClose);
    voiceChatCore.on('error', handleError);
    voiceChatCore.on('session.created', handleSessionCreated);

    // 清理函数
    return () => {
      // voiceChatCore.off('connectionStatusChange', handleConnectionStatusChange);
      voiceChatCore.off('open', handleOpen);
      voiceChatCore.off('close', handleClose);
      voiceChatCore.off('error', handleError);
      voiceChatCore.off('session.created', handleSessionCreated);
    };
  }, [realTime, setIsWsConnected, updateSessionStatus]);

  // 定期更新会话状态
  useEffect(() => {
    const interval = setInterval(updateSessionStatus, 1000); // 每秒更新一次
    return () => clearInterval(interval);
  }, [updateSessionStatus]);

  return (
    <div className={styles.container}>
      <div className={styles.mainSection}>
        {/* 实时响应速度监控显示 */}
        <div className={`${styles.performanceMonitor} ${isPerformanceMonitorVisible ? styles.visible : styles.hidden}`}>
          <div className={styles.performanceHeader}>
            <h4>实时响应监控</h4>
            <div className={styles.headerButtons}>
              <button
                className={`${styles.toggleButton} ${isPerformanceMonitorEnabled ? styles.enabled : styles.disabled}`}
                onClick={togglePerformanceMonitor}
                title={isPerformanceMonitorEnabled ? "关闭性能监控" : "开启性能监控"}
                type="button"
              >
                {isPerformanceMonitorEnabled ? "⏸" : "▶"}
              </button>
              <button
                className={styles.toggleButton}
                onClick={togglePerformanceVisibility}
                title="展开/收起"
                type="button"
              >
                {isPerformanceMonitorVisible ? '−' : '+'}
              </button>
            </div>
          </div>

          {isPerformanceMonitorVisible && (
            <div className={styles.performanceContent}>
              <div className={styles.metricsGrid}>
                <div className={styles.metric}>
                  <span className={styles.metricLabel}>平均音频延迟</span>
                  <span className={`${styles.metricValue} ${getLatencyColor(responseMetrics.averageAudioLatency)}`}>
                    {responseMetrics.averageAudioLatency > 0
                      ? `${responseMetrics.averageAudioLatency.toFixed(0)}ms`
                      : '--'}
                  </span>
                </div>

                <div className={styles.metric}>
                  <span className={styles.metricLabel}>最近音频延迟</span>
                  <span className={`${styles.metricValue} ${getLatencyColor(responseMetrics.lastAudioLatency)}`}>
                    {responseMetrics.lastAudioLatency > 0
                      ? `${responseMetrics.lastAudioLatency.toFixed(0)}ms`
                      : '--'}
                  </span>
                </div>
                <div className={styles.metric}>
                  <span className={styles.metricLabel}>对话轮次</span>
                  <span className={styles.metricValue}>
                    {responseMetrics.totalInteractions}
                  </span>
                </div>

                <div className={styles.metric}>
                  <span className={styles.metricLabel}>计时精度</span>
                  <span className={`${styles.metricValue} ${responseMetrics.isTimingActive ? styles.timing : styles.excellent}`}>
                    {responseMetrics.isTimingActive ? '高精度计时中...' : '1ms精度'}
                  </span>
                </div>
              </div>

              {/* 响应时间趋势图 */}
              {responseMetrics.audioResponseTimes.length > 1 && (
                <TrendChart responseMetrics={responseMetrics} />
              )}
            </div>
          )}
        </div>
        <div className={styles.squadyUIContainer}>
          <LiveAudioVisualizerComponent pause={!isUserSpeaking} />
          <div
            className={styles.squadyUI}
            onPointerDown={(e) => {
              e.preventDefault();
              record();
            }}
          >
            <img src={squadyLogo} alt="squady" />
          </div>
          <div className={styles.squadyUIText}>
            <div>Squady</div>
            <StatusPoint />
          </div>
          <div className={styles.squadySubtext}>
            {talkHint || (isUserSpeaking ? '正在聆听...' : '点击开始对话')}
          </div>
        </div>

        <div className={styles.wsConfigBar}>
          <div className={styles.wsConfigRow}>
            <span className={styles.wsConfigLabel}>WS</span>
            <select
              value={wsPresetId}
              onChange={async (e) => {
                const id = e.target.value as WsPresetId;
                setWsPresetId(id);
                const preset = wsPresets.find(p => p.id === id);
                if (preset && preset.id !== 'custom') {
                  await applyWebSocketUrl(preset.url, id);
                }
              }}
              disabled={isSwitchingWs}
              className={styles.wsConfigSelect}
            >
              {wsPresets.map(p => (
                <option key={p.id} value={p.id}>{p.label}</option>
              ))}
            </select>

            {wsPresetId === 'custom' && (
              <input
                value={wsInput}
                onChange={(e) => setWsInput(e.target.value)}
                onKeyDown={async (e) => {
                  if (e.key === 'Enter') {
                    await applyWebSocketUrl(wsInput, 'custom');
                  }
                }}
                placeholder="ws://host:port/path"
                disabled={isSwitchingWs}
                className={styles.wsConfigInput}
              />
            )}

            {wsPresetId === 'custom' && (
              <button
                type="button"
                onClick={async () => applyWebSocketUrl(wsInput, 'custom')}
                disabled={isSwitchingWs}
                className={styles.wsConfigButton}
              >
                Apply
              </button>
            )}
          </div>
          <div className={styles.wsConfigHint} title={wsUrl}>
            当前: {wsUrl}
          </div>

          <div className={styles.wsConfigHint} title={currentSessionId || undefined}>
            Session: {currentSessionId || '--'}
          </div>

          <div className={styles.wsConfigRow}>
            <span className={styles.wsConfigLabel}>Session</span>
            <input
              value={sessionIdInput}
              onChange={(e) => setSessionIdInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') applySessionId();
              }}
              placeholder="输入 sessionId（如16位）"
              className={styles.wsConfigInput}
              disabled={isSwitchingWs}
            />
            <button
              type="button"
              onClick={applySessionId}
              disabled={isSwitchingWs}
              className={styles.wsConfigButton}
            >
              Apply
            </button>
          </div>

          <div className={styles.wsConfigRow}>
            <span className={styles.wsConfigLabel}>Voice</span>
            <label style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
              <input
                type="checkbox"
                checked={voicePreset === 'default'}
                disabled={isSwitchingWs}
                onChange={(e) => { if (e.target.checked) applyVoicePreset('default'); }}
              />
              默认
            </label>
            <label style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
              <input
                type="checkbox"
                checked={voicePreset === 'soft'}
                disabled={isSwitchingWs}
                onChange={(e) => { if (e.target.checked) applyVoicePreset('soft'); }}
              />
              轻柔音色
            </label>
          </div>
        </div>



      </div>

      <div className={styles.secondary}>
        {/* 图像上传区域 */}
        <div className={styles.imageUploadSection}>
          {/* 切换按钮 */}
          <button
            onClick={toggleSecondarySections}
            className={styles.sectionToggle}
            title={isImageUploadSectionExpanded ? "切换到响应显示" : "切换到图片上传"}
          >
            {isImageUploadSectionExpanded ? "✉️" : "📷"}

          </button>

          {/* 图片输入框内容 */}
          <div className={`${styles.imageInputContent} ${isImageUploadSectionExpanded ? styles.expanded : styles.collapsed}`}>
            {/* 图片输入框切换按钮 */}
            {/* <button
              onClick={toggleImageInputExpanded}
              className={styles.imageInputToggle}
              title={isImageInputExpanded ? "折叠图片输入" : "展开图片输入"}
            >
              {isImageInputExpanded ? "▲" : "▼"}
            </button> */}

            <input
              id="imageInput"
              type="file"
              accept="image/jpeg,image/png,image/webp"
              onChange={handleImageSelect}
              style={{ display: 'none' }}
              disabled={!sessionStatus.connected || !sessionStatus.sessionCreated}
            />

            {!selectedImage ? (
              <div className={styles.imgLabelContainer}>
                <label
                  htmlFor="imageInput"
                  className={`${styles.imageUploadButton} ${(!sessionStatus.connected || !sessionStatus.sessionCreated) ? styles.disabled : ''}`}
                >
                  📷 选择图片
                </label>

                {(!sessionStatus.connected || !sessionStatus.sessionCreated) && (
                  <div className={styles.uploadTip}>
                    请先进行语音对话以建立会话
                  </div>
                )}
              </div>
            ) : (
              <div className={styles.imagePreviewContainer}>
                {imagePreview && (
                  <img
                    src={imagePreview}
                    alt="预览"
                    className={styles.imagePreview}
                  />
                )}

                {/* 提示词输入框 - 只在选择图片后显示 */}
                <div className={styles.promptInputContainer}>
                  <textarea
                    placeholder="可选：输入额外的提示词或问题（留空则使用最后一轮对话内容）"
                    value={imagePrompt}
                    onChange={(e) => setImagePrompt(e.target.value)}
                    className={styles.promptInput}
                    rows={3}
                    disabled={!sessionStatus.connected || !sessionStatus.sessionCreated}
                  />
                  <div className={styles.imageActions}>
                    <span className={styles.imageInfo}>
                      {selectedImage.name} ({(selectedImage.size / 1024 / 1024).toFixed(2)} MB)
                    </span>

                    <div className={styles.imageButtons}>
                      <button
                        onClick={handleSendImage}
                        disabled={isImageUploading}
                        className={styles.sendButton}
                      >
                        {isImageUploading ? '发送中...' : '发送图片'}
                      </button>
                      <button
                        onClick={handleCancelImage}
                        className={styles.cancelButton}
                      >
                        取消
                      </button>
                    </div>
                  </div>
                </div>


              </div>
            )}
          </div>

          {/* 服务器文本响应显示区域 */}
          <div className={`${styles.responseTextDisplay} ${!isImageUploadSectionExpanded ? styles.expanded : styles.collapsed}`}>
            <div className={styles.responseContentWrapper}>
              <ReactMarkdown>
                {currentText || finalTexts[0] || '服务器响应信息'}
              </ReactMarkdown>
            </div>
          </div>
        </div>
      </div>

      {/* 视觉功能测试面板 (开发调试用) */}
      {/* <VisionTestPanel
        realTime={realTime.current}
        sessionStatus={sessionStatus}
      /> */}

      {/* PWA安装提示 */}
      <PWAInstallPrompt />

      {/* 提醒面板 */}
      <ReminderPanel />
    </div>
  )
}
