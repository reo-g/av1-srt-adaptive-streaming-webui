//! Link state machine, one per role. It owns the sessions (one chain of child processes each)
//! and decides what to spawn, stop or retry; the media backend does the process work. Pure
//! logic with explicit times, so it is tested against a fake backend.
//!
//! TX: check the input → sender → (sender says it is connected) bridge → live.
//!     Sender exit → reconnecting, the next try 5 s after the previous one started.
//!     A missing or busy camera → input_lost and a backoff, never a connect loop. The input
//!     counts as working only once the bridge reports frames ("@evt" input ok), and the backoff
//!     starts again only after it has kept working for INPUT_STABLE: a camera that opens but
//!     fails once it streams waits longer each time instead of reconnecting every 5 s.
//! RX: player → receiver → (accepted) live. Receiver exit → the player plays out what it
//!     holds while a new pair starts listening. The player always starts with the standard
//!     profile; the receiver prints the transmitter's stream id right before it accepts, and a
//!     low-delay transmitter is followed by writing the low profile's player_ctl to the player.
//!     The monitor output (受信機のモニター) goes to every player right after its start, and to
//!     the current one whenever it is applied; while the player says it could not open it, it is
//!     written again every DISPLAY_RETRY.

use crate::config::Role;
use crate::events::Ev;
use crate::model::{params, DelayProfile, DisplayMode, LinkState, Params, SIGNAL_FORMAT};
use crate::profile::{Input, Plan};
use crate::stats::{Obs, STALL_WINDOW_S};
use serde_json::{json, Value};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const RETRY_PERIOD: Duration = Duration::from_secs(5);
const RETRY_MIN_WAIT: Duration = Duration::from_secs(1);
const STOP_TERM_AFTER: Duration = Duration::from_secs(6);
const STOP_KILL_AFTER: Duration = Duration::from_secs(8);
const INPUT_BACKOFF_S: [u64; 4] = [5, 10, 20, 30];
/// The input has delivered frames this long: the input backoff starts again from its first step.
const INPUT_STABLE: Duration = Duration::from_secs(10);
/// A bridge that reports no input events at all (control input off): the input is taken as
/// working once the link has been live this long.
const INPUT_ASSUME_OK: Duration = Duration::from_secs(10);
/// Markers the bridge writes on stderr; other prints are not single writes, so a marker can
/// follow an unterminated line and is looked for anywhere in a line.
const EVT_MARK: &str = "@evt ";
const SRC_ERR_MARK: &str = "[SRC ERR]";
/// The receiver's line with the caller's stream id, right before its "accepted from" line.
const STREAM_ID_MARK: &str = "[srt-recv] stream id ";
const RX_RESPAWN: Duration = Duration::from_millis(500);
const RX_RESPAWN_FAILED: Duration = Duration::from_secs(5);
/// An old player may play out what it holds (about the delay) before it is stopped.
const RX_PLAYOUT_MAX: Duration = Duration::from_secs(30);
/// rx: while the player says it could not open the monitor output (the desktop may not be up yet,
/// or nobody is logged in), its line is written again at most this often.
pub const DISPLAY_RETRY: Duration = Duration::from_secs(30);
pub const STOP_LINE: &str = r#"{"cmd":"stop"}"#;

/// The player's control line for the monitor output (answered `[player] ctl display <mode> ok`).
pub fn display_line(d: DisplayMode) -> String {
    format!("display {}", d.name())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Sender,
    Bridge,
    Receiver,
    Player,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Sender => "sender",
            Kind::Bridge => "bridge",
            Kind::Receiver => "receiver",
            Kind::Player => "player",
        }
    }
}

#[derive(Debug)]
pub enum MediaEvent {
    /// A line a child wrote on stderr.
    Line { session: u64, kind: Kind, line: String },
    /// A line from the player's stats fd.
    Stats { session: u64, line: String },
    Exit { session: u64, kind: Kind, code: Option<i32> },
}

pub trait MediaBackend {
    /// TX: Ok when the input can be opened (the camera exists and is not in use, the file exists).
    fn check_input(&mut self, input: &Input) -> Result<(), String>;
    fn spawn(&mut self, session: &Session, kind: Kind, plan: &Plan) -> io::Result<()>;
    /// A line to the bridge's control input; None closes it (the bridge then stops too).
    fn control(&mut self, session: u64, line: Option<&str>);
    /// rx: a line to the player's control input (--ctl-fd).
    fn player_control(&mut self, session: u64, line: &str);
    /// SIGTERM (or SIGKILL) to the child's process group.
    fn signal(&mut self, session: u64, kind: Kind, kill: bool);
    /// The session is over: drop the pipe ends the daemon still holds for it.
    fn release(&mut self, session: u64);
}

#[derive(Debug, Clone)]
pub struct AlertEntry {
    pub level: &'static str,
    pub code: &'static str,
    pub since: Instant,
    pub params: Params,
}

#[derive(Debug, Clone)]
pub struct Link {
    pub state: LinkState,
    pub since: Instant,
    pub attempt: Option<u32>,
    pub next_try: Option<Instant>,
    pub last_live_until: Option<Instant>,
    /// rx: the transmitter's address from the receiver's "accepted from" line
    pub peer_addr: Option<(String, u16)>,
    /// The delay profile of the transmission: tx the plan's (set at each start), rx the
    /// transmitter's of the current or last connection (standard before any; memory only).
    pub delay: DelayProfile,
    pub alerts: Vec<AlertEntry>,
    /// rx: no frame rendered for a second
    pub rx_black: bool,
    /// Bumped on every change, so the status is pushed when it changes.
    pub rev: u64,
}

impl Link {
    pub fn has(&self, code: &str) -> bool {
        self.alerts.iter().any(|a| a.code == code)
    }
}

#[derive(Debug)]
pub struct Session {
    pub id: u64,
    pub dir: PathBuf,
    pub started: Instant,
    alive: Vec<Kind>,
    /// tx: the sender is connected; rx: a transmitter was accepted
    connected: bool,
    /// tx: the bridge reported an input error
    src_err: bool,
    /// tx: when the bridge was started (the link is live)
    live_at: Option<Instant>,
    /// tx: the bridge reported an input event
    evt_seen: bool,
    /// tx: since when the input delivers frames (None while it does not)
    input_ok_at: Option<Instant>,
    /// rx: the profile the transmitter's stream id names (None until the receiver printed one)
    peer_delay: Option<DelayProfile>,
    /// rx: when the monitor output was last written to the player
    display_sent: Option<Instant>,
    term_at: Option<Instant>,
    kill_at: Option<Instant>,
}

impl Session {
    fn has(&self, k: Kind) -> bool {
        self.alive.contains(&k)
    }
    fn stop_by(&mut self, now: Instant) {
        self.term_at = Some(now + STOP_TERM_AFTER);
        self.kill_at = Some(now + STOP_KILL_AFTER);
    }
}

/// The sender's stderr line once the connection is up: "[srt-sender] nodrop: connected, ..." (only
/// with SRT_NODROP=1), else its fast-stats line, which it prints right after connecting.
fn is_connected_line(line: &str) -> bool {
    line.starts_with("[srt-sender]") && (line.contains(" connected") || line.starts_with("[srt-sender] fast stats:"))
}

/// The profile a transmitter's stream id names (profile.rs `stream_id`): tokens separated by ';' or
/// white space, the first `delay=…` counts, unknown tokens are ignored. Missing or unknown: standard.
pub fn stream_id_delay(text: &str) -> DelayProfile {
    text.split(|c: char| c == ';' || c.is_whitespace())
        .find_map(|t| t.strip_prefix("delay="))
        .and_then(DelayProfile::parse)
        .unwrap_or(DelayProfile::Standard)
}

pub struct Machine<B: MediaBackend> {
    pub role: Role,
    pub backend: B,
    pub link: Link,
    pub plan: Plan,
    dir: PathBuf,
    cur: Option<Session>,
    old: Vec<Session>,
    next_id: u64,
    backoff: usize,
    last_ack: Option<Instant>,
    /// rx: the monitor output in effect (OutputSettings.display; set_display).
    display: DisplayMode,
    /// Events to log and push (drained by the caller).
    pub out: Vec<Ev>,
    /// Directories of sessions whose children have all exited (drained by the caller).
    pub finished: Vec<PathBuf>,
}

impl<B: MediaBackend> Machine<B> {
    pub fn new(role: Role, backend: B, plan: Plan, dir: PathBuf, now: Instant) -> Self {
        Machine {
            role,
            backend,
            link: Link {
                state: LinkState::Stopped,
                since: now,
                attempt: None,
                next_try: None,
                last_live_until: None,
                peer_addr: None,
                delay: DelayProfile::Standard,
                alerts: Vec::new(),
                rx_black: true,
                rev: 0,
            },
            plan,
            dir,
            cur: None,
            old: Vec::new(),
            next_id: 1,
            backoff: 0,
            last_ack: None,
            display: DisplayMode::Off,
            out: Vec::new(),
            finished: Vec::new(),
        }
    }

    /// rx: the monitor output (OutputSettings.display), applied at once: written to the current
    /// player if one runs (also when it is the same value: applying again is a retry by hand) and
    /// to every player started from now on. Players playing out an earlier connection have no
    /// control input any more and keep what they had until they exit.
    pub fn set_display(&mut self, d: DisplayMode, now: Instant) {
        if self.role != Role::Rx {
            return;
        }
        self.display = d;
        if let Some(s) = self.cur.as_mut().filter(|s| s.has(Kind::Player)) {
            s.display_sent = Some(now);
            let id = s.id;
            self.backend.player_control(id, &display_line(d));
        }
    }

    /// The current session (id and directory), for the sampler.
    pub fn session(&self) -> Option<(u64, &Path)> {
        self.cur.as_ref().map(|s| (s.id, s.dir.as_path()))
    }

    /// No child process left.
    pub fn idle(&self) -> bool {
        self.cur.is_none() && self.old.is_empty()
    }

    fn set_state(&mut self, state: LinkState, since: Instant) {
        if self.link.state != state {
            self.link.state = state;
            self.link.since = since;
            self.link.rev += 1;
        }
    }

    /// Raise or update an alert. `since` None keeps the time it was first raised.
    pub fn set_alert(&mut self, level: &'static str, code: &'static str, p: Params, since: Option<Instant>, now: Instant) {
        if let Some(a) = self.link.alerts.iter_mut().find(|a| a.code == code) {
            let since = since.unwrap_or(a.since);
            if a.params != p || a.since != since {
                (a.params, a.since) = (p, since);
                self.link.rev += 1;
            }
            return;
        }
        self.link.alerts.push(AlertEntry { level, code, since: since.unwrap_or(now), params: p });
        self.link.rev += 1;
    }

    fn set_delay(&mut self, d: DelayProfile) {
        if self.link.delay != d {
            self.link.delay = d;
            self.link.rev += 1;
        }
    }

    pub fn clear_alert(&mut self, code: &str) {
        let n = self.link.alerts.len();
        self.link.alerts.retain(|a| a.code != code);
        if self.link.alerts.len() != n {
            self.link.rev += 1;
        }
    }

    fn new_session(&mut self, now: Instant) -> Session {
        let id = self.next_id;
        self.next_id += 1;
        Session {
            id,
            dir: self.dir.join(format!("{}-s{id:04}", crate::util::stamp())),
            started: now,
            alive: Vec::new(),
            connected: false,
            src_err: false,
            live_at: None,
            evt_seen: false,
            input_ok_at: None,
            peer_delay: None,
            display_sent: None,
            term_at: None,
            kill_at: None,
        }
    }

    /// The session is over for the link; keep it until its children have exited.
    fn retire(&mut self, s: Session) {
        self.backend.release(s.id);
        if s.alive.is_empty() {
            self.finished.push(s.dir);
        } else {
            self.old.push(s);
        }
    }

    // ------------------------------------------------------------------ commands

    pub fn start(&mut self, now: Instant) {
        if self.link.state != LinkState::Stopped {
            return;
        }
        self.link.attempt = None;
        self.backoff = 0;
        if self.role == Role::Tx {
            // the plan is fixed at the start (retries and reconnects keep it)
            self.set_delay(self.plan.delay);
        }
        let first = if self.role == Role::Tx { LinkState::Connecting } else { LinkState::Listening };
        self.set_state(first, now);
        // A start right after a stop waits for the old children (camera, port) in tick().
        self.link.next_try = Some(now);
        self.tick(now);
    }

    pub fn stop(&mut self, now: Instant) {
        if self.link.state == LinkState::Stopped {
            return;
        }
        self.out.push(Ev::info("transport_stopped"));
        self.link.alerts.clear();
        self.link.attempt = None;
        self.link.next_try = None;
        self.link.rev += 1;
        self.set_state(LinkState::Stopped, now);
        if self.role == Role::Rx {
            // Players still playing out the previous session: the output goes black now.
            for s in &mut self.old {
                for &k in &s.alive {
                    self.backend.signal(s.id, k, false);
                }
                (s.term_at, s.kill_at) = (None, Some(now + STOP_KILL_AFTER));
            }
        }
        if let Some(mut s) = self.cur.take() {
            match self.role {
                Role::Tx if s.has(Kind::Bridge) => {
                    // The bridge ends the stream; the sender then sees the end of its input.
                    self.backend.control(s.id, Some(STOP_LINE));
                    self.backend.control(s.id, None);
                }
                Role::Tx if s.has(Kind::Sender) => self.backend.signal(s.id, Kind::Sender, false),
                Role::Tx => {}
                Role::Rx => {
                    for k in [Kind::Receiver, Kind::Player] {
                        if s.has(k) {
                            self.backend.signal(s.id, k, false);
                        }
                    }
                }
            }
            s.stop_by(now);
            self.retire(s);
        }
    }

    /// Try to connect now instead of waiting for the next attempt.
    pub fn retry(&mut self, now: Instant) {
        if matches!(self.link.state, LinkState::Connecting | LinkState::Reconnecting) && self.cur.is_none() {
            self.link.next_try = Some(now);
            self.tick(now);
        }
    }

    /// Deadlines of stopping children, and the next try when it is due.
    pub fn tick(&mut self, now: Instant) {
        let backend = &mut self.backend;
        for s in self.old.iter_mut().chain(self.cur.iter_mut()) {
            if s.term_at.is_some_and(|t| now >= t) {
                s.term_at = None;
                for &k in &s.alive {
                    backend.signal(s.id, k, false);
                }
            }
            if s.kill_at.is_some_and(|t| now >= t) {
                s.kill_at = None;
                for &k in &s.alive {
                    backend.signal(s.id, k, true);
                }
            }
        }
        if self.role == Role::Tx {
            self.tx_input_tick(now);
        }
        if self.cur.is_some() || !self.link.next_try.is_some_and(|t| now >= t) {
            return;
        }
        match (self.role, self.link.state) {
            // the previous chain still holds the camera
            (Role::Tx, LinkState::Connecting | LinkState::Reconnecting) if self.old.is_empty() => self.try_connect(now),
            // the previous receiver still holds the port (old players may play out meanwhile)
            (Role::Rx, LinkState::Listening) if !self.old.iter().any(|s| s.has(Kind::Receiver)) => self.open_pair(now),
            _ => {}
        }
    }

    // ------------------------------------------------------------------ TX

    fn try_connect(&mut self, now: Instant) {
        self.link.next_try = None;
        if let Err(why) = self.backend.check_input(&self.plan.input) {
            dlog!("input not available: {why}");
            self.set_alert("error", "input_lost", Params::new(), None, now);
            self.wait_for_input(now);
            return;
        }
        // The input opens; it counts as working once the bridge reports frames.
        let mut s = self.new_session(now);
        match self.backend.spawn(&s, Kind::Sender, &self.plan) {
            Ok(()) => {
                s.alive.push(Kind::Sender);
                self.cur = Some(s);
            }
            Err(e) => {
                dlog!("session {}: sender: {e}", s.id);
                let started = s.started;
                self.retire(s);
                self.tx_ended(started, false, false, now);
            }
        }
    }

    fn wait_for_input(&mut self, now: Instant) {
        let wait = INPUT_BACKOFF_S[self.backoff.min(INPUT_BACKOFF_S.len() - 1)];
        self.backoff += 1;
        self.link.attempt = None;
        self.set_state(LinkState::Connecting, now);
        self.link.next_try = Some(now + Duration::from_secs(wait));
        self.link.rev += 1;
    }

    /// The input delivers frames (the bridge says so).
    fn input_ok(&mut self, now: Instant) {
        if let Some(s) = self.cur.as_mut() {
            s.input_ok_at.get_or_insert(now);
        }
        if self.link.has("input_lost") {
            self.clear_alert("input_lost");
            self.out.push(Ev::info("input_detected").with(json!({ "format": SIGNAL_FORMAT })));
        }
    }

    /// Input checks that depend on time: the backoff restarts once the input has kept working,
    /// and a bridge without input events is trusted after a while live.
    fn tx_input_tick(&mut self, now: Instant) {
        let Some(s) = self.cur.as_mut() else { return };
        if !s.evt_seen && s.live_at.is_some_and(|t| now >= t + INPUT_ASSUME_OK) {
            s.evt_seen = true;
            dlog!("session {}: no input events from the bridge; taking the input as working", s.id);
            self.input_ok(now);
        }
        if self.cur.as_ref().and_then(|s| s.input_ok_at).is_some_and(|t| now >= t + INPUT_STABLE) {
            self.backoff = 0;
        }
    }

    fn tx_connected(&mut self, now: Instant) {
        let Some(s) = self.cur.as_mut() else { return };
        if s.connected {
            return;
        }
        s.connected = true;
        if let Err(e) = self.backend.spawn(s, Kind::Bridge, &self.plan) {
            dlog!("session {}: bridge: {e}", s.id);
            s.connected = false;
            self.backend.signal(s.id, Kind::Sender, false);
            s.kill_at = Some(now + STOP_KILL_AFTER);
            return;
        }
        s.alive.push(Kind::Bridge);
        s.live_at = Some(now);
        self.link.attempt = None;
        self.link.next_try = None;
        self.clear_alert("peer_unreachable");
        self.set_state(LinkState::Live, now);
        self.out.push(Ev::info("peer_connected"));
        self.out.push(Ev::info("transport_started"));
    }

    fn bridge_line(&mut self, line: &str, now: Instant) {
        if line.contains(SRC_ERR_MARK) {
            if let Some(s) = self.cur.as_mut() {
                s.src_err = true;
            }
        }
        let Some(at) = line.find(EVT_MARK) else { return };
        let v: Value = serde_json::from_str(line[at + EVT_MARK.len()..].trim()).unwrap_or(Value::Null);
        let input = v.get("input").and_then(Value::as_str);
        if let (Some(s), Some(_)) = (self.cur.as_mut(), input) {
            s.evt_seen = true;
        }
        match input {
            Some("lost") => {
                if let Some(s) = self.cur.as_mut() {
                    s.input_ok_at = None;
                }
                self.set_alert("error", "input_lost", Params::new(), None, now);
            }
            Some("ok") => self.input_ok(now),
            _ => {}
        }
    }

    /// The sender exited: the link is gone (or never came up).
    fn tx_ended(&mut self, started: Instant, was_live: bool, src_err: bool, now: Instant) {
        if was_live {
            self.out.push(Ev::warn("peer_disconnected"));
            self.link.last_live_until = Some(self.last_ack.filter(|t| *t >= started).unwrap_or(now));
            self.link.rev += 1;
        }
        if src_err {
            self.set_alert("error", "input_lost", Params::new(), None, now);
            self.wait_for_input(now);
            return;
        }
        let first = was_live || self.link.state != LinkState::Reconnecting;
        self.link.attempt = Some(if first { 1 } else { self.link.attempt.unwrap_or(0) + 1 });
        if !was_live {
            self.set_alert("error", "peer_unreachable", Params::new(), None, now);
        }
        let since = if was_live { self.link.last_live_until.unwrap_or(now) } else { now };
        self.set_state(LinkState::Reconnecting, since);
        self.link.next_try = Some((started + RETRY_PERIOD).max(now + RETRY_MIN_WAIT));
        self.link.rev += 1;
    }

    // ------------------------------------------------------------------ RX

    fn open_pair(&mut self, now: Instant) {
        self.link.next_try = None;
        let mut s = self.new_session(now);
        // The player first: the receiver opens its pipe before it listens.
        if let Err(e) = self.backend.spawn(&s, Kind::Player, &self.plan) {
            dlog!("session {}: player: {e}", s.id);
            self.retire(s);
            self.link.next_try = Some(now + RX_RESPAWN_FAILED);
            return;
        }
        s.alive.push(Kind::Player);
        // The monitor output first (also off), before anything the transmitter's stream id brings.
        self.backend.player_control(s.id, &display_line(self.display));
        s.display_sent = Some(now);
        if let Err(e) = self.backend.spawn(&s, Kind::Receiver, &self.plan) {
            dlog!("session {}: receiver: {e}", s.id);
            self.backend.signal(s.id, Kind::Player, false);
            s.kill_at = Some(now + STOP_KILL_AFTER);
            self.retire(s);
            self.link.next_try = Some(now + RX_RESPAWN_FAILED);
            return;
        }
        s.alive.push(Kind::Receiver);
        self.cur = Some(s);
    }

    fn receiver_line(&mut self, line: &str, now: Instant) {
        if let Some(at) = line.find(STREAM_ID_MARK) {
            self.stream_id(line[at + STREAM_ID_MARK.len()..].trim());
            return;
        }
        if line.contains("listening on") {
            self.out.push(Ev::info("listening_started").with(json!({ "port": self.plan.listen_port })));
            return;
        }
        let Some((_, from)) = line.split_once("accepted from ") else { return };
        let Some(s) = self.cur.as_mut() else { return };
        if s.connected {
            return;
        }
        s.connected = true;
        // no stream id before the accepted line: a transmitter that does not say, standard
        let delay = s.peer_delay.unwrap_or(DelayProfile::Standard);
        if let Some((addr, port)) = from.trim().rsplit_once(':') {
            self.link.peer_addr = Some((addr.to_string(), port.parse().unwrap_or(0)));
        }
        self.set_delay(delay);
        self.set_state(LinkState::Live, now);
        self.out.push(Ev::info("peer_connected"));
    }

    /// The transmitter's stream id (before the accepted line): a low-delay transmitter is followed
    /// by writing the low profile's player_ctl to the player before any video arrives.
    fn stream_id(&mut self, text: &str) {
        let Some(s) = self.cur.as_mut() else { return };
        if s.connected || s.peer_delay.is_some() {
            return;
        }
        let delay = stream_id_delay(text);
        s.peer_delay = Some(delay);
        let id = s.id;
        dlog!("session {id}: the transmitter's stream id {text:?}: profile {}", delay.name());
        if delay == DelayProfile::Low {
            for l in &self.plan.low_ctl {
                self.backend.player_control(id, l);
            }
        }
        self.set_delay(delay);
    }

    // ------------------------------------------------------------------ children

    pub fn on_media(&mut self, ev: MediaEvent, now: Instant) {
        match ev {
            MediaEvent::Line { session, kind, line } => {
                if self.cur.as_ref().map(|s| s.id) != Some(session) {
                    return;
                }
                match (self.role, kind) {
                    (Role::Tx, Kind::Sender) if is_connected_line(&line) => self.tx_connected(now),
                    (Role::Tx, Kind::Bridge) => self.bridge_line(&line, now),
                    (Role::Rx, Kind::Receiver) => self.receiver_line(&line, now),
                    _ => {}
                }
            }
            MediaEvent::Exit { session, kind, code } => self.on_exit(session, kind, code, now),
            MediaEvent::Stats { .. } => {}
        }
    }

    fn on_exit(&mut self, session: u64, kind: Kind, code: Option<i32>, now: Instant) {
        dlog!("session {session}: {} exited ({})", kind.name(), code.map_or("signal".into(), |c| c.to_string()));
        if let Some(i) = self.old.iter().position(|s| s.id == session) {
            self.old[i].alive.retain(|k| *k != kind);
            if self.old[i].alive.is_empty() {
                let s = self.old.remove(i);
                self.finished.push(s.dir);
            }
            return;
        }
        let Some(s) = self.cur.as_mut().filter(|s| s.id == session) else { return };
        s.alive.retain(|k| *k != kind);
        match (self.role, kind) {
            (Role::Tx, Kind::Bridge) => {
                // The sender sees the end of its input and drains (SRT_DRAIN_MAX_MS); then it exits.
                s.stop_by(now);
            }
            (Role::Tx, Kind::Sender) => {
                let Some(mut s) = self.cur.take() else { return };
                if s.has(Kind::Bridge) {
                    self.backend.control(s.id, None);
                    s.stop_by(now);
                }
                let (started, was_live, src_err) = (s.started, s.connected, s.src_err);
                self.retire(s);
                self.tx_ended(started, was_live, src_err, now);
            }
            (Role::Rx, Kind::Receiver) => {
                let Some(mut s) = self.cur.take() else { return };
                let (was_live, lived) = (s.connected, now.duration_since(s.started));
                if s.has(Kind::Player) {
                    // The player got the end of its input: it plays out what it holds, then exits.
                    s.term_at = Some(now + RX_PLAYOUT_MAX);
                    s.kill_at = Some(now + RX_PLAYOUT_MAX + Duration::from_secs(2));
                }
                self.retire(s);
                if was_live {
                    self.out.push(Ev::warn("peer_disconnected"));
                    self.link.last_live_until = Some(now);
                    self.link.rev += 1;
                }
                self.set_state(LinkState::Listening, now);
                let quick = was_live || lived >= Duration::from_secs(2);
                self.link.next_try = Some(now + if quick { RX_RESPAWN } else { RX_RESPAWN_FAILED });
            }
            (Role::Rx, Kind::Player) if s.has(Kind::Receiver) => {
                // Nothing would show what arrives: restart the pair (the receiver's exit does it).
                self.backend.signal(s.id, Kind::Receiver, false);
                s.kill_at = Some(now + STOP_KILL_AFTER);
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------ what the sampler saw

    pub fn observe(&mut self, mut obs: Obs, now: Instant) {
        self.last_ack = obs.last_ack;
        let live = self.link.state == LinkState::Live;
        if self.role == Role::Tx && live {
            if obs.ack_silent {
                self.set_alert("error", "peer_unreachable", Params::new(), None, now);
            } else {
                self.clear_alert("peer_unreachable");
            }
        }
        match obs.margin_low {
            Some(m) if live => self.set_alert("warn", "delay_margin_low", params(json!({ "margin_s": m })), None, now),
            _ => self.clear_alert("delay_margin_low"),
        }
        match obs.stalled {
            Some((count, seconds, first)) if self.link.state != LinkState::Stopped => {
                let p = params(json!({ "count": count, "seconds": seconds, "window_s": STALL_WINDOW_S }));
                self.set_alert("warn", "video_stalled", p, Some(first), now);
            }
            _ => self.clear_alert("video_stalled"),
        }
        if self.role == Role::Rx {
            if self.link.rx_black != obs.black {
                self.link.rx_black = obs.black;
                self.link.rev += 1;
            }
            if obs.first_frames && live {
                self.out.push(Ev::info("transport_started").with(json!({ "format": SIGNAL_FORMAT })));
            }
            self.retry_display(obs.player_display.as_deref(), now);
        }
        self.out.append(&mut obs.events);
    }

    /// rx: the current player said it could not open the monitor output while it is on: write the
    /// line again, at most every DISPLAY_RETRY after the last time it was written.
    fn retry_display(&mut self, reported: Option<&str>, now: Instant) {
        if !self.display.is_on() || reported != Some("failed") {
            return;
        }
        let Some(s) = self.cur.as_mut().filter(|s| s.has(Kind::Player)) else { return };
        if s.display_sent.is_some_and(|t| now < t + DISPLAY_RETRY) {
            return;
        }
        s.display_sent = Some(now);
        let id = s.id;
        dlog!("session {id}: the player could not open the monitor output; asking again");
        self.backend.player_control(id, &display_line(self.display));
    }
}

// ------------------------------------------------------------------ tests with a fake backend

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        calls: Vec<String>,
        input_err: Option<String>,
    }

    impl MediaBackend for Fake {
        fn check_input(&mut self, _: &Input) -> Result<(), String> {
            self.calls.push("check".into());
            self.input_err.clone().map_or(Ok(()), Err)
        }
        fn spawn(&mut self, s: &Session, k: Kind, _: &Plan) -> io::Result<()> {
            self.calls.push(format!("spawn {} {}", s.id, k.name()));
            Ok(())
        }
        fn control(&mut self, s: u64, line: Option<&str>) {
            self.calls.push(format!("ctl {s} {}", line.unwrap_or("close")));
        }
        fn player_control(&mut self, s: u64, line: &str) {
            self.calls.push(format!("player {s} {line}"));
        }
        fn signal(&mut self, s: u64, k: Kind, kill: bool) {
            self.calls.push(format!("{} {s} {}", if kill { "kill" } else { "term" }, k.name()));
        }
        fn release(&mut self, s: u64) {
            self.calls.push(format!("release {s}"));
        }
    }

    fn machine(role: Role) -> (Machine<Fake>, Instant) {
        let plan = Plan {
            peer_host: "10.0.0.2".into(),
            peer_port: 60021,
            listen_port: 60021,
            input: Input::Camera("/dev/video0".into()),
            manual_kbps: None,
            maxbw_kbps: 5000,
            delay: DelayProfile::Standard,
            low_ctl: vec!["ts-offset-ms 400".into(), "adapt on".into()],
        };
        let t0 = Instant::now();
        (Machine::new(role, Fake::default(), plan, PathBuf::from("/run/ctrld"), t0), t0)
    }

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    fn calls(m: &mut Machine<Fake>) -> Vec<String> {
        std::mem::take(&mut m.backend.calls)
    }

    fn events(m: &mut Machine<Fake>) -> Vec<&'static str> {
        m.out.drain(..).map(|e| e.code).collect()
    }

    fn line(m: &mut Machine<Fake>, session: u64, kind: Kind, text: &str, now: Instant) {
        m.on_media(MediaEvent::Line { session, kind, line: text.into() }, now);
    }

    fn exit(m: &mut Machine<Fake>, session: u64, kind: Kind, now: Instant) {
        m.on_media(MediaEvent::Exit { session, kind, code: Some(0) }, now);
    }

    const CONNECTED: &str = "[srt-sender] nodrop: connected, tlpktdrop=0 snddropdelay=-1";

    fn go_live(m: &mut Machine<Fake>, t0: Instant) {
        m.start(t0);
        line(m, 1, Kind::Sender, "[srt-sender] connecting to 10.0.0.2:60021 (latency=8000 ms)", at(t0, 100));
        line(m, 1, Kind::Sender, CONNECTED, at(t0, 300));
    }

    #[test]
    fn tx_connects_then_goes_live() {
        let (mut m, t0) = machine(Role::Tx);
        m.start(t0);
        assert_eq!(m.link.state, LinkState::Connecting);
        assert_eq!(calls(&mut m), ["check", "spawn 1 sender"]);
        line(&mut m, 1, Kind::Sender, "[srt-sender] connecting to 10.0.0.2:60021 (latency=8000 ms)", at(t0, 100));
        assert!(calls(&mut m).is_empty(), "the bridge waits for the connection");
        line(&mut m, 1, Kind::Sender, CONNECTED, at(t0, 300));
        assert_eq!(calls(&mut m), ["spawn 1 bridge"]);
        assert_eq!(m.link.state, LinkState::Live);
        assert_eq!(m.link.since, at(t0, 300));
        assert_eq!(events(&mut m), ["peer_connected", "transport_started"]);
        // a second start does nothing
        m.start(at(t0, 400));
        assert!(calls(&mut m).is_empty());
    }

    #[test]
    fn connected_lines() {
        assert!(is_connected_line(CONNECTED));
        assert!(is_connected_line("[srt-sender] fast stats: /run/s1/fast.jsonl every 100 ms (jsonl=1)"));
        assert!(!is_connected_line("[srt-sender] connecting to 10.0.0.2:60021 (latency=8000 ms)"));
        assert!(!is_connected_line("srt_connect: Connection setup failure: connection timed out"));
    }

    #[test]
    fn tx_connect_failures_retry_every_5s() {
        let (mut m, t0) = machine(Role::Tx);
        m.start(t0);
        exit(&mut m, 1, Kind::Sender, at(t0, 3000));
        assert_eq!(m.link.state, LinkState::Reconnecting);
        assert_eq!(m.link.attempt, Some(1));
        assert_eq!(m.link.next_try, Some(at(t0, 5000)));
        assert!(m.link.has("peer_unreachable"));
        assert_eq!(m.finished.len(), 1);
        m.tick(at(t0, 4900));
        assert_eq!(calls(&mut m), ["check", "spawn 1 sender", "release 1"]);
        m.tick(at(t0, 5000));
        assert_eq!(calls(&mut m), ["check", "spawn 2 sender"]);
        // the first connect try fails fast: still at least 1 s between tries
        exit(&mut m, 2, Kind::Sender, at(t0, 9500));
        assert_eq!(m.link.attempt, Some(2));
        assert_eq!(m.link.next_try, Some(at(t0, 10_500)));
        assert_eq!(m.link.since, at(t0, 3000), "since stays at the start of reconnecting");
        // retry now
        m.retry(at(t0, 9600));
        assert_eq!(calls(&mut m), ["release 2", "check", "spawn 3 sender"]);
        line(&mut m, 3, Kind::Sender, CONNECTED, at(t0, 9900));
        assert_eq!((m.link.state, m.link.attempt), (LinkState::Live, None));
        assert!(!m.link.has("peer_unreachable"));
    }

    #[test]
    fn tx_link_lost_after_ack_silence() {
        let (mut m, t0) = machine(Role::Tx);
        go_live(&mut m, t0);
        calls(&mut m);
        events(&mut m);
        let last_ack = at(t0, 60_000);
        m.observe(Obs { ack_silent: true, last_ack: Some(last_ack), ..Obs::default() }, at(t0, 63_000));
        assert!(m.link.has("peer_unreachable"));
        assert_eq!(m.link.state, LinkState::Live, "stays live within the peer idle time");
        // the sender gives up after the idle timeout
        exit(&mut m, 1, Kind::Sender, at(t0, 90_000));
        assert_eq!(calls(&mut m), ["ctl 1 close", "release 1"]);
        assert_eq!(m.link.state, LinkState::Reconnecting);
        assert_eq!(m.link.since, last_ack);
        assert_eq!(m.link.last_live_until, Some(last_ack));
        assert_eq!(m.link.attempt, Some(1));
        assert_eq!(m.link.next_try, Some(at(t0, 91_000)));
        assert_eq!(events(&mut m), ["peer_disconnected"]);
        // the next try waits for the old bridge (it holds the camera)
        m.tick(at(t0, 91_000));
        assert!(calls(&mut m).is_empty());
        exit(&mut m, 1, Kind::Bridge, at(t0, 91_500));
        assert_eq!(m.finished.len(), 1);
        m.tick(at(t0, 91_600));
        assert_eq!(calls(&mut m), ["check", "spawn 2 sender"]);
    }

    #[test]
    fn tx_stop_is_graceful_then_forced() {
        let (mut m, t0) = machine(Role::Tx);
        go_live(&mut m, t0);
        m.set_alert("warn", "delay_margin_low", Params::new(), None, t0);
        calls(&mut m);
        events(&mut m);
        m.stop(at(t0, 10_000));
        assert_eq!(m.link.state, LinkState::Stopped);
        assert!(m.link.alerts.is_empty());
        assert_eq!(events(&mut m), ["transport_stopped"]);
        assert_eq!(calls(&mut m), [r#"ctl 1 {"cmd":"stop"}"#, "ctl 1 close", "release 1"]);
        // a start meanwhile waits for the old chain
        m.start(at(t0, 11_000));
        assert_eq!(m.link.state, LinkState::Connecting);
        assert!(calls(&mut m).is_empty());
        m.tick(at(t0, 15_900));
        assert!(calls(&mut m).is_empty());
        m.tick(at(t0, 16_000));
        assert_eq!(calls(&mut m), ["term 1 sender", "term 1 bridge"]);
        m.tick(at(t0, 18_000));
        assert_eq!(calls(&mut m), ["kill 1 sender", "kill 1 bridge"]);
        exit(&mut m, 1, Kind::Bridge, at(t0, 18_010));
        exit(&mut m, 1, Kind::Sender, at(t0, 18_020));
        assert!(m.idle());
        m.tick(at(t0, 18_200));
        assert_eq!(calls(&mut m), ["check", "spawn 2 sender"]);
    }

    #[test]
    fn tx_stop_while_connecting_terminates_the_sender() {
        let (mut m, t0) = machine(Role::Tx);
        m.start(t0);
        calls(&mut m);
        m.stop(at(t0, 500));
        assert_eq!(calls(&mut m), ["term 1 sender", "release 1"]);
        exit(&mut m, 1, Kind::Sender, at(t0, 600));
        assert!(m.idle());
        assert_eq!(m.link.state, LinkState::Stopped, "an exit after stop changes nothing");
    }

    #[test]
    fn tx_camera_missing_backs_off_without_connecting() {
        let (mut m, t0) = machine(Role::Tx);
        m.backend.input_err = Some("/dev/video0: in use".into());
        m.start(t0);
        assert_eq!(calls(&mut m), ["check"]);
        assert!(m.link.has("input_lost"));
        assert_eq!((m.link.state, m.link.attempt), (LinkState::Connecting, None));
        assert_eq!(m.link.next_try, Some(at(t0, 5000)));
        m.tick(at(t0, 5000));
        assert_eq!(m.link.next_try, Some(at(t0, 15_000)));
        m.backend.input_err = None;
        m.tick(at(t0, 15_000));
        assert_eq!(calls(&mut m), ["check", "check", "spawn 1 sender"]);
        assert!(m.link.has("input_lost"), "the input counts as working once the bridge reports frames");
        line(&mut m, 1, Kind::Sender, CONNECTED, at(t0, 15_300));
        assert!(events(&mut m).iter().all(|e| *e != "input_detected"));
        line(&mut m, 1, Kind::Bridge, r#"@evt {"input":"ok"}"#, at(t0, 16_000));
        assert!(!m.link.has("input_lost"));
        assert_eq!(events(&mut m), ["input_detected"]);
    }

    /// One cycle of a camera that opens but fails once it streams; returns the wait for the next try.
    fn src_err_cycle(m: &mut Machine<Fake>, sid: u64, t: Instant, ok_first: bool) -> u64 {
        line(m, sid, Kind::Sender, CONNECTED, t + Duration::from_millis(300));
        if ok_first {
            line(m, sid, Kind::Bridge, r#"@evt {"input":"ok"}"#, t + Duration::from_millis(1000));
        }
        line(m, sid, Kind::Bridge, "[SRC ERR] Internal data stream error.", t + Duration::from_millis(2000));
        m.tick(t + Duration::from_millis(2050));
        exit(m, sid, Kind::Bridge, t + Duration::from_millis(2100));
        exit(m, sid, Kind::Sender, t + Duration::from_millis(4000));
        assert_eq!(m.link.state, LinkState::Connecting);
        assert!(m.link.has("input_lost"));
        m.link.next_try.unwrap().duration_since(t + Duration::from_millis(4000)).as_secs()
    }

    #[test]
    fn tx_input_failing_once_streaming_backs_off() {
        let (mut m, t0) = machine(Role::Tx);
        m.start(t0);
        let mut t = t0;
        let mut waits = Vec::new();
        for sid in 1..=5u64 {
            let wait = src_err_cycle(&mut m, sid, t, false);
            waits.push(wait);
            t += Duration::from_millis(4000) + Duration::from_secs(wait);
            m.tick(t);
        }
        assert_eq!(waits, [5, 10, 20, 30, 30]);
        let ev = events(&mut m);
        assert!(!ev.contains(&"input_detected"), "{ev:?}");

        // frames for a moment before the error: detected, but the backoff still grows
        let w = src_err_cycle(&mut m, 6, t, true);
        assert_eq!(w, 30);
        assert!(events(&mut m).contains(&"input_detected"));
    }

    #[test]
    fn tx_input_working_for_a_while_restarts_the_backoff() {
        let (mut m, t0) = machine(Role::Tx);
        m.backend.input_err = Some("/dev/video0: in use".into());
        m.start(t0);
        m.tick(at(t0, 5000));
        m.tick(at(t0, 15_000));
        assert_eq!(m.link.next_try, Some(at(t0, 35_000)));
        m.backend.input_err = None;
        m.tick(at(t0, 35_000));
        line(&mut m, 1, Kind::Sender, CONNECTED, at(t0, 35_300));
        line(&mut m, 1, Kind::Bridge, r#"@evt {"input":"ok"}"#, at(t0, 36_000));
        m.tick(at(t0, 46_000));
        // the camera goes away after 10 s of frames: the first step again
        line(&mut m, 1, Kind::Bridge, "[SRC ERR] Could not read from resource.", at(t0, 50_000));
        exit(&mut m, 1, Kind::Bridge, at(t0, 50_100));
        exit(&mut m, 1, Kind::Sender, at(t0, 51_000));
        assert_eq!(m.link.next_try, Some(at(t0, 56_000)));
    }

    #[test]
    fn tx_bridge_without_input_events_is_trusted_after_a_while() {
        let (mut m, t0) = machine(Role::Tx);
        m.backend.input_err = Some("/dev/video0: no such device".into());
        m.start(t0);
        m.backend.input_err = None;
        m.tick(at(t0, 5000));
        line(&mut m, 1, Kind::Sender, CONNECTED, at(t0, 5300));
        m.tick(at(t0, 15_000));
        assert!(m.link.has("input_lost"));
        m.tick(at(t0, 15_300));
        assert!(!m.link.has("input_lost"));
        assert!(events(&mut m).contains(&"input_detected"));
    }

    #[test]
    fn tx_bridge_markers_anywhere_in_a_line() {
        let (mut m, t0) = machine(Role::Tx);
        go_live(&mut m, t0);
        events(&mut m);
        // an unterminated print, then the marker line (written with one call)
        line(&mut m, 1, Kind::Bridge, r#"  [enc EOS id=2]@evt {"input":"lost"}"#, at(t0, 1000));
        assert!(m.link.has("input_lost"));
        line(&mut m, 1, Kind::Bridge, r#"[t=  3.0s] switch@evt {"input":"ok"}"#, at(t0, 2000));
        assert!(!m.link.has("input_lost"));
        line(&mut m, 1, Kind::Bridge, "  [STATUS] cur=3500[SRC ERR] Internal data stream error.", at(t0, 3000));
        exit(&mut m, 1, Kind::Bridge, at(t0, 3100));
        exit(&mut m, 1, Kind::Sender, at(t0, 5000));
        assert_eq!(m.link.state, LinkState::Connecting);
        assert!(m.link.has("input_lost"));
    }

    #[test]
    fn tx_source_error_and_input_events() {
        let (mut m, t0) = machine(Role::Tx);
        go_live(&mut m, t0);
        events(&mut m);
        line(&mut m, 1, Kind::Bridge, r#"@evt {"input":"lost"}"#, at(t0, 1000));
        assert!(m.link.has("input_lost"));
        line(&mut m, 1, Kind::Bridge, r#"@evt {"input":"ok"}"#, at(t0, 2000));
        assert!(!m.link.has("input_lost"));
        assert_eq!(events(&mut m), ["input_detected"]);
        // the camera goes away: the bridge quits, the sender drains and exits
        line(&mut m, 1, Kind::Bridge, "[SRC ERR] Could not read from resource.", at(t0, 3000));
        exit(&mut m, 1, Kind::Bridge, at(t0, 3100));
        exit(&mut m, 1, Kind::Sender, at(t0, 5000));
        assert_eq!(m.link.state, LinkState::Connecting);
        assert!(m.link.has("input_lost"));
        assert!(!m.link.has("peer_unreachable"));
        assert_eq!(m.link.next_try, Some(at(t0, 10_000)));
        assert_eq!(events(&mut m), ["peer_disconnected"]);
    }

    #[test]
    fn rx_player_first_and_plays_out_on_disconnect() {
        let (mut m, t0) = machine(Role::Rx);
        m.start(t0);
        assert_eq!(m.link.state, LinkState::Listening);
        assert_eq!(calls(&mut m), ["spawn 1 player", "player 1 display off", "spawn 1 receiver"]);
        line(&mut m, 1, Kind::Receiver, "[srt-recv] listening on :60021 (latency=8000 ms)", at(t0, 50));
        line(&mut m, 1, Kind::Receiver, "[srt-recv] accepted from 192.168.0.42:51234", at(t0, 2000));
        assert_eq!(m.link.state, LinkState::Live);
        assert_eq!(m.link.peer_addr, Some(("192.168.0.42".into(), 51234)));
        let ev: Vec<_> = m.out.drain(..).collect();
        assert_eq!(ev[0].code, "listening_started");
        assert_eq!(ev[0].params["port"], 60021);
        assert_eq!(ev[1].code, "peer_connected");
        m.observe(Obs { first_frames: true, black: false, ..Obs::default() }, at(t0, 3000));
        assert_eq!(events(&mut m), ["transport_started"]);
        assert!(!m.link.rx_black);

        exit(&mut m, 1, Kind::Receiver, at(t0, 60_000));
        assert_eq!(m.link.state, LinkState::Listening);
        assert_eq!(events(&mut m), ["peer_disconnected"]);
        assert_eq!(calls(&mut m), ["release 1"], "the old player is not stopped");
        m.tick(at(t0, 60_500));
        assert_eq!(calls(&mut m), ["spawn 2 player", "player 2 display off", "spawn 2 receiver"]);
        exit(&mut m, 1, Kind::Player, at(t0, 69_000));
        assert_eq!(m.finished.len(), 1);
        // stop: the new pair is terminated
        m.stop(at(t0, 70_000));
        assert_eq!(calls(&mut m), ["term 2 receiver", "term 2 player", "release 2"]);
    }

    #[test]
    fn rx_quick_failure_waits_longer_and_player_crash_restarts() {
        let (mut m, t0) = machine(Role::Rx);
        m.start(t0);
        calls(&mut m);
        // the receiver cannot bind: exits at once
        exit(&mut m, 1, Kind::Receiver, at(t0, 100));
        assert_eq!(m.link.next_try, Some(at(t0, 5100)));
        exit(&mut m, 1, Kind::Player, at(t0, 200));
        m.tick(at(t0, 5100));
        assert_eq!(calls(&mut m), ["release 1", "spawn 2 player", "player 2 display off", "spawn 2 receiver"]);
        exit(&mut m, 2, Kind::Player, at(t0, 9000));
        assert_eq!(calls(&mut m), ["term 2 receiver"]);
    }

    #[test]
    fn stream_ids() {
        use DelayProfile::*;
        for (text, want) in [
            ("delay=low", Low),
            ("delay=standard", Standard),
            ("x=1;delay=low", Low),
            ("id=TX-01 delay=low", Low),
            ("delay=low;delay=standard", Low),
            ("delay=fast", Standard),
            ("delay=", Standard),
            ("lowdelay=low", Standard),
            ("#!::r=live,m=publish", Standard),
            ("", Standard),
        ] {
            assert_eq!(stream_id_delay(text), want, "{text:?}");
        }
    }

    #[test]
    fn rx_follows_a_low_delay_transmitter() {
        let (mut m, t0) = machine(Role::Rx);
        m.start(t0);
        calls(&mut m);
        line(&mut m, 1, Kind::Receiver, "[srt-recv] listening on :60021 (latency=8000 ms)", at(t0, 50));
        assert_eq!(m.link.delay, DelayProfile::Standard);
        let rev = m.link.rev;
        // the stream id comes right before the accepted line: the player gets the low profile's lines first
        line(&mut m, 1, Kind::Receiver, "[srt-recv] stream id delay=low", at(t0, 2000));
        assert_eq!(calls(&mut m), ["player 1 ts-offset-ms 400", "player 1 adapt on"]);
        assert_eq!(m.link.delay, DelayProfile::Low);
        assert!(m.link.rev > rev, "a status push");
        // a second one, or one after the accepted line, changes nothing
        line(&mut m, 1, Kind::Receiver, "[srt-recv] stream id delay=standard", at(t0, 2001));
        line(&mut m, 1, Kind::Receiver, "[srt-recv] accepted from 192.168.0.42:51234", at(t0, 2002));
        line(&mut m, 1, Kind::Receiver, "[srt-recv] stream id delay=low", at(t0, 2003));
        assert!(calls(&mut m).is_empty());
        assert_eq!((m.link.state, m.link.delay), (LinkState::Live, DelayProfile::Low));

        // the transmitter goes away: the last connection's profile stays until the next one
        exit(&mut m, 1, Kind::Receiver, at(t0, 60_000));
        assert_eq!((m.link.state, m.link.delay), (LinkState::Listening, DelayProfile::Low));
        m.tick(at(t0, 60_500));
        calls(&mut m);
        // a transmitter that says nothing, or names another profile: standard, nothing written
        line(&mut m, 2, Kind::Receiver, "[srt-recv] accepted from 192.168.0.42:51240", at(t0, 61_000));
        assert_eq!(m.link.delay, DelayProfile::Standard);
        exit(&mut m, 2, Kind::Receiver, at(t0, 70_000));
        m.tick(at(t0, 70_500));
        line(&mut m, 3, Kind::Receiver, "[srt-recv] stream id delay=fast;x=1", at(t0, 71_000));
        line(&mut m, 3, Kind::Receiver, "[srt-recv] accepted from 192.168.0.42:51241", at(t0, 71_001));
        assert_eq!(m.link.delay, DelayProfile::Standard);
        let to_player: Vec<String> = calls(&mut m).into_iter().filter(|c| c.starts_with("player")).collect();
        assert_eq!(to_player, ["player 3 display off"], "nothing for the player but its monitor output");
        // a line of an old session is not followed
        line(&mut m, 2, Kind::Receiver, "[srt-recv] stream id delay=low", at(t0, 71_002));
        assert_eq!(m.link.delay, DelayProfile::Standard);
        // the stop keeps it (memory only)
        m.stop(at(t0, 80_000));
        assert_eq!(m.link.delay, DelayProfile::Standard);
    }

    #[test]
    fn rx_players_get_the_monitor_output_first() {
        let (mut m, t0) = machine(Role::Rx);
        m.set_display(DisplayMode::Window, t0);
        assert!(calls(&mut m).is_empty(), "no player yet: kept for the next one");
        m.start(t0);
        assert_eq!(calls(&mut m), ["spawn 1 player", "player 1 display window", "spawn 1 receiver"]);
        // the transmitter's stream id comes later: the low-delay lines follow the monitor output
        line(&mut m, 1, Kind::Receiver, "[srt-recv] stream id delay=low", at(t0, 2000));
        line(&mut m, 1, Kind::Receiver, "[srt-recv] accepted from 192.168.0.42:51234", at(t0, 2001));
        assert_eq!(calls(&mut m), ["player 1 ts-offset-ms 400", "player 1 adapt on"]);
        // applied while live: at once, to the running player (the same value again too: a retry by hand)
        m.set_display(DisplayMode::Fullscreen, at(t0, 5000));
        m.set_display(DisplayMode::Fullscreen, at(t0, 6000));
        assert_eq!(calls(&mut m), ["player 1 display fullscreen", "player 1 display fullscreen"]);
        assert_eq!(m.link.state, LinkState::Live, "the link is not touched");
        // the transmitter goes away: the old player plays out without control, the next one gets the value first
        exit(&mut m, 1, Kind::Receiver, at(t0, 60_000));
        m.set_display(DisplayMode::Off, at(t0, 60_100));
        assert_eq!(calls(&mut m), ["release 1"], "no current player between two pairs");
        m.tick(at(t0, 60_500));
        assert_eq!(calls(&mut m), ["spawn 2 player", "player 2 display off", "spawn 2 receiver"]);
        // stopped: no player, the value is kept for the next start
        m.stop(at(t0, 70_000));
        calls(&mut m);
        m.set_display(DisplayMode::Window, at(t0, 70_100));
        assert!(calls(&mut m).is_empty());
        for (sid, k) in [(1, Kind::Player), (2, Kind::Receiver), (2, Kind::Player)] {
            exit(&mut m, sid, k, at(t0, 70_200));
        }
        m.start(at(t0, 71_000));
        assert_eq!(calls(&mut m), ["spawn 3 player", "player 3 display window", "spawn 3 receiver"]);

        // the transmitter has no monitor output
        let (mut m, t0) = machine(Role::Tx);
        go_live(&mut m, t0);
        calls(&mut m);
        m.set_display(DisplayMode::Fullscreen, at(t0, 1000));
        m.observe(Obs { player_display: Some("failed".into()), ..Obs::default() }, at(t0, 40_000));
        assert!(calls(&mut m).is_empty());
    }

    #[test]
    fn rx_monitor_output_is_asked_again_every_30s_while_it_fails() {
        let (mut m, t0) = machine(Role::Rx);
        m.set_display(DisplayMode::Fullscreen, t0);
        m.start(t0);
        calls(&mut m);
        let report = |m: &mut Machine<Fake>, d: Option<&str>, ms: u64| {
            m.observe(Obs { player_display: d.map(str::to_string), ..Obs::default() }, at(t0, ms));
            calls(m)
        };
        let failed = Some("failed");
        assert!(report(&mut m, failed, 1000).is_empty());
        assert!(report(&mut m, failed, 29_999).is_empty(), "not before 30 s after the line at the start");
        assert_eq!(report(&mut m, failed, 30_000), ["player 1 display fullscreen"]);
        assert!(report(&mut m, failed, 31_000).is_empty());
        // applied meanwhile: written at once, and the next try is 30 s after that
        m.set_display(DisplayMode::Window, at(t0, 40_000));
        assert_eq!(calls(&mut m), ["player 1 display window"]);
        assert!(report(&mut m, failed, 60_000).is_empty());
        assert_eq!(report(&mut m, failed, 70_000), ["player 1 display window"]);
        // it opened, or the player does not say: nothing
        assert!(report(&mut m, Some("window"), 100_000).is_empty());
        assert!(report(&mut m, None, 101_000).is_empty());
        assert_eq!(report(&mut m, failed, 102_000), ["player 1 display window"], "failed again later");
        // turned off: never asked again
        m.set_display(DisplayMode::Off, at(t0, 110_000));
        assert_eq!(calls(&mut m), ["player 1 display off"]);
        assert!(report(&mut m, failed, 150_000).is_empty());
        // on again, and no current player (between two pairs): nothing until the next one starts
        m.set_display(DisplayMode::Fullscreen, at(t0, 151_000));
        exit(&mut m, 1, Kind::Receiver, at(t0, 152_000));
        calls(&mut m);
        assert!(report(&mut m, failed, 152_100).is_empty());
        m.tick(at(t0, 152_500));
        assert_eq!(calls(&mut m), ["spawn 2 player", "player 2 display fullscreen", "spawn 2 receiver"]);
        assert!(report(&mut m, failed, 182_499).is_empty(), "30 s after the line at its start");
        assert_eq!(report(&mut m, failed, 182_500), ["player 2 display fullscreen"]);
    }

    #[test]
    fn tx_takes_the_profile_of_the_plan_at_each_start() {
        let (mut m, t0) = machine(Role::Tx);
        m.plan.delay = DelayProfile::Low;
        go_live(&mut m, t0);
        assert_eq!(m.link.delay, DelayProfile::Low);
        m.stop(at(t0, 1000));
        m.plan.delay = DelayProfile::Standard;
        // a receiver's line never reaches the transmitter's state
        m.start(at(t0, 20_000));
        assert_eq!(m.link.delay, DelayProfile::Standard);
    }

    #[test]
    fn alerts_from_the_sampler() {
        let (mut m, t0) = machine(Role::Tx);
        go_live(&mut m, t0);
        m.observe(Obs { margin_low: Some(2.9), ..Obs::default() }, at(t0, 1000));
        let a = m.link.alerts.iter().find(|a| a.code == "delay_margin_low").unwrap();
        assert_eq!((a.level, a.params["margin_s"].as_f64()), ("warn", Some(2.9)));
        let rev = m.link.rev;
        m.observe(Obs { margin_low: Some(2.9), ..Obs::default() }, at(t0, 2000));
        assert_eq!(m.link.rev, rev, "unchanged alert: no status push");
        m.observe(Obs::default(), at(t0, 3000));
        assert!(m.link.alerts.is_empty());

        let (mut m, t0) = machine(Role::Rx);
        m.start(t0);
        m.observe(Obs { stalled: Some((2, 1.3, at(t0, 10))), black: true, ..Obs::default() }, at(t0, 1000));
        let a = &m.link.alerts[0];
        assert_eq!((a.code, a.since), ("video_stalled", at(t0, 10)));
        assert_eq!(a.params["count"], 2);
        assert_eq!(a.params["window_s"], 300);
    }
}
