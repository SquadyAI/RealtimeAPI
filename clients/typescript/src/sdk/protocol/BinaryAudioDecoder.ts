/**
 * 二进制音频协议解码器
 *
 * 这个文件实现了二进制音频协议的解码功能，包括：
 * - 主要的解码函数 decodeResponseAudioDeltaMessage
 * - 协议头解析
 * - 载荷数据解析
 * - 性能监控和错误处理
 *
 * 注意：延迟计算方式已从 response.audio.delta 改为 binary_audio_delta
 * 二进制协议提供更精确的延迟测量能力
 */

import {
  DecodingError,
  BINARY_AUDIO_PROTOCOL,
  DEFAULT_BINARY_AUDIO_CONFIG
} from './BinaryAudioTypes';

import type {
  DecodedAudioDeltaMessage,
  BinaryAudioProtocolConfig,
  BinaryAudioPerformanceMetrics,
  DecodingContext,
  DecodingResult
} from './BinaryAudioTypes';

/**
 * 解码响应音频增量消息
 *
 * 这是主要的解码函数，将二进制 ArrayBuffer 解码为结构化的音频消息
 * 支持新的 binary_audio_delta 延迟计算方式
 *
 * @param buffer - 包含二进制音频消息的 ArrayBuffer
 * @param config - 可选的配置选项
 * @returns 解码后的音频增量消息
 * @throws {DecodingError} 当解码过程中发生错误时
 */
export function decodeResponseAudioDeltaMessage(
  buffer: ArrayBuffer,
  config: Partial<BinaryAudioProtocolConfig> = {}
): DecodedAudioDeltaMessage {
  const finalConfig = { ...DEFAULT_BINARY_AUDIO_CONFIG, ...config };
  const context: DecodingContext = {
    config: finalConfig,
    metrics: createInitialMetrics(),
    startTime: performance.now()
  };

  try {
    // 基本验证
    validateBuffer(buffer);

    // 解析协议头
    const header = parseHeader(buffer, context);

    // 验证协议和命令ID
    validateProtocolAndCommand(header, context);

    // 解析载荷数据
    const payload = parsePayload(buffer, context);

    // 更新性能指标
    updateMetrics(context, true, buffer.byteLength);

    if (finalConfig.enableDebugLogging) {
      console.debug('[BinaryAudioDecoder] 解码成功:', {
        responseId: payload.responseId,
        itemId: payload.itemId,
        outputIndex: payload.outputIndex,
        contentIndex: payload.contentIndex,
        audioDataSize: payload.audioData.length,
        decodeTime: performance.now() - context.startTime
      });
    }

    return payload;
  } catch (error) {
    updateMetrics(context, false, buffer.byteLength);

    if (error instanceof DecodingError) {
      throw error;
    }

    // 将未知错误包装为 DecodingError
    throw DecodingError.corruptedData(
      `未知解码错误: ${error instanceof Error ? error.message : String(error)}`,
      error instanceof Error ? error : undefined
    );
  }
}

/**
 * 带性能监控的解码函数
 * 
 * 返回详细的解码结果，包括性能指标
 * 
 * @param buffer - 包含二进制音频消息的 ArrayBuffer
 * @param config - 可选的配置选项
 * @returns 解码结果，包含数据、错误信息和性能指标
 */
export function decodeResponseAudioDeltaMessageWithMetrics(
  buffer: ArrayBuffer,
  config: Partial<BinaryAudioProtocolConfig> = {}
): DecodingResult {
  const startTime = performance.now();

  try {
    const data = decodeResponseAudioDeltaMessage(buffer, config);
    const duration = performance.now() - startTime;

    return {
      success: true,
      data,
      duration,
      bytesProcessed: buffer.byteLength
    };
  } catch (error) {
    const duration = performance.now() - startTime;

    return {
      success: false,
      error: error instanceof DecodingError ? error : DecodingError.corruptedData(
        `解码失败: ${error instanceof Error ? error.message : String(error)}`
      ),
      duration,
      bytesProcessed: buffer.byteLength
    };
  }
}

/**
 * 验证缓冲区的基本有效性
 */
function validateBuffer(buffer: ArrayBuffer): void {
  if (!(buffer instanceof ArrayBuffer)) {
    throw DecodingError.corruptedData('输入必须是 ArrayBuffer');
  }

  if (buffer.byteLength < BINARY_AUDIO_PROTOCOL.HEADER_SIZE) {
    throw DecodingError.bufferTooSmall(
      BINARY_AUDIO_PROTOCOL.HEADER_SIZE,
      buffer.byteLength
    );
  }
}

/**
 * 解析协议头
 */
function parseHeader(buffer: ArrayBuffer, _context: DecodingContext) {
  const view = new DataView(buffer);
  const uint8Array = new Uint8Array(buffer);

  // 提取会话ID (16字节)
  const sessionIdBytes = uint8Array.subarray(0, BINARY_AUDIO_PROTOCOL.SESSION_ID_SIZE);
  const sessionId = new TextDecoder().decode(sessionIdBytes);

  // 提取协议ID (1字节)
  const protocolId = view.getUint8(BINARY_AUDIO_PROTOCOL.PROTOCOL_ID_OFFSET);

  // 提取命令ID (1字节)
  const commandId = view.getUint8(BINARY_AUDIO_PROTOCOL.COMMAND_ID_OFFSET);

  // 提取保留字段 (14字节)
  const reservedBytes = uint8Array.subarray(
    BINARY_AUDIO_PROTOCOL.COMMAND_ID_OFFSET + 1,
    BINARY_AUDIO_PROTOCOL.HEADER_SIZE
  );

  return {
    sessionId,
    protocolId,
    commandId,
    reservedBytes
  };
}

/**
 * 验证协议和命令ID
 */
function validateProtocolAndCommand(
  header: { protocolId: number; commandId: number },
  context: DecodingContext
): void {
  // 验证协议ID
  if (header.protocolId !== BINARY_AUDIO_PROTOCOL.PROTOCOL_ID.ALL) {
    throw DecodingError.invalidProtocolId(
      BINARY_AUDIO_PROTOCOL.PROTOCOL_ID.ALL,
      header.protocolId
    );
  }

  // 验证命令ID - 支持多种音频命令ID
  const validAudioCommandIds = [
    BINARY_AUDIO_PROTOCOL.COMMAND_ID.RESPONSE_AUDIO_DELTA,
    BINARY_AUDIO_PROTOCOL.COMMAND_ID.AUDIO_DELTA
  ];

  if (!validAudioCommandIds.includes(header.commandId as any)) {
    throw DecodingError.invalidCommandId(
      25, // 主要期望的命令ID
      header.commandId
    );
  }

  if (context.config.enableDebugLogging) {
    console.debug('[BinaryAudioDecoder] 协议头验证通过:', {
      protocolId: header.protocolId,
      commandId: header.commandId
    });
  }
}

/**
 * 解析载荷数据
 */
function parsePayload(buffer: ArrayBuffer, _context: DecodingContext): DecodedAudioDeltaMessage {
  const view = new DataView(buffer);
  const uint8Array = new Uint8Array(buffer);

  let offset = BINARY_AUDIO_PROTOCOL.HEADER_SIZE;

  // 解析 responseId (4字节长度 + 字符串)
  const responseIdLength = view.getUint32(offset, true); // 小端序
  offset += 4;

  if (responseIdLength > BINARY_AUDIO_PROTOCOL.STRING_LIMITS.RESPONSE_ID_MAX_LENGTH) {
    throw DecodingError.invalidStringLength(
      'responseId',
      BINARY_AUDIO_PROTOCOL.STRING_LIMITS.RESPONSE_ID_MAX_LENGTH,
      responseIdLength
    );
  }

  const responseIdBytes = uint8Array.subarray(offset, offset + responseIdLength);
  const responseId = new TextDecoder().decode(responseIdBytes);
  offset += responseIdLength;

  // 解析 itemId (4字节长度 + 字符串)
  const itemIdLength = view.getUint32(offset, true);
  offset += 4;

  if (itemIdLength > BINARY_AUDIO_PROTOCOL.STRING_LIMITS.ITEM_ID_MAX_LENGTH) {
    throw DecodingError.invalidStringLength(
      'itemId',
      BINARY_AUDIO_PROTOCOL.STRING_LIMITS.ITEM_ID_MAX_LENGTH,
      itemIdLength
    );
  }

  const itemIdBytes = uint8Array.subarray(offset, offset + itemIdLength);
  const itemId = new TextDecoder().decode(itemIdBytes);
  offset += itemIdLength;

  // 解析 outputIndex (4字节)
  const outputIndex = view.getUint32(offset, true);
  offset += 4;

  // 解析 contentIndex (4字节)
  const contentIndex = view.getUint32(offset, true);
  offset += 4;

  // 剩余数据为音频数据
  const audioData = uint8Array.subarray(offset);

  if (audioData.length === 0) {
    throw DecodingError.corruptedData('音频数据为空');
  }

  return {
    responseId,
    itemId,
    outputIndex,
    contentIndex,
    audioData
  };
}

/**
 * 创建初始性能指标
 */
function createInitialMetrics(): BinaryAudioPerformanceMetrics {
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
function updateMetrics(
  context: DecodingContext,
  success: boolean,
  bytesProcessed: number
): void {
  const metrics = context.metrics;
  const decodeTime = performance.now() - context.startTime;

  metrics.decodeOperations++;
  metrics.totalBytesDecoded += bytesProcessed;
  metrics.lastDecodeTime = decodeTime;

  if (success) {
    metrics.successfulDecodes++;
  } else {
    metrics.failedDecodes++;
  }

  // 计算平均解码时间
  metrics.averageDecodeTime =
    (metrics.averageDecodeTime * (metrics.decodeOperations - 1) + decodeTime) /
    metrics.decodeOperations;

  if (context.config.enablePerformanceMonitoring) {
    console.debug('[BinaryAudioDecoder] 性能指标更新:', {
      operations: metrics.decodeOperations,
      successRate: (metrics.successfulDecodes / metrics.decodeOperations * 100).toFixed(1) + '%',
      averageTime: metrics.averageDecodeTime.toFixed(2) + 'ms',
      totalTime: (metrics.totalBytesDecoded / 1024).toFixed(1) + 'KB'
    });
  }
}

/**
 * 创建模拟二进制数据包（用于测试）
 * 
 * 这个函数创建一个符合协议格式的测试数据包
 * 
 * @param options - 可选的自定义选项
 * @returns 模拟的二进制数据包
 */
export function createMockBinaryPacket(options: {
  sessionId?: string;
  responseId?: string;
  itemId?: string;
  outputIndex?: number;
  contentIndex?: number;
  audioData?: Uint8Array;
} = {}): ArrayBuffer {
  const {
    sessionId = "01H2X3J4K5L6M7N8O9P0Q1R2S3",
    responseId = "resp-12345",
    itemId = "item-abcde",
    outputIndex = 0,
    contentIndex = 5,
    audioData = new TextEncoder().encode('binary-audio-stream-data')
  } = options;

  // 确保sessionId是16字节
  const sessionIdBytes = new TextEncoder().encode(sessionId.substring(0, 16));

  // 计算载荷大小
  const payloadSize = 4 + responseId.length + 4 + itemId.length + 4 + 4 + audioData.length;
  const totalSize = BINARY_AUDIO_PROTOCOL.HEADER_SIZE + payloadSize;

  // 创建缓冲区
  const buffer = new ArrayBuffer(totalSize);
  const view = new DataView(buffer);
  const uint8Array = new Uint8Array(buffer);

  let offset = 0;

  // 写入协议头
  uint8Array.set(sessionIdBytes, offset);
  offset += BINARY_AUDIO_PROTOCOL.SESSION_ID_SIZE;

  view.setUint8(offset, BINARY_AUDIO_PROTOCOL.PROTOCOL_ID.ALL);
  offset += 1;

  view.setUint8(offset, BINARY_AUDIO_PROTOCOL.COMMAND_ID.RESPONSE_AUDIO_DELTA);
  offset += 1;

  // 保留字段（14字节）
  const reserved = new Uint8Array(BINARY_AUDIO_PROTOCOL.RESERVED_SIZE);
  uint8Array.set(reserved, offset);
  offset += BINARY_AUDIO_PROTOCOL.RESERVED_SIZE;

  // 写入载荷（小端序）
  view.setUint32(offset, responseId.length, true);
  offset += 4;
  uint8Array.set(new TextEncoder().encode(responseId), offset);
  offset += responseId.length;

  view.setUint32(offset, itemId.length, true);
  offset += 4;
  uint8Array.set(new TextEncoder().encode(itemId), offset);
  offset += itemId.length;

  view.setUint32(offset, outputIndex, true);
  offset += 4;

  view.setUint32(offset, contentIndex, true);
  offset += 4;

  uint8Array.set(audioData, offset);

  return buffer;
}