/// <reference types="vite/client" />

/**
 * 扩展 Vite 的环境变量类型定义
 * 
 * 设计目的：
 * - 为项目中使用的环境变量提供 TypeScript 类型支持
 * - 确保在代码中访问 import.meta.env 时有正确的类型提示和检查
 */
interface ImportMetaEnv {
    /** WebSocket 服务器 URL */
    readonly VITE_WEBSOCKET_URL: string

    /** 调试模式开关 */
    readonly VITE_DEBUG_MODE: string

    /** 最大重连尝试次数 */
    readonly VITE_MAX_RECONNECT_ATTEMPTS: string

    /** 重连间隔时间（毫秒） */
    readonly VITE_RECONNECT_INTERVAL: string
}

interface ImportMeta {
    readonly env: ImportMetaEnv
}
