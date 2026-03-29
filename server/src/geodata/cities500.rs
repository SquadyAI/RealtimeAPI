use once_cell::sync::OnceCell;
use rustc_hash::FxHashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use tracing::{info, warn};

#[derive(Clone, Debug)]
pub struct CityRecord {
    pub name: String,
    pub asciiname: String,
    pub timezone: String,
    pub country_code: String,
    pub population: u64,
}

struct CityIndex {
    map: FxHashMap<String, Vec<CityRecord>>, // normalized city name -> candidates
}

static CITY_INDEX: OnceCell<CityIndex> = OnceCell::new();

fn normalize_city_name(name: &str) -> String {
    let mut s = name.trim().to_ascii_lowercase();
    s = s.trim_end_matches('市').to_string();
    s.replace(['\t', '\n', '\r'], " ").trim().to_string()
}

fn load_city_index() -> Option<CityIndex> {
    let path = std::env::var("GEONAMES_CITIES_FILE").unwrap_or_else(|_| "data/cities500.txt".to_string());
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(_) => {
            warn!("未找到 GeoNames 数据文件: {}，将回退内置映射", path);
            return None;
        },
    };

    let reader = BufReader::new(file);
    let mut map: FxHashMap<String, Vec<CityRecord>> = FxHashMap::default();

    for line in reader.lines().map_while(Result::ok) {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 19 {
            continue;
        }

        let name = parts[1];
        let asciiname = parts[2];
        let alternatenames = parts[3];
        let country_code = parts[8];
        let population = parts[14].parse::<u64>().unwrap_or(0);
        let timezone = parts[17];

        if timezone.is_empty() {
            continue;
        }

        let mut candidates: Vec<&str> = Vec::with_capacity(8);
        candidates.push(name);
        if !asciiname.is_empty() {
            candidates.push(asciiname);
        }
        if !alternatenames.is_empty() {
            for alt in alternatenames.split(',') {
                if alt.len() <= 64 && !alt.is_empty() {
                    candidates.push(alt);
                }
            }
        }

        let english_name = if !asciiname.is_empty() { asciiname } else { name };
        let entry_template = CityRecord {
            name: name.to_string(),
            asciiname: english_name.to_string(),
            timezone: timezone.to_string(),
            country_code: country_code.to_string(),
            population,
        };

        for alias in candidates {
            let key = normalize_city_name(alias);
            if key.is_empty() {
                continue;
            }
            map.entry(key).or_default().push(entry_template.clone());
        }
    }

    info!("GeoNames 城市索引已加载，键数: {}", map.len());
    Some(CityIndex { map })
}

fn get_city_index() -> Option<&'static CityIndex> {
    if let Some(idx) = CITY_INDEX.get() {
        return Some(idx);
    }
    match CITY_INDEX.set(load_city_index()?) {
        Ok(_) => CITY_INDEX.get(),
        Err(_) => CITY_INDEX.get(),
    }
}

fn pick_best_candidate(mut candidates: Vec<CityRecord>, country_code: Option<&str>) -> Option<CityRecord> {
    if let Some(cc) = country_code {
        let cc_upper = cc.trim().to_ascii_uppercase();
        // Avoid moving `candidates` before final selection. First check if any matches exist,
        // then narrow in-place so we can still consume `candidates` once at the end.
        let has_match = candidates.iter().any(|e| e.country_code == cc_upper);
        if has_match {
            candidates.retain(|e| e.country_code == cc_upper);
        }
    }

    candidates.into_iter().max_by_key(|e| e.population)
}

fn resolve_city(city: &str, country_code: Option<&str>) -> Option<CityRecord> {
    let idx = get_city_index()?;
    let key = normalize_city_name(city);
    let candidates = idx.map.get(&key)?.clone();
    pick_best_candidate(candidates, country_code)
}

pub fn resolve_timezone(city: &str, country_code: Option<&str>) -> Option<String> {
    resolve_city(city, country_code).map(|c| c.timezone)
}

pub fn get_english_name(city: &str) -> Option<String> {
    resolve_city(city, None).map(|c| if c.asciiname.is_empty() { c.name } else { c.asciiname })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_english_name_ok() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("cities500.txt");
        // 测试代码中设置环境变量是安全的
        // unsafe 是必要的，因为 std::env::set_var 不是线程安全的
        unsafe {
            std::env::set_var("GEONAMES_CITIES_FILE", &path);
        }

        let shenzhen = get_english_name("深圳").expect("missing Shenzhen");
        assert_eq!(shenzhen, "Shenzhen");

        let beijing_en = get_english_name("北京").expect("missing Beijing");
        assert_eq!(beijing_en, "Beijing");
    }
}
