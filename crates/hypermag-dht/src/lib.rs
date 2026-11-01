//! Mainline DHT (BEP 5).
//!
//! Planned contents (milestone M4):
//!
//! - Kademlia routing table with bucket refresh,
//! - `ping`, `find_node`, `get_peers`, `announce_peer` transactions,
//! - iterative lookup and peer storage,
//! - bootstrap and IPv6 support.

#![forbid(unsafe_code)]
