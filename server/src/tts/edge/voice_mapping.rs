//! Edge TTS 语言到声音的映射
//!
//! 覆盖 lan.xlsx 中的全部 139 种语言
//! 使用女声作为默认声音

use once_cell::sync::Lazy;
use rustc_hash::FxHashMap;

/// 语言代码到 Edge TTS 女声 ID 的映射
pub static EDGE_TTS_VOICE_MAP: Lazy<FxHashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = FxHashMap::default();

    // ========== 中文 ==========
    m.insert("zh", "zh-CN-XiaoxiaoNeural");
    m.insert("zh-CN", "zh-CN-XiaoxiaoNeural");
    m.insert("zh-TW", "zh-TW-HsiaoChenNeural");
    m.insert("zh-HK", "zh-HK-HiuGaaiNeural");
    m.insert("yue", "yue-CN-XiaoMinNeural");
    m.insert("yue-CN", "yue-CN-XiaoMinNeural");

    // ========== 英文 ==========
    m.insert("en", "en-US-JennyNeural");
    m.insert("en-US", "en-US-JennyNeural");
    m.insert("en-GB", "en-GB-SoniaNeural");
    m.insert("en-AU", "en-AU-NatashaNeural");
    m.insert("en-CA", "en-CA-ClaraNeural");
    m.insert("en-HK", "en-HK-YanNeural");
    m.insert("en-IE", "en-IE-EmilyNeural");
    m.insert("en-IN", "en-IN-NeerjaNeural");
    m.insert("en-KE", "en-KE-AsiliaNeural");
    m.insert("en-NG", "en-NG-EzinneNeural");
    m.insert("en-NZ", "en-NZ-MollyNeural");
    m.insert("en-PH", "en-PH-RosaNeural");
    m.insert("en-SG", "en-SG-LunaNeural");
    m.insert("en-TZ", "en-TZ-ImaniNeural");
    m.insert("en-ZA", "en-ZA-LeahNeural");
    m.insert("en-GH", "en-NG-EzinneNeural"); // 降级: 加纳 → 尼日利亚

    // ========== 阿拉伯语 ==========
    m.insert("ar", "ar-SA-ZariyahNeural");
    m.insert("ar-AE", "ar-AE-FatimaNeural");
    m.insert("ar-BH", "ar-BH-LailaNeural");
    m.insert("ar-DZ", "ar-DZ-AminaNeural");
    m.insert("ar-EG", "ar-EG-SalmaNeural");
    m.insert("ar-IQ", "ar-IQ-RanaNeural");
    m.insert("ar-JO", "ar-JO-SanaNeural");
    m.insert("ar-KW", "ar-KW-NouraNeural");
    m.insert("ar-LB", "ar-LB-LaylaNeural");
    m.insert("ar-LY", "ar-LY-ImanNeural");
    m.insert("ar-MA", "ar-MA-MounaNeural");
    m.insert("ar-OM", "ar-OM-AyshaNeural");
    m.insert("ar-QA", "ar-QA-AmalNeural");
    m.insert("ar-SA", "ar-SA-ZariyahNeural");
    m.insert("ar-SY", "ar-SY-AmanyNeural");
    m.insert("ar-TN", "ar-TN-ReemNeural");
    m.insert("ar-YE", "ar-YE-MaryamNeural");
    m.insert("ar-IL", "ar-SA-ZariyahNeural"); // 降级: 以色列 → 沙特
    m.insert("ar-PS", "ar-SA-ZariyahNeural"); // 降级: 巴勒斯坦 → 沙特

    // ========== 西班牙语 ==========
    m.insert("es", "es-ES-ElviraNeural");
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

    // ========== 法语 ==========
    m.insert("fr", "fr-FR-DeniseNeural");
    m.insert("fr-BE", "fr-BE-CharlineNeural");
    m.insert("fr-CA", "fr-CA-SylvieNeural");
    m.insert("fr-CH", "fr-CH-ArianeNeural");
    m.insert("fr-FR", "fr-FR-DeniseNeural");

    // ========== 德语 ==========
    m.insert("de", "de-DE-KatjaNeural");
    m.insert("de-AT", "de-AT-IngridNeural");
    m.insert("de-CH", "de-CH-LeniNeural");
    m.insert("de-DE", "de-DE-KatjaNeural");

    // ========== 葡萄牙语 ==========
    m.insert("pt", "pt-BR-FranciscaNeural");
    m.insert("pt-BR", "pt-BR-FranciscaNeural");
    m.insert("pt-PT", "pt-PT-RaquelNeural");

    // ========== 意大利语 ==========
    m.insert("it", "it-IT-ElsaNeural");
    m.insert("it-IT", "it-IT-ElsaNeural");
    m.insert("it-CH", "it-IT-ElsaNeural"); // 降级: 瑞士 → 意大利

    // ========== 荷兰语 ==========
    m.insert("nl", "nl-NL-ColetteNeural");
    m.insert("nl-BE", "nl-BE-DenaNeural");
    m.insert("nl-NL", "nl-NL-ColetteNeural");

    // ========== 东亚语言 ==========
    m.insert("ja", "ja-JP-NanamiNeural");
    m.insert("ja-JP", "ja-JP-NanamiNeural");
    m.insert("ko", "ko-KR-SunHiNeural");
    m.insert("ko-KR", "ko-KR-SunHiNeural");
    m.insert("vi", "vi-VN-HoaiMyNeural");
    m.insert("vi-VN", "vi-VN-HoaiMyNeural");
    m.insert("th", "th-TH-PremwadeeNeural");
    m.insert("th-TH", "th-TH-PremwadeeNeural");
    m.insert("id", "id-ID-GadisNeural");
    m.insert("id-ID", "id-ID-GadisNeural");
    m.insert("ms", "ms-MY-YasminNeural");
    m.insert("ms-MY", "ms-MY-YasminNeural");
    m.insert("fil", "fil-PH-BlessicaNeural");
    m.insert("fil-PH", "fil-PH-BlessicaNeural");
    m.insert("jv-ID", "jv-ID-SitiNeural");
    m.insert("km-KH", "km-KH-SreymomNeural");
    m.insert("lo-LA", "lo-LA-KeomanyNeural");
    m.insert("my-MM", "my-MM-NilarNeural");

    // ========== 南亚语言 ==========
    m.insert("hi", "hi-IN-SwaraNeural");
    m.insert("hi-IN", "hi-IN-SwaraNeural");
    m.insert("bn", "bn-IN-TanishaaNeural");
    m.insert("bn-IN", "bn-IN-TanishaaNeural");
    m.insert("bn-BD", "bn-BD-NabanitaNeural");
    m.insert("ta", "ta-IN-PallaviNeural");
    m.insert("ta-IN", "ta-IN-PallaviNeural");
    m.insert("ta-LK", "ta-LK-SaranyaNeural");
    m.insert("ta-MY", "ta-MY-KaniNeural");
    m.insert("ta-SG", "ta-SG-VenbaNeural");
    m.insert("te-IN", "te-IN-ShrutiNeural");
    m.insert("mr-IN", "mr-IN-AarohiNeural");
    m.insert("gu-IN", "gu-IN-DhwaniNeural");
    m.insert("kn-IN", "kn-IN-SapnaNeural");
    m.insert("ml-IN", "ml-IN-SobhanaNeural");
    m.insert("pa-IN", "pa-IN-VaaniNeural");
    m.insert("ur", "ur-PK-UzmaNeural");
    m.insert("ur-IN", "ur-IN-GulNeural");
    m.insert("ur-PK", "ur-PK-UzmaNeural");
    m.insert("ne-NP", "ne-NP-HemkalaNeural");
    m.insert("si-LK", "si-LK-ThiliniNeural");

    // ========== 东欧/斯拉夫语言 ==========
    m.insert("ru", "ru-RU-SvetlanaNeural");
    m.insert("ru-RU", "ru-RU-SvetlanaNeural");
    m.insert("uk", "uk-UA-PolinaNeural");
    m.insert("uk-UA", "uk-UA-PolinaNeural");
    m.insert("pl", "pl-PL-ZofiaNeural");
    m.insert("pl-PL", "pl-PL-ZofiaNeural");
    m.insert("cs", "cs-CZ-VlastaNeural");
    m.insert("cs-CZ", "cs-CZ-VlastaNeural");
    m.insert("sk", "sk-SK-ViktoriaNeural");
    m.insert("sk-SK", "sk-SK-ViktoriaNeural");
    m.insert("hr", "hr-HR-GabrijelaNeural");
    m.insert("hr-HR", "hr-HR-GabrijelaNeural");
    m.insert("sr", "sr-RS-SophieNeural");
    m.insert("sr-RS", "sr-RS-SophieNeural");
    m.insert("sl", "sl-SI-PetraNeural");
    m.insert("sl-SI", "sl-SI-PetraNeural");
    m.insert("bg", "bg-BG-KalinaNeural");
    m.insert("bg-BG", "bg-BG-KalinaNeural");
    m.insert("mk-MK", "mk-MK-MarijaNeural");
    m.insert("bs-BA", "bs-BA-VesnaNeural");
    m.insert("sq", "sq-AL-AnilaNeural");
    m.insert("sq-AL", "sq-AL-AnilaNeural");

    // ========== 北欧语言 ==========
    m.insert("sv", "sv-SE-SofieNeural");
    m.insert("sv-SE", "sv-SE-SofieNeural");
    m.insert("da", "da-DK-ChristelNeural");
    m.insert("da-DK", "da-DK-ChristelNeural");
    m.insert("nb", "nb-NO-PernilleNeural");
    m.insert("nb-NO", "nb-NO-PernilleNeural");
    m.insert("no", "nb-NO-PernilleNeural"); // 挪威语默认用 Bokmål
    m.insert("nn", "nb-NO-PernilleNeural"); // Nynorsk fallback
    m.insert("nn-NO", "nb-NO-PernilleNeural");
    m.insert("fi", "fi-FI-NooraNeural");
    m.insert("fi-FI", "fi-FI-NooraNeural");
    m.insert("is-IS", "is-IS-GudrunNeural");
    m.insert("et", "et-EE-AnuNeural");
    m.insert("et-EE", "et-EE-AnuNeural");
    m.insert("lt", "lt-LT-OnaNeural");
    m.insert("lt-LT", "lt-LT-OnaNeural");
    m.insert("lv", "lv-LV-EveritaNeural");
    m.insert("lv-LV", "lv-LV-EveritaNeural");

    // ========== 其他欧洲语言 ==========
    m.insert("el", "el-GR-AthinaNeural");
    m.insert("el-GR", "el-GR-AthinaNeural");
    m.insert("hu", "hu-HU-NoemiNeural");
    m.insert("hu-HU", "hu-HU-NoemiNeural");
    m.insert("ro", "ro-RO-AlinaNeural");
    m.insert("ro-RO", "ro-RO-AlinaNeural");
    m.insert("tr", "tr-TR-EmelNeural");
    m.insert("tr-TR", "tr-TR-EmelNeural");
    m.insert("ca", "ca-ES-AlbaNeural");
    m.insert("ca-ES", "ca-ES-AlbaNeural");
    m.insert("eu-ES", "eu-ES-AinhoaNeural");
    m.insert("gl-ES", "gl-ES-SabelaNeural");
    m.insert("cy", "cy-GB-NiaNeural");
    m.insert("cy-GB", "cy-GB-NiaNeural");
    m.insert("ga", "ga-IE-OrlaNeural");
    m.insert("ga-IE", "ga-IE-OrlaNeural");
    m.insert("mt", "mt-MT-GraceNeural");
    m.insert("mt-MT", "mt-MT-GraceNeural");

    // ========== 中亚/西亚语言 ==========
    m.insert("fa", "fa-IR-DilaraNeural");
    m.insert("fa-IR", "fa-IR-DilaraNeural");
    m.insert("he", "he-IL-HilaNeural");
    m.insert("he-IL", "he-IL-HilaNeural");
    m.insert("az", "az-AZ-BanuNeural");
    m.insert("az-AZ", "az-AZ-BanuNeural");
    m.insert("ka", "ka-GE-EkaNeural");
    m.insert("ka-GE", "ka-GE-EkaNeural");
    m.insert("hy", "hy-AM-AnahitNeural");
    m.insert("hy-AM", "hy-AM-AnahitNeural");
    m.insert("kk", "kk-KZ-AigulNeural");
    m.insert("kk-KZ", "kk-KZ-AigulNeural");
    m.insert("uz", "uz-UZ-MadinaNeural");
    m.insert("uz-UZ", "uz-UZ-MadinaNeural");
    m.insert("mn", "mn-MN-YesuiNeural");
    m.insert("mn-MN", "mn-MN-YesuiNeural");
    m.insert("ps", "ps-AF-LatifaNeural");
    m.insert("ps-AF", "ps-AF-LatifaNeural");

    // ========== 非洲语言 ==========
    m.insert("af", "af-ZA-AdriNeural");
    m.insert("af-ZA", "af-ZA-AdriNeural");
    m.insert("am", "am-ET-MekdesNeural");
    m.insert("am-ET", "am-ET-MekdesNeural");
    m.insert("sw", "sw-KE-ZuriNeural");
    m.insert("sw-KE", "sw-KE-ZuriNeural");
    m.insert("sw-TZ", "sw-TZ-RehemaNeural");
    m.insert("so", "so-SO-UbaxNeural");
    m.insert("so-SO", "so-SO-UbaxNeural");
    m.insert("zu", "zu-ZA-ThandoNeural");
    m.insert("zu-ZA", "zu-ZA-ThandoNeural");

    m
});

/// 根据语言代码获取对应的 Edge TTS 女声 ID
///
/// # Arguments
/// * `lang_code` - ISO 639-1 或 BCP 47 语言代码（如 "zh", "zh-CN", "en-US"）
///
/// # Returns
/// * `Some(voice_id)` - 对应的 Edge TTS 声音 ID
/// * `None` - 不支持的语言
pub fn get_voice_for_language(lang_code: &str) -> Option<&'static str> {
    // 尝试精确匹配
    if let Some(voice) = EDGE_TTS_VOICE_MAP.get(lang_code) {
        return Some(voice);
    }

    // 尝试语言基础码匹配 (zh-CN -> zh)
    if let Some(base) = lang_code.split('-').next() {
        return EDGE_TTS_VOICE_MAP.get(base).copied();
    }

    None
}

/// 获取所有支持的语言代码
#[allow(dead_code)]
pub fn get_supported_languages() -> Vec<&'static str> {
    EDGE_TTS_VOICE_MAP.keys().copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_voice_for_language() {
        // 测试精确匹配
        assert_eq!(get_voice_for_language("zh-CN"), Some("zh-CN-XiaoxiaoNeural"));
        assert_eq!(get_voice_for_language("en-US"), Some("en-US-JennyNeural"));
        assert_eq!(get_voice_for_language("ja-JP"), Some("ja-JP-NanamiNeural"));

        // 测试基础码匹配
        assert_eq!(get_voice_for_language("zh"), Some("zh-CN-XiaoxiaoNeural"));
        assert_eq!(get_voice_for_language("en"), Some("en-US-JennyNeural"));
        assert_eq!(get_voice_for_language("ja"), Some("ja-JP-NanamiNeural"));

        // 测试不支持的语言
        assert_eq!(get_voice_for_language("xyz"), None);
    }

    #[test]
    fn test_fallback_voices() {
        // 测试降级声音
        assert_eq!(get_voice_for_language("en-GH"), Some("en-NG-EzinneNeural"));
        assert_eq!(get_voice_for_language("ar-IL"), Some("ar-SA-ZariyahNeural"));
        assert_eq!(get_voice_for_language("it-CH"), Some("it-IT-ElsaNeural"));
    }
}
