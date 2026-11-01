//! Peer transport and connection management.
//!
//! Planned contents (milestones M1 and M5):
//!
//! - TCP dialer with half-open limits and backoff,
//! - listeners with dual-stack and `SO_REUSEPORT` support,
//! - no-delay, keep-alive and socket buffer tuning,
//! - inbound/outbound connection accounting,
//! - later: MSE/PE encryption and uTP (BEP 29).

#![forbid(unsafe_code)]
