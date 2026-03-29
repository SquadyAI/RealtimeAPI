/**
 * 全局日志管理器
 * 用于处理整个SDK中的日志记录
 */

import { Logger, type LoggerOptions } from './Logger';

class GlobalLogger extends Logger {
  constructor(options: LoggerOptions = {}) {
    super(options);
  }
}

// 创建全局的日志管理器实例
export const globalLogger = new GlobalLogger({ prefix: 'SDK', level: 'warn' });

// 为了向后兼容，导出之前定义的特定模块的logger
export const AudioManagerLogger = globalLogger;
export const AudioRecorderLogger = globalLogger;
export const AudioPlayerLogger = globalLogger;
export const SDKLogger = globalLogger;

// 导出Logger类，以便其他模块可以使用
export { Logger };

// 默认导出全局logger
export default globalLogger;