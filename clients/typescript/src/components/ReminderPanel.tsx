import React from 'react';
import { useAtomValue, useSetAtom } from 'jotai';
import { remindersAtom } from '../state/reminderAtoms';
import { ReminderCard } from './ReminderCard';
import styles from './ReminderPanel.module.scss';

export const ReminderPanel: React.FC = () => {
    const reminders = useAtomValue(remindersAtom);
    const setReminders = useSetAtom(remindersAtom);

    const handleComplete = (id: string) => {
        // 完成即销毁组件：从列表移除即可（UI会卸载该 ReminderCard）
        setReminders(prev => prev.filter(r => r.id !== id));
    };

    const handleDelete = (id: string) => {
        setReminders(prev => prev.filter(r => r.id !== id));
    };

    return (
        <div className={styles.panel}>
            <h3>📌 提醒列表</h3>
            <div className={styles.list}>
                {reminders.length === 0 ? (
                    <div className={styles.emptyState}>
                        <span style={{ fontSize: '48px', opacity: 0.5 }}>📭</span>
                        <p>暂无提醒</p>
                        <p style={{ fontSize: '12px', color: '#999' }}>AI 会在需要时创建提醒</p>
                    </div>
                ) : (
                    reminders.map(r => (
                        <ReminderCard key={r.id} reminder={r} onComplete={handleComplete} onDelete={handleDelete} />
                    ))
                )}
            </div>
        </div>
    );
};