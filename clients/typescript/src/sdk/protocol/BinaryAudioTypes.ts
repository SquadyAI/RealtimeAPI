/**
 * 二进制音频协议类型定义
 *
 * 这个文件定义了用于二进制音频协议的所有类型，包括：
 * - 解码后的音频消息结构
 * - 解码错误类型
 * - 协议常量
 * - 配置选项
 *
 * 注意：延迟计算方式已从 response.audio.delta 改为 binary_audio_delta
 * 二进制协议提供更精确的延迟测量能力
 */

/**
 * 解码后的音频增量消息
 *
 * 包含从二进制数据包中解析出的所有音频相关信息
 * 这是新的延迟计算方式，替代原有的 response.audio.delta
 */
export interface DecodedAudioDeltaMessage {
  /** 响应ID，用于标识唯一的响应会话 */
  responseId: string;
  /** 项目ID，用于标识响应中的具体项目 */
  itemId: string;
  /** 输出索引，用于标识多个输出中的哪一个 */
  outputIndex: number;
  /** 内容索引，用于标识内容块的位置 */
  contentIndex: number;
  /** 音频数据，原始PCM16格式 */
  audioData: Uint8Array;
}

/**
 * 二进制协议解码错误
 * 
 * 当解码过程中发生错误时抛出的异常类型
 */
export class DecodingError extends Error {
  /** 错误代码，用于程序化处理不同类型的错误 */
  public readonly code: string;
  /** 导致错误的原因（可选） */
  public override readonly cause?: Error;

  constructor(code: string, message: string, cause?: Error) {
    super(message);
    this.name = 'DecodingError';
    this.code = code;
    this.cause = cause;

    // 保持原型链的正确性
    if (Object.setPrototypeOf) {
      Object.setPrototypeOf(this, DecodingError.prototype);
    }
  }

  /**
   * 创建缓冲区长度错误
   */
  static bufferTooSmall(expected: number, actual: number): DecodingError {
    return new DecodingError(
      'BUFFER_TOO_SMALL',
      `缓冲区太小：期望至少 ${expected} 字节，实际 ${actual} 字节`,
      undefined
    );
  }

  /**
   * 创建协议ID错误
   */
  static invalidProtocolId(expected: number, actual: number): DecodingError {
    return new DecodingError(
      'INVALID_PROTOCOL_ID',
      `无效的协议ID：期望 ${expected}，实际 ${actual}`,
      undefined
    );
  }

  /**
   * 创建命令ID错误
   */
  static invalidCommandId(expected: number, actual: number): DecodingError {
    return new DecodingError(
      'INVALID_COMMAND_ID',
      `无效的命令ID：期望 ${expected}，实际 ${actual}`,
      undefined
    );
  }

  /**
   * 创建字符串长度错误
   */
  static invalidStringLength(field: string, maxLength: number, actual: number): DecodingError {
    return new DecodingError(
      'INVALID_STRING_LENGTH',
      `无效的${field}长度：最大 ${maxLength}，实际 ${actual}`,
      undefined
    );
  }

  /**
   * 创建数据损坏错误
   */
  static corruptedData(message: string, cause?: Error): DecodingError {
    return new DecodingError(
      'CORRUPTED_DATA',
      `数据损坏：${message}`,
      cause
    );
  }
}

/**
 * 二进制协议常量
 * 
 * 定义了二进制协议中使用的各种常量值
 */
export const BINARY_AUDIO_PROTOCOL = {
  /** 协议头大小（字节） */
  HEADER_SIZE: 32,
  /** 会话ID大小（字节） */
  SESSION_ID_SIZE: 16,
  /** 协议ID偏移量 */
  PROTOCOL_ID_OFFSET: 16,
  /** 命令ID偏移量 */
  COMMAND_ID_OFFSET: 17,
  /** 保留字段大小（字节） */
  RESERVED_SIZE: 14,
  
  /** 协议ID值 */
  PROTOCOL_ID: {
    ALL: 100, // 全协议
    ASR: 1,   // ASR协议
    LLM: 2,   // LLM协议
    TTS: 3,   // TTS协议
    CONTROL: 0 // 控制协议
  } as const,
  
  /** 命令ID值 */
  COMMAND_ID: {
    RESPONSE_AUDIO_DELTA: 25, // 响应音频增量命令
    AUDIO_CHUNK: 3,          // 音频块命令
    AUDIO_DELTA: 20,         // 音频增量命令（服务器实际发送的）
    TEXT_DATA: 4,            // 文本数据命令
    IMAGE_DATA: 6,           // 图像数据命令
    PING: 7,                 // Ping命令
    START: 1,                // 开始命令
    STOP: 2,                 // 停止命令
    RESULT: 100,             // 结果命令
    ERROR: 255               // 错误命令
  } as const,
  
  /** 字符串长度限制 */
  STRING_LIMITS: {
    RESPONSE_ID_MAX_LENGTH: 256,
    ITEM_ID_MAX_LENGTH: 256
  } as const
} as const;

/**
 * 二进制音频协议配置
 * 
 * 用于配置二进制协议的行为选项
 */
export interface BinaryAudioProtocolConfig {
  /** 是否启用二进制协议 */
  enabled: boolean;
  /** 是否在错误时回退到JSON协议 */
  fallbackToJson: boolean;
  /** 是否启用性能监控 */
  enablePerformanceMonitoring: boolean;
  /** 是否启用调试日志 */
  enableDebugLogging: boolean;
  /** 最大重试次数 */
  maxRetries: number;
  /** 解码超时时间（毫秒） */
  decodeTimeout: number;
}

/**
 * 默认配置
 */
export const DEFAULT_BINARY_AUDIO_CONFIG: BinaryAudioProtocolConfig = {
  enabled: false, // 默认禁用，需要显式启用
  fallbackToJson: true,
  enablePerformanceMonitoring: true,
  enableDebugLogging: true,
  maxRetries: 3,
  decodeTimeout: 1000 // 1秒超时
} as const;

/**
 * 性能指标接口
 *
 * 用于跟踪二进制协议的性能表现
 * 包括基于 binary_audio_delta 事件的高精度延迟计算
 */
export interface BinaryAudioPerformanceMetrics {
  /** 解码操作计数 */
  decodeOperations: number;
  /** 成功解码计数 */
  successfulDecodes: number;
  /** 失败解码计数 */
  failedDecodes: number;
  /** 平均解码时间（毫秒） */
  averageDecodeTime: number;
  /** 总解码字节数 */
  totalBytesDecoded: number;
  /** 最后解码时间 */
  lastDecodeTime: number | null;
  /** 错误类型统计 */
  errorCounts: Record<string, number>;
}

/**
 * 解码上下文
 * 
 * 包含解码过程中需要的上下文信息
 */
export interface DecodingContext {
  /** 配置选项 */
  config: BinaryAudioProtocolConfig;
  /** 性能指标 */
  metrics: BinaryAudioPerformanceMetrics;
  /** 解码开始时间 */
  startTime: number;
}

/**
 * 解码结果
 * 
 * 封装解码操作的结果，包括成功和失败的情况
 */
export interface DecodingResult<T = DecodedAudioDeltaMessage> {
  /** 是否成功 */
  success: boolean;
  /** 解码后的数据（成功时） */
  data?: T;
  /** 错误信息（失败时） */
  error?: DecodingError;
  /** 解码耗时（毫秒） */
  duration: number;
  /** 处理的字节数 */
  bytesProcessed: number;
}

// 类型导出
export type ProtocolId = typeof BINARY_AUDIO_PROTOCOL.PROTOCOL_ID[keyof typeof BINARY_AUDIO_PROTOCOL.PROTOCOL_ID];
export type CommandId = typeof BINARY_AUDIO_PROTOCOL.COMMAND_ID[keyof typeof BINARY_AUDIO_PROTOCOL.COMMAND_ID];