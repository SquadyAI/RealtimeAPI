import { AUDIO_QUALITY } from '../protocol/ClientProtocol';
import { pcm16ToFloat32 } from './AudioUtils';

export class AudioPlayer {
    private audioContext: AudioContext | null = null;
    private isPlaying: boolean = false;
    private playbackNode: AudioWorkletNode | null = null;
    private workletReady: Promise<void> | null = null;
    private processorName: string;

    // 新增：智能打断控制
    private lastClearTime: number = 0;
    private lastResetTime: number = 0;
    private clearCount: number = 0;
    private resetCount: number = 0;
    private readonly MIN_CLEAR_INTERVAL: number = 100; // 最小清空间隔100ms
    private readonly MIN_RESET_INTERVAL: number = 500; // 最小重置间隔500ms
    private readonly MAX_CLEAR_COUNT: number = 5; // 最大连续清空次数
    private readonly MAX_RESET_COUNT: number = 3; // 最大连续重置次数

    private debugMode: boolean = true; // 临时启用调试模式以诊断音频问题
    private autoGain: number = 1.0; // 自动增益，默认1.0（不增益）
    private chunkCount: number = 0; // 已处理的音频块数量

    constructor() {
        this.initAudioContext();
        // 随机化处理器名称，允许多个 player 共存
        this.processorName = `pcm-player-processor-` + Math.random().toString(36).slice(2) + Date.now().toString(36);
        console.log('[AudioPlayer] 🔊 音频播放器已创建，调试模式已启用');
    }

    // 设置自动增益
    public setAutoGain(gain: number): void {
        this.autoGain = Math.max(0.1, Math.min(10.0, gain)); // 限制在0.1-10.0之间
        console.log('[AudioPlayer] 🔊 设置音频增益:', this.autoGain);
    }

    // 设置调试模式
    public setDebugMode(enabled: boolean): void {
        this.debugMode = enabled;
        if (this.playbackNode) {
            this.playbackNode.port.postMessage({ type: 'setDebugMode', enabled });
        }
        console.log('[AudioPlayer] 调试模式:', enabled ? '启用' : '禁用');
    }

    // 初始化音频上下文
    private initAudioContext(): void {
        try {
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            this.audioContext = new (window.AudioContext || (window as any).webkitAudioContext)({
                sampleRate: AUDIO_QUALITY.SAMPLE_RATE
            });
        } catch (error) {
            console.error('Failed to create audio context:', error);
        }
    }

    // 内联注册播放用的 AudioWorkletProcessor，避免新增文件
    private async ensurePlaybackWorklet(): Promise<void> {
        if (!this.audioContext) {
            this.initAudioContext();
        }
        if (!this.audioContext) return;

        // 防并发：已有初始化中的 Promise 时直接复用
        if (this.workletReady) return this.workletReady;

        const workletCode = `
            class PCMPlayerProcessor extends AudioWorkletProcessor {
                constructor() {
                    super();
                    this.queue = [];
                    this.readIndex = 0;
                    this.current = null;
                    this.isPlaying = false;
                    this.clearCount = 0; // 用于调试的清空计数器
                    this.debugMode = false; // 默认关闭；需要诊断时再通过 setDebugMode 开启
                    this.lastLogTime = 0;
                    this.underrunCount = 0; // 缓冲区欠载计数
                    this.lastSample = 0; // 记录最后一个样本值，用于平滑过渡
                    this.totalSamplesReceived = 0; // 总接收的样本数
                    this.port.onmessage = (event) => {
                        const data = event.data;
                        if (!data) return;
                        if (data.type === 'push' && data.samples instanceof Float32Array) {
                            // 将一帧样本入队
                            this.totalSamplesReceived += data.samples.length;
                            
                            // 平滑处理：如果队列中有数据，对新数据的开头应用淡入，避免突变
                            if (this.queue.length > 0) {
                                const lastBuffer = this.queue[this.queue.length - 1];
                                const lastSampleValue = lastBuffer[lastBuffer.length - 1];
                                const firstSampleValue = data.samples[0];
                                const diff = Math.abs(firstSampleValue - lastSampleValue);
                                
                                // 如果相邻样本差值过大，应用平滑过渡
                                if (diff > 0.1) {  // 阈值0.1（归一化范围[-1,1]）
                                    const smoothLength = Math.min(16, data.samples.length);
                                    const newSamples = new Float32Array(data.samples);
                                    for (let i = 0; i < smoothLength; i++) {
                                        const ratio = i / smoothLength;
                                        newSamples[i] = lastSampleValue * (1 - ratio) + data.samples[i] * ratio;
                                    }
                                    this.queue.push(newSamples);
                                    
                                    if (this.debugMode && diff > 0.3) {
                                        console.warn('[AudioWorklet] 🔧 检测到样本跳变:', {
                                            diff: diff.toFixed(3),
                                            lastSample: lastSampleValue.toFixed(3),
                                            firstSample: firstSampleValue.toFixed(3),
                                            appliedSmoothing: true
                                        });
                                    }
                                } else {
                                    this.queue.push(data.samples);
                                }
                            } else {
                                this.queue.push(data.samples);
                            }
                            
                            this.isPlaying = true;
                            
                            // 每秒输出一次状态日志
                            const now = Date.now();
                            if (this.debugMode && now - this.lastLogTime > 1000) {
                                console.log('[AudioWorklet] 📊 队列状态:', {
                                    queueLength: this.queue.length,
                                    samplesInQueue: this.queue.reduce((sum, arr) => sum + arr.length, 0),
                                    totalReceived: this.totalSamplesReceived,
                                    underruns: this.underrunCount,
                                    clearCount: this.clearCount
                                });
                                this.lastLogTime = now;
                            }
                        } else if (data.type === 'clear') {
                            // 清空队列 - 用于音频打断
                            const queueLength = this.queue.length;
                            this.clearCount++;
                            if (this.debugMode) {
                                console.log('[AudioWorklet] 🔴 收到清空队列指令 #' + this.clearCount + '，当前队列长度:', queueLength);
                            }
                            this.queue = [];
                            this.current = null;
                            this.readIndex = 0;
                            this.isPlaying = false;
                            if (this.debugMode) {
                                console.log('[AudioWorklet] 🔴 队列已清空，清空计数:', this.clearCount);
                            }
                        } else if (data.type === 'reset') {
                            // 重置播放器状态
                            if (this.debugMode) {
                                console.log('[AudioWorklet] 🔴 收到重置指令');
                            }
                            this.queue = [];
                            this.current = null;
                            this.readIndex = 0;
                            this.isPlaying = false;
                            this.clearCount = 0;
                            if (this.debugMode) {
                                console.log('[AudioWorklet] 🔴 播放器已重置');
                            }
                        } else if (data.type === 'setDebugMode') {
                            this.debugMode = data.enabled;
                            console.log('[AudioWorklet] 调试模式:', this.debugMode ? '启用' : '禁用');
                        }
                    };
                }
                process(_inputs, outputs) {
                    const output = outputs[0];
                    const channel = output[0];
                    const framesNeeded = channel.length;
                    let offset = 0;

                    while (offset < framesNeeded) {
                        if (!this.current) {
                            if (this.queue.length > 0) {
                                this.current = this.queue.shift();
                                this.readIndex = 0;
                            } else {
                                // 队列为空：如果刚刚在播放，则做一次短淡出；否则直接输出静音（idle）
                                const remainingSamples = framesNeeded - offset;
                                const wasPlaying = this.isPlaying;
                                this.isPlaying = false;

                                if (wasPlaying) {
                                    this.underrunCount++;
                                    if (this.debugMode && this.underrunCount % 10 === 1) {
                                        console.warn('[AudioWorklet] ⚠️ 缓冲区欠载 #' + this.underrunCount + '，队列为空，使用平滑淡出');
                                    }

                                    const fadeOutLength = Math.min(remainingSamples, 64); // 最多64个样本的淡出
                                    for (let i = 0; i < fadeOutLength; i++) {
                                        const fadeRatio = 1.0 - (i / fadeOutLength);
                                        channel[offset + i] = this.lastSample * fadeRatio;
                                    }
                                    for (let i = fadeOutLength; i < remainingSamples; i++) {
                                        channel[offset + i] = 0;
                                    }
                                } else {
                                    for (let i = 0; i < remainingSamples; i++) {
                                        channel[offset + i] = 0;
                                    }
                                }

                                this.lastSample = 0; // 重置最后样本值
                                break;
                            }
                        }

                        const remainingInCurrent = this.current.length - this.readIndex;
                        const remainingInOutput = framesNeeded - offset;
                        const toCopy = remainingInOutput < remainingInCurrent ? remainingInOutput : remainingInCurrent;

                        channel.set(this.current.subarray(this.readIndex, this.readIndex + toCopy), offset);
                        
                        // 记录最后一个样本值
                        if (toCopy > 0) {
                            this.lastSample = this.current[this.readIndex + toCopy - 1];
                        }
                        
                        offset += toCopy;
                        this.readIndex += toCopy;

                        if (this.readIndex >= this.current.length) {
                            this.current = null;
                            this.readIndex = 0;
                        }
                    }

                    return true;
                }
            }
            registerProcessor('${this.processorName}', PCMPlayerProcessor);
        `;

        const blob = new Blob([workletCode], { type: 'application/javascript' });
        const url = URL.createObjectURL(blob);

        this.workletReady = (async (): Promise<void> => {
            await this.audioContext!.audioWorklet.addModule(url);
            this.playbackNode = new AudioWorkletNode(this.audioContext!, this.processorName, {
                numberOfInputs: 0,
                numberOfOutputs: 1,
                outputChannelCount: [1]
            });
            this.playbackNode.connect(this.audioContext!.destination);
        })();

        await this.workletReady;
        URL.revokeObjectURL(url);
    }

    // 播放音频分片（通过 AudioWorklet 精确拉流播放）
    async playAudioChunk(audioData: Uint8Array): Promise<void> {
        if (!this.audioContext) {
            this.initAudioContext();
            if (!this.audioContext) {
                console.warn('Audio context is not available');
                return;
            }
        }

        if (this.audioContext.state === 'suspended') {
            await this.audioContext.resume();
        }

        await this.ensurePlaybackWorklet();

        try {
            // 调试：记录音频数据到达情况和质量检查
            // (keep no per-chunk timestamp unless needed; logs are gated/commented to reduce noise)

            // 检查PCM数据的前几个样本，看是否有异常值
            const dataView = new DataView(audioData.buffer, audioData.byteOffset, Math.min(20, audioData.byteLength));
            const firstSamples = [];
            for (let i = 0; i < Math.min(5, Math.floor(audioData.length / 2)); i++) {
                firstSamples.push(dataView.getInt16(i * 2, true));
            }

            if (this.debugMode) {
                // NOTE: noisy per-chunk log; keep commented unless debugging audio playback.
                // console.log('[AudioPlayer] 🎵 收到音频数据:', {
                //     dataSize: audioData.length,
                //     timestamp: performance.now().toFixed(2),
                //     isPlaying: this.isPlaying,
                //     firstSamples: firstSamples,
                //     hasExtremeValues: firstSamples.some(s => Math.abs(s) > 30000)
                // });
            }

            // 性能优化：仅在数据异常时检查PCM数据质量
            if (audioData.length < 4) {
                console.warn('[AudioPlayer] ⚠️ PCM数据长度异常:', audioData.length);
            }

            let floatData = pcm16ToFloat32(audioData);

            // 性能优化：移除详细的Float32数据质量检查，仅做基本验证
            if (floatData.length === 0) {
                console.warn('[AudioPlayer] ⚠️ 转换后的Float32数据为空');
                return;
            }

            // 音量检测和自动增益
            this.chunkCount++;
            // const sumSquared = floatData.reduce((sum, sample) => sum + sample * sample, 0);
            // const rms = Math.sqrt(sumSquared / floatData.length);
            // const avgAmplitude = floatData.reduce((sum, sample) => sum + Math.abs(sample), 0) / floatData.length;

            if (this.debugMode && this.chunkCount % 10 === 0) {
                // console.log('[AudioPlayer] 📊 音量分析 (每10个chunk):', {
                //     chunkCount: this.chunkCount,
                //     rms: rms.toFixed(4),
                //     avgAmplitude: avgAmplitude.toFixed(4),
                //     maxSample: Math.max(...Array.from(floatData).map(Math.abs)).toFixed(4),
                //     currentGain: this.autoGain
                // });
            }

            // 应用增益（如果设置了）
            if (this.autoGain !== 1.0) {
                const gainedData = new Float32Array(floatData.length);
                for (let i = 0; i < floatData.length; i++) {
                    // 应用增益并限制在[-1, 1]范围内
                    gainedData[i] = Math.max(-1, Math.min(1, floatData[i] * this.autoGain));
                }
                floatData = gainedData;

                if (this.debugMode && this.chunkCount === 1) {
                    console.log('[AudioPlayer] 🔊 应用音频增益:', this.autoGain);
                }
            }

            // 将样本推送到播放队列，使用可转移对象减少拷贝
            if (this.playbackNode) {
                this.playbackNode.port.postMessage({ type: 'push', samples: floatData }, [floatData.buffer]);
                if (this.debugMode) {
                    // NOTE: noisy per-chunk log; keep commented unless debugging audio playback.
                    // console.log('[AudioPlayer] ✅ 音频数据已推送到AudioWorklet队列');
                }
            } else {
                console.warn('[AudioPlayer] ⚠️ AudioWorklet未初始化，无法播放音频');
            }

            this.isPlaying = true;
        } catch (error) {
            console.error('[AudioPlayer] ❌ 音频播放失败:', error);
            this.isPlaying = false;
        }
    }

    // 清空音频队列 - 用于音频打断（高性能优化版本）
    clearQueue(): void {
        const now = performance.now();

        // 智能频率检查：防止过于频繁的清空操作
        if (now - this.lastClearTime < this.MIN_CLEAR_INTERVAL) {
            // 移除详细日志，仅在必要时输出警告
            return;
        }

        // 智能状态检查：只有在真正需要时才清空
        if (!this.isPlaying) {
            return;
        }

        if (!this.playbackNode) {
            return;
        }

        // 更新统计信息
        this.lastClearTime = now;
        this.clearCount++;

        // 性能优化：减少日志输出
        if (this.clearCount % 10 === 0) {
            console.log('[AudioPlayer] 🔴 执行智能音频队列清空 #' + this.clearCount);
        }

        // 发送清空指令
        this.playbackNode.port.postMessage({ type: 'clear' });

        // 立即更新播放状态
        this.isPlaying = false;

        // 如果清空次数过多，记录警告
        if (this.clearCount > this.MAX_CLEAR_COUNT) {
            console.warn('[AudioPlayer] 🚨 清空次数过多，可能存在异常:', this.clearCount);
        }
    }

    // 强制重置播放器 - 用于彻底的音频打断（优化版本）
    forceReset(): void {
        const now = performance.now();

        // 智能频率检查：防止过于频繁的重置操作
        if (now - this.lastResetTime < this.MIN_RESET_INTERVAL) {
            console.warn('[AudioPlayer] ⚠️ 强制重置操作过于频繁，跳过本次操作');
            console.log('[AudioPlayer] 📊 频率检查:', {
                lastResetTime: this.lastResetTime,
                currentTime: now,
                interval: now - this.lastResetTime,
                minInterval: this.MIN_RESET_INTERVAL,
                resetCount: this.resetCount
            });
            return;
        }

        // 智能状态检查：只有在真正需要时才重置
        if (!this.isPlaying && !this.playbackNode) {
            console.log('[AudioPlayer] 🔍 播放器未活跃且未初始化，跳过强制重置');
            return;
        }

        // 更新统计信息
        this.lastResetTime = now;
        this.resetCount++;

        console.log('[AudioPlayer] 🔴 执行智能强制重置播放器');
        console.log('[AudioPlayer] 🚨 强制重置影响分析:');
        console.log('  - AudioWorklet时序基准将丢失');
        console.log('  - 后续音频播放可能短暂错乱');
        console.log('  - PCM数据与AudioContext需要重新同步');
        console.log('[AudioPlayer] 📊 重置统计:', {
            resetCount: this.resetCount,
            lastResetTime: this.lastResetTime,
            isPlaying: this.isPlaying,
            hasWorklet: !!this.playbackNode
        });

        if (this.playbackNode) {
            console.log('[AudioPlayer] 🔴 发送重置指令到AudioWorklet');
            this.playbackNode.port.postMessage({ type: 'reset' });
        }

        // 重置本地状态
        this.isPlaying = false;
        console.log('[AudioPlayer] 🔴 播放器已强制重置');

        // 如果重置次数过多，记录警告
        if (this.resetCount > this.MAX_RESET_COUNT) {
            console.warn('[AudioPlayer] 🚨 重置次数过多，可能存在异常:', {
                resetCount: this.resetCount,
                maxResetCount: this.MAX_RESET_COUNT,
                timeWindow: '最近一段时间'
            });
        }
    }

    // 检查播放器是否处于活跃状态
    get isPlaybackActive(): boolean {
        return this.isPlaying;
    }

    // 获取队列状态信息（用于调试和智能判断）
    getQueueStatus(): {
        isPlaying: boolean;
        hasWorklet: boolean;
        clearCount: number;
        resetCount: number;
        lastClearTime: number;
        lastResetTime: number;
    } {
        return {
            isPlaying: this.isPlaying,
            hasWorklet: !!this.playbackNode,
            clearCount: this.clearCount,
            resetCount: this.resetCount,
            lastClearTime: this.lastClearTime,
            lastResetTime: this.lastResetTime
        };
    }

    // 重置统计信息（用于调试或重置异常状态）
    resetStatistics(): void {
        this.clearCount = 0;
        this.resetCount = 0;
        this.lastClearTime = 0;
        this.lastResetTime = 0;
        console.log('[AudioPlayer] 📊 统计信息已重置');
    }

    // 停止播放
    stopPlayback(): void {
        console.log('[AudioPlayer] 🔴 停止播放被调用 - 开始完整清理');
        console.log('[AudioPlayer] 🔴 停止前状态:', {
            isPlaying: this.isPlaying,
            hasWorklet: !!this.playbackNode,
            hasContext: !!this.audioContext,
            contextState: this.audioContext?.state,
            clearCount: this.clearCount,
            resetCount: this.resetCount
        });

        // 先清空队列
        this.clearQueue();

        if (this.playbackNode) {
            try {
                console.log('[AudioPlayer] 🔴 断开AudioWorklet节点连接');
                this.playbackNode.disconnect();
            } catch (error) {
                console.warn('[AudioPlayer] 断开AudioWorklet连接时出错:', error);
            }
        }
        this.playbackNode = null;
        this.workletReady = null;

        if (this.audioContext && this.audioContext.state !== 'closed') {
            console.log('[AudioPlayer] 🔴 关闭AudioContext');
            this.audioContext.close().catch(error => {
                console.warn('Failed to close audio context:', error);
            });
        }

        this.audioContext = null;
        this.isPlaying = false;

        console.log('[AudioPlayer] 🔴 停止播放完成 - 全部资源已清理');
        console.log('[AudioPlayer] 🔴 停止后状态:', {
            isPlaying: this.isPlaying,
            hasWorklet: !!this.playbackNode,
            hasContext: !!this.audioContext
        });
    }
}