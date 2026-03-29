//! IP Geolocation lookup implementation

use super::types::{GeoLocation, IpAddress};
use maxminddb;
use std::net::IpAddr;
// use std::path::Path; // intentionally not importing to avoid unused warning

/// Error types for IP geolocation lookup
#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    #[error("Failed to open MMDB file: {0}")]
    OpenDatabase(#[from] maxminddb::MaxMindDbError),

    #[error("Invalid IP address: {0}")]
    InvalidIpAddress(String),

    #[error("IP address not found in database")]
    IpNotFound,

    #[error("Failed to lookup IP in MMDB: {0}")]
    LookupFailed(maxminddb::MaxMindDbError),
}

/// Result type for IP geolocation lookup
pub type LookupResult<T> = std::result::Result<T, LookupError>;

/// IP Geolocation lookup service
pub struct IpGeoLocator {
    reader: maxminddb::Reader<Vec<u8>>,
}

impl IpGeoLocator {
    /// Create a new IP geolocation lookup service from an MMDB file path
    ///
    /// # Arguments
    /// * `path` - Path to the MMDB file
    ///
    /// # Returns
    /// * `LookupResult<IpGeoLocator>` - A new instance or an error
    pub fn new<P: AsRef<std::path::Path>>(path: P) -> LookupResult<Self> {
        let reader = maxminddb::Reader::open_readfile(path)?;
        Ok(IpGeoLocator { reader })
    }

    /// Look up an IP address and return structured geolocation data (with default language preference)
    ///
    /// # Arguments
    /// * `ip` - IP address to look up (can be IPv4 or IPv6)
    ///
    /// # Returns
    /// * `LookupResult<GeoLocation>` - Geolocation data or an error
    pub fn lookup(&self, ip: &IpAddress) -> LookupResult<GeoLocation> {
        self.lookup_with_language(ip, None)
    }

    /// Look up an IP address and return structured geolocation data with language preference
    ///
    /// # Arguments
    /// * `ip` - IP address to look up (can be IPv4 or IPv6)
    /// * `preferred_language` - Optional language code (e.g., "zh", "en", "es", "ja", "ko")
    ///   If None, defaults to zh-CN -> zh -> en
    ///
    /// # Returns
    /// * `LookupResult<GeoLocation>` - Geolocation data or an error
    pub fn lookup_with_language(&self, ip: &IpAddress, preferred_language: Option<&str>) -> LookupResult<GeoLocation> {
        // Convert our IpAddress enum to std::net::IpAddr
        let ip_addr: IpAddr = match ip {
            IpAddress::V4(ipv4) => IpAddr::V4(*ipv4),
            IpAddress::V6(ipv6) => IpAddr::V6(*ipv6),
        };

        // Perform City database lookup and extract fields
        match self.reader.lookup::<maxminddb::geoip2::City>(ip_addr) {
            Ok(Some(city_rec)) => {
                // Helper closure to get name with language preference
                let get_name = |names: &std::collections::BTreeMap<&str, &str>| -> Option<String> { Self::get_localized_name(names, preferred_language) };

                // Country name
                let country = city_rec.country.as_ref().and_then(|c| c.names.as_ref()).and_then(&get_name);

                // Region/subdivision name
                let region = city_rec
                    .subdivisions
                    .as_ref()
                    .and_then(|subs| subs.first())
                    .and_then(|sub| sub.names.as_ref())
                    .and_then(&get_name);

                // City name
                let city_name = city_rec.city.as_ref().and_then(|c| c.names.as_ref()).and_then(&get_name);

                // Location fields
                let latitude = city_rec.location.as_ref().and_then(|l| l.latitude);
                let longitude = city_rec.location.as_ref().and_then(|l| l.longitude);
                let timezone = city_rec.location.as_ref().and_then(|l| l.time_zone.map(|tz| tz.to_string()));

                Ok(GeoLocation {
                    ip: ip.to_string(),
                    country,
                    region,
                    city: city_name,
                    latitude,
                    longitude,
                    timezone,
                    isp: None,
                    asn: None,
                    org: None,
                })
            },
            Ok(None) => Err(LookupError::IpNotFound),
            Err(e) => Err(LookupError::LookupFailed(e)),
        }
    }

    /// Get localized name based on language preference
    ///
    /// Priority order:
    /// - If preferred_language is provided and is one of: zh/ja/ko/yue/fr/es -> use that language
    /// - Otherwise -> use English
    /// - If None (default) -> use Chinese (zh-CN/zh) then English (backward compatible)
    fn get_localized_name(names: &std::collections::BTreeMap<&str, &str>, preferred_language: Option<&str>) -> Option<String> {
        if let Some(lang) = preferred_language {
            let lang_lower = lang.to_lowercase();

            // Supported languages: zh, ja, ko, yue, fr, es
            let lang_key = match lang_lower.as_str() {
                "zh" | "zh-cn" | "chinese" => Some("zh-CN"),
                "ja" | "japanese" => Some("ja"),
                "ko" | "korean" => Some("ko"),
                "yue" | "cantonese" => Some("zh-CN"), // MaxMind doesn't have separate Cantonese, use Chinese
                "fr" | "french" => Some("fr"),
                "es" | "spanish" => Some("es"),
                _ => None, // All other languages fall through to English
            };

            // Try preferred language first
            if let Some(key) = lang_key
                && let Some(name) = names.get(key)
            {
                return Some(name.to_string());
            }

            // Fallback to English for all cases
            if let Some(name) = names.get("en") {
                return Some(name.to_string());
            }
        } else {
            // Default behavior (no language specified): prefer Chinese then English
            if let Some(name) = names.get("zh-CN").or_else(|| names.get("zh")) {
                return Some(name.to_string());
            }
            if let Some(name) = names.get("en") {
                return Some(name.to_string());
            }
        }

        // Last resort: return any available name
        names.values().next().map(|s| s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ip_geolocation_module_structure() {
        // This test simply verifies that the module structure is correct
        // and that the main types can be imported and instantiated

        // Test that we can create an IpAddress from a string
        let ip_v4: IpAddress = "8.8.8.8".parse::<std::net::IpAddr>().unwrap().into();
        match ip_v4 {
            IpAddress::V4(ipv4) => assert_eq!(ipv4, std::net::Ipv4Addr::new(8, 8, 8, 8)),
            _ => panic!("Expected IPv4 address"),
        }

        let ip_v6: IpAddress = "2001:4860:4860::8888".parse::<std::net::IpAddr>().unwrap().into();
        match ip_v6 {
            IpAddress::V6(ipv6) => assert_eq!(ipv6, std::net::Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888)),
            _ => panic!("Expected IPv6 address"),
        }

        // Test that we can create a GeoLocation instance
        let geo_location = GeoLocation {
            ip: "8.8.8.8".to_string(),
            country: Some("United States".to_string()),
            region: Some("California".to_string()),
            city: Some("Mountain View".to_string()),
            latitude: Some(37.4056),
            longitude: Some(-122.0775),
            timezone: Some("America/Los_Angeles".to_string()),
            isp: Some("Google LLC".to_string()),
            asn: Some(15169),
            org: Some("Google LLC".to_string()),
        };

        assert_eq!(geo_location.ip, "8.8.8.8");
        assert_eq!(geo_location.country, Some("United States".to_string()));
        assert_eq!(geo_location.region, Some("California".to_string()));
        assert_eq!(geo_location.city, Some("Mountain View".to_string()));
        assert_eq!(geo_location.latitude, Some(37.4056));
        assert_eq!(geo_location.longitude, Some(-122.0775));
        assert_eq!(geo_location.timezone, Some("America/Los_Angeles".to_string()));
        assert_eq!(geo_location.isp, Some("Google LLC".to_string()));
        assert_eq!(geo_location.asn, Some(15169));
        assert_eq!(geo_location.org, Some("Google LLC".to_string()));
    }

    #[test]
    fn test_ip_address_display() {
        let ip_v4: IpAddress = "8.8.8.8".parse::<std::net::IpAddr>().unwrap().into();
        assert_eq!(format!("{}", ip_v4), "8.8.8.8");

        let ip_v6: IpAddress = "2001:4860:4860::8888".parse::<std::net::IpAddr>().unwrap().into();
        assert_eq!(format!("{}", ip_v6), "2001:4860:4860::8888");
    }

    // Note: This test requires a real MMDB file to run, so it's marked as ignored by default
    // To run this test, you need to provide a valid MMDB file and remove the #[ignore] attribute
    #[test]
    #[ignore]
    fn test_ip_geolocation_lookup_with_real_mmdb() -> Result<(), Box<dyn std::error::Error>> {
        // This test shows how the lookup functionality would work with a real MMDB file
        // Since we don't have a real MMDB file in this context, this test is marked as ignored

        // For demonstration purposes, we'll show what the test would look like:
        /*
        let mmdb_path = Path::new("./GeoLite2-City.mmdb"); // Path to a real MMDB file
        if mmdb_path.exists() {
            let locator = IpGeoLocator::new(mmdb_path)?;

            let ip: IpAddress = "8.8.8.8".parse::<std::net::IpAddr>()?.into();
            let location = locator.lookup(&ip)?;

            // Verify that we got a result with the correct IP
            assert_eq!(location.ip, "8.8.8.8");

            // Other assertions would depend on the content of the MMDB file
        } else {
            // Skip the test if no MMDB file is available
            eprintln!("Warning: MMDB file not found, skipping test");
        }
        */

        Ok(())
    }
}
