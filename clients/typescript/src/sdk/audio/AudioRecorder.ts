import { speechFilter, preloadModel } from '@steelbrain/media-speech-detection-web';
import { float32ToPcm16 } from './AudioUtils';
import { AUDIO_QUALITY } from "../protocol/ClientProtocol";

// Debug-only counters to correlate start/stop and underlying AudioWorklet lifecycle.
let __audioRecorderRunSeq = 0;
let __ingestStreamSeq = 0;

export class AudioRecorder {
    private mediaStream: MediaStream | null = null;
    private vadTransform: TransformStream<Float32Array, Float32Array> | null = null;
    // 移除未使用的旧管道句柄，避免无用字段
    // private speechProcessor: WritableStream<Float32Array> | null = null;

    // VAD控制相关属性
    private vadEnabled: boolean = false;
    private onSpeechStartCallback: (() => void) | null = null;
    private onSpeechEndCallback: ((speechAudio: Float32Array) => void) | null = null;
    private vadThreshold: number = 0.52;  // 使用推荐的阈值

    // 音频数据回调，供外部收集音频
    private onAudioDataCallback: ((audioData: Float32Array) => void) | null = null;

    // 由 VAD 事件驱动的门控标志（仅当启用 VAD 时生效）
    private isSpeaking: boolean = false;

    // 复用的 PCM S16LE 输出缓冲（固定 40ms@16kHz -> 1280 字节）
    private pcmOutBuffer: Uint8Array = new Uint8Array(AUDIO_QUALITY.FIXED_CHUNK_SIZE);

    // 自定义回溯窗口（毫秒），在编码分支通过环形缓冲实现
    private lookBackDurationMs: number = 384;
    private preSpeechBuffer: Float32Array[] = [];
    private needFlushPreBuffer: boolean = false;
    private readonly frameDurationMs: number = (AUDIO_QUALITY.FIXED_CHUNK_SIZE / 2) / AUDIO_QUALITY.SAMPLE_RATE * 1000; // 40ms

    // WebRTC 回声消除选项
    private webrtcOptions: WebRTCAudioOptions = {
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true
    };

    // 模型预加载状态
    private modelPreloaded: boolean = false;

    // Debug: correlate a "start -> stop" cycle (one user click start).
    private currentRunId: number | null = null;

    // Stop must actually cancel the pipeline; otherwise the ReadableStream won't cancel
    // and ingestAudioStream() cleanup (AudioContext.close, revokeObjectURL, etc) will never run.
    private pipelineAbort: AbortController | null = null;

    // 明确无需自定义初始化逻辑，移除空构造器

    /**
     * 预加载 VAD 模型（页面打开时调用，避免用户点击时等待 WASM 加载）
     * @returns Promise<void>
     */
    async preloadModel(): Promise<void> {
        if (this.modelPreloaded) {
            console.log('[AudioRecorder] VAD 模型已预加载，跳过');
            return;
        }

        const isIsolated = (globalThis as unknown as { crossOriginIsolated?: boolean }).crossOriginIsolated === true;
        if (!isIsolated) {
            console.log('[AudioRecorder] 非跨域隔离环境，跳过模型预加载（按需加载）');
            return;
        }

        try {
            console.log('[AudioRecorder] 开始预加载 VAD 模型...');
            await preloadModel();
            this.modelPreloaded = true;
            console.log('[AudioRecorder] VAD 模型预加载完成');
        } catch (error) {
            console.error('[AudioRecorder] VAD 模型预加载失败:', error);
            // 预加载失败不阻塞，后续按需加载
        }
    }

    /**
     * 启用或禁用VAD功能
     * @param enable 是否启用VAD
     */
    enableVAD(enable: boolean): void {
        this.vadEnabled = enable;
    }

    /**
     * 设置VAD阈值
     * @param threshold 阈值 (0-1)
     */
    setVADThreshold(threshold: number): void {
        this.vadThreshold = threshold;
    }

    /**
     * 设置VAD语音开始事件回调
     * @param callback 回调函数
     */
    setOnSpeechStartCallback(callback: (() => void) | null): void {
        this.onSpeechStartCallback = callback;
    }

    /**
     * 设置VAD语音结束事件回调
     * @param callback 回调函数
     */
    setOnSpeechEndCallback(callback: ((speechAudio: Float32Array) => void) | null): void {
        this.onSpeechEndCallback = callback;
    }

    /**
     * 设置音频数据回调，用于外部收集原始音频数据
     * @param callback 回调函数
     */
    setOnAudioDataCallback(callback: ((audioData: Float32Array) => void) | null): void {
        this.onAudioDataCallback = callback;
    }

    /**
     * 设置回溯时长（毫秒）。实际回溯由工作线程的环形缓冲实现。
     */
    setLookBackDurationMs(ms: number): void {
        const clamped = Math.max(0, Math.floor(ms));
        this.lookBackDurationMs = clamped;
    }

    /**
     * 配置 WebRTC 音频处理选项
     * @param options WebRTC 音频选项
     */
    configureWebRTCAudio(options: Partial<WebRTCAudioOptions>): void {
        this.webrtcOptions = { ...this.webrtcOptions, ...options };
    }

    /**
     * 设置回声消除状态
     * @param enabled 是否启用回声消除
     */
    setEchoCancellation(enabled: boolean): void {
        this.webrtcOptions.echoCancellation = enabled;
    }

    /**
     * 设置噪声抑制状态
     * @param enabled 是否启用噪声抑制
     */
    setNoiseSuppression(enabled: boolean): void {
        this.webrtcOptions.noiseSuppression = enabled;
    }

    /**
     * 设置自动增益控制状态
     * @param enabled 是否启用自动增益控制
     */
    setAutoGainControl(enabled: boolean): void {
        this.webrtcOptions.autoGainControl = enabled;
    }

    /**
     * 获取当前 WebRTC 音频配置
     * @returns 当前的 WebRTC 音频选项
     */
    getWebRTCConfig(): WebRTCAudioOptions {
        return { ...this.webrtcOptions };
    }

    /**
     * 获取带有 WebRTC 回声消除功能的音频流
     * @param deviceId 可选的设备ID
     * @returns 音频流
     */
    async getAudioStream(deviceId?: string): Promise<ReadableStream<Float32Array>> {
        // 检查是否支持mediaDevices API
        if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) {
            throw new Error('浏览器不支持getUserMedia API');
        }

        // 使用 WebRTC 回声消除功能创建音频流
        // 不指定采样率，让系统自动检测设备支持的最佳采样率
        // 但指定目标采样率为16kHz，这样会自动重采样
        const { mediaStream, audioStream } = await createWebRTCAudioStream(deviceId, {
            ...this.webrtcOptions,
            targetSampleRate: 16000,
        });

        this.mediaStream = mediaStream;
        return audioStream;
    }

    /**
     * 创建VAD过滤器转换流
     * @param options VAD选项
     * @returns 转换流
     */
    createVADTransform(options: {
        onSpeechStart?: () => void,
        onSpeechEnd?: (speechAudio: Float32Array) => void,
        onMisfire?: () => void,
        threshold?: number,
        noEmit?: boolean
    }): TransformStream<Float32Array, Float32Array> {
        return speechFilter({
            onSpeechStart: options.onSpeechStart,
            onSpeechEnd: options.onSpeechEnd,
            onMisfire: options.onMisfire,
            threshold: options.threshold || 0.5,
            noEmit: options.noEmit !== undefined ? options.noEmit : false,
        });
    }

    /**
     * 开始VAD音频处理
     * @param deviceId 可选的设备ID
     * @param audioDataHandler 音频数据处理回调
     */
    async startVADProcessing(
        deviceId: string | undefined,
        audioDataHandler: (audioData: Uint8Array) => void
    ): Promise<void> {
        // Defensive: avoid stacking multiple pipelines on repeated start/stop clicks.
        this.stopVADProcessing();

        this.currentRunId = ++__audioRecorderRunSeq;
        const runId = this.currentRunId;
        console.log('[AudioRecorder] startVADProcessing', {
            runId,
            deviceId: deviceId ?? null,
            vadEnabled: this.vadEnabled,
            lookBackDurationMs: this.lookBackDurationMs,
            frameDurationMs: this.frameDurationMs,
            threshold: this.vadThreshold,
        });

        if (!this.vadEnabled) {
            this.vadEnabled = true;
        }

        // 启动前按需预加载模型（懒加载），失败不阻塞
        const isIsolated = (globalThis as unknown as { crossOriginIsolated?: boolean }).crossOriginIsolated === true;
        console.log('[AudioRecorder] startVADProcessing env', { runId, crossOriginIsolated: isIsolated });
        if (isIsolated) {
            await preloadModel();
        }

        // 获取音频流
        const audioStream = await this.getAudioStream(deviceId);
        console.log('[AudioRecorder] got audio stream', {
            runId,
            hasMediaStream: !!this.mediaStream,
            trackCount: this.mediaStream?.getTracks().length ?? 0,
        });

        // Pipe abort controller (critical for releasing AudioContext/Worklet on stop)
        const abort = new AbortController();
        this.pipelineAbort = abort;

        // 创建 VAD 转换流（仅做判定，不改动分片）
        this.vadTransform = this.createVADTransform({
            onSpeechStart: () => {
                this.isSpeaking = true;
                if (this.onSpeechStartCallback) this.onSpeechStartCallback();
                // 标记需要回溯冲刷
                this.needFlushPreBuffer = true;
            },
            onSpeechEnd: (speechAudio: Float32Array) => {
                this.isSpeaking = false;
                if (this.onSpeechEndCallback) this.onSpeechEndCallback(speechAudio);
            },
            onMisfire: undefined,
            threshold: this.vadThreshold,
            noEmit: true // 不从 VAD 分支输出音频，避免改变帧边界
        });

        // 兼容旧结构：此处不再创建统一的 speechProcessor，改为分支管道
        // 将音频流拆为两路：一路用于 VAD 判定（无音频输出），一路用于编码（保持 worklet 的帧大小）
        const [streamForVad, streamForEncode] = audioStream.tee();

        // VAD 分支：保持流动以触发判定回调
        const vadPipelinePromise = streamForVad
            .pipeThrough(this.vadTransform)
            .pipeTo(
                new WritableStream<Float32Array>({ write: (): void => { /* no-op */ } }),
                { signal: abort.signal }
            )
            .catch(error => { throw error; });

        // 编码分支：仅在说话时向下游发送数据
        const encodePipelinePromise = streamForEncode
            .pipeTo(new WritableStream<Float32Array>({
                write: (audioData: Float32Array): void => {
                    // 维护回溯环形缓冲
                    const maxFrames = this.lookBackDurationMs > 0 ? Math.max(1, Math.ceil(this.lookBackDurationMs / this.frameDurationMs)) : 0;
                    if (maxFrames > 0) {
                        this.preSpeechBuffer.push(audioData);
                        if (this.preSpeechBuffer.length > maxFrames) {
                            this.preSpeechBuffer.shift();
                        }
                    }

                    if (this.vadEnabled && !this.isSpeaking) {
                        return; // 静音期仅缓冲，不发送
                    }

                    // 首次讲话：先冲刷回溯缓冲
                    if (this.vadEnabled && this.needFlushPreBuffer) {
                        if (this.preSpeechBuffer.length) {
                            for (let i = 0; i < this.preSpeechBuffer.length; i++) {
                                const backChunk = this.preSpeechBuffer[i];
                                const backBytes = float32ToPcm16(backChunk, this.pcmOutBuffer);
                                audioDataHandler(backBytes);
                            }
                        }
                        this.preSpeechBuffer.length = 0;
                        this.needFlushPreBuffer = false;
                    }
                    // 通知外部有音频数据（用于外部收集）
                    if (this.onAudioDataCallback) {
                        this.onAudioDataCallback(audioData);
                    }
                    const uint8Data = float32ToPcm16(audioData, this.pcmOutBuffer);
                    audioDataHandler(uint8Data);
                }
            }), { signal: abort.signal })
            .catch(error => { throw error; });

        const pipelinePromise = Promise.all([vadPipelinePromise, encodePipelinePromise]);
        // 不等待pipeline完成
        pipelinePromise.catch(() => undefined);

    }

    /**
     * 停止VAD音频处理
     */
    stopVADProcessing(): void {
        const runId = this.currentRunId;
        console.log('[AudioRecorder] stopVADProcessing', {
            runId,
            hasMediaStream: !!this.mediaStream,
            trackCount: this.mediaStream?.getTracks().length ?? 0,
        });

        // Critical: abort pipeTo -> triggers ReadableStream.cancel -> ingestAudioStream.cleanup()
        if (this.pipelineAbort) {
            try {
                this.pipelineAbort.abort();
            } catch (_e) { void 0; }
            this.pipelineAbort = null;
        }

        if (this.mediaStream) {
            this.mediaStream.getTracks().forEach(track => {
                try { track.stop(); } catch (_e) { void 0; }
            });
            this.mediaStream = null;
        }
        // 清理引用
        this.vadTransform = null;
        // 清理回溯缓冲
        this.preSpeechBuffer = [];
        this.needFlushPreBuffer = false;
        // NOTE: do NOT null callbacks here; AudioManager wires them once in constructor.
        // Clearing them will make subsequent start() runs lose events (and complicate diagnosis).
        // 重置说话状态
        this.isSpeaking = false;

        console.log('[AudioRecorder] stopVADProcessing done', { runId });
    }
}

// ---- 内置实现，避免依赖外部文件 ----

export interface WebRTCAudioOptions {
    gain?: number;
    channelId?: number;
    sampleRate?: number;
    targetSampleRate?: number; // 目标输出采样率，用于重采样
    echoCancellation?: boolean | { exact: boolean };
    noiseSuppression?: boolean | { exact: boolean };
    autoGainControl?: boolean | { exact: boolean };
}

const PREFERRED_SAMPLE_RATES = Object.freeze([16000, 48000, 44100, 22050]);

const RECOMMENDED_AUDIO_CONSTRAINTS: MediaTrackConstraints = Object.freeze({
    sampleRate: { ideal: 16000 },
    channelCount: 1,
    echoCancellation: true,
    noiseSuppression: true,
    autoGainControl: true,
});

async function detectBestSampleRate(deviceId?: string): Promise<number> {
    if (!navigator.mediaDevices || !navigator.mediaDevices.getUserMedia) {
        console.error('[Audio Detection] MediaDevices API not supported');
        return 44100;
    }
    for (const sampleRate of PREFERRED_SAMPLE_RATES) {
        try {
            const timeoutPromise = new Promise<never>((_, reject) =>
                setTimeout(() => reject(new Error('Timeout')), 5000)
            );
            const constraints = {
                audio: {
                    deviceId: deviceId ? { exact: deviceId } : undefined,
                    sampleRate: { exact: sampleRate },
                    channelCount: 1,
                },
            };
            const testStream = await Promise.race([
                navigator.mediaDevices.getUserMedia(constraints),
                timeoutPromise,
            ]);
            const audioTrack = testStream.getAudioTracks()[0];
            const settings = audioTrack.getSettings();
            testStream.getTracks().forEach(track => track.stop());
            if (settings.sampleRate === sampleRate) {
                return sampleRate;
            }
        } catch (_e) { /* try next */ }
    }
    const defaultStream = await navigator.mediaDevices.getUserMedia({
        audio: {
            deviceId: deviceId ? { exact: deviceId } : undefined,
            channelCount: 1,
        },
    });
    const audioTrack = defaultStream.getAudioTracks()[0];
    const settings = audioTrack.getSettings();
    defaultStream.getTracks().forEach(track => track.stop());
    return settings.sampleRate || 16000;
}

async function createWebRTCAudioStream(deviceId?: string, options: WebRTCAudioOptions = {}): Promise<{ mediaStream: MediaStream; audioStream: ReadableStream<Float32Array> }> {
    let targetSampleRate = options.sampleRate;
    if (!targetSampleRate) {
        targetSampleRate = await detectBestSampleRate(deviceId);
    }
    const baseConstraints = RECOMMENDED_AUDIO_CONSTRAINTS;
    const audioConstraints: MediaTrackConstraints = {
        deviceId: deviceId ? { exact: deviceId } : undefined,
        ...baseConstraints,
        sampleRate: targetSampleRate,
        ...(options.echoCancellation !== undefined && { echoCancellation: options.echoCancellation }),
        ...(options.noiseSuppression !== undefined && { noiseSuppression: options.noiseSuppression }),
        ...(options.autoGainControl !== undefined && { autoGainControl: options.autoGainControl }),
    };
    const mediaStream = await navigator.mediaDevices.getUserMedia({ audio: audioConstraints });
    const audioTrack = mediaStream.getAudioTracks()[0];
    const actualSampleRate = audioTrack?.getSettings().sampleRate || targetSampleRate;
    // 强制输出采样率为 Opus 期望的采样率
    const outputSampleRate = AUDIO_QUALITY.SAMPLE_RATE;
    const audioStream = await ingestAudioStream(mediaStream, {
        gain: options.gain,
        channelId: options.channelId,
        sampleRate: actualSampleRate,
        targetSampleRate: outputSampleRate,
    });
    return { mediaStream, audioStream };
}

async function ingestAudioStream(
    mediaStream: MediaStream,
    options: { gain?: number; channelId?: number; sampleRate?: number; targetSampleRate?: number } = {}
): Promise<ReadableStream<Float32Array>> {
    if (!mediaStream.getAudioTracks().length) {
        throw new Error('MediaStream must contain at least one audio track');
    }
    const ingestId = ++__ingestStreamSeq;
    let audioContext: AudioContext | null = null;
    let workletNode: AudioWorkletNode | null = null;
    let workletObjectUrl: string | null = null;

    const createResamplerWorkletURL = (): string => {
        const workletCode = `
class ResamplerProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const opts = (options && options.processorOptions) || {};
    this.gain = opts.gain ?? 1.0;
    this.channelId = opts.channelId ?? 0;
    this.inputSampleRate = opts.inputSampleRate ?? 16000;
    this.targetSampleRate = opts.targetSampleRate ?? 16000;
    this.targetBufferSize = opts.targetBufferSize ?? 1024;
    // 使用分片队列避免频繁拷贝
    this.queue = [];
    this.queuedLength = 0;
    this.chunkCount = 0;
    this.needsResampling = this.inputSampleRate !== this.targetSampleRate;

  }
  resampleAudioBuffer(inputBuffer) {
    if (!this.needsResampling) return inputBuffer;
    const inData = inputBuffer;
    const inLen = inData.length >>> 0;
    const ratio = this.inputSampleRate / this.targetSampleRate;
    const outLen = Math.round(inLen / ratio) >>> 0;
    const out = new Float32Array(outLen);
    let pos = 0;
    for (let i = 0; i < outLen; i++) {
      const idx = pos | 0; // floor
      const frac = pos - idx;
      const next = idx + 1;
      const a = inData[idx] || 0;
      const b = next < inLen ? inData[next] : 0;
      // linear interpolation: a + (b - a) * frac
      out[i] = a + (b - a) * frac;
      pos += ratio;
    }
    return out;
  }
  enqueue(data) {
    this.queue.push(data);
    this.queuedLength += data.length;
  }
  dequeueChunk(size) {
    const out = new Float32Array(size);
    let offset = 0;
    while (offset < size && this.queue.length > 0) {
      const head = this.queue[0];
      const remaining = size - offset;
      if (head.length <= remaining) {
        out.set(head, offset);
        offset += head.length;
        this.queue.shift();
      } else {
        out.set(head.subarray(0, remaining), offset);
        this.queue[0] = head.subarray(remaining);
        offset += remaining;
      }
    }
    this.queuedLength -= size;
    return out;
  }
  process(inputs, _outputs, _parameters) {
    const input = inputs[0];
    if (!input || !input[0]) return true;
    if (this.channelId >= input.length) this.channelId = 0;
    const channelData = input[this.channelId];
    if (channelData && channelData.length > 0) {
      let processedData;
      if (this.gain !== 1.0) {
        processedData = new Float32Array(channelData.length);
        for (let i = 0; i < channelData.length; i++) {
          processedData[i] = channelData[i] * this.gain;
        }
      } else {
        processedData = channelData;
      }
      if (this.needsResampling) {
        processedData = this.resampleAudioBuffer(processedData);
      }
      this.enqueue(processedData);
      while (this.queuedLength >= this.targetBufferSize) {
        const chunkToSend = this.dequeueChunk(this.targetBufferSize);
        this.chunkCount = (this.chunkCount || 0) + 1;
        // 使用 Transferable 零拷贝传输
        this.port.postMessage({ type: 'audioData', data: chunkToSend, chunkIndex: this.chunkCount, bufferSize: chunkToSend.length }, [chunkToSend.buffer]);
      }
    }
    return true;
  }
}
registerProcessor('resampler-processor', ResamplerProcessor);
`;
        const blob = new Blob([workletCode], { type: 'application/javascript' });
        return URL.createObjectURL(blob);
    };

    const cleanup = (reason: string): void => {
        // This is the most important log: if you DON'T see it after stop, resources are leaking.
        console.log('[AudioRecorder][ingestAudioStream] cleanup', {
            ingestId,
            reason,
            hasWorkletNode: !!workletNode,
            audioContextState: audioContext?.state ?? null,
        });
        if (workletNode) { workletNode.disconnect(); workletNode = null; }
        if (audioContext) { audioContext.close(); audioContext = null; }
        if (workletObjectUrl) { try { URL.revokeObjectURL(workletObjectUrl); } catch { /* noop */ } workletObjectUrl = null; }
    };

    return new ReadableStream<Float32Array>({
        start: async (controller: ReadableStreamDefaultController<Float32Array>): Promise<void> => {
            try {
                audioContext = new AudioContext({ sampleRate: options.sampleRate ?? 16000, latencyHint: 'interactive' });
                if (audioContext.state === 'suspended') { try { await audioContext.resume(); } catch { /* noop */ } }
                console.log('[AudioRecorder][ingestAudioStream] start', {
                    ingestId,
                    audioContextState: audioContext.state,
                    inputSampleRate: options.sampleRate ?? 16000,
                    targetSampleRate: options.targetSampleRate ?? (options.sampleRate ?? 16000),
                });
                workletObjectUrl = createResamplerWorkletURL();
                await audioContext.audioWorklet.addModule(workletObjectUrl);
                const inputSampleRate = options.sampleRate ?? 16000;
                const outputSampleRate = options.targetSampleRate ?? inputSampleRate;
                // 令输出帧长度与 Opus 帧长一致：由固定字节数推导出样本数（单声道、S16LE -> 2 字节/样本）
                const opusFrameSamples = Math.floor(AUDIO_QUALITY.FIXED_CHUNK_SIZE / 2);
                const targetBufferSize = opusFrameSamples;
                workletNode = new AudioWorkletNode(audioContext, 'resampler-processor', {
                    processorOptions: {
                        gain: options.gain ?? 1.0,
                        channelId: options.channelId ?? 0,
                        inputSampleRate,
                        targetSampleRate: outputSampleRate,
                        targetBufferSize,
                    },
                    numberOfOutputs: 0,
                });
                workletNode.port.onmessage = (event: MessageEvent<{ type: string; data: Float32Array }>): void => {
                    const { type, data } = event.data;
                    if (type === 'audioData') {
                        try {
                            controller.enqueue(data);
                        } catch (_e) {
                            cleanup('controller.enqueue failed');
                        }
                    }
                };
                const source = audioContext.createMediaStreamSource(mediaStream);
                source.connect(workletNode);
            } catch (error) {
                console.error('Failed to setup AudioWorklet:', error);
                throw new Error(`AudioWorklet setup failed: ${error instanceof Error ? error.message : String(error)}`);
            }
        },
        cancel: (): void => { cleanup('ReadableStream.cancel'); },
    });
}