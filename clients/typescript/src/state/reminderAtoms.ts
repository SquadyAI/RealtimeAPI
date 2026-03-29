import { atom } from 'jotai';
import type { Reminder } from '../types/reminder';

export const remindersAtom = atom<Reminder[]>([]);
export const activeRemindersAtom = atom((get) =>
    get(remindersAtom).filter(r => r.status === 'pending')
);