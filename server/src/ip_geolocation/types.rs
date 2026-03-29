//! Data types for IP geolocation module

use serde::{Deserialize, Serialize};

/// Represents an IP address that can be either IPv4 or IPv6
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum IpAddress {
    /// IPv4 address
    V4(std::net::Ipv4Addr),
    /// IPv6 address
    V6(std::net::Ipv6Addr),
}

impl From<std::net::IpAddr> for IpAddress {
    fn from(ip: std::net::IpAddr) -> Self {
        match ip {
            std::net::IpAddr::V4(ipv4) => IpAddress::V4(ipv4),
            std::net::IpAddr::V6(ipv6) => IpAddress::V6(ipv6),
        }
    }
}

impl From<&str> for IpAddress {
    fn from(s: &str) -> Self {
        match s.parse::<std::net::IpAddr>() {
            Ok(ip) => ip.into(),
            Err(_) => panic!("Invalid IP address"),
        }
    }
}

impl std::fmt::Display for IpAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpAddress::V4(ipv4) => write!(f, "{}", ipv4),
            IpAddress::V6(ipv6) => write!(f, "{}", ipv6),
        }
    }
}

/// Structured geolocation data returned by the lookup
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoLocation {
    /// IP address that was looked up
    pub ip: String,

    /// Country name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,

    /// Region/State name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,

    /// City name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,

    /// Latitude coordinate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,

    /// Longitude coordinate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,

    /// Timezone identifier
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,

    /// ISP/Organization name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isp: Option<String>,

    /// Autonomous System Number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asn: Option<u32>,

    /// Organization name associated with ASN
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
}
