import { useCallback, useRef, useState } from 'react';

interface CameraHandler {
    isCameraOpen: boolean;
    isCapturing: boolean;
    error: string | null;
    openCamera: () => Promise<void>;
    closeCamera: () => void;
    capturePhoto: () => Promise<Blob | null>;
    switchCamera: () => void;
}

export const useCameraHandler = (): CameraHandler => {
    const [isCameraOpen, setIsCameraOpen] = useState(false);
    const [isCapturing, setIsCapturing] = useState(false);
    const [error, setError] = useState<string | null>(null);

    const videoRef = useRef<HTMLVideoElement | null>(null);
    const streamRef = useRef<MediaStream | null>(null);
    const facingModeRef = useRef<'user' | 'environment'>('environment');

    const openCamera = useCallback(async () => {
        setError(null);

        try {
            if (!navigator.mediaDevices?.getUserMedia) {
                throw new Error('当前浏览器不支持摄像头');
            }
            // iOS/Safari & 部分安卓浏览器要求 HTTPS（或 localhost）才能调用 getUserMedia
            if (!window.isSecureContext) {
                throw new Error('需要在 HTTPS 环境下才能打开摄像头');
            }

            // 停止之前的流
            if (streamRef.current) {
                streamRef.current.getTracks().forEach(track => track.stop());
            }

            // 请求摄像头权限（移动端对 facingMode 兼容性差，做多档 fallback）
            const tryGetUserMedia = async (constraints: MediaStreamConstraints) => {
                return await navigator.mediaDevices.getUserMedia(constraints);
            };

            let stream: MediaStream | null = null;
            const baseVideo = { width: { ideal: 1920 }, height: { ideal: 1080 } };
            const attempts: MediaStreamConstraints[] = [
                // 部分浏览器支持 exact
                { video: { ...baseVideo, facingMode: { exact: facingModeRef.current } }, audio: false } as MediaStreamConstraints,
                // 通用 ideal
                { video: { ...baseVideo, facingMode: { ideal: facingModeRef.current } }, audio: false } as MediaStreamConstraints,
                // 最后兜底：让浏览器自己挑
                { video: true, audio: false }
            ];

            for (const c of attempts) {
                try {
                    stream = await tryGetUserMedia(c);
                    break;
                } catch (_e) {
                    // try next
                }
            }
            if (!stream) {
                throw new Error('无法打开摄像头（权限/设备/浏览器限制）');
            }

            streamRef.current = stream;

            // 创建 video 元素
            const video = document.createElement('video');
            video.srcObject = stream;
            video.autoplay = true;
            video.playsInline = true;
            // iOS Safari 兼容：需要 attribute
            video.setAttribute('playsinline', 'true');
            video.setAttribute('webkit-playsinline', 'true');
            video.muted = true;

            videoRef.current = video;

            // 等待视频加载
            await new Promise<void>((resolve) => {
                video.onloadedmetadata = () => {
                    // 部分移动端会拒绝非手势 play，先尝试，不阻塞打开流程
                    void video.play().catch(() => {
                        // ignore - 下面会提供“点击开始预览”的兜底
                    });
                    resolve();
                };
            });

            // 将 video 添加到 body
            video.style.position = 'fixed';
            video.style.top = '50%';
            video.style.left = '50%';
            video.style.transform = 'translate(-50%, -50%)';
            video.style.width = '100%';
            video.style.maxWidth = '500px';
            video.style.borderRadius = '12px';
            video.style.zIndex = '9999';
            video.style.boxShadow = '0 4px 20px rgba(0,0,0,0.3)';

            // 添加拍照按钮
            const captureBtn = document.createElement('button');
            captureBtn.innerHTML = '📸 拍照';
            captureBtn.style.position = 'fixed';
            captureBtn.style.bottom = '100px';
            captureBtn.style.left = '50%';
            captureBtn.style.transform = 'translateX(-50%)';
            captureBtn.style.padding = '16px 48px';
            captureBtn.style.fontSize = '18px';
            captureBtn.style.borderRadius = '30px';
            captureBtn.style.border = 'none';
            captureBtn.style.background = 'linear-gradient(135deg, #667eea 0%, #764ba2 100%)';
            captureBtn.style.color = 'white';
            captureBtn.style.cursor = 'pointer';
            captureBtn.style.zIndex = '10000';
            captureBtn.style.boxShadow = '0 4px 15px rgba(0,0,0,0.3)';

            // 添加关闭按钮
            const closeBtn = document.createElement('button');
            closeBtn.innerHTML = '✕';
            closeBtn.style.position = 'fixed';
            closeBtn.style.top = '20px';
            closeBtn.style.right = '20px';
            closeBtn.style.width = '40px';
            closeBtn.style.height = '40px';
            closeBtn.style.borderRadius = '50%';
            closeBtn.style.border = 'none';
            closeBtn.style.background = 'rgba(0,0,0,0.5)';
            closeBtn.style.color = 'white';
            closeBtn.style.fontSize = '20px';
            closeBtn.style.cursor = 'pointer';
            closeBtn.style.zIndex = '10000';

            // 容器
            const container = document.createElement('div');
            container.id = 'camera-container';
            container.style.position = 'fixed';
            container.style.top = '0';
            container.style.left = '0';
            container.style.width = '100%';
            container.style.height = '100%';
            container.style.background = 'rgba(0,0,0,0.8)';
            container.style.zIndex = '9998';

            container.appendChild(video);
            container.appendChild(captureBtn);
            container.appendChild(closeBtn);
            document.body.appendChild(container);

            // 如果自动播放失败，让用户点一下开始预览（满足用户手势要求）
            const ensurePreview = async () => {
                try {
                    await video.play();
                } catch (_e) {
                    const existing = document.getElementById('camera-preview-start');
                    if (existing) return;
                    const startBtn = document.createElement('button');
                    startBtn.id = 'camera-preview-start';
                    startBtn.innerHTML = '▶ 点击开始预览';
                    startBtn.style.position = 'fixed';
                    startBtn.style.bottom = '170px';
                    startBtn.style.left = '50%';
                    startBtn.style.transform = 'translateX(-50%)';
                    startBtn.style.padding = '12px 24px';
                    startBtn.style.fontSize = '16px';
                    startBtn.style.borderRadius = '24px';
                    startBtn.style.border = 'none';
                    startBtn.style.background = 'rgba(0,0,0,0.65)';
                    startBtn.style.color = 'white';
                    startBtn.style.cursor = 'pointer';
                    startBtn.style.zIndex = '10001';
                    startBtn.onclick = async () => {
                        try {
                            await video.play();
                            startBtn.remove();
                        } catch (e2) {
                            console.error('[CAMERA] 预览启动失败:', e2);
                        }
                    };
                    container.appendChild(startBtn);
                }
            };
            void ensurePreview();

            // 拍照事件
            captureBtn.onclick = async () => {
                setIsCapturing(true);
                try {
                    const canvas = document.createElement('canvas');
                    canvas.width = video.videoWidth;
                    canvas.height = video.videoHeight;
                    const ctx = canvas.getContext('2d');
                    if (ctx) {
                        ctx.drawImage(video, 0, 0);
                        canvas.toBlob(async (blob) => {
                            if (blob) {
                                console.log('[CAMERA] 拍照成功，文件大小:', blob.size, '字节');
                                // 关闭摄像头
                                closeCamera();
                                // 触发自定义事件通知拍照完成
                                window.dispatchEvent(new CustomEvent('photoCaptured', { detail: blob }));
                            }
                            setIsCapturing(false);
                        }, 'image/jpeg', 0.9);
                    }
                } catch (err) {
                    console.error('[CAMERA] 拍照失败:', err);
                    setIsCapturing(false);
                }
            };

            // 关闭事件
            closeBtn.onclick = closeCamera;
            container.onclick = (e) => {
                if (e.target === container) {
                    closeCamera();
                }
            };

            setIsCameraOpen(true);
            console.log('[CAMERA] 摄像头已打开');

        } catch (err) {
            console.error('[CAMERA] 打开摄像头失败:', err);
            const msg = err instanceof Error ? err.message : '无法打开摄像头';
            setError(msg);
            setIsCameraOpen(false);

            // 移动端常见：必须由用户点击触发（非手势调用会被拦）。提供“一键重试”覆盖按钮。
            const domErr = err as any;
            const name = typeof domErr?.name === 'string' ? domErr.name : '';
            const shouldOfferRetry =
                name === 'NotAllowedError' ||
                name === 'SecurityError' ||
                msg.includes('HTTPS') ||
                msg.includes('权限') ||
                msg.includes('gesture');

            if (shouldOfferRetry) {
                const existing = document.getElementById('camera-retry-overlay');
                if (existing) return;

                const overlay = document.createElement('div');
                overlay.id = 'camera-retry-overlay';
                overlay.style.position = 'fixed';
                overlay.style.top = '0';
                overlay.style.left = '0';
                overlay.style.width = '100%';
                overlay.style.height = '100%';
                overlay.style.background = 'rgba(0,0,0,0.7)';
                overlay.style.zIndex = '10002';
                overlay.style.display = 'flex';
                overlay.style.alignItems = 'center';
                overlay.style.justifyContent = 'center';

                const btn = document.createElement('button');
                btn.innerHTML = '点一下授权/打开摄像头';
                btn.style.padding = '14px 22px';
                btn.style.fontSize = '16px';
                btn.style.borderRadius = '28px';
                btn.style.border = 'none';
                btn.style.background = 'white';
                btn.style.color = '#111';
                btn.style.cursor = 'pointer';

                btn.onclick = async () => {
                    overlay.remove();
                    await openCamera(); // 这次由用户手势触发
                };

                overlay.onclick = (e) => {
                    if (e.target === overlay) overlay.remove();
                };

                overlay.appendChild(btn);
                document.body.appendChild(overlay);
            }
        }
    }, []);

    const closeCamera = useCallback(() => {
        // 停止流
        if (streamRef.current) {
            streamRef.current.getTracks().forEach(track => track.stop());
            streamRef.current = null;
        }

        // 移除容器
        const container = document.getElementById('camera-container');
        if (container) {
            container.remove();
        }

        videoRef.current = null;
        setIsCameraOpen(false);
        console.log('[CAMERA] 摄像头已关闭');
    }, []);

    const capturePhoto = useCallback(async (): Promise<Blob | null> => {
        if (!videoRef.current) {
            console.error('[CAMERA] 摄像头未打开');
            return null;
        }

        setIsCapturing(true);

        try {
            const canvas = document.createElement('canvas');
            canvas.width = videoRef.current.videoWidth;
            canvas.height = videoRef.current.videoHeight;
            const ctx = canvas.getContext('2d');

            if (!ctx) {
                throw new Error('无法获取 canvas 上下文');
            }

            ctx.drawImage(videoRef.current, 0, 0);

            return new Promise((resolve) => {
                canvas.toBlob((blob) => {
                    if (blob) {
                        console.log('[CAMERA] 拍照成功，文件大小:', blob.size, '字节');
                    }
                    setIsCapturing(false);
                    resolve(blob);
                }, 'image/jpeg', 0.9);
            });
        } catch (err) {
            console.error('[CAMERA] 拍照失败:', err);
            setIsCapturing(false);
            return null;
        }
    }, []);

    const switchCamera = useCallback(() => {
        facingModeRef.current = facingModeRef.current === 'user' ? 'environment' : 'user';
        if (isCameraOpen) {
            openCamera();
        }
    }, [isCameraOpen, openCamera]);

    return {
        isCameraOpen,
        isCapturing,
        error,
        openCamera,
        closeCamera,
        capturePhoto,
        switchCamera
    };
};