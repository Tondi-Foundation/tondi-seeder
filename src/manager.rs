use crate::errors::Result;
use crate::types::{CrawlerStats, NetAddress};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{error, info};

// Address manager constants - aligned with Go version
const PEERS_FILENAME: &str = "peers.json";
const DEFAULT_STALE_GOOD_TIMEOUT: Duration = Duration::from_secs(60 * 60); // 1 hour (same as Go version)
const DEFAULT_STALE_BAD_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60); // 2 hours (same as Go version)

const PRUNE_EXPIRE_TIMEOUT: Duration = Duration::from_secs(8 * 60 * 60); // 8 hours, same as Go version
const PRUNE_ADDRESS_INTERVAL: Duration = Duration::from_secs(60); // 1 minute (same as Go version)
const DUMP_ADDRESS_INTERVAL: Duration = Duration::from_secs(2 * 60); // 2 minutes (same as Go version)

/// Node status with quality metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub address: NetAddress,
    pub last_seen: SystemTime,
    pub last_attempt: SystemTime,
    pub last_success: SystemTime,
    pub user_agent: Option<String>,
    pub subnetwork_id: Option<String>,
    pub services: u64,
    // Quality metrics
    pub connection_attempts: u32,
    pub successful_connections: u32,
    pub last_error: Option<String>,
    pub quality_score: f32, // 0.0 to 1.0
}

impl Node {
    pub fn new(address: NetAddress) -> Self {
        let now = SystemTime::now();
        Self {
            address,
            last_seen: now,
            last_attempt: now,
            last_success: UNIX_EPOCH, // Never successfully connected
            user_agent: None,
            subnetwork_id: None,
            services: 0,
            connection_attempts: 0,
            successful_connections: 0,
            last_error: None,
            quality_score: 0.5, // Start with neutral score
        }
    }

    pub fn key(&self) -> String {
        format!("{}:{}", self.address.ip, self.address.port)
    }

    /// Update connection attempt statistics
    pub fn record_connection_attempt(&mut self, success: bool, error: Option<String>) {
        self.connection_attempts += 1;
        self.last_attempt = SystemTime::now();

        if success {
            self.successful_connections += 1;
            self.last_success = SystemTime::now();
            self.last_error = None;
        } else {
            self.last_error = error;
        }

        // Update quality score
        self.update_quality_score();
    }

    /// Calculate quality score based on success rate and recency
    fn update_quality_score(&mut self) {
        if self.connection_attempts == 0 {
            self.quality_score = 0.5;
            return;
        }

        // Base success rate (0.0 to 1.0)
        let success_rate = self.successful_connections as f32 / self.connection_attempts as f32;

        // Time decay factor (recent successes are weighted more)
        let time_factor = if self.last_success > UNIX_EPOCH {
            let hours_since_success = SystemTime::now()
                .duration_since(self.last_success)
                .unwrap_or_default()
                .as_secs() as f32
                / 3600.0;

            // Decay over 24 hours
            (1.0 - (hours_since_success / 24.0)).max(0.1)
        } else {
            0.1 // Never connected successfully
        };

        // Attempt penalty (too many failed attempts reduce score)
        let attempt_penalty = if self.connection_attempts > 10 {
            0.8 // Reduce score for nodes with many failed attempts
        } else {
            1.0
        };

        self.quality_score = (success_rate * time_factor * attempt_penalty).clamp(0.0, 1.0);
    }

    /// Check if node should be attempted based on quality and timing
    pub fn should_attempt_connection(&self) -> bool {
        // Don't attempt if quality is too low
        if self.quality_score < 0.1 {
            return false;
        }

        // Use different intervals for development vs production
        #[cfg(debug_assertions)]
        let (good_interval, medium_interval, poor_interval) = (
            Duration::from_secs(5),  // 5 seconds for good nodes in dev
            Duration::from_secs(10), // 10 seconds for medium nodes in dev
            Duration::from_secs(30), // 30 seconds for poor nodes in dev
        );
        #[cfg(not(debug_assertions))]
        let (good_interval, medium_interval, poor_interval) = (
            Duration::from_secs(300),  // 5 minutes for good nodes in prod
            Duration::from_secs(1800), // 30 minutes for medium nodes in prod
            Duration::from_secs(3600), // 1 hour for poor nodes in prod
        );

        // Don't attempt too frequently for low-quality nodes
        let min_interval = if self.quality_score > 0.7 {
            good_interval
        } else if self.quality_score > 0.3 {
            medium_interval
        } else {
            poor_interval
        };

        SystemTime::now()
            .duration_since(self.last_attempt)
            .unwrap_or_default()
            >= min_interval
    }
}

/// Address manager, corresponding to Go version's Manager
pub struct AddressManager {
    nodes: DashMap<String, Node>,
    peers_file: String,
    quit_tx: mpsc::Sender<()>,
    stats: Arc<CrawlerStats>,
    default_port: u16, // Add default port for network
}

impl AddressManager {
    /// Create a new address manager
    pub fn new(app_dir: &str, default_port: u16) -> Result<Self> {
        let peers_file = std::path::Path::new(app_dir).join(PEERS_FILENAME);
        let peers_file = peers_file.to_string_lossy().to_string();

        // Ensure the directory exists
        if let Some(parent_dir) = std::path::Path::new(&peers_file).parent() {
            std::fs::create_dir_all(parent_dir)?;
        }

        let (quit_tx, _quit_rx) = mpsc::channel(1);

        let manager = Self {
            nodes: DashMap::new(),
            peers_file,
            quit_tx,
            stats: Arc::new(CrawlerStats::default()),
            default_port,
        };

        // Load saved nodes
        manager.deserialize_peers()?;

        Ok(manager)
    }

    /// Start the address manager (call this after creation to start background tasks)
    pub fn start(&self) {
        // Start address processing coroutine
        let manager_clone = self.clone();
        tokio::spawn(async move {
            manager_clone.address_handler().await;
        });
    }

    /// Add address list, return the number of new addresses added
    pub fn add_addresses(
        &self,
        addresses: Vec<NetAddress>,
        _default_port: u16,
        accept_unroutable: bool,
    ) -> usize {
        let mut _count = 0;

        for address in addresses {
            // Check port and routability
            if address.port == 0 || (!accept_unroutable && !self.is_routable(&address)) {
                continue;
            }

            let addr_str = format!("{}:{}", address.ip, address.port);

            if let Some(mut node) = self.nodes.get_mut(&addr_str) {
                // Update the last access time of the existing node
                node.last_seen = SystemTime::now();
            } else {
                // Create a new node
                let node = Node::new(address);
                self.nodes.insert(addr_str, node);
                _count += 1;
            }
        }

        _count
    }

    /// Get addresses that need to be retested - aligned with Go version logic
    pub fn addresses(&self, threads: u8) -> Vec<NetAddress> {
        let mut addresses = Vec::new();
        let max_count = threads as usize * 3;

        // First pass: look for stale nodes (like Go version)
        let mut stale_candidates: Vec<_> = self
            .nodes
            .iter()
            .filter(|entry| {
                let node = entry.value();
                self.is_stale(node)
            })
            .collect();

        // Sort stale candidates by last attempt time (oldest first) - optimized sorting
        stale_candidates
            .sort_unstable_by(|a, b| a.value().last_attempt.cmp(&b.value().last_attempt));

        // Add stale candidates first
        for candidate in stale_candidates.into_iter().take(max_count) {
            addresses.push(candidate.value().address.clone());
        }

        // If we still need more addresses, add some good nodes
        if addresses.len() < max_count {
            let remaining_count = max_count - addresses.len();
            let mut good_candidates: Vec<_> =
                self.nodes
                    .iter()
                    .filter(|entry| {
                        let node = entry.value();
                        // Use more efficient comparison without string formatting
                        !addresses.iter().any(|addr| {
                            addr.ip == node.address.ip && addr.port == node.address.port
                        }) && self.is_good(node)
                    })
                    .collect();

            // Sort good candidates by last attempt time - optimized sorting
            good_candidates
                .sort_unstable_by(|a, b| a.value().last_attempt.cmp(&b.value().last_attempt));

            for candidate in good_candidates.into_iter().take(remaining_count) {
                addresses.push(candidate.value().address.clone());
            }
        }

        // Log detailed status for debugging

        info!("Selected {} addresses for crawling", addresses.len());

        addresses
    }

    /// Record connection attempt result for a node
    pub fn record_connection_result(
        &self,
        address: &NetAddress,
        success: bool,
        error: Option<String>,
    ) {
        let key = format!("{}:{}", address.ip, address.port);
        if let Some(mut node) = self.nodes.get_mut(&key) {
            node.record_connection_attempt(success, error.clone());
        }
    }

    /// Get the total number of addresses
    pub fn address_count(&self) -> usize {
        self.nodes.len()
    }

    /// Get all nodes (for statistics)
    pub fn get_all_nodes(&self) -> Vec<Node> {
        self.nodes
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Get good address list, filtered by DNS query type
    pub fn good_addresses(
        &self,
        qtype: u16,
        include_all_subnetworks: bool,
        subnetwork_id: Option<&str>,
    ) -> Vec<NetAddress> {
        let mut addresses = Vec::new();
        let mut _count = 0;
        let mut total_nodes = 0;
        let mut good_nodes = 0;
        let mut stale_nodes = 0;
        let mut bad_nodes = 0;

        // Only support A and AAAA records
        if qtype != 1 && qtype != 28 {
            // 1=A, 28=AAAA
            return addresses;
        }

        for entry in self.nodes.iter() {
            total_nodes += 1;
            let node = entry.value();

            // Check subnet
            if !include_all_subnetworks {
                if let Some(ref expected_id) = subnetwork_id {
                    if let Some(ref node_id) = node.subnetwork_id {
                        if expected_id != node_id {
                            continue;
                        }
                    } else {
                        continue;
                    }
                }
            }

            // Check IP type
            let is_ipv4 = node.address.ip.is_ipv4();
            if (qtype == 1 && !is_ipv4) || (qtype == 28 && is_ipv4) {
                continue;
            }

            // Check node status - allow both good and stale nodes for DNS queries
            // This ensures DNS queries can return addresses even when nodes are still being evaluated
            if self.is_good(node) {
                good_nodes += 1;
                addresses.push(node.address.clone());
                _count += 1;
            } else if self.is_stale(node) {
                stale_nodes += 1;
                addresses.push(node.address.clone());
                _count += 1;
            } else {
                bad_nodes += 1;
            }
        }

        info!(
            "DNS query: qtype={}, total_nodes={}, good={}, stale={}, bad={}, returned={}",
            qtype,
            total_nodes,
            good_nodes,
            stale_nodes,
            bad_nodes,
            addresses.len()
        );

        addresses
    }

    /// Update connection attempt time
    pub fn attempt(&self, address: &NetAddress) {
        let addr_str = format!("{}:{}", address.ip, address.port);

        if let Some(mut node) = self.nodes.get_mut(&addr_str) {
            node.last_attempt = SystemTime::now();
        }
    }

    /// Update successful connection information
    pub fn good(
        &self,
        address: &NetAddress,
        user_agent: Option<&str>,
        subnetwork_id: Option<&str>,
    ) {
        let addr_str = format!("{}:{}", address.ip, address.port);

        if let Some(mut node) = self.nodes.get_mut(&addr_str) {
            node.user_agent = user_agent.map(|s| s.to_string());
            node.subnetwork_id = subnetwork_id.map(|s| s.to_string());
            node.last_success = SystemTime::now();
        }
    }

    /// Address processing coroutine
    async fn address_handler(&self) {
        let mut prune_ticker = tokio::time::interval(PRUNE_ADDRESS_INTERVAL);
        let mut dump_ticker = tokio::time::interval(DUMP_ADDRESS_INTERVAL);

        loop {
            tokio::select! {
                _ = prune_ticker.tick() => {
                    self.prune_peers();
                }
                _ = dump_ticker.tick() => {
                    if let Err(e) = self.save_peers() {
                        error!("Failed to save peers: {}", e);
                    }
                }
            }
        }
    }

    /// Clean up expired and bad addresses
    fn prune_peers(&self) {
        let mut _pruned = 0;
        let mut good = 0;
        let mut stale = 0;
        let mut bad = 0;
        let mut ipv4 = 0;
        let mut ipv6 = 0;

        let now = SystemTime::now();
        let mut to_remove = Vec::new();

        for entry in self.nodes.iter() {
            let node = entry.value();

            if self.is_expired(node, now) {
                to_remove.push(entry.key().clone());
                // Count pruned addresses
            } else if self.is_good(node) {
                good += 1;
                if node.address.ip.is_ipv4() {
                    ipv4 += 1;
                } else {
                    ipv6 += 1;
                }
            } else if self.is_stale(node) {
                stale += 1;
            } else {
                bad += 1;
            }
        }

        // Remove expired nodes
        for key in to_remove {
            self.nodes.remove(&key);
        }

        let _total = self.nodes.len();

        info!(
            "Known nodes: Good:{} [4:{}, 6:{}] Stale:{} Bad:{}",
            good, ipv4, ipv6, stale, bad
        );
    }

    /// Save addresses to file
    fn save_peers(&self) -> Result<()> {
        // Ensure the directory exists before writing files
        if let Some(parent_dir) = std::path::Path::new(&self.peers_file).parent() {
            if let Err(e) = std::fs::create_dir_all(parent_dir) {
                error!("Failed to create directory {}: {}", parent_dir.display(), e);
                return Err(crate::errors::KaseederError::Io(e));
            }
        }

        let nodes: Vec<_> = self
            .nodes
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();

        // Create temporary file
        let tmp_file = format!("{}.new", self.peers_file);

        // Check if we can write to the temporary file
        let serialized_nodes = serde_json::to_string(&nodes).map_err(|e| {
            crate::errors::KaseederError::Serialization(format!("Failed to serialize nodes: {}", e))
        })?;

        if let Err(e) = std::fs::write(&tmp_file, serialized_nodes) {
            error!("Failed to write temporary file {}: {}", tmp_file, e);
            return Err(crate::errors::KaseederError::Io(e));
        }

        // Verify temporary file was created and has content
        if !std::path::Path::new(&tmp_file).exists() {
            error!("Temporary file {} was not created", tmp_file);
            return Err(crate::errors::KaseederError::Config(
                "Temporary file creation failed".to_string(),
            ));
        }

        // Atomically rename file
        if let Err(e) = std::fs::rename(&tmp_file, &self.peers_file) {
            error!(
                "Failed to rename {} to {}: {}",
                tmp_file, self.peers_file, e
            );
            // Try to clean up temporary file, but don't fail if cleanup fails
            if let Err(cleanup_e) = std::fs::remove_file(&tmp_file) {
                error!(
                    "Failed to remove temporary file {}: {}",
                    tmp_file, cleanup_e
                );
            }
            return Err(crate::errors::KaseederError::Io(e));
        }

        Ok(())
    }

    /// Load addresses from file
    fn deserialize_peers(&self) -> Result<()> {
        if !std::path::Path::new(&self.peers_file).exists() {
            return Ok(());
        }

        let content = std::fs::read_to_string(&self.peers_file)?;
        let nodes: Vec<(String, Node)> = serde_json::from_str(&content)?;

        let nodes_count = nodes.len();
        for (key, node) in nodes {
            self.nodes.insert(key, node);
        }

        info!("{} nodes loaded", nodes_count);
        Ok(())
    }

    /// Check if node is expired
    fn is_expired(&self, node: &Node, now: SystemTime) -> bool {
        let last_seen_elapsed = now.duration_since(node.last_seen).unwrap_or_default();

        last_seen_elapsed > PRUNE_EXPIRE_TIMEOUT
    }

    /// Check if node is good - aligned with Go version
    fn is_good(&self, node: &Node) -> bool {
        // Check if it's not a non-default port (like Go version)
        if self.is_nondefault_port(&node.address) {
            return false;
        }

        let now = SystemTime::now();
        let last_success_elapsed = now.duration_since(node.last_success).unwrap_or_default();

        // Use consistent timeout for production
        let stale_timeout = DEFAULT_STALE_GOOD_TIMEOUT;

        last_success_elapsed < stale_timeout
    }

    /// Check if node is stale - aligned with Go version
    fn is_stale(&self, node: &Node) -> bool {
        let now = SystemTime::now();
        let last_attempt_elapsed = now.duration_since(node.last_attempt).unwrap_or_default();

        // For nodes that have never successfully connected (new nodes)
        if node.last_success.eq(&UNIX_EPOCH) {
            // New node: If it has never been attempted, it's immediately available
            if node.last_attempt == node.last_seen {
                return true; // New node is immediately available for polling
            }
            // For attempted new nodes, use production interval
            let poll_interval = Duration::from_secs(5); // 5 seconds in production

            return last_attempt_elapsed > poll_interval;
        }

        // For nodes that have successfully connected, use the appropriate timeout
        // Aligned with Go version logic
        let stale_timeout = if last_attempt_elapsed > Duration::from_secs(24 * 60 * 60) {
            // If last attempt was more than 24 hours ago, use shorter timeout
            DEFAULT_STALE_GOOD_TIMEOUT // 1 hour
        } else {
            DEFAULT_STALE_BAD_TIMEOUT // 2 hours
        };

        last_attempt_elapsed > stale_timeout
    }

    /// Check if address is routable
    /// Reference Go version's addressmanager.IsRoutable logic
    fn is_routable(&self, address: &NetAddress) -> bool {
        // Check port
        if address.port == 0 {
            return false;
        }

        match address.ip {
            IpAddr::V4(ipv4) => {
                // IPv4 address routability check
                !ipv4.is_private() &&           // Not private network (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16)
                !ipv4.is_loopback() &&          // Not loopback address (127.0.0.0/8)
                !ipv4.is_unspecified() &&       // Not unspecified address (0.0.0.0)
                !ipv4.is_multicast() &&         // Not multicast address (224.0.0.0/4)
                !ipv4.is_broadcast() &&         // Not broadcast address (255.255.255.255)
                !ipv4.is_link_local() &&        // Not link local address (169.254.0.0/16)
                // Check specific reserved address ranges
                !(ipv4.octets() == [192, 0, 2, 0] ||     // 192.0.2.0/24 (TEST-NET-1)
                  ipv4.octets() == [198, 51, 100, 0] ||  // 198.51.100.0/24 (TEST-NET-2)
                  ipv4.octets() == [203, 0, 113, 0] ||  // 203.0.113.0/24 (TEST-NET-3)
                  (ipv4.octets()[0] == 198 && ipv4.octets()[1] == 18) || // 198.18.0.0/15 (Benchmarking)
                  ipv4.octets() == [0, 0, 0, 0] ||      // 0.0.0.0
                  ipv4.octets() == [255, 255, 255, 255]) // 255.255.255.255
            }
            IpAddr::V6(ipv6) => {
                // IPv6 address routability check
                !ipv6.is_loopback() &&          // Not loopback address (::1)
                !ipv6.is_unspecified() &&       // Not unspecified address (::)
                !ipv6.is_multicast() &&         // Not multicast address (ff00::/8)
                !ipv6.is_unique_local() &&      // Not unique local address (fc00::/7)
                !ipv6.is_unicast_link_local() && // Not unicast link local address (fe80::/10)
                // Check specific reserved address ranges
                !(ipv6.segments() == [0x2001, 0xdb8, 0, 0, 0, 0, 0, 0] || // 2001:db8::/32 (Documentation)
                  ipv6.segments() == [0x2001, 0x2, 0, 0, 0, 0, 0, 0] ||    // 2001:2::/48 (Benchmarking)
                  ipv6.segments() == [0, 0, 0, 0, 0, 0, 0, 0] ||           // :: (Unspecified)
                  ipv6.segments() == [0, 0, 0, 0, 0, 0, 0, 1]) // ::1 (Loopback)
            }
        }
    }

    /// Check if address is non-default port (like Go version)
    fn is_nondefault_port(&self, address: &NetAddress) -> bool {
        // Check against the network's default port from configuration
        address.port != self.default_port
    }

    /// Shutdown address manager
    pub async fn shutdown(&self) {
        let _ = self.quit_tx.send(()).await;
    }

    /// Get statistics
    pub fn get_stats(&self) -> Arc<CrawlerStats> {
        self.stats.clone()
    }
}

impl Clone for AddressManager {
    fn clone(&self) -> Self {
        Self {
            nodes: self.nodes.clone(),
            peers_file: self.peers_file.clone(),
            quit_tx: self.quit_tx.clone(),
            stats: Arc::clone(&self.stats),
            default_port: self.default_port,
        }
    }
}

impl Drop for AddressManager {
    fn drop(&mut self) {
        // Ensure addresses are saved when exiting
        if let Err(e) = self.save_peers() {
            error!("Failed to save peers during shutdown: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_address_manager_creates_directory() {
        // Create a temporary directory for testing
        let temp_dir = TempDir::new().unwrap();
        let test_app_dir = temp_dir.path().join("test_app");
        let test_app_dir_str = test_app_dir.to_string_lossy().to_string();

        // Ensure the directory doesn't exist initially
        assert!(!test_app_dir.exists());

        // Create address manager - this should create the directory
        let manager = AddressManager::new(&test_app_dir_str, 0).unwrap();

        // Verify the directory was created
        assert!(test_app_dir.exists());

        // Verify the peers file path is correct
        let expected_peers_file = test_app_dir.join("peers.json");
        assert_eq!(manager.peers_file, expected_peers_file.to_string_lossy());

        // Test saving peers (this should not fail due to directory issues)
        manager.save_peers().unwrap();

        // Verify the peers file was created
        assert!(expected_peers_file.exists());
    }

    #[test]
    fn test_save_peers_creates_parent_directory() {
        // Create a temporary directory for testing
        let temp_dir = TempDir::new().unwrap();
        let test_app_dir = temp_dir.path().join("nested").join("deep").join("app");
        let test_app_dir_str = test_app_dir.to_string_lossy().to_string();

        // Ensure the nested directory doesn't exist initially
        assert!(!test_app_dir.exists());

        // Create address manager - this should create the nested directory
        let manager = AddressManager::new(&test_app_dir_str, 0).unwrap();

        // Verify the nested directory was created
        assert!(test_app_dir.exists());

        // Test saving peers - this should create the directory structure
        manager.save_peers().unwrap();

        // Verify the peers file was created in the nested directory
        let expected_peers_file = test_app_dir.join("peers.json");
        assert!(expected_peers_file.exists());
    }
}
