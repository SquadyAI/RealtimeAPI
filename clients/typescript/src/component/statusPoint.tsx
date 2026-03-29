/* eslint-disable @typescript-eslint/no-unused-vars */
import React, { useMemo } from 'react';
import { wsConnectedAtom, sessionCreatedAtom } from '../state';
import { useAtomValue } from 'jotai';

export const StatusPoint = React.memo(function StatusPoint() {
    const isWsConnected = useAtomValue(wsConnectedAtom);
    const isSessionCreated = useAtomValue(sessionCreatedAtom);

    // 优化内联样式，减少重渲染时的对象创建
    const statusPointStyle = useMemo(() => {
        let backgroundColor = '#ef4444'; // 默认红色 - 未连接

        if (isWsConnected && isSessionCreated) {
            backgroundColor = '#00ff00'; // 绿色 - 连接且已创建会话
        } else if (isWsConnected && !isSessionCreated) {
            backgroundColor = '#3b82f6'; // 蓝色 - 已连接但未创建会话
        }

        return {
            marginLeft: '8px',
            width: '12px',
            height: '12px',
            borderRadius: '50%',
            backgroundColor,
        };
    }, [isWsConnected, isSessionCreated]);

    return (
        <div style={statusPointStyle} />
    );
});
