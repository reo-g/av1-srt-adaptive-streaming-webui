//! The browser preview: a copy of the picture being sent (tx) or shown (rx).
//!
//! The media programs copy their compressed video to a local socket (the bridge's PREVIEW_SOCK,
//! the player's --preview-sock; profile.rs) while something reads it. The preview program (one
//! per device, `[preview].program`) listens there, makes a light copy for browsers and sends it
//! to each viewer. This manager starts the program on the first request, passes offers and
//! answers between the HTTP handlers and the program, tracks the viewers and stops the program
//! on shutdown. The transmission never depends on it: it is not part of the link's state
//! machine, and a program that is missing or fails only refuses previews.
//!
//! The program's stdin and stdout carry JSON lines (stdout nothing else; its stderr is the log):
//!   in:  {"cmd":"offer","id","sdp"}  {"cmd":"close","id"}  {"cmd":"quit"} (or the end of stdin)
//!   out: {"evt":"ready"}  {"evt":"answer","id","sdp","codec"}  {"evt":"error","id","code","msg"}
//!        {"evt":"closed","id","why"}
//!        {"evt":"stats","viewers","in_fps","out_fps","connected","late_frames","delay_ms"}
//!        (late_frames and delay_ms, the pacing, are newer: a line without them is fine too)
//! Ids are made here; an answer or error is matched to its offer by id. The program exits by
//! itself a while after the last viewer left; the next offer starts it again. An offer that
//! reached it just as it left (exit 0, not asked to) gets one more try on a new program. A
//! program that says nothing for `stall_after` (it writes stats every 5 s) is taken as hung and
//! stopped, so a stuck one never keeps the preview away until the daemon restarts.

use crate::config::Config;
use crate::media;
use crate::profile::{self, Env};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

/// Longest offer (the contract's maxLength; SDP is ASCII, so bytes = characters).
pub const OFFER_LIMIT: usize = 64 * 1024;
const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// Shutdown: "quit" first, SIGTERM to the program's group after this, SIGKILL after the next.
const QUIT_TERM_AFTER: Duration = Duration::from_secs(2);
const QUIT_KILL_AFTER: Duration = Duration::from_secs(4);
/// A ready program that writes nothing for this long (stats come every 5 s) is hung.
const STALL_AFTER: Duration = Duration::from_secs(20);
/// The developer log repeats a stats line whose pacing alone changed at most this often
/// (preview.log has every line).
const PACING_LOG_EVERY: Duration = Duration::from_secs(60);

/// Why an offer got no answer (the API's error codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Turned off, the program did not start or answer in time, or it failed.
    Unavailable,
    TooManyViewers,
    BadOffer,
    NoCommonCodec,
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Refusal::Unavailable => "preview_unavailable",
            Refusal::TooManyViewers => "too_many_viewers",
            Refusal::BadOffer => "bad_offer",
            Refusal::NoCommonCodec => "no_common_codec",
        }
    }

    /// The program's error code for an offer.
    fn from_program(code: &str) -> Refusal {
        match code {
            "bad_offer" => Refusal::BadOffer,
            "no_common_codec" => Refusal::NoCommonCodec,
            _ => Refusal::Unavailable,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub id: String,
    pub sdp: String,
}

/// How the program is run and how long the manager waits for it.
#[derive(Debug, Clone)]
pub struct Launch {
    pub spec: profile::Spec,
    /// The program's whole environment (the inherited variables that pass, see media.rs).
    pub env: Env,
    pub log: PathBuf,
    pub max_viewers: usize,
    pub ready_timeout: Duration,
    pub answer_timeout: Duration,
    pub term_after: Duration,
    pub kill_after: Duration,
    pub stall_after: Duration,
}

impl Launch {
    /// None when the preview is turned off.
    pub fn from_config(cfg: &Config) -> Option<Launch> {
        let p = &cfg.preview;
        if !p.enabled {
            return None;
        }
        let spec = profile::preview(cfg);
        let env = media::child_env(media::inherited(), &cfg.media.pass_env, &spec.env);
        Some(Launch {
            spec,
            env,
            log: cfg.run_dir().join("preview.log"),
            max_viewers: p.max_viewers as usize,
            ready_timeout: READY_TIMEOUT,
            answer_timeout: Duration::from_secs(u64::from(p.answer_timeout_s)),
            term_after: QUIT_TERM_AFTER,
            kill_after: QUIT_KILL_AFTER,
            stall_after: STALL_AFTER,
        })
    }
}

enum Req {
    Offer { sdp: String, reply: oneshot::Sender<Result<Answer, Refusal>> },
    Close { id: String },
    Stop { done: oneshot::Sender<()> },
}

/// The handle the HTTP handlers use (cheap to clone). The manager runs as a task of the
/// daemon's single runtime thread, so the program is forked from that thread (PDEATHSIG).
#[derive(Clone)]
pub struct Preview {
    tx: Option<UnboundedSender<Req>>,
}

impl Preview {
    /// Starts the manager (not the program: that waits for the first offer). None = turned off.
    pub fn start(launch: Option<Launch>) -> Preview {
        let Some(launch) = launch else { return Preview { tx: None } };
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(Manager::new(launch), rx));
        Preview { tx: Some(tx) }
    }

    pub fn enabled(&self) -> bool {
        self.tx.is_some()
    }

    /// A browser's offer; the answer once the program has made one.
    pub async fn offer(&self, sdp: String) -> Result<Answer, Refusal> {
        let Some(tx) = &self.tx else { return Err(Refusal::Unavailable) };
        let (reply, rx) = oneshot::channel();
        if tx.send(Req::Offer { sdp, reply }).is_err() {
            return Err(Refusal::Unavailable);
        }
        rx.await.unwrap_or(Err(Refusal::Unavailable))
    }

    /// The viewer is gone (an unknown id is fine).
    pub fn close(&self, id: &str) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Req::Close { id: id.to_string() });
        }
    }

    /// Stop the program now (quit, then SIGTERM and SIGKILL) and refuse later offers. The
    /// receiver resolves once the program has exited.
    pub fn stop(&self) -> oneshot::Receiver<()> {
        let (done, rx) = oneshot::channel();
        match &self.tx {
            Some(tx) => {
                if let Err(mpsc::error::SendError(Req::Stop { done })) = tx.send(Req::Stop { done }) {
                    let _ = done.send(());
                }
            }
            None => {
                let _ = done.send(());
            }
        }
        rx
    }
}

// ------------------------------------------------------------------ the manager

/// What the program's watcher reports, tagged with the program's generation.
enum Out {
    Line(String),
    Exit(Option<i32>),
}

struct Program {
    gen: u64,
    pid: i32,
    /// Lines to its stdin; None once closed (quit sent).
    input: Option<UnboundedSender<String>>,
    ready: bool,
    ready_by: Instant,
    /// Its last stdout line (the hang watchdog).
    last_out: Instant,
    quitting: bool,
    term_at: Option<Instant>,
    kill_at: Option<Instant>,
}

struct Pending {
    reply: oneshot::Sender<Result<Answer, Refusal>>,
    sdp: String,
    /// Passed on to the program (false while it is still starting).
    sent: bool,
    /// Already passed on again after the program left by itself.
    retried: bool,
    deadline: Instant,
}

struct Manager {
    l: Launch,
    prog: Option<Program>,
    gen: u64,
    pending: HashMap<String, Pending>,
    viewers: HashSet<String>,
    /// The daemon is shutting down: nothing starts again.
    stopping: bool,
    waiters: Vec<oneshot::Sender<()>>,
    out_tx: UnboundedSender<(u64, Out)>,
    out_rx: UnboundedReceiver<(u64, Out)>,
    /// What the developer log repeats of the stats lines.
    stats_log: StatsLog,
}

async fn run(mut m: Manager, mut reqs: UnboundedReceiver<Req>) {
    let mut open = true;
    loop {
        let next = m.next_deadline();
        let sleep = async move {
            match next {
                Some(t) => tokio::time::sleep_until(t.into()).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            r = reqs.recv(), if open => match r {
                Some(r) => m.request(r, Instant::now()),
                None => {
                    // every handle is gone: stop the program and end
                    open = false;
                    m.stopping = true;
                    m.stop(Instant::now());
                }
            },
            Some((gen, out)) = m.out_rx.recv() => m.output(gen, out, Instant::now()),
            _ = sleep => m.tick(Instant::now()),
        }
        if !open && m.prog.is_none() {
            break;
        }
    }
}

impl Manager {
    fn new(l: Launch) -> Manager {
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        Manager {
            l,
            prog: None,
            gen: 0,
            pending: HashMap::new(),
            viewers: HashSet::new(),
            stopping: false,
            waiters: Vec::new(),
            out_tx,
            out_rx,
            stats_log: StatsLog::default(),
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        let p = self.prog.as_ref();
        let ready = p.filter(|p| !p.ready && !p.quitting).map(|p| p.ready_by);
        let stall = p.filter(|p| p.ready && !p.quitting).map(|p| p.last_out + self.l.stall_after);
        let signals = p.into_iter().flat_map(|p| [p.term_at, p.kill_at]).flatten();
        self.pending.values().map(|p| p.deadline).chain(ready).chain(stall).chain(signals).min()
    }

    fn request(&mut self, r: Req, now: Instant) {
        match r {
            Req::Offer { sdp, reply } => self.offer(sdp, reply, now),
            Req::Close { id } => self.close(&id),
            Req::Stop { done } => {
                self.waiters.push(done);
                if !self.stopping {
                    dlog!("preview: stopping");
                }
                self.stopping = true;
                self.stop(now);
                self.notify();
            }
        }
    }

    fn offer(&mut self, sdp: String, reply: oneshot::Sender<Result<Answer, Refusal>>, now: Instant) {
        if self.stopping || self.prog.as_ref().is_some_and(|p| p.quitting) {
            let _ = reply.send(Err(Refusal::Unavailable));
            return;
        }
        self.drop_abandoned();
        if self.viewers.len() + self.pending.len() >= self.l.max_viewers {
            let _ = reply.send(Err(Refusal::TooManyViewers));
            return;
        }
        if self.prog.is_none() {
            if let Err(e) = self.spawn(now) {
                dlog!("preview: cannot start the program: {e}");
                let _ = reply.send(Err(Refusal::Unavailable));
                return;
            }
        }
        let id = self.new_id();
        let Some(p) = self.prog.as_ref() else { return };
        let pending = if p.ready {
            self.send(json!({ "cmd": "offer", "id": id, "sdp": sdp }));
            Pending { reply, sdp, sent: true, retried: false, deadline: now + self.l.answer_timeout }
        } else {
            // passed on once the program is ready
            Pending { reply, sdp, sent: false, retried: false, deadline: p.ready_by }
        };
        self.pending.insert(id, pending);
    }

    /// Offers whose request went away (the browser left) no longer hold a place.
    fn drop_abandoned(&mut self) {
        let gone: Vec<String> = self.pending.iter().filter(|(_, p)| p.reply.is_closed()).map(|(k, _)| k.clone()).collect();
        for id in gone {
            if self.pending.remove(&id).is_some_and(|p| p.sent) {
                self.send(json!({ "cmd": "close", "id": id }));
            }
        }
    }

    fn close(&mut self, id: &str) {
        let viewer = self.viewers.remove(id);
        let sent = match self.pending.remove(id) {
            Some(p) => {
                let _ = p.reply.send(Err(Refusal::Unavailable));
                p.sent
            }
            None => false,
        };
        if viewer || sent {
            self.send(json!({ "cmd": "close", "id": id }));
        }
    }

    /// Ask the program to quit (it has `term_after` to do so), fail what waits for it.
    fn stop(&mut self, now: Instant) {
        for (_, p) in self.pending.drain() {
            let _ = p.reply.send(Err(Refusal::Unavailable));
        }
        let (term, kill) = (self.l.term_after, self.l.kill_after);
        if let Some(p) = self.prog.as_mut().filter(|p| !p.quitting) {
            p.quitting = true;
            if let Some(input) = p.input.take() {
                // dropping the sender closes stdin once the line is written
                let _ = input.send(json!({ "cmd": "quit" }).to_string());
            }
            p.term_at = Some(now + term);
            p.kill_at = Some(now + kill);
        }
    }

    fn notify(&mut self) {
        if self.prog.is_none() {
            for w in self.waiters.drain(..) {
                let _ = w.send(());
            }
        }
    }

    fn tick(&mut self, now: Instant) {
        if self.prog.as_ref().is_some_and(|p| !p.ready && !p.quitting && now >= p.ready_by) {
            dlog!("preview: the program did not get ready in {:?}; stopping it", self.l.ready_timeout);
            self.stop(now);
        }
        if self.prog.as_ref().is_some_and(|p| p.ready && !p.quitting && now >= p.last_out + self.l.stall_after) {
            // the next offer starts a new one
            dlog!("preview: the program has written nothing for {:?} (hung?); stopping it", self.l.stall_after);
            self.stop(now);
        }
        if let Some(p) = self.prog.as_mut() {
            if p.term_at.is_some_and(|t| now >= t) {
                p.term_at = None;
                signal(p.pid, libc::SIGTERM);
            }
            if p.kill_at.is_some_and(|t| now >= t) {
                p.kill_at = None;
                signal(p.pid, libc::SIGKILL);
            }
        }
        let late: Vec<String> = self.pending.iter().filter(|(_, p)| now >= p.deadline).map(|(k, _)| k.clone()).collect();
        for id in late {
            let Some(p) = self.pending.remove(&id) else { continue };
            dlog!("preview: no answer for {id} in time");
            let _ = p.reply.send(Err(Refusal::Unavailable));
            if p.sent {
                // it may still answer: it must not keep a viewer nobody uses
                self.send(json!({ "cmd": "close", "id": id }));
            }
        }
    }

    fn output(&mut self, gen: u64, out: Out, now: Instant) {
        if self.prog.as_ref().map(|p| p.gen) != Some(gen) {
            return;
        }
        match out {
            Out::Line(line) => {
                if let Some(p) = self.prog.as_mut() {
                    p.last_out = now;
                }
                self.event(&line, now)
            }
            Out::Exit(code) => {
                dlog!("preview: the program exited ({})", code.map_or("signal".into(), |c| c.to_string()));
                let asked = self.prog.take().is_some_and(|p| p.quitting);
                self.viewers.clear();
                self.stats_log = StatsLog::default();
                // It left by itself without an error (its idle time ran out) while these offers
                // were on their way to it: they never reached it, a new program gets them once.
                let again = !self.stopping && !asked && code == Some(0);
                let mut retry = Vec::new();
                for (id, p) in self.pending.drain() {
                    if again && !p.retried && !p.reply.is_closed() {
                        retry.push((id, p));
                    } else {
                        let _ = p.reply.send(Err(Refusal::Unavailable));
                    }
                }
                if !retry.is_empty() {
                    match self.spawn(now) {
                        Ok(()) => {
                            let ready_by = now + self.l.ready_timeout;
                            for (id, mut p) in retry {
                                (p.sent, p.retried, p.deadline) = (false, true, ready_by);
                                self.pending.insert(id, p);
                            }
                        }
                        Err(e) => {
                            dlog!("preview: cannot start the program: {e}");
                            for (_, p) in retry {
                                let _ = p.reply.send(Err(Refusal::Unavailable));
                            }
                        }
                    }
                }
                self.notify();
            }
        }
    }

    fn event(&mut self, line: &str, now: Instant) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        let id = v.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        match v.get("evt").and_then(Value::as_str) {
            Some("ready") => {
                let Some(p) = self.prog.as_mut() else { return };
                if p.ready || p.quitting {
                    return;
                }
                p.ready = true;
                dlog!("preview: ready");
                self.drop_abandoned();
                let queued: Vec<String> = self.pending.iter().filter(|(_, p)| !p.sent).map(|(k, _)| k.clone()).collect();
                for id in queued {
                    let Some(p) = self.pending.get_mut(&id) else { continue };
                    (p.sent, p.deadline) = (true, now + self.l.answer_timeout);
                    let msg = json!({ "cmd": "offer", "id": id, "sdp": p.sdp });
                    self.send(msg);
                }
            }
            Some("answer") => {
                let sdp = v.get("sdp").and_then(Value::as_str).filter(|s| !s.is_empty());
                match (self.pending.remove(&id), sdp) {
                    (Some(p), Some(sdp)) => {
                        let codec = v.get("codec").and_then(Value::as_str).unwrap_or("?");
                        if p.reply.send(Ok(Answer { id: id.clone(), sdp: sdp.to_string() })).is_ok() {
                            dlog!("preview: viewer {id} joins ({codec})");
                            self.viewers.insert(id);
                        } else {
                            // the request went away meanwhile
                            self.send(json!({ "cmd": "close", "id": id }));
                        }
                    }
                    (Some(p), None) => {
                        let _ = p.reply.send(Err(Refusal::Unavailable));
                        self.send(json!({ "cmd": "close", "id": id }));
                    }
                    // an answer after its offer timed out or was closed
                    (None, _) if !self.viewers.contains(&id) => self.send(json!({ "cmd": "close", "id": id })),
                    (None, _) => {}
                }
            }
            Some("error") => {
                if let Some(p) = self.pending.remove(&id) {
                    let code = v.get("code").and_then(Value::as_str).unwrap_or_default();
                    let msg = v.get("msg").and_then(Value::as_str).unwrap_or_default();
                    dlog!("preview: offer {id} refused: {code} {msg}");
                    let _ = p.reply.send(Err(Refusal::from_program(code)));
                }
            }
            Some("closed") => {
                if self.viewers.remove(&id) {
                    let why = v.get("why").and_then(Value::as_str).unwrap_or_default();
                    dlog!("preview: viewer {id} left ({why})");
                }
                if let Some(p) = self.pending.remove(&id) {
                    let _ = p.reply.send(Err(Refusal::Unavailable));
                }
            }
            Some("stats") => {
                if let Some(text) = self.stats_log.line(StatsNote::parse(&v), now) {
                    dlog!("preview: {text}");
                }
            }
            _ => {}
        }
    }

    fn send(&self, msg: Value) {
        if let Some(input) = self.prog.as_ref().and_then(|p| p.input.as_ref()) {
            let _ = input.send(msg.to_string());
        }
    }

    fn new_id(&self) -> String {
        loop {
            let id = random_id();
            if !self.pending.contains_key(&id) && !self.viewers.contains(&id) {
                return id;
            }
        }
    }

    fn spawn(&mut self, now: Instant) -> io::Result<()> {
        self.gen += 1;
        let (pid, input) = start(&self.l, self.gen, self.out_tx.clone())?;
        dlog!("preview: program started, pid {pid}");
        self.prog = Some(Program {
            gen: self.gen,
            pid,
            input: Some(input),
            ready: false,
            ready_by: now + self.l.ready_timeout,
            last_out: now,
            quitting: false,
            term_at: None,
            kill_at: None,
        });
        Ok(())
    }
}

fn signal(pid: i32, sig: i32) {
    if pid > 0 {
        // SAFETY: signals the program's own process group (process_group(0) → pgid = pid).
        unsafe { libc::kill(-pid, sig) };
        dlog!("preview: {} sent", if sig == libc::SIGKILL { "SIGKILL" } else { "SIGTERM" });
    }
}

/// What the developer log needs of a stats line. Fields the program does not send (an older
/// one) are None and left out of the log.
#[derive(Debug, Clone, PartialEq)]
struct StatsNote {
    viewers: String,
    connected: bool,
    /// The delay in use.
    delay_ms: Option<i64>,
    /// Frames that came too late for it since the program's previous stats line.
    late_frames: Option<i64>,
}

impl StatsNote {
    fn parse(v: &Value) -> StatsNote {
        let num = |k: &str| v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite()).map(|x| x.round() as i64);
        StatsNote {
            viewers: v.get("viewers").map(Value::to_string).unwrap_or_default(),
            connected: v.get("connected").and_then(Value::as_bool).unwrap_or(false),
            delay_ms: num("delay_ms"),
            late_frames: num("late_frames").map(|n| n.max(0)),
        }
    }
}

/// Which stats lines the developer log repeats (preview.log has them all). A change of the
/// viewers or of the writer is logged at once. The pacing (a longer delay, frames that came
/// late) can change at every line over a slow path, so it is logged at most every
/// PACING_LOG_EVERY, with the late frames added up since the previous logged line: a burst
/// between two logged lines is still counted.
#[derive(Debug, Default)]
struct StatsLog {
    /// The last line written to the developer log, and when.
    last: Option<(StatsNote, Instant)>,
    /// Late frames reported since `last` was written, the current line included.
    late: i64,
}

impl StatsLog {
    /// The developer log text for this stats line, when one is due.
    fn line(&mut self, note: StatsNote, now: Instant) -> Option<String> {
        self.late = self.late.saturating_add(note.late_frames.unwrap_or(0));
        let due = match &self.last {
            None => true,
            Some((l, at)) => {
                (&l.viewers, l.connected) != (&note.viewers, note.connected)
                    || ((l.delay_ms != note.delay_ms || self.late > 0)
                        && now.saturating_duration_since(*at) >= PACING_LOG_EVERY)
            }
        };
        if !due {
            return None;
        }
        let mut t = format!("viewers {} video {}", note.viewers, if note.connected { "attached" } else { "not attached" });
        if let Some(d) = note.delay_ms {
            t += &format!(", delay {d} ms");
        }
        if note.late_frames.is_some() {
            t += &format!(", late frames {} since the previous line", self.late);
        }
        self.late = 0;
        self.last = Some((note, now));
        Some(t)
    }
}

/// 96 random bits as hex (the API's opaque session id).
fn random_id() -> String {
    let mut b = [0u8; 12];
    if File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).is_err() {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        b[..8].copy_from_slice(&t.to_le_bytes());
        b[8..].copy_from_slice(&(n as u32).to_le_bytes());
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ------------------------------------------------------------------ the program's process

/// Spawns the program in its own process group with PDEATHSIG (media.rs), its stderr into
/// the log (the previous instance's log is kept as preview.log.1).
fn start(l: &Launch, gen: u64, out: UnboundedSender<(u64, Out)>) -> io::Result<(i32, UnboundedSender<String>)> {
    if let Some(d) = l.log.parent() {
        fs::create_dir_all(d)?;
    }
    let mut old = l.log.clone().into_os_string();
    old.push(".1");
    let _ = fs::rename(&l.log, &old);
    let mut log = OpenOptions::new().create(true).append(true).open(&l.log)?;
    let s = &l.spec;
    for (k, v) in &l.env {
        writeln!(log, "# env {k}={v}")?;
    }
    writeln!(log, "# exec {} {}", s.program.display(), s.args.join(" "))?;
    let header = log.stream_position()?;

    let mut cmd = std::process::Command::new(&s.program);
    cmd.args(&s.args)
        .env_clear()
        .envs(&l.env)
        .current_dir(&s.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let parent = std::process::id();
    // SAFETY: child_setup only makes async-signal-safe calls.
    unsafe {
        cmd.pre_exec(move || media::child_setup(parent, &[]));
    }
    cmd.process_group(0);
    let mut child = tokio::process::Command::from(cmd)
        .spawn()
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", s.program.display())))?;
    let pid = child.id().map_or(0, |p| p as i32);
    let (in_tx, in_rx) = mpsc::unbounded_channel();
    if let Some(stdin) = child.stdin.take() {
        tokio::spawn(feed(stdin, in_rx));
    }
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    tokio::spawn(watch(child, stdout, stderr, log, header, gen, out));
    Ok((pid, in_tx))
}

/// Writes the command lines to the program's stdin; its end closes stdin.
async fn feed(mut stdin: ChildStdin, mut lines: UnboundedReceiver<String>) {
    while let Some(mut l) = lines.recv().await {
        l.push('\n');
        if stdin.write_all(l.as_bytes()).await.is_err() {
            break;
        }
    }
}

/// One stdout line in the log: answers without their (long) description.
fn log_line(line: &str) -> String {
    match serde_json::from_str::<Value>(line) {
        Ok(v) if v.get("evt").and_then(Value::as_str) == Some("answer") => {
            format!("@out {}", json!({ "evt": "answer", "id": v["id"], "codec": v["codec"] }))
        }
        _ => format!("@out {line}"),
    }
}

/// Reads the program's stdout (events, to the manager and the log) and stderr (the log), then
/// reports its exit after everything it wrote.
async fn watch(
    mut child: tokio::process::Child,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    mut log: File,
    header: u64,
    gen: u64,
    out: UnboundedSender<(u64, Out)>,
) {
    let limit = Some((media::LOG_CAP, header));
    let mut emit = |buf: &mut Vec<u8>, from_stdout: bool| {
        let text = String::from_utf8_lossy(buf);
        let line = text.trim_end_matches(['\n', '\r']).to_string();
        buf.clear();
        if from_stdout {
            let _ = writeln!(log, "{}", log_line(&line));
            let _ = out.send((gen, Out::Line(line)));
        } else {
            let _ = writeln!(log, "{line}");
        }
        media::cap_log(&mut log, limit);
    };
    let mut so = stdout.map(BufReader::new);
    let mut se = stderr.map(BufReader::new);
    let (mut so_buf, mut se_buf) = (Vec::new(), Vec::new());
    let (mut so_eof, mut se_eof) = (so.is_none(), se.is_none());
    let status = loop {
        tokio::select! {
            n = async { so.as_mut().unwrap().read_until(b'\n', &mut so_buf).await }, if !so_eof => match n {
                Ok(0) | Err(_) => so_eof = true,
                Ok(_) => emit(&mut so_buf, true),
            },
            n = async { se.as_mut().unwrap().read_until(b'\n', &mut se_buf).await }, if !se_eof => match n {
                Ok(0) | Err(_) => se_eof = true,
                Ok(_) => emit(&mut se_buf, false),
            },
            st = child.wait() => break st,
        }
    };
    // what is still in the pipes (a grandchild may hold them open: do not wait long)
    let _ = tokio::time::timeout(Duration::from_millis(300), async {
        while !so_eof || !se_eof {
            tokio::select! {
                n = async { so.as_mut().unwrap().read_until(b'\n', &mut so_buf).await }, if !so_eof => match n {
                    Ok(0) | Err(_) => so_eof = true,
                    Ok(_) => emit(&mut so_buf, true),
                },
                n = async { se.as_mut().unwrap().read_until(b'\n', &mut se_buf).await }, if !se_eof => match n {
                    Ok(0) | Err(_) => se_eof = true,
                    Ok(_) => emit(&mut se_buf, false),
                },
            }
        }
    })
    .await;
    if !so_buf.is_empty() {
        emit(&mut so_buf, true);
    }
    if !se_buf.is_empty() {
        emit(&mut se_buf, false);
    }
    let code = status.ok().and_then(|s| s.code().or_else(|| s.signal().map(|g| 128 + g)));
    let _ = writeln!(log, "# exit {}", code.map_or("?".into(), |c| c.to_string()));
    let _ = out.send((gen, Out::Exit(code)));
}

// ------------------------------------------------------------------ tests with the stub program

/// A Launch that runs tests/fixtures/stub_preview.py with the command line the daemon builds
/// (the stub takes exactly the real program's arguments) and short waits.
#[cfg(test)]
pub fn stub_launch(name: &str, start: &str, edit: impl FnOnce(&mut Config), max_viewers: u32) -> (Launch, PathBuf) {
    let dir = std::env::temp_dir().join(format!("ctrld-preview-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let text = format!(
        "id = \"TX-01\"\nrole = \"tx\"\nrun_dir = \"{}\"\n[tx]\npeer_host = \"10.0.0.2\"\n[media]\nrepo = \"{}\"\n\
         [preview]\nprogram = \"stub_preview.py\"\nmax_viewers = {max_viewers}\nanswer_timeout_s = 1\n",
        dir.display(),
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"),
    );
    let mut cfg = Config::parse(&text).unwrap();
    edit(&mut cfg);
    let mut l = Launch::from_config(&cfg).unwrap();
    l.env.insert("STUB_PREVIEW_START".into(), start.into());
    l.env.insert("STUB_PREVIEW_LOG".into(), dir.join("stub.jsonl").to_string_lossy().into_owned());
    l.env.insert("STUB_PREVIEW_STATS_S".into(), "0.2".into());
    l.ready_timeout = Duration::from_secs(1);
    l.term_after = Duration::from_millis(500);
    l.kill_after = Duration::from_millis(1000);
    l.stall_after = Duration::from_millis(1500);
    (l, dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    pub const H265: &str = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96 102\r\na=rtpmap:96 H265/90000\r\na=rtpmap:102 H264/90000\r\n";
    pub const H264: &str = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\na=rtpmap:102 H264/90000\r\na=fmtp:102 packetization-mode=1\r\n";
    const VP8: &str = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=rtpmap:96 VP8/90000\r\n";

    fn with(sdp: &str, marker: &str) -> String {
        format!("{sdp}a=x-stub:{marker}\r\n")
    }

    /// What the stub saw: one JSON object per start, command and exit.
    fn seen(dir: &Path) -> Vec<Value> {
        fs::read_to_string(dir.join("stub.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn starts(dir: &Path) -> usize {
        seen(dir).iter().filter(|v| v.get("start").is_some()).count()
    }

    fn cmds(dir: &Path, cmd: &str) -> Vec<String> {
        seen(dir)
            .iter()
            .filter(|v| v["cmd"] == cmd)
            .map(|v| v["id"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    async fn settle(ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    #[tokio::test]
    async fn answers_are_matched_by_id() {
        let (l, dir) = stub_launch("ids", "", |_| {}, 3);
        let p = Preview::start(Some(l));
        let (a, b) = tokio::join!(p.offer(H265.into()), p.offer(H264.into()));
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_ne!(a.id, b.id);
        assert_eq!(a.id.len(), 24);
        assert!(a.id.bytes().all(|c| c.is_ascii_hexdigit()));
        assert!(a.sdp.starts_with("v=0") && a.sdp.contains(" 96\r\n"), "{}", a.sdp);
        assert!(b.sdp.contains(" 102\r\n"), "{}", b.sdp);
        assert_eq!(p.offer(VP8.into()).await, Err(Refusal::NoCommonCodec));
        assert_eq!(p.offer("v=0\r\ns=-\r\n".into()).await, Err(Refusal::BadOffer));
        assert_eq!(p.offer(with(H264, "fail")).await, Err(Refusal::Unavailable));
        assert_eq!(starts(&dir), 1, "one program for every viewer");
        let offered = cmds(&dir, "offer");
        assert_eq!(offered.len(), 5);
        assert!(offered.contains(&a.id) && offered.contains(&b.id));
        // the command line the daemon builds is the one the program takes
        let args = seen(&dir)[0]["args"].clone();
        assert!(args.to_string().contains(r#""--delay-ms","1500","--max-delay-ms","5000""#), "{args}");

        p.close(&a.id);
        p.close("not-a-viewer");
        settle(300).await;
        assert_eq!(cmds(&dir, "close"), [a.id.clone()], "only known ids are passed on");
        let t = Instant::now();
        p.stop().await.unwrap();
        assert!(t.elapsed() < Duration::from_millis(500), "the program quits when asked");
        assert!(seen(&dir).iter().any(|v| v["why"] == "quit"));
        assert_eq!(p.offer(H264.into()).await, Err(Refusal::Unavailable), "nothing starts after a stop");
        assert!(fs::read_to_string(dir.join("preview.log")).unwrap().contains("[preview-stub] start"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn viewer_cap_counts_waiting_offers() {
        let (l, dir) = stub_launch("cap", "", |c| c.preview.answer_timeout_s = 3, 2);
        let p = Preview::start(Some(l));
        let first = p.offer(H264.into()).await.unwrap();
        // a second offer still waiting for its answer counts too
        let p2 = p.clone();
        let slow = tokio::spawn(async move { p2.offer(with(H264, "slow")).await });
        settle(200).await;
        assert_eq!(p.offer(H264.into()).await, Err(Refusal::TooManyViewers));
        assert_eq!(cmds(&dir, "offer").len(), 2, "refused without asking the program");
        assert!(slow.await.unwrap().is_ok());
        p.close(&first.id);
        assert!(p.offer(H264.into()).await.is_ok(), "a closed viewer frees its place");
        // a viewer that goes away by itself frees it too
        let (l, dir2) = stub_launch("cap1", "", |_| {}, 1);
        let q = Preview::start(Some(l));
        assert!(q.offer(with(H264, "drop")).await.is_ok());
        assert_eq!(q.offer(H264.into()).await, Err(Refusal::TooManyViewers));
        settle(600).await;
        let b = q.offer(H264.into()).await.unwrap();
        // an offer whose request went away does not hold the place
        q.close(&b.id);
        let gone = tokio::time::timeout(Duration::from_millis(200), q.offer(with(H264, "silent"))).await;
        assert!(gone.is_err());
        assert!(q.offer(H264.into()).await.is_ok());
        let offered = cmds(&dir2, "offer");
        assert!(cmds(&dir2, "close").contains(&offered[offered.len() - 2]), "the program forgets it");
        p.stop().await.unwrap();
        q.stop().await.unwrap();
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&dir2).unwrap();
    }

    #[tokio::test]
    async fn answer_timeouts_and_abandoned_offers_are_closed() {
        let (l, dir) = stub_launch("timeout", "", |_| {}, 3);
        let p = Preview::start(Some(l));
        let t = Instant::now();
        assert_eq!(p.offer(with(H264, "silent")).await, Err(Refusal::Unavailable));
        assert!(t.elapsed() >= Duration::from_millis(900), "{:?}", t.elapsed());
        settle(200).await;
        let offered = cmds(&dir, "offer");
        assert_eq!(cmds(&dir, "close"), offered, "the program forgets the offer");

        // the request goes away; the answer comes later: the viewer is closed, its place freed
        let gone = tokio::time::timeout(Duration::from_millis(300), p.offer(with(H264, "slow"))).await;
        assert!(gone.is_err());
        settle(1600).await;
        let offered = cmds(&dir, "offer");
        assert_eq!(cmds(&dir, "close"), offered);
        // closing an offer that is still waiting
        let p2 = p.clone();
        let waiting = tokio::spawn(async move { p2.offer(with(H264, "silent")).await });
        settle(200).await;
        let id = cmds(&dir, "offer").pop().unwrap();
        p.close(&id);
        assert_eq!(waiting.await.unwrap(), Err(Refusal::Unavailable));
        settle(200).await;
        assert_eq!(cmds(&dir, "close").last(), Some(&id));
        p.stop().await.unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn not_ready_in_time_then_started_again() {
        let (l, dir) = stub_launch("noready", "noready", |_| {}, 3);
        let p = Preview::start(Some(l));
        let t = Instant::now();
        assert_eq!(p.offer(H264.into()).await, Err(Refusal::Unavailable));
        let waited = t.elapsed();
        assert!(waited >= Duration::from_millis(900) && waited < Duration::from_millis(1500), "{waited:?}");
        assert!(cmds(&dir, "offer").is_empty(), "nothing is passed on before ready");
        // while the old one quits, offers are refused; once it is gone the next offer starts one
        settle(300).await;
        assert_eq!(p.offer(H264.into()).await, Err(Refusal::Unavailable));
        assert_eq!(starts(&dir), 2);
        p.stop().await.unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn restarted_after_it_exited() {
        let (l, dir) = stub_launch("restart", "", |c| c.preview.idle_exit_s = 1, 3);
        let p = Preview::start(Some(l));
        assert_eq!(p.offer(with(H264, "exit")).await, Err(Refusal::Unavailable), "the program died on it");
        let a = p.offer(H264.into()).await.unwrap();
        assert_eq!(starts(&dir), 2);
        // idle exit after the last viewer left; the next offer starts it again
        p.close(&a.id);
        settle(1500).await;
        assert!(seen(&dir).iter().any(|v| v["why"] == "idle"));
        assert!(p.offer(H265.into()).await.is_ok());
        assert_eq!(starts(&dir), 3);
        p.stop().await.unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn start_failures() {
        let (l, dir) = stub_launch("crash", "crash", |_| {}, 3);
        let p = Preview::start(Some(l));
        assert_eq!(p.offer(H264.into()).await, Err(Refusal::Unavailable));
        assert_eq!(p.offer(H264.into()).await, Err(Refusal::Unavailable));
        assert_eq!(starts(&dir), 2, "tried again on the next offer");
        p.stop().await.unwrap();
        // no interpreter at all
        let (l, dir2) = stub_launch("missing", "", |c| c.media.python = "/nonexistent/python3".into(), 3);
        let q = Preview::start(Some(l));
        assert_eq!(q.offer(H264.into()).await, Err(Refusal::Unavailable));
        q.stop().await.unwrap();
        // turned off
        let off = Preview::start(None);
        assert!(!off.enabled());
        assert_eq!(off.offer(H264.into()).await, Err(Refusal::Unavailable));
        off.stop().await.unwrap();
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&dir2).unwrap();
    }

    #[tokio::test]
    async fn stop_escalates_to_kill() {
        let (l, dir) = stub_launch("stubborn", "stubborn", |_| {}, 3);
        let p = Preview::start(Some(l));
        assert!(p.offer(H264.into()).await.is_ok());
        let t = Instant::now();
        p.stop().await.unwrap();
        let took = t.elapsed();
        assert!(took >= Duration::from_millis(950) && took < Duration::from_millis(2000), "{took:?}");
        let s = seen(&dir);
        assert!(s.iter().any(|v| v["cmd"] == "quit") && s.iter().any(|v| v["cmd"] == "eof"));
        assert!(!s.iter().any(|v| v.get("exit").is_some()), "it never exited by itself");
        assert!(fs::read_to_string(dir.join("preview.log")).unwrap().contains("# exit 137"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn hung_program_is_replaced() {
        let (l, dir) = stub_launch("hang", "", |_| {}, 3);
        let p = Preview::start(Some(l));
        assert!(p.offer(H264.into()).await.is_ok());
        // a program that keeps writing its stats is left alone
        settle(2000).await;
        assert_eq!(starts(&dir), 1);
        assert_eq!(p.offer(with(H264, "hang")).await, Err(Refusal::Unavailable), "no answer in time");
        // silent for longer than stall_after: stopped (it ignores quit; SIGTERM ends it), then a new one
        settle(2000).await;
        assert!(seen(&dir).iter().any(|v| v["hang"] == true));
        assert!(p.offer(H264.into()).await.is_ok());
        assert_eq!(starts(&dir), 2);
        assert!(fs::read_to_string(dir.join("preview.log.1")).unwrap().contains("# exit 143"));
        p.stop().await.unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn offer_racing_an_idle_exit_gets_one_more_try() {
        let (l, dir) = stub_launch("leave", "", |_| {}, 3);
        let p = Preview::start(Some(l));
        // the program leaves by itself (exit 0) just as the offer reaches it: a new one answers
        let a = p.offer(with(H264, "leave-once")).await.unwrap();
        assert_eq!(starts(&dir), 2);
        assert_eq!(cmds(&dir, "offer"), [a.id.clone(), a.id.clone()]);
        // only once, and never after a crash
        let (l, dir2) = stub_launch("leave2", "", |_| {}, 3);
        let q = Preview::start(Some(l));
        assert_eq!(q.offer(with(H264, "leave")).await, Err(Refusal::Unavailable));
        assert_eq!(starts(&dir2), 2);
        assert_eq!(q.offer(with(H264, "exit")).await, Err(Refusal::Unavailable));
        assert_eq!(starts(&dir2), 3, "a crash is not retried");
        p.stop().await.unwrap();
        q.stop().await.unwrap();
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&dir2).unwrap();
    }

    #[test]
    fn program_codes() {
        assert_eq!(Refusal::from_program("no_common_codec"), Refusal::NoCommonCodec);
        assert_eq!(Refusal::from_program("bad_offer"), Refusal::BadOffer);
        assert_eq!(Refusal::from_program("failed"), Refusal::Unavailable);
        assert_eq!(Refusal::from_program("anything"), Refusal::Unavailable);
        assert_ne!(random_id(), random_id());
        let l = log_line(r#"{"evt":"answer","id":"ab","sdp":"v=0\r\n...","codec":"h264"}"#);
        assert_eq!(l, r#"@out {"codec":"h264","evt":"answer","id":"ab"}"#);
    }

    #[test]
    fn stats_log_lines() {
        let parse = |s: &str| StatsNote::parse(&serde_json::from_str(s).unwrap());
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let fresh = |note: StatsNote| StatsLog::default().line(note, t0).unwrap();
        // an older program: no pacing fields
        let old = parse(r#"{"evt":"stats","viewers":{"h265":1,"h264":0},"in_fps":30.0,"connected":true}"#);
        assert_eq!(fresh(old), r#"viewers {"h264":0,"h265":1} video attached"#);
        let base = r#"{"evt":"stats","viewers":{"h265":1,"h264":0},"in_fps":30.0,"out_fps":{"h265":30.0,"h264":0.0},"connected":true"#;
        let line = |late: &str, delay: &str| parse(&format!(r#"{base},"late_frames":{late},"delay_ms":{delay}}}"#));
        let a = line("0", "1500");
        assert_eq!(fresh(a.clone()), r#"viewers {"h264":0,"h265":1} video attached, delay 1500 ms, late frames 0 since the previous line"#);
        // floats are rounded; other types and missing fields are left out
        let f = line("3.0", "1749.6");
        assert_eq!((f.delay_ms, f.late_frames), (Some(1750), Some(3)));
        let odd = parse(r#"{"evt":"stats","viewers":{},"connected":false,"late_frames":"x","delay_ms":null}"#);
        assert_eq!(fresh(odd), "viewers {} video not attached");

        let mut log = StatsLog::default();
        assert!(log.line(a.clone(), t0).is_some(), "the first line is logged");
        assert_eq!(log.line(a.clone(), at(30)), None, "nothing changed");
        // late frames reported between two logged lines (a burst, then calm again) are added up
        assert_eq!(log.line(line("4", "1500"), at(35)), None, "pacing alone: not yet");
        assert_eq!(log.line(line("2", "1500"), at(40)), None);
        let t = log.line(a.clone(), at(60)).expect("pacing alone: once a minute, though this line has none");
        assert!(t.ends_with("delay 1500 ms, late frames 6 since the previous line"), "{t}");
        assert_eq!(log.line(a.clone(), at(300)), None, "counted once; nothing changed since");
        // a longer delay alone: at most once a minute
        assert!(log.line(line("0", "2000"), at(305)).unwrap().contains("delay 2000 ms, late frames 0"));
        assert_eq!(log.line(line("0", "2500"), at(310)), None);
        assert!(log.line(line("0", "2500"), at(365)).unwrap().contains("delay 2500 ms"));
        // the viewers or the writer at once, with what was counted so far
        assert_eq!(log.line(line("1", "2500"), at(370)), None);
        let left = parse(r#"{"evt":"stats","viewers":{"h265":0,"h264":0},"connected":true,"late_frames":2,"delay_ms":2500}"#);
        assert!(log.line(left, at(375)).unwrap().ends_with("late frames 3 since the previous line"), "a viewer change at once");
        let detached = parse(r#"{"evt":"stats","viewers":{"h265":0,"h264":0},"connected":false,"late_frames":0,"delay_ms":2500}"#);
        assert!(log.line(detached, at(380)).is_some(), "the writer at once");
    }
}
