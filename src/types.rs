use tondi_utils::networking::{IpAddress as TondiIpAddress, NetAddress as TondiNetAddress};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

/// Network address, wrapping rusty-tondi's NetAddress
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct NetAddress {
    pub ip: IpAddr,
    pub port: u16,
}

impl NetAddress {
    pub fn new(ip: IpAddr, port: u16) -> Self {
        Self { ip, port }
    }

    pub fn from_tondi(tondi_addr: &TondiNetAddress) -> Self {
        Self {
            ip: tondi_addr.ip.0,
            port: tondi_addr.port,
        }
    }

    pub fn to_tondi(&self) -> TondiNetAddress {
        TondiNetAddress {
            ip: TondiIpAddress::new(self.ip),
            port: self.port,
        }
    }
}

/// Network address extension traits
pub trait NetAddressExt {
    fn to_string(&self) -> String;
    fn is_ipv4(&self) -> bool;
    fn is_ipv6(&self) -> bool;
}

impl NetAddressExt for NetAddress {
    fn to_string(&self) -> String {
        format!("{}:{}", self.ip, self.port)
    }

    fn is_ipv4(&self) -> bool {
        self.ip.is_ipv4()
    }

    fn is_ipv6(&self) -> bool {
        self.ip.is_ipv6()
    }
}

/// Version message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionMessage {
    pub protocol_version: u32,
    pub user_agent: String,
    pub timestamp: u64,
    pub nonce: u64,
}

/// Address entry message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddressesMessage {
    pub addresses: Vec<NetAddress>,
}

/// Request address message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestAddressesMessage {
    pub include_all_subnetworks: bool,
    pub subnetwork_id: Option<String>,
}

/// Network message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkMessage {
    pub command: String,
    pub payload: Vec<u8>,
}

impl NetworkMessage {
    pub fn version(version_msg: &VersionMessage) -> Self {
        let payload = bincode::serialize(version_msg).unwrap_or_else(|_| Vec::new());
        Self {
            command: "version".to_string(),
            payload,
        }
    }

    pub fn request_addresses(request: &RequestAddressesMessage) -> Self {
        let payload = bincode::serialize(request).unwrap_or_else(|_| Vec::new());
        Self {
            command: "getaddr".to_string(),
            payload,
        }
    }
}

/// Crawler statistics
#[derive(Debug, Serialize, Deserialize)]
pub struct CrawlerStats {
    pub total_nodes: AtomicU64,
    pub active_nodes: AtomicU64,
    pub failed_connections: AtomicU64,
    pub successful_connections: AtomicU64,
    pub last_update: SystemTime,
}

impl Default for CrawlerStats {
    fn default() -> Self {
        Self {
            total_nodes: AtomicU64::new(0),
            active_nodes: AtomicU64::new(0),
            failed_connections: AtomicU64::new(0),
            successful_connections: AtomicU64::new(0),
            last_update: SystemTime::now(),
        }
    }
}

impl CrawlerStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn increment_failed_connections(&self) {
        self.failed_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_successful_connections(&self) {
        self.successful_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn update_total_nodes(&self, count: u64) {
        self.total_nodes.store(count, Ordering::Relaxed);
    }

    pub fn update_active_nodes(&self, count: u64) {
        self.active_nodes.store(count, Ordering::Relaxed);
    }

    pub fn update_last_update(&mut self) {
        self.last_update = SystemTime::now();
    }
}

/// DNS record type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsRecord {
    pub name: String,
    pub record_type: u16,
    pub ttl: u32,
    pub data: String,
}

/// Address entry (for backward compatibility)
pub type AddressEntry = NetAddress;

/// Node information (for backward compatibility)
pub type NodeInfo = NetAddress;
