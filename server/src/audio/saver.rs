use std::time::Instant;

use std::path::Path;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tracing::{info, warn, error};

use crate::asr::AudioSegmentData;

/// 音频保存器统计信息
#[derive(Debug)]
pub struct SimpleAudioSaverStats {
    pub save_directory: String,
    pub total_files: usize,
    pub enabled: bool,
}

/// VAD语音段收集器
#[derive(Debug)]
pub struct VadSegmentCollector {
    /// 当前完整语音段的音频样本
    current_complete_segment: Vec<f32>,
    /// 是否正在收集完整语音段
    is_collecting_complete_segment: bool,
    /// 完整语音段开始时间
    complete_segment_start_time: Option<Instant>,
    /// 会话ID
    session_id: String,
}

impl VadSegmentCollector {
    pub fn new(session_id: String) -> Self {
        Self {
            current_complete_segment: Vec::new(),
            is_collecting_complete_segment: false,
            complete_segment_start_time: None,
            session_id,
        }
    }

    /// 处理来自ASR模块的音频段数据，组装成完整的语音段
    pub fn handle_audio_segment(&mut self, segment: &AudioSegmentData) -> Option<VadAudioSegment> {
        let segment_duration_ms = (segment.samples.len() as f32 / 16000.0) * 1000.0;

        // 启发式判断：
        // 1. 如果音频段为空，说明是VAD结束标志，需要结束当前收集
        // 2. 如果音频段包含较长音频（>100ms），说明可能是语音段开始
        // 3. 继续收集中间音频段

        if segment.samples.is_empty() {
            // 空音频段，说明VAD检测到语音结束
            info!("🔚 收到空音频段，VAD语音结束");
            return self.force_end_segment();
        }

        if !self.is_collecting_complete_segment {
            // 开始新的语音段收集
            info!("🎤 开始收集完整VAD语音段，音频长度: {:.0}ms", segment_duration_ms);
            self.start_complete_segment();
            self.current_complete_segment.extend_from_slice(&segment.samples);

            // 如果是较长的音频段（比如>150ms），可能包含完整的短语音
            if segment_duration_ms > 150.0 {
                info!("🎯 检测到较长语音段（{:.0}ms），可能是完整语音", segment_duration_ms);
                // 暂时不结束，等待更多音频段或空音频段信号
            }

            None
        } else {
            // 继续收集音频段
            // info!("🎙️ 继续收集VAD语音段，新增: {:.0}ms", segment_duration_ms);
            self.current_complete_segment.extend_from_slice(&segment.samples);

            // 继续收集，等待空音频段作为结束信号
            None
        }
    }

    /// 强制结束当前语音段（用于VAD状态变化）
    pub fn force_end_segment(&mut self) -> Option<VadAudioSegment> {
        if self.is_collecting_complete_segment {
            self.end_complete_segment()
        } else {
            None
        }
    }

    /// 开始收集完整语音段
    pub fn start_complete_segment(&mut self) {
        self.current_complete_segment.clear();
        self.is_collecting_complete_segment = true;
        self.complete_segment_start_time = Some(Instant::now());
    }

    /// 结束完整语音段收集并返回结果
    pub fn end_complete_segment(&mut self) -> Option<VadAudioSegment> {
        if !self.is_collecting_complete_segment {
            return None;
        }

        self.is_collecting_complete_segment = false;

        if self.current_complete_segment.is_empty() {
            warn!("完整语音段为空，跳过保存");
            return None;
        }

        let segment = VadAudioSegment {
            samples: self.current_complete_segment.clone(),
            session_id: self.session_id.clone(),
            start_time: self.complete_segment_start_time.unwrap_or_else(Instant::now),
            duration_ms: (self.current_complete_segment.len() as f32 / 16000.0) * 1000.0,
        };

        info!("✅ 完整VAD语音段收集完成，总时长: {:.0}ms, 总样本数: {}",
            segment.duration_ms, segment.samples.len());

        self.current_complete_segment.clear();
        self.complete_segment_start_time = None;

        Some(segment)
    }
}

/// VAD检测到的完整语音段
#[derive(Debug, Clone)]
pub struct VadAudioSegment {
    /// 音频样本
    pub samples: Vec<f32>,
    /// 会话ID
    pub session_id: String,
    /// 开始时间
    pub start_time: Instant,
    /// 时长（毫秒）
    pub duration_ms: f32,
}

/// 简单的音频保存器
#[derive(Debug)]
pub struct SimpleAudioSaver {
    save_directory: String,
    file_counter: std::sync::atomic::AtomicUsize,
    enabled: bool,
}

impl SimpleAudioSaver {
    pub async fn new(save_directory: &str) -> Result<Self, anyhow::Error> {
        // 创建保存目录
        tokio::fs::create_dir_all(save_directory).await?;

        Ok(Self {
            save_directory: save_directory.to_string(),
            file_counter: std::sync::atomic::AtomicUsize::new(0),
            enabled: true,
        })
    }

    pub async fn save_vad_segment(&self, segment: &VadAudioSegment) -> Result<(), anyhow::Error> {
        if !self.enabled || segment.samples.is_empty() {
            return Ok(());
        }

        // 生成文件名
        let counter = self.file_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let filename = format!(
            "vad_segment_{}_{}_{}_{:06}.wav",
            segment.session_id,
            timestamp,
            segment.start_time.elapsed().as_millis(),
            counter
        );
        let filepath = Path::new(&self.save_directory).join(&filename);

        // 保存为WAV文件
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };

        let mut writer = hound::WavWriter::create(&filepath, spec)?;
        for &sample in &segment.samples {
            writer.write_sample(sample)?;
        }
        writer.finalize()?;

        info!(
            "💾 保存VAD语音段: {} (时长: {:.0}ms, 样本数: {})",
            filename,
            segment.duration_ms,
            segment.samples.len()
        );

        Ok(())
    }

    pub fn get_stats(&self) -> SimpleAudioSaverStats {
        SimpleAudioSaverStats {
            save_directory: self.save_directory.clone(),
            total_files: self.file_counter.load(std::sync::atomic::Ordering::SeqCst),
            enabled: self.enabled,
        }
    }
}

/// 全局音频保存器管理器
#[derive(Debug, Clone)]
pub struct GlobalAudioSaver {
    saver: Arc<SimpleAudioSaver>,
    save_tx: Arc<mpsc::UnboundedSender<VadAudioSegment>>, // 🆕 新增：保存通道发送端
}

impl GlobalAudioSaver {
    /// 创建全局音频保存器
    pub async fn new(save_directory: &str) -> Result<Self, anyhow::Error> {
        let saver = Arc::new(SimpleAudioSaver::new(save_directory).await?);

        // 🆕 创建后台保存通道
        let (save_tx, mut save_rx) = mpsc::unbounded_channel::<VadAudioSegment>();
        let save_tx = Arc::new(save_tx);

        // 🆕 启动后台保存任务
        let saver_clone = saver.clone();
        tokio::spawn(async move {
            info!("🔄 启动全局音频保存器后台任务");
            let mut pending_count = 0;

            while let Some(segment) = save_rx.recv().await {
                pending_count += 1;
                info!("📥 收到音频段保存请求: session={}, 时长={:.0}ms (待处理: {})",
                      segment.session_id, segment.duration_ms, pending_count);

                // 在后台异步保存
                let saver = saver_clone.clone();
                tokio::spawn(async move {
                    match saver.save_vad_segment(&segment).await {
                        Ok(_) => {
                            info!("✅ 后台保存音频段成功: session={}, 时长={:.0}ms",
                                  segment.session_id, segment.duration_ms);
                        },
                        Err(e) => {
                            error!("❌ 后台保存音频段失败: session={}, error={}",
                                   segment.session_id, e);
                        }
                    }
                });

                pending_count -= 1;
            }
            info!("🔄 全局音频保存器后台任务结束");
        });

        Ok(Self { saver, save_tx })
    }

    /// 🆕 非阻塞保存VAD语音段（立即返回，后台保存）
    pub fn save_vad_segment_async(&self, segment: VadAudioSegment) -> Result<(), anyhow::Error> {
        // 立即发送到后台保存队列，不等待保存完成
        self.save_tx.send(segment)
            .map_err(|e| anyhow::anyhow!("发送音频段到后台保存队列失败: {}", e))
    }

    /// 🆕 同步保存VAD语音段（阻塞等待保存完成）
    pub async fn save_vad_segment(&self, segment: &VadAudioSegment) -> Result<(), anyhow::Error> {
        self.saver.save_vad_segment(segment).await
    }

    /// 获取统计信息
    pub fn get_stats(&self) -> SimpleAudioSaverStats {
        
        // 🆕 添加待保存文件数量（这里简化处理，实际可以通过原子计数器实现）
        self.saver.get_stats()
    }
}

lazy_static::lazy_static! {
    static ref GLOBAL_AUDIO_SAVER: Arc<Mutex<Option<GlobalAudioSaver>>> = Arc::new(Mutex::new(None));
}

/// 获取全局音频保存器实例
pub async fn get_global_audio_saver() -> Result<Arc<GlobalAudioSaver>, anyhow::Error> {
    let mut saver_guard = GLOBAL_AUDIO_SAVER.lock().await;

    if saver_guard.is_none() {
        // 🆕 从环境变量获取保存根目录，如果未设置则返回错误
        match std::env::var("AUDIO_SAVE_ROOT") {
            Ok(save_root) => {
                info!("💾 从环境变量获取音频保存根目录: {}", save_root);

                // 创建新的全局音频保存器
                let global_saver = GlobalAudioSaver::new(&save_root).await?;
                *saver_guard = Some(global_saver);
                info!("🌍 创建全局音频保存器实例，保存目录: {}", save_root);
            },
            Err(_) => {
                // 🆕 如果环境变量未设置，则不创建保存器，返回错误
                info!("⚠️ AUDIO_SAVE_ROOT 环境变量未设置，音频保存功能已禁用");
                return Err(anyhow::anyhow!("AUDIO_SAVE_ROOT 环境变量未设置，音频保存功能已禁用"));
            }
        }
    }

    Ok(Arc::new(saver_guard.as_ref().unwrap().clone()))
}
