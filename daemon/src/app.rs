//! Shared state and the supervisor loop. The loop is the only owner of the state machine and
//! the sampler; after every step it publishes a snapshot that the HTTP handlers read.

use crate::config::{Config, Role};
use crate::events::{Ev, EventRing};
use crate::history::History;
use crate::link_filter::{self, LinkFilter};
use crate::media::{self, Real};
use crate::model::{
    Alert, DelayProfile, Device, InputKind, LinkState, Media, Mode, NetworkStored, Peer, Settings, Stats, Status,
    SIGNAL_FORMAT, TIMEZONE,
};
use crate::nettest::{NetTests, Server, ServerLaunch};
use crate::preview::{self, Preview};
use crate::profile::{Input, Plan, Profile, Profiles};
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
    /// The standard and the low-delay profile (profile.rs).
    pub profiles: Profiles,
    pub firmware: String,
    /// The browser preview (independent of the link; preview.rs).
    pub preview: Preview,
    /// The network tests and, on rx, the bandwidth tool's server (independent of the link; nettest.rs).
    pub nettest: NetTests,
    /// state_dir/link_filter.json for the radio line's filter (link_filter.rs). Its lock is taken
    /// after the state's, never the other way round.
    pub link_filter: Arc<LinkFilter>,
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
        Media { kind, label, format: (!lost).then(|| SIGNAL_FORMAT.into()), audio: "none", black: lost }
    }

    /// The delay profile of the applied settings: tx the transport settings' (what the next
    /// transmission uses), rx the transmitter's of the current or last connection.
    pub fn applied_delay(&self, i: &Inner) -> DelayProfile {
        match self.cfg.role {
            Role::Tx => self.profiles.in_effect(i.settings.transport.delay),
            Role::Rx => i.link.delay,
        }
    }

    pub fn applied_profile(&self, i: &Inner) -> &Profile {
        self.profiles.get(self.applied_delay(i))
    }

    /// Automatic mode can be in effect (VideoOptions.auto_available): not while the applied delay
    /// is low (it sends the fixed bitrate whatever the video mode says; the stored mode is kept).
    pub fn auto_available(&self, i: &Inner) -> bool {
        !self.applied_profile(i).fixed_bitrate
    }

    /// The bitrate automatic mode aims at now (its first target while nothing is measured).
    pub fn auto_mbps(&self, i: &Inner) -> f64 {
        match (i.settings.video.mode, i.stats.target_mbps) {
            (Mode::Auto, Some(t)) => t,
            _ => self.profiles.standard.initial_mbps(),
        }
    }

    /// The bitrate in effect now (the check rules compare a draft with it).
    pub fn current_mbps(&self, i: &Inner) -> f64 {
        match i.settings.video.mode {
            Mode::Auto if self.auto_available(i) => self.auto_mbps(i),
            _ => i.settings.video.manual_mbps,
        }
    }

    /// What the next transmission uses (fixed at its start: retries and reconnects keep it).
    pub(crate) fn plan(&self, i: &Inner) -> Plan {
        let (v, t) = (&i.settings.video, &i.settings.transport);
        // rx: the standard profile until the transmitter says otherwise (supervisor.rs)
        let delay = match self.cfg.role {
            Role::Tx => self.profiles.in_effect(t.delay),
            Role::Rx => DelayProfile::Standard,
        };
        let fixed = v.mode == Mode::Manual || self.profiles.get(delay).fixed_bitrate;
        Plan {
            peer_host: t.peer_host.clone(),
            peer_port: t.peer_port,
            listen_port: self.cfg.rx.listen_port,
            input: match v.input.kind {
                InputKind::Camera => Input::Camera(v.input.camera.clone()),
                InputKind::File => Input::File(v.input.file.clone()),
            },
            manual_kbps: fixed.then(|| (v.manual_mbps * 1000.0).round() as u32),
            maxbw_kbps: t.send_limit.kbps(),
            delay,
            low_ctl: self.profiles.low.player_ctl.clone(),
        }
    }

    /// 途中の無線機 in effect: the applied list, else device.toml's (version 1).
    pub fn network(&self, i: &Inner) -> NetworkStored {
        i.settings.network.clone().unwrap_or_else(|| store::network_defaults(&self.cfg))
    }

    /// The peer as the radio line's filter and the network check rules see it: tx the transport
    /// settings' peer host, rx the configured transmitter, else the last one that connected.
    fn peer_source(&self, i: &Inner) -> Option<String> {
        let p = match self.cfg.role {
            Role::Tx => Some(i.settings.transport.peer_host.clone()),
            Role::Rx => self.cfg.rx.peer_host.clone().or_else(|| i.link.peer_addr.as_ref().map(|(a, _)| a.clone())),
        };
        p.filter(|p| !p.is_empty())
    }

    /// The peer's addresses for the is_peer rule: as given, and what the filter file has (a host
    /// name looked up, or rx's transmitter from before a restart).
    pub fn peer_addresses(&self, i: &Inner) -> Vec<String> {
        let mut v: Vec<String> = self.peer_source(i).into_iter().collect();
        v.extend(self.link_filter.peer());
        v
    }

    /// What link_filter.json is made of now.
    pub fn filter_inputs(&self, i: &Inner) -> link_filter::Inputs {
        let video_port = match self.cfg.role {
            Role::Tx => i.settings.transport.peer_port,
            Role::Rx => self.cfg.rx.listen_port,
        };
        link_filter::Inputs {
            role: self.cfg.role,
            peer: self.peer_source(i),
            video_port,
            test_port: self.cfg.network_test.port,
            hosts: self.network(i).hops.into_iter().map(|h| h.address).collect(),
        }
    }

    /// Writes link_filter.json if what it is made of changed (the first call always writes).
    pub fn update_link_filter(&self, i: &Inner) {
        self.link_filter.update(self.filter_inputs(i));
    }

    /// The developer log line at each start: the profile, the send cap the sender gets and the bitrate.
    fn log_start(&self, i: &Inner, plan: &Plan) {
        if self.cfg.role != Role::Tx {
            return;
        }
        let p = self.profiles.get(plan.delay);
        let l = &i.settings.transport.send_limit;
        let rate = match plan.manual_kbps {
            Some(k) => format!("fixed {k} kbps"),
            None => format!("automatic from {} kbps", p.initial_mbps() * 1000.0),
        };
        let off = if i.settings.transport.delay != plan.delay { " (low is turned off in device.toml)" } else { "" };
        if crate::profile::takes_send_cap(p) {
            let at = l.measured_at.as_deref().map(|t| format!(", measured {t}")).unwrap_or_default();
            dlog!("start: profile {}{off}, send cap {} kbps ({}{at}), {rate}", plan.delay.name(), plan.maxbw_kbps, l.source.name());
        } else {
            let args = p.sender_args.join(" ");
            dlog!(
                "start: profile {}{off}, the profile's sender_args fix the send cap ({args}); the setting {} Mbps is not used, {rate}",
                plan.delay.name(),
                l.mbps
            );
        }
    }

    /// The profile the statistics follow: tx the running transmission's, or the applied one while
    /// stopped (what the next start uses); rx the transmitter's of the current or last connection.
    fn stats_profile<B: MediaBackend>(&self, m: &Machine<B>) -> &Profile {
        let d = match (self.cfg.role, m.link.state) {
            (Role::Tx, LinkState::Stopped) => self.profiles.in_effect(self.lock().settings.transport.delay),
            _ => m.link.delay,
        };
        self.profiles.get(d)
    }

    /// Point the sampler at the profile in effect (no change when it is the same).
    fn sync_sampler<B: MediaBackend>(&self, m: &Machine<B>, sampler: &mut Sampler) {
        sampler.set_profile(self.stats_profile(m), &self.profiles.standard.player_args);
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
    let profiles = Profiles::resolve(&cfg)?;
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
    let standard = &profiles.standard;
    let mut sampler = Sampler::new(cfg.role, standard.delay_target_ms, standard.margin_warn_s.unwrap_or(3.0), cap);
    let first = match cfg.role {
        Role::Tx => profiles.get(profiles.in_effect(settings.transport.delay)),
        Role::Rx => standard,
    };
    sampler.set_profile(first, &standard.player_args);
    let (cmd_tx, cmd_rx) = mpsc::channel(16);
    let (media_tx, media_rx) = mpsc::unbounded_channel();
    let (live, _) = broadcast::channel(256);
    let backend = Real::new(cfg.clone(), profiles.clone(), media_tx);
    if cfg.preview.enabled && cfg.preview_sock().as_os_str().len() >= 108 {
        dlog!("preview: {} is too long for a socket path; set a shorter run_dir", cfg.preview_sock().display());
    }
    let preview = Preview::start(preview::Launch::from_config(&cfg));
    let nettest = NetTests::new(&cfg, Server::start(ServerLaunch::from_config(&cfg)));
    let link_filter =
        Arc::new(LinkFilter::new(Some(state_dir.join(link_filter::FILE_NAME)), cfg.role, link_filter::system_resolver));
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
        profiles,
        firmware,
        preview,
        nettest,
        link_filter,
        inner: Mutex::new(inner),
        live,
        cmd: cmd_tx,
        settings_path,
    });
    app.emit(&mut app.lock(), Ev::info("device_started"));
    app.update_link_filter(&app.lock());

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
        maxbw_kbps: crate::model::SendLimit::default().kbps(),
        delay: DelayProfile::Standard,
        low_ctl: Vec::new(),
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
    let mut preview_done = None;
    let mut server_done = None;
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
                            let i = app.lock();
                            m.plan = app.plan(&i);
                            app.log_start(&i, &m.plan);
                            drop(i);
                            sampler.reset_run();
                        }
                        m.start(now);
                    }
                    CmdKind::Stop => m.stop(now),
                    CmdKind::Retry => m.retry(now),
                }
                app.sync_sampler(&m, &mut sampler);
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
                        m.on_media(ev, now);
                        // rx: the transmitter's stream id may have switched the profile
                        app.sync_sampler(&m, &mut sampler);
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
                app.sync_sampler(&m, &mut sampler);
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
                // a second signal: exit now (the children get SIGTERM from PDEATHSIG)
                if shutdown.is_some() { preview_done = None; server_done = None; break; }
                dlog!("SIGTERM: stopping the media programs");
                m.stop(Instant::now());
                preview_done = Some(app.preview.stop());
                server_done = Some(app.nettest.stop_server());
                shutdown = Some(Instant::now());
            }
            _ = next_signal(&mut int) => {
                if shutdown.is_some() { preview_done = None; server_done = None; break; }
                dlog!("SIGINT: stopping the media programs");
                m.stop(Instant::now());
                preview_done = Some(app.preview.stop());
                server_done = Some(app.nettest.stop_server());
                shutdown = Some(Instant::now());
            }
        }
    }
    // The preview program was asked to quit with the media programs (SIGKILL after 4 s), the
    // network test server too (SIGKILL after 2 s); not waited for after a second signal. A
    // network test still running dies with the daemon (PDEATHSIG).
    if let Some(t) = shutdown {
        let by = (t + Duration::from_millis(4500)).into();
        if let Some(done) = preview_done {
            let _ = tokio::time::timeout_at(by, done).await;
        }
        if let Some(done) = server_done {
            let _ = tokio::time::timeout_at(by, done).await;
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
            // rx: a transmitter connected (the filter lets it through); unchanged content is not rewritten
            if app.cfg.role == Role::Rx {
                app.update_link_filter(&i);
            }
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

/// An App for the HTTP tests: no supervisor loop, the link in `state`.
#[cfg(test)]
pub fn test_app(cfg: Config, preview: Preview, state: LinkState) -> Arc<App> {
    let nettest = NetTests::new(&cfg, Server::start(None));
    test_app_with(cfg, preview, state, nettest)
}

/// test_app with the network tests given (stub programs, short waits; nettest.rs). The settings
/// are read from the configured state_dir as at a start (the defaults when there is no file yet).
#[cfg(test)]
pub fn test_app_with(cfg: Config, preview: Preview, state: LinkState, nettest: NetTests) -> Arc<App> {
    use crate::supervisor::Link;
    let now = Instant::now();
    let link = Link {
        state,
        since: now,
        attempt: None,
        next_try: None,
        last_live_until: None,
        peer_addr: None,
        delay: DelayProfile::Standard,
        alerts: Vec::new(),
        rx_black: false,
        rev: 0,
    };
    // only from a state_dir the test chose (never the developer's own)
    let stored = cfg.state_dir.as_ref().and_then(|d| store::load(&d.join("settings.json")));
    let settings = stored.unwrap_or_else(|| store::defaults(&cfg));
    let inner = Inner { link, stats: Stats::default(), history: History::default(), events: EventRing::new(0), settings };
    let (cmd, _) = mpsc::channel(1);
    let (live, _) = broadcast::channel(16);
    // written as at a start, only into a state_dir the test chose
    let filter_path = cfg.state_dir.as_ref().map(|d| d.join(link_filter::FILE_NAME));
    let link_filter = Arc::new(LinkFilter::new(filter_path, cfg.role, link_filter::system_resolver));
    let app = Arc::new(App {
        profiles: Profiles::resolve(&cfg).expect("profiles"),
        firmware: "test".into(),
        preview,
        nettest,
        link_filter,
        inner: Mutex::new(inner),
        live,
        cmd,
        settings_path: cfg.state_dir().join("settings.json"),
        cfg,
    });
    app.update_link_filter(&app.lock());
    app
}
