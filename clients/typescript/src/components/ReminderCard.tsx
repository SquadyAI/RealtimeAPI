import React, { useEffect, useRef, useState } from 'react';
import styles from './ReminderCard.module.scss';
import type { Reminder } from '../types/reminder';

interface ReminderCardProps {
    reminder: Reminder;
    onComplete: (id: string) => void;
    onDelete: (id: string) => void;
}

export const ReminderCard: React.FC<ReminderCardProps> = ({ reminder, onComplete, onDelete }) => {
    const [countdown, setCountdown] = useState(reminder.countdown || '');
    const [isDue, setIsDue] = useState(false);
    const hasPlayedRef = useRef(false);

    const playDueBeep = () => {
        // 到点提示音：用 WebAudio 生成短促“滴”声，避免引入静态音频资源
        try {
            const AudioCtx = window.AudioContext || (window as any).webkitAudioContext;
            if (!AudioCtx) return;
            const ctx: AudioContext = new AudioCtx();

            const osc = ctx.createOscillator();
            const gain = ctx.createGain();

            osc.type = 'triangle';
            osc.frequency.value = 880; // A5

            gain.gain.setValueAtTime(0.0001, ctx.currentTime);
            gain.gain.exponentialRampToValueAtTime(0.25, ctx.currentTime + 0.01);
            gain.gain.exponentialRampToValueAtTime(0.0001, ctx.currentTime + 0.18);

            osc.connect(gain);
            gain.connect(ctx.destination);

            osc.start();
            osc.stop(ctx.currentTime + 0.2);

            osc.onended = () => {
                // 释放音频资源
                ctx.close().catch(() => undefined);
            };
        } catch {
            // 某些浏览器/策略下可能禁止非用户手势播放，失败就静默
        }
    };

    useEffect(() => {
        const updateCountdown = () => {
            const now = new Date();
            const target = new Date(reminder.startAt);
            const diff = target.getTime() - now.getTime();

            if (diff <= 0) {
                setCountdown('⏰ 已触发');
                setIsDue(true);
                return;
            }
            setIsDue(false);

            const days = Math.floor(diff / (1000 * 60 * 60 * 24));
            const hours = Math.floor((diff % (1000 * 60 * 60 * 24)) / (1000 * 60 * 60));
            const minutes = Math.floor((diff % (1000 * 60 * 60)) / (1000 * 60));
            const seconds = Math.floor((diff % (1000 * 60)) / 1000);

            if (days > 0) setCountdown(`${days}天${hours}时${minutes}分`);
            else if (hours > 0) setCountdown(`${hours}时${minutes}分${seconds}秒`);
            else if (minutes > 0) setCountdown(`${minutes}分${seconds}秒`);
            else setCountdown(`${seconds}秒`);
        };

        updateCountdown();
        const timer = setInterval(updateCountdown, 1000);
        return () => clearInterval(timer);
    }, [reminder.startAt]);

    useEffect(() => {
        if (!isDue) return;
        if (reminder.status !== 'pending') return;
        if (hasPlayedRef.current) return;
        hasPlayedRef.current = true;
        playDueBeep();
    }, [isDue, reminder.status]);

    const getTypeIcon = (type: string) => {
        switch (type) {
            case 'medicine': return '💊';
            case 'meeting': return '📅';
            case 'alarm': return '⏰';
            case 'birthday': return '🎂';
            default: return '📝';
        }
    };

    return (
        <div className={`${styles.card} ${styles[reminder.type]} ${isDue && reminder.status === 'pending' ? styles.due : ''}`}>
            <div className={styles.header}>
                <span className={styles.typeIcon}>{getTypeIcon(reminder.type)}</span>
                <span className={styles.countdown}>{countdown}</span>
            </div>
            <div className={styles.content}>{reminder.content}</div>
            <div className={styles.time}>
                触发时间: {new Date(reminder.startAt).toLocaleString('zh-CN')}
            </div>
            <div className={styles.actions}>
                <button onClick={() => onComplete(reminder.id)} className={styles.completeBtn}>✅ 完成</button>
                <button onClick={() => onDelete(reminder.id)} className={styles.deleteBtn}>🗑️ 删除</button>
            </div>
        </div>
    );
};