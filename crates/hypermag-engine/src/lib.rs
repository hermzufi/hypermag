//! Swarm orchestration.
//!
//! Planned contents (milestones M2 and M3):
//!
//! - torrent lifecycle and peer set management,
//! - peer scoring, choking/unchoking and optimistic slots,
//! - batched picker grants and backpressure policy,
//! - adaptive request windows driven by RTT and delivered rate,
//! - global and per-torrent statistics.

#![forbid(unsafe_code)]
