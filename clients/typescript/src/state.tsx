import { atom } from "jotai";

export interface LogEntry {
  timestamp: number;
  level: 'debug' | 'info' | 'warn' | 'error';
  message: string;
  data?: unknown;
}

const MAX_LOGS = 1000; // 限制日志数量防止内存溢出

export const isUserSpeakingAtom = atom(false)
export const wsConnectedAtom = atom(false)
export const sessionCreatedAtom = atom(false)
export const logsAtom = atom<LogEntry[]>([])
