use crate::errors::{KaseederError, Result};
use crate::types::NetAddress;
use tondi_consensus_core::config::Config as ConsensusConfig;
use tondi_core::time::unix_now;
use tondi_p2p_lib::{
    Adaptor, ConnectionInitializer, Hub, IncomingRoute, TondidHandshake, TondidMessagePayloadType,
    PeerKey, Router,
    common::ProtocolError,
    make_message,
    pb::{VersionMessage, tondid_message::Payload, RequestAddressesMessage},
};
use tondi_utils_tower::counters::TowerConnectionCounters;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tonic::async_trait;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// DNS seeder connection initializer, specifically for address collection
pub struct KaseederConnectionInitializer {
    version_message: VersionMessage,
    addresses_tx: mpsc::Sender<Vec<NetAddress>>,
}

impl KaseederConnectionInitializer {
    pub fn new(
        consensus_config: &ConsensusConfig,
        addresses_tx: mpsc::Sender<Vec<NetAddress>>,
    ) -> Self {
        let version_message = VersionMessage {
            protocol_version: 0, // Use 0 for auto-negotiation (like Go version)
            services: 0,
            timestamp: unix_now() as i64,
            address: None,
            id: Vec::from(Uuid::new_v4().as_bytes()),
            user_agent: "/tondi-seeder:1.0.0/".to_string(), // Match tondi standard format
            disable_relay_tx: true,
            subnetwork_id: None,
            network: consensus_config.params.network_name().to_string(),
        };

        Self {
            version_message,
            addresses_tx,
        }
    }
}

#[async_trait]
impl ConnectionInitializer for KaseederConnectionInitializer {
    async fn initialize_connection(
        &self,
        router: Arc<Router>,
    ) -> std::result::Result<(), ProtocolError> {
        // 1. Subscribe to handshake messages and start the router
        let mut handshake = TondidHandshake::new(&router);
        router.start();

        // 2. Perform handshake with protocol version negotiation
        debug!("Starting handshake with peer");

        // Force protocol version 7 to connect to active Crescendo nodes (v6 nodes are "zombie" nodes)
        let mut version_msg = self.version_message.clone();
        version_msg.protocol_version = 7; // Force v7 for active Crescendo nodes

        let peer_version = match handshake.handshake(version_msg.clone()).await {
            Ok(version) => {
                let user_agent = version.user_agent.clone();
                info!(
                    "Handshake completed with peer using protocol version 7. User agent: {}",
                    user_agent
                );
                Some(version)
            }
            Err(e) => {
                debug!("Handshake failed with protocol version 7: {}", e);
                None
            }
        };

        let _peer_version = peer_version.ok_or_else(|| {
            ProtocolError::from_reject_message(
                "Failed to establish handshake with protocol version 7".to_string(),
            )
        })?;

        // 3. Subscribe to messages for address collection (avoid duplicate subscriptions)
        let all_messages_receiver = router.subscribe(vec![
            TondidMessagePayloadType::Addresses,
            TondidMessagePayloadType::RequestAddresses,
            // Remove Ping from here to avoid duplicate subscription conflict
        ]);

        // 4. Register message flows before Ready exchange (rusty-tondi style)
        debug!("Registering message flows before Ready exchange");

        // 5. Complete handshake with Ready exchange (rusty-tondi style)
        handshake.exchange_ready_messages().await?;
        debug!("Ready exchange completed, handshake fully established");

        // 6. Send address request to get peer addresses (Tondi P2P standard)
        debug!("Sending address request to peer");
        let request_addresses_msg = make_message!(
            Payload::RequestAddresses,
            RequestAddressesMessage { 
                include_all_subnetworks: false, 
                subnetwork_id: None 
            }
        );
        
        if let Err(e) = router.enqueue(request_addresses_msg).await {
            debug!("Failed to send address request: {}", e);
        } else {
            debug!("Address request sent successfully");
        }

        // 7. Start ping-pong handler to keep connection alive
        let router_clone = router.clone();
        tokio::spawn(async move {
            if let Err(e) = DnsseedNetAdapter::handle_ping_pong(router_clone).await {
                debug!("Ping-pong handler error: {}", e);
            }
        });

        // 7. Wait for address response
        // Start address response handler coroutine
        let addresses_tx = self.addresses_tx.clone();

        tokio::spawn(async move {
            if let Err(e) =
                Self::handle_addresses_response(all_messages_receiver, addresses_tx).await
            {
                debug!("Address response handler error: {}", e);
            }
        });

        Ok(())
    }
}

impl KaseederConnectionInitializer {
    async fn handle_addresses_response(
        mut all_messages_receiver: IncomingRoute,
        addresses_tx: mpsc::Sender<Vec<NetAddress>>,
    ) -> std::result::Result<(), ProtocolError> {
        // Wait for address message with timeout, skipping irrelevant messages (like Go version)
        let timeout = Duration::from_secs(3); // Shorter timeout like Go version
        let start_time = std::time::Instant::now();

        loop {
            if start_time.elapsed() > timeout {
                debug!("Timeout waiting for addresses from peer (3s)");
                break;
            }

            tokio::select! {
                msg_opt = all_messages_receiver.recv() => {
                    if let Some(msg) = msg_opt {
                        match msg.payload {
                            Some(Payload::Addresses(addresses_msg)) => {
                                debug!("Received {} addresses from peer", addresses_msg.address_list.len());

                                // Convert address format
                                let addresses: Vec<NetAddress> = addresses_msg.address_list
                                    .into_iter()
                                    .filter_map(|addr| {
                                        // Parse IP address bytes
                                        if addr.ip.len() == 4 {
                                            // IPv4
                                            let ip_bytes: [u8; 4] = [addr.ip[0], addr.ip[1], addr.ip[2], addr.ip[3]];
                                            let ipv4 = std::net::Ipv4Addr::from(ip_bytes);
                                            Some(NetAddress::new(std::net::IpAddr::V4(ipv4), addr.port as u16))
                                        } else if addr.ip.len() == 16 {
                                            // IPv6
                                            let mut ip_bytes = [0u8; 16];
                                            ip_bytes.copy_from_slice(&addr.ip);
                                            let ipv6 = std::net::Ipv6Addr::from(ip_bytes);
                                            Some(NetAddress::new(std::net::IpAddr::V6(ipv6), addr.port as u16))
                                        } else {
                                            debug!("Invalid IP address length: {}", addr.ip.len());
                                            None
                                        }
                                    })
                                    .collect();

                                // Send addresses to main thread
                                if let Err(e) = addresses_tx.send(addresses).await {
                                    debug!("Failed to send addresses to main thread: {}", e);
                                }

                                // Successfully received addresses, break the loop
                                break;
                            }
                            Some(Payload::Ping(_)) => {
                                // Skip ping messages, continue waiting for addresses
                                debug!("Skipping ping message, waiting for addresses");
                                continue;
                            }
                            Some(Payload::Version(_)) => {
                                // Skip version messages, continue waiting for addresses
                                debug!("Skipping version message, waiting for addresses");
                                continue;
                            }
                            Some(Payload::Verack(_)) => {
                                // Skip verack messages, continue waiting for addresses
                                debug!("Skipping verack message, waiting for addresses");
                                continue;
                            }
                                                                Some(Payload::RequestAddresses(_)) => {
                                        // Skip request addresses messages, continue waiting for addresses
                                        debug!("Skipping request addresses message, waiting for addresses");
                                        continue;
                                    }
                                    Some(Payload::Ready(_)) => {
                                        // Skip ready messages, continue waiting for addresses
                                        debug!("Skipping ready message, waiting for addresses");
                                        continue;
                                    }
                            _ => {
                                // Skip any other message types, continue waiting for addresses
                                debug!("Skipping other message type, waiting for addresses");
                                continue;
                            }
                        }
                    } else {
                        // Connection closed
                        debug!("Message receiver closed");
                        break;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    // Small sleep to avoid busy waiting
                    continue;
                }
            }
        }

        Ok(())
    }
}

/// DNS seeder network adapter, using the real tondi-p2p-lib
pub struct DnsseedNetAdapter {
    adaptor: Arc<Adaptor>,
    addresses_rx: Arc<Mutex<mpsc::Receiver<Vec<NetAddress>>>>,
}

impl DnsseedNetAdapter {
    /// Create a new network adapter instance
    pub fn new(consensus_config: Arc<ConsensusConfig>) -> Result<Self> {
        let (addresses_tx, addresses_rx) = mpsc::channel(100);

        let initializer = Arc::new(KaseederConnectionInitializer::new(
            &consensus_config,
            addresses_tx,
        ));

        let hub = Hub::new();
        let counters = Arc::new(TowerConnectionCounters::default());

        let adaptor = Adaptor::client_only(hub, initializer, counters);

        Ok(Self {
            adaptor,
            addresses_rx: Arc::new(Mutex::new(addresses_rx)),
        })
    }

    /// Connect to the specified address and get the address list
    pub async fn connect_and_get_addresses(
        &self,
        address: &str,
    ) -> Result<(VersionMessage, Vec<NetAddress>)> {
        info!("Connecting to peer: {}", address);

        // Implement fast failure strategy for better performance
        let mut retry_count = 0;
        let max_retries = 1; // Reduced to 1 for fastest failure detection
        let base_delay = Duration::from_secs(1); // Keep 1 second for single retry

        loop {
            match self.try_connect_peer(address).await {
                Ok((peer_key, version_message, addresses)) => {
                    info!(
                        "Successfully connected to peer: {} (key: {})",
                        address, peer_key
                    );
                    return Ok((version_message, addresses));
                }
                Err(e) => {
                    retry_count += 1;
                    if retry_count >= max_retries {
                        return Err(KaseederError::ConnectionFailed(format!(
                            "Failed to connect to peer {} after {} retries: {}",
                            address, max_retries, e
                        )));
                    }

                    let delay = base_delay * 2_u32.pow(retry_count as u32 - 1);
                    warn!(
                        "Connection attempt {} failed for {}: {}. Retrying in {:?}...",
                        retry_count, address, e, delay
                    );
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    /// Try to connect to a single node
    async fn try_connect_peer(
        &self,
        address: &str,
    ) -> Result<(PeerKey, VersionMessage, Vec<NetAddress>)> {
        // Connect to peer node with increased timeout
        let peer_key = self
            .adaptor
            .connect_peer_with_retries(
                address.to_string(),
                1,                      // Single connection attempt
                Duration::from_secs(5), // Reduced connection timeout to 5 seconds for faster failure
            )
            .await
            .map_err(|e| {
                // Enhanced error classification for better debugging
                match e {
                    tondi_p2p_lib::ConnectionError::ProtocolError(proto_err) => {
                        // Check if it's a protocol version mismatch
                        if proto_err.to_string().contains("version")
                            || proto_err.to_string().contains("protocol")
                        {
                            KaseederError::ProtocolVersionMismatch(format!(
                                "Protocol version mismatch connecting to {}: {}",
                                address, proto_err
                            ))
                        } else {
                            KaseederError::Protocol(format!(
                                "Protocol error connecting to {}: {}",
                                address, proto_err
                            ))
                        }
                    }
                    tondi_p2p_lib::ConnectionError::NoAddress => KaseederError::InvalidAddress(
                        format!("Invalid address format for {}: {}", address, e),
                    ),
                    tondi_p2p_lib::ConnectionError::IoError(ref io_err) => {
                        // Check if it's a connection refused or timeout
                        if io_err.kind() == std::io::ErrorKind::ConnectionRefused {
                            KaseederError::PeerUnavailable(format!(
                                "Peer {} refused connection: {}",
                                address, io_err
                            ))
                        } else if io_err.kind() == std::io::ErrorKind::TimedOut {
                            KaseederError::NetworkTimeout(format!(
                                "Connection timeout to {}: {}",
                                address, io_err
                            ))
                        } else {
                            KaseederError::Io(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                format!("I/O error connecting to {}: {}", address, e),
                            ))
                        }
                    }
                    _ => KaseederError::ConnectionFailed(format!(
                        "Connection failed to {}: {}",
                        address, e
                    )),
                }
            })?;

        // Wait for address response with increased timeout
        let addresses = self.wait_for_addresses_with_timeout(peer_key).await?;

        // Get peer node information (including version information)
        let version_message = self.get_peer_version_info(peer_key).await?;

        // Keep connection alive for a bit longer to ensure stability
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Disconnect
        self.adaptor.terminate(peer_key).await;

        Ok((peer_key, version_message, addresses))
    }

    /// Wait for address response with increased timeout
    async fn wait_for_addresses_with_timeout(&self, peer_key: PeerKey) -> Result<Vec<NetAddress>> {
        let mut addresses_rx = self.addresses_rx.lock().await;

        tokio::select! {
            result = addresses_rx.recv() => {
                match result {
                    Some(addresses) => {
                        info!("Received {} addresses from peer {}", addresses.len(), peer_key);
                        Ok(addresses)
                    }
                    None => {
                        debug!("Address channel closed for peer {}", peer_key);
                        Ok(Vec::new())
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(8)) => {   // Reduced to 8 seconds for faster failure
                debug!("Timeout waiting for addresses from peer {} (8s)", peer_key);
                Ok(Vec::new())
            }
        }
    }

    /// Get peer node version information
    async fn get_peer_version_info(&self, peer_key: PeerKey) -> Result<VersionMessage> {
        let peers = self.adaptor.active_peers();
        let version_message = peers
            .iter()
            .find(|peer| peer.key() == peer_key)
            .map(|peer| {
                let props = peer.properties();
                VersionMessage {
                    protocol_version: 7, // Force v7 for active Crescendo nodes (ignore rusty-tondi default)
                    services: 0,
                    timestamp: unix_now() as i64,
                    address: None,
                    id: Vec::new(),
                    user_agent: props.user_agent.clone(),
                    disable_relay_tx: props.disable_relay_tx,
                    subnetwork_id: props.subnetwork_id.clone().map(|id| {
                        tondi_p2p_lib::pb::SubnetworkId {
                            bytes: <[u8]>::to_vec(id.as_ref()),
                        }
                    }),
                    network: "".to_string(), // Network name not in properties
                }
            })
            .unwrap_or_else(|| {
                warn!("Could not find peer properties for {}", peer_key);
                VersionMessage {
                    protocol_version: 7, // Force v7 for active Crescendo nodes
                    services: 0,
                    timestamp: unix_now() as i64,
                    address: None,
                    id: Vec::new(),
                    user_agent: "unknown".to_string(),
                    disable_relay_tx: false,
                    subnetwork_id: None,
                    network: "".to_string(),
                }
            });

        Ok(version_message)
    }

    /// Close the adapter
    pub async fn close(&self) {
        self.adaptor.close().await;
    }

    /// Handle ping-pong messages to keep connection alive
    async fn handle_ping_pong(router: Arc<Router>) -> std::result::Result<(), ProtocolError> {
        // Subscribe to ping messages
        let mut ping_receiver = router.subscribe(vec![TondidMessagePayloadType::Ping]);

        loop {
            tokio::select! {
                msg_opt = ping_receiver.recv() => {
                    if let Some(msg) = msg_opt {
                        if let Some(Payload::Ping(ping_msg)) = msg.payload {
                            debug!("Received ping message with nonce: {}", ping_msg.nonce);

                            // Send pong response
                            let pong_message = make_message!(
                                Payload::Pong,
                                tondi_p2p_lib::pb::PongMessage { nonce: ping_msg.nonce }
                            );

                            if let Err(e) = router.enqueue(pong_message).await {
                                warn!("Failed to send pong response: {}", e);
                                break;
                            }

                            debug!("Sent pong response with nonce: {}", ping_msg.nonce);
                        }
                    } else {
                        // Connection closed
                        debug!("Ping receiver closed, stopping ping-pong handler");
                        break;
                    }
                }
                _ = tokio::time::sleep(Duration::from_secs(60)) => {
                    // Periodically send ping messages to keep connection alive
                    let ping_message = make_message!(
                        Payload::Ping,
                        tondi_p2p_lib::pb::PingMessage { nonce: rand::random::<u64>() }
                    );

                    if let Err(e) = router.enqueue(ping_message).await {
                        debug!("Failed to send ping message: {}", e);
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    /// Diagnostic method to test network connectivity
    pub async fn diagnose_connection(&self, address: &str) -> Result<String> {
        info!("Diagnosing connection to: {}", address);

        // Test basic connectivity first
        let start_time = std::time::Instant::now();

        match self.try_connect_peer(address).await {
            Ok((peer_key, _, addresses)) => {
                let duration = start_time.elapsed();
                let result = format!(
                    "✅ Connection successful to {} (key: {}) in {:?}. Received {} addresses.",
                    address,
                    peer_key,
                    duration,
                    addresses.len()
                );
                info!("{}", result);

                // Clean up
                self.adaptor.terminate(peer_key).await;
                Ok(result)
            }
            Err(e) => {
                let duration = start_time.elapsed();
                let error_msg = format!(
                    "❌ Connection failed to {} after {:?}: {}",
                    address, duration, e
                );
                warn!("{}", error_msg);

                // Provide specific error analysis
                let analysis = match e {
                    KaseederError::Protocol(_) => {
                        "Protocol compatibility issue - node may be running different version"
                    }
                    KaseederError::Io(_) => {
                        "Network I/O error - check firewall, routing, or node availability"
                    }
                    KaseederError::ConnectionFailed(_) => {
                        "General connection failure - node may be offline or overloaded"
                    }
                    _ => "Unknown error type",
                };

                Ok(format!("{} | Analysis: {}", error_msg, analysis))
            }
        }
    }
}

impl Clone for DnsseedNetAdapter {
    fn clone(&self) -> Self {
        Self {
            adaptor: Arc::clone(&self.adaptor),
            addresses_rx: Arc::clone(&self.addresses_rx),
        }
    }
}
