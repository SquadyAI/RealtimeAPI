/**
 * 二进制音频协议测试入口
 *
 * 这个文件提供了一个简单的测试入口，可以在浏览器控制台中运行
 * 来测试二进制音频解码器的功能。
 *
 * 注意：测试包含传统的 response.audio.delta 和新的 binary_audio_delta 协议
 */

import {
  testBinaryAudioDecoder,
  performanceBenchmark
} from './BinaryAudioDecoder.test';

import {
  createMockBinaryPacket,
  decodeResponseAudioDeltaMessage
} from './BinaryAudioDecoder';

import {
  AudioProtocolType,
  createDefaultAudioAdapter
} from './BinaryAudioAdapter';

/**
 * 运行所有测试
 */
export async function runAllTests(): Promise<void> {
  console.log('🚀 开始运行二进制音频协议测试套件...');
  
  try {
    // 1. 基础解码器测试
    await testBinaryAudioDecoder();
    
    // 2. 性能基准测试
    await performanceBenchmark();
    
    // 3. 适配器测试
    await testAudioAdapter();
    
    console.log('✅ 所有测试完成！');
  } catch (error) {
    console.error('❌ 测试失败:', error);
    throw error;
  }
}

/**
 * 测试音频适配器
 */
async function testAudioAdapter(): Promise<void> {
  console.log('📦 测试音频适配器...');
  
  const adapter = createDefaultAudioAdapter();
  
  // 测试JSON协议（传统 response.audio.delta）
  const jsonData = JSON.stringify({
    type: "response.audio.delta",
    event_id: "test-123",
    response_id: "resp-456",
    item_id: "item-789",
    output_index: 0,
    content_index: 1,
    delta: btoa('mock-audio-data') // Base64编码的模拟音频数据
  });
  
  try {
    const jsonResult = adapter.processAudioData(jsonData);
    console.log('✅ JSON协议测试通过:', {
      type: jsonResult.type,
      responseId: jsonResult.responseId,
      itemId: jsonResult.itemId,
      dataSize: jsonResult.data.length
    });
  } catch (error) {
    console.error('❌ JSON协议测试失败:', error);
  }
  
  // 测试二进制协议
  const binaryData = createMockBinaryPacket({
    responseId: 'binary-resp-123',
    itemId: 'binary-item-456',
    outputIndex: 1,
    contentIndex: 2,
    audioData: new Uint8Array([0x01, 0x02, 0x03, 0x04, 0x05])
  });
  
  try {
    adapter.setProtocolType(AudioProtocolType.BINARY);
    const binaryResult = adapter.processAudioData(binaryData);
    console.log('✅ 二进制协议测试通过:', {
      type: binaryResult.type,
      responseId: binaryResult.responseId,
      itemId: binaryResult.itemId,
      outputIndex: binaryResult.outputIndex,
      contentIndex: binaryResult.contentIndex,
      dataSize: binaryResult.data.length
    });
  } catch (error) {
    console.error('❌ 二进制协议测试失败:', error);
  }
  
  // 测试自动检测
  try {
    adapter.setProtocolType(AudioProtocolType.AUTO);
    const autoResult1 = adapter.processAudioData(jsonData);
    const autoResult2 = adapter.processAudioData(binaryData);
    
    console.log('✅ 自动检测测试通过:', {
      jsonDataProcessed: !!autoResult1,
      binaryDataProcessed: !!autoResult2,
      currentProtocol: adapter.getCurrentProtocol()
    });
  } catch (error) {
    console.error('❌ 自动检测测试失败:', error);
  }
  
  // 显示适配器状态
  console.log('📊 适配器状态:', adapter.getStatus());
  console.log('📊 性能指标:', adapter.getPerformanceMetrics());
}

/**
 * 快速测试解码器
 */
export function quickTest(): void {
  console.log('⚡ 快速测试二进制音频解码器...');
  
  try {
    // 创建测试数据包
    const testPacket = createMockBinaryPacket();
    console.log('📦 测试数据包创建成功，大小:', testPacket.byteLength, '字节');
    
    // 解码数据包
    const decoded = decodeResponseAudioDeltaMessage(testPacket);
    console.log('🔓 解码成功:', {
      responseId: decoded.responseId,
      itemId: decoded.itemId,
      outputIndex: decoded.outputIndex,
      contentIndex: decoded.contentIndex,
      audioDataSize: decoded.audioData.length
    });
    
    console.log('✅ 快速测试通过！');
  } catch (error) {
    console.error('❌ 快速测试失败:', error);
  }
}

/**
 * 测试协议兼容性
 */
export function testCompatibility(): void {
  console.log('🔄 测试协议兼容性...');
  
  const adapter = createDefaultAudioAdapter();
  
  // 测试传统JSON格式（response.audio.delta - 备用方案）
  const traditionalJson = {
    type: "response.audio.delta",
    event_id: "test-event",
    response_id: "test-response",
    item_id: "test-item",
    output_index: 0,
    content_index: 1,
    delta: "dGVzdC1hdWRpby1kYXRh" // "test-audio-data" in base64
  };
  
  try {
    const result1 = adapter.processAudioData(JSON.stringify(traditionalJson));
    console.log('✅ 传统JSON格式兼容:', result1.responseId === 'test-response');
  } catch (error) {
    console.error('❌ 传统JSON格式不兼容:', error);
  }
  
  // 测试新的二进制格式（binary_audio_delta - 优先方案）
  const binaryPacket = createMockBinaryPacket({
    responseId: 'binary-test-response',
    itemId: 'binary-test-item'
  });
  
  try {
    adapter.setProtocolType(AudioProtocolType.BINARY);
    const result2 = adapter.processAudioData(binaryPacket);
    console.log('✅ 二进制格式兼容:', result2.responseId === 'binary-test-response');
  } catch (error) {
    console.error('❌ 二进制格式不兼容:', error);
  }
  
  console.log('🔄 协议兼容性测试完成');
}

// 在浏览器环境中暴露测试函数
if (typeof window !== 'undefined') {
  (window as any).testBinaryAudio = {
    runAllTests,
    quickTest,
    testCompatibility,
    testBinaryAudioDecoder,
    performanceBenchmark
  };
  
  console.log('🧪 二进制音频测试函数已加载到 window.testBinaryAudio');
  console.log('💡 使用方法:');
  console.log('  - window.testBinaryAudio.quickTest() - 快速测试');
  console.log('  - window.testBinaryAudio.runAllTests() - 运行所有测试');
  console.log('  - window.testBinaryAudio.testCompatibility() - 测试兼容性');
  console.log('  - window.testBinaryAudio.performanceBenchmark() - 性能基准测试');
}

// 导出测试函数供其他模块使用
export {
  testBinaryAudioDecoder,
  performanceBenchmark
};