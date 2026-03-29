import { EventEmitter } from '../utils/EventEmitter';
// import { AUDIO_QUALITY } from "../protocol/ClientProtocol";
import { AudioPlayer } from "./AudioPlayer";
import { AudioRecorder } from "./AudioRecorder";
import { audioProcessor } from "./AudioCodec";
import { AudioManagerLogger as logger } from '../utils/GlobalLogger';
import type { WebRTCAudioOptions } from './AudioRecorder';

/**
 * 音频事件类型定义
 */
export interface AudioEvents {
    'speech:start': () => void;
    'speech:end': (speechAudio: Float32Array) => void;
    'speech:misfire': () => void;
    'audio:data': (audioData: Float32Array) => void;
    'audio:processed': (processedData: Uint8Array) => void;
    'vad:enabled': (enabled: boolean) => void;
    'vad:threshold': (threshold: number) => void;
    'error': (error: Error) => void;
}

/**
 * VAD配置选项
 */
export interface VADConfig {
    enabled: boolean;
    threshold: number;
    deviceId?: string;
}

/**
 * 音频处理模块配置
 */
export interface AudioModuleConfig {
    loopback?: boolean;
    uplink?: {
        chunkSize?: number;
        encoderType?: 'opus' | 'pcm';
        onChunkProcessed: (chunk: Uint8Array) => Promise<void>;
    };
    webrtc?: WebRTCAudioOptions;
}

/**
 * 优雅的音频管理器
 *
 * 设计原则：
 * 1. 事件驱动架构，避免回调地狱
 * 2. 单一职责，每个方法只做一件事
 * 3. 组合而非继承，模块化设计
 * 4. 类型安全，完善的TypeScript支持
 * 5. 简洁的API，易于使用和测试
 * 6. 支持依赖注入，提高可测试性和灵活性
 * 7. 统一的日志管理，便于调试和监控
 * 8. 改进的错误处理机制，提高健壮性
 */
export class AudioManager extends EventEmitter {
    private recorder: AudioRecorder;
    private player: AudioPlayer | null = null;
    private encodeOpus: boolean = true;

    private vadConfig: VADConfig = {
        enabled: false,
        threshold: 0.5
    };

    private moduleConfig: AudioModuleConfig = {};
    private isProcessing: boolean = false;
    private modelPreloaded: boolean = false;

    /**
     * 构造函数
     * @param recorder 可选的 AudioRecorder 实例，用于依赖注入
     * @param player 可选的 AudioPlayer 实例，用于依赖注入
     */
    constructor(recorder?: AudioRecorder, player?: AudioPlayer) {
        super();
        this.recorder = recorder || new AudioRecorder();
        this.player = player || null;
        this.setupRecorderEvents();
        logger.info('Initialized with event-driven architecture');
    }

    /**
     * 预加载 VAD 模型（页面打开时调用，避免用户点击时等待）
     * @returns Promise<void>
     */
    async preloadModel(): Promise<void> {
        if (this.modelPreloaded) {
            logger.info('VAD model already preloaded');
            return;
        }

        try {
            logger.info('Preloading VAD model...');
            await this.recorder.preloadModel();
            this.modelPreloaded = true;
            logger.info('VAD model preloaded successfully');
        } catch (error) {
            logger.error('Failed to preload VAD model:', error);
            // 预加载失败不阻塞，允许后续按需加载
        }
    }

    /**
     * 设置录音器事件监听
     */
    private setupRecorderEvents(): void {
        this.recorder.setOnSpeechStartCallback(() => {
            this.emit('speech:start');
        });

        this.recorder.setOnSpeechEndCallback((speechAudio: Float32Array) => {
            this.emit('speech:end', speechAudio);
        });

        this.recorder.setOnAudioDataCallback((audioData: Float32Array) => {
            this.emit('audio:data', audioData);
        });
    }

    /**
     * 配置VAD设置和音频模块
     * @param config 配置对象，可以包含VAD配置和音频模块配置
     */
    configure(config: Partial<VADConfig & AudioModuleConfig>): this {
        // 更新VAD配置
        if (config.enabled !== undefined) {
            this.vadConfig.enabled = config.enabled;
            this.recorder.enableVAD(config.enabled);
            this.emit('vad:enabled', config.enabled);
        }

        if (config.threshold !== undefined) {
            this.vadConfig.threshold = config.threshold;
            this.recorder.setVADThreshold(config.threshold);
            this.emit('vad:threshold', config.threshold);
        }

        if (config.deviceId !== undefined) {
            this.vadConfig.deviceId = config.deviceId;
        }

        // 更新模块配置
        if (config.loopback !== undefined) {
            this.moduleConfig.loopback = config.loopback;
        }

        if (config.uplink !== undefined) {
            this.moduleConfig.uplink = config.uplink;
        }

        // 更新 WebRTC 配置
        if (config.webrtc !== undefined) {
            this.moduleConfig.webrtc = config.webrtc;
            this.recorder.configureWebRTCAudio(config.webrtc);
            // info log removed to satisfy lint rules
        }

        // config updated

        return this;
    }

    /**
     * 启动音频处理
     * @returns 返回一个 Promise，解析为当前实例
     */
    async start(): Promise<this> {
        if (this.isProcessing) {
            console.warn('[AudioManager] Already processing, ignoring start request');
            return this;
        }

        try {
            // starting
            this.isProcessing = true;

            // 启动VAD处理
            await this.recorder.startVADProcessing(
                this.vadConfig.deviceId,
                (audioData: Uint8Array) => {
                    this.emit('audio:processed', audioData);
                    this.handleProcessedAudio(audioData);
                }
            );

            // 启动配置的模块
            if (this.moduleConfig.loopback) {
                this.startLoopback();
            }

            if (this.moduleConfig.uplink) {
                this.startUplink(this.moduleConfig.uplink);
            }

            // started
            return this;

        } catch (error) {
            this.isProcessing = false;
            const err = error instanceof Error ? error : new Error(String(error));
            console.error('[AudioManager] Failed to start audio processing:', err);
            this.emit('error', err);
            throw err;
        }
    }

    /**
     * 停止音频处理
     * @returns 返回一个 Promise，解析为当前实例
     */
    async stop(): Promise<this> {
        if (!this.isProcessing) {
            console.warn('[AudioManager] Not processing, ignoring stop request');
            return this;
        }

        try {
            // stopping

            // 停止各个模块
            this.stopLoopback();
            this.stopUplink();

            // 延迟停止VAD以确保事件完成
            await new Promise(resolve => {
                setTimeout(() => {
                    this.recorder.stopVADProcessing();
                    this.isProcessing = false;
                    resolve(void 0);
                }, 100);
            });

            // stopped
            return this;

        } catch (error) {
            const err = error instanceof Error ? error : new Error(String(error));
            console.error('[AudioManager] Error stopping audio processing:', err);
            this.emit('error', err);
            throw err;
        }
    }

    /**
     * 处理已处理的音频数据
     */
    private handleProcessedAudio(audioData: Uint8Array): void {
        // 检查是否还在处理状态，如果已停止则不处理音频数据
        if (!this.isProcessing) {
            return;
        }

        // 音频回环播放
        if (this.moduleConfig.loopback && this.player) {
            this.player.playAudioChunk(audioData);
        }

        // 音频上传处理（直接编码/直通，不再使用Chunker）
        if (this.moduleConfig.uplink) {
            const handler = this.moduleConfig.uplink.onChunkProcessed;
            if (this.encodeOpus) {
                try {
                    const encoded = audioProcessor.encodeOpus(audioData);
                    if (encoded.length) {
                        handler(encoded);
                    }
                } catch (error) {
                    console.error('[AudioManager] Opus encoding failed:', error);
                }
            } else {
                handler(audioData);
            }
        }
    }

    /**
     * 启动音频回环播放
     */
    private startLoopback(): void {
        if (!this.player) {
            this.player = new AudioPlayer();
            logger.info('Audio loopback started');
        }
    }

    /**
     * 停止音频回环播放
     */
    private stopLoopback(): void {
        if (this.player) {
            this.player.stopPlayback();
            this.player = null;
        }
    }

    /**
     * 启动音频上传处理
     */
    private startUplink(config: NonNullable<AudioModuleConfig['uplink']>): void {
        this.encodeOpus = config.encoderType !== 'pcm';
        logger.info('Audio uplink configured');
    }

    /**
     * 停止音频上传处理
     */
    private stopUplink(): void { /* no-op */ }

    /**
     * 获取当前状态
     * @returns 返回一个包含当前状态信息的对象
     */
    getStatus(): {
        isProcessing: boolean;
        vadConfig: VADConfig;
        moduleConfig: AudioModuleConfig;
        hasPlayer: boolean;
        hasChunker: boolean;
    } {
        return {
            isProcessing: this.isProcessing,
            vadConfig: { ...this.vadConfig },
            moduleConfig: { ...this.moduleConfig },
            hasPlayer: !!this.player,
            hasChunker: false
        };
    }

    /**
     * 设置Opus编码
     * @param enabled 是否启用Opus编码
     * @returns 返回当前实例，支持链式调用
     */
    setOpusEncoding(enabled: boolean): this {
        this.encodeOpus = enabled;
        // set opus encoding
        return this;
    }

    /**
     * 配置 WebRTC 音频处理选项
     * @param options WebRTC 音频选项
     * @returns 返回当前实例，支持链式调用
     */
    configureWebRTCAudio(options: Partial<WebRTCAudioOptions>): this {
        if (!this.moduleConfig.webrtc) {
            this.moduleConfig.webrtc = {};
        }
        this.moduleConfig.webrtc = { ...this.moduleConfig.webrtc, ...options };
        this.recorder.configureWebRTCAudio(options);
        // updated
        return this;
    }

    /**
     * 设置回声消除状态
     * @param enabled 是否启用回声消除
     * @returns 返回当前实例，支持链式调用
     */
    setEchoCancellation(enabled: boolean): this {
        this.recorder.setEchoCancellation(enabled);
        if (!this.moduleConfig.webrtc) {
            this.moduleConfig.webrtc = {};
        }
        this.moduleConfig.webrtc.echoCancellation = enabled;
        // updated
        return this;
    }

    /**
     * 设置噪声抑制状态
     * @param enabled 是否启用噪声抑制
     * @returns 返回当前实例，支持链式调用
     */
    setNoiseSuppression(enabled: boolean): this {
        this.recorder.setNoiseSuppression(enabled);
        if (!this.moduleConfig.webrtc) {
            this.moduleConfig.webrtc = {};
        }
        this.moduleConfig.webrtc.noiseSuppression = enabled;
        // updated
        return this;
    }

    /**
     * 设置自动增益控制状态
     * @param enabled 是否启用自动增益控制
     * @returns 返回当前实例，支持链式调用
     */
    setAutoGainControl(enabled: boolean): this {
        this.recorder.setAutoGainControl(enabled);
        if (!this.moduleConfig.webrtc) {
            this.moduleConfig.webrtc = {};
        }
        this.moduleConfig.webrtc.autoGainControl = enabled;
        // updated
        return this;
    }

    /**
     * 获取当前 WebRTC 音频配置
     * @returns 当前的 WebRTC 音频选项
     */
    getWebRTCConfig(): WebRTCAudioOptions {
        return this.recorder.getWebRTCConfig();
    }

    /**
     * 销毁音频管理器
     * 清理所有资源并停止音频处理
     */
    destroy(): void {
        logger.info('Destroying audio manager');

        if (this.isProcessing) {
            this.stop().catch(err =>
                logger.error('Error during destruction:', err)
            );
        }

        this.stopLoopback();
        this.stopUplink();
        this.removeAllListeners();

        logger.info('Audio manager destroyed');
    }
}

// 类型安全的事件监听器
export interface TypedAudioManager {
    on<K extends keyof AudioEvents>(event: K, listener: AudioEvents[K]): this;
    off<K extends keyof AudioEvents>(event: K, listener: AudioEvents[K]): this;
    emit<K extends keyof AudioEvents>(event: K, ...args: Parameters<AudioEvents[K]>): boolean;
}
