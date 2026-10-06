//! Per-coworker execution lanes.
//!
//! A persistent coworker has one canonical writer per conversation. Detached
//! jobs may use scoped lanes, but they never create another product identity.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

pub(super) fn agent_lane_lock(main_session_id: &str, agent: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LANES: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let lanes = LANES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = lanes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let key = format!("{main_session_id}/{agent}");
    Arc::clone(
        map.entry(key)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_lock_is_per_session_per_coworker_and_stable() {
        let first = agent_lane_lock("lane-test-a", "coder");
        let same = agent_lane_lock("lane-test-a", "coder");
        let another_session = agent_lane_lock("lane-test-b", "coder");
        let another_coworker = agent_lane_lock("lane-test-a", "researcher");
        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &another_session));
        assert!(!Arc::ptr_eq(&first, &another_coworker));
    }
}
