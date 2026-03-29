import { CombinedConnectionManager } from '../connection/CombinedConnectionManager';
import { globalLogger as logger } from '../utils/GlobalLogger';
import { ClientBinaryMessage as BinaryMessage, ProtocolId } from '../protocol/ClientProtocol';
import type { SessionConfigPayload } from '../protocol/ClientProtocol';
import { EventEmitter } from '../utils/EventEmitter';

export class VoiceChatCore extends EventEmitter {
  private connectionManager: CombinedConnectionManager;
  private isDestroyed: boolean = false;

  constructor(defaultSessionConfig?: Partial<SessionConfigPayload>) {
    super();
    this.connectionManager = new CombinedConnectionManager(undefined, defaultSessionConfig);

    // 绑定消息处理器的回调
    this.setupMessageHandlerCallbacks();
  }

  /**
   * 设置消息处理器回调
   */
  private setupMessageHandlerCallbacks(): void {
    // 使用onAny监听所有从connectionManager发出的事件并进行转发
    this.connectionManager.onAny((event: string, ...args: any[]) => {
      // 直接转发所有事件
      this.emit(event, ...args);
    });
  }

  /**
   * 连接到服务器
   */
  public async connect(): Promise<void> {
    if (this.isDestroyed) {
      throw new Error('VoiceChatCore instance has been destroyed');
    }

    try {
      // 通知连接状态变化
      this.emit('connectionStatusChange', 'connecting', '正在连接...');

      // 连接WebSocket
      await this.connectionManager.connect();

    } catch (error) {
      logger.error(`连接失败: ${error instanceof Error ? error.message : String(error)}`);
      this.emit('connectionStatusChange', 'error', `连接失败: ${error instanceof Error ? error.message : String(error)}`);
      throw error;
    }
  }

  /**
   * 断开连接
   */
  public disconnect(): void {
    if (this.isDestroyed) {
      return;
    }

    this.connectionManager.disconnect();
    this.emit('connectionStatusChange', 'disconnected', '连接已断开');
  }

  /**
   * 销毁实例
   */
  public destroy(): void {
    this.disconnect();
    this.isDestroyed = true;
    this.removeAllListeners();
    this.connectionManager = null as any;
  }

  /**
   * 公共验证方法 - 检查是否可以发送数据
   */
  private validateSendOperation(dataName: string, dataLength: number, minLength: number = 1): { valid: boolean; sessionId: string | null; error?: string } {
    if (dataLength < minLength) {
      return { valid: false, sessionId: null, error: `${dataName}数据太短或为空` };
    }

    if (this.isDestroyed) {
      return { valid: false, sessionId: null, error: '实例已销毁' };
    }

    if (!this.connectionManager.isConnected) {
      return { valid: false, sessionId: null, error: 'WebSocket未连接' };
    }

    if (!this.connectionManager.isSessionCreated) {
      return { valid: false, sessionId: null, error: '会话未创建' };
    }

    const sessionId = this.connectionManager.currentSessionId;
    if (!sessionId) {
      return { valid: false, sessionId: null, error: '会话ID缺失' };
    }

    return { valid: true, sessionId };
  }

  /**
   * 发送 stopInput 消息（VAD语音结束时调用）
   * 用于通知服务端用户语音输入已停止
   * @param eventId 事件ID（可选，自动生成）
   * @param audioEndMs 音频结束时间戳（毫秒，可选，自动计算）
   * @param itemId 消息项ID（可选）
   * @param useBinary 是否使用二进制格式（默认false，使用JSON格式）
   */
  public sendStopInput(eventId?: string, audioEndMs?: number, itemId?: string, useBinary: boolean = false): void {
    if (this.isDestroyed) {
      logger.warn('[VoiceChatCore] 实例已销毁，无法发送stopInput');
      return;
    }

    if (!this.connectionManager.isConnected) {
      logger.warn('[VoiceChatCore] WebSocket未连接，无法发送stopInput');
      return;
    }

    this.connectionManager.sendStopInput(eventId, audioEndMs, itemId, useBinary);
  }

  /**
   * 发送已编码的音频数据
   */
  public sendEncodedAudioData(audioData: Uint8Array): boolean {
    const validation = this.validateSendOperation('音频', audioData.length, 10);

    if (!validation.valid) {
      logger.warn(`尝试发送空的音频数据，已阻止发送: ${validation.error}`);
      return false;
    }

    logger.debug(`准备发送音频数据: ${audioData.length} 字节`);

    try {
      // 创建并发送二进制音频数据消息
      const audioMessage = BinaryMessage.createAudioChunk(validation.sessionId!, audioData);
      this.connectionManager.sendBinaryMessage(audioMessage);

      logger.debug(`发送已编码音频数据: ${audioData.length} 字节`);
      return true;
    } catch (error) {
      logger.error(`发送已编码音频数据失败: ${error instanceof Error ? error.message : String(error)}`);
      return false;
    }
  }

  /**
   * 获取连接状态
   */
  public get isConnected(): boolean {
    return this.connectionManager.isConnected;
  }

  /**
   * 获取会话创建状态
   */
  public get isSessionCreated(): boolean {
    return this.connectionManager.isSessionCreated;
  }

  /**
   * 获取当前会话ID
   */
  public get currentSessionId(): string | null {
    return this.connectionManager.currentSessionId || null;
  }

  /**
   * 设置会话配置
   */
  public setSessionConfig(config: Partial<SessionConfigPayload>): void {
    // 更新 CombinedConnectionManager 的默认配置
    this.connectionManager.setDefaultSessionConfig(config);

    // 如果已经连接，重新创建会话并传递配置
    if (this.isConnected) {
      this.connectionManager.recreateSession(undefined, config);
    }
    // 如果尚未连接，配置将在下次连接时自动使用
  }

  /**
   * 开始新会话
   */
  public startSession(config?: Partial<SessionConfigPayload>): void {
    if (this.isDestroyed) {
      throw new Error('VoiceChatCore instance has been destroyed');
    }

    // 复用当前 sessionId 重新发 session_config（不会强制新 sessionId）
    this.connectionManager.recreateSession(undefined, config);
  }

  /**
   * 开始全新会话（生成新 sessionId）
   * 某些服务端在 StopInput 后不允许继续输入时可用，但默认不要用。
   */
  public startNewSession(config?: Partial<SessionConfigPayload>): void {
    if (this.isDestroyed) {
      throw new Error('VoiceChatCore instance has been destroyed');
    }
    this.connectionManager.startNewSession(config);
  }

  /**
   * 发送图像数据
   * @param imageData 图像二进制数据
   * @param prompt 可选的提示词，如果不提供则使用空字符串
   * @param protocolId 协议ID，默认为All
   */
  public sendImageData(imageData: Uint8Array, prompt?: string, protocolId?: ProtocolId): boolean {
    // 验证图像大小（默认5MB限制）
    const maxImageSize = 5 * 1024 * 1024;
    if (imageData.length > maxImageSize) {
      logger.warn(`图像文件过大: ${imageData.length} 字节，最大限制: ${maxImageSize} 字节`);
      return false;
    }

    const validation = this.validateSendOperation('图像', imageData.length);
    if (!validation.valid) {
      logger.warn(`尝试发送图像数据失败: ${validation.error}`);
      return false;
    }

    logger.debug(`准备发送图像数据: ${imageData.length} 字节`);

    try {
      // 使用提供的协议ID或默认为All(100)
      const finalProtocolId = protocolId || ProtocolId.All;
      // 默认提示词：避免空prompt导致服务端/模型不触发分析
      const promptText = (prompt && prompt.trim().length > 0) ? prompt : "请描述我前面有什么";

      logger.info(`[IMAGE] 准备发送图像数据 - 会话ID: ${validation.sessionId}, 协议ID: ${finalProtocolId}, 数据大小: ${imageData.length} 字节`);

      // 创建并发送二进制图像数据消息
      const imageMessage = BinaryMessage.createImageData(validation.sessionId!, imageData, promptText, finalProtocolId);

      logger.info(`[IMAGE] 二进制消息已创建，头部信息 - 会话ID: ${imageMessage.header.sessionId}, 协议ID: ${imageMessage.header.protocolId}, 命令ID: ${imageMessage.header.commandId}, 总payload大小: ${imageMessage.payload.length} 字节`);

      this.connectionManager.sendBinaryMessage(imageMessage);

      logger.info(`[IMAGE] 图像数据发送完成: 图像${imageData.length}字节 + 提示词${new TextEncoder().encode(promptText).length}字节，协议ID: ${finalProtocolId}`);
      return true;
    } catch (error) {
      logger.error(`[IMAGE] 发送图像数据失败: ${error instanceof Error ? error.message : String(error)}`);
      return false;
    }
  }

  /**
   * 获取连接管理器实例 - 用于高精度性能监控
   */
  public getConnectionManager(): CombinedConnectionManager {
    return this.connectionManager;
  }
}