import { CONFIG, getWebSocketUrl } from '../types/protocol';
import { globalLogger as logger } from '../utils/GlobalLogger';
import { CommandId, ProtocolId, ClientBinaryMessage as BinaryMessage } from '../protocol/ClientProtocol';
import type { WebSocketMessage, SessionConfigPayload, ConversationItemCreatePayload } from '../protocol/ClientProtocol';
import {
  WebSocketMessageParser,
  BinaryMessageParser,
} from '../protocol/UnifiedParser';
import { EventEmitter } from '../utils/EventEmitter';

import {
  decodeResponseAudioDeltaMessage
} from '../protocol/BinaryAudioDecoder';

import type {
  BinaryAudioProtocolConfig,
  DecodingResult,
  DecodedAudioDeltaMessage
} from '../protocol/BinaryAudioTypes';


// 解码和播放逻辑应在上层 App 中处理

// 事件类型定义
export interface ConnectionEvents {
  'open': (event: Event) => void;
  'close': (event: CloseEvent) => void;
  'error': (event: Event) => void;
}

export class CombinedConnectionManager extends EventEmitter {
  // WebSocket连接相关属性
  private websocket: WebSocket | null = null;
  private connectionAttempts: number = 0;
  private reconnectTimeout: number | null = null;

  // 会话相关属性
  private sessionId: string | undefined = undefined;
  private sessionCreated: boolean = false; // 服务端确认的会话创建状态（由 session.created 驱动）
  private defaultSessionConfig?: Partial<SessionConfigPayload>;

  // 消息解析器
  private webSocketParser: WebSocketMessageParser;
  private binaryParser: BinaryMessageParser;

  // ========== 高精度性能监控 ==========
  // 支持二进制协议优先的延迟计算：binary_audio_delta 事件优先于 response.audio.delta
  // 延迟计算方式已从 response.audio.delta 改为 binary_audio_delta
  private performanceTimers = new Map<string, number>(); // 存储各种事件的开始时间
  private latencyMetrics: Array<{
    type: string;
    startTime: number;
    endTime: number;
    latency: number;
    timestamp: number;
  }> = [];
  private maxMetricsHistory = 100; // 保留最近100次测量


  constructor(
    sessionId?: string,
    defaultSessionConfig?: Partial<SessionConfigPayload>,
    binaryAudioConfig?: Partial<BinaryAudioProtocolConfig>
  ) {
    super();
    this.webSocketParser = new WebSocketMessageParser();
    this.binaryParser = new BinaryMessageParser(binaryAudioConfig);
    this.defaultSessionConfig = defaultSessionConfig;
    if (sessionId) {
      this.sessionId = sessionId;
    }
    else {
      // 16字的随机字符串
      this.sessionId = Math.random().toString(36).substring(2, 10) + Math.random().toString(36).substring(2, 10);
    }
  }

  /**
   * 连接到WebSocket服务器
   */
  async connect(): Promise<void> {
    try {
      if (!window.WebSocket) {
        throw new Error('当前浏览器不支持WebSocket');
      }

      this.websocket = new WebSocket(getWebSocketUrl());

      this.websocket.onopen = (event: Event): void => {
        this.handleOpen(event);
        this.emit('open', event);
      };

      this.websocket.onmessage = (event: MessageEvent): void => {
        this.handleMessage(event);
      };

      this.websocket.onclose = (event: CloseEvent): void => {
        this.handleClose(event);
        this.emit('close', event);
      };

      this.websocket.onerror = (event: Event): void => {
        this.emit('error', event);
      };
    } catch (_error) {
      throw new Error("连接失败");
    }
  }

  /**
   * 断开连接
   */
  disconnect(): void {
    if (this.websocket) {
      const currentState = this.websocket.readyState;
      if (currentState === WebSocket.OPEN || currentState === WebSocket.CONNECTING) {
        this.websocket.close(1000, 'Client requested disconnection');
      }
    }

    this.cleanup();
  }

  /**
   * 发送文本消息
   */
  sendTextMessage(message: WebSocketMessage['payload'], commandID: CommandId): void {
    if (this.websocket && this.websocket.readyState === WebSocket.OPEN) {
      const fullMessage = {
        protocol_id: ProtocolId.All,
        command_id: commandID,
        session_id: this.sessionId!,
        payload: message
      } satisfies WebSocketMessage;

      const messageStr = JSON.stringify(fullMessage);

      // 性能优化：减少详细日志输出
      if (message && typeof message === 'object' && 'type' in message && message.type === 'session_config') {
        const sessionConfig = message as SessionConfigPayload;
        logger.info(`[CLIENT] Sending WebSocket message with asr_language: ${sessionConfig.asr_language}`);
        // 移除详细的完整消息日志
      }

      try {
        this.websocket.send(messageStr);
        // 移除频繁的成功发送日志
      } catch (error) {
        logger.error(`发送文本消息失败: ${error instanceof Error ? error.message : String(error)}`);
      }
    } else {
      logger.error('WebSocket未连接，无法发送消息');
    }
  }

  /**
   * 发送 stopInput 消息（VAD语音结束时调用）
   * 方式1：WebSocket JSON 消息（推荐，带payload）
   * 方式2：二进制消息（无payload）
   * @param eventId 事件ID
   * @param audioEndMs 音频结束时间戳（毫秒）
   * @param itemId 消息项ID
   * @param useBinary 是否使用二进制格式（默认false，使用JSON格式）
   */
  sendStopInput(eventId?: string, audioEndMs?: number, itemId?: string, useBinary: boolean = false): void {
    if (!this.websocket || this.websocket.readyState !== WebSocket.OPEN) {
      logger.error('[StopInput] WebSocket未连接，无法发送stopInput');
      return;
    }

    if (!this.sessionId) {
      logger.error('[StopInput] sessionId为空，无法发送stopInput');
      return;
    }

    try {
      if (useBinary) {
        // 方式2：二进制消息
        const binaryMessage = BinaryMessage.createStopInput(this.sessionId);
        const bytes = binaryMessage.toBytes();
        logger.info(`[StopInput] 发送二进制stopInput消息 - 会话ID: ${this.sessionId}, 大小: ${bytes.length}字节`);
        this.websocket.send(bytes);
      } else {
        // 方式1：WebSocket JSON 消息（推荐）
        const stopInputPayload = {
          type: "input_audio_buffer.speech_stopped",
          event_id: eventId || `event_${Date.now()}`,
          audio_end_ms: audioEndMs || Math.floor(Date.now()),
          item_id: itemId || ''
        };

        const fullMessage = {
          protocol_id: ProtocolId.Asr,
          command_id: CommandId.StopInput,
          session_id: this.sessionId,
          payload: stopInputPayload
        } satisfies WebSocketMessage;

        const messageStr = JSON.stringify(fullMessage);
        logger.info(`[StopInput] 发送JSON stopInput消息 - 会话ID: ${this.sessionId}, payload:`, stopInputPayload);
        this.websocket.send(messageStr);
      }
    } catch (error) {
      logger.error(`[StopInput] 发送失败: ${error instanceof Error ? error.message : String(error)}`);
    }
  }

  /**
   * 工具调用闭环：把本地执行结果回传给服务端（conversation.item.create + function_call_output）
   * 备注：output 必须是字符串；这里对对象做 JSON.stringify
   */
  sendFunctionCallOutput(callId: string, output: unknown): void {
    if (!callId) {
      logger.error('[ToolCall] callId 为空，无法回传 function_call_output');
      return;
    }

    const payload: ConversationItemCreatePayload = {
      type: "conversation.item.create",
      item: {
        type: "function_call_output",
        call_id: callId,
        output: typeof output === 'string' ? output : JSON.stringify(output ?? {}),
      }
    };

    this.sendTextMessage(payload, CommandId.Result);
  }

  /**
   * 发送二进制消息
   */
  sendBinaryMessage(message: BinaryMessage): void {
    if (this.websocket && this.websocket.readyState === WebSocket.OPEN) {
      try {
        const bytes = message.toBytes();

        // 性能优化：减少二进制消息的详细日志
        if (message.header.commandId === 6) { // ImageData command - 仅图像数据记录详情
          logger.info(`[BINARY] 发送图像数据 - 会话ID: ${message.header.sessionId}, 大小: ${bytes.length}字节`);
        }

        this.websocket.send(bytes);
        // 移除频繁的成功发送日志
      } catch (error) {
        logger.error(`[BINARY] 发送二进制消息失败: ${error instanceof Error ? error.message : String(error)}`);
      }
    } else {
      logger.error('[BINARY] WebSocket未连接，无法发送二进制消息');
    }
  }

  /**
   * 设置消息事件回调 - 为了保持向后兼容性
   */

  // === 以下是从原WebSocketManager迁移的方法 ===

  private handleOpen(_event: Event): void {
    this.connectionAttempts = 0;
    // 如果没有提供 sessionID，则生成一个新的
    if (!this.sessionId) {
      this.sessionId = this.generateSessionId();
    }
    this.sessionCreated = false;
    this.createSession();
  }

  private handleClose(event: CloseEvent): void {
    this.sessionCreated = false;
    if (!event.wasClean && this.connectionAttempts < CONFIG.MAX_RECONNECT_ATTEMPTS) {
      this.connectionAttempts++;
      this.reconnectTimeout = window.setTimeout(() => {
        // 重新连接逻辑
      }, CONFIG.RECONNECT_INTERVAL);
    }
  }

  private generateSessionId(): string {
    const timestamp = Date.now().toString(36);
    const random = Math.random().toString(36).substr(2);
    const combined = timestamp + random;
    return combined.substr(0, 16).padEnd(16, '0');
  }
  /**
   * 重新创建会话
   */
  recreateSession(sessionId?: string, config?: Partial<SessionConfigPayload>): void {
    if (this.websocket && this.websocket.readyState === WebSocket.OPEN) {
      // 使用提供的 sessionID 或者保持现有的 sessionID
      if (sessionId) {
        this.sessionId = sessionId;
      }
      // 创建会话
      this.createSession(config);
    }
  }

  /**
   * 开始一个全新的会话（生成新的 sessionId）
   * 用于 StopInput 之后重新开始对话，避免复用旧 sessionId 导致服务端不再接受输入
   */
  startNewSession(config?: Partial<SessionConfigPayload>): void {
    if (this.websocket && this.websocket.readyState === WebSocket.OPEN) {
      this.sessionId = this.generateSessionId();
      this.sessionCreated = false;
      this.createSession(config);
    }
  }

  private createSession(config?: Partial<SessionConfigPayload>): void {
    if (this.websocket && this.websocket.readyState === WebSocket.OPEN && this.sessionId) {
      const sessionConfigPayload: SessionConfigPayload = {
        type: "session_config",
        mode: "vad",
        system_prompt: "You are a helpful voice assistant powered by Realtime API. Keep responses concise and conversational. Reply in the same language as the user.",
        enable_search: true,
        signal_only: false,
        asr_language: "zh",
        timezone: "Asia/Shenzhen",
        location: "中国",
        initial_burst_count: 10,
        initial_burst_delay_ms: 5,
        send_rate_multiplier: 1.0,
        output_audio_config: {
          format: "opus",
          slice_ms: 20,
          sample_rate: 16000,
          channels: 1,
          bitrate: 32000,
          application: "voip"
        },
        input_audio_config: {
          format: "opus",
          sample_rate: 16000,
          channels: 1,
          bitrate: 32000,
          application: "voip"
        },
        text_done_signal_only: false,
        ...this.defaultSessionConfig,
        ...config
      };

      // 添加调试日志
      logger.info(`[CLIENT] Creating session with asr_language: ${sessionConfigPayload.asr_language}`);
      logger.info(`[CLIENT] Full session config:`, JSON.stringify(sessionConfigPayload, null, 2));
      logger.info(`[CLIENT] defaultSessionConfig:`, JSON.stringify(this.defaultSessionConfig, null, 2));
      logger.info(`[CLIENT] config parameter:`, JSON.stringify(config, null, 2));

      this.sendTextMessage(sessionConfigPayload, CommandId.Start);
    }
  }


  private cleanup(): void {
    if (this.reconnectTimeout) {
      clearTimeout(this.reconnectTimeout);
      this.reconnectTimeout = null;
    }

    // 清理 websocket 引用，避免内存泄漏
    this.websocket = null;
    this.sessionCreated = false;
  }

  // Getters
  get isConnected(): boolean {
    return this.websocket?.readyState === WebSocket.OPEN;
  }

  get currentSessionId(): string | undefined {
    return this.sessionId;
  }

  get isSessionCreated(): boolean {
    // 以服务端下发 session.created 为准（sessionId 只是客户端本地标识）
    return this.sessionCreated;
  }

  /**
   * 更新默认会话配置
   */
  setDefaultSessionConfig(config: Partial<SessionConfigPayload>): void {
    // 合并更新：避免调用方只传一个字段（如 voice_setting）就把默认的 asr/timezone/location 等覆盖掉
    this.defaultSessionConfig = {
      ...(this.defaultSessionConfig ?? {}),
      ...(config ?? {}),
    };
  }

  // === 以下是从原SDKMessageHandler迁移的方法 ===

  private handleMessage(event: MessageEvent): void {
    try {
      if (typeof event.data === 'string') {
        // 处理JSON消息（包括控制事件）
        const parsedMessage = this.webSocketParser.parse(event.data);
        logger.info(`[SDK] 收到服务器文本消息: ${JSON.stringify(parsedMessage.rawData)}`);

        try {
          const rawData = parsedMessage.rawData;
          let actualMessage = rawData;
          let messageType = rawData.type;
          if (!messageType && rawData.payload && rawData.payload.type) {
            actualMessage = rawData.payload;
            messageType = rawData.payload.type;
            actualMessage.session_id = rawData.session_id;
          }

          // 🔧 修复：检查是否收到正确的音频缓冲区信令事件
          if (messageType && [
            'output_audio_buffer.started',
            'output_audio_buffer.stopped',
            'response.cancel',
            'output_audio_buffer.cleared',
            'conversation.item.truncated'
          ].includes(messageType)) {

            if (['output_audio_buffer.started', 'output_audio_buffer.stopped'].includes(messageType)) {
              console.log(`[SDK] 🟢 收到正确的音频缓冲区信令: ${messageType}`);
              console.log(`[SDK] 🟢 信令详情:`, actualMessage);
              console.log(`[SDK] 🟢 信令时间戳:`, new Date().toISOString());
            } else {
              console.log(`[SDK] 📝 收到其他控制事件: ${messageType}`);
              console.log(`[SDK] 📝 事件详情:`, actualMessage);
            }

            console.log(`[SDK] 📤 即将emit事件: ${messageType}`);
            this.emit(messageType, actualMessage);
            console.log(`[SDK] ✅ 事件已emit，等待App层处理`);
            // 移除return语句，确保正常消息处理流程继续
          }

          // ========== 高精度性能监控 - WebSocket层立即停止计时 ==========
          // 在消息解析完成后立即停止对应的计时器
          // 优先处理 binary_audio_delta 事件，response.audio.delta 作为备用方案
          if (messageType) {
            const latency = this.stopHighPrecisionTimer(messageType);
            if (latency !== null) {
              logger.info(`[PERF] High-precision latency for ${messageType}: ${latency.toFixed(3)}ms`);
            }
          }

          // 针对 response.audio.delta 进行解码（Base64 -> Opus -> PCM16）
          // response.audio.delta 的解码播放交由 App 层处理

          // 直接emit事件，事件类型就是messageType
          if (messageType === 'session.created') {
            this.sessionCreated = true;
          }
          this.emit(messageType, actualMessage);
        } catch (error) {
          logger.error(`[SDK] 处理文本消息时出错: ${error instanceof Error ? error.message : String(error)}`);
        }
      } else {
        // 处理二进制消息（可能是混合协议）
        const dataSize = event.data instanceof ArrayBuffer ? event.data.byteLength :
          event.data instanceof Blob ? event.data.size : 0;

        if (dataSize === 0) {
          logger.warn(`[SDK] 收到空的二进制消息，跳过解析`);
          return;
        }

        // NOTE: noisy per-message log; uncomment only when debugging binary framing/parsing.
        // console.log(`[SDK] 🔍 收到二进制消息，长度: ${dataSize} 字节 - 开始混合协议处理`);

        // 🔧 修复：支持混合协议处理
        // 尝试解析是否为混合协议消息（包含控制事件 + 二进制音频数据）
        this.handleMixedProtocolMessage(event.data).catch(error => {
          logger.error(`[SDK] 混合协议处理失败，回退到纯二进制音频处理:`, error);

          // 回退到原有的二进制音频处理
          this.handleBinaryAudioMessage(event.data).catch(fallbackError => {
            logger.error(`[SDK] 回退处理二进制音频消息时出错:`, fallbackError);
          });
        });
      }
    } catch (_error) {
      logger.error(`[SDK] 处理WebSocket消息时出错: ${_error instanceof Error ? _error.message : String(_error)}`);
      logger.error(`[SDK] 原始消息数据: ${event.data}`);
    }
  }

  // Base64 编解码工具改用 AudioUtils 中的实现

  // ========== 高精度性能监控方法 ==========

  /**
   * 开始计时 - 1ms精度
   * @param eventType 事件类型，支持 'binary_audio_delta'（二进制协议优先）和 'response.audio.delta'（JSON协议备用）
   * 延迟计算方式已优化为优先使用 binary_audio_delta 事件
   */
  public startHighPrecisionTimer(eventType: string): void {
    const now = performance.now();
    this.performanceTimers.set(eventType, now);
    // 性能优化：移除频繁的调试日志

    // 立即发出计时开始事件
    this.emit('performance:timer:start', {
      type: eventType,
      timestamp: now
    });
  }

  /**
   * 停止计时并记录延迟 - 1ms精度
   * @param eventType 事件类型，支持 'binary_audio_delta'（二进制协议优先）和 'response.audio.delta'（JSON协议备用）
   * @returns 延迟时间（毫秒），如果未找到对应的计时器则返回 null
   * 注意：binary_audio_delta 事件提供更精确的延迟计算
   */
  public stopHighPrecisionTimer(eventType: string): number | null {
    const endTime = performance.now();
    const startTime = this.performanceTimers.get(eventType);
    // 性能优化：移除频繁的调试日志

    if (startTime === undefined) {
      return null;
    }

    const latency = endTime - startTime;

    // 记录到历史记录
    const metric = {
      type: eventType,
      startTime,
      endTime,
      latency,
      timestamp: Date.now()
    };

    this.latencyMetrics.push(metric);

    // 限制历史记录大小
    if (this.latencyMetrics.length > this.maxMetricsHistory) {
      this.latencyMetrics.shift();
    }

    // 清除计时器
    this.performanceTimers.delete(eventType);

    // 立即发出延迟测量完成事件
    this.emit('performance:latency:measured', metric);

    return latency;
  }

  /**
   * 更新二进制音频协议配置
   * @param config 新的配置选项
   */
  public updateBinaryAudioConfig(config: Partial<BinaryAudioProtocolConfig>): void {
    this.binaryParser.updateBinaryAudioConfig(config);
    logger.info('[ConnectionManager] 二进制音频配置已更新:', config);
  }

  /**
   * 直接解析二进制音频消息
   * @param buffer 二进制缓冲区
   * @returns 解码结果
   */
  public parseBinaryAudioMessage(buffer: ArrayBuffer): DecodingResult {
    return this.binaryParser.parseBinaryAudioMessage(buffer);
  }

  /**
   * 获取性能指标
   */
  public getPerformanceMetrics(): {
    latest: HighPrecisionMetric | null;
    average: number;
    min: number;
    max: number;
    count: number;
    history: HighPrecisionMetric[];
  } {
    if (this.latencyMetrics.length === 0) {
      return {
        latest: null,
        average: 0,
        min: 0,
        max: 0,
        count: 0,
        history: []
      };
    }

    const latencies = this.latencyMetrics.map(m => m.latency);
    const latest = this.latencyMetrics[this.latencyMetrics.length - 1];
    const average = latencies.reduce((sum, lat) => sum + lat, 0) / latencies.length;
    const min = Math.min(...latencies);
    const max = Math.max(...latencies);

    return {
      latest,
      average,
      min,
      max,
      count: this.latencyMetrics.length,
      history: [...this.latencyMetrics]
    };
  }

  /**
   * 清除性能历史记录
   */
  public clearPerformanceMetrics(): void {
    this.latencyMetrics = [];
    this.performanceTimers.clear();
  }

  /**
   * 处理混合协议消息
   * 支持同时包含控制事件和二进制音频数据的消息
   * @param data 二进制数据
   */
  private async handleMixedProtocolMessage(data: ArrayBuffer | Blob): Promise<void> {
    try {
      let buffer: ArrayBuffer;

      // 处理不同类型的二进制数据
      if (data instanceof ArrayBuffer) {
        buffer = data;
      } else if (data instanceof Blob) {
        buffer = await data.arrayBuffer();
      } else {
        throw new Error(`不支持的数据类型: ${typeof data}`);
      }

      // 检查缓冲区是否为空
      if (buffer.byteLength === 0) {
        logger.warn(`[SDK] 收到空的混合协议数据，跳过处理`);
        return;
      }

      logger.debug(`[SDK] 开始处理混合协议消息，大小: ${buffer.byteLength} 字节`);

      // 尝试解析混合协议消息
      // 策略1：检查是否包含JSON控制事件前缀
      const uint8Array = new Uint8Array(buffer);

      // 检查是否以JSON开始（控制事件可能以JSON格式发送）
      if (uint8Array.length > 0 && uint8Array[0] === 123) { // 123 = '{' 的ASCII码
        logger.debug(`[SDK] 检测到可能的JSON控制事件，尝试解析`);

        try {
          // 尝试解析JSON部分
          const jsonText = new TextDecoder().decode(uint8Array);
          const jsonData = JSON.parse(jsonText);

          logger.info(`[SDK] 成功解析JSON控制事件:`, jsonData);

          // 检查是否是控制事件
          const messageType = jsonData.type || jsonData.payload?.type;
          if (messageType && [
            'output_audio_buffer.started',
            'output_audio_buffer.stopped',
            'response.cancel',
            'output_audio_buffer.cleared',
            'conversation.item.truncated'
          ].includes(messageType)) {

            if (['output_audio_buffer.started', 'output_audio_buffer.stopped'].includes(messageType)) {
              console.log(`[SDK] 🟢 混合协议中发现正确的音频缓冲区信令: ${messageType}`);
              console.log(`[SDK] 🟢 信令详情:`, jsonData);
            } else {
              console.log(`[SDK] 📝 混合协议中发现其他控制事件: ${messageType}`);
              console.log(`[SDK] 📝 控制事件详情:`, jsonData);
            }

            // 处理控制事件
            let actualMessage = jsonData;
            if (!messageType && jsonData.payload && jsonData.payload.type) {
              actualMessage = jsonData.payload;
              actualMessage.session_id = jsonData.session_id;
            }

            // ========== 高精度性能监控 ==========
            const latency = this.stopHighPrecisionTimer(messageType);
            if (latency !== null) {
              logger.info(`[PERF] High-precision latency for ${messageType}: ${latency.toFixed(3)}ms`);
            }

            // 发射控制事件
            this.emit(messageType, actualMessage);
            logger.info(`[SDK] 混合协议控制事件已发射: ${messageType}`);
            return;
          }
        } catch (jsonError) {
          logger.debug(`[SDK] JSON解析失败，继续尝试其他解析方式:`, jsonError);
        }
      }

      // 策略2：检查是否包含嵌入的控制事件标记
      // 某些协议可能在二进制数据中嵌入控制事件标记
      if (uint8Array.length >= 8) {
        // 检查控制事件标记（假设有特殊标记）
        const controlMarker = new DataView(uint8Array.buffer).getUint32(0, true); // 小端序
        if (controlMarker === 0x43414E43) { // "CANC" 标记
          logger.info(`[SDK] 检测到嵌入的控制事件标记`);

          // 提取控制事件数据
          const controlDataLength = new DataView(uint8Array.buffer).getUint32(4, true);
          if (uint8Array.length >= 8 + controlDataLength) {
            const controlData = uint8Array.slice(8, 8 + controlDataLength);
            const controlText = new TextDecoder().decode(controlData);

            try {
              const controlEvent = JSON.parse(controlText);
              const messageType = controlEvent.type;

              if (messageType && [
                'output_audio_buffer.started',
                'output_audio_buffer.stopped',
                'response.cancel',
                'output_audio_buffer.cleared',
                'conversation.item.truncated'
              ].includes(messageType)) {

                if (['output_audio_buffer.started', 'output_audio_buffer.stopped'].includes(messageType)) {
                  console.log(`[SDK] 🟢 嵌入式音频缓冲区信令: ${messageType}`);
                } else {
                  console.log(`[SDK] 📝 嵌入式其他控制事件: ${messageType}`);
                }

                // ========== 高精度性能监控 ==========
                const latency = this.stopHighPrecisionTimer(messageType);
                if (latency !== null) {
                  logger.info(`[PERF] High-precision latency for ${messageType}: ${latency.toFixed(3)}ms`);
                }

                // 发射控制事件
                this.emit(messageType, controlEvent);
                logger.info(`[SDK] 嵌入式控制事件已发射: ${messageType}`);

                // 如果还有音频数据，继续处理
                const audioData = uint8Array.slice(8 + controlDataLength);
                if (audioData.length > 0) {
                  logger.debug(`[SDK] 继续处理嵌入的音频数据，大小: ${audioData.length} 字节`);
                  await this.handleBinaryAudioMessage(audioData.buffer);
                }
                return;
              }
            } catch (parseError) {
              logger.debug(`[SDK] 嵌入式控制事件解析失败:`, parseError);
            }
          }
        }
      }

      // 策略3：如果都不是控制事件，回退到纯二进制音频处理
      logger.debug(`[SDK] 未检测到控制事件，回退到纯二进制音频处理`);
      await this.handleBinaryAudioMessage(buffer);

    } catch (error) {
      logger.error(`[SDK] 混合协议处理失败:`, error);
      throw error;
    }
  }

  /**
   * 处理二进制音频消息
   * @param buffer 二进制缓冲区
   */
  private async handleBinaryAudioMessage(data: ArrayBuffer | Blob): Promise<void> {
    try {
      let buffer: ArrayBuffer;

      // 处理不同类型的二进制数据
      if (data instanceof ArrayBuffer) {
        buffer = data;
        logger.debug(`[SDK] 收到 ArrayBuffer 类型数据，大小: ${buffer.byteLength} 字节`);
      } else if (data instanceof Blob) {
        // 将 Blob 转换为 ArrayBuffer
        buffer = await data.arrayBuffer();
        logger.debug(`[SDK] 收到 Blob 类型数据，转换后大小: ${buffer.byteLength} 字节`);
      } else {
        logger.error(`[SDK] 不支持的数据类型: ${typeof data}`);
        this.emit('binary_audio_error', {
          type: 'binary_audio_error',
          error: `不支持的数据类型: ${typeof data}`,
          bufferSize: undefined
        });
        return;
      }

      // 检查缓冲区是否为空
      if (buffer.byteLength === 0) {
        logger.warn(`[SDK] 收到空的二进制数据，跳过处理`);
        return;
      }

      logger.debug(`[SDK] 开始处理二进制音频消息，大小: ${buffer.byteLength} 字节`);

      // 使用新的二进制解码器
      const decodedAudio: DecodedAudioDeltaMessage = decodeResponseAudioDeltaMessage(buffer);

      logger.debug(`[SDK] 二进制音频解码成功:`, {
        responseId: decodedAudio.responseId,
        itemId: decodedAudio.itemId,
        outputIndex: decodedAudio.outputIndex,
        contentIndex: decodedAudio.contentIndex,
        audioDataSize: decodedAudio.audioData.length
      });

      // ========== 高精度性能监控 - 二进制音频延迟计算 ==========
      // 停止 binary_audio_delta 事件的计时器（优先方案）
      const latency = this.stopHighPrecisionTimer('binary_audio_delta');
      if (latency !== null) {
        logger.info(`[PERF] High-precision latency for binary_audio_delta: ${latency.toFixed(3)}ms`);
      }

      // 性能优化：移除频繁的调试日志
      this.emit('binary_audio_delta', {
        type: 'binary_audio_delta',
        responseId: decodedAudio.responseId,
        itemId: decodedAudio.itemId,
        outputIndex: decodedAudio.outputIndex,
        contentIndex: decodedAudio.contentIndex,
        audioData: decodedAudio.audioData
      });

    } catch (error) {
      logger.error(`[SDK] 二进制音频解码失败:`, error);

      // 发射错误事件
      this.emit('binary_audio_error', {
        type: 'binary_audio_error',
        error: error instanceof Error ? error.message : String(error),
        bufferSize: data instanceof ArrayBuffer ? data.byteLength :
          data instanceof Blob ? data.size : undefined
      });
    }
  }


}

// 性能测试结果接口
export interface PerformanceTestResult {
  summary: {
    totalTests: number;
    totalSuccessful: number;
    overallSuccessRate: number;
    testDuration: number;
  };
  ping: {
    count: number;
    successRate: number;
    averageLatency: number;
    minLatency: number;
    maxLatency: number;
    p50: number;
    p95: number;
    p99: number;
  };
  text: {
    count: number;
    successRate: number;
    averageLatency: number;
    minLatency: number;
    maxLatency: number;
    p50: number;
    p95: number;
    p99: number;
  };
  audio: {
    count: number;
    successRate: number;
    averageLatency: number;
    minLatency: number;
    maxLatency: number;
    p50: number;
    p95: number;
    p99: number;
  };
  rawResults: Array<{
    testId: string;
    testType: 'ping' | 'text' | 'audio';
    startTime: number;
    endTime?: number;
    latency?: number;
    success: boolean;
    error?: string;
  }>;
}

// ========== 高精度性能监控接口 ==========
export interface HighPrecisionMetric {
  type: string;
  startTime: number;
  endTime: number;
  latency: number;
  timestamp: number;
}

// 在 CombinedConnectionManager 类中添加高精度性能监控方法
declare module './CombinedConnectionManager' {
  interface CombinedConnectionManager {
    startHighPrecisionTimer(eventType: string): void;
    stopHighPrecisionTimer(eventType: string): number | null;
    getPerformanceMetrics(): {
      latest: HighPrecisionMetric | null;
      average: number;
      min: number;
      max: number;
      count: number;
      history: HighPrecisionMetric[];
    };
    clearPerformanceMetrics(): void;
  }
}