/**
 * 二进制音频协议适配器
 * 
 * 这个文件提供了一个适配器层，用于在新的二进制音频协议和现有的
 * JSON协议之间提供无缝的切换和兼容性。
 */

import {
  decodeResponseAudioDeltaMessage,
  createMockBinaryPacket
} from './BinaryAudioDecoder';

import type {
  BinaryAudioProtocolConfig,
  BinaryAudioPerformanceMetrics
} from './BinaryAudioTypes';

import {
  DEFAULT_BINARY_AUDIO_CONFIG
} from './BinaryAudioTypes';

import type {
  ResponseAudioDeltaEvent,
  AudioChunkPayload
} from './ClientProtocol';

/**
 * 音频协议类型
 */
export type AudioProtocolType = 'json' | 'binary' | 'auto';

export const AudioProtocolType = {
  JSON: 'json' as const,
  BINARY: 'binary' as const,
  AUTO: 'auto' as const
};

/**
 * 音频协议适配器配置
 */
export interface AudioAdapterConfig {
  /** 使用的协议类型 */
  protocolType: AudioProtocolType;
  /** 二进制协议配置 */
  binaryConfig: Partial<BinaryAudioProtocolConfig>;
  /** 是否启用协议自动切换 */
  enableAutoSwitch: boolean;
  /** 协议切换阈值（连续失败次数） */
  switchThreshold: number;
  /** 是否启用性能监控 */
  enablePerformanceMonitoring: boolean;
}

/**
 * 默认适配器配置
 */
export const DEFAULT_ADAPTER_CONFIG: AudioAdapterConfig = {
  protocolType: AudioProtocolType.JSON,
  binaryConfig: DEFAULT_BINARY_AUDIO_CONFIG,
  enableAutoSwitch: true,
  switchThreshold: 3,
  enablePerformanceMonitoring: true
};

/**
 * 音频协议适配器
 * 
 * 提供统一的接口来处理不同协议类型的音频数据
 */
export class BinaryAudioAdapter {
  private config: AudioAdapterConfig;
  private performanceMetrics: BinaryAudioPerformanceMetrics;
  private consecutiveFailures: number = 0;
  private currentProtocol: AudioProtocolType;
  private isBinaryProtocolAvailable: boolean = false;

  constructor(config: Partial<AudioAdapterConfig> = {}) {
    this.config = { ...DEFAULT_ADAPTER_CONFIG, ...config };
    this.currentProtocol = this.config.protocolType;
    this.performanceMetrics = this.createInitialMetrics();

    // 检测二进制协议是否可用
    this.detectBinaryProtocolAvailability();
  }

  /**
   * 检测二进制协议是否可用
   */
  private detectBinaryProtocolAvailability(): void {
    try {
      // 尝试创建一个测试数据包并解码
      const testPacket = createMockBinaryPacket();
      decodeResponseAudioDeltaMessage(testPacket);
      this.isBinaryProtocolAvailable = true;
      console.debug('[BinaryAudioAdapter] 二进制协议可用');
    } catch (error) {
      this.isBinaryProtocolAvailable = false;
      console.warn('[BinaryAudioAdapter] 二进制协议不可用:', error);
    }
  }

  /**
   * 处理音频数据
   * 
   * 根据当前配置的协议类型处理音频数据
   * 
   * @param data 音频数据（可能是JSON字符串或ArrayBuffer）
   * @returns 处理后的音频载荷
   */
  public processAudioData(data: string | ArrayBuffer): AudioChunkPayload {
    const startTime = performance.now();

    try {
      let result: AudioChunkPayload;

      switch (this.currentProtocol) {
        case AudioProtocolType.BINARY:
          result = this.processBinaryAudioData(data as ArrayBuffer);
          break;
        case AudioProtocolType.JSON:
          result = this.processJsonAudioData(data as string);
          break;
        case AudioProtocolType.AUTO:
          result = this.autoDetectAndProcess(data);
          break;
        default:
          throw new Error(`不支持的协议类型: ${this.currentProtocol}`);
      }

      // 重置失败计数
      this.consecutiveFailures = 0;

      // 更新性能指标
      this.updatePerformanceMetrics(true, performance.now() - startTime);

      return result;

    } catch (error) {
      // 增加失败计数
      this.consecutiveFailures++;

      // 更新性能指标
      this.updatePerformanceMetrics(false, performance.now() - startTime);

      // 检查是否需要切换协议
      if (this.config.enableAutoSwitch && this.shouldSwitchProtocol()) {
        this.switchProtocol();
      }

      // 如果启用了回退，尝试使用备用协议
      if (this.config.binaryConfig.fallbackToJson && this.currentProtocol === AudioProtocolType.BINARY) {
        console.warn('[BinaryAudioAdapter] 二进制协议失败，回退到JSON协议:', error);
        return this.processJsonAudioData(data as string);
      }

      throw error;
    }
  }

  /**
   * 处理二进制音频数据
   */
  private processBinaryAudioData(buffer: ArrayBuffer): AudioChunkPayload {
    if (!(buffer instanceof ArrayBuffer)) {
      throw new Error('二进制协议需要 ArrayBuffer 类型的数据');
    }

    const decodedAudio = decodeResponseAudioDeltaMessage(buffer, this.config.binaryConfig);

    return {
      type: "audio_chunk",
      data: this.arrayBufferToBase64(decodedAudio.audioData.buffer.slice(0) as ArrayBuffer),
      sample_rate: 16000,
      channels: 1,
      responseId: decodedAudio.responseId,
      itemId: decodedAudio.itemId,
      outputIndex: decodedAudio.outputIndex,
      contentIndex: decodedAudio.contentIndex
    };
  }

  /**
   * 处理JSON音频数据
   */
  private processJsonAudioData(jsonString: string): AudioChunkPayload {
    if (typeof jsonString !== 'string') {
      throw new Error('JSON协议需要字符串类型的数据');
    }

    try {
      const audioEvent: ResponseAudioDeltaEvent = JSON.parse(jsonString);

      if (!audioEvent || typeof audioEvent.delta !== 'string') {
        throw new Error('无效的音频事件格式');
      }

      return {
        type: "audio_chunk",
        data: audioEvent.delta,
        sample_rate: 16000,
        channels: 1,
        responseId: audioEvent.response_id,
        itemId: audioEvent.item_id,
        outputIndex: audioEvent.output_index,
        contentIndex: audioEvent.content_index
      };

    } catch (error) {
      throw new Error(`JSON解析失败: ${error instanceof Error ? error.message : String(error)}`);
    }
  }

  /**
   * 自动检测并处理音频数据
   */
  private autoDetectAndProcess(data: string | ArrayBuffer): AudioChunkPayload {
    // 如果是ArrayBuffer，尝试二进制协议
    if (data instanceof ArrayBuffer) {
      if (this.isBinaryProtocolAvailable) {
        return this.processBinaryAudioData(data);
      } else {
        throw new Error('二进制协议不可用，无法处理 ArrayBuffer 数据');
      }
    }

    // 如果是字符串，尝试JSON协议
    if (typeof data === 'string') {
      return this.processJsonAudioData(data);
    }

    throw new Error('无法识别的数据类型，期望 ArrayBuffer 或 string');
  }

  /**
   * 判断是否应该切换协议
   */
  private shouldSwitchProtocol(): boolean {
    return this.consecutiveFailures >= this.config.switchThreshold;
  }

  /**
   * 切换协议
   */
  private switchProtocol(): void {
    const oldProtocol = this.currentProtocol;

    if (this.currentProtocol === AudioProtocolType.BINARY && this.isBinaryProtocolAvailable) {
      this.currentProtocol = AudioProtocolType.JSON;
    } else if (this.currentProtocol === AudioProtocolType.JSON && this.isBinaryProtocolAvailable) {
      this.currentProtocol = AudioProtocolType.BINARY;
    }

    if (oldProtocol !== this.currentProtocol) {
      console.info(`[BinaryAudioAdapter] 协议切换: ${oldProtocol} -> ${this.currentProtocol}`);
      this.consecutiveFailures = 0;
    }
  }

  /**
   * 更新配置
   */
  public updateConfig(config: Partial<AudioAdapterConfig>): void {
    this.config = { ...this.config, ...config };

    // 如果协议类型改变，重置失败计数
    if (config.protocolType && config.protocolType !== this.currentProtocol) {
      this.currentProtocol = config.protocolType;
      this.consecutiveFailures = 0;
      console.info(`[BinaryAudioAdapter] 协议类型已更新为: ${this.currentProtocol}`);
    }
  }

  /**
   * 获取当前配置
   */
  public getConfig(): Readonly<AudioAdapterConfig> {
    return Object.freeze({ ...this.config });
  }

  /**
   * 获取当前协议类型
   */
  public getCurrentProtocol(): AudioProtocolType {
    return this.currentProtocol;
  }

  /**
   * 获取性能指标
   */
  public getPerformanceMetrics(): BinaryAudioPerformanceMetrics {
    return { ...this.performanceMetrics };
  }

  /**
   * 重置性能指标
   */
  public resetPerformanceMetrics(): void {
    this.performanceMetrics = this.createInitialMetrics();
  }

  /**
   * 手动设置协议类型
   */
  public setProtocolType(protocolType: AudioProtocolType): void {
    if (protocolType !== this.currentProtocol) {
      console.info(`[BinaryAudioAdapter] 手动设置协议类型: ${this.currentProtocol} -> ${protocolType}`);
      this.currentProtocol = protocolType;
      this.consecutiveFailures = 0;
    }
  }

  /**
   * 检查二进制协议是否可用
   */
  public isBinaryAvailable(): boolean {
    return this.isBinaryProtocolAvailable;
  }

  /**
   * 获取适配器状态
   */
  public getStatus(): {
    currentProtocol: AudioProtocolType;
    binaryAvailable: boolean;
    consecutiveFailures: number;
    config: Readonly<AudioAdapterConfig>;
  } {
    return Object.freeze({
      currentProtocol: this.currentProtocol,
      binaryAvailable: this.isBinaryProtocolAvailable,
      consecutiveFailures: this.consecutiveFailures,
      config: this.getConfig()
    });
  }

  /**
   * 创建初始性能指标
   */
  private createInitialMetrics(): BinaryAudioPerformanceMetrics {
    return {
      decodeOperations: 0,
      successfulDecodes: 0,
      failedDecodes: 0,
      averageDecodeTime: 0,
      totalBytesDecoded: 0,
      lastDecodeTime: null,
      errorCounts: {}
    };
  }

  /**
   * 更新性能指标
   */
  private updatePerformanceMetrics(success: boolean, duration: number): void {
    this.performanceMetrics.decodeOperations++;
    this.performanceMetrics.lastDecodeTime = duration;

    if (success) {
      this.performanceMetrics.successfulDecodes++;
    } else {
      this.performanceMetrics.failedDecodes++;
    }

    // 计算平均解码时间
    this.performanceMetrics.averageDecodeTime =
      (this.performanceMetrics.averageDecodeTime * (this.performanceMetrics.decodeOperations - 1) + duration) /
      this.performanceMetrics.decodeOperations;

    // 仅在启用监控且每100次操作输出一次日志，减少性能影响
    if (this.config.enablePerformanceMonitoring && this.performanceMetrics.decodeOperations % 100 === 0) {
      console.debug('[BinaryAudioAdapter] 性能指标更新:', {
        operations: this.performanceMetrics.decodeOperations,
        successRate: (this.performanceMetrics.successfulDecodes / this.performanceMetrics.decodeOperations * 100).toFixed(1) + '%',
        averageTime: this.performanceMetrics.averageDecodeTime.toFixed(2) + 'ms',
        currentProtocol: this.currentProtocol
      });
    }
  }

  /**
   * ArrayBuffer转Base64
   */
  private arrayBufferToBase64(buffer: ArrayBuffer): string {
    const bytes = new Uint8Array(buffer);
    // 使用更高效的二进制字符串创建方式
    return btoa(String.fromCharCode.apply(null, bytes as unknown as number[]));
  }
}

/**
 * 创建默认的音频适配器实例
 */
export function createDefaultAudioAdapter(): BinaryAudioAdapter {
  return new BinaryAudioAdapter({
    protocolType: AudioProtocolType.AUTO,
    enableAutoSwitch: true,
    enablePerformanceMonitoring: true,
    binaryConfig: {
      enabled: true,
      fallbackToJson: true,
      enablePerformanceMonitoring: true
    }
  });
}