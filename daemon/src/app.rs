//! Shared state and the supervisor loop. The loop is the only owner of the state machine and
//! the sampler; after every step it publishes a snapshot that the HTTP handlers read.

use crate::config::{Config, Role};
use crate::events::{Ev, EventRing};
use crate::history::History;
use crate::media::{self, Real};
use crate::model::{
    Alert, Device, InputKind, LinkState, Media, Mode, Peer, Settings, Stats, Status, SIGNAL_FORMAT, TIMEZONE,
};
use crate::profile::{Input, Plan, Profile};
use crate::stats::{Ctx, Sampler};
use crate::store;
use crate::supervisor::{Kind, Link, Machine, MediaBackend, MediaEvent};
use crate::util::{iso_at, iso_jst, now_ms};
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::signal::unix::{signal, Signal, SignalKind};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::{interval, MissedTickBehavior};

#[derive(Debug, Clone, Copy)]
pub enum CmdKind {
    Start,
    Stop,
    Retry,
}

pub struct Cmd {
    pub kind: CmdKind,
    pub reply: oneshot::Sender<()>,
}

/// What the handlers read; the supervisor loop writes it.
pub struct Inner {
    pub link: Link,
    pub stats: Stats,
    pub history: History,
    pub events: EventRing,
    pub settings: Settings,
}

pub struct App {
    pub cfg: Config,
    pub prof: Profile,
    pub firmware: String,
    inner: Mutex<Inner>,
    live: broadcast::Sender<Arc<str>>,
    cmd: mpsc::Sender<Cmd>,
    settings_path: PathBuf,
}

impl App {
    pub fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.live.subscribe()
    }

    /// Push a message on the live channel.
    pub fn send(&self, msg: &impl Serialize) {
        if let Ok(s) = serde_json::to_string(msg) {
            let _ = self.live.send(s.into());
        }
    }

    pub fn emit(&self, i: &mut Inner, ev: Ev) {
        dlog!("event {} {:?}", ev.code, ev.params);
        let e = i.events.push(ev, now_ms());
        self.send(&json!({ "type": "event", "event": e }));
    }

    pub fn push_status(&self, i: &Inner) {
        self.send(&json!({ "type": "status", "status": self.status(i) }));
    }

    pub fn hello(&self) -> String {
        let device = self.device();
        let i = self.lock();
        json!({ "type": "hello", "device": device, "status": self.status(&i), "stats": i.stats }).to_string()
    }

    pub fn save_settings(&self, s: &Settings) -> io::Result<()> {
        store::save(&self.settings_path, s)
    }

    pub async fn command(&self, kind: CmdKind) -> Status {
        let (tx, rx) = oneshot::channel();
        if self.cmd.send(Cmd { kind, reply: tx }).await.is_ok() {
            let _ = rx.await;
        }
        self.status(&self.lock())
    }

    pub fn device(&self) -> Device {
        let toward = match self.cfg.role {
            Role::Tx => self.lock().settings.transport.peer_host.clone(),
            Role::Rx => String::new(),
        };
        Device {
            id: self.cfg.id.clone(),
            role: self.cfg.role,
            address: crate::util::local_ipv4(&toward),
            firmware: self.firmware.clone(),
            serial: self.cfg.serial.clone(),
            timezone: TIMEZONE,
        }
    }

    pub fn status(&self, i: &Inner) -> Status {
        let l = &i.link;
        let now = Instant::now();
        let tx = self.cfg.role == Role::Tx;
        let t = &i.settings.transport;
        let peer = match (tx, &l.peer_addr) {
            (true, _) => Peer { id: None, address: t.peer_host.clone(), port: t.peer_port },
            (false, Some((a, p))) => Peer { id: None, address: a.clone(), port: *p },
            (false, None) => Peer { id: None, address: String::new(), port: 0 },
        };
        let waiting = tx && matches!(l.state, LinkState::Connecting | LinkState::Reconnecting);
        Status {
            server_time: iso_jst(now_ms()),
            state: l.state,
            since: iso_at(l.since),
            live_s: (l.state == LinkState::Live).then(|| now.saturating_duration_since(l.since).as_secs()),
            attempt: if l.state == LinkState::Reconnecting { l.attempt } else { None },
            next_retry_s: l
                .next_try
                .filter(|_| waiting)
                .map(|t| t.saturating_duration_since(now).as_secs_f64().ceil() as u64),
            // as the mock: only while waiting for the link to come back
            last_live_until: l
                .last_live_until
                .filter(|_| matches!(l.state, LinkState::Reconnecting | LinkState::Listening))
                .map(iso_at),
            peer,
            listen_port: (!tx).then_some(self.cfg.rx.listen_port),
            alerts: l
                .alerts
                .iter()
                .map(|a| Alert { level: a.level, code: a.code, since: iso_at(a.since), params: a.params.clone() })
                .collect(),
            media: self.media(i),
        }
    }

    fn media(&self, i: &Inner) -> Media {
        if self.cfg.role == Role::Rx {
            let black = i.link.rx_black;
            return Media {
                kind: "output",
                label: "OUT 1".into(),
                format: (!black).then(|| SIGNAL_FORMAT.into()),
                preview_fps: 5.0,
                audio: "none",
                black,
            };
        }
        let v = &i.settings.video.input;
        let lost = i.link.has("input_lost");
        let (kind, label) = match v.kind {
            InputKind::Camera => ("camera", media::camera_label(&v.camera)),
            InputKind::File => ("file", base_name(&v.file)),
        };
        Media { kind, label, format: (!lost).then(|| SIGNAL_FORMAT.into()), preview_fps: 5.0, audio: "none", black: lost }
    }

    /// The bitrate automatic mode aims at now (its first target while nothing is measured).
    pub fn auto_mbps(&self, i: &Inner) -> f64 {
        match (i.settings.video.mode, i.stats.target_mbps) {
            (Mode::Auto, Some(t)) => t,
            _ => self.prof.initial_mbps(),
        }
    }

    /// The bitrate in effect now (the check rules compare a draft with it).
    pub fn current_mbps(&self, i: &Inner) -> f64 {
        match i.settings.video.mode {
            Mode::Manual => i.settings.video.manual_mbps,
            Mode::Auto => self.auto_mbps(i),
        }
    }

    fn plan(&self, i: &Inner) -> Plan {
        let (v, t) = (&i.settings.video, &i.settings.transport);
        Plan {
            peer_host: t.peer_host.clone(),
            peer_port: t.peer_port,
            listen_port: self.cfg.rx.listen_port,
            input: match v.input.kind {
                InputKind::Camera => Input::Camera(v.input.camera.clone()),
                InputKind::File => Input::File(v.input.file.clone()),
            },
            manual_kbps: (v.mode == Mode::Manual).then(|| (v.manual_mbps * 1000.0).round() as u32),
        }
    }
}

pub fn base_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The commit of the media programs' checkout, if it is one.
fn system_version(repo: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--short=7", "HEAD"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !v.is_empty()).then_some(v)
}

/// Session directories left by an earlier run (only names this daemon makes: 20261104-104218-s0001).
fn remove_old_sessions(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let b = name.as_bytes();
        let ours = b.len() == 21
            && b[8] == b'-'
            && &b[15..17] == b"-s"
            && b.iter().enumerate().all(|(i, c)| matches!(i, 8 | 15 | 16) || c.is_ascii_digit());
        if ours && e.path().is_dir() {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

pub async fn run(cfg: Config) -> Result<(), String> {
    let prof = Profile::resolve(&cfg.profile)?;
    let (run_dir, state_dir, sessions) = (cfg.run_dir(), cfg.state_dir(), cfg.sessions_dir());
    for d in [&run_dir, &state_dir, &sessions] {
        fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    if cfg.archive_dir.is_none() {
        remove_old_sessions(&run_dir);
    }
    let settings_path = state_dir.join("settings.json");
    let mut settings = store::load(&settings_path).unwrap_or_else(|| store::defaults(&cfg));
    if settings.video.input.file.is_empty() {
        // No file chosen yet: the first one offered, as the list shows it (the mock does the same).
        if let Some(f) = media::media_files(cfg.tx.media_dir.as_deref()).first() {
            settings.video.input.file = f.path.clone();
        }
    }
    let firmware = match system_version(&cfg.media.repo) {
        Some(v) => format!("{} ({v})", env!("CARGO_PKG_VERSION")),
        None => env!("CARGO_PKG_VERSION").to_string(),
    };

    let now = Instant::now();
    let cap = cfg.archive_dir.is_none().then_some(crate::stats::FILE_CAP);
    let sampler = Sampler::new(cfg.role, prof.delay_target_ms, prof.margin_warn_s, cap);
    let (cmd_tx, cmd_rx) = mpsc::channel(16);
    let (media_tx, media_rx) = mpsc::unbounded_channel();
    let (live, _) = broadcast::channel(256);
    let backend = Real::new(cfg.clone(), prof.clone(), media_tx);
    let machine = Machine::new(cfg.role, backend, placeholder_plan(&cfg), sessions, now);
    let inner = Inner {
        link: machine.link.clone(),
        stats: sampler.blank(),
        history: History::default(),
        events: EventRing::new(now_ms() / 1000),
        settings,
    };
    let bind = format!("{}:{}", cfg.http.bind, cfg.http.port);
    let app = Arc::new(App {
        cfg,
        prof,
        firmware,
        inner: Mutex::new(inner),
        live,
        cmd: cmd_tx,
        settings_path,
    });
    app.emit(&mut app.lock(), Ev::info("device_started"));

    let listener = tokio::net::TcpListener::bind(&bind).await.map_err(|e| format!("{bind}: {e}"))?;
    dlog!("{} ({:?}) serving http://{bind}, sessions in {}", app.cfg.id, app.cfg.role, app.cfg.sessions_dir().display());
    let router = crate::api::router(app.clone());
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            dlog!("http: {e}");
        }
    });
    supervise(app, machine, sampler, cmd_rx, media_rx).await;
    Ok(())
}

fn placeholder_plan(cfg: &Config) -> Plan {
    Plan {
        peer_host: cfg.tx.peer_host.clone(),
        peer_port: cfg.tx.peer_port,
        listen_port: cfg.rx.listen_port,
        input: Input::Camera(cfg.tx.camera.clone()),
        manual_kbps: None,
    }
}

async fn next_signal(s: &mut Option<Signal>) {
    match s {
        Some(s) => {
            s.recv().await;
        }
        None => std::future::pending().await,
    }
}

async fn supervise<B: MediaBackend>(
    app: Arc<App>,
    mut m: Machine<B>,
    mut sampler: Sampler,
    mut cmds: mpsc::Receiver<Cmd>,
    mut media: mpsc::UnboundedReceiver<MediaEvent>,
) {
    let mut fast = interval(Duration::from_millis(200));
    fast.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut second = interval(Duration::from_secs(1));
    second.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut term = signal(SignalKind::terminate()).ok();
    let mut int = signal(SignalKind::interrupt()).ok();
    let mut published = u64::MAX;
    let mut kept: Option<PathBuf> = None;
    let mut shutdown: Option<Instant> = None;
    loop {
        tokio::select! {
            Some(cmd) = cmds.recv() => {
                let now = Instant::now();
                match cmd.kind {
                    // Shutting down: nothing new is started (it would only die with the daemon).
                    CmdKind::Start | CmdKind::Retry if shutdown.is_some() => {
                        dlog!("{:?} ignored: shutting down", cmd.kind);
                    }
                    CmdKind::Start => {
                        if m.link.state == LinkState::Stopped {
                            m.plan = app.plan(&app.lock());
                            sampler.reset_run();
                        }
                        m.start(now);
                    }
                    CmdKind::Stop => m.stop(now),
                    CmdKind::Retry => m.retry(now),
                }
                publish(&app, &mut m, &mut published, &mut kept);
                let _ = cmd.reply.send(());
            }
            Some(ev) = media.recv() => {
                let now = Instant::now();
                match ev {
                    MediaEvent::Stats { session, line } => sampler.player_line(session, &line, now),
                    ev => {
                        if let MediaEvent::Exit { session, kind: Kind::Player, .. } = ev {
                            sampler.player_done(session, now);
                        }
                        m.on_media(ev, now)
                    }
                }
                publish(&app, &mut m, &mut published, &mut kept);
            }
            _ = fast.tick() => {
                m.tick(Instant::now());
                publish(&app, &mut m, &mut published, &mut kept);
                if shutdown.is_some_and(|t| m.idle() || t.elapsed() > Duration::from_secs(6)) {
                    break;
                }
            }
            _ = second.tick() => {
                let now = Instant::now();
                let state = m.link.state;
                let out = sampler.sample(
                    Ctx { live: state == LinkState::Live, running: state != LinkState::Stopped, session: m.session() },
                    now,
                );
                m.observe(out.obs, now);
                {
                    let mut i = app.lock();
                    i.stats = out.stats;
                    i.history.push(out.sample, now_ms());
                }
                publish(&app, &mut m, &mut published, &mut kept);
                // the mock's order: stats, then status
                let i = app.lock();
                app.send(&json!({ "type": "stats", "stats": i.stats }));
                app.push_status(&i);
            }
            _ = next_signal(&mut term) => {
                if shutdown.is_some() { break; }
                dlog!("SIGTERM: stopping the media programs");
                m.stop(Instant::now());
                shutdown = Some(Instant::now());
            }
            _ = next_signal(&mut int) => {
                if shutdown.is_some() { break; }
                dlog!("SIGINT: stopping the media programs");
                m.stop(Instant::now());
                shutdown = Some(Instant::now());
            }
        }
    }
    dlog!("exit");
}

/// Copy the machine's state into the shared snapshot, push its events, push the status when it
/// changed, and tidy up finished session directories.
fn publish<B: MediaBackend>(app: &App, m: &mut Machine<B>, published: &mut u64, kept: &mut Option<PathBuf>) {
    {
        let mut i = app.lock();
        i.link = m.link.clone();
        for ev in m.out.drain(..) {
            app.emit(&mut i, ev);
        }
        if m.link.rev != *published {
            *published = m.link.rev;
            app.push_status(&i);
        }
    }
    for dir in m.finished.drain(..) {
        if dir.is_dir() {
            let _ = fs::write(dir.join("daemon.log"), crate::util::log_dump());
        }
        // Without an archive only the latest finished session is kept (for a developer to look at).
        if app.cfg.archive_dir.is_none() {
            if let Some(prev) = kept.replace(dir) {
                let _ = fs::remove_dir_all(prev);
            }
        }
    }
}
