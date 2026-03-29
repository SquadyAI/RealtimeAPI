/**
 * 二进制音频解码器测试
 * 
 * 这个文件包含了二进制音频解码器的测试用例，验证解码功能的正确性
 */

import {
  decodeResponseAudioDeltaMessage,
  decodeResponseAudioDeltaMessageWithMetrics,
  createMockBinaryPacket
} from './BinaryAudioDecoder';
import {
  DecodingError,
  BINARY_AUDIO_PROTOCOL
} from './BinaryAudioTypes';

/**
 * 测试解码功能
 */
export async function testBinaryAudioDecoder(): Promise<void> {
  console.log('=== 开始测试二进制音频解码器 ===');
  
  try {
    // 测试1: 基本解码功能
    await testBasicDecoding();
    
    // 测试2: 错误处理
    await testErrorHandling();
    
    // 测试3: 性能监控
    await testPerformanceMonitoring();
    
    // 测试4: 边界情况
    await testEdgeCases();
    
    console.log('=== 所有测试通过！ ===');
  } catch (error) {
    console.error('=== 测试失败 ===', error);
    throw error;
  }
}

/**
 * 测试基本解码功能
 */
async function testBasicDecoding(): Promise<void> {
  console.log('测试1: 基本解码功能');
  
  // 创建模拟数据包
  const mockPacket = createMockBinaryPacket({
    responseId: 'test-response-123',
    itemId: 'test-item-456',
    outputIndex: 1,
    contentIndex: 2,
    audioData: new Uint8Array([0x01, 0x02, 0x03, 0x04])
  });
  
  // 解码数据包
  const decoded = decodeResponseAudioDeltaMessage(mockPacket);
  
  // 验证结果
  if (decoded.responseId !== 'test-response-123') {
    throw new Error(`responseId不匹配: 期望 'test-response-123', 实际 '${decoded.responseId}'`);
  }
  
  if (decoded.itemId !== 'test-item-456') {
    throw new Error(`itemId不匹配: 期望 'test-item-456', 实际 '${decoded.itemId}'`);
  }
  
  if (decoded.outputIndex !== 1) {
    throw new Error(`outputIndex不匹配: 期望 1, 实际 ${decoded.outputIndex}`);
  }
  
  if (decoded.contentIndex !== 2) {
    throw new Error(`contentIndex不匹配: 期望 2, 实际 ${decoded.contentIndex}`);
  }
  
  if (decoded.audioData.length !== 4) {
    throw new Error(`audioData长度不匹配: 期望 4, 实际 ${decoded.audioData.length}`);
  }
  
  console.log('✓ 基本解码功能测试通过');
}

/**
 * 测试错误处理
 */
async function testErrorHandling(): Promise<void> {
  console.log('测试2: 错误处理');
  
  // 测试缓冲区太小
  try {
    decodeResponseAudioDeltaMessage(new ArrayBuffer(10));
    throw new Error('应该抛出缓冲区太小的错误');
  } catch (error) {
    if (!(error instanceof DecodingError) || error.code !== 'BUFFER_TOO_SMALL') {
      throw new Error(`错误类型不正确: 期望 DecodingError with code 'BUFFER_TOO_SMALL'`);
    }
  }
  
  // 测试无效协议ID
  const invalidProtocolPacket = createMockBinaryPacket();
  const view = new DataView(invalidProtocolPacket);
  view.setUint8(BINARY_AUDIO_PROTOCOL.PROTOCOL_ID_OFFSET, 999); // 无效的协议ID
  
  try {
    decodeResponseAudioDeltaMessage(invalidProtocolPacket);
    throw new Error('应该抛出无效协议ID的错误');
  } catch (error) {
    if (!(error instanceof DecodingError) || error.code !== 'INVALID_PROTOCOL_ID') {
      throw new Error(`错误类型不正确: 期望 DecodingError with code 'INVALID_PROTOCOL_ID'`);
    }
  }
  
  // 测试无效命令ID
  const invalidCommandPacket = createMockBinaryPacket();
  const cmdView = new DataView(invalidCommandPacket);
  cmdView.setUint8(BINARY_AUDIO_PROTOCOL.COMMAND_ID_OFFSET, 999); // 无效的命令ID
  
  try {
    decodeResponseAudioDeltaMessage(invalidCommandPacket);
    throw new Error('应该抛出无效命令ID的错误');
  } catch (error) {
    if (!(error instanceof DecodingError) || error.code !== 'INVALID_COMMAND_ID') {
      throw new Error(`错误类型不正确: 期望 DecodingError with code 'INVALID_COMMAND_ID'`);
    }
  }
  
  console.log('✓ 错误处理测试通过');
}

/**
 * 测试性能监控
 */
async function testPerformanceMonitoring(): Promise<void> {
  console.log('测试3: 性能监控');
  
  const mockPacket = createMockBinaryPacket();
  
  // 使用带性能监控的解码函数
  const result = decodeResponseAudioDeltaMessageWithMetrics(mockPacket, {
    enablePerformanceMonitoring: true,
    enableDebugLogging: true
  });
  
  if (!result.success) {
    throw new Error('解码应该成功');
  }
  
  if (result.duration <= 0) {
    throw new Error(`解码时间应该大于0: 实际 ${result.duration}ms`);
  }
  
  if (result.bytesProcessed !== mockPacket.byteLength) {
    throw new Error(`处理字节数不匹配: 期望 ${mockPacket.byteLength}, 实际 ${result.bytesProcessed}`);
  }
  
  console.log(`✓ 性能监控测试通过 (解码时间: ${result.duration.toFixed(2)}ms, 字节数: ${result.bytesProcessed})`);
}

/**
 * 测试边界情况
 */
async function testEdgeCases(): Promise<void> {
  console.log('测试4: 边界情况');
  
  // 测试空音频数据
  const emptyAudioPacket = createMockBinaryPacket({
    audioData: new Uint8Array(0)
  });
  
  try {
    decodeResponseAudioDeltaMessage(emptyAudioPacket);
    throw new Error('应该抛出音频数据为空的错误');
  } catch (error) {
    if (!(error instanceof DecodingError) || error.code !== 'CORRUPTED_DATA') {
      throw new Error(`错误类型不正确: 期望 DecodingError with code 'CORRUPTED_DATA'`);
    }
  }
  
  // 测试最大长度的字符串
  const maxResponseId = 'a'.repeat(BINARY_AUDIO_PROTOCOL.STRING_LIMITS.RESPONSE_ID_MAX_LENGTH);
  const maxItemId = 'b'.repeat(BINARY_AUDIO_PROTOCOL.STRING_LIMITS.ITEM_ID_MAX_LENGTH);
  
  const maxStringPacket = createMockBinaryPacket({
    responseId: maxResponseId,
    itemId: maxItemId
  });
  
  const decoded = decodeResponseAudioDeltaMessage(maxStringPacket);
  
  if (decoded.responseId !== maxResponseId) {
    throw new Error('最大长度responseId解码失败');
  }
  
  if (decoded.itemId !== maxItemId) {
    throw new Error('最大长度itemId解码失败');
  }
  
  // 测试超过最大长度的字符串
  const tooLongResponseId = 'a'.repeat(BINARY_AUDIO_PROTOCOL.STRING_LIMITS.RESPONSE_ID_MAX_LENGTH + 1);
  const tooLongPacket = createMockBinaryPacket({
    responseId: tooLongResponseId
  });
  
  try {
    decodeResponseAudioDeltaMessage(tooLongPacket);
    throw new Error('应该抛出字符串长度过长的错误');
  } catch (error) {
    if (!(error instanceof DecodingError) || error.code !== 'INVALID_STRING_LENGTH') {
      throw new Error(`错误类型不正确: 期望 DecodingError with code 'INVALID_STRING_LENGTH'`);
    }
  }
  
  console.log('✓ 边界情况测试通过');
}

/**
 * 性能基准测试
 */
export async function performanceBenchmark(): Promise<void> {
  console.log('=== 开始性能基准测试 ===');
  
  const iterations = 1000;
  const mockPacket = createMockBinaryPacket();
  
  // 预热
  for (let i = 0; i < 100; i++) {
    decodeResponseAudioDeltaMessage(mockPacket);
  }
  
  // 基准测试
  const startTime = performance.now();
  
  for (let i = 0; i < iterations; i++) {
    decodeResponseAudioDeltaMessage(mockPacket);
  }
  
  const endTime = performance.now();
  const totalTime = endTime - startTime;
  const averageTime = totalTime / iterations;
  const throughput = iterations / (totalTime / 1000); // 每秒操作数
  
  console.log(`性能基准测试结果:`);
  console.log(`- 总迭代次数: ${iterations}`);
  console.log(`- 总时间: ${totalTime.toFixed(2)}ms`);
  console.log(`- 平均时间: ${averageTime.toFixed(3)}ms/次`);
  console.log(`- 吞吐量: ${throughput.toFixed(0)}次/秒`);
  console.log(`- 数据包大小: ${mockPacket.byteLength}字节`);
  console.log(`- 数据处理速度: ${(throughput * mockPacket.byteLength / 1024).toFixed(1)}KB/秒`);
  
  // 验证性能目标（平均解码时间应该小于1ms）
  if (averageTime > 1.0) {
    console.warn(`⚠️  性能警告: 平均解码时间 ${averageTime.toFixed(3)}ms 超过目标 1ms`);
  } else {
    console.log(`✓ 性能目标达成: 平均解码时间 ${averageTime.toFixed(3)}ms`);
  }
}

// 如果在浏览器环境中，将测试函数暴露到全局对象
if (typeof window !== 'undefined') {
  (window as any).testBinaryAudioDecoder = testBinaryAudioDecoder;
  (window as any).performanceBenchmark = performanceBenchmark;
}

// 如果在Node.js环境中，导出测试函数
if (typeof module !== 'undefined' && module.exports) {
  module.exports = {
    testBinaryAudioDecoder,
    performanceBenchmark
  };
}