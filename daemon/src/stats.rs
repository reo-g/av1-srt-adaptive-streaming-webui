//! Statistics. Every second the sampler reads the lines appended to the session's files since
//! the last read (tx: fast stats, controller ticks and events; rx: receiver stats) plus the
//! player's stats lines, and maps them to Stats. Lines are timed by when they are read; rates
//! use the rows' own monotonic timestamps only as intervals (no clock conversion).
//!
//! rx stops: the player counts a stop once its render gap passes 2 frame periods and then lets
//! `stall_ms` grow every second until a frame comes. The sampler follows each stop while it is
//! open (the alert and the 5-minute totals grow with it) and reports `video_stalled` with its
//! full length once it has ended. Stops shorter than MIN_STALL_S (a late or missing source
//! frame) are not reported anywhere.

use crate::config::Role;
use crate::events::Ev;
use crate::history::Sample;
use crate::model::Stats;
use crate::profile::{EVENTS, FAST, RCV, TICKS};
use crate::util::{floor1, iso_jst, now_ms, round};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A measurement older than this is not shown.
const STALE: Duration = Duration::from_millis(2500);
const WINDOW: Duration = Duration::from_secs(300);
/// No ACK progress this long while data is outstanding: the receiver cannot be reached.
const ACK_SILENT: Duration = Duration::from_secs(3);
/// The margin must stay at or above the warning level this many seconds to clear the alert.
const MARGIN_CLEAR_S: u32 = 3;
/// rx: no frame rendered for this long = black output.
const BLACK_AFTER: Duration = Duration::from_millis(1500);
pub const STALL_WINDOW_S: u64 = 300;
/// rx: a stop shorter than this is not reported (the player already counts one missing frame).
const MIN_STALL_S: f64 = 0.2;
/// rx: one frame period of the signal (4K30), as the player measures its gaps.
const FRAME_MS: f64 = 1000.0 / 30.0;
/// rx: players remembered (the current one and old ones still playing out).
const PLAYERS_KEPT: usize = 8;

// ------------------------------------------------------------------ reading files

/// Without an archive the session files are emptied in place once they pass this size (tmpfs):
/// every writer appends, and the controller's reader starts again from 0 (bufctrl.py FastStatsSource).
pub const FILE_CAP: u64 = 64 << 20;

/// Reads the complete lines appended to a file since the last call.
pub struct Tailer {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    cap: Option<u64>,
}

impl Tailer {
    pub fn new(path: PathBuf, cap: Option<u64>) -> Self {
        Tailer { path, offset: 0, partial: Vec::new(), cap }
    }

    pub fn lines(&mut self) -> Vec<String> {
        let Ok(mut f) = File::open(&self.path) else { return Vec::new() };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            // truncated in place: start again
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset || f.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut buf = Vec::new();
        if (&mut f).take(len - self.offset).read_to_end(&mut buf).is_err() {
            return Vec::new();
        }
        self.offset += buf.len() as u64;
        self.partial.extend_from_slice(&buf);
        let mut out = Vec::new();
        let mut start = 0;
        for (i, b) in self.partial.iter().enumerate() {
            if *b == b'\n' {
                out.push(String::from_utf8_lossy(&self.partial[start..i]).into_owned());
                start = i + 1;
            }
        }
        self.partial.drain(..start);
        if self.partial.len() > 1 << 20 {
            self.partial.clear();
        }
        if self.cap.is_some_and(|c| self.offset > c) {
            // everything up to here was read; a line being written now may start half-way (skipped)
            if std::fs::OpenOptions::new().write(true).open(&self.path).and_then(|w| w.set_len(0)).is_ok() {
                self.offset = 0;
                self.partial.clear();
            }
        }
        out
    }
}

fn parse<T: DeserializeOwned>(line: &str) -> Option<T> {
    serde_json::from_str(line.trim()).ok()
}

/// Change of a cumulative counter; None when a value is missing or the counter went back.
fn delta(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    let d = b? - a?;
    (d >= 0.0).then_some(d)
}

// ------------------------------------------------------------------ rows (field names as the programs write them)

/// Sender fast stats (SRT_FAST_STATS_JSONL), one row per 100 ms, cumulative counters.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FastRow {
    pub rc: Option<i64>,
    pub t_us: Option<f64>,
    #[serde(rename = "inBytes")]
    pub in_bytes: Option<f64>,
    #[serde(rename = "inMsgs")]
    pub in_msgs: Option<f64>,
    #[serde(rename = "pktSentTotal")]
    pub pkt_sent: Option<f64>,
    #[serde(rename = "pktRetransTotal")]
    pub pkt_retrans: Option<f64>,
    #[serde(rename = "pktSndDropTotal")]
    pub pkt_drop: Option<f64>,
    #[serde(rename = "pktRecvACKTotal")]
    pub pkt_ack: Option<f64>,
    #[serde(rename = "pktFlightSize")]
    pub flight: Option<f64>,
    #[serde(rename = "pktSndBuf")]
    pub snd_buf: Option<f64>,
    #[serde(rename = "byteSentTotal")]
    pub byte_sent: Option<f64>,
    #[serde(rename = "msRTT")]
    pub rtt: Option<f64>,
}

/// Controller tick (BUFCTRL_TICKS_JSONL), every 100 ms.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TickRow {
    /// active target, kbps
    pub cur: Option<f64>,
    /// D0 − H: seconds before the receiver's picture would stop
    pub nd_budget: Option<f64>,
    /// link capacity while the sender is backlogged, kbps (null otherwise)
    pub nd_c: Option<f64>,
}

/// Controller event (BRIDGE_EVENTS_JSONL); `decide` and `complete` are joined by `cmd`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EventRow {
    pub ev: Option<String>,
    pub cmd: Option<Value>,
    pub cause: Option<String>,
    #[serde(rename = "R_old")]
    pub r_old: Option<f64>,
    #[serde(rename = "R_new")]
    pub r_new: Option<f64>,
}

/// Receiver stats (SRT_RCV_STATS_JSONL); the counters are null when the stats call failed.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RcvRow {
    pub rc: Option<i64>,
    pub t_us: Option<f64>,
    #[serde(rename = "pktRecvTotal")]
    pub pkt_recv: Option<f64>,
    #[serde(rename = "pktRcvLossTotal")]
    pub pkt_loss: Option<f64>,
    #[serde(rename = "pktRcvDropTotal")]
    pub pkt_drop: Option<f64>,
    #[serde(rename = "pktRcvRetrans")]
    pub pkt_retrans: Option<f64>,
    #[serde(rename = "byteRecvTotal")]
    pub byte_recv: Option<f64>,
    #[serde(rename = "msRTT")]
    pub rtt: Option<f64>,
    #[serde(rename = "msgnoGapTotal")]
    pub gaps: Option<f64>,
}

/// Player stats (--stats-fd), one line per second, totals since the player started.
/// `stall_ms` includes the stop still open at the time of the line; `last_gap_ms` is the
/// longest render gap of the last second (the open one included).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlayerRow {
    pub frames: Option<f64>,
    pub fps: Option<f64>,
    pub stalls: Option<f64>,
    pub stall_ms: Option<f64>,
    pub last_gap_ms: Option<f64>,
}

/// Why the automatic mode changed the bitrate, from the controller's cause. Downward steps of
/// the buffer layer (never-drop budget, protection, a failed probe) and of the send-buffer rule
/// are delay-driven; the loss rule and its emergency brake are loss-driven.
pub fn reason(cause: &str, from_kbps: f64, to_kbps: f64) -> &'static str {
    if cause == "manual" {
        "manual"
    } else if to_kbps > from_kbps {
        "recovered"
    } else if cause.starts_with("nd-") || cause.starts_with("prot") || matches!(cause, "probe-revert" | "buffer_mode") {
        "delay"
    } else {
        "loss"
    }
}

/// Change of a cumulative counter over the last 5 minutes.
#[derive(Default)]
struct Window {
    items: VecDeque<(Instant, f64)>,
}

impl Window {
    fn push(&mut self, at: Instant, v: Option<f64>) {
        let Some(v) = v else { return };
        self.items.push_back((at, v));
        while self.items.front().is_some_and(|(t, _)| at.duration_since(*t) > WINDOW) {
            self.items.pop_front();
        }
    }
    fn delta(&self) -> Option<f64> {
        delta(Some(self.items.front()?.1), Some(self.items.back()?.1))
    }
}

// ------------------------------------------------------------------ rx stops

/// One stop of the picture, as far as the player has reported it.
#[derive(Debug, Clone, Copy)]
struct Stall {
    start: Instant,
    secs: f64,
}

/// Seconds of a stop that are reported (none below the minimum).
fn counted(secs: f64) -> f64 {
    if secs >= MIN_STALL_S {
        secs
    } else {
        0.0
    }
}

/// Lengthen a stop; returns the change of its reported seconds.
fn grow(s: &mut Stall, ms: f64) -> f64 {
    let before = counted(s.secs);
    s.secs += ms / 1000.0;
    counted(s.secs) - before
}

/// One player (by session); an old player may still be playing out.
#[derive(Debug, Default)]
struct PlayerState {
    last: Option<PlayerRow>,
    /// The latest stop; it may still be growing.
    open: Option<Stall>,
    /// The player has exited: lines still in its pipe are ignored.
    done: bool,
}

// ------------------------------------------------------------------ the sampler

/// What the sampler learnt that the link state needs.
#[derive(Debug, Default)]
pub struct Obs {
    pub ack_silent: bool,
    pub last_ack: Option<Instant>,
    /// Some(margin) while delay_margin_low holds.
    pub margin_low: Option<f64>,
    /// (stops, seconds, first stop) in the last 5 minutes.
    pub stalled: Option<(usize, f64, Instant)>,
    pub black: bool,
    /// rx: the current session's player rendered its first frames.
    pub first_frames: bool,
    pub events: Vec<Ev>,
}

pub struct Output {
    pub stats: Stats,
    pub sample: Sample,
    pub obs: Obs,
}

pub struct Ctx<'a> {
    pub live: bool,
    pub running: bool,
    pub session: Option<(u64, &'a Path)>,
}

#[derive(Default)]
struct TxState {
    fast: Option<Tailer>,
    ticks: Option<Tailer>,
    events: Option<Tailer>,
    prev: Option<FastRow>,
    row_at: Option<Instant>,
    send: Option<f64>,
    loss: Option<f64>,
    rtt: Option<f64>,
    /// (t_s, inBytes) of the last 6 s, for the 5 s mean of the video rate
    input: VecDeque<(f64, f64)>,
    drops: Window,
    ack: Option<f64>,
    /// when the last ACK progress was read, and the row time (s) of that progress
    ack_at: Option<Instant>,
    ack_t: Option<f64>,
    last_t: Option<f64>,
    armed: bool,
    outstanding: bool,
    tick: Option<TickRow>,
    tick_at: Option<Instant>,
    decides: HashMap<String, EventRow>,
}

#[derive(Default)]
struct RxState {
    rcv: Option<Tailer>,
    prev: Option<RcvRow>,
    row_at: Option<Instant>,
    recv: Option<f64>,
    loss: Option<f64>,
    retrans: Option<f64>,
    rtt: Option<f64>,
    unrecovered: Window,
    players: BTreeMap<u64, PlayerState>,
    /// each reported stop that ended in the last 5 minutes: (start, end, seconds)
    stalls: VecDeque<(Instant, Instant, f64)>,
    /// reported stop seconds since the last sample (history)
    stall_acc: f64,
    frame_at: Option<Instant>,
    /// sessions whose player has rendered frames, and the last one reported
    rendered: Vec<u64>,
    reported: Option<u64>,
    events: Vec<Ev>,
}

pub struct Sampler {
    role: Role,
    /// size cap of the session files (None with an archive)
    cap: Option<u64>,
    delay_profile: &'static str,
    delay_target_ms: u32,
    warn_s: f64,
    session: Option<u64>,
    tx: TxState,
    rx: RxState,
    last_rtt: Option<f64>,
    margin_low: bool,
    margin_last: f64,
    margin_ok: u32,
}

impl Sampler {
    pub fn new(role: Role, delay_target_ms: u32, warn_s: f64, cap: Option<u64>) -> Self {
        Sampler {
            role,
            cap,
            delay_profile: "standard",
            delay_target_ms,
            warn_s,
            session: None,
            tx: TxState::default(),
            rx: RxState::default(),
            last_rtt: None,
            margin_low: false,
            margin_last: 0.0,
            margin_ok: 0,
        }
    }

    /// Stats with nothing measured.
    pub fn blank(&self) -> Stats {
        Stats {
            at: iso_jst(now_ms()),
            delay_profile: self.delay_profile,
            delay_target_ms: self.delay_target_ms,
            delay_margin_warn_s: (self.role == Role::Tx).then_some(self.warn_s),
            last_rtt_ms: self.last_rtt,
            ..Stats::default()
        }
    }

    /// A new start: forget the stops and alarms of the previous run.
    pub fn reset_run(&mut self) {
        let r = &mut self.rx;
        r.stalls.clear();
        r.events.clear();
        r.stall_acc = 0.0;
        for p in r.players.values_mut() {
            (p.open, p.done) = (None, true);
        }
        self.margin_low = false;
        self.margin_ok = 0;
    }

    fn enter_session(&mut self, session: Option<(u64, &Path)>) {
        self.session = session.map(|s| s.0);
        let dir = session.map(|s| s.1);
        let cap = self.cap;
        let tail = |name: &str| dir.map(|d| Tailer::new(d.join(name), cap));
        self.tx = TxState { fast: tail(FAST), ticks: tail(TICKS), events: tail(EVENTS), ..TxState::default() };
        // rx: the receiver's counters start again; the players' stops and frames carry on
        let r = &mut self.rx;
        r.rcv = tail(RCV);
        (r.prev, r.row_at, r.recv, r.loss, r.retrans, r.rtt) = (None, None, None, None, None, None);
        r.unrecovered = Window::default();
    }

    /// A stats line from a player (rx), as it arrives.
    pub fn player_line(&mut self, session: u64, line: &str, now: Instant) {
        let Some(row) = parse::<PlayerRow>(line) else { return };
        let r = &mut self.rx;
        if !r.players.contains_key(&session) && r.players.keys().any(|k| *k > session) {
            return; // a player already forgotten
        }
        let p = r.players.entry(session).or_default();
        if p.done {
            return;
        }
        let before = |f: fn(&PlayerRow) -> Option<f64>| p.last.as_ref().and_then(f).unwrap_or(0.0);
        let new_stops = (row.stalls.unwrap_or(0.0) - before(|x| x.stalls)).max(0.0) as usize;
        let grew_ms = (row.stall_ms.unwrap_or(0.0) - before(|x| x.stall_ms)).max(0.0);
        let advanced = row.frames.unwrap_or(0.0) > before(|x| x.frames);
        let mut ended = Vec::new();
        let mut acc = 0.0;
        if new_stops == 0 {
            // All growth belongs to the latest stop: it was still open when the last line was written.
            if let Some(s) = p.open.as_mut() {
                acc += grow(s, grew_ms);
            } else if grew_ms > 0.0 {
                let mut s = Stall { start: now, secs: 0.0 };
                acc += grow(&mut s, grew_ms);
                p.open = Some(s);
            }
            // A frame ended it in this second, or it had ended before (nothing grew).
            if advanced || grew_ms == 0.0 {
                ended.extend(p.open.take());
            }
        } else {
            // A new stop needs a frame after the open one, so that one has ended. Its last part is its
            // whole gap less what it had (its gap is most likely the longest of the second); the rest
            // belongs to the new stops, the latest of which may still be open.
            let mut rest = grew_ms;
            if let Some(mut s) = p.open.take() {
                let part = row.last_gap_ms.map_or(0.0, |g| g - FRAME_MS - s.secs * 1000.0).clamp(0.0, rest);
                acc += grow(&mut s, part);
                rest -= part;
                ended.push(s);
            }
            for i in 0..new_stops {
                let mut s = Stall { start: now, secs: 0.0 };
                acc += grow(&mut s, rest / new_stops as f64);
                if i + 1 < new_stops {
                    ended.push(s);
                } else {
                    p.open = Some(s);
                }
            }
        }
        let rendered = row.fps.unwrap_or(0.0) > 0.0 || advanced;
        p.last = Some(row);
        r.stall_acc += acc;
        for s in ended {
            r.end_stall(s, now);
        }
        if rendered {
            r.frame_at = Some(now);
            if !r.rendered.contains(&session) {
                r.rendered.push(session);
            }
        }
        while r.players.len() > PLAYERS_KEPT {
            r.players.pop_first();
        }
        if r.rendered.len() > PLAYERS_KEPT {
            r.rendered.remove(0);
        }
    }

    /// A player exited (rx): its open stop has ended.
    pub fn player_done(&mut self, session: u64, now: Instant) {
        let r = &mut self.rx;
        let p = r.players.entry(session).or_default();
        p.done = true;
        if let Some(s) = p.open.take() {
            r.end_stall(s, now);
        }
        while r.players.len() > PLAYERS_KEPT {
            r.players.pop_first();
        }
    }

    pub fn sample(&mut self, ctx: Ctx, now: Instant) -> Output {
        if ctx.session.map(|s| s.0) != self.session {
            self.enter_session(ctx.session);
        }
        let mut st = self.blank();
        let mut obs = Obs::default();
        let mut stall_sample = None;
        match self.role {
            Role::Tx => self.sample_tx(ctx.live, now, &mut st, &mut obs),
            Role::Rx => stall_sample = self.sample_rx(&ctx, now, &mut st, &mut obs),
        }
        if st.rtt_ms.is_some() {
            self.last_rtt = st.rtt_ms;
        }
        st.last_rtt_ms = self.last_rtt;
        let sample = [
            st.send_mbps,
            st.recv_mbps,
            st.target_mbps,
            st.actual_mbps,
            st.loss_pct,
            st.rtt_ms,
            st.bandwidth_mbps,
            st.delay_margin_s,
            stall_sample,
        ];
        Output { stats: st, sample, obs }
    }

    fn sample_tx(&mut self, live: bool, now: Instant, st: &mut Stats, obs: &mut Obs) {
        let t = &mut self.tx;
        let rows: Vec<FastRow> = match t.fast.as_mut() {
            Some(f) => f.lines().iter().filter_map(|l| parse::<FastRow>(l)).collect(),
            None => Vec::new(),
        };
        let mut latest = None;
        for r in rows.into_iter().filter(|r| r.rc.unwrap_or(0) == 0 && r.t_us.is_some()) {
            if r.in_msgs.unwrap_or(0.0) > 0.0 {
                t.armed = true;
            }
            let row_t = r.t_us.map(|us| us / 1e6);
            if let Some(a) = r.pkt_ack {
                if t.ack.is_none_or(|old| a > old) {
                    (t.ack, t.ack_at, t.ack_t) = (Some(a), Some(now), row_t);
                }
            }
            t.last_t = row_t;
            t.outstanding = r.flight.unwrap_or(0.0) > 0.0 || r.snd_buf.unwrap_or(0.0) > 0.0;
            if let (Some(ts), Some(b)) = (r.t_us, r.in_bytes) {
                let ts = ts / 1e6;
                t.input.push_back((ts, b));
                while t.input.front().is_some_and(|(x, _)| ts - x > 6.0) {
                    t.input.pop_front();
                }
            }
            latest = Some(r);
        }
        if let Some(cur) = latest {
            if let Some(p) = &t.prev {
                let dt = (cur.t_us.unwrap_or(0.0) - p.t_us.unwrap_or(0.0)) / 1e6;
                if dt > 0.0 {
                    t.send = delta(p.byte_sent, cur.byte_sent).map(|b| b * 8.0 / dt / 1e6);
                    t.loss = match (delta(p.pkt_sent, cur.pkt_sent), delta(p.pkt_retrans, cur.pkt_retrans)) {
                        (Some(s), Some(r)) if s > 0.0 => Some((r / s * 100.0).min(100.0)),
                        _ => None,
                    };
                }
            }
            t.rtt = cur.rtt;
            t.drops.push(now, cur.pkt_drop);
            t.row_at = Some(now);
            t.prev = Some(cur);
        }
        if let Some(f) = t.ticks.as_mut() {
            if let Some(row) = f.lines().iter().rev().find_map(|l| parse::<TickRow>(l)) {
                t.tick = Some(row);
                t.tick_at = Some(now);
            }
        }
        if let Some(f) = t.events.as_mut() {
            for row in f.lines().iter().filter_map(|l| parse::<EventRow>(l)) {
                let key = row.cmd.as_ref().map(Value::to_string).unwrap_or_default();
                match row.ev.as_deref() {
                    Some("decide") => {
                        if t.decides.len() > 64 {
                            t.decides.clear();
                        }
                        t.decides.insert(key, row);
                    }
                    Some("complete") => {
                        let Some(d) = t.decides.remove(&key) else { continue };
                        let (Some(from), Some(to)) = (d.r_old, d.r_new) else { continue };
                        if from != to {
                            let why = reason(d.cause.as_deref().unwrap_or(""), from, to);
                            obs.events.push(Ev::info("bitrate_changed").with(json!({
                                "mbps": round(to / 1000.0, 2), "from_mbps": round(from / 1000.0, 2), "reason": why
                            })));
                        }
                    }
                    _ => {}
                }
            }
        }

        let fresh = |at: Option<Instant>| at.is_some_and(|a| now.duration_since(a) <= STALE);
        if live {
            if fresh(t.row_at) {
                st.send_mbps = t.send.map(|v| round(v, 2));
                st.loss_pct = t.loss.map(|v| round(v, 1));
                st.rtt_ms = t.rtt.map(|v| round(v, 0));
                st.actual_mbps = video_rate(&t.input).map(|v| round(v, 2));
            }
            st.send_late_5m = t.drops.delta().map(|d| d as i64);
            if let (true, Some(k)) = (fresh(t.tick_at), &t.tick) {
                st.target_mbps = k.cur.map(|c| round(c / 1000.0, 2));
                st.bandwidth_mbps = k.nd_c.map(|c| round(c / 1000.0, 2));
                st.delay_margin_s = k.nd_budget.map(|b| floor1(b.max(0.0)));
            }
            // Silence between the rows' own times; by arrival time only when no rows come at all.
            let by_rows = match (t.ack_t, t.last_t) {
                (Some(a), Some(l)) => l - a >= ACK_SILENT.as_secs_f64(),
                _ => false,
            };
            let no_rows = t.row_at.is_some_and(|a| now.duration_since(a) >= ACK_SILENT);
            obs.ack_silent = t.armed && t.outstanding && (by_rows || no_rows);
        }
        obs.last_ack = t.ack_at;

        match st.delay_margin_s {
            Some(m) if m < self.warn_s => {
                if !self.margin_low {
                    obs.events.push(Ev::warn("delay_margin_low").with(json!({ "margin_s": m })));
                }
                self.margin_low = true;
                self.margin_last = m;
                self.margin_ok = 0;
            }
            cur if self.margin_low => {
                // the alert shows the same margin as the stats until it clears
                if let Some(m) = cur {
                    self.margin_last = m;
                }
                self.margin_ok += 1;
                if self.margin_ok >= MARGIN_CLEAR_S || !live {
                    self.margin_low = false;
                }
            }
            _ => {}
        }
        if self.margin_low {
            obs.margin_low = Some(self.margin_last);
        }
    }

    fn sample_rx(&mut self, ctx: &Ctx, now: Instant, st: &mut Stats, obs: &mut Obs) -> Option<f64> {
        let session = self.session;
        let r = &mut self.rx;
        let rows: Vec<RcvRow> = match r.rcv.as_mut() {
            Some(f) => f.lines().iter().filter_map(|l| parse::<RcvRow>(l)).collect(),
            None => Vec::new(),
        };
        if let Some(cur) = rows.into_iter().filter(|x| x.rc.unwrap_or(0) == 0 && x.pkt_recv.is_some()).last() {
            if let Some(p) = &r.prev {
                let dt = (cur.t_us.unwrap_or(0.0) - p.t_us.unwrap_or(0.0)) / 1e6;
                if dt > 0.0 {
                    r.recv = delta(p.byte_recv, cur.byte_recv).map(|b| b * 8.0 / dt / 1e6);
                    let got = delta(p.pkt_recv, cur.pkt_recv);
                    let lost = delta(p.pkt_loss, cur.pkt_loss);
                    r.loss = match (got, lost) {
                        (Some(g), Some(l)) if g + l > 0.0 => Some(l / (g + l) * 100.0),
                        _ => None,
                    };
                    r.retrans = match (got, delta(p.pkt_retrans, cur.pkt_retrans)) {
                        (Some(g), Some(x)) if g > 0.0 => Some((x / g * 100.0).min(100.0)),
                        _ => None,
                    };
                }
            }
            r.rtt = cur.rtt;
            let lost_for_good = match (cur.pkt_drop, cur.gaps) {
                (Some(a), Some(b)) => Some(a + b),
                (a, b) => a.or(b),
            };
            r.unrecovered.push(now, lost_for_good);
            r.row_at = Some(now);
            r.prev = Some(cur);
        }
        while r.stalls.front().is_some_and(|(_, end, _)| now.duration_since(*end) > WINDOW) {
            r.stalls.pop_front();
        }
        // the ended stops plus the open ones long enough to report
        let open: Vec<Stall> = r.players.values().filter_map(|p| p.open).filter(|s| s.secs >= MIN_STALL_S).collect();
        let stop_count = r.stalls.len() + open.len();
        let stop_s: f64 = r.stalls.iter().map(|s| s.2).sum::<f64>() + open.iter().map(|s| s.secs).sum::<f64>();
        let first_stop = r.stalls.iter().map(|s| s.0).chain(open.iter().map(|s| s.start)).min();
        let fresh = r.row_at.is_some_and(|a| now.duration_since(a) <= STALE);
        let mut stall_sample = None;
        if ctx.live {
            if fresh {
                st.recv_mbps = r.recv.map(|v| round(v, 2));
                st.loss_pct = r.loss.map(|v| round(v, 1));
                st.retrans_pct = r.retrans.map(|v| round(v, 1));
                st.rtt_ms = r.rtt.map(|v| round(v, 0));
            }
            st.unrecovered_5m = r.unrecovered.delta().map(|d| d as i64);
            st.stalls_5m = Some(stop_count as i64);
            st.stall_s_5m = Some(round(stop_s, 1));
            stall_sample = Some(round(r.stall_acc, 2));
        }
        r.stall_acc = 0.0;
        if ctx.running {
            obs.events.append(&mut r.events);
            if let Some(first) = first_stop {
                obs.stalled = Some((stop_count, round(stop_s, 1), first));
            }
        } else {
            r.events.clear();
        }
        obs.black = !r.frame_at.is_some_and(|a| now.duration_since(a) <= BLACK_AFTER);
        if let Some(sid) = session.filter(|s| r.rendered.contains(s) && r.reported != Some(*s)) {
            r.reported = Some(sid);
            obs.first_frames = true;
        }
        stall_sample
    }
}

impl RxState {
    /// A stop has ended: it is reported when it is long enough.
    fn end_stall(&mut self, s: Stall, now: Instant) {
        if s.secs >= MIN_STALL_S {
            self.stalls.push_back((s.start, now, s.secs));
            self.events.push(Ev::warn("video_stalled").with(json!({ "seconds": round(s.secs, 1) })));
        }
    }
}

/// Video rate (Mbps) from the input byte counter: the mean over the last 5 s (at least 1 s).
fn video_rate(input: &VecDeque<(f64, f64)>) -> Option<f64> {
    let last = input.back()?;
    let first = input.iter().find(|(t, _)| last.0 - t <= 5.0)?;
    let dt = last.0 - first.0;
    (dt >= 1.0).then(|| (last.1 - first.1).max(0.0) * 8.0 / dt / 1e6)
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::io::Write;

    const FAST_ROWS: &str = include_str!("../tests/fixtures/fast.jsonl");
    const TICK_ROWS: &str = include_str!("../tests/fixtures/ticks.jsonl");
    const EVENT_ROWS: &str = include_str!("../tests/fixtures/events.jsonl");
    const RCV_ROWS: &str = include_str!("../tests/fixtures/rcv.jsonl");
    const PLAYER_ROWS: &str = include_str!("../tests/fixtures/player.jsonl");

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ctrld-stats-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn append(path: &Path, lines: &[&str]) {
        let mut f = OpenOptions::new().create(true).append(true).open(path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn live<'a>(dir: &'a Path) -> Ctx<'a> {
        Ctx { live: true, running: true, session: Some((1, dir)) }
    }

    #[test]
    fn tailer_keeps_partial_lines_and_follows_truncation() {
        let d = tmpdir("tail");
        let p = d.join("x.jsonl");
        let mut t = Tailer::new(p.clone(), None);
        assert!(t.lines().is_empty());
        fs::write(&p, "a\nb\npart").unwrap();
        assert_eq!(t.lines(), ["a", "b"]);
        append(&p, &["ial", "c"]);
        assert_eq!(t.lines(), ["partial", "c"]);
        fs::write(&p, "new\n").unwrap();
        assert_eq!(t.lines(), ["new"]);
        // over the cap: emptied once read; appending writers carry on from 0
        let mut capped = Tailer::new(p.clone(), Some(8));
        append(&p, &["0123456789"]);
        assert_eq!(capped.lines(), ["new", "0123456789"]);
        assert_eq!(fs::metadata(&p).unwrap().len(), 0);
        append(&p, &["next"]);
        assert_eq!(capped.lines(), ["next"]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn tx_rates_target_margin_and_bitrate_events() {
        let d = tmpdir("tx");
        let fast: Vec<&str> = FAST_ROWS.lines().collect();
        let ticks: Vec<&str> = TICK_ROWS.lines().collect();
        let mut s = Sampler::new(Role::Tx, 9500, 3.0, None);
        let t0 = Instant::now();

        append(&d.join(FAST), &fast[..11]);
        append(&d.join(TICKS), &ticks[..2]);
        let o = s.sample(live(&d), t0);
        assert_eq!(o.stats.send_mbps, None, "a rate needs the previous second");
        assert_eq!(o.stats.actual_mbps, Some(3.5), "1 s of input is enough for the video rate");
        assert_eq!(o.stats.target_mbps, Some(3.5));
        assert_eq!(o.stats.delay_margin_s, Some(6.4));
        assert_eq!(o.stats.bandwidth_mbps, None);
        assert_eq!(o.stats.delay_margin_warn_s, Some(3.0));
        assert_eq!(o.stats.retrans_pct, None);
        assert_eq!(o.stats.rtt_ms, Some(1.0));

        append(&d.join(FAST), &fast[11..21]);
        append(&d.join(TICKS), &ticks[2..]);
        append(&d.join(EVENTS), &EVENT_ROWS.lines().collect::<Vec<_>>());
        let o = s.sample(live(&d), t0 + Duration::from_secs(1));
        // 10 rows of +46240 B in 1.0 s; 10 of 340 packets resent; 20 × 43750 B of video in 2.0 s
        assert_eq!(o.stats.send_mbps, Some(3.7));
        assert_eq!(o.stats.loss_pct, Some(2.9));
        assert_eq!(o.stats.actual_mbps, Some(3.5));
        assert_eq!(o.stats.send_late_5m, Some(0));
        assert_eq!(o.stats.target_mbps, Some(2.5));
        assert_eq!(o.stats.bandwidth_mbps, Some(2.88));
        assert_eq!(o.stats.delay_margin_s, Some(2.9));
        assert_eq!(o.sample[0], Some(3.7));
        let codes: Vec<_> = o.obs.events.iter().map(|e| (e.code, e.params.get("reason").cloned())).collect();
        assert_eq!(
            codes,
            [
                ("bitrate_changed", Some(json!("delay"))),
                ("bitrate_changed", Some(json!("recovered"))),
                ("delay_margin_low", None)
            ]
        );
        assert_eq!(o.obs.events[0].params["mbps"], 2.5);
        assert_eq!(o.obs.events[0].params["from_mbps"], 3.5);
        assert_eq!(o.obs.events[2].params["margin_s"], 2.9);
        assert_eq!(o.obs.margin_low, Some(2.9));
        assert!(!o.obs.ack_silent);

        // no new rows: still shown for 2.5 s, the ACK silence counts from the last progress
        let o = s.sample(live(&d), t0 + Duration::from_secs(2));
        assert_eq!(o.stats.send_mbps, Some(3.7));
        assert!(!o.obs.ack_silent);
        let o = s.sample(live(&d), t0 + Duration::from_secs(4));
        assert_eq!(o.stats.send_mbps, None);
        assert!(o.obs.ack_silent);
        assert_eq!(o.stats.last_rtt_ms, Some(1.0));

        // not live: nothing measured, the margin alarm ends
        let o = s.sample(Ctx { live: false, running: true, session: Some((1, &d)) }, t0 + Duration::from_secs(5));
        assert_eq!((o.stats.target_mbps, o.stats.delay_margin_s), (None, None));
        assert_eq!(o.obs.margin_low, None);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn tx_ack_rule_waits_for_data() {
        let d = tmpdir("ack");
        let mut s = Sampler::new(Role::Tx, 9500, 3.0, None);
        let t0 = Instant::now();
        // connected, nothing sent yet, ACK counter still: not armed
        let idle = r#"{"rc":0,"t_us":1000000,"inMsgs":0,"pktRecvACKTotal":0,"pktFlightSize":0,"pktSndBuf":0}"#;
        append(&d.join(FAST), &[idle]);
        s.sample(live(&d), t0);
        let o = s.sample(live(&d), t0 + Duration::from_secs(5));
        assert!(!o.obs.ack_silent);
        let busy = r#"{"rc":0,"t_us":6000000,"inMsgs":40,"pktRecvACKTotal":0,"pktFlightSize":30,"pktSndBuf":40}"#;
        append(&d.join(FAST), &[busy]);
        let o = s.sample(live(&d), t0 + Duration::from_secs(6));
        assert!(o.obs.ack_silent, "data outstanding and no ACK progress since t0");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn rx_rates_skip_null_rows_and_count_stops() {
        let d = tmpdir("rx");
        let rows: Vec<&str> = RCV_ROWS.lines().collect();
        let players: Vec<&str> = PLAYER_ROWS.lines().collect();
        let mut s = Sampler::new(Role::Rx, 9500, 3.0, None);
        let t0 = Instant::now();
        append(&d.join(RCV), &rows[..1]);
        s.player_line(1, players[0], t0);
        let o = s.sample(live(&d), t0);
        assert_eq!(o.stats.recv_mbps, None);
        assert_eq!(o.stats.delay_margin_warn_s, None);
        assert!(o.obs.first_frames);
        assert!(!o.obs.black);

        // a failed stats row (null counters) is skipped
        append(&d.join(RCV), &rows[1..3]);
        s.player_line(1, players[1], t0 + Duration::from_secs(1));
        let o = s.sample(live(&d), t0 + Duration::from_secs(1));
        assert_eq!(o.stats.recv_mbps, Some(3.7));
        assert_eq!(o.stats.loss_pct, Some(2.0));
        assert_eq!(o.stats.retrans_pct, Some(2.1));
        assert_eq!(o.stats.rtt_ms, Some(1.0));
        assert_eq!(o.stats.unrecovered_5m, Some(0));
        assert_eq!(o.stats.stalls_5m, Some(0));
        assert!(!o.obs.first_frames, "reported once per session");

        // a stop: counted at once, reported as an event once a later line shows it has ended
        s.player_line(1, players[2], t0 + Duration::from_secs(2));
        let o = s.sample(live(&d), t0 + Duration::from_secs(2));
        assert_eq!(o.stats.stalls_5m, Some(1));
        assert_eq!(o.stats.stall_s_5m, Some(0.4));
        assert_eq!(o.sample[8], Some(0.43));
        assert!(o.obs.events.is_empty());
        let (count, secs, _) = o.obs.stalled.unwrap();
        assert_eq!((count, secs), (1, 0.4));
        s.player_line(1, players[3], t0 + Duration::from_secs(3));
        let o = s.sample(live(&d), t0 + Duration::from_secs(3));
        assert_eq!(o.obs.events.len(), 1);
        assert_eq!(o.obs.events[0].code, "video_stalled");
        assert_eq!(o.obs.events[0].params["seconds"], 0.4);
        assert_eq!((o.stats.stalls_5m, o.stats.stall_s_5m, o.sample[8]), (Some(1), Some(0.4), Some(0.0)));

        // no frames for more than 1.5 s: black
        let o = s.sample(live(&d), t0 + Duration::from_secs(5));
        assert!(o.obs.black);
        assert_eq!(o.sample[8], Some(0.0));
        fs::remove_dir_all(&d).unwrap();
    }

    /// (stalls_5m, stall_s_5m, the alert's (count, seconds))
    type StopTotals = (Option<i64>, Option<f64>, Option<(usize, f64)>);

    /// Feeds real player lines (one per second) and samples after each; returns the events
    /// (code, seconds), the history stop seconds and the last totals.
    fn run_player(name: &str, lines: &str) -> (Vec<(&'static str, f64)>, Vec<f64>, StopTotals) {
        let d = tmpdir(name);
        let mut s = Sampler::new(Role::Rx, 9500, 3.0, None);
        let t0 = Instant::now();
        let (mut events, mut hist, mut last) = (Vec::new(), Vec::new(), (None, None, None));
        for (k, l) in lines.lines().enumerate() {
            let now = t0 + Duration::from_secs(k as u64 + 1);
            s.player_line(1, l, now);
            let o = s.sample(live(&d), now);
            hist.push(o.sample[8].unwrap());
            events.extend(o.obs.events.iter().map(|e| (e.code, e.params["seconds"].as_f64().unwrap())));
            last = (o.stats.stalls_5m, o.stats.stall_s_5m, o.obs.stalled.map(|x| (x.0, x.1)));
        }
        fs::remove_dir_all(&d).unwrap();
        (events, hist, last)
    }

    #[test]
    fn rx_long_stop_is_followed_while_open_and_reported_whole() {
        // ll_player RenderStats: play 0-3 s, picture still 3-9.5 s, play to 13 s
        let lines = include_str!("../tests/fixtures/player_freeze.jsonl");
        let d = tmpdir("freeze-open");
        let mut s = Sampler::new(Role::Rx, 9500, 3.0, None);
        let t0 = Instant::now();
        for (k, l) in lines.lines().take(7).enumerate() {
            s.player_line(1, l, t0 + Duration::from_secs(k as u64 + 1));
        }
        // 4 s into the stop: the alert and the totals already show it, no event yet
        let o = s.sample(live(&d), t0 + Duration::from_secs(7));
        assert_eq!((o.stats.stalls_5m, o.stats.stall_s_5m), (Some(1), Some(4.0)));
        assert_eq!(o.obs.stalled.map(|x| (x.0, x.1)), Some((1, 4.0)));
        assert!(o.obs.events.is_empty());
        fs::remove_dir_all(&d).unwrap();

        let (events, hist, last) = run_player("freeze", lines);
        assert_eq!(events, [("video_stalled", 6.5)]);
        assert_eq!(last, (Some(1), Some(6.5), Some((1, 6.5))));
        assert!((hist.iter().sum::<f64>() - 6.5).abs() < 0.02, "history: {hist:?}");
    }

    #[test]
    fn rx_back_to_back_stops_are_split() {
        // ll_player RenderStats: still 2.0-4.5 s, 6 frames, still 4.7-5.2 s (begins in the second the first ends)
        let (events, hist, last) = run_player("choppy", include_str!("../tests/fixtures/player_choppy.jsonl"));
        assert_eq!(events, [("video_stalled", 2.5), ("video_stalled", 0.5)]);
        assert_eq!(last, (Some(2), Some(3.0), Some((2, 3.0))));
        assert!((hist.iter().sum::<f64>() - 3.0).abs() < 0.02, "history: {hist:?}");
    }

    #[test]
    fn rx_one_missing_frame_is_not_a_stop() {
        // ll_player RenderStats: a 29.9 fps camera, one source frame missing (66.9 ms gap = 1 stall for the player)
        let (events, hist, last) = run_player("drop", include_str!("../tests/fixtures/player_drop.jsonl"));
        assert!(events.is_empty());
        assert_eq!(last, (Some(0), Some(0.0), None));
        assert!(hist.iter().all(|h| *h == 0.0));
    }

    #[test]
    fn rx_player_exit_ends_its_stop_and_late_lines_are_ignored() {
        let lines: Vec<&str> = include_str!("../tests/fixtures/player_freeze.jsonl").lines().collect();
        let d = tmpdir("exit");
        let mut s = Sampler::new(Role::Rx, 9500, 3.0, None);
        let t0 = Instant::now();
        for (k, l) in lines[..6].iter().enumerate() {
            s.player_line(1, l, t0 + Duration::from_secs(k as u64 + 1));
        }
        s.player_done(1, t0 + Duration::from_secs(6));
        // the player's last line arrives after its exit: ignored (it would count the stop again)
        s.player_line(1, lines[6], t0 + Duration::from_secs(6));
        let o = s.sample(live(&d), t0 + Duration::from_secs(6));
        let ev: Vec<_> = o.obs.events.iter().map(|e| (e.code, e.params["seconds"].as_f64().unwrap())).collect();
        assert_eq!(ev, [("video_stalled", 3.0)]);
        assert_eq!((o.stats.stalls_5m, o.stats.stall_s_5m), (Some(1), Some(3.0)));
        // a line from an older player that was never seen is ignored too
        s.player_line(2, lines[0], t0 + Duration::from_secs(7));
        s.player_line(1, lines[3], t0 + Duration::from_secs(7));
        let o = s.sample(live(&d), t0 + Duration::from_secs(7));
        assert!(o.obs.events.is_empty());
        assert_eq!(o.stats.stalls_5m, Some(1));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn tx_margin_alert_follows_the_stats_while_clearing() {
        let d = tmpdir("margin");
        let mut s = Sampler::new(Role::Tx, 9500, 3.0, None);
        let t0 = Instant::now();
        let tick = |b: f64| format!(r#"{{"cur":3500,"nd_budget":{b},"nd_c":null}}"#);
        let mut seen = Vec::new();
        for (k, b) in [2.5, 3.2, 3.4, 3.6, 3.6].iter().enumerate() {
            append(&d.join(TICKS), &[&tick(*b)]);
            let o = s.sample(live(&d), t0 + Duration::from_secs(k as u64));
            seen.push((o.stats.delay_margin_s, o.obs.margin_low));
        }
        assert_eq!(
            seen,
            [
                (Some(2.5), Some(2.5)),
                (Some(3.2), Some(3.2)),
                (Some(3.4), Some(3.4)),
                (Some(3.6), None),
                (Some(3.6), None)
            ]
        );
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn reasons() {
        assert_eq!(reason("loss", 3500.0, 3000.0), "loss");
        assert_eq!(reason("nd-budget", 3500.0, 2500.0), "delay");
        assert_eq!(reason("prot-re", 3500.0, 2500.0), "delay");
        assert_eq!(reason("probe-revert", 3000.0, 2500.0), "delay");
        assert_eq!(reason("buffer_mode", 3000.0, 2500.0), "delay");
        assert_eq!(reason("panic", 3500.0, 1500.0), "loss");
        assert_eq!(reason("nd-up", 2500.0, 3000.0), "recovered");
        assert_eq!(reason("loss", 2500.0, 3000.0), "recovered");
        assert_eq!(reason("manual", 2500.0, 3000.0), "manual");
    }
}
