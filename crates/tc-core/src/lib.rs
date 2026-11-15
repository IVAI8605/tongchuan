//! 通传内核：协议、校验、落盘、会话状态机。
//! v0.2 起传输层抽象成明文 TCP 与 TLS 1.3 两条路，帧、清单、校验和续传状态机共用一份。

pub mod clip;
pub mod daemon;
pub mod diag;
pub mod discover;
pub mod frame;
pub mod hash;
pub mod hashcache;
pub mod history;
pub mod manifest;
pub mod pathsafe;
pub mod queue;
pub mod retry;
pub mod sendjob;
pub mod session;
pub mod store;
pub mod tls;
pub mod transport;
pub mod trust;
pub mod web;

pub use frame::{FrameHeader, FrameType, BLOCK_SIZE};
pub use hashcache::HashCache;
pub use manifest::{build_manifest, build_manifest_with_progress, DeviceInfo, FileEntry, Manifest, Root};
pub use session::{
    list_destinations, list_destinations_for, protocol_compatible, run_receiver, run_receiver_on,
    run_receiver_with, run_receiver_with_gate, run_receiver_with_gate_streams,
    run_receiver_with_store, run_receiver_with_store_gate, run_receiver_with_store_gate_streams,
    run_receiver_with_streams, run_sender, run_sender_with, run_sender_with_device,
    run_sender_with_device_control, run_sender_with_device_control_opts,
    run_sender_with_device_progress, run_sender_with_opts, validate_manifest_paths, DestEntry,
    Destination, GateVerdict, HubReport, PathClaims, ReceiverHub, ReceiverReport, SendControl,
    SendOptions, SendProgress, SendReport, TrustGate, TrustMode, DEFAULT_STREAMS,
    PROTOCOL_VERSION,
};
pub use store::{PartFile, PathStore, Store};
pub use tls::{client_config, client_config_with_identity, server_config, Identity};
pub use trust::{TrustEvent, TrustStore, TrustedPeer};
