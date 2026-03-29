// MiMalloc 全局分配器必须在所有其他代码之前声明
use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use anyhow::Result;
use realtime::env_utils::{env_or_default, env_string_or_default};
use realtime::rpc::ActixRpcSystem;
use realtime::{
    AsrEngine,
    llm::LlmClient,
    monitoring::METRICS,
    rpc::RpcConfig,
    storage::{ConversationStore, InMemoryStore, StorageConfig},
    tts::minimax::global_voice_library,
};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

#[derive(Debug, Clone, Default)]
struct SystemConfig {
    rpc: RpcConfig,
}

fn load_config() -> Result<SystemConfig> {
    // 返回默认配置（现在会从环境变量读取）
    Ok(SystemConfig::default())
}
#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file (works on all platforms, including Windows)
    dotenv::dotenv().ok();

    init_logging();

    info!("🚀 启动实时语音系统...");

    // 初始化 Langfuse 追踪（可选，失败不影响启动）
    realtime::telemetry::init();

    // 初始化监控指标
    info!("📊 初始化监控指标...");
    // 触发 METRICS 的初始化
    let _ = &*METRICS;
    info!("✅ 监控指标初始化完成");

    // 🆕 初始化 IP 地理位置服务（可选）
    // 优先使用环境变量指定的路径，如果没有设置则使用项目内的默认路径
    let mmdb_path = std::env::var("GEOIP_MMDB_PATH").unwrap_or_else(|_| "src/GeoIP2-City.mmdb".to_string());

    if !mmdb_path.trim().is_empty() {
        info!("🌍 尝试初始化 IP Geolocation 服务: {}", mmdb_path);
        realtime::ip_geolocation::init_ip_geolocation_service(Some(realtime::ip_geolocation::IpGeolocationConfig {
            mmdb_path: Some(mmdb_path),
        }));
    } else {
        info!("🌐 MMDB 路径为空，跳过 IP Geolocation 初始化");
    }

    // 加载配置
    let config = load_config()?;
    info!("📋 配置加载完成");

    // 初始化引擎组件
    info!("🔧 初始化ASR引擎...");
    let asr_engine = Arc::new(
        AsrEngine::new(realtime::asr::ASRModuleConfig::default())
            .await
            .map_err(|e| anyhow::anyhow!("ASR引擎初始化失败: {}", e))?,
    );

    // 🗃️ 初始化对话存储 - 使用StorageConfig并复用共享连接池
    info!("🗃️ 初始化对话存储...");
    let storage_cfg = StorageConfig::from_env();
    let store: Arc<dyn ConversationStore> = match storage_cfg.create_conversation_store().await {
        Ok(s) => {
            info!("✅ 对话存储初始化完成");
            s
        },
        Err(e) => {
            error!("❌ 对话存储初始化失败: {}", e);
            warn!("⚠️ 回退到内存存储（数据将不会持久化）");
            Arc::new(InMemoryStore::new())
        },
    };

    info!("🔧 初始化LLM引擎...");

    // LLM_API_KEY: 可选（自托管 LLM 可能不需要 Key）
    let llm_api_key = match std::env::var("LLM_API_KEY") {
        Ok(v) if !v.is_empty() && v != "sk-your-api-key-here" && v != "none" => v,
        _ => {
            info!("ℹ️ LLM_API_KEY 未设置，将以无认证模式连接 LLM（适用于自托管模型）");
            String::new()
        },
    };
    let llm_base_url = match std::env::var("LLM_BASE_URL") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            error!("❌ LLM_BASE_URL 未设置！");
            error!("   请运行 ./realtime onboard 配置 LLM API 地址");
            error!("   或手动设置环境变量: export LLM_BASE_URL=https://api.openai.com/v1");
            std::process::exit(1);
        },
    };
    let llm_model = match std::env::var("LLM_MODEL") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            error!("❌ LLM_MODEL 未设置！");
            error!("   请运行 ./realtime onboard 配置 LLM 模型");
            error!("   或手动设置环境变量: export LLM_MODEL=gpt-4o-mini");
            std::process::exit(1);
        },
    };
    let llm_timeout = env_or_default("LLM_TIMEOUT_SECS", 30);

    // 配置验证和诊断信息
    info!("🔍 LLM配置验证:");
    info!("📍 LLM Base URL: {}", llm_base_url);
    info!("🤖 LLM Model: {}", llm_model);
    info!("⏱️ LLM Timeout: {}s", llm_timeout);
    info!(
        "🔑 LLM API Key: {}*** (长度: {})",
        &llm_api_key[..std::cmp::min(8, llm_api_key.len())],
        llm_api_key.len()
    );

    // 🆕 使用存储创建LLM客户端
    let llm_config = realtime::llm::llm::LlmConfig {
        api_key: llm_api_key,
        base_url: llm_base_url.clone(),
        model: llm_model.clone(),
        timeout_secs: llm_timeout,
        ..Default::default()
    };
    let llm_engine = Arc::new(LlmClient::from_config_with_store(llm_config, store.clone()));

    // 🆕 启动LLM连接预热（可选，帮助提前发现问题）
    info!("🔥 启动LLM连接预热...");
    llm_engine.start_connection_warm();

    // 等待连接预热完成（最多5秒）
    let is_warmed = llm_engine.wait_for_connection_warm(5000).await;
    if is_warmed {
        info!("✅ LLM连接预热成功");
    } else {
        warn!("⚠️ LLM连接预热超时，可能存在网络问题");
        warn!("   请检查LLM服务器地址: {}", llm_base_url);
    }

    // 🏭 取消全局TTS池初始化：MiniMax按需创建与复用WS连接

    // 🔍 初始化声音库（MiniMax TTS）
    info!("🔍 初始化 MiniMax 声音库...");
    init_voice_library().await;
    info!("✅ MiniMax 声音库初始化完成");

    // 🔍 全局初始化SearXNG搜索客户端
    info!("🔍 初始化SearXNG搜索引擎...");
    let searxng_config = realtime::function_callback::searxng_client::SearXNGConfig::default();
    info!(
        "🔍 SearXNG配置: 并发={}，缓存={}，最大结果={}，默认引擎={:?}",
        searxng_config.max_concurrent_requests, searxng_config.enable_cache, searxng_config.max_results, searxng_config.default_engines
    );

    match realtime::function_callback::searxng_client::init_searxng_client(searxng_config).await {
        Ok(_) => {
            info!("✅ SearXNG全局客户端初始化成功");
        },
        Err(e) => {
            error!("❌ SearXNG全局客户端初始化失败: {}", e);
            // 不影响系统启动，但搜索功能将不可用
            warn!("⚠️ 搜索功能将不可用，但系统将继续启动");
        },
    }

    // 🆕 创建Actix-Web RPC系统，替换原有的axum版本
    let actix_rpc_system = ActixRpcSystem::new(config.rpc).await?;
    info!("✅ Actix-Web RPC系统初始化完成");

    // 设置服务器地址（从环境变量读取）
    let bind_addr = env_string_or_default("BIND_ADDR", "0.0.0.0:8080");
    info!("🌐 Actix-Web服务器将绑定到: {}", bind_addr);

    // 注册信号处理器
    let actix_rpc_system_shutdown = Arc::new(actix_rpc_system);
    let actix_rpc_clone = actix_rpc_system_shutdown.clone();

    tokio::spawn(async move {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!("监听Ctrl+C信号失败: {}", e);
            return;
        }
        info!("收到Ctrl+C信号，开始关闭系统...");
        if let Err(e) = actix_rpc_clone.stop().await {
            error!("关闭Actix RPC系统失败: {}", e);
        }
        std::process::exit(0);
    });

    // 启动Actix-Web服务器
    info!("🎧 启动Actix-Web RPC服务器...");

    // ActixRpcSystem内部已经实现了自动重启机制
    match actix_rpc_system_shutdown
        .start(&bind_addr, asr_engine.clone(), Some(llm_engine.clone()), store.clone())
        .await
    {
        Ok(_) => {
            info!("✅ Actix-Web服务器正常关闭");
        },
        Err(e) => {
            error!("❌ Actix-Web服务器启动失败: {}", e);
            return Err(e);
        },
    }

    Ok(())
}

/// 初始化声音库
async fn init_voice_library() {
    // 尝试从环境变量读取配置文件路径
    let config_path = std::env::var("VOICE_LIBRARY_CONFIG_PATH").ok();

    // 尝试从环境变量读取配置JSON
    let config_json = std::env::var("VOICE_LIBRARY_CONFIG").ok();

    let loaded = if let Some(path) = config_path {
        // 从文件加载
        info!("📁 从文件加载声音库配置: {}", path);
        match tokio::fs::read_to_string(&path).await {
            Ok(json) => match global_voice_library().update_from_json(&json) {
                Ok(_) => {
                    info!("✅ 声音库配置从文件加载成功");
                    true
                },
                Err(e) => {
                    error!("❌ 声音库配置解析失败: {}", e);
                    false
                },
            },
            Err(e) => {
                error!("❌ 读取声音库配置文件失败: {}", e);
                false
            },
        }
    } else if let Some(json) = config_json {
        // 从环境变量加载
        info!("📝 从环境变量加载声音库配置");
        match global_voice_library().update_from_json(&json) {
            Ok(_) => {
                info!("✅ 声音库配置从环境变量加载成功");
                true
            },
            Err(e) => {
                error!("❌ 声音库配置解析失败: {}", e);
                false
            },
        }
    } else {
        false
    };

    // 如果没有加载配置，使用默认配置
    if !loaded {
        info!("🔧 使用默认声音库配置");
        let default_config = get_default_voice_library_config();
        match global_voice_library().update_from_json(&default_config) {
            Ok(_) => {
                info!("✅ 默认声音库配置加载成功");
                info!("💡 提示: 可以通过 POST /api/voice-library/config 更新配置");
            },
            Err(e) => {
                warn!("⚠️ 默认声音库配置加载失败: {}", e);
                warn!("   TTS服务可能无法正常工作，请通过 API 配置声音库");
            },
        }
    }

    // 显示配置状态
    if global_voice_library().is_configured() {
        info!("📊 声音库状态: 已配置 {} 个API key", global_voice_library().key_count());
        let virtual_voice_ids = global_voice_library().get_virtual_voice_ids();
        info!("🎙️ 可用的虚拟voice_id: {:?}", virtual_voice_ids);
    } else {
        warn!("⚠️ 声音库未配置，TTS服务将无法使用");
    }
}

/// 获取默认声音库配置
fn get_default_voice_library_config() -> String {
    // 默认配置：支持中文和英文虚拟voice_id映射
    r#"{
  "keys": {
    "default1": "your_minimax_api_key_here",
    "apikey": "your_api_key_here"
  },
  "gains": {
    "ttv-voice-2025120918423925-Ktg6miPT": 7.0
  },
  "speeds": {
    "Chinese (Mandarin)_Gentle_Senior": 1.1,
    "Chinese (Mandarin)_Wise_Women": 1.1,
    "ttv-voice-2026012217272626-tk9kPJcw": 1.2,
    "ttv-voice-2026012217272626-tk9kPJcw-26": 1.2
  },
  "pitches": {
    "Chinese (Mandarin)_Gentle_Senior": -1,
    "Chinese (Mandarin)_Wise_Women": 1,
    "ttv-voice-2026012217272626-tk9kPJcw": 1,
    "ttv-voice-2026012217272626-tk9kPJcw-26": 1
  },
  "models": {
    "Chinese (Mandarin)_Wise_Women": "speech-2.6-hd",
    "ttv-voice-2026012217272626-tk9kPJcw": "speech-2.8-hd",
    "ttv-voice-2026012217272626-tk9kPJcw-26": "speech-2.6-hd"
  },
  "volumes": {
    "ttv-voice-2026012217272626-tk9kPJcw": 1.56,
    "ttv-voice-2026012217272626-tk9kPJcw-26": 1.56
  },
  "zh_female_wanwanxiaohe_moon_bigtts": {
    "default1": "wanwanxiaohe_moon2"
  },
  "wanwanxiaohe_moon": {
    "default1": "wanwanxiaohe_moon2"
  },
  "en_female_lauren_moon_bigtts": {
    "default1": "en_us_female_lauren"
  },
  "ICL_en_male_aussie_v1_tob": {
    "default1": "en_au_male_ethan"
  },
  "ICL_en_male_cc_alastor_tob": {
    "default1": "en_gb_male_alastor"
  },
  "zh_male_zhoujielun_emo_v2_mars_bigtts": {
    "default1": "zh_male_zhoujielun_emo_v2_mars_bigtts"
  },
  "multi_male_M100_conversation_wvae_bigtts": {
    "default1": "multi_male_M100_conversation_wvae_bigtts"
  },
  "multi_female_shuangkuaisisi_moon_bigtts": {
    "default1": "multi_female_shuangkuaisisi_moon_bigtts"
  },
  "multi_female_gaolengyujie_moon_bigtts": {
    "default1": "multi_female_gaolengyujie_moon_bigtts"
  },
  "multi_male_wanqudashu_moon_bigtts": {
    "default1": "multi_male_wanqudashu_moon_bigtts"
  },
  "ar-AE-FatimaNeural": {
    "default1": "ar-AE-FatimaNeural"
  },
  "ar-AE-HamdanNeural": {
    "default1": "ar-AE-HamdanNeural"
  },
  "ko-KR-SunHiNeural": {
    "default1": "ko-KR-SunHiNeural"
  },
  "ko-KR-InJoonNeural": {
    "default1": "ko-KR-InJoonNeural"
  },
  "ru-RU-SvetlanaNeural": {
    "default1": "ru-RU-SvetlanaNeural"
  },
  "ru-RU-DmitryNeural": {
    "default1": "ru-RU-DmitryNeural"
  },
  "pt-BR-FranciscaNeural": {
    "default1": "pt-BR-FranciscaNeural"
  },
  "pt-BR-AntonioNeural": {
    "default1": "pt-BR-AntonioNeural"
  },
  "th-TH-PremwadeeNeural": {
    "default1": "th-TH-PremwadeeNeural"
  },
  "th-TH-NiwatNeural": {
    "default1": "th-TH-NiwatNeural"
  },
  "zh-HK-HiuMaanNeural": {
    "default1": "zh-HK-HiuMaanNeural"
  },
  "zh-HK-WanLungNeural": {
    "default1": "zh-HK-WanLungNeural"
  },
  "tr-TR-EmelNeural": {
    "default1": "tr-TR-EmelNeural"
  },
  "tr-TR-AhmetNeural": {
    "default1": "tr-TR-AhmetNeural"
  },
  "fr-FR-VivienneMultilingualNeural": {
    "default1": "fr-FR-VivienneMultilingualNeural"
  },
  "fr-FR-RemyMultilingualNeural": {
    "default1": "fr-FR-RemyMultilingualNeural"
  },
  "de-DE-SeraphinaMultilingualNeural": {
    "default1": "de-DE-SeraphinaMultilingualNeural"
  },
  "de-DE-FlorianMultilingualNeural": {
    "default1": "de-DE-FlorianMultilingualNeural"
  },
  "id-ID-ArdiNeural": {
    "default1": "id-ID-ArdiNeural"
  },
  "ms-MY-OsmanNeural": {
    "default1": "ms-MY-OsmanNeural"
  },
  "vi-VN-HoaiMyNeural": {
    "default1": "vi-VN-HoaiMyNeural"
  },
  "vi-VN-NamMinhNeural": {
    "default1": "vi-VN-NamMinhNeural"
  },
  "it-IT-IsabellaMultilingualNeural": {
    "default1": "it-IT-IsabellaMultilingualNeural"
  },
  "it-IT-AlessioMultilingualNeural": {
    "default1": "it-IT-AlessioMultilingualNeural"
  },
  "he-IL-HilaNeural": {
    "default1": "he-IL-HilaNeural"
  },
  "he-IL-AvriNeural": {
    "default1": "he-IL-AvriNeural"
  },
  "pl-PL-AgnieszkaNeural": {
    "default1": "pl-PL-AgnieszkaNeural"
  },
  "pl-PL-MarekNeural": {
    "default1": "pl-PL-MarekNeural"
  },
  "el-GR-AthinaNeural": {
    "default1": "el-GR-AthinaNeural"
  },
  "el-GR-NestorasNeural": {
    "default1": "el-GR-NestorasNeural"
  },
  "uk-UA-PolinaNeural": {
    "default1": "uk-UA-PolinaNeural"
  },
  "uk-UA-OstapNeural": {
    "default1": "uk-UA-OstapNeural"
  },
  "fa-IR-DilaraNeural": {
    "default1": "fa-IR-DilaraNeural"
  },
  "fa-IR-FaridNeural": {
    "default1": "fa-IR-FaridNeural"
  },
  "zh_female_meituojieer_moon_bigtts": {
    "default1": "zh_female_meituojieer_moon_bigtts"
  },
  "zh_female_linzhiling_mars_bigtts": {
    "default1": "zh_female_linzhiling_mars_bigtts"
  },
  "zh_male_jingqiangkanye_emo_mars_bigtts": {
    "default1": "zh_male_jingqiangkanye_emo_mars_bigtts"
  },
  "id-ID-GadisNeural": {
    "default1": "id-ID-GadisNeural"
  },
  "ms-MY-YasminNeural": {
    "default1": "ms-MY-YasminNeural"
  },
  "ttv-voice-2025120918423925-Ktg6miPT": {
    "default1": "ttv-voice-2025120918423925-Ktg6miPT"
  },
  "Chinese (Mandarin)_Gentle_Senior": {
    "default1": "Chinese (Mandarin)_Gentle_Senior"
  },
  "Chinese (Mandarin)_Wise_Women": {
    "default1": "Chinese (Mandarin)_Wise_Women"
  },
  "ttv-voice-2026012217272626-tk9kPJcw": {
    "apikey": "ttv-voice-2026012217272626-tk9kPJcw"
  },
  "ttv-voice-2026012217272626-tk9kPJcw-26": {
    "apikey": "ttv-voice-2026012217272626-tk9kPJcw"
  },
  "male-qn-qingse": {
    "default1": "male-qn-qingse"
  },
  "male-qn-jingying": {
    "default1": "male-qn-jingying"
  },
  "male-qn-badao": {
    "default1": "male-qn-badao"
  },
  "male-qn-daxuesheng": {
    "default1": "male-qn-daxuesheng"
  },
  "female-shaonv": {
    "default1": "female-shaonv"
  },
  "female-yujie": {
    "default1": "female-yujie"
  },
  "female-chengshu": {
    "default1": "female-chengshu"
  },
  "female-tianmei": {
    "default1": "female-tianmei"
  },
  "male-qn-qingse-jingpin": {
    "default1": "male-qn-qingse-jingpin"
  },
  "male-qn-jingying-jingpin": {
    "default1": "male-qn-jingying-jingpin"
  },
  "male-qn-badao-jingpin": {
    "default1": "male-qn-badao-jingpin"
  },
  "male-qn-daxuesheng-jingpin": {
    "default1": "male-qn-daxuesheng-jingpin"
  },
  "female-shaonv-jingpin": {
    "default1": "female-shaonv-jingpin"
  },
  "female-yujie-jingpin": {
    "default1": "female-yujie-jingpin"
  },
  "female-chengshu-jingpin": {
    "default1": "female-chengshu-jingpin"
  },
  "female-tianmei-jingpin": {
    "default1": "female-tianmei-jingpin"
  },
  "clever_boy": {
    "default1": "clever_boy"
  },
  "cute_boy": {
    "default1": "cute_boy"
  },
  "lovely_girl": {
    "default1": "lovely_girl"
  },
  "cartoon_pig": {
    "default1": "cartoon_pig"
  },
  "bingjiao_didi": {
    "default1": "bingjiao_didi"
  },
  "junlang_nanyou": {
    "default1": "junlang_nanyou"
  },
  "chunzhen_xuedi": {
    "default1": "chunzhen_xuedi"
  },
  "lengdan_xiongzhang": {
    "default1": "lengdan_xiongzhang"
  },
  "badao_shaoye": {
    "default1": "badao_shaoye"
  },
  "tianxin_xiaoling": {
    "default1": "tianxin_xiaoling"
  },
  "qiaopi_mengmei": {
    "default1": "qiaopi_mengmei"
  },
  "wumei_yujie": {
    "default1": "wumei_yujie"
  },
  "diadia_xuemei": {
    "default1": "diadia_xuemei"
  },
  "danya_xuejie": {
    "default1": "danya_xuejie"
  },
  "Chinese (Mandarin)_Reliable_Executive": {
    "default1": "Chinese (Mandarin)_Reliable_Executive"
  },
  "Chinese (Mandarin)_News_Anchor": {
    "default1": "Chinese (Mandarin)_News_Anchor"
  },
  "Chinese (Mandarin)_Mature_Woman": {
    "default1": "Chinese (Mandarin)_Mature_Woman"
  },
  "Chinese (Mandarin)_Unrestrained_Young_Man": {
    "default1": "Chinese (Mandarin)_Unrestrained_Young_Man"
  },
  "Arrogant_Miss": {
    "default1": "Arrogant_Miss"
  },
  "Robot_Armor": {
    "default1": "Robot_Armor"
  },
  "Chinese (Mandarin)_Kind-hearted_Antie": {
    "default1": "Chinese (Mandarin)_Kind-hearted_Antie"
  },
  "Chinese (Mandarin)_HK_Flight_Attendant": {
    "default1": "Chinese (Mandarin)_HK_Flight_Attendant"
  },
  "Chinese (Mandarin)_Humorous_Elder": {
    "default1": "Chinese (Mandarin)_Humorous_Elder"
  },
  "Chinese (Mandarin)_Gentleman": {
    "default1": "Chinese (Mandarin)_Gentleman"
  },
  "Chinese (Mandarin)_Warm_Bestie": {
    "default1": "Chinese (Mandarin)_Warm_Bestie"
  },
  "Chinese (Mandarin)_Male_Announcer": {
    "default1": "Chinese (Mandarin)_Male_Announcer"
  },
  "Chinese (Mandarin)_Sweet_Lady": {
    "default1": "Chinese (Mandarin)_Sweet_Lady"
  },
  "Chinese (Mandarin)_Southern_Young_Man": {
    "default1": "Chinese (Mandarin)_Southern_Young_Man"
  },
  "Chinese (Mandarin)_Gentle_Youth": {
    "default1": "Chinese (Mandarin)_Gentle_Youth"
  },
  "Chinese (Mandarin)_Warm_Girl": {
    "default1": "Chinese (Mandarin)_Warm_Girl"
  },
  "Chinese (Mandarin)_Kind-hearted_Elder": {
    "default1": "Chinese (Mandarin)_Kind-hearted_Elder"
  },
  "Chinese (Mandarin)_Cute_Spirit": {
    "default1": "Chinese (Mandarin)_Cute_Spirit"
  },
  "Chinese (Mandarin)_Radio_Host": {
    "default1": "Chinese (Mandarin)_Radio_Host"
  },
  "Chinese (Mandarin)_Lyrical_Voice": {
    "default1": "Chinese (Mandarin)_Lyrical_Voice"
  },
  "Chinese (Mandarin)_Straightforward_Boy": {
    "default1": "Chinese (Mandarin)_Straightforward_Boy"
  },
  "Chinese (Mandarin)_Sincere_Adult": {
    "default1": "Chinese (Mandarin)_Sincere_Adult"
  },
  "Chinese (Mandarin)_Stubborn_Friend": {
    "default1": "Chinese (Mandarin)_Stubborn_Friend"
  },
  "Chinese (Mandarin)_Crisp_Girl": {
    "default1": "Chinese (Mandarin)_Crisp_Girl"
  },
  "Chinese (Mandarin)_Pure-hearted_Boy": {
    "default1": "Chinese (Mandarin)_Pure-hearted_Boy"
  },
  "Chinese (Mandarin)_Soft_Girl": {
    "default1": "Chinese (Mandarin)_Soft_Girl"
  },
  "Cantonese_ProfessionalHost（F)": {
    "default1": "Cantonese_ProfessionalHost（F)"
  },
  "Cantonese_GentleLady": {
    "default1": "Cantonese_GentleLady"
  },
  "Cantonese_ProfessionalHost（M)": {
    "default1": "Cantonese_ProfessionalHost（M)"
  },
  "Cantonese_PlayfulMan": {
    "default1": "Cantonese_PlayfulMan"
  },
  "Cantonese_CuteGirl": {
    "default1": "Cantonese_CuteGirl"
  },
  "Cantonese_KindWoman": {
    "default1": "Cantonese_KindWoman"
  },
  "Santa_Claus ": {
    "default1": "Santa_Claus "
  },
  "Grinch": {
    "default1": "Grinch"
  },
  "Rudolph": {
    "default1": "Rudolph"
  },
  "Arnold": {
    "default1": "Arnold"
  },
  "Charming_Santa": {
    "default1": "Charming_Santa"
  },
  "Charming_Lady": {
    "default1": "Charming_Lady"
  },
  "Sweet_Girl": {
    "default1": "Sweet_Girl"
  },
  "Cute_Elf": {
    "default1": "Cute_Elf"
  },
  "Attractive_Girl": {
    "default1": "Attractive_Girl"
  },
  "Serene_Woman": {
    "default1": "Serene_Woman"
  },
  "English_Trustworthy_Man": {
    "default1": "English_Trustworthy_Man"
  },
  "English_Graceful_Lady": {
    "default1": "English_Graceful_Lady"
  },
  "English_Aussie_Bloke": {
    "default1": "English_Aussie_Bloke"
  },
  "English_Whispering_girl": {
    "default1": "English_Whispering_girl"
  },
  "English_Diligent_Man": {
    "default1": "English_Diligent_Man"
  },
  "English_Gentle-voiced_man": {
    "default1": "English_Gentle-voiced_man"
  },
  "Japanese_IntellectualSenior": {
    "default1": "Japanese_IntellectualSenior"
  },
  "Japanese_DecisivePrincess": {
    "default1": "Japanese_DecisivePrincess"
  },
  "Japanese_LoyalKnight": {
    "default1": "Japanese_LoyalKnight"
  },
  "Japanese_DominantMan": {
    "default1": "Japanese_DominantMan"
  },
  "Japanese_SeriousCommander": {
    "default1": "Japanese_SeriousCommander"
  },
  "Japanese_ColdQueen": {
    "default1": "Japanese_ColdQueen"
  },
  "Japanese_DependableWoman": {
    "default1": "Japanese_DependableWoman"
  },
  "Japanese_GentleButler": {
    "default1": "Japanese_GentleButler"
  },
  "Japanese_KindLady": {
    "default1": "Japanese_KindLady"
  },
  "Japanese_CalmLady": {
    "default1": "Japanese_CalmLady"
  },
  "Japanese_OptimisticYouth": {
    "default1": "Japanese_OptimisticYouth"
  },
  "Japanese_GenerousIzakayaOwner": {
    "default1": "Japanese_GenerousIzakayaOwner"
  },
  "Japanese_SportyStudent": {
    "default1": "Japanese_SportyStudent"
  },
  "Japanese_InnocentBoy": {
    "default1": "Japanese_InnocentBoy"
  },
  "Japanese_GracefulMaiden": {
    "default1": "Japanese_GracefulMaiden"
  },
  "Korean_SweetGirl": {
    "default1": "Korean_SweetGirl"
  },
  "Korean_CheerfulBoyfriend": {
    "default1": "Korean_CheerfulBoyfriend"
  },
  "Korean_EnchantingSister": {
    "default1": "Korean_EnchantingSister"
  },
  "Korean_ShyGirl": {
    "default1": "Korean_ShyGirl"
  },
  "Korean_ReliableSister": {
    "default1": "Korean_ReliableSister"
  },
  "Korean_StrictBoss": {
    "default1": "Korean_StrictBoss"
  },
  "Korean_SassyGirl": {
    "default1": "Korean_SassyGirl"
  },
  "Korean_ChildhoodFriendGirl": {
    "default1": "Korean_ChildhoodFriendGirl"
  },
  "Korean_PlayboyCharmer": {
    "default1": "Korean_PlayboyCharmer"
  },
  "Korean_ElegantPrincess": {
    "default1": "Korean_ElegantPrincess"
  },
  "Korean_BraveFemaleWarrior": {
    "default1": "Korean_BraveFemaleWarrior"
  },
  "Korean_BraveYouth": {
    "default1": "Korean_BraveYouth"
  },
  "Korean_CalmLady": {
    "default1": "Korean_CalmLady"
  },
  "Korean_EnthusiasticTeen": {
    "default1": "Korean_EnthusiasticTeen"
  },
  "Korean_SoothingLady": {
    "default1": "Korean_SoothingLady"
  },
  "Korean_IntellectualSenior": {
    "default1": "Korean_IntellectualSenior"
  },
  "Korean_LonelyWarrior": {
    "default1": "Korean_LonelyWarrior"
  },
  "Korean_MatureLady": {
    "default1": "Korean_MatureLady"
  },
  "Korean_InnocentBoy": {
    "default1": "Korean_InnocentBoy"
  },
  "Korean_CharmingSister": {
    "default1": "Korean_CharmingSister"
  },
  "Korean_AthleticStudent": {
    "default1": "Korean_AthleticStudent"
  },
  "Korean_BraveAdventurer": {
    "default1": "Korean_BraveAdventurer"
  },
  "Korean_CalmGentleman": {
    "default1": "Korean_CalmGentleman"
  },
  "Korean_WiseElf": {
    "default1": "Korean_WiseElf"
  },
  "Korean_CheerfulCoolJunior": {
    "default1": "Korean_CheerfulCoolJunior"
  },
  "Korean_DecisiveQueen": {
    "default1": "Korean_DecisiveQueen"
  },
  "Korean_ColdYoungMan": {
    "default1": "Korean_ColdYoungMan"
  },
  "Korean_MysteriousGirl": {
    "default1": "Korean_MysteriousGirl"
  },
  "Korean_QuirkyGirl": {
    "default1": "Korean_QuirkyGirl"
  },
  "Korean_ConsiderateSenior": {
    "default1": "Korean_ConsiderateSenior"
  },
  "Korean_CheerfulLittleSister": {
    "default1": "Korean_CheerfulLittleSister"
  },
  "Korean_DominantMan": {
    "default1": "Korean_DominantMan"
  },
  "Korean_AirheadedGirl": {
    "default1": "Korean_AirheadedGirl"
  },
  "Korean_ReliableYouth": {
    "default1": "Korean_ReliableYouth"
  },
  "Korean_FriendlyBigSister": {
    "default1": "Korean_FriendlyBigSister"
  },
  "Korean_GentleBoss": {
    "default1": "Korean_GentleBoss"
  },
  "Korean_ColdGirl": {
    "default1": "Korean_ColdGirl"
  },
  "Korean_HaughtyLady": {
    "default1": "Korean_HaughtyLady"
  },
  "Korean_CharmingElderSister": {
    "default1": "Korean_CharmingElderSister"
  },
  "Korean_IntellectualMan": {
    "default1": "Korean_IntellectualMan"
  },
  "Korean_CaringWoman": {
    "default1": "Korean_CaringWoman"
  },
  "Korean_WiseTeacher": {
    "default1": "Korean_WiseTeacher"
  },
  "Korean_ConfidentBoss": {
    "default1": "Korean_ConfidentBoss"
  },
  "Korean_AthleticGirl": {
    "default1": "Korean_AthleticGirl"
  },
  "Korean_PossessiveMan": {
    "default1": "Korean_PossessiveMan"
  },
  "Korean_GentleWoman": {
    "default1": "Korean_GentleWoman"
  },
  "Korean_CockyGuy": {
    "default1": "Korean_CockyGuy"
  },
  "Korean_ThoughtfulWoman": {
    "default1": "Korean_ThoughtfulWoman"
  },
  "Korean_OptimisticYouth": {
    "default1": "Korean_OptimisticYouth"
  },
  "Spanish_SereneWoman": {
    "default1": "Spanish_SereneWoman"
  },
  "Spanish_MaturePartner": {
    "default1": "Spanish_MaturePartner"
  },
  "Spanish_CaptivatingStoryteller": {
    "default1": "Spanish_CaptivatingStoryteller"
  },
  "Spanish_Narrator": {
    "default1": "Spanish_Narrator"
  },
  "Spanish_WiseScholar": {
    "default1": "Spanish_WiseScholar"
  },
  "Spanish_Kind-heartedGirl": {
    "default1": "Spanish_Kind-heartedGirl"
  },
  "Spanish_DeterminedManager": {
    "default1": "Spanish_DeterminedManager"
  },
  "Spanish_BossyLeader": {
    "default1": "Spanish_BossyLeader"
  },
  "Spanish_ReservedYoungMan": {
    "default1": "Spanish_ReservedYoungMan"
  },
  "Spanish_ConfidentWoman": {
    "default1": "Spanish_ConfidentWoman"
  },
  "Spanish_ThoughtfulMan": {
    "default1": "Spanish_ThoughtfulMan"
  },
  "Spanish_Strong-WilledBoy": {
    "default1": "Spanish_Strong-WilledBoy"
  },
  "Spanish_SophisticatedLady": {
    "default1": "Spanish_SophisticatedLady"
  },
  "Spanish_RationalMan": {
    "default1": "Spanish_RationalMan"
  },
  "Spanish_AnimeCharacter": {
    "default1": "Spanish_AnimeCharacter"
  },
  "Spanish_Deep-tonedMan": {
    "default1": "Spanish_Deep-tonedMan"
  },
  "Spanish_Fussyhostess": {
    "default1": "Spanish_Fussyhostess"
  },
  "Spanish_SincereTeen": {
    "default1": "Spanish_SincereTeen"
  },
  "Spanish_FrankLady": {
    "default1": "Spanish_FrankLady"
  },
  "Spanish_Comedian": {
    "default1": "Spanish_Comedian"
  },
  "Spanish_Debator": {
    "default1": "Spanish_Debator"
  },
  "Spanish_ToughBoss": {
    "default1": "Spanish_ToughBoss"
  },
  "Spanish_Wiselady": {
    "default1": "Spanish_Wiselady"
  },
  "Spanish_Steadymentor": {
    "default1": "Spanish_Steadymentor"
  },
  "Spanish_Jovialman": {
    "default1": "Spanish_Jovialman"
  },
  "Spanish_SantaClaus": {
    "default1": "Spanish_SantaClaus"
  },
  "Spanish_Rudolph": {
    "default1": "Spanish_Rudolph"
  },
  "Spanish_Intonategirl": {
    "default1": "Spanish_Intonategirl"
  },
  "Spanish_Arnold": {
    "default1": "Spanish_Arnold"
  },
  "Spanish_Ghost": {
    "default1": "Spanish_Ghost"
  },
  "Spanish_HumorousElder": {
    "default1": "Spanish_HumorousElder"
  },
  "Spanish_EnergeticBoy": {
    "default1": "Spanish_EnergeticBoy"
  },
  "Spanish_WhimsicalGirl": {
    "default1": "Spanish_WhimsicalGirl"
  },
  "Spanish_StrictBoss": {
    "default1": "Spanish_StrictBoss"
  },
  "Spanish_ReliableMan": {
    "default1": "Spanish_ReliableMan"
  },
  "Spanish_SereneElder": {
    "default1": "Spanish_SereneElder"
  },
  "Spanish_AngryMan": {
    "default1": "Spanish_AngryMan"
  },
  "Spanish_AssertiveQueen": {
    "default1": "Spanish_AssertiveQueen"
  },
  "Spanish_CaringGirlfriend": {
    "default1": "Spanish_CaringGirlfriend"
  },
  "Spanish_PowerfulSoldier": {
    "default1": "Spanish_PowerfulSoldier"
  },
  "Spanish_PassionateWarrior": {
    "default1": "Spanish_PassionateWarrior"
  },
  "Spanish_ChattyGirl": {
    "default1": "Spanish_ChattyGirl"
  },
  "Spanish_RomanticHusband": {
    "default1": "Spanish_RomanticHusband"
  },
  "Spanish_CompellingGirl": {
    "default1": "Spanish_CompellingGirl"
  },
  "Spanish_PowerfulVeteran": {
    "default1": "Spanish_PowerfulVeteran"
  },
  "Spanish_SensibleManager": {
    "default1": "Spanish_SensibleManager"
  },
  "Spanish_ThoughtfulLady": {
    "default1": "Spanish_ThoughtfulLady"
  },
  "Portuguese_SentimentalLady": {
    "default1": "Portuguese_SentimentalLady"
  },
  "Portuguese_BossyLeader": {
    "default1": "Portuguese_BossyLeader"
  },
  "Portuguese_Wiselady": {
    "default1": "Portuguese_Wiselady"
  },
  "Portuguese_Strong-WilledBoy": {
    "default1": "Portuguese_Strong-WilledBoy"
  },
  "Portuguese_Deep-VoicedGentleman": {
    "default1": "Portuguese_Deep-VoicedGentleman"
  },
  "Portuguese_UpsetGirl": {
    "default1": "Portuguese_UpsetGirl"
  },
  "Portuguese_PassionateWarrior": {
    "default1": "Portuguese_PassionateWarrior"
  },
  "Portuguese_AnimeCharacter": {
    "default1": "Portuguese_AnimeCharacter"
  },
  "Portuguese_ConfidentWoman": {
    "default1": "Portuguese_ConfidentWoman"
  },
  "Portuguese_AngryMan": {
    "default1": "Portuguese_AngryMan"
  },
  "Portuguese_CaptivatingStoryteller": {
    "default1": "Portuguese_CaptivatingStoryteller"
  },
  "Portuguese_Godfather": {
    "default1": "Portuguese_Godfather"
  },
  "Portuguese_ReservedYoungMan": {
    "default1": "Portuguese_ReservedYoungMan"
  },
  "Portuguese_SmartYoungGirl": {
    "default1": "Portuguese_SmartYoungGirl"
  },
  "Portuguese_Kind-heartedGirl": {
    "default1": "Portuguese_Kind-heartedGirl"
  },
  "Portuguese_Pompouslady": {
    "default1": "Portuguese_Pompouslady"
  },
  "Portuguese_Grinch": {
    "default1": "Portuguese_Grinch"
  },
  "Portuguese_Debator": {
    "default1": "Portuguese_Debator"
  },
  "Portuguese_SweetGirl": {
    "default1": "Portuguese_SweetGirl"
  },
  "Portuguese_AttractiveGirl": {
    "default1": "Portuguese_AttractiveGirl"
  },
  "Portuguese_ThoughtfulMan": {
    "default1": "Portuguese_ThoughtfulMan"
  },
  "Portuguese_PlayfulGirl": {
    "default1": "Portuguese_PlayfulGirl"
  },
  "Portuguese_GorgeousLady": {
    "default1": "Portuguese_GorgeousLady"
  },
  "Portuguese_LovelyLady": {
    "default1": "Portuguese_LovelyLady"
  },
  "Portuguese_SereneWoman": {
    "default1": "Portuguese_SereneWoman"
  },
  "Portuguese_SadTeen": {
    "default1": "Portuguese_SadTeen"
  },
  "Portuguese_MaturePartner": {
    "default1": "Portuguese_MaturePartner"
  },
  "Portuguese_Comedian": {
    "default1": "Portuguese_Comedian"
  },
  "Portuguese_NaughtySchoolgirl": {
    "default1": "Portuguese_NaughtySchoolgirl"
  },
  "Portuguese_Narrator": {
    "default1": "Portuguese_Narrator"
  },
  "Portuguese_ToughBoss": {
    "default1": "Portuguese_ToughBoss"
  },
  "Portuguese_Fussyhostess": {
    "default1": "Portuguese_Fussyhostess"
  },
  "Portuguese_Dramatist": {
    "default1": "Portuguese_Dramatist"
  },
  "Portuguese_Steadymentor": {
    "default1": "Portuguese_Steadymentor"
  },
  "Portuguese_Jovialman": {
    "default1": "Portuguese_Jovialman"
  },
  "Portuguese_CharmingQueen": {
    "default1": "Portuguese_CharmingQueen"
  },
  "Portuguese_SantaClaus": {
    "default1": "Portuguese_SantaClaus"
  },
  "Portuguese_Rudolph": {
    "default1": "Portuguese_Rudolph"
  },
  "Portuguese_Arnold": {
    "default1": "Portuguese_Arnold"
  },
  "Portuguese_CharmingSanta": {
    "default1": "Portuguese_CharmingSanta"
  },
  "Portuguese_CharmingLady": {
    "default1": "Portuguese_CharmingLady"
  },
  "Portuguese_Ghost": {
    "default1": "Portuguese_Ghost"
  },
  "Portuguese_HumorousElder": {
    "default1": "Portuguese_HumorousElder"
  },
  "Portuguese_CalmLeader": {
    "default1": "Portuguese_CalmLeader"
  },
  "Portuguese_GentleTeacher": {
    "default1": "Portuguese_GentleTeacher"
  },
  "Portuguese_EnergeticBoy": {
    "default1": "Portuguese_EnergeticBoy"
  },
  "Portuguese_ReliableMan": {
    "default1": "Portuguese_ReliableMan"
  },
  "Portuguese_SereneElder": {
    "default1": "Portuguese_SereneElder"
  },
  "Portuguese_GrimReaper": {
    "default1": "Portuguese_GrimReaper"
  },
  "Portuguese_AssertiveQueen": {
    "default1": "Portuguese_AssertiveQueen"
  },
  "Portuguese_WhimsicalGirl": {
    "default1": "Portuguese_WhimsicalGirl"
  },
  "Portuguese_StressedLady": {
    "default1": "Portuguese_StressedLady"
  },
  "Portuguese_FriendlyNeighbor": {
    "default1": "Portuguese_FriendlyNeighbor"
  },
  "Portuguese_CaringGirlfriend": {
    "default1": "Portuguese_CaringGirlfriend"
  },
  "Portuguese_PowerfulSoldier": {
    "default1": "Portuguese_PowerfulSoldier"
  },
  "Portuguese_FascinatingBoy": {
    "default1": "Portuguese_FascinatingBoy"
  },
  "Portuguese_RomanticHusband": {
    "default1": "Portuguese_RomanticHusband"
  },
  "Portuguese_StrictBoss": {
    "default1": "Portuguese_StrictBoss"
  },
  "Portuguese_InspiringLady": {
    "default1": "Portuguese_InspiringLady"
  },
  "Portuguese_PlayfulSpirit": {
    "default1": "Portuguese_PlayfulSpirit"
  },
  "Portuguese_ElegantGirl": {
    "default1": "Portuguese_ElegantGirl"
  },
  "Portuguese_CompellingGirl": {
    "default1": "Portuguese_CompellingGirl"
  },
  "Portuguese_PowerfulVeteran": {
    "default1": "Portuguese_PowerfulVeteran"
  },
  "Portuguese_SensibleManager": {
    "default1": "Portuguese_SensibleManager"
  },
  "Portuguese_ThoughtfulLady": {
    "default1": "Portuguese_ThoughtfulLady"
  },
  "Portuguese_TheatricalActor": {
    "default1": "Portuguese_TheatricalActor"
  },
  "Portuguese_FragileBoy": {
    "default1": "Portuguese_FragileBoy"
  },
  "Portuguese_ChattyGirl": {
    "default1": "Portuguese_ChattyGirl"
  },
  "Portuguese_Conscientiousinstructor": {
    "default1": "Portuguese_Conscientiousinstructor"
  },
  "Portuguese_RationalMan": {
    "default1": "Portuguese_RationalMan"
  },
  "Portuguese_WiseScholar": {
    "default1": "Portuguese_WiseScholar"
  },
  "Portuguese_FrankLady": {
    "default1": "Portuguese_FrankLady"
  },
  "Portuguese_DeterminedManager": {
    "default1": "Portuguese_DeterminedManager"
  },
  "French_Male_Speech_New": {
    "default1": "French_Male_Speech_New"
  },
  "French_Female_News Anchor": {
    "default1": "French_Female_News Anchor"
  },
  "French_CasualMan": {
    "default1": "French_CasualMan"
  },
  "French_MovieLeadFemale": {
    "default1": "French_MovieLeadFemale"
  },
  "French_FemaleAnchor": {
    "default1": "French_FemaleAnchor"
  },
  "French_MaleNarrator": {
    "default1": "French_MaleNarrator"
  },
  "Indonesian_SweetGirl": {
    "default1": "Indonesian_SweetGirl"
  },
  "Indonesian_ReservedYoungMan": {
    "default1": "Indonesian_ReservedYoungMan"
  },
  "Indonesian_CharmingGirl": {
    "default1": "Indonesian_CharmingGirl"
  },
  "Indonesian_CalmWoman": {
    "default1": "Indonesian_CalmWoman"
  },
  "Indonesian_ConfidentWoman": {
    "default1": "Indonesian_ConfidentWoman"
  },
  "Indonesian_CaringMan": {
    "default1": "Indonesian_CaringMan"
  },
  "Indonesian_BossyLeader": {
    "default1": "Indonesian_BossyLeader"
  },
  "Indonesian_DeterminedBoy": {
    "default1": "Indonesian_DeterminedBoy"
  },
  "Indonesian_GentleGirl": {
    "default1": "Indonesian_GentleGirl"
  },
  "German_FriendlyMan": {
    "default1": "German_FriendlyMan"
  },
  "German_SweetLady": {
    "default1": "German_SweetLady"
  },
  "German_PlayfulMan": {
    "default1": "German_PlayfulMan"
  },
  "Russian_HandsomeChildhoodFriend": {
    "default1": "Russian_HandsomeChildhoodFriend"
  },
  "Russian_BrightHeroine": {
    "default1": "Russian_BrightHeroine"
  },
  "Russian_AmbitiousWoman": {
    "default1": "Russian_AmbitiousWoman"
  },
  "Russian_ReliableMan": {
    "default1": "Russian_ReliableMan"
  },
  "Russian_CrazyQueen": {
    "default1": "Russian_CrazyQueen"
  },
  "Russian_PessimisticGirl": {
    "default1": "Russian_PessimisticGirl"
  },
  "Russian_AttractiveGuy": {
    "default1": "Russian_AttractiveGuy"
  },
  "Russian_Bad-temperedBoy": {
    "default1": "Russian_Bad-temperedBoy"
  },
  "Italian_BraveHeroine": {
    "default1": "Italian_BraveHeroine"
  },
  "Italian_Narrator": {
    "default1": "Italian_Narrator"
  },
  "Italian_WanderingSorcerer": {
    "default1": "Italian_WanderingSorcerer"
  },
  "Italian_DiligentLeader": {
    "default1": "Italian_DiligentLeader"
  },
  "Arabic_CalmWoman": {
    "default1": "Arabic_CalmWoman"
  },
  "Arabic_FriendlyGuy": {
    "default1": "Arabic_FriendlyGuy"
  },
  "Turkish_CalmWoman": {
    "default1": "Turkish_CalmWoman"
  },
  "Turkish_Trustworthyman": {
    "default1": "Turkish_Trustworthyman"
  },
  "Ukrainian_CalmWoman": {
    "default1": "Ukrainian_CalmWoman"
  },
  "Ukrainian_WiseScholar": {
    "default1": "Ukrainian_WiseScholar"
  },
  "Dutch_kindhearted_girl": {
    "default1": "Dutch_kindhearted_girl"
  },
  "Dutch_bossy_leader": {
    "default1": "Dutch_bossy_leader"
  },
  "Vietnamese_kindhearted_girl": {
    "default1": "Vietnamese_kindhearted_girl"
  },
  "Thai_male_1_sample8": {
    "default1": "Thai_male_1_sample8"
  },
  "Thai_male_2_sample2": {
    "default1": "Thai_male_2_sample2"
  },
  "Thai_female_1_sample1": {
    "default1": "Thai_female_1_sample1"
  },
  "Thai_female_2_sample2": {
    "default1": "Thai_female_2_sample2"
  },
  "Polish_male_1_sample4": {
    "default1": "Polish_male_1_sample4"
  },
  "Polish_male_2_sample3": {
    "default1": "Polish_male_2_sample3"
  },
  "Polish_female_1_sample1": {
    "default1": "Polish_female_1_sample1"
  },
  "Polish_female_2_sample3": {
    "default1": "Polish_female_2_sample3"
  },
  "Romanian_male_1_sample2": {
    "default1": "Romanian_male_1_sample2"
  },
  "Romanian_male_2_sample1": {
    "default1": "Romanian_male_2_sample1"
  },
  "Romanian_female_1_sample4": {
    "default1": "Romanian_female_1_sample4"
  },
  "Romanian_female_2_sample1": {
    "default1": "Romanian_female_2_sample1"
  },
  "greek_male_1a_v1": {
    "default1": "greek_male_1a_v1"
  },
  "Greek_female_1_sample1": {
    "default1": "Greek_female_1_sample1"
  },
  "Greek_female_2_sample3": {
    "default1": "Greek_female_2_sample3"
  },
  "czech_male_1_v1": {
    "default1": "czech_male_1_v1"
  },
  "czech_female_5_v7": {
    "default1": "czech_female_5_v7"
  },
  "czech_female_2_v2": {
    "default1": "czech_female_2_v2"
  },
  "finnish_male_3_v1": {
    "default1": "finnish_male_3_v1"
  },
  "finnish_male_1_v2": {
    "default1": "finnish_male_1_v2"
  },
  "finnish_female_4_v1": {
    "default1": "finnish_female_4_v1"
  },
  "hindi_male_1_v2": {
    "default1": "hindi_male_1_v2"
  },
  "hindi_female_2_v1": {
    "default1": "hindi_female_2_v1"
  },
  "hindi_female_1_v2": {
    "default1": "hindi_female_1_v2"
  }
}"#
    .to_string()
}

/// 初始化日志系统
fn init_logging() {
    use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

    // 生产环境建议使用 info 级别以减少性能开销
    let log_level = env_string_or_default("RUST_LOG", "info");
    let enable_console = env_string_or_default("TOKIO_CONSOLE", "true");

    // 创建环境过滤器，确保 maxminddb::decoder 的 debug 日志被静音
    // 策略：手动处理环境变量，移除任何 maxminddb::decoder 相关的设置，然后添加 maxminddb::decoder=warn
    let env_filter = if let Ok(env_log) = std::env::var("RUST_LOG") {
        // 移除环境变量中可能存在的 maxminddb::decoder 相关设置
        let cleaned_parts: Vec<&str> = env_log
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && !s.starts_with("maxminddb::decoder"))
            .collect();

        // 构建新的过滤器字符串，确保 maxminddb::decoder=warn 在最后（优先级最高）
        let filter_str = if cleaned_parts.is_empty() {
            format!("{},maxminddb::decoder=warn", log_level)
        } else {
            format!("{},maxminddb::decoder=warn", cleaned_parts.join(","))
        };

        EnvFilter::new(&filter_str)
    } else {
        // 如果环境变量不存在，使用默认级别并添加指令
        EnvFilter::new(format!("{},maxminddb::decoder=warn", log_level))
    };

    // 设置环境变量供其他代码使用
    // 注意：set_var 修改全局状态，在多线程环境下可能存在竞态
    // 但在 main 初始化阶段（异步 runtime 启动前）使用是安全的
    // unsafe 是必要的，因为 std::env::set_var 不是线程安全的
    unsafe {
        std::env::set_var("RUST_LOG", &log_level);
    }

    if enable_console.to_lowercase() == "true" {
        // 获取tokio-console端口配置
        let console_port = env_string_or_default("TOKIO_CONSOLE_PORT", "16992");
        let console_bind = env_string_or_default("TOKIO_CONSOLE_BIND", &format!("0.0.0.0:{}", console_port));

        println!("🔍 启用tokio-console支持 (绑定: {}) + 标准日志输出", console_bind);

        // 解析地址
        let server_addr: std::net::SocketAddr = console_bind.parse().unwrap_or_else(|_| {
            println!(
                "⚠️ 无效的TOKIO_CONSOLE_BIND地址: {}，使用默认地址 0.0.0.0:{}",
                console_bind, console_port
            );
            format!("0.0.0.0:{}", console_port).parse().expect("默认地址解析失败")
        });

        // 创建console-subscriber layer
        let console_layer = console_subscriber::ConsoleLayer::builder()
            .retention(Duration::from_secs(60))
            .server_addr(server_addr)
            .spawn();

        // 创建fmt layer用于标准日志输出
        let is_debug = log_level.to_lowercase() == "debug" || log_level.to_lowercase() == "trace";
        let fmt_layer = if is_debug {
            fmt::layer()
                .with_target(false) // 不显示模块路径，只显示文件位置
                .with_thread_ids(true)
                .with_file(true)
                .with_line_number(true)
                .with_ansi(false)
        } else {
            // 生产环境：简化格式
            fmt::layer()
                .with_target(false)
                .with_thread_ids(false)
                .with_file(false)
                .with_line_number(false)
                .with_ansi(false)
        };

        // 同时应用过滤器和两个layer
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .with(console_layer)
            .init();

        println!(
            "✅ tokio-console已启动，请在另一个终端运行: tokio-console --connect {}",
            console_bind
        );
        println!("💡 或在浏览器打开: http://{}", console_bind);
    } else {
        println!("📝 使用标准tracing日志系统");

        // 根据日志级别决定是否显示详细信息（减少生产环境开销）
        let is_debug = log_level.to_lowercase() == "debug" || log_level.to_lowercase() == "trace";

        let fmt_layer = if is_debug {
            // 开发环境：显示详细信息，但不显示模块路径
            fmt::layer()
                .with_target(false) // 不显示模块路径，只显示文件位置
                .with_thread_ids(true)
                .with_file(true)
                .with_line_number(true)
                .with_ansi(false)
        } else {
            // 生产环境：简化格式以提高性能
            fmt::layer()
                .with_target(false)
                .with_thread_ids(false)
                .with_file(false)
                .with_line_number(false)
                .with_ansi(false)
        };

        tracing_subscriber::registry()
            .with(env_filter) // 全局过滤器
            .with(fmt_layer)
            .init();
    }
}
