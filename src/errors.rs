use thiserror::Error;

/// Application error types
#[derive(Error, Debug)]
pub enum TondiSeederError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("DNS error: {0}")]
    Dns(String),

    #[error("gRPC error: {0}")]
    Grpc(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Service error: {0}")]
    Service(String),

    #[error("Address manager error: {0}")]
    AddressManager(String),

    #[error("Crawler error: {0}")]
    Crawler(String),

    #[error("Invalid address format: {0}")]
    InvalidAddress(String),

    #[error("Invalid port number: {0}")]
    InvalidPort(u16),

    #[error("Invalid IP address: {0}")]
    InvalidIp(String),

    #[error("Invalid configuration value: {field} = {value}, expected: {expected}")]
    InvalidConfigValue {
        field: String,
        value: String,
        expected: String,
    },

    #[error("File not found: {0}")]
    FileNotFound(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Timeout error: {0}")]
    Timeout(String),

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Resource exhausted: {0}")]
    ResourceExhausted(String),

    #[error("Network timeout: {0}")]
    NetworkTimeout(String),

    #[error("Peer unavailable: {0}")]
    PeerUnavailable(String),

    #[error("Protocol version mismatch: {0}")]
    ProtocolVersionMismatch(String),

    #[error("Service unavailable: {0}")]
    ServiceUnavailable(String),
}

/// Result type for the application
pub type Result<T> = std::result::Result<T, TondiSeederError>;

impl From<toml::de::Error> for TondiSeederError {
    fn from(err: toml::de::Error) -> Self {
        TondiSeederError::Serialization(format!("TOML deserialization error: {}", err))
    }
}

impl From<toml::ser::Error> for TondiSeederError {
    fn from(err: toml::ser::Error) -> Self {
        TondiSeederError::Serialization(format!("TOML serialization error: {}", err))
    }
}

impl From<serde_json::Error> for TondiSeederError {
    fn from(err: serde_json::Error) -> Self {
        TondiSeederError::Serialization(format!("JSON error: {}", err))
    }
}

impl From<tonic::transport::Error> for TondiSeederError {
    fn from(err: tonic::transport::Error) -> Self {
        TondiSeederError::Grpc(format!("gRPC transport error: {}", err))
    }
}

impl From<tonic::Status> for TondiSeederError {
    fn from(err: tonic::Status) -> Self {
        TondiSeederError::Grpc(format!("gRPC status error: {}", err))
    }
}

impl From<std::net::AddrParseError> for TondiSeederError {
    fn from(err: std::net::AddrParseError) -> Self {
        TondiSeederError::InvalidAddress(format!("Address parse error: {}", err))
    }
}

impl From<std::num::ParseIntError> for TondiSeederError {
    fn from(err: std::num::ParseIntError) -> Self {
        TondiSeederError::Validation(format!("Number parse error: {}", err))
    }
}

impl From<uuid::Error> for TondiSeederError {
    fn from(err: uuid::Error) -> Self {
        TondiSeederError::Validation(format!("UUID error: {}", err))
    }
}

impl From<chrono::ParseError> for TondiSeederError {
    fn from(err: chrono::ParseError) -> Self {
        TondiSeederError::Validation(format!("Date/time parse error: {}", err))
    }
}

impl From<sled::Error> for TondiSeederError {
    fn from(err: sled::Error) -> Self {
        TondiSeederError::Database(format!("Sled database error: {}", err))
    }
}

impl From<reqwest::Error> for TondiSeederError {
    fn from(err: reqwest::Error) -> Self {
        TondiSeederError::Network(format!("HTTP request error: {}", err))
    }
}

impl From<tokio::time::error::Elapsed> for TondiSeederError {
    fn from(_err: tokio::time::error::Elapsed) -> Self {
        TondiSeederError::Timeout("Operation timed out".to_string())
    }
}

impl From<tokio::sync::AcquireError> for TondiSeederError {
    fn from(err: tokio::sync::AcquireError) -> Self {
        TondiSeederError::ResourceExhausted(format!("Failed to acquire semaphore: {}", err))
    }
}

impl From<trust_dns_proto::error::ProtoError> for TondiSeederError {
    fn from(err: trust_dns_proto::error::ProtoError) -> Self {
        TondiSeederError::Protocol(format!("DNS protocol error: {}", err))
    }
}

impl From<tondi_p2p_lib::common::ProtocolError> for TondiSeederError {
    fn from(err: tondi_p2p_lib::common::ProtocolError) -> Self {
        TondiSeederError::Protocol(format!("Tondi protocol error: {}", err))
    }
}

impl From<tracing_subscriber::filter::ParseError> for TondiSeederError {
    fn from(err: tracing_subscriber::filter::ParseError) -> Self {
        TondiSeederError::Config(format!("Log level parse error: {}", err))
    }
}
