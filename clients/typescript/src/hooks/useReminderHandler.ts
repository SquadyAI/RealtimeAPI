import { useCallback } from 'react';
import { useSetAtom } from 'jotai';
import { remindersAtom } from '../state/reminderAtoms';
import type { ReminderArguments, Reminder } from '../types/reminder';

const calculateCountdown = (startAt: string): string => {
    const now = new Date();
    const target = new Date(startAt);
    const diff = target.getTime() - now.getTime();

    if (diff <= 0) return '已触发';

    const days = Math.floor(diff / (1000 * 60 * 60 * 24));
    const hours = Math.floor((diff % (1000 * 60 * 60 * 24)) / (1000 * 60 * 60));
    const minutes = Math.floor((diff % (1000 * 60 * 60)) / (1000 * 60));
    const seconds = Math.floor((diff % (1000 * 60)) / 1000);

    if (days > 0) return `${days}天${hours}小时`;
    if (hours > 0) return `${hours}小时${minutes}分`;
    if (minutes > 0) return `${minutes}分${seconds}秒`;
    return `${seconds}秒`;
};

export const useReminderHandler = () => {
    const setReminders = useSetAtom(remindersAtom);

    const handleReminderCall = useCallback((args: ReminderArguments): Reminder => {
        const reminderId = `reminder_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;

        const reminder: Reminder = {
            id: reminderId,
            content: args.content,
            startAt: args.startAt,
            type: args.reminderType || 'custom',
            status: 'pending',
            createdAt: new Date().toISOString(),
            countdown: calculateCountdown(args.startAt)
        };

        setReminders(prev => [...prev, reminder]);
        console.log('[Reminder] 新增提醒:', reminder);
        return reminder;
    }, [setReminders]);

    return { handleReminderCall };
};