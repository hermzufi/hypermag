//! Mainline DHT (BEP 5) minimal node.

#![forbid(unsafe_code)]

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct NodeId([u8; 20]);

impl NodeId {
    #[must_use]
    pub fn random() -> Self {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        std::time::SystemTime::now().hash(&mut h);
        let v = h.finish();
        let mut b = [0u8; 20];
        b[0..8].copy_from_slice(&v.to_be_bytes());
        Self(b)
    }
}
