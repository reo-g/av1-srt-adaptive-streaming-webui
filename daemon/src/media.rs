//! The real media backend: spawns the media programs, wires their pipes, reads their stderr
//! into the session's log files, and signals their process groups.
//!
//! Every child runs in its own process group with PR_SET_PDEATHSIG=SIGTERM, so no encoder keeps
//! the camera if the daemon dies. PDEATHSIG fires when the *thread* that forked exits: the
//! daemon runs a single-threaded runtime and spawns from it, never from spawn_blocking.
//!
//! Pipes: tx  bridge stdout → sender stdin; the bridge's stdin is the control input.
//!        rx  receiver fd 3 (SRT_RCV_PIPE=/dev/fd/3) → player stdin; player stats fd → daemon;
//!            daemon → player control fd (--ctl-fd: a low-delay transmitter is followed there).
//!
//! Environment: a child gets only the inherited variables in PASS_ENV (plus `media.pass_env`)
//! and then the profile's, so variables exported for experiments in the shell that started the
//! daemon never change what the children do or which files they write.

use crate::config::Config;
use crate::model::SIGNAL_FORMAT;
use crate::profile::{self, Env, Input, Plan, Profiles, Spec};
use crate::supervisor::{Kind, MediaBackend, MediaEvent, Session};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::UnboundedSender;

/// Inherited variables a child keeps (a trailing '*' is a prefix): what the programs need to
/// run (paths, locale, the user's runtime directories, where the encoder plugin and the video
/// driver are), nothing that changes their behaviour.
const PASS_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LANGUAGE",
    "LC_*",
    "TZ",
    "TMPDIR",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_DATA_DIRS",
    "XDG_STATE_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
    "LD_LIBRARY_PATH",
    "PYTHONPATH",
    "GST_PLUGIN_PATH",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_REGISTRY",
    "LIBVA_DRIVER_NAME",
    "LIBVA_DRIVERS_PATH",
    "SVTAV1_GST_PLUGIN_DIR",
    "SVTAV1_LIB_DIR",
];

/// Without an archive a child's log is cut back to its header (environment and command line)
/// once it passes this size (tmpfs; the sender alone writes about 0.8 MB an hour).
pub const LOG_CAP: u64 = 16 << 20;

fn passes(name: &str, extra: &[String]) -> bool {
    PASS_ENV.iter().copied().chain(extra.iter().map(String::as_str)).any(|p| match p.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == p,
    })
}

/// A child's whole environment: the inherited variables that pass, then the profile's.
pub fn child_env(inherited: impl IntoIterator<Item = (String, String)>, extra: &[String], profile: &Env) -> Env {
    let mut env: Env = inherited.into_iter().filter(|(k, _)| passes(k, extra)).collect();
    env.extend(profile.iter().map(|(k, v)| (k.clone(), v.clone())));
    env
}

pub fn inherited() -> impl Iterator<Item = (String, String)> {
    std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
}

/// Cut a log back to its first `keep` bytes once it is over `cap` (it is opened for appending,
/// so every writer, the player's stdout included, carries on at the new end).
pub fn cap_log(log: &mut File, limit: Option<(u64, u64)>) {
    let Some((cap, keep)) = limit else { return };
    if log.metadata().is_ok_and(|m| m.len() > cap) && log.set_len(keep).is_ok() {
        let _ = writeln!(log, "# older lines dropped (the log passed {} MiB)", cap >> 20);
    }
}

struct Child {
    pid: i32,
    exited: Arc<AtomicBool>,
}

pub struct Real {
    cfg: Config,
    profiles: Profiles,
    events: UnboundedSender<MediaEvent>,
    children: HashMap<(u64, Kind), Child>,
    /// The video pipe end waiting for the session's second child (sender → bridge, player → receiver).
    pipes: HashMap<u64, OwnedFd>,
    /// The bridge's control input.
    ctl: HashMap<u64, File>,
    /// The player's control input (--ctl-fd; non-blocking: a few short lines per session).
    player_ctl: HashMap<u64, File>,
}

impl Real {
    pub fn new(cfg: Config, profiles: Profiles, events: UnboundedSender<MediaEvent>) -> Self {
        Real {
            cfg,
            profiles,
            events,
            children: HashMap::new(),
            pipes: HashMap::new(),
            ctl: HashMap::new(),
            player_ctl: HashMap::new(),
        }
    }

    /// The commands of the session's profile (rx: the standard one; the player follows a
    /// low-delay transmitter through its control input).
    fn spec(&self, s: &Session, kind: Kind, plan: &Plan) -> Spec {
        let p = self.profiles.get(plan.delay);
        match kind {
            Kind::Sender => profile::sender(&self.cfg, p, plan, &s.dir),
            Kind::Bridge => profile::bridge(&self.cfg, p, plan, &s.dir),
            Kind::Receiver => profile::receiver(&self.cfg, p, plan, &s.dir),
            Kind::Player => profile::player(&self.cfg, p, &s.dir),
        }
    }
}

impl MediaBackend for Real {
    fn check_input(&mut self, input: &Input) -> Result<(), String> {
        match input {
            Input::Camera(dev) => camera_ok(dev),
            Input::File(f) if Path::new(f).is_file() => Ok(()),
            Input::File(f) => Err(format!("{f}: no such file")),
        }
    }

    fn spawn(&mut self, s: &Session, kind: Kind, plan: &Plan) -> io::Result<()> {
        fs::create_dir_all(&s.dir)?;
        let spec = self.spec(s, kind, plan);
        let mut log = OpenOptions::new().create(true).append(true).open(s.dir.join(format!("{}.log", kind.name())))?;
        let env = child_env(inherited(), &self.cfg.media.pass_env, &spec.env);
        // What ran, for offline analysis: the whole environment, then the command line.
        for (k, v) in &env {
            writeln!(log, "# env {k}={v}")?;
        }
        writeln!(log, "# exec {} {}", spec.program.display(), spec.args.join(" "))?;
        let header = log.stream_position()?;
        let log_limit = self.cfg.archive_dir.is_none().then_some((LOG_CAP, header));

        let mut cmd = std::process::Command::new(&spec.program);
        cmd.args(&spec.args)
            .env_clear()
            .envs(&env)
            .current_dir(&spec.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut held: Vec<OwnedFd> = Vec::new(); // the child's pipe ends; closed here right after the spawn
        let mut extra: Vec<(RawFd, RawFd)> = Vec::new(); // (our fd, fd number in the child)
        let mut ctl = None;
        let mut player_ctl = None;
        let mut stats = None;
        match kind {
            Kind::Sender => {
                let (r, w) = pipe()?;
                cmd.stdin(Stdio::from(r));
                self.pipes.insert(s.id, w);
            }
            Kind::Bridge => {
                let video = self.pipes.remove(&s.id).ok_or_else(|| io::Error::other("no sender pipe"))?;
                cmd.stdout(Stdio::from(video));
                let (r, w) = pipe()?;
                cmd.stdin(Stdio::from(r));
                ctl = Some(File::from(w));
            }
            Kind::Player => {
                let (r, w) = pipe()?;
                // room for about 2 s of video, so the receiver never waits on the player
                // SAFETY: fcntl on a pipe fd we own.
                unsafe { libc::fcntl(w.as_raw_fd(), libc::F_SETPIPE_SZ, 1 << 20) };
                cmd.stdin(Stdio::from(r)).stdout(Stdio::from(log.try_clone()?));
                self.pipes.insert(s.id, w);
                if let Some(fd) = profile::stats_fd(&spec.args) {
                    let (sr, sw) = pipe()?;
                    extra.push((sw.as_raw_fd(), fd));
                    held.push(sw);
                    stats = Some(sr);
                }
                if let Some(fd) = profile::ctl_fd(&spec.args) {
                    let (cr, cw) = pipe()?;
                    // never wait on the player: a line that does not fit is logged and dropped
                    // SAFETY: fcntl on a pipe fd we own.
                    unsafe { libc::fcntl(cw.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
                    extra.push((cr.as_raw_fd(), fd));
                    held.push(cr);
                    player_ctl = Some(File::from(cw));
                }
            }
            Kind::Receiver => {
                let video = self.pipes.remove(&s.id).ok_or_else(|| io::Error::other("no player pipe"))?;
                extra.push((video.as_raw_fd(), 3));
                held.push(video);
            }
        }
        let parent = std::process::id();
        // SAFETY: child_setup only makes async-signal-safe calls.
        unsafe {
            cmd.pre_exec(move || child_setup(parent, &extra));
        }
        cmd.process_group(0);
        let mut tcmd = tokio::process::Command::from(cmd);
        let spawned = tcmd.spawn();
        // The command and `held` own the parent's copies of the child's pipe ends: close them now,
        // or the next child would never see the end of its input.
        drop(tcmd);
        drop(held);
        let mut child = spawned.map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", spec.program.display())))?;

        let pid = child.id().map_or(0, |p| p as i32);
        let exited = Arc::new(AtomicBool::new(false));
        self.children.insert((s.id, kind), Child { pid, exited: exited.clone() });
        if let Some(c) = ctl {
            self.ctl.insert(s.id, c);
        }
        if let Some(c) = player_ctl {
            self.player_ctl.insert(s.id, c);
        }
        dlog!("session {}: {} started, pid {pid}", s.id, kind.name());
        let stderr = child.stderr.take();
        tokio::spawn(watch(child, stderr, log, log_limit, self.events.clone(), s.id, kind, exited));
        if let Some(sr) = stats {
            tokio::spawn(read_stats(sr, self.events.clone(), s.id));
        }
        Ok(())
    }

    fn control(&mut self, session: u64, line: Option<&str>) {
        match line {
            Some(l) => {
                if let Some(f) = self.ctl.get_mut(&session) {
                    if let Err(e) = writeln!(f, "{l}") {
                        dlog!("session {session}: bridge control: {e}");
                    }
                }
            }
            None => {
                self.ctl.remove(&session);
            }
        }
    }

    fn player_control(&mut self, session: u64, line: &str) {
        match self.player_ctl.get_mut(&session) {
            Some(f) => match f.write_all(format!("{line}\n").as_bytes()) {
                Ok(()) => dlog!("session {session}: player control: {line}"),
                Err(e) => dlog!("session {session}: player control {line:?}: {e}"),
            },
            None => dlog!("session {session}: player control {line:?}: the player has no control input"),
        }
    }

    fn signal(&mut self, session: u64, kind: Kind, kill: bool) {
        if let Some(c) = self.children.get(&(session, kind)) {
            if !c.exited.load(Ordering::Relaxed) && c.pid > 0 {
                let sig = if kill { libc::SIGKILL } else { libc::SIGTERM };
                // SAFETY: signals the child's own process group (process_group(0) → pgid = pid).
                unsafe { libc::kill(-c.pid, sig) };
                dlog!("session {session}: {} {} sent", kind.name(), if kill { "SIGKILL" } else { "SIGTERM" });
            }
        }
        self.children.retain(|_, c| !c.exited.load(Ordering::Relaxed));
    }

    fn release(&mut self, session: u64) {
        self.pipes.remove(&session);
        self.ctl.remove(&session);
        self.player_ctl.remove(&session);
        self.children.retain(|_, c| !c.exited.load(Ordering::Relaxed));
    }
}

/// Runs in the child between fork and exec: async-signal-safe calls only.
pub fn child_setup(parent: u32, extra: &[(RawFd, RawFd)]) -> io::Result<()> {
    // SAFETY: plain syscalls on fds this process owns.
    unsafe {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::getppid() as u32 != parent {
            libc::_exit(1); // the daemon died before the prctl took effect
        }
        for &(src, dst) in extra {
            // dup2 clears close-on-exec on the new fd
            if libc::dup2(src, dst) < 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// A close-on-exec pipe whose fds are ≥ 10, so they never collide with the fd numbers a
/// child gets (3, 4) when they are dup2'ed in the child.
fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: pipe2 fills two new fds that we then own.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: just created above, owned by nobody else.
    let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    Ok((high(r)?, high(w)?))
}

fn high(fd: OwnedFd) -> io::Result<OwnedFd> {
    // SAFETY: F_DUPFD_CLOEXEC returns a new fd we own; the old one is closed on drop.
    let n = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(n) })
}

/// Forwards the child's stderr lines (and writes them to its log), then reports its exit.
#[allow(clippy::too_many_arguments)]
async fn watch(
    mut child: tokio::process::Child,
    stderr: Option<tokio::process::ChildStderr>,
    mut log: File,
    log_limit: Option<(u64, u64)>,
    tx: UnboundedSender<MediaEvent>,
    session: u64,
    kind: Kind,
    exited: Arc<AtomicBool>,
) {
    let mut out = |buf: &mut Vec<u8>| {
        let text = String::from_utf8_lossy(buf);
        let line = text.trim_end_matches(['\n', '\r']).to_string();
        let _ = writeln!(log, "{line}");
        cap_log(&mut log, log_limit);
        let _ = tx.send(MediaEvent::Line { session, kind, line });
        buf.clear();
    };
    let mut buf = Vec::new();
    let status = match stderr {
        None => child.wait().await,
        Some(e) => {
            let mut r = BufReader::new(e);
            let mut eof = false;
            let status = loop {
                tokio::select! {
                    n = r.read_until(b'\n', &mut buf), if !eof => match n {
                        Ok(0) | Err(_) => eof = true,
                        Ok(_) => out(&mut buf),
                    },
                    st = child.wait() => break st,
                }
            };
            // what is still in the pipe (a grandchild may hold it open: do not wait long)
            if !eof {
                let _ = tokio::time::timeout(Duration::from_millis(300), async {
                    while let Ok(n) = r.read_until(b'\n', &mut buf).await {
                        if n == 0 {
                            break;
                        }
                        out(&mut buf);
                    }
                })
                .await;
            }
            if !buf.is_empty() {
                out(&mut buf);
            }
            status
        }
    };
    exited.store(true, Ordering::Relaxed);
    let code = status.ok().and_then(|s| s.code().or_else(|| s.signal().map(|g| 128 + g)));
    let _ = tx.send(MediaEvent::Exit { session, kind, code });
}

async fn read_stats(fd: OwnedFd, tx: UnboundedSender<MediaEvent>, session: u64) {
    let Ok(rx) = tokio::net::unix::pipe::Receiver::from_owned_fd(fd) else { return };
    let mut lines = BufReader::new(rx).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let _ = tx.send(MediaEvent::Stats { session, line });
    }
}

// ------------------------------------------------------------------ inputs

/// Ok when the camera node opens and nobody is streaming from it. Opening alone succeeds even
/// while another process captures; asking to release buffers (count 0) fails with EBUSY then.
fn camera_ok(dev: &str) -> Result<(), String> {
    #[repr(C)]
    struct RequestBuffers {
        count: u32,
        kind: u32,
        memory: u32,
        capabilities: u32,
        flags: u8,
        reserved: [u8; 3],
    }
    const VIDIOC_REQBUFS: libc::c_ulong = 0xC014_5608; // _IOWR('V', 8, struct v4l2_requestbuffers)
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(dev)
        .map_err(|e| format!("{dev}: {e}"))?;
    let mut rb = RequestBuffers { count: 0, kind: 1, memory: 1, capabilities: 0, flags: 0, reserved: [0; 3] };
    // SAFETY: VIDIOC_REQBUFS with a correctly sized struct on an fd we own.
    let r = unsafe { libc::ioctl(f.as_raw_fd(), VIDIOC_REQBUFS as _, &mut rb as *mut RequestBuffers) };
    if r < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EBUSY) {
        return Err(format!("{dev}: in use"));
    }
    Ok(())
}

/// The camera's name from sysfs (e.g. "Facecam 4K"), else the node name.
pub fn camera_label(dev: &str) -> String {
    let node = Path::new(dev).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    fs::read_to_string(format!("/sys/class/video4linux/{node}/name"))
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or(node)
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaFile {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration_s: u64,
}

impl MediaFile {
    pub fn format(&self) -> String {
        let fps = if (self.fps - self.fps.round()).abs() < 0.01 {
            format!("{}", self.fps.round())
        } else {
            format!("{:.2}", self.fps)
        };
        format!("{}×{}\u{3000}{fps}p", self.width, self.height)
    }
    pub fn selectable(&self) -> bool {
        self.format() == SIGNAL_FORMAT
    }
}

/// Header of an uncompressed y4m file; duration = (size − header) / (6 + W·H·1.5) / fps.
pub fn y4m_info(path: &Path) -> Option<MediaFile> {
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; 512];
    let n = f.read(&mut head).ok()?;
    let end = head[..n].iter().position(|&b| b == b'\n')?;
    let header = std::str::from_utf8(&head[..end]).ok()?;
    let mut parts = header.split(' ');
    if parts.next()? != "YUV4MPEG2" {
        return None;
    }
    let (mut w, mut h, mut fps) = (0u32, 0u32, 0.0f64);
    for p in parts {
        match p.split_at_checked(1)? {
            ("W", v) => w = v.parse().ok()?,
            ("H", v) => h = v.parse().ok()?,
            ("F", v) => {
                let (a, b) = v.split_once(':')?;
                fps = a.parse::<f64>().ok()? / b.parse::<f64>().ok()?.max(1.0);
            }
            _ => {}
        }
    }
    if w == 0 || h == 0 || fps <= 0.0 {
        return None;
    }
    let size = f.metadata().ok()?.len();
    let frame = 6 + u64::from(w) * u64::from(h) * 3 / 2;
    let frames = size.saturating_sub(end as u64 + 1) / frame;
    Some(MediaFile {
        path: path.to_string_lossy().into_owned(),
        width: w,
        height: h,
        fps,
        duration_s: (frames as f64 / fps) as u64,
    })
}

/// The 4K30 y4m files in the media folder, by name.
pub fn media_files(dir: Option<&Path>) -> Vec<MediaFile> {
    let Some(dir) = dir else { return Vec::new() };
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<MediaFile> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "y4m"))
        .filter_map(|p| y4m_info(&p))
        .filter(MediaFile::selectable)
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn y4m_headers() {
        let d = std::env::temp_dir().join(format!("ctrld-media-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let frame = 6 + 3840 * 2160 * 3 / 2;
        let header = b"YUV4MPEG2 W3840 H2160 F30:1 Ip A1:1 C420jpeg\n";
        let mut f = File::create(d.join("a.y4m")).unwrap();
        f.write_all(header).unwrap();
        f.set_len((header.len() + 61 * frame) as u64).unwrap();
        fs::write(d.join("b.y4m"), b"YUV4MPEG2 W1920 H1080 F30000:1001 Ip C420\n").unwrap();
        fs::write(d.join("c.txt"), b"x").unwrap();
        let a = y4m_info(&d.join("a.y4m")).unwrap();
        assert_eq!((a.width, a.height, a.duration_s), (3840, 2160, 2));
        assert_eq!(a.format(), SIGNAL_FORMAT);
        let b = y4m_info(&d.join("b.y4m")).unwrap();
        assert_eq!(b.format(), "1920×1080\u{3000}29.97p");
        let list = media_files(Some(&d));
        assert_eq!(list.len(), 1, "only 4K30 can be chosen");
        assert!(list[0].path.ends_with("a.y4m"));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn children_inherit_only_the_listed_variables() {
        let inherited = [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/op"),
            ("LC_ALL", "C.UTF-8"),
            ("SVTAV1_LIB_DIR", "/opt/svt/lib"),
            ("LAT_LOG", "/tmp/lat.csv"),
            ("SVT_PRED_STRUCT", "low-delay"),
            ("SRT_MAXBW_SCHEDULE", "0:3000"),
            ("BUFCTRL_RUN_DIR", "/tmp/run"),
            ("BUF_ND_D0_S", "4"),
            ("GST_DEBUG", "3"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        let mut profile = Env::new();
        profile.insert("BUF_ND_D0_S".into(), "9.5".into());
        profile.insert("PATH".into(), "/opt/bin:/usr/bin".into());
        let env = child_env(inherited.clone(), &[], &profile);
        let names: Vec<&str> = env.keys().map(String::as_str).collect();
        assert_eq!(names, ["BUF_ND_D0_S", "HOME", "LC_ALL", "PATH", "SVTAV1_LIB_DIR"]);
        assert_eq!(env["BUF_ND_D0_S"], "9.5", "the profile wins");
        assert_eq!(env["PATH"], "/opt/bin:/usr/bin");
        // device.toml can pass more
        let env = child_env(inherited, &["GST_DEBUG*".to_string()], &Env::new());
        assert_eq!(env["GST_DEBUG"], "3");
        assert!(!env.contains_key("LAT_LOG"));
    }

    #[test]
    fn logs_are_cut_back_to_their_header() {
        let p = std::env::temp_dir().join(format!("ctrld-log-{}.log", std::process::id()));
        let _ = fs::remove_file(&p);
        let mut log = OpenOptions::new().create(true).append(true).open(&p).unwrap();
        writeln!(log, "# exec sender").unwrap();
        let keep = log.stream_position().unwrap();
        let other = log.try_clone().unwrap(); // the player's stdout shares the file
        for i in 0..100 {
            writeln!(log, "line {i}").unwrap();
            cap_log(&mut log, Some((256, keep)));
        }
        writeln!(&other, "from stdout").unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert!(text.starts_with("# exec sender\n# older lines dropped"), "{text}");
        assert!(text.len() <= 256 + 64, "{}", text.len());
        assert!(text.ends_with("line 99\nfrom stdout\n"), "{text}");
        cap_log(&mut log, None);
        fs::remove_file(&p).unwrap();
    }

    #[test]
    fn missing_camera() {
        assert!(camera_ok("/dev/does-not-exist").is_err());
        assert_eq!(camera_label("/dev/does-not-exist"), "does-not-exist");
    }
}
