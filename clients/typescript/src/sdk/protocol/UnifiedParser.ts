import type {
  WebSocketMessage,
  MessagePayload,
  ProtocolId,
  AudioChunkPayload,
  TextDataPayload
} from './ClientProtocol';
import {
  ClientBinaryMessage,
  CommandId,
  BinaryHeader
} from './ClientProtocol';

import {
  decodeResponseAudioDeltaMessage,
  decodeResponseAudioDeltaMessageWithMetrics
} from './BinaryAudioDecoder';

import type {
  BinaryAudioProtocolConfig,
  DecodingResult
} from './BinaryAudioTypes';

import {
  BINARY_AUDIO_PROTOCOL,
  DEFAULT_BINARY_AUDIO_CONFIG
} from './BinaryAudioTypes';

/**
 * 统一消息解析器接口
 */
export interface MessageParser<T> {
  /**
   * 解析消息
   * @param data 消息数据
   * @returns 解析后的消息对象
   */
  parse(data: T): ParsedMessage;

  /**
   * 序列化消息
   * @param message 消息对象
   * @returns 序列化后的数据
   */
  serialize(message: ParsedMessage): T;
}

/**
 * 解析后的消息结构
 */
export interface ParsedMessage {
  /** 协议ID */
  protocolId: ProtocolId;
  /** 命令ID */
  commandId: CommandId;
  /** 会话ID */
  sessionId: string;
  /** 载荷数据 */
  payload?: MessagePayload;
  /** 消息类型 */
  messageType: 'websocket' | 'binary';
  /** 原始数据 */
  rawData: any;
}

/**
 * WebSocket消息解析器
 */
export class WebSocketMessageParser implements MessageParser<string> {
  /**
   * 解析WebSocket JSON消息
   * @param json JSON字符串
   * @returns 解析后的消息对象
   */
  parse(json: string): ParsedMessage {
    try {
      const message: WebSocketMessage = JSON.parse(json);

      return {
        protocolId: message.protocol_id,
        commandId: message.command_id,
        sessionId: message.session_id,
        payload: message.payload,
        messageType: 'websocket',
        rawData: message
      };
    } catch (error) {
      throw new Error(`Failed to parse WebSocket message: ${error instanceof Error ? error.message : String(error)}`);
    }
  }

  /**
   * 序列化消息为JSON字符串
   * @param message 消息对象
   * @returns JSON字符串
   */
  serialize(message: ParsedMessage): string {
    const wsMessage: WebSocketMessage = {
      protocol_id: message.protocolId,
      command_id: message.commandId,
      session_id: message.sessionId,
      payload: message.payload
    };

    return JSON.stringify(wsMessage);
  }
}

/**
 * 二进制消息解析器
 */
export class BinaryMessageParser implements MessageParser<ArrayBuffer> {
  /** 二进制音频协议配置 */
  private binaryAudioConfig: BinaryAudioProtocolConfig;

  constructor(config: Partial<BinaryAudioProtocolConfig> = {}) {
    this.binaryAudioConfig = { ...DEFAULT_BINARY_AUDIO_CONFIG, ...config };
  }

  /**
   * 更新二进制音频配置
   * @param config 新的配置选项
   */
  updateBinaryAudioConfig(config: Partial<BinaryAudioProtocolConfig>): void {
    this.binaryAudioConfig = { ...this.binaryAudioConfig, ...config };
  }

  /**
   * 解析二进制消息
   * @param data 二进制数据
   * @returns 解析后的消息对象
   */
  parse(data: ArrayBuffer | SharedArrayBuffer): ParsedMessage {
    try {
      const uint8Array = new Uint8Array(data);
      const binaryMessage = ClientBinaryMessage.fromBytes(uint8Array);

      return {
        protocolId: binaryMessage.header.protocolId,
        commandId: binaryMessage.header.commandId,
        sessionId: binaryMessage.header.sessionId,
        payload: this.extractPayload(binaryMessage),
        messageType: 'binary',
        rawData: binaryMessage
      };
    } catch (error) {
      throw new Error(`Failed to parse binary message: ${error instanceof Error ? error.message : String(error)}`);
    }
  }

  /**
   * 序列化消息为二进制数据
   * @param message 消息对象
   * @returns 二进制数据
   */
  serialize(message: ParsedMessage): ArrayBuffer {
    // 创建二进制消息头部
    const header = new BinaryHeader(
      message.sessionId,
      message.protocolId,
      message.commandId
    );

    // 创建二进制消息
    const clientMessage = new ClientBinaryMessage(
      header,
      this.encodePayload(message.payload)
    );

    // 转换为字节数组
    const bytes = clientMessage.toBytes();
    // 直接返回底层缓冲区，最小化性能开销
    // 支持 ArrayBuffer 和 SharedArrayBuffer
    return bytes.buffer as ArrayBuffer;
  }

  /**
   * 从二进制消息中提取载荷
   * @param binaryMessage 二进制消息
   * @returns 消息载荷
   */
  private extractPayload(binaryMessage: ClientBinaryMessage): MessagePayload | undefined {
    // 根据命令ID和协议ID确定载荷类型
    switch (binaryMessage.header.commandId) {
      case CommandId.AudioChunk: {
        // 音频数据载荷
        // 检查是否启用新的二进制音频协议
        if (this.binaryAudioConfig.enabled &&
          binaryMessage.header.protocolId === BINARY_AUDIO_PROTOCOL.PROTOCOL_ID.ALL) {
          return this.extractBinaryAudioPayload(binaryMessage);
        }

        // 传统音频块处理
        const buffer = binaryMessage.payload.buffer;
        return {
          type: "audio_chunk",
          data: this.arrayBufferToBase64(buffer),
          sample_rate: 16000,
          channels: 1
        };
      }

      case CommandId.TextData: {
        // 文本数据载荷
        const text = new TextDecoder().decode(binaryMessage.payload);
        return {
          type: "text_data",
          text
        };
      }

      case CommandId.ImageData: {
        // 图像数据载荷
        const buffer = binaryMessage.payload.buffer;
        return {
          type: "image_data",
          data: this.arrayBufferToBase64(buffer),
          size: binaryMessage.payload.length
        };
      }

      case CommandId.Ping: {
        // Ping命令没有载荷
        return undefined;
      }

      default:
        // 其他命令可能没有载荷
        return undefined;
    }
  }

  /**
   * 将载荷编码为Uint8Array
   * @param payload 消息载荷
   * @returns 编码后的字节数组
   */
  private encodePayload(payload?: MessagePayload): Uint8Array {
    if (!payload) {
      return new Uint8Array(0);
    }

    switch (payload.type) {
      case "audio_chunk": {
        // 音频数据载荷
        const audioPayload = payload as AudioChunkPayload;
        return this.base64ToArrayBuffer(audioPayload.data);
      }

      case "text_data": {
        // 文本数据载荷
        const textPayload = payload as TextDataPayload;
        return new TextEncoder().encode(textPayload.text);
      }

      default:
        // 其他载荷类型
        return new Uint8Array(0);
    }
  }

  /**
   * 将ArrayBuffer转换为Base64字符串
   * @param buffer ArrayBuffer
   * @returns Base64字符串
   */
  private arrayBufferToBase64(buffer: ArrayBuffer | SharedArrayBuffer): string {
    const bytes = new Uint8Array(buffer);
    let binary = '';
    for (let i = 0; i < bytes.byteLength; i++) {
      binary += String.fromCharCode(bytes[i]);
    }
    return btoa(binary);
  }

  /**
   * 将Base64字符串转换为ArrayBuffer
   * @param base64 Base64字符串
   * @returns ArrayBuffer
   */
  private base64ToArrayBuffer(base64: string): Uint8Array {
    const binaryString = atob(base64);
    const bytes = new Uint8Array(binaryString.length);
    for (let i = 0; i < binaryString.length; i++) {
      bytes[i] = binaryString.charCodeAt(i);
    }
    return bytes;
  }

  /**
   * 提取二进制音频载荷
   * @param binaryMessage 二进制消息
   * @returns 音频载荷
   */
  private extractBinaryAudioPayload(binaryMessage: ClientBinaryMessage): MessagePayload | undefined {
    try {
      if (this.binaryAudioConfig.enableDebugLogging) {
        console.debug('[BinaryMessageParser] 尝试使用新的二进制音频协议解码');
      }

      // 使用新的解码函数
      const decodedAudio = decodeResponseAudioDeltaMessage(
        binaryMessage.payload.buffer.slice(0) as ArrayBuffer,
        this.binaryAudioConfig
      );

      if (this.binaryAudioConfig.enableDebugLogging) {
        console.debug('[BinaryMessageParser] 二进制音频解码成功:', {
          responseId: decodedAudio.responseId,
          itemId: decodedAudio.itemId,
          outputIndex: decodedAudio.outputIndex,
          contentIndex: decodedAudio.contentIndex,
          audioDataSize: decodedAudio.audioData.length
        });
      }

      // 将解码后的音频数据转换为传统格式以保持兼容性
      return {
        type: "audio_chunk",
        data: this.arrayBufferToBase64(decodedAudio.audioData.buffer),
        sample_rate: 16000,
        channels: 1,
        // 添加新的二进制音频特有的元数据
        responseId: decodedAudio.responseId,
        itemId: decodedAudio.itemId,
        outputIndex: decodedAudio.outputIndex,
        contentIndex: decodedAudio.contentIndex
      };

    } catch (error) {
      console.warn('[BinaryMessageParser] 二进制音频解码失败，回退到传统方式:', error);

      if (this.binaryAudioConfig.fallbackToJson) {
        // 回退到传统的音频块处理
        const buffer = binaryMessage.payload.buffer;
        return {
          type: "audio_chunk",
          data: this.arrayBufferToBase64(buffer),
          sample_rate: 16000,
          channels: 1
        };
      }

      // 如果不回退，则抛出错误
      throw error;
    }
  }

  /**
   * 解析二进制音频消息（独立方法，用于直接处理二进制音频数据）
   * @param buffer 二进制缓冲区
   * @returns 解码结果
   */
  public parseBinaryAudioMessage(buffer: ArrayBuffer): DecodingResult {
    return decodeResponseAudioDeltaMessageWithMetrics(buffer, this.binaryAudioConfig);
  }
}

/**
 * 解析器工厂 - 创建对应类型的解析器
 */
