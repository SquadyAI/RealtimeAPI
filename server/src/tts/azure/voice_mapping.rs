//! Azure TTS 语言到声音的映射
//!
//! 覆盖 139 种语言/地区变体

use std::collections::HashMap;
use std::sync::LazyLock;

/// 语言代码到 Azure 神经网络声音的映射
/// 使用每种语言最自然的默认声音
pub static AZURE_VOICE_MAP: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut m = HashMap::new();

    // 南非荷兰语
    m.insert("af-ZA", "af-ZA-AdriNeural");

    // 阿姆哈拉语
    m.insert("am-ET", "am-ET-MekdesNeural");

    // 阿拉伯语变体
    m.insert("ar-AE", "ar-AE-FatimaNeural");
    m.insert("ar-BH", "ar-BH-LailaNeural");
    m.insert("ar-DZ", "ar-DZ-AminaNeural");
    m.insert("ar-EG", "ar-EG-SalmaNeural");
    m.insert("ar-IL", "ar-IL-HilaNeural");
    m.insert("ar-IQ", "ar-IQ-RanaNeural");
    m.insert("ar-JO", "ar-JO-SanaNeural");
    m.insert("ar-KW", "ar-KW-NouraNeural");
    m.insert("ar-LB", "ar-LB-LaylaNeural");
    m.insert("ar-LY", "ar-LY-ImanNeural");
    m.insert("ar-MA", "ar-MA-MounaNeural");
    m.insert("ar-OM", "ar-OM-AyshaNeural");
    m.insert("ar-PS", "ar-PS-HibaNeural");
    m.insert("ar-QA", "ar-QA-AmalNeural");
    m.insert("ar-SA", "ar-SA-ZariyahNeural");
    m.insert("ar-SY", "ar-SY-AmanyNeural");
    m.insert("ar-TN", "ar-TN-ReemNeural");
    m.insert("ar-YE", "ar-YE-MaryamNeural");

    // 阿塞拜疆语
    m.insert("az-AZ", "az-AZ-BabekNeural");

    // 保加利亚语
    m.insert("bg-BG", "bg-BG-KalinaNeural");

    // 孟加拉语
    m.insert("bn-IN", "bn-IN-TanishaaNeural");

    // 波斯尼亚语
    m.insert("bs-BA", "bs-BA-VesnaNeural");

    // 加泰罗尼亚语
    m.insert("ca-ES", "ca-ES-JoanaNeural");

    // 捷克语
    m.insert("cs-CZ", "cs-CZ-VlastaNeural");

    // 威尔士语
    m.insert("cy-GB", "cy-GB-NiaNeural");

    // 丹麦语
    m.insert("da-DK", "da-DK-ChristelNeural");

    // 德语变体
    m.insert("de-AT", "de-AT-IngridNeural");
    m.insert("de-CH", "de-CH-LeniNeural");
    m.insert("de-DE", "de-DE-KatjaNeural");

    // 希腊语
    m.insert("el-GR", "el-GR-AthinaNeural");

    // 英语变体
    m.insert("en-AU", "en-AU-NatashaNeural");
    m.insert("en-CA", "en-CA-ClaraNeural");
    m.insert("en-GB", "en-GB-SoniaNeural");
    m.insert("en-GH", "en-GH-EsiNeural");
    m.insert("en-HK", "en-HK-YanNeural");
    m.insert("en-IE", "en-IE-EmilyNeural");
    m.insert("en-IN", "en-IN-NeerjaNeural");
    m.insert("en-KE", "en-KE-AsiliaNeural");
    m.insert("en-NG", "en-NG-EzinneNeural");
    m.insert("en-NZ", "en-NZ-MollyNeural");
    m.insert("en-PH", "en-PH-RosaNeural");
    m.insert("en-SG", "en-SG-LunaNeural");
    m.insert("en-TZ", "en-TZ-ImaniNeural");
    m.insert("en-US", "en-US-JennyNeural");
    m.insert("en-ZA", "en-ZA-LeahNeural");

    // 西班牙语变体
    m.insert("es-AR", "es-AR-ElenaNeural");
    m.insert("es-BO", "es-BO-SofiaNeural");
    m.insert("es-CL", "es-CL-CatalinaNeural");
    m.insert("es-CO", "es-CO-SalomeNeural");
    m.insert("es-CR", "es-CR-MariaNeural");
    m.insert("es-CU", "es-CU-BelkysNeural");
    m.insert("es-DO", "es-DO-RamonaNeural");
    m.insert("es-EC", "es-EC-AndreaNeural");
    m.insert("es-ES", "es-ES-ElviraNeural");
    m.insert("es-GQ", "es-GQ-TeresaNeural");
    m.insert("es-GT", "es-GT-MartaNeural");
    m.insert("es-HN", "es-HN-KarlaNeural");
    m.insert("es-MX", "es-MX-DaliaNeural");
    m.insert("es-NI", "es-NI-YolandaNeural");
    m.insert("es-PA", "es-PA-MargaritaNeural");
    m.insert("es-PE", "es-PE-CamilaNeural");
    m.insert("es-PR", "es-PR-KarinaNeural");
    m.insert("es-PY", "es-PY-TaniaNeural");
    m.insert("es-SV", "es-SV-LorenaNeural");
    m.insert("es-US", "es-US-PalomaNeural");
    m.insert("es-UY", "es-UY-ValentinaNeural");
    m.insert("es-VE", "es-VE-PaolaNeural");

    // 爱沙尼亚语
    m.insert("et-EE", "et-EE-AnuNeural");

    // 巴斯克语
    m.insert("eu-ES", "eu-ES-AinhoaNeural");

    // 波斯语
    m.insert("fa-IR", "fa-IR-DilaraNeural");

    // 芬兰语
    m.insert("fi-FI", "fi-FI-NooraNeural");

    // 菲律宾语
    m.insert("fil-PH", "fil-PH-BlessicaNeural");

    // 法语变体
    m.insert("fr-BE", "fr-BE-CharlineNeural");
    m.insert("fr-CA", "fr-CA-SylvieNeural");
    m.insert("fr-CH", "fr-CH-ArianeNeural");
    m.insert("fr-FR", "fr-FR-DeniseNeural");

    // 爱尔兰语
    m.insert("ga-IE", "ga-IE-OrlaNeural");

    // 加利西亚语
    m.insert("gl-ES", "gl-ES-SabelaNeural");

    // 古吉拉特语
    m.insert("gu-IN", "gu-IN-DhwaniNeural");

    // 希伯来语
    m.insert("he-IL", "he-IL-HilaNeural");

    // 印地语
    m.insert("hi-IN", "hi-IN-SwaraNeural");

    // 克罗地亚语
    m.insert("hr-HR", "hr-HR-GabrijelaNeural");

    // 匈牙利语
    m.insert("hu-HU", "hu-HU-NoemiNeural");

    // 亚美尼亚语
    m.insert("hy-AM", "hy-AM-AnahitNeural");

    // 印尼语
    m.insert("id-ID", "id-ID-GadisNeural");

    // 冰岛语
    m.insert("is-IS", "is-IS-GudrunNeural");

    // 意大利语变体
    m.insert("it-CH", "it-CH-IsabellaNeural");
    m.insert("it-IT", "it-IT-ElsaNeural");

    // 日语
    m.insert("ja-JP", "ja-JP-NanamiNeural");

    // 爪哇语
    m.insert("jv-ID", "jv-ID-SitiNeural");

    // 格鲁吉亚语
    m.insert("ka-GE", "ka-GE-EkaNeural");

    // 哈萨克语
    m.insert("kk-KZ", "kk-KZ-AigulNeural");

    // 高棉语
    m.insert("km-KH", "km-KH-SreymomNeural");

    // 卡纳达语
    m.insert("kn-IN", "kn-IN-SapnaNeural");

    // 韩语
    m.insert("ko-KR", "ko-KR-SunHiNeural");

    // 老挝语
    m.insert("lo-LA", "lo-LA-KeomanyNeural");

    // 立陶宛语
    m.insert("lt-LT", "lt-LT-OnaNeural");

    // 拉脱维亚语
    m.insert("lv-LV", "lv-LV-EveritaNeural");

    // 马其顿语
    m.insert("mk-MK", "mk-MK-MarijaNeural");

    // 马拉雅拉姆语
    m.insert("ml-IN", "ml-IN-SobhanaNeural");

    // 蒙古语
    m.insert("mn-MN", "mn-MN-YesunNeural");

    // 马拉地语
    m.insert("mr-IN", "mr-IN-AarohiNeural");

    // 马来语
    m.insert("ms-MY", "ms-MY-YasminNeural");

    // 马耳他语
    m.insert("mt-MT", "mt-MT-GraceNeural");

    // 缅甸语
    m.insert("my-MM", "my-MM-NilarNeural");

    // 挪威语
    m.insert("nb-NO", "nb-NO-PernilleNeural");

    // 尼泊尔语
    m.insert("ne-NP", "ne-NP-HemkalaNeural");

    // 荷兰语变体
    m.insert("nl-BE", "nl-BE-DenaNeural");
    m.insert("nl-NL", "nl-NL-ColetteNeural");

    // 旁遮普语
    m.insert("pa-IN", "pa-IN-HarpreetNeural");

    // 波兰语
    m.insert("pl-PL", "pl-PL-AgnieszkaNeural");

    // 普什图语
    m.insert("ps-AF", "ps-AF-LatifaNeural");

    // 葡萄牙语变体
    m.insert("pt-BR", "pt-BR-FranciscaNeural");
    m.insert("pt-PT", "pt-PT-RaquelNeural");

    // 罗马尼亚语
    m.insert("ro-RO", "ro-RO-AlinaNeural");

    // 俄语
    m.insert("ru-RU", "ru-RU-SvetlanaNeural");

    // 僧伽罗语
    m.insert("si-LK", "si-LK-ThiliniNeural");

    // 斯洛伐克语
    m.insert("sk-SK", "sk-SK-ViktoriaNeural");

    // 斯洛文尼亚语
    m.insert("sl-SI", "sl-SI-PetraNeural");

    // 索马里语
    m.insert("so-SO", "so-SO-UbaxNeural");

    // 阿尔巴尼亚语
    m.insert("sq-AL", "sq-AL-AnilaNeural");

    // 塞尔维亚语
    m.insert("sr-RS", "sr-RS-SophieNeural");

    // 瑞典语
    m.insert("sv-SE", "sv-SE-SofieNeural");

    // 斯瓦希里语变体
    m.insert("sw-KE", "sw-KE-ZuriNeural");
    m.insert("sw-TZ", "sw-TZ-RehemaNeural");

    // 泰米尔语
    m.insert("ta-IN", "ta-IN-PallaviNeural");

    // 泰卢固语
    m.insert("te-IN", "te-IN-ShrutiNeural");

    // 泰语
    m.insert("th-TH", "th-TH-PremwadeeNeural");

    // 土耳其语
    m.insert("tr-TR", "tr-TR-EmelNeural");

    // 乌克兰语
    m.insert("uk-UA", "uk-UA-PolinaNeural");

    // 乌尔都语
    m.insert("ur-IN", "ur-IN-GulNeural");

    // 乌兹别克语
    m.insert("uz-UZ", "uz-UZ-MadinaNeural");

    // 越南语
    m.insert("vi-VN", "vi-VN-HoaiMyNeural");

    // 中文变体
    m.insert("zh-CN", "zh-CN-XiaoxiaoNeural");
    m.insert("zh-HK", "zh-HK-HiuMaanNeural");
    m.insert("zh-TW", "zh-TW-HsiaoChenNeural");

    // 祖鲁语
    m.insert("zu-ZA", "zu-ZA-ThandoNeural");

    m
});

/// 根据语言代码获取 Azure 声音名称
pub fn get_voice_for_language(lang: &str) -> Option<&'static str> {
    // 先尝试精确匹配
    if let Some(voice) = AZURE_VOICE_MAP.get(lang) {
        return Some(*voice);
    }

    // 尝试不区分大小写匹配
    let lang_lower = lang.to_lowercase();
    for (key, voice) in AZURE_VOICE_MAP.iter() {
        if key.to_lowercase() == lang_lower {
            return Some(*voice);
        }
    }

    // 尝试只匹配语言部分（如 "zh" 匹配 "zh-CN"）
    let lang_prefix = lang.split('-').next().unwrap_or(lang);
    for (key, voice) in AZURE_VOICE_MAP.iter() {
        if key.starts_with(lang_prefix) {
            return Some(*voice);
        }
    }

    None
}

/// 检查语言是否被支持
pub fn is_language_supported(lang: &str) -> bool {
    get_voice_for_language(lang).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_voice_for_language() {
        assert_eq!(get_voice_for_language("zh-CN"), Some("zh-CN-XiaoxiaoNeural"));
        assert_eq!(get_voice_for_language("en-US"), Some("en-US-JennyNeural"));
        assert_eq!(get_voice_for_language("ja-JP"), Some("ja-JP-NanamiNeural"));
    }

    #[test]
    fn test_get_voice_fallback() {
        // 只匹配语言部分
        assert!(get_voice_for_language("zh").is_some());
        assert!(get_voice_for_language("en").is_some());
    }

    #[test]
    fn test_all_139_languages_covered() {
        // 验证所有 139 种语言都有映射
        assert!(AZURE_VOICE_MAP.len() >= 139);
    }
}
