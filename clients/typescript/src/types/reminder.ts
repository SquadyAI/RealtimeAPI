export type ReminderType = 'medicine' | 'meeting' | 'alarm' | 'birthday' | 'custom';

export interface Reminder {
    id: string;
    content: string;
    startAt: string;
    type: ReminderType;
    status: 'pending' | 'completed' | 'expired';
    createdAt: string;
    countdown?: string;
}

export interface ReminderArguments {
    content: string;
    startAt: string;
    reminderType: ReminderType;
}