//! Event log: a ring of the last 500 events, newest first. Ids are "<boot>-<seq>" so they stay
//! unique across daemon restarts (the UI de-duplicates by id).

use crate::model::{params, DeviceEvent, Params};
use crate::util::iso_jst;
use serde_json::Value;
use std::collections::VecDeque;

pub const CAPACITY: usize = 500;

/// An event before it gets its id and time.
#[derive(Debug, Clone, PartialEq)]
pub struct Ev {
    pub level: &'static str,
    pub code: &'static str,
    pub params: Params,
}

impl Ev {
    pub fn info(code: &'static str) -> Ev {
        Ev { level: "info", code, params: Params::new() }
    }
    pub fn warn(code: &'static str) -> Ev {
        Ev { level: "warn", code, params: Params::new() }
    }
    pub fn with(mut self, v: Value) -> Ev {
        self.params = params(v);
        self
    }
}

pub struct EventRing {
    boot: i64,
    seq: u64,
    items: VecDeque<DeviceEvent>,
}

impl EventRing {
    pub fn new(boot_s: i64) -> Self {
        EventRing { boot: boot_s, seq: 0, items: VecDeque::new() }
    }

    pub fn push(&mut self, ev: Ev, at_ms: i64) -> DeviceEvent {
        self.seq += 1;
        let e = DeviceEvent {
            id: format!("{}-{}", self.boot, self.seq),
            at: iso_jst(at_ms),
            level: ev.level,
            code: ev.code,
            params: ev.params,
        };
        self.items.push_front(e.clone());
        self.items.truncate(CAPACITY);
        e
    }

    /// Newest first.
    pub fn latest(&self, limit: usize) -> Vec<DeviceEvent> {
        self.items.iter().take(limit).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ring_keeps_newest_500() {
        let mut r = EventRing::new(1700000000);
        for i in 0..600 {
            r.push(Ev::info("bitrate_changed").with(json!({ "mbps": i })), 0);
        }
        let all = r.latest(1000);
        assert_eq!(all.len(), CAPACITY);
        assert_eq!(all[0].id, "1700000000-600");
        assert_eq!(all[0].params["mbps"], 599);
        assert_eq!(all[CAPACITY - 1].id, "1700000000-101");
        assert_eq!(r.latest(20).len(), 20);
    }
}
