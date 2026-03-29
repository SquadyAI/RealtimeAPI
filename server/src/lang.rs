//! 统一语言配置模块
//!
//! 提供 32 种语言的统一配置，避免在多处重复维护语言映射。
//! 包括：
//! - 语言代码到全名的映射（用于 LLM prompt）
//! - 语言代码到 Lingua 名称的映射（用于语言检测）
//! - 语言示例表（用于同声传译）

/// 语言信息结构体
#[derive(Debug, Clone)]
pub struct LanguageInfo {
    /// 语言代码别名列表（第一个为主代码）
    pub codes: &'static [&'static str],
    /// 英文全名（用于 LLM prompt）
    pub english_name: &'static str,
    /// 中文全名（用于中文界面显示）
    pub chinese_name: &'static str,
    /// 原生语言名称
    pub native_name: &'static str,
    /// Lingua 检测库返回的语言名（小写）
    pub lingua_name: &'static str,
    /// "你是谁"的翻译示例（用于同传示例）
    pub example_phrase: &'static str,
}

/// 32 种支持的语言配置表
/// 添加新语言只需在此表中添加一条记录
pub static LANGUAGES: &[LanguageInfo] = &[
    // 1. 中文（普通话）
    LanguageInfo {
        codes: &["zh", "zh-cn", "zh-hans"],
        english_name: "Chinese (Mandarin)",
        chinese_name: "中文",
        native_name: "中文",
        lingua_name: "chinese",
        example_phrase: "你是谁",
    },
    // 2. 中文（粤语）
    LanguageInfo {
        codes: &["zh-hk", "yue"],
        english_name: "Cantonese",
        chinese_name: "粤语",
        native_name: "粵語",
        lingua_name: "chinese",
        example_phrase: "你係邊個",
    },
    // 3. 英语（美式）- 作为默认英语
    LanguageInfo {
        codes: &["en-us", "en"],
        english_name: "English (American)",
        chinese_name: "英语（美式）",
        native_name: "English",
        lingua_name: "english",
        example_phrase: "Who are you",
    },
    // 4. 英语（英式）
    LanguageInfo {
        codes: &["en-uk", "en-gb"],
        english_name: "English (British)",
        chinese_name: "英语（英式）",
        native_name: "English",
        lingua_name: "english",
        example_phrase: "Who are you",
    },
    // 5. 英语（澳式）
    LanguageInfo {
        codes: &["en-au", "en-nz"],
        english_name: "English (Australian)",
        chinese_name: "英语（澳式）",
        native_name: "English",
        lingua_name: "english",
        example_phrase: "Who are you",
    },
    // 6. 英语（印式）
    LanguageInfo {
        codes: &["en-in", "en-pk"],
        english_name: "English (Indian)",
        chinese_name: "英语（印式）",
        native_name: "English",
        lingua_name: "english",
        example_phrase: "Who are you",
    },
    // 6.1 英语（其他变体）- 香港、新加坡、菲律宾等
    LanguageInfo {
        codes: &[
            "en-hk", "en-sg", "en-ph", "en-ca", "en-ie", "en-za", "en-gh", "en-ke", "en-ng", "en-tz",
        ],
        english_name: "English",
        chinese_name: "英语",
        native_name: "English",
        lingua_name: "english",
        example_phrase: "Who are you",
    },
    // 7. 日语
    LanguageInfo {
        codes: &["ja", "jp", "ja-jp"],
        english_name: "Japanese",
        chinese_name: "日语",
        native_name: "日本語",
        lingua_name: "japanese",
        example_phrase: "あなたは誰ですか",
    },
    // 8. 韩语
    LanguageInfo {
        codes: &["ko", "ko-kr"],
        english_name: "Korean",
        chinese_name: "韩语",
        native_name: "한국어",
        lingua_name: "korean",
        example_phrase: "당신은 누구입니까",
    },
    // 9. 越南语
    LanguageInfo {
        codes: &["vi", "vi-vn"],
        english_name: "Vietnamese",
        chinese_name: "越南语",
        native_name: "Tiếng Việt",
        lingua_name: "vietnamese",
        example_phrase: "Bạn là ai",
    },
    // 10. 印尼语
    LanguageInfo {
        codes: &["id", "id-id"],
        english_name: "Indonesian",
        chinese_name: "印尼语",
        native_name: "Bahasa Indonesia",
        lingua_name: "indonesian",
        example_phrase: "Siapa kamu",
    },
    // 11. 泰语
    LanguageInfo {
        codes: &["th", "th-th"],
        english_name: "Thai",
        chinese_name: "泰语",
        native_name: "ภาษาไทย",
        lingua_name: "thai",
        example_phrase: "คุณเป็นใคร",
    },
    // 12. 印地语
    LanguageInfo {
        codes: &["hi", "hi-in"],
        english_name: "Hindi",
        chinese_name: "印地语",
        native_name: "हिन्दी",
        lingua_name: "hindi",
        example_phrase: "आप कौन हैं",
    },
    // 13. 西班牙语
    LanguageInfo {
        codes: &["es", "es-es", "es-mx", "es-ar", "es-co", "es-cl", "es-pe", "es-ve", "es-us"],
        english_name: "Spanish",
        chinese_name: "西班牙语",
        native_name: "Español",
        lingua_name: "spanish",
        example_phrase: "¿Quién eres tú?",
    },
    // 14. 法语
    LanguageInfo {
        codes: &["fr", "fr-fr", "fr-ca", "fr-be", "fr-ch"],
        english_name: "French",
        chinese_name: "法语",
        native_name: "Français",
        lingua_name: "french",
        example_phrase: "Qui êtes-vous?",
    },
    // 15. 德语
    LanguageInfo {
        codes: &["de", "de-de", "de-at", "de-ch"],
        english_name: "German",
        chinese_name: "德语",
        native_name: "Deutsch",
        lingua_name: "german",
        example_phrase: "Wer bist du?",
    },
    // 16. 葡萄牙语（欧洲）
    LanguageInfo {
        codes: &["pt-pt", "pt"],
        english_name: "Portuguese (European)",
        chinese_name: "葡萄牙语（欧洲）",
        native_name: "Português",
        lingua_name: "portuguese",
        example_phrase: "Quem é você?",
    },
    // 17. 葡萄牙语（巴西）
    LanguageInfo {
        codes: &["pt-br"],
        english_name: "Portuguese (Brazilian)",
        chinese_name: "葡萄牙语（巴西）",
        native_name: "Português",
        lingua_name: "portuguese",
        example_phrase: "Quem é você?",
    },
    // 18. 意大利语
    LanguageInfo {
        codes: &["it", "it-it", "it-ch"],
        english_name: "Italian",
        chinese_name: "意大利语",
        native_name: "Italiano",
        lingua_name: "italian",
        example_phrase: "Chi sei tu?",
    },
    // 19. 俄语
    LanguageInfo {
        codes: &["ru", "ru-ru"],
        english_name: "Russian",
        chinese_name: "俄语",
        native_name: "Русский",
        lingua_name: "russian",
        example_phrase: "Кто ты?",
    },
    // 20. 土耳其语
    LanguageInfo {
        codes: &["tr", "tr-tr"],
        english_name: "Turkish",
        chinese_name: "土耳其语",
        native_name: "Türkçe",
        lingua_name: "turkish",
        example_phrase: "Sen kimsin?",
    },
    // 21. 乌克兰语
    LanguageInfo {
        codes: &["uk", "uk-ua"],
        english_name: "Ukrainian",
        chinese_name: "乌克兰语",
        native_name: "Українська",
        lingua_name: "ukrainian",
        example_phrase: "Хто ти?",
    },
    // 22. 波兰语
    LanguageInfo {
        codes: &["pl", "pl-pl"],
        english_name: "Polish",
        chinese_name: "波兰语",
        native_name: "Polski",
        lingua_name: "polish",
        example_phrase: "Kim jesteś?",
    },
    // 23. 荷兰语
    LanguageInfo {
        codes: &["nl", "nl-nl", "nl-be"],
        english_name: "Dutch",
        chinese_name: "荷兰语",
        native_name: "Nederlands",
        lingua_name: "dutch",
        example_phrase: "Wie ben jij?",
    },
    // 24. 希腊语
    LanguageInfo {
        codes: &["el", "el-gr"],
        english_name: "Greek",
        chinese_name: "希腊语",
        native_name: "Ελληνικά",
        lingua_name: "greek",
        example_phrase: "Ποιος είσαι;",
    },
    // 25. 罗马尼亚语
    LanguageInfo {
        codes: &["ro", "ro-ro"],
        english_name: "Romanian",
        chinese_name: "罗马尼亚语",
        native_name: "Română",
        lingua_name: "romanian",
        example_phrase: "Cine ești tu?",
    },
    // 26. 捷克语
    LanguageInfo {
        codes: &["cs", "cs-cz"],
        english_name: "Czech",
        chinese_name: "捷克语",
        native_name: "Čeština",
        lingua_name: "czech",
        example_phrase: "Kdo jsi?",
    },
    // 27. 芬兰语
    LanguageInfo {
        codes: &["fi", "fi-fi"],
        english_name: "Finnish",
        chinese_name: "芬兰语",
        native_name: "Suomi",
        lingua_name: "finnish",
        example_phrase: "Kuka sinä olet?",
    },
    // 28. 阿拉伯语
    LanguageInfo {
        codes: &[
            "ar", "ar-sa", "ar-ae", "ar-eg", "ar-ma", "ar-iq", "ar-jo", "ar-kw", "ar-lb", "ar-qa",
        ],
        english_name: "Arabic",
        chinese_name: "阿拉伯语",
        native_name: "العربية",
        lingua_name: "arabic",
        example_phrase: "من أنت؟",
    },
    // 29. 瑞典语
    LanguageInfo {
        codes: &["sv", "sv-se"],
        english_name: "Swedish",
        chinese_name: "瑞典语",
        native_name: "Svenska",
        lingua_name: "swedish",
        example_phrase: "Vem är du?",
    },
    // 30. 挪威语
    LanguageInfo {
        codes: &["no", "nb", "nb-no", "nn", "nn-no"],
        english_name: "Norwegian",
        chinese_name: "挪威语",
        native_name: "Norsk",
        lingua_name: "norwegian",
        example_phrase: "Hvem er du?",
    },
    // 31. 丹麦语
    LanguageInfo {
        codes: &["da", "da-dk"],
        english_name: "Danish",
        chinese_name: "丹麦语",
        native_name: "Dansk",
        lingua_name: "danish",
        example_phrase: "Hvem er du?",
    },
    // 32. 南非荷兰语
    LanguageInfo {
        codes: &["af", "af-za"],
        english_name: "Afrikaans",
        chinese_name: "南非荷兰语",
        native_name: "Afrikaans",
        lingua_name: "afrikaans",
        example_phrase: "Wie is jy?",
    },
    // 繁体中文（台湾）- 额外支持
    LanguageInfo {
        codes: &["zh-tw", "zh-hant"],
        english_name: "Traditional Chinese",
        chinese_name: "繁体中文",
        native_name: "繁體中文",
        lingua_name: "chinese",
        example_phrase: "你是誰",
    },
    // ============ 以下为 BCP-47 补充语言 ============
    // 33. 阿姆哈拉语
    LanguageInfo {
        codes: &["am", "am-et"],
        english_name: "Amharic",
        chinese_name: "阿姆哈拉语",
        native_name: "አማርኛ",
        lingua_name: "amharic",
        example_phrase: "አንተ ማን ነህ?",
    },
    // 34. 阿塞拜疆语
    LanguageInfo {
        codes: &["az", "az-az"],
        english_name: "Azerbaijani",
        chinese_name: "阿塞拜疆语",
        native_name: "Azərbaycan",
        lingua_name: "azerbaijani",
        example_phrase: "Sən kimsən?",
    },
    // 35. 保加利亚语
    LanguageInfo {
        codes: &["bg", "bg-bg"],
        english_name: "Bulgarian",
        chinese_name: "保加利亚语",
        native_name: "Български",
        lingua_name: "bulgarian",
        example_phrase: "Кой си ти?",
    },
    // 36. 孟加拉语
    LanguageInfo {
        codes: &["bn", "bn-in", "bn-bd"],
        english_name: "Bengali",
        chinese_name: "孟加拉语",
        native_name: "বাংলা",
        lingua_name: "bengali",
        example_phrase: "তুমি কে?",
    },
    // 37. 波斯尼亚语
    LanguageInfo {
        codes: &["bs", "bs-ba"],
        english_name: "Bosnian",
        chinese_name: "波斯尼亚语",
        native_name: "Bosanski",
        lingua_name: "bosnian",
        example_phrase: "Ko si ti?",
    },
    // 38. 加泰罗尼亚语
    LanguageInfo {
        codes: &["ca", "ca-es"],
        english_name: "Catalan",
        chinese_name: "加泰罗尼亚语",
        native_name: "Català",
        lingua_name: "catalan",
        example_phrase: "Qui ets tu?",
    },
    // 39. 威尔士语
    LanguageInfo {
        codes: &["cy", "cy-gb"],
        english_name: "Welsh",
        chinese_name: "威尔士语",
        native_name: "Cymraeg",
        lingua_name: "welsh",
        example_phrase: "Pwy wyt ti?",
    },
    // 40. 爱沙尼亚语
    LanguageInfo {
        codes: &["et", "et-ee"],
        english_name: "Estonian",
        chinese_name: "爱沙尼亚语",
        native_name: "Eesti",
        lingua_name: "estonian",
        example_phrase: "Kes sa oled?",
    },
    // 41. 巴斯克语
    LanguageInfo {
        codes: &["eu", "eu-es"],
        english_name: "Basque",
        chinese_name: "巴斯克语",
        native_name: "Euskara",
        lingua_name: "basque",
        example_phrase: "Nor zara zu?",
    },
    // 42. 波斯语
    LanguageInfo {
        codes: &["fa", "fa-ir", "per"],
        english_name: "Persian",
        chinese_name: "波斯语",
        native_name: "فارسی",
        lingua_name: "persian",
        example_phrase: "تو کی هستی؟",
    },
    // 43. 菲律宾语/塔加洛语
    LanguageInfo {
        codes: &["fil", "fil-ph", "tl"],
        english_name: "Filipino",
        chinese_name: "菲律宾语",
        native_name: "Filipino",
        lingua_name: "tagalog",
        example_phrase: "Sino ka?",
    },
    // 44. 爱尔兰语
    LanguageInfo {
        codes: &["ga", "ga-ie"],
        english_name: "Irish",
        chinese_name: "爱尔兰语",
        native_name: "Gaeilge",
        lingua_name: "irish",
        example_phrase: "Cé tú féin?",
    },
    // 45. 加利西亚语
    LanguageInfo {
        codes: &["gl", "gl-es"],
        english_name: "Galician",
        chinese_name: "加利西亚语",
        native_name: "Galego",
        lingua_name: "galician",
        example_phrase: "Quen es ti?",
    },
    // 46. 古吉拉特语
    LanguageInfo {
        codes: &["gu", "gu-in"],
        english_name: "Gujarati",
        chinese_name: "古吉拉特语",
        native_name: "ગુજરાતી",
        lingua_name: "gujarati",
        example_phrase: "તમે કોણ છો?",
    },
    // 47. 希伯来语
    LanguageInfo {
        codes: &["he", "he-il", "iw"],
        english_name: "Hebrew",
        chinese_name: "希伯来语",
        native_name: "עברית",
        lingua_name: "hebrew",
        example_phrase: "?מי אתה",
    },
    // 48. 克罗地亚语
    LanguageInfo {
        codes: &["hr", "hr-hr"],
        english_name: "Croatian",
        chinese_name: "克罗地亚语",
        native_name: "Hrvatski",
        lingua_name: "croatian",
        example_phrase: "Tko si ti?",
    },
    // 49. 匈牙利语
    LanguageInfo {
        codes: &["hu", "hu-hu"],
        english_name: "Hungarian",
        chinese_name: "匈牙利语",
        native_name: "Magyar",
        lingua_name: "hungarian",
        example_phrase: "Ki vagy te?",
    },
    // 50. 亚美尼亚语
    LanguageInfo {
        codes: &["hy", "hy-am"],
        english_name: "Armenian",
        chinese_name: "亚美尼亚语",
        native_name: "Հdelays",
        lingua_name: "armenian",
        example_phrase: " Delays delays?",
    },
    // 51. 冰岛语
    LanguageInfo {
        codes: &["is", "is-is"],
        english_name: "Icelandic",
        chinese_name: "冰岛语",
        native_name: "Íslenska",
        lingua_name: "icelandic",
        example_phrase: "Hver ert þú?",
    },
    // 52. 爪哇语
    LanguageInfo {
        codes: &["jv", "jv-id"],
        english_name: "Javanese",
        chinese_name: "爪哇语",
        native_name: "Basa Jawa",
        lingua_name: "javanese",
        example_phrase: "Kowe sopo?",
    },
    // 53. 格鲁吉亚语
    LanguageInfo {
        codes: &["ka", "ka-ge"],
        english_name: "Georgian",
        chinese_name: "格鲁吉亚语",
        native_name: "ქართული",
        lingua_name: "georgian",
        example_phrase: "ვინ ხარ შენ?",
    },
    // 54. 哈萨克语
    LanguageInfo {
        codes: &["kk", "kk-kz"],
        english_name: "Kazakh",
        chinese_name: "哈萨克语",
        native_name: "Қазақша",
        lingua_name: "kazakh",
        example_phrase: "Сен кімсің?",
    },
    // 55. 高棉语
    LanguageInfo {
        codes: &["km", "km-kh"],
        english_name: "Khmer",
        chinese_name: "高棉语",
        native_name: "ភាសាខ្មែរ",
        lingua_name: "khmer",
        example_phrase: "អ្នកជានរណា?",
    },
    // 56. 卡纳达语
    LanguageInfo {
        codes: &["kn", "kn-in"],
        english_name: "Kannada",
        chinese_name: "卡纳达语",
        native_name: "ಕನ್ನಡ",
        lingua_name: "kannada",
        example_phrase: "ನೀನು ಯಾರು?",
    },
    // 57. 老挝语
    LanguageInfo {
        codes: &["lo", "lo-la"],
        english_name: "Lao",
        chinese_name: "老挝语",
        native_name: "ລາວ",
        lingua_name: "lao",
        example_phrase: "ເຈົ້າແມ່ນໃຜ?",
    },
    // 58. 立陶宛语
    LanguageInfo {
        codes: &["lt", "lt-lt"],
        english_name: "Lithuanian",
        chinese_name: "立陶宛语",
        native_name: "Lietuvių",
        lingua_name: "lithuanian",
        example_phrase: "Kas tu esi?",
    },
    // 59. 拉脱维亚语
    LanguageInfo {
        codes: &["lv", "lv-lv"],
        english_name: "Latvian",
        chinese_name: "拉脱维亚语",
        native_name: "Latviešu",
        lingua_name: "latvian",
        example_phrase: "Kas tu esi?",
    },
    // 60. 马其顿语
    LanguageInfo {
        codes: &["mk", "mk-mk"],
        english_name: "Macedonian",
        chinese_name: "马其顿语",
        native_name: "Македонски",
        lingua_name: "macedonian",
        example_phrase: "Кој си ти?",
    },
    // 61. 马拉雅拉姆语
    LanguageInfo {
        codes: &["ml", "ml-in"],
        english_name: "Malayalam",
        chinese_name: "马拉雅拉姆语",
        native_name: "മലയാളം",
        lingua_name: "malayalam",
        example_phrase: "നീ ആരാണ്?",
    },
    // 62. 蒙古语
    LanguageInfo {
        codes: &["mn", "mn-mn"],
        english_name: "Mongolian",
        chinese_name: "蒙古语",
        native_name: "Монгол",
        lingua_name: "mongolian",
        example_phrase: "Чи хэн бэ?",
    },
    // 63. 马拉地语
    LanguageInfo {
        codes: &["mr", "mr-in"],
        english_name: "Marathi",
        chinese_name: "马拉地语",
        native_name: "मराठी",
        lingua_name: "marathi",
        example_phrase: "तू कोण आहेस?",
    },
    // 64. 马来语
    LanguageInfo {
        codes: &["ms", "ms-my"],
        english_name: "Malay",
        chinese_name: "马来语",
        native_name: "Bahasa Melayu",
        lingua_name: "malay",
        example_phrase: "Siapa awak?",
    },
    // 65. 马耳他语
    LanguageInfo {
        codes: &["mt", "mt-mt"],
        english_name: "Maltese",
        chinese_name: "马耳他语",
        native_name: "Malti",
        lingua_name: "maltese",
        example_phrase: "Int min int?",
    },
    // 66. 缅甸语
    LanguageInfo {
        codes: &["my", "my-mm"],
        english_name: "Burmese",
        chinese_name: "缅甸语",
        native_name: "မြန်မာစာ",
        lingua_name: "burmese",
        example_phrase: "မင်းကဘယ်သူလဲ?",
    },
    // 67. 尼泊尔语
    LanguageInfo {
        codes: &["ne", "ne-np"],
        english_name: "Nepali",
        chinese_name: "尼泊尔语",
        native_name: "नेपाली",
        lingua_name: "nepali",
        example_phrase: "तिमी को हौ?",
    },
    // 68. 旁遮普语
    LanguageInfo {
        codes: &["pa", "pa-in"],
        english_name: "Punjabi",
        chinese_name: "旁遮普语",
        native_name: "ਪੰਜਾਬੀ",
        lingua_name: "punjabi",
        example_phrase: "ਤੂੰ ਕੌਣ ਹੈਂ?",
    },
    // 69. 普什图语
    LanguageInfo {
        codes: &["ps", "ps-af"],
        english_name: "Pashto",
        chinese_name: "普什图语",
        native_name: "پښتو",
        lingua_name: "pashto",
        example_phrase: "ته څوک یې?",
    },
    // 70. 僧伽罗语
    LanguageInfo {
        codes: &["si", "si-lk"],
        english_name: "Sinhala",
        chinese_name: "僧伽罗语",
        native_name: "සිංහල",
        lingua_name: "sinhala",
        example_phrase: "ඔයා කවුද?",
    },
    // 71. 斯洛伐克语
    LanguageInfo {
        codes: &["sk", "sk-sk"],
        english_name: "Slovak",
        chinese_name: "斯洛伐克语",
        native_name: "Slovenčina",
        lingua_name: "slovak",
        example_phrase: "Kto si?",
    },
    // 72. 斯洛文尼亚语
    LanguageInfo {
        codes: &["sl", "sl-si"],
        english_name: "Slovenian",
        chinese_name: "斯洛文尼亚语",
        native_name: "Slovenščina",
        lingua_name: "slovenian",
        example_phrase: "Kdo si ti?",
    },
    // 73. 索马里语
    LanguageInfo {
        codes: &["so", "so-so"],
        english_name: "Somali",
        chinese_name: "索马里语",
        native_name: "Soomaali",
        lingua_name: "somali",
        example_phrase: "Waa kuma?",
    },
    // 74. 阿尔巴尼亚语
    LanguageInfo {
        codes: &["sq", "sq-al"],
        english_name: "Albanian",
        chinese_name: "阿尔巴尼亚语",
        native_name: "Shqip",
        lingua_name: "albanian",
        example_phrase: "Kush je ti?",
    },
    // 75. 塞尔维亚语
    LanguageInfo {
        codes: &["sr", "sr-rs"],
        english_name: "Serbian",
        chinese_name: "塞尔维亚语",
        native_name: "Српски",
        lingua_name: "serbian",
        example_phrase: "Ко си ти?",
    },
    // 76. 斯瓦希里语
    LanguageInfo {
        codes: &["sw", "sw-ke", "sw-tz"],
        english_name: "Swahili",
        chinese_name: "斯瓦希里语",
        native_name: "Kiswahili",
        lingua_name: "swahili",
        example_phrase: "Wewe ni nani?",
    },
    // 77. 泰米尔语
    LanguageInfo {
        codes: &["ta", "ta-in"],
        english_name: "Tamil",
        chinese_name: "泰米尔语",
        native_name: "தமிழ்",
        lingua_name: "tamil",
        example_phrase: "நீ யார்?",
    },
    // 78. 泰卢固语
    LanguageInfo {
        codes: &["te", "te-in"],
        english_name: "Telugu",
        chinese_name: "泰卢固语",
        native_name: "తెలుగు",
        lingua_name: "telugu",
        example_phrase: "నువ్వు ఎవరు?",
    },
    // 79. 乌尔都语
    LanguageInfo {
        codes: &["ur", "ur-in", "ur-pk"],
        english_name: "Urdu",
        chinese_name: "乌尔都语",
        native_name: "اردو",
        lingua_name: "urdu",
        example_phrase: "تم کون ہو؟",
    },
    // 80. 乌兹别克语
    LanguageInfo {
        codes: &["uz", "uz-uz"],
        english_name: "Uzbek",
        chinese_name: "乌兹别克语",
        native_name: "Oʻzbek",
        lingua_name: "uzbek",
        example_phrase: "Sen kimsan?",
    },
    // 81. 祖鲁语
    LanguageInfo {
        codes: &["zu", "zu-za"],
        english_name: "Zulu",
        chinese_name: "祖鲁语",
        native_name: "isiZulu",
        lingua_name: "zulu",
        example_phrase: "Ungubani?",
    },
];

/// 根据语言代码查找语言信息
/// 支持模糊匹配：先精确匹配，再尝试基础语言代码匹配
/// 例如 "ja-JP" 会先尝试精确匹配，找不到则尝试匹配 "ja"
pub fn find_language(code: &str) -> Option<&'static LanguageInfo> {
    let code_lower = code.to_lowercase();

    // 1. 精确匹配
    if let Some(lang) = LANGUAGES.iter().find(|lang| lang.codes.iter().any(|&c| c == code_lower)) {
        return Some(lang);
    }

    // 2. 模糊匹配：尝试匹配基础语言代码（如 ja-JP -> ja, ja_JP -> ja）
    let base_code = code_lower.split(['-', '_']).next().unwrap_or(&code_lower);

    if base_code != code_lower {
        return LANGUAGES.iter().find(|lang| lang.codes.contains(&base_code));
    }

    None
}

/// 获取语言的英文全名（用于 LLM prompt）
/// 如果未找到，返回 "Unknown"
pub fn get_english_name(code: &str) -> &'static str {
    find_language(code).map(|lang| lang.english_name).unwrap_or("Unknown")
}

/// 获取语言的中文全名（用于中文界面显示）
/// 如果未找到，返回原始代码
pub fn get_chinese_name(code: &str) -> &'static str {
    find_language(code).map(|lang| lang.chinese_name).unwrap_or("未知语言")
}

/// 根据用户语言获取显示名称
/// - 如果 display_lang 是中文（zh 开头），返回中文名
/// - 否则返回英文名
pub fn get_display_name(code: &str, display_lang: &str) -> &'static str {
    let is_chinese = display_lang.to_lowercase().starts_with("zh");
    if is_chinese { get_chinese_name(code) } else { get_english_name(code) }
}

/// 获取语言的 Lingua 名称（用于语言检测匹配）
/// 如果未找到，返回小写的原始代码
pub fn get_lingua_name(code: &str) -> String {
    find_language(code)
        .map(|lang| lang.lingua_name.to_string())
        .unwrap_or_else(|| code.to_lowercase())
}

/// 根据 Lingua 检测返回的语言名称查找语言信息
/// 返回对应的 BCP-47 代码（取 codes 数组的第一个）
/// lingua_name 参数不区分大小写，如 "English", "chinese" 都可以
pub fn find_bcp47_by_lingua_name(lingua_name: &str) -> Option<&'static str> {
    let name_lower = lingua_name.to_lowercase();
    LANGUAGES
        .iter()
        .find(|lang| lang.lingua_name == name_lower)
        .and_then(|lang| lang.codes.first().copied())
}

/// 判断 MiniMax 格式的语言名是否匹配某个语言代码
/// minimax_name: MiniMax 格式，如 "English", "Chinese", "Filipino"
/// lang_code: BCP-47 代码，如 "en", "zh", "fil"
pub fn minimax_name_matches_code(minimax_name: &str, lang_code: &str) -> bool {
    // 先找到 lang_code 对应的 LanguageInfo
    let lang_info = match find_language(lang_code) {
        Some(info) => info,
        None => return false,
    };

    // MiniMax 名称到 Lingua 名称的映射
    let minimax_to_lingua: &[(&str, &str)] = &[
        ("Chinese", "chinese"),
        ("English", "english"),
        ("Spanish", "spanish"),
        ("French", "french"),
        ("German", "german"),
        ("Portuguese", "portuguese"),
        ("Italian", "italian"),
        ("Japanese", "japanese"),
        ("Korean", "korean"),
        ("Russian", "russian"),
        ("Arabic", "arabic"),
        ("Turkish", "turkish"),
        ("Dutch", "dutch"),
        ("Ukrainian", "ukrainian"),
        ("Vietnamese", "vietnamese"),
        ("Indonesian", "indonesian"),
        ("Thai", "thai"),
        ("Polish", "polish"),
        ("Romanian", "romanian"),
        ("Greek", "greek"),
        ("Czech", "czech"),
        ("Finnish", "finnish"),
        ("Hindi", "hindi"),
        ("Bulgarian", "bulgarian"),
        ("Danish", "danish"),
        ("Hebrew", "hebrew"),
        ("Malay", "malay"),
        ("Persian", "persian"),
        ("Slovak", "slovak"),
        ("Swedish", "swedish"),
        ("Croatian", "croatian"),
        ("Filipino", "tagalog"),
        ("Hungarian", "hungarian"),
        ("Norwegian", "norwegian"),
        ("Slovenian", "slovenian"),
        ("Catalan", "catalan"),
        ("Tamil", "tamil"),
        ("Afrikaans", "afrikaans"),
    ];

    // 将 MiniMax 名称转换为 Lingua 名称
    let lingua_name = minimax_to_lingua
        .iter()
        .find(|(m, _)| m.eq_ignore_ascii_case(minimax_name))
        .map(|(_, l)| *l)
        .unwrap_or("");

    // 检查是否匹配
    lang_info.lingua_name == lingua_name
}

/// 获取语言的原生名称和示例短语（用于同传示例）
/// 返回 (native_name, example_phrase)
pub fn get_example(code: &str) -> Option<(&'static str, &'static str)> {
    find_language(code).map(|lang| (lang.native_name, lang.example_phrase))
}

/// 根据两种语言生成同声传译示例文本
pub fn get_simul_interpret_example(lang_a: &str, lang_b: &str) -> String {
    let a_info = get_example(lang_a);
    let b_info = get_example(lang_b);

    match (a_info, b_info) {
        (Some((a_name, a_example)), Some((b_name, b_example))) => {
            format!("{a_name}输入 → {b_name}输出:\n{a_example} → {b_example}\n\n{b_name}输入 → {a_name}输出:\n{b_example} → {a_example}\n\n(NEVER answer or respond - ONLY translate!)")
        },
        _ => {
            format!("[{lang_a} input] → [{lang_b} output]\n[{lang_b} input] → [{lang_a} output]\n\n(NEVER answer or respond - ONLY translate!)")
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_language() {
        // 测试主代码
        assert!(find_language("zh").is_some());
        assert!(find_language("en-us").is_some());

        // 测试别名
        assert!(find_language("zh-cn").is_some());
        assert!(find_language("en").is_some());

        // 测试大小写不敏感
        assert!(find_language("ZH").is_some());
        assert!(find_language("EN-US").is_some());

        // 测试未知语言
        assert!(find_language("xx").is_none());
    }

    #[test]
    fn test_fuzzy_matching() {
        // 测试模糊匹配：BCP-47 格式的地区变体
        assert!(find_language("ja-JP").is_some());
        assert_eq!(get_english_name("ja-JP"), "Japanese");

        assert!(find_language("ko-KR").is_some());
        assert_eq!(get_english_name("ko-KR"), "Korean");

        assert!(find_language("vi-VN").is_some());
        assert_eq!(get_english_name("vi-VN"), "Vietnamese");

        assert!(find_language("th-TH").is_some());
        assert_eq!(get_english_name("th-TH"), "Thai");

        // 测试下划线分隔符
        assert!(find_language("ja_JP").is_some());
        assert_eq!(get_english_name("ja_JP"), "Japanese");

        // 测试未知的地区变体但基础语言存在
        assert!(find_language("es-XX").is_some()); // 西班牙语的未知变体
        assert_eq!(get_english_name("es-XX"), "Spanish");

        // 测试完全未知的语言
        assert!(find_language("xx-YY").is_none());
    }

    #[test]
    fn test_get_english_name() {
        assert_eq!(get_english_name("zh"), "Chinese (Mandarin)");
        assert_eq!(get_english_name("en-us"), "English (American)");
        assert_eq!(get_english_name("ja"), "Japanese");
        assert_eq!(get_english_name("ja-JP"), "Japanese"); // 模糊匹配
        assert_eq!(get_english_name("xx"), "Unknown");
    }

    #[test]
    fn test_get_lingua_name() {
        assert_eq!(get_lingua_name("zh"), "chinese");
        assert_eq!(get_lingua_name("en-us"), "english");
        assert_eq!(get_lingua_name("ja"), "japanese");
        assert_eq!(get_lingua_name("xx"), "xx"); // 未知语言返回小写原始代码
    }

    #[test]
    fn test_find_bcp47_by_lingua_name() {
        // 测试 Lingua 返回的语言名（首字母大写）映射到 BCP-47
        assert_eq!(find_bcp47_by_lingua_name("English"), Some("en-us"));
        assert_eq!(find_bcp47_by_lingua_name("Chinese"), Some("zh"));
        assert_eq!(find_bcp47_by_lingua_name("Japanese"), Some("ja"));
        assert_eq!(find_bcp47_by_lingua_name("Korean"), Some("ko"));
        assert_eq!(find_bcp47_by_lingua_name("Spanish"), Some("es"));
        assert_eq!(find_bcp47_by_lingua_name("French"), Some("fr"));
        assert_eq!(find_bcp47_by_lingua_name("German"), Some("de"));

        // 测试小写也能匹配
        assert_eq!(find_bcp47_by_lingua_name("english"), Some("en-us"));
        assert_eq!(find_bcp47_by_lingua_name("chinese"), Some("zh"));

        // 测试未知语言
        assert_eq!(find_bcp47_by_lingua_name("Unknown"), None);
        assert_eq!(find_bcp47_by_lingua_name("xyz"), None);
    }

    #[test]
    fn test_get_example() {
        let (name, phrase) = get_example("zh").unwrap();
        assert_eq!(name, "中文");
        assert_eq!(phrase, "你是谁");

        let (name, phrase) = get_example("en-us").unwrap();
        assert_eq!(name, "English");
        assert_eq!(phrase, "Who are you");
    }

    #[test]
    fn test_all_32_languages() {
        // 验证所有 32 种语言都已配置
        let expected_codes = vec![
            "zh", "zh-hk", "en-us", "en-uk", "en-au", "en-in", "ja", "ko", "vi", "id", "th", "hi", "es", "fr", "de", "pt-pt", "pt-br", "it", "ru", "tr", "uk", "pl", "nl", "el", "ro", "cs", "fi",
            "ar", "sv", "no", "da", "af",
        ];

        for code in expected_codes {
            assert!(find_language(code).is_some(), "Language {} not found", code);
        }
    }
}
