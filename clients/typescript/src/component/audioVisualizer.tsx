import React, { useState, useEffect } from 'react';
import { LiveAudioVisualizer } from 'react-audio-visualize';
import styles from './index.module.scss';

interface LiveAudioVisualizerComponentProps {
  width?: number;
  height?: number;
  barWidth?: number;
  gap?: number;
  barColor?: string;
  smoothingTimeConstant?: number;
  fftSize?: number;
  isRecording?: boolean;
  pause?: boolean;
}

export const LiveAudioVisualizerComponent: React.FC<LiveAudioVisualizerComponentProps> = ({
  width = 500,
  height = 200,
  barWidth = 2,
  gap = 1,
  barColor = '#ffffff',
  smoothingTimeConstant = 0.8,
  fftSize = 512,
  pause = true,
}) => {
  const [mediaRecorder, setMediaRecorder] = useState<MediaRecorder | null>(null);

  // 监听录音状态变化
  useEffect(() => {
    const startRecording = async () => {
      try {
        // 首次进入页面会触发权限弹窗：一次性请求麦克风+摄像头权限（Chrome 会合并弹窗）
        // 但这里的可视化只需要音频，所以拿到后立刻停掉视频轨，仅保留音频轨给 MediaRecorder。
        const stream = await navigator.mediaDevices.getUserMedia({
          audio: {
            echoCancellation: true,
            noiseSuppression: true,
            autoGainControl: true,
          }
          ,
          video: true
        });

        // 只保留音频轨
        const audioOnlyStream = new MediaStream(stream.getAudioTracks());
        // 摄像头权限已经获取到，立即停止视频轨，避免占用摄像头
        stream.getVideoTracks().forEach(track => track.stop());

        const recorder = new MediaRecorder(audioOnlyStream);
        setMediaRecorder(recorder);
        recorder.start();
      } catch {
        // 静默失败，不显示提示
      }
    };
    startRecording();
  }, []);

  // 组件卸载时清理资源
  useEffect(() => {
    return () => {
      if (mediaRecorder) {
        mediaRecorder.stop();
        mediaRecorder.stream.getTracks().forEach(track => track.stop());
      }
    };
  }, [mediaRecorder]);


  useEffect(() => {
    if (pause) {
      mediaRecorder?.pause();
    } else {
      mediaRecorder?.resume();
    }
  }, [mediaRecorder, pause]);

  return (
    <div className={styles.audioVisualizerOverlay}>
      {mediaRecorder && (
        <LiveAudioVisualizer
          mediaRecorder={mediaRecorder}
          width={width}
          height={height}
          barWidth={barWidth}
          gap={gap}
          barColor={barColor}
          smoothingTimeConstant={smoothingTimeConstant}
          fftSize={fftSize as 512}
        />
      )}
    </div>
  );
};

// 导出默认组件
export default LiveAudioVisualizerComponent;

