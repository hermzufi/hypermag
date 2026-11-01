//! Piece and block picker.
//!
//! Planned contents (milestone M2):
//!
//! - rarest-first selection with partial-piece affinity,
//! - per-block request deadlines and requeue,
//! - endgame duplication with cancellation,
//! - file priorities and sequential mode,
//! - fast-resume serialization.
//!
//! This crate is deliberately runtime-free and I/O-free so it can be tested as
//! a pure state machine against a reference model.

#![forbid(unsafe_code)]
