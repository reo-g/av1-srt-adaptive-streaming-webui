//! Sparkline history: 60 points of 5 s, oldest first. A point is the mean of the 1 s samples in
//! its interval (stall_s: their sum); null when nothing was measured in the interval.

use crate::util::round;
use serde_json::{Map, Value};
use std::collections::VecDeque;

pub const METRICS: [&str; 9] = [
    "send_mbps",
    "recv_mbps",
    "target_mbps",
    "actual_mbps",
    "loss_pct",
    "rtt_ms",
    "bandwidth_mbps",
    "delay_margin_s",
    "stall_s",
];
const STALL: usize = 8;
pub const POINTS: usize = 60;
pub const INTERVAL_S: usize = 5;

/// One 1 s sample, in the order of METRICS.
pub type Sample = [Option<f64>; 9];

#[derive(Default)]
pub struct History {
    points: VecDeque<Sample>,
    acc: Vec<Sample>,
    end_ms: Option<i64>,
}

impl History {
    pub fn push(&mut self, s: Sample, now_ms: i64) {
        self.acc.push(s);
        if self.acc.len() < INTERVAL_S {
            return;
        }
        self.points.push_back(bucket(&self.acc));
        while self.points.len() > POINTS {
            self.points.pop_front();
        }
        self.acc.clear();
        self.end_ms = Some(now_ms);
    }

    /// End time of the last point, and the last `n` points of each series (null-padded at the start).
    pub fn series(&self, n: usize) -> (Option<i64>, Map<String, Value>) {
        let n = n.clamp(1, POINTS);
        let mut out = Map::new();
        for (k, name) in METRICS.iter().enumerate() {
            let mut v: Vec<Value> = Vec::with_capacity(n);
            let have = self.points.len().min(n);
            v.resize(n - have, Value::Null);
            for p in self.points.iter().skip(self.points.len() - have) {
                v.push(p[k].map_or(Value::Null, Value::from));
            }
            out.insert(name.to_string(), Value::Array(v));
        }
        (self.end_ms, out)
    }
}

fn bucket(acc: &[Sample]) -> Sample {
    let mut out: Sample = [None; 9];
    for (k, slot) in out.iter_mut().enumerate() {
        let vals: Vec<f64> = acc.iter().filter_map(|s| s[k]).collect();
        if vals.is_empty() {
            continue;
        }
        let sum: f64 = vals.iter().sum();
        *slot = Some(round(if k == STALL { sum } else { sum / vals.len() as f64 }, 2));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(send: Option<f64>, stall: Option<f64>) -> Sample {
        let mut x: Sample = [None; 9];
        x[0] = send;
        x[STALL] = stall;
        x
    }

    #[test]
    fn buckets_mean_sum_and_gaps() {
        let mut h = History::default();
        let (end, ser) = h.series(60);
        assert_eq!(end, None);
        assert_eq!(ser["send_mbps"].as_array().unwrap().len(), 60);
        assert!(ser["send_mbps"][59].is_null());

        // 5 samples, two of them unmeasured: the mean of the measured ones
        for v in [Some(3.0), None, Some(4.0), None, Some(3.5)] {
            h.push(s(v, Some(0.0)), 1000);
        }
        // 5 samples with nothing measured: a gap; stalls add up
        for st in [Some(0.4), None, None, Some(0.3), None] {
            h.push(s(None, st), 2000);
        }
        // 4 samples: not a full interval yet
        for _ in 0..4 {
            h.push(s(Some(9.0), None), 3000);
        }
        let (end, ser) = h.series(60);
        assert_eq!(end, Some(2000));
        let send = ser["send_mbps"].as_array().unwrap();
        assert_eq!(send[58], 3.5);
        assert!(send[59].is_null());
        let stall = ser["stall_s"].as_array().unwrap();
        assert_eq!(stall[58], 0.0);
        assert_eq!(stall[59], 0.7);
        assert!(ser["rtt_ms"][58].is_null());
        assert_eq!(h.series(2).1["send_mbps"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn keeps_60_points() {
        let mut h = History::default();
        for i in 0..(70 * INTERVAL_S) {
            h.push(s(Some((i / INTERVAL_S) as f64), None), i as i64);
        }
        let (_, ser) = h.series(60);
        let send = ser["send_mbps"].as_array().unwrap();
        assert_eq!(send.len(), 60);
        assert_eq!(send[0], 10.0);
        assert_eq!(send[59], 69.0);
    }
}
