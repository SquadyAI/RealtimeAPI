import React, { useState, useCallback } from 'react';
import { ProtocolId } from '../sdk/protocol/ClientProtocol';

interface VisionTestPanelProps {
    realTime: any;
    sessionStatus: { connected: boolean; sessionCreated: boolean };
}

export const VisionTestPanel: React.FC<VisionTestPanelProps> = ({ realTime, sessionStatus }) => {
    const [logs, setLogs] = useState<string[]>([]);

    const addLog = useCallback((message: string) => {
        const timestamp = new Date().toLocaleTimeString();
        setLogs(prev => [`[${timestamp}] ${message}`, ...prev].slice(0, 20));
    }, []);

    const testImageSend = useCallback(async () => {
        addLog('开始测试图像发送功能...');

        if (!sessionStatus.connected) {
            addLog('错误: WebSocket未连接');
            return;
        }

        if (!sessionStatus.sessionCreated) {
            addLog('错误: 会话未创建');
            return;
        }

        // 创建一个简单的测试图像 (1x1 像素的透明PNG)
        const testImageBase64 = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==';
        const testPrompt = '这是一个测试图像，请分析一下';

        try {
            // 将Base64转换为Uint8Array
            const binaryString = atob(testImageBase64);
            const bytes = new Uint8Array(binaryString.length);
            for (let i = 0; i < binaryString.length; i++) {
                bytes[i] = binaryString.charCodeAt(i);
            }

            addLog(`创建测试图像数据: ${bytes.length} 字节`);
            addLog(`测试提示词: "${testPrompt}"`);

            const voiceChatCore = realTime?.getVoiceChatCore();
            addLog(`会话ID: ${voiceChatCore?.currentSessionId}`);
            addLog(`连接状态: ${voiceChatCore?.isConnected}`);
            addLog(`会话状态: ${voiceChatCore?.isSessionCreated}`);

            // 发送图像（包含提示词）
            const result = realTime?.sendImageData(bytes, testPrompt, ProtocolId.All);

            if (result) {
                addLog('✅ 图像发送成功（新payload格式）');
                addLog(`   - 图像数据: ${bytes.length} 字节`);
                addLog(`   - 提示词: ${new TextEncoder().encode(testPrompt).length} 字节`);
                addLog(`   - 总payload: ${4 + new TextEncoder().encode(testPrompt).length + bytes.length} 字节`);
            } else {
                addLog('❌ 图像发送失败');
            }
        } catch (error) {
            addLog(`❌ 发送失败: ${error instanceof Error ? error.message : String(error)}`);
        }
    }, [realTime, sessionStatus, addLog]);

    const clearLogs = useCallback(() => {
        setLogs([]);
    }, []);

    return (
        <div style={{
            position: 'fixed',
            top: '10px',
            right: '10px',
            width: '300px',
            maxHeight: '400px',
            background: 'rgba(0, 0, 0, 0.8)',
            color: 'white',
            padding: '1rem',
            borderRadius: '8px',
            fontSize: '12px',
            zIndex: 9999
        }}>
            <h4 style={{ margin: '0 0 1rem 0' }}>视觉功能测试面板</h4>

            <div style={{ marginBottom: '1rem' }}>
                <button
                    onClick={testImageSend}
                    style={{
                        padding: '0.5rem 1rem',
                        background: '#4CAF50',
                        color: 'white',
                        border: 'none',
                        borderRadius: '4px',
                        cursor: 'pointer',
                        marginRight: '0.5rem'
                    }}
                >
                    测试图像发送
                </button>
                <button
                    onClick={clearLogs}
                    style={{
                        padding: '0.5rem 1rem',
                        background: '#f44336',
                        color: 'white',
                        border: 'none',
                        borderRadius: '4px',
                        cursor: 'pointer'
                    }}
                >
                    清除日志
                </button>
            </div>

            <div style={{
                maxHeight: '250px',
                overflowY: 'auto',
                background: 'rgba(255, 255, 255, 0.1)',
                padding: '0.5rem',
                borderRadius: '4px'
            }}>
                {logs.length === 0 ? (
                    <div style={{ opacity: 0.6 }}>点击"测试图像发送"开始测试...</div>
                ) : (
                    logs.map((log, index) => (
                        <div key={index} style={{
                            marginBottom: '0.25rem',
                            wordBreak: 'break-all',
                            lineHeight: '1.3'
                        }}>
                            {log}
                        </div>
                    ))
                )}
            </div>
        </div>
    );
};
