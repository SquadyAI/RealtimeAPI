pub mod location_cleaner;
pub mod lookup;
pub mod types;

/// Re-export IpGeoLocator to make it accessible from outside
pub use lookup::IpGeoLocator;
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};

/// Re-export IpAddress and GeoLocation types
pub use types::{GeoLocation, IpAddress};

/// Re-export location cleaner function
pub use location_cleaner::clean_location;

/// Configuration for IP Geolocation service
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IpGeolocationConfig {
    /// Path to the MaxMind MMDB file
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mmdb_path: Option<String>,
}

/// Global IP Geolocation service instance
static IP_GEOLOCATION_SERVICE: OnceCell<IpGeoLocator> = OnceCell::new();

/// Initialize the IP Geolocation service
///
/// This function attempts to initialize the global IP Geolocation service
/// with the provided configuration. If initialization fails, it logs a warning
/// but does not prevent the system from starting.
///
/// # Arguments
/// * `config` - Optional IP Geolocation configuration
pub fn init_ip_geolocation_service(config: Option<IpGeolocationConfig>) {
    if let Some(config) = config
        && let Some(mmdb_path) = config.mmdb_path
    {
        match IpGeoLocator::new(&mmdb_path) {
            Ok(locator) => {
                if IP_GEOLOCATION_SERVICE.set(locator).is_err() {
                    tracing::warn!("IP Geolocation service was already initialized");
                } else {
                    tracing::info!("IP Geolocation service initialized with MMDB file: {}", mmdb_path);
                }
            },
            Err(e) => {
                tracing::warn!(
                    "Failed to initialize IP Geolocation service with MMDB file '{}': {}",
                    mmdb_path,
                    e
                );
            },
        }
    }
}

/// Get a reference to the global IP Geolocation service
///
/// Returns None if the service has not been initialized or if initialization failed.
pub fn get_ip_geolocation_service() -> Option<&'static IpGeoLocator> {
    IP_GEOLOCATION_SERVICE.get()
}
// Re-export the main structs and functions for easier access
pub use lookup::LookupResult;
