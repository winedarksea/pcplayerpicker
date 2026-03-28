use crate::models::SessionId;
use rand::rngs::StdRng;
use rand::SeedableRng;
use std::hash::{Hash, Hasher};

/// Deterministic RNG tied to a session. The same session ID and reseed count
/// always produce the same sequence — ensuring two devices generate identical
/// schedules given the same event history.
#[derive(Clone)]
pub struct SessionRng {
    base_seed: u64,
    reseed_count: u32,
}

impl SessionRng {
    pub fn new(session_id: &SessionId) -> Self {
        Self {
            base_seed: hash_session_id(session_id),
            reseed_count: 0,
        }
    }

    pub fn with_reseed_count(session_id: &SessionId, reseed_count: u32) -> Self {
        Self {
            base_seed: hash_session_id(session_id),
            reseed_count,
        }
    }

    /// Increment the reseed counter (coach action). Returns the new count.
    pub fn reseed(&mut self) -> u32 {
        self.reseed_count += 1;
        self.reseed_count
    }

    pub fn reseed_count(&self) -> u32 {
        self.reseed_count
    }

    /// Produce a seeded RNG for schedule generation.
    /// Always call this fresh at the start of each scheduling step so the
    /// output is deterministic regardless of call order.
    pub fn make_rng(&self) -> StdRng {
        StdRng::seed_from_u64(self.effective_seed())
    }

    fn effective_seed(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.base_seed.hash(&mut h);
        self.reseed_count.hash(&mut h);
        h.finish()
    }
}

fn hash_session_id(id: &SessionId) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.0.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

    #[test]
    fn same_session_same_output() {
        use uuid::Uuid;
        let id = SessionId(Uuid::new_v4());
        let rng1 = SessionRng::new(&id);
        let rng2 = SessionRng::new(&id);
        let v1: u64 = rng1.make_rng().gen();
        let v2: u64 = rng2.make_rng().gen();
        assert_eq!(v1, v2);
    }

    #[test]
    fn reseed_changes_output() {
        use uuid::Uuid;
        let id = SessionId(Uuid::new_v4());
        let rng1 = SessionRng::new(&id);
        let mut rng2 = SessionRng::new(&id);
        rng2.reseed();
        let v1: u64 = rng1.make_rng().gen();
        let v2: u64 = rng2.make_rng().gen();
        assert_ne!(v1, v2);
    }
}
