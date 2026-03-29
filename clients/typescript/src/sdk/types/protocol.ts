/// <reference types="../../@types/env" />

import { getRuntimeWebSocketUrl } from '../config/runtimeConfig';

/**
 * 配置常量
 * 
 * 设计目的：
 * - 从环境变量读取配置，便于不同环境（开发/生产）使用不同配置
 * - 提供默认值作为后备，确保配置始终有效
 * 
 * 原理：
 * - Vite 在构建时会将 import.meta.env.VITE_* 替换为实际值
 * - 使用类型断言确保配置值的类型正确（如将字符串转为布尔值和数字）
 */
/**
 * 根据当前页面地址自动推断 WebSocket URL
 * https → wss, http → ws，连接同一 host 的 /ws 端点
 */
function autoDetectWebSocketUrl(): string {
    if (typeof window !== 'undefined') {
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        return `${protocol}//${window.location.host}/ws`;
    }
    return 'ws://localhost:8080/ws';
}

export const CONFIG = {
    // WebSocket 服务器地址：优先从环境变量读取，否则自动检测当前页面地址
    WEBSOCKET_URL: import.meta.env.VITE_WEBSOCKET_URL || autoDetectWebSocketUrl(),

    // 调试模式开关
    DEBUG_MODE: import.meta.env.VITE_DEBUG_MODE === 'true' || true,

    // 最大重连尝试次数
    MAX_RECONNECT_ATTEMPTS: Number(import.meta.env.VITE_MAX_RECONNECT_ATTEMPTS) || 5,

    // 重连间隔时间（毫秒）
    RECONNECT_INTERVAL: Number(import.meta.env.VITE_RECONNECT_INTERVAL) || 3000,
} as const;

export const DEFAULT_WEBSOCKET_URL = CONFIG.WEBSOCKET_URL;

// 运行时 WS URL（用于 UI 切换 / localStorage 覆盖）
export function getWebSocketUrl(): string {
    return getRuntimeWebSocketUrl(DEFAULT_WEBSOCKET_URL);
}