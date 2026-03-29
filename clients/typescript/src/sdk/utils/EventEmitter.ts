/* eslint-disable @typescript-eslint/no-unsafe-function-type */
// 简单的EventEmitter实现，与项目中其他地方保持一致
export class EventEmitter {
    private events: { [key: string]: Function[] } = {};
    private anyListeners: Function[] = [];

    on(event: string, listener: Function): this {
        if (!this.events[event]) {
            this.events[event] = [];
        }
        this.events[event].push(listener);
        return this;
    }

    off(event: string, listener?: Function): this {
        if (!this.events[event]) return this;
        // 如果未提供特定监听器，则移除此事件的所有监听器
        if (listener === undefined) {
            delete this.events[event];
            return this;
        }
        // 否则仅移除匹配的监听器
        this.events[event] = this.events[event].filter(l => l !== listener);
        return this;
    }

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    emit(event: string, ...args: any[]): boolean {
        // 首先触发特定事件的监听器
        if (this.events[event]) {
            this.events[event].forEach(listener => listener(...args));
        }

        // 然后触发所有事件的监听器（onAny）
        this.anyListeners.forEach(listener => listener(event, ...args));

        return true;
    }

    // 监听所有事件
    onAny(listener: Function): this {
        this.anyListeners.push(listener);
        return this;
    }

    // 移除特定的onAny监听器
    offAny(listener: Function): this {
        this.anyListeners = this.anyListeners.filter(l => l !== listener);
        return this;
    }

    removeAllListeners(event?: string): this {
        if (event) {
            delete this.events[event];
        } else {
            this.events = {};
        }
        // 如果没有指定特定事件，则清除所有onAny监听器
        if (!event) {
            this.anyListeners = [];
        }
        return this;
    }
}