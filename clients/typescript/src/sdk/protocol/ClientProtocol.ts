/**
 * 二进制消息协议常量
 */
export const BINARY_MESSAGE = {
    NANOID_SIZE: 16,
    PROTOCOL_ID_OFFSET: 16,
    COMMAND_ID_OFFSET: 17,
    BINARY_HEADER_SIZE: 32,

    // 协议ID
    PROTOCOL_ID_ASR: 1,  // ASR协议ID
    PROTOCOL_ID_LLM: 2,  // LLM协议ID
    PROTOCOL_ID_TTS: 3,  // TTS协议ID
    PROTOCOL_ID_CONTROL: 0, // 控制协议ID

    // 命令ID
    CMD_START: 1,        // 开始命令
    CMD_STOP: 2,         // 停止命令
    CMD_AUDIO_CHUNK: 3,  // 音频块命令
    CMD_TEXT_DATA: 4,    // 文本数据命令
    CMD_STOP_INPUT: 5,   // 停止输入命令
    CMD_RESULT: 100,     // 结果命令
    CMD_ERROR: 255       // 错误命令
} as const;

// 音频质量控制参数
export const AUDIO_QUALITY = {
    SAMPLE_RATE: 16000, // 固定采样率
    TARGET_LATENCY_MS: 100, // 低延迟目标
    VOLUME_BOOST: 1.0, // 不额外调整音量，在解码时处理
    FIXED_CHUNK_SIZE: 1280, // 20ms的音频块大小（16000Hz * 2字节/sample * 0.02s = 640字节）
} as const;

/**
 * 协议ID枚举
 */
export const ProtocolId = {
    Asr: 1,
    Llm: 2,
    Tts: 3,
    Control: 4,
    All: 100,
} as const;

export type ProtocolId = typeof ProtocolId[keyof typeof ProtocolId];

/**
 * 命令ID枚举
 */
export const CommandId = {
    Start: 1,
    Stop: 2,
    AudioChunk: 3,
    TextData: 4,
    StopInput: 5,
    ImageData: 6,
    Ping: 7,
    Result: 100,
    Error: 255,
} as const;

export type CommandId = typeof CommandId[keyof typeof CommandId];

/**
 * 消息载荷类型
 */
export type MessagePayload =
    // 客户端发送的消息载荷类型
    | AudioChunkPayload
    | TextDataPayload
    | ImageDataPayload
    | SessionConfigPayload
    | ConversationItemCreatePayload
    // 服务端发送的事件载荷类型
    | SessionCreateEvent
    | ConversationItemCreatedEvent
    | ConversationItemUpdatedEvent
    | InputAudioSpeechStartedEvent
    | InputAudioSpeechStoppedEvent
    | AsrTranscriptionDeltaEvent
    | AsrTranscriptionCompletedEvent
    | ResponseCreatedEvent
    | ResponseTextDeltaEvent
    | ResponseTextDoneEvent
    | ResponseAudioDeltaEvent
    | ResponseAudioDoneEvent
    | ResponseOutputItemAddedEvent
    | ResponseOutputItemDoneEvent
    | ResponseDoneEvent
    | ConversationItemTruncatedEvent
    | OutputAudioBufferStartedEvent
    | OutputAudioBufferStoppedEvent
    | ErrorEvent
    | OutputAudioBufferClearedEvent
    | AsrTranspectionFailedEvent
    | ResponseFunctionCallArgumentsDeltaEvent
    | ResponseFunctionCallArgumentsDoneEvent
    | ResponseFunctionCallResultDoneEvent
    | SessionUpdateEvent
    | ResponseCancelEvent
    | ResponseFunctionCallDeltaEvent
    | ResponseFunctionCallDoneEvent
    | ResponseFunctionCallResultDeltaEvent
    | UnknownPayload;

// 客户端发送的消息载荷类型定义
export interface AudioChunkPayload {
    type: "audio_chunk";
    data: string; // Base64编码的音频数据
    sample_rate: number;
    channels: number;
    // 二进制音频协议的额外元数据（可选）
    responseId?: string;
    itemId?: string;
    outputIndex?: number;
    contentIndex?: number;
}

export interface TextDataPayload {
    type: "text_data";
    text: string;
}

export interface ImageDataPayload {
    type: "image_data";
    data: string; // Base64编码的图像数据
    size: number; // 图像数据大小
}

export interface AudioConfig {
    format: "opus" | "pcm";
    slice_ms?: number;
    sample_rate?: number;
    channels?: number;
    bitrate?: number;
    application?: "voip" | "audio" | "restricted_lowdelay";
}

export interface SessionConfigPayload {
    type: "session_config";
    mode?: string;
    vad_threshold?: number;
    silence_duration_ms?: number;
    min_speech_duration_ms?: number;
    system_prompt?: string;
    mcp_server_config?: Record<string, unknown> | Record<string, unknown>[];
    tools_endpoint?: string;
    tools?: Record<string, unknown>[];
    tool_choice?: string | Record<string, unknown>;
    enable_search?: boolean;
    search_config?: Record<string, unknown>;
    voice_setting?: Record<string, unknown>;
    asr_language?: string;
    timezone?: string;
    location?: string;
    initial_burst_count?: number;
    initial_burst_delay_ms?: number;
    send_rate_multiplier?: number;
    output_audio_config?: AudioConfig;
    input_audio_config?: AudioConfig;
    text_done_signal_only?: boolean;
    signal_only?: boolean;
}

export interface ConversationItemCreatePayload {
    type: "conversation.item.create";
    previous_item_id?: string;
    item: Record<string, unknown>;
}

// 服务端发送的事件载荷类型定义
export interface SessionCreateEvent {
    type: "session.created";
    event_id: string;
    session: Record<string, unknown>;
}

export interface ConversationItemCreatedEvent {
    type: "conversation.item.created";
    event_id: string;
    previous_item_id?: string;
    item: Record<string, unknown>;
}

export interface ConversationItemUpdatedEvent {
    type: "conversation.item.updated";
    event_id: string;
    item: Record<string, unknown>;
}

export interface InputAudioSpeechStartedEvent {
    type: "input_audio_buffer.speech_started";
    event_id: string;
    audio_start_ms: number;
    item_id: string;
}

export interface InputAudioSpeechStoppedEvent {
    type: "input_audio_buffer.speech_stopped";
    event_id: string;
    audio_end_ms: number;
    item_id: string;
}

export interface AsrTranscriptionDeltaEvent {
    type: "conversation.item.input_audio_transcription.delta";
    event_id: string;
    item_id: string;
    content_index: number;
    delta: string;
}

export interface AsrTranscriptionCompletedEvent {
    type: "conversation.item.input_audio_transcription.completed";
    event_id: string;
    item_id: string;
    content_index: number;
    transcript: string;
}

export interface ResponseCreatedEvent {
    type: "response.created";
    event_id: string;
    response: Record<string, unknown>;
}

export interface ResponseTextDeltaEvent {
    type: "response.text.delta";
    event_id: string;
    response_id: string;
    item_id: string;
    output_index: number;
    content_index: number;
    delta: string;
}

export interface ResponseTextDoneEvent {
    type: "response.text.done";
    event_id: string;
    response_id: string;
    item_id: string;
    output_index: number;
    content_index: number;
    text: string;
}

export interface ResponseAudioDeltaEvent {
    type: "response.audio.delta";
    event_id: string;
    response_id: string;
    item_id: string;
    output_index: number;
    content_index: number;
    delta: string;
    // 注意：这是传统的JSON音频协议，作为 binary_audio_delta 的备用方案
}

export interface ResponseAudioDoneEvent {
    type: "response.audio.done";
    event_id: string;
    response_id: string;
    item_id: string;
    output_index: number;
    content_index: number;
}

export interface ResponseOutputItemAddedEvent {
    type: "response.output_item.added";
    event_id: string;
    response_id: string;
    output_index: number;
    item: Record<string, unknown>;
}

export interface ResponseOutputItemDoneEvent {
    type: "response.output_item.done";
    event_id: string;
    response_id: string;
    output_index: number;
    item: Record<string, unknown>;
}

export interface ResponseDoneEvent {
    type: "response.done";
    event_id: string;
    response: Record<string, unknown>;
}

export interface ConversationItemTruncatedEvent {
    type: "conversation.item.truncated";
    event_id: string;
    item_id: string;
    content_index: number;
    audio_end_ms: number;
}

export interface OutputAudioBufferStartedEvent {
    type: "output_audio_buffer.started";
    event_id: string;
    response_id: string;
}

export interface OutputAudioBufferStoppedEvent {
    type: "output_audio_buffer.stopped";
    event_id: string;
    response_id: string;
}

export interface ErrorEvent {
    type: "error.event";
    event_id: string;
    code: number;
    message: string;
}

export interface OutputAudioBufferClearedEvent {
    type: "output_audio_buffer.cleared";
    event_id: string;
    response_id: string;
}

export interface AsrTranspectionFailedEvent {
    type: "conversation.item.input_audio_transcription.failed";
    event_id: string;
    item_id: string;
    content_index: number;
    error: Record<string, unknown>;
}

export interface ResponseFunctionCallArgumentsDeltaEvent {
    type: "response.function_call_arguments.delta";
    event_id: string;
    response_id: string;
    item_id: string;
    call_id: string;
    delta: string;
}

export interface ResponseFunctionCallArgumentsDoneEvent {
    type: "response.function_call_arguments.done";
    event_id: string;
    response_id: string;
    item_id: string;
    call_id: string;
    function_name: string;
    arguments: string;
}

export interface ResponseFunctionCallResultDoneEvent {
    type: "response.function_call_result.done";
    event_id: string;
    response_id: string;
    item_id: string;
    call_id: string;
    result: string;
}

export interface SessionUpdateEvent {
    type: "session.update";
    event_id: string;
    session: Record<string, unknown>;
}

export interface ResponseCancelEvent {
    type: "response.cancel";
    event_id: string;
    response_id: string;
}

export interface ResponseFunctionCallDeltaEvent {
    type: "response.function_call.delta";
    event_id: string;
    response_id: string;
    item_id: string;
    call_id: string;
    delta: string;
}

export interface ResponseFunctionCallDoneEvent {
    type: "response.function_call.done";
    event_id: string;
    response_id: string;
    item_id: string;
    call_id: string;
    arguments: string;
}

export interface ResponseFunctionCallResultDeltaEvent {
    type: "response.function_call_result.delta";
    event_id: string;
    response_id: string;
    item_id: string;
    call_id: string;
    delta: string;
}

export interface UnknownPayload {
    type: string;
    [key: string]: unknown;
}

/**
 * WebSocket消息结构
 */
export interface WebSocketMessage {
    protocol_id: ProtocolId;
    command_id: CommandId;
    session_id: string;
    payload?: MessagePayload;
}

/**
 * 二进制协议头部
 */
export class BinaryHeader {
    public sessionId: string;
    public protocolId: ProtocolId;
    public commandId: CommandId;
    public reserved: Uint8Array;

    constructor(sessionId: string, protocolId: ProtocolId, commandId: CommandId) {
        if (!sessionId || sessionId.length !== BINARY_MESSAGE.NANOID_SIZE) {
            throw new Error(`会话ID必须为 ${BINARY_MESSAGE.NANOID_SIZE} 字符`);
        }

        this.sessionId = sessionId;
        this.protocolId = protocolId;
        this.commandId = commandId;
        this.reserved = new Uint8Array(14); // 14字节保留字段
    }

    static fromBytes(bytes: Uint8Array): BinaryHeader {
        if (bytes.length < BINARY_MESSAGE.BINARY_HEADER_SIZE) {
            throw new Error(`字节数组长度不足: 期望 ${BINARY_MESSAGE.BINARY_HEADER_SIZE}, 实际 ${bytes.length}`);
        }

        // 提取nanoid (16字节)
        const sessionIdBytes = bytes.subarray(0, BINARY_MESSAGE.NANOID_SIZE);
        const sessionId = new TextDecoder().decode(sessionIdBytes);

        // 提取业务ID (1字节)
        const protocolIdRaw = bytes[BINARY_MESSAGE.PROTOCOL_ID_OFFSET];
        let protocolId: ProtocolId;
        switch (protocolIdRaw) {
            case 1:
                protocolId = ProtocolId.Asr;
                break;
            case 2:
                protocolId = ProtocolId.Llm;
                break;
            case 3:
                protocolId = ProtocolId.Tts;
                break;
            case 100:
                protocolId = ProtocolId.All;
                break;
            default:
                console.warn(`未知协议ID: ${protocolIdRaw}, 默认为All`);
                protocolId = ProtocolId.All;
                break;
        }

        // 提取命令ID (1字节)
        const commandIdRaw = bytes[BINARY_MESSAGE.COMMAND_ID_OFFSET];
        let commandId: CommandId;
        switch (commandIdRaw) {
            case 1:
                commandId = CommandId.Start;
                break;
            case 2:
                commandId = CommandId.Stop;
                break;
            case 3:
                commandId = CommandId.AudioChunk;
                break;
            case 4:
                commandId = CommandId.TextData;
                break;
            case 5:
                commandId = CommandId.StopInput;
                break;
            case 6:
                commandId = CommandId.ImageData;
                break;
            case 7:
                commandId = CommandId.Ping;
                break;
            case 100:
                commandId = CommandId.Result;
                break;
            case 255:
                commandId = CommandId.Error;
                break;
            default:
                console.warn(`未知命令ID: ${commandIdRaw}, 默认为Error`);
                commandId = CommandId.Error;
                break;
        }

        // 解析 reserved 字段 (bytes[18..32])
        const reserved = bytes.subarray(BINARY_MESSAGE.COMMAND_ID_OFFSET + 1, BINARY_MESSAGE.BINARY_HEADER_SIZE);

        const header = new BinaryHeader(sessionId, protocolId, commandId);
        header.reserved = reserved;
        return header;
    }

    toBytes(): Uint8Array {
        const headerBytes = new Uint8Array(BINARY_MESSAGE.BINARY_HEADER_SIZE);

        // 写入会话ID (前16字节)
        const sessionIdBytes = new TextEncoder().encode(this.sessionId);
        headerBytes.set(sessionIdBytes, 0);

        // 写入协议ID (第17字节)
        headerBytes[BINARY_MESSAGE.PROTOCOL_ID_OFFSET] = this.protocolId;

        // 写入命令ID (第18字节)
        headerBytes[BINARY_MESSAGE.COMMAND_ID_OFFSET] = this.commandId;

        // 写入 reserved
        headerBytes.set(this.reserved, BINARY_MESSAGE.COMMAND_ID_OFFSET + 1);

        return headerBytes;
    }
}

/**
 * 二进制消息格式
 */
export class ClientBinaryMessage {
    public header: BinaryHeader;
    public payload: Uint8Array;

    constructor(header: BinaryHeader, payload?: Uint8Array) {
        this.header = header;
        this.payload = payload || new Uint8Array(0);
    }

    static fromBytes(bytes: Uint8Array): ClientBinaryMessage {
        if (bytes.length < BINARY_MESSAGE.BINARY_HEADER_SIZE) {
            throw new Error(`字节数组长度不足: 期望 ${BINARY_MESSAGE.BINARY_HEADER_SIZE}, 实际 ${bytes.length}`);
        }

        const header = BinaryHeader.fromBytes(bytes.subarray(0, BINARY_MESSAGE.BINARY_HEADER_SIZE));
        const payload = bytes.subarray(BINARY_MESSAGE.BINARY_HEADER_SIZE);

        return new ClientBinaryMessage(header, payload);
    }

    toBytes(): Uint8Array {
        const headerBytes = this.header.toBytes();
        const result = new Uint8Array(headerBytes.length + this.payload.length);

        result.set(headerBytes, 0);
        result.set(this.payload, headerBytes.length);
        return result;
    }

    /**
     * 创建音频数据二进制消息
     */
    static createAudioChunk(sessionId: string, audioData: Uint8Array): ClientBinaryMessage {
        const header = new BinaryHeader(sessionId, ProtocolId.Asr, CommandId.AudioChunk);
        return new ClientBinaryMessage(header, audioData);
    }

    /**
     * 创建会话开始二进制消息
     */
    static createStartSession(sessionId: string, protocolId: ProtocolId): ClientBinaryMessage {
        const header = new BinaryHeader(sessionId, protocolId, CommandId.Start);
        return new ClientBinaryMessage(header, new Uint8Array(0));
    }

    /**
     * 创建会话停止二进制消息
     */
    static createStopSession(sessionId: string, protocolId: ProtocolId): ClientBinaryMessage {
        const header = new BinaryHeader(sessionId, protocolId, CommandId.Stop);
        return new ClientBinaryMessage(header, new Uint8Array(0));
    }

    /**
     * 创建停止输入二进制消息
     */
    static createStopInput(sessionId: string): ClientBinaryMessage {
        const header = new BinaryHeader(sessionId, ProtocolId.All, CommandId.StopInput);
        return new ClientBinaryMessage(header, new Uint8Array(0));
    }

    /**
     * 创建文本数据二进制消息
     */
    static createTextData(sessionId: string, text: string): ClientBinaryMessage {
        const header = new BinaryHeader(sessionId, ProtocolId.Tts, CommandId.TextData);
        const textBytes = new TextEncoder().encode(text);
        return new ClientBinaryMessage(header, textBytes);
    }

    /**
     * 创建Ping二进制消息
     */
    static createPing(sessionId: string): ClientBinaryMessage {
        const header = new BinaryHeader(sessionId, ProtocolId.Control, CommandId.Ping);
        return new ClientBinaryMessage(header, new Uint8Array(0));
    }

    /**
     * 创建图像数据二进制消息
     * payload格式: [prompt_length(4bytes)] + [prompt_utf8_bytes] + [image_data]
     */
    static createImageData(sessionId: string, imageBytes: Uint8Array, prompt?: string, protocolId: ProtocolId = ProtocolId.All): ClientBinaryMessage {
        const header = new BinaryHeader(sessionId, protocolId, CommandId.ImageData);

        // 构建新的payload格式
        const promptText = prompt || ""; // 如果没有提示词，使用空字符串
        const promptBytes = new TextEncoder().encode(promptText);
        const promptLength = promptBytes.length;

        // 创建payload: [prompt_length(4bytes)] + [prompt_utf8_bytes] + [image_data]
        const payload = new Uint8Array(4 + promptLength + imageBytes.length);

        // 写入prompt长度 (4字节，小端序)
        const lengthView = new DataView(payload.buffer, 0, 4);
        lengthView.setUint32(0, promptLength, true); // true表示小端序

        // 写入prompt UTF-8字节
        payload.set(promptBytes, 4);

        // 写入图像数据
        payload.set(imageBytes, 4 + promptLength);

        return new ClientBinaryMessage(header, payload);
    }
}

/**
 * 创建新的WebSocket消息
 */
export function createWebSocketMessage(
    protocol_id: ProtocolId,
    command_id: CommandId,
    session_id: string,
    payload?: MessagePayload
): WebSocketMessage {
    return {
        protocol_id,
        command_id,
        session_id,
        payload
    };
}

/**
 * 序列化WebSocket消息为JSON字符串
 */
export function serializeWebSocketMessage(message: WebSocketMessage): string {
    return JSON.stringify(message);
}

/**
 * 从JSON字符串解析WebSocket消息
 */
export function parseWebSocketMessage(json: string): WebSocketMessage {
    return JSON.parse(json);
}