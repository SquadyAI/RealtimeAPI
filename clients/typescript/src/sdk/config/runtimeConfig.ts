export const WS_URL_STORAGE_KEY = 'realtime_ts_demo.websocket_url';

export function getStoredWebSocketUrl(): string | null {
  try {
    const value = localStorage.getItem(WS_URL_STORAGE_KEY);
    const trimmed = value?.trim();
    return trimmed ? trimmed : null;
  } catch {
    return null;
  }
}

export function setStoredWebSocketUrl(url: string): void {
  try {
    const trimmed = url.trim();
    if (!trimmed) return;
    localStorage.setItem(WS_URL_STORAGE_KEY, trimmed);
  } catch {
    // ignore (e.g. SSR, privacy mode, storage denied)
  }
}

export function getRuntimeWebSocketUrl(fallbackUrl: string): string {
  return getStoredWebSocketUrl() ?? fallbackUrl;
}

