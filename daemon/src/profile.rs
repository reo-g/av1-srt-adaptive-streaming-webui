//! The profiles: environment and arguments of each child. The defaults are the standard
//! profile (= the current automatic mode); device.toml can change any of them, so the
//! controller settings being tuned are not fixed in the daemon. The low-delay profile (低遅延)
//! is the resolved standard profile with a few changes (`Profile::low`), then [profile_low].
//! Also builds the command line of each child for a session (pure functions, unit-tested).

use crate::config::{Config, ProfileConfig, ProfileLowConfig};
use crate::model::DelayProfile;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub type Env = BTreeMap<String, String>;

#[derive(Debug, Clone)]
pub struct Profile {
    /// Which profile this is (Stats.delay_profile, the sender's stream id).
    pub delay: DelayProfile,
    pub sender_env: Env,
    pub bridge_env: Env,
    pub bridge_manual_env: Env,
    pub receiver_env: Env,
    pub sender_args: Vec<String>,
    pub receiver_args: Vec<String>,
    pub player_args: Vec<String>,
    /// rx: lines written to the player's control input (--ctl-fd) when a transmitter with this
    /// profile connects (the player always starts with the standard profile's arguments).
    pub player_ctl: Vec<String>,
    /// Capture-to-display delay of the profile (Stats.delay_target_ms).
    pub delay_target_ms: u32,
    /// delay_margin_low is raised below this margin; None: no margin warning and no margin shown.
    pub margin_warn_s: Option<f64>,
    /// The bitrate is always the fixed one (encode.manual), whatever the video mode says: no
    /// automatic steps, no never-drop controller (bridge_manual_env).
    pub fixed_bitrate: bool,
}

/// The standard and the low-delay profile, resolved once at the start.
#[derive(Debug, Clone)]
pub struct Profiles {
    pub standard: Profile,
    pub low: Profile,
    /// [profile_low] enabled: low is offered in the transport settings.
    pub low_enabled: bool,
}

impl Profiles {
    pub fn resolve(cfg: &Config) -> Result<Profiles, String> {
        let standard = Profile::resolve(&cfg.profile)?;
        let low = Profile::resolve_low(&standard, &cfg.profile_low)?;
        Ok(Profiles { standard, low, low_enabled: cfg.profile_low.enabled })
    }

    pub fn get(&self, d: DelayProfile) -> &Profile {
        match d {
            DelayProfile::Standard => &self.standard,
            DelayProfile::Low => &self.low,
        }
    }

    /// The profile a transmission with this setting uses: low only while it is offered (a
    /// settings.json from before [profile_low] enabled = false still says low).
    pub fn in_effect(&self, d: DelayProfile) -> DelayProfile {
        if d == DelayProfile::Low && !self.low_enabled {
            DelayProfile::Standard
        } else {
            d
        }
    }
}

fn env(pairs: &[(&str, &str)]) -> Env {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

impl Profile {
    pub fn standard() -> Profile {
        Profile {
            delay: DelayProfile::Standard,
            sender_env: env(&[
                ("SRT_NODROP", "1"),
                ("SRT_OHEADBW", "50"),
                ("SRT_PEERIDLETIMEO_MS", "30000"),
                ("SRT_SNDBUF", "33554432"),
                ("SRT_RCVBUF", "33554432"),
                ("SRT_FC", "131072"),
                ("SRT_STATS_PERIOD_MS", "1000"),
                ("SRT_FAST_STATS_MS", "100"),
                ("SRT_CLEAN_SIGTERM", "1"),
                ("SRT_DRAIN_MAX_MS", "3000"),
            ]),
            bridge_env: env(&[
                ("SRT_NODROP", "1"),
                ("BUFCTRL_VARIANT", "V1"),
                ("BUF_NODROP", "1"),
                ("BUF_ND_PACE", "1"),
                ("SVT_PRESET", "8"),
                ("GOP", "60"),
                ("INITIAL_TARGET", "3500"),
                ("EMA_ALPHA", "0.2"),
                ("SIGNIFICANT_DELTA_KBPS", "400"),
                ("HYSTER_DOWN_SEC", "10"),
                ("HYSTER_UP_SEC", "60"),
                ("DISABLE_UPSHIFT", "0"),
                ("POLL_INTERVAL_SEC", "1"),
                ("USE_NATIVE_LOSS", "0"),
                ("STREAM_ALIGN", "tu"),
                ("SOURCE_KIND", "v4l2"),
                ("CAM_PTS", "v4l2"),
                ("TOTAL_SECONDS", "0"),
                ("BRIDGE_CTL_STDIN", "1"),
            ]),
            // Fixed bitrate (not validated live yet): no automatic steps, no never-drop controller.
            bridge_manual_env: env(&[
                ("DISABLE_UPSHIFT", "1"),
                ("SIGNIFICANT_DELTA_KBPS", "1000000"),
                ("BUF_NODROP", "0"),
                ("BUF_ND_PACE", "0"),
            ]),
            receiver_env: env(&[
                ("SRT_NODROP", "1"),
                ("SRT_PEERIDLETIMEO_MS", "30000"),
                ("SRT_RCVBUF", "33554432"),
                ("SRT_FC", "131072"),
                ("SRT_RCV_STATS_MS", "1000"),
                ("SRT_RCV_CLOSE_LISTEN", "1"),
            ]),
            // the 7th argument is the sender's MAXBW (kbps): the send cap of the transport settings
            sender_args: args(&["-", "{peer_host}", "{peer_port}", "8000", "live", "10000", "{maxbw_kbps}"]),
            receiver_args: args(&["{out}", "{listen_port}", "8000"]),
            player_args: args(&[
                "--out",
                "/dev/null",
                "--dec",
                "vaav1dec",
                "--ts-offset-ms",
                "7000",
                "--no-record",
                "--stats-fd",
                "4",
            ]),
            player_ctl: Vec::new(),
            delay_target_ms: 9500,
            margin_warn_s: Some(3.0),
            fixed_bitrate: false,
        }
    }

    /// The standard profile: the defaults, then [profile].
    pub fn resolve(c: &ProfileConfig) -> Result<Profile, String> {
        let mut p = Self::standard();
        p.layer(c)?;
        Ok(p)
    }

    /// The low-delay profile's own changes to the resolved standard profile (docs/12 of the system
    /// repo): the encoder without look-ahead (preset 10, 3 hierarchical levels), small messages sent
    /// at once (only in this profile: in the standard one they make the keyframes burst past the
    /// send cap, docs/12 §12.6), always the fixed bitrate, and on the receiver a display offset of
    /// 400 ms with the margin watch. Never-drop, the access-unit alignment, the camera's capture
    /// times, the send cap and the receiver's arguments stay those of the standard profile.
    pub fn low(standard: &Profile) -> Profile {
        let mut p = standard.clone();
        p.delay = DelayProfile::Low;
        p.sender_env.insert("SRT_SEND_PARTIAL".into(), "1".into());
        p.bridge_env.insert("SVT_PRESET".into(), "10".into());
        p.bridge_env.insert("SVT_PARAMS".into(), "hierarchical-levels=3:lookahead=0".into());
        p.player_ctl = args(&["ts-offset-ms 400", "adapt on"]);
        p.delay_target_ms = 1500;
        p.margin_warn_s = None;
        p.fixed_bitrate = true;
        p
    }

    /// The low-delay profile: `low(standard)`, then [profile_low].
    pub fn resolve_low(standard: &Profile, c: &ProfileLowConfig) -> Result<Profile, String> {
        let mut p = Self::low(standard);
        p.layer(&c.changes()).map_err(|e| e.replacen("profile:", "profile_low:", 1))?;
        if let Some(lines) = &c.player_ctl {
            if let Some(bad) = lines.iter().find(|l| l.contains(['\n', '\r'])) {
                return Err(format!("profile_low.player_ctl: {bad:?} is more than one line"));
            }
            p.player_ctl = lines.iter().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        }
        Ok(p)
    }

    /// A table of device.toml on top: env values replace (an empty string removes), args replace.
    fn layer(&mut self, c: &ProfileConfig) -> Result<(), String> {
        merge(&mut self.sender_env, &c.sender_env)?;
        merge(&mut self.bridge_env, &c.bridge_env)?;
        merge(&mut self.bridge_manual_env, &c.bridge_manual_env)?;
        merge(&mut self.receiver_env, &c.receiver_env)?;
        if let Some(a) = &c.sender_args {
            self.sender_args = a.clone();
        }
        if let Some(a) = &c.receiver_args {
            self.receiver_args = a.clone();
        }
        if let Some(a) = &c.player_args {
            self.player_args = a.clone();
        }
        if let Some(v) = c.delay_target_ms {
            self.delay_target_ms = v;
        }
        if let Some(v) = c.delay_margin_warn_s {
            self.margin_warn_s = Some(v);
        }
        Ok(())
    }

    /// The player's display offset when a transmitter with this profile has just connected: the
    /// last `ts-offset-ms` line of player_ctl, else `--ts-offset-ms` of the player's arguments
    /// (the player starts with the standard profile's). The player raises it when frames come
    /// late (its margin watch), so the delay aimed at grows by as much (stats.rs).
    pub fn initial_offset_ms(&self, start_args: &[String]) -> Option<f64> {
        let from_ctl = self.player_ctl.iter().rev().find_map(|l| {
            let mut w = l.split_whitespace();
            (w.next() == Some("ts-offset-ms")).then(|| w.next()?.parse::<f64>().ok()).flatten()
        });
        from_ctl.or_else(|| arg_value(start_args, "--ts-offset-ms")?.parse().ok())
    }

    /// Automatic mode's first target, Mbps (shown while nothing is measured).
    pub fn initial_mbps(&self) -> f64 {
        self.bridge_env.get("INITIAL_TARGET").and_then(|v| v.parse::<f64>().ok()).unwrap_or(3500.0) / 1000.0
    }

    /// Seconds between keyframes (GOP frames at 30 fps).
    pub fn keyframe_s(&self) -> f64 {
        self.bridge_env.get("GOP").and_then(|v| v.parse::<f64>().ok()).unwrap_or(60.0) / 30.0
    }
}

fn merge(dst: &mut Env, src: &BTreeMap<String, toml::Value>) -> Result<(), String> {
    for (k, v) in src {
        let s = match v {
            toml::Value::String(s) => s.clone(),
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::Float(f) => f.to_string(),
            toml::Value::Boolean(b) => if *b { "1" } else { "0" }.to_string(),
            _ => return Err(format!("profile: {k} must be a string, a number or a boolean")),
        };
        if s.is_empty() {
            dst.remove(k);
        } else {
            dst.insert(k.clone(), s);
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ one session's commands

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Camera(String),
    File(String),
}

/// What a session needs from the applied settings.
#[derive(Debug, Clone)]
pub struct Plan {
    pub peer_host: String,
    pub peer_port: u16,
    pub listen_port: u16,
    pub input: Input,
    /// Some(kbps) when the bitrate is fixed: the video mode is "fixed bitrate", or the profile
    /// knows no other (low delay).
    pub manual_kbps: Option<u32>,
    /// The send cap (送り出しの上限) in kbps: `{maxbw_kbps}` in the sender's arguments.
    pub maxbw_kbps: u32,
    /// The session's profile (the media backend picks the commands by it): tx the applied
    /// transport settings' delay; rx always standard (the transmitter's comes when it connects).
    pub delay: DelayProfile,
    /// rx: written to the player when a low-delay transmitter connects (the low profile's player_ctl).
    pub low_ctl: Vec<String>,
}

/// Whether the sender's arguments take the send cap (a profile may fix it with a literal instead).
pub fn takes_send_cap(p: &Profile) -> bool {
    p.sender_args.iter().any(|a| a.contains("{maxbw_kbps}"))
}

#[derive(Debug, Clone)]
pub struct Spec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Env,
    pub cwd: PathBuf,
}

/// File names inside a session directory (the sampler reads them).
pub const FAST: &str = "fast.jsonl";
pub const TICKS: &str = "ticks.jsonl";
pub const EVENTS: &str = "events.jsonl";
pub const RCV: &str = "rcv.jsonl";

fn path_str(p: PathBuf) -> String {
    p.to_string_lossy().into_owned()
}

fn subst(list: &[String], vars: &[(&str, String)]) -> Vec<String> {
    list.iter()
        .map(|a| vars.iter().fold(a.clone(), |s, (k, v)| s.replace(&format!("{{{k}}}"), v)))
        .collect()
}

fn bridge_env(p: &Profile, plan: &Plan) -> Env {
    let mut e = p.bridge_env.clone();
    match &plan.input {
        Input::Camera(dev) => {
            e.insert("SOURCE_KIND".into(), "v4l2".into());
            e.insert("CAM_DEVICE".into(), dev.clone());
        }
        Input::File(f) => {
            e.insert("SOURCE_KIND".into(), "y4m".into());
            e.insert("SOURCE_Y4M".into(), f.clone());
        }
    }
    if plan.manual_kbps.is_some() || p.fixed_bitrate {
        e.extend(p.bridge_manual_env.clone());
    }
    if let Some(kbps) = plan.manual_kbps {
        e.insert("INITIAL_TARGET".into(), kbps.to_string());
    }
    e
}

/// Files the sender writes and the bridge reads (same paths for both).
fn tx_shared(p: &Profile, plan: &Plan, dir: &Path) -> Env {
    let mut e = Env::new();
    e.insert("SRT_FAST_STATS_JSONL".into(), path_str(dir.join(FAST)));
    e.insert("SRT_STATS_FILE".into(), path_str(dir.join("st.json")));
    e.insert("SRT_T0_FILE".into(), path_str(dir.join("t0.json")));
    if bridge_env(p, plan).get("BUF_ND_PACE").map(String::as_str) == Some("1") {
        e.insert("SRT_MAXBW_CTL".into(), path_str(dir.join("maxbw_ctl")));
    }
    e
}

/// The text the sender gives the receiver when it connects (its stream id): the receiver reads the
/// profile from it (supervisor.rs `stream_id_delay`) and follows it.
pub fn stream_id(p: &Profile) -> String {
    format!("delay={}", p.delay.name())
}

pub fn sender(cfg: &Config, p: &Profile, plan: &Plan, dir: &Path) -> Spec {
    let mut env = p.sender_env.clone();
    env.extend(tx_shared(p, plan, dir));
    // the daemon's, not the profile's: every session says which profile it uses
    env.insert("SRT_STREAMID".into(), stream_id(p));
    let vars = [
        ("peer_host", plan.peer_host.clone()),
        ("peer_port", plan.peer_port.to_string()),
        ("maxbw_kbps", plan.maxbw_kbps.to_string()),
    ];
    Spec { program: cfg.media.path(&cfg.media.sender), args: subst(&p.sender_args, &vars), env, cwd: dir.into() }
}

pub fn bridge(cfg: &Config, p: &Profile, plan: &Plan, dir: &Path) -> Spec {
    let mut env = bridge_env(p, plan);
    env.extend(tx_shared(p, plan, dir));
    // Only the tick and event logs: no BUFCTRL_RUN_DIR, so the other per-frame logs are not written.
    env.insert("BUFCTRL_TICKS_JSONL".into(), path_str(dir.join(TICKS)));
    env.insert("BRIDGE_EVENTS_JSONL".into(), path_str(dir.join(EVENTS)));
    // A copy of what is sent, for the preview (only while the preview program reads it).
    if cfg.preview.enabled {
        env.insert("PREVIEW_SOCK".into(), path_str(cfg.preview_sock()));
    }
    Spec {
        program: PathBuf::from(&cfg.media.python),
        args: vec!["-u".into(), path_str(cfg.media.path(&cfg.media.bridge))],
        env,
        cwd: cfg.media.bridge_cwd(),
    }
}

pub fn receiver(cfg: &Config, p: &Profile, plan: &Plan, dir: &Path) -> Spec {
    let mut env = p.receiver_env.clone();
    env.insert("SRT_RCV_STATS_JSONL".into(), path_str(dir.join(RCV)));
    // The player's input pipe is the receiver's fd 3 (media.rs).
    env.insert("SRT_RCV_PIPE".into(), "/dev/fd/3".into());
    let out = if cfg.archive_video && cfg.archive_dir.is_some() {
        path_str(dir.join("recv.mkv"))
    } else {
        "/dev/null".into()
    };
    let vars = [("out", out), ("listen_port", plan.listen_port.to_string())];
    Spec { program: cfg.media.path(&cfg.media.receiver), args: subst(&p.receiver_args, &vars), env, cwd: dir.into() }
}

pub fn player(cfg: &Config, p: &Profile, dir: &Path) -> Spec {
    let mut args = vec!["-u".into(), path_str(cfg.media.path(&cfg.media.player))];
    args.extend(p.player_args.iter().cloned());
    // The control input (always: the transmitter's profile is known only once it connects, and a
    // low-delay one is followed by writing the low profile's player_ctl there; media.rs).
    if ctl_fd(&args).is_none() {
        let fd = if stats_fd(&args) == Some(PLAYER_CTL_FD) { PLAYER_CTL_FD + 1 } else { PLAYER_CTL_FD };
        args.push("--ctl-fd".into());
        args.push(fd.to_string());
    }
    // A copy of what is shown, for the preview (always while enabled: a viewer can come at any time).
    if cfg.preview.enabled {
        args.push("--preview-sock".into());
        args.push(path_str(cfg.preview_sock()));
    }
    Spec { program: PathBuf::from(&cfg.media.python), args, env: Env::new(), cwd: dir.into() }
}

/// The preview program (one per device, started on demand; preview.rs). It listens on the
/// socket the bridge or the player copies the video to.
pub fn preview(cfg: &Config) -> Spec {
    let p = &cfg.preview;
    let mut args = vec!["-u".into(), path_str(cfg.media.path(&p.program))];
    let opts = [
        ("--sock", path_str(cfg.preview_sock())),
        ("--width", p.width.to_string()),
        ("--height", p.height.to_string()),
        ("--h265-kbps", p.h265_kbps.to_string()),
        ("--h264-kbps", p.h264_kbps.to_string()),
        ("--keyint", p.keyint.to_string()),
        ("--delay-ms", p.delay_ms(cfg.role).to_string()),
        ("--max-viewers", p.max_viewers.to_string()),
        ("--idle-exit-s", p.idle_exit_s.to_string()),
    ];
    for (k, v) in opts {
        args.push(k.into());
        args.push(v);
    }
    Spec { program: PathBuf::from(&cfg.media.python), args, env: Env::new(), cwd: cfg.run_dir() }
}

/// The value of `--name V` or `--name=V` in a command line.
fn arg_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().enumerate().find_map(|(i, a)| match a.strip_prefix(name) {
        Some("") => args.get(i + 1).map(String::as_str),
        Some(v) => v.strip_prefix('='),
        None => None,
    })
}

/// The fd the player writes its per-second stats to (`--stats-fd N`), if any.
pub fn stats_fd(args: &[String]) -> Option<i32> {
    arg_value(args, "--stats-fd")?.parse().ok()
}

/// The fd the player reads its control lines from (the daemon adds `--ctl-fd PLAYER_CTL_FD`).
pub fn ctl_fd(args: &[String]) -> Option<i32> {
    arg_value(args, "--ctl-fd")?.parse().ok()
}

/// The player's control input, unless its arguments name another one (or the stats use it).
pub const PLAYER_CTL_FD: i32 = 5;

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::parse(
            "id = \"TX-01\"\nrole = \"tx\"\nrun_dir = \"/run/ctrld\"\n[tx]\npeer_host = \"10.0.0.2\"\n[media]\nrepo = \"/opt/media\"\n",
        )
        .unwrap()
    }

    fn plan() -> Plan {
        Plan {
            peer_host: "10.0.0.2".into(),
            peer_port: 60021,
            listen_port: 60021,
            input: Input::Camera("/dev/video0".into()),
            manual_kbps: None,
            maxbw_kbps: 5000,
            delay: DelayProfile::Standard,
            low_ctl: Vec::new(),
        }
    }

    #[test]
    fn overrides_merge() {
        let pc: ProfileConfig = toml::from_str(
            "[bridge_env]\nSVT_PRESET = 10\nUSE_NATIVE_LOSS = true\nSTREAM_ALIGN = \"\"\n[sender_env]\nSRT_OHEADBW = 50\n",
        )
        .unwrap();
        let p = Profile::resolve(&pc).unwrap();
        assert_eq!(p.bridge_env["SVT_PRESET"], "10");
        assert_eq!(p.bridge_env["USE_NATIVE_LOSS"], "1");
        assert!(!p.bridge_env.contains_key("STREAM_ALIGN"));
        assert_eq!(p.sender_env["SRT_OHEADBW"], "50");
        assert_eq!(p.initial_mbps(), 3.5);
        assert_eq!(p.keyframe_s(), 2.0);
    }

    #[test]
    fn tx_commands() {
        let (c, p, dir) = (cfg(), Profile::standard(), Path::new("/run/s1"));
        let s = sender(&c, &p, &plan(), dir);
        assert_eq!(s.program, PathBuf::from("/opt/media/sender/scripts/srt_proper_sender"));
        assert_eq!(s.args, ["-", "10.0.0.2", "60021", "8000", "live", "10000", "5000"]);
        assert!(takes_send_cap(&p));
        assert_eq!(s.env["SRT_FAST_STATS_JSONL"], "/run/s1/fast.jsonl");
        assert_eq!(s.env["SRT_MAXBW_CTL"], "/run/s1/maxbw_ctl");
        assert_eq!(s.env["SRT_CLEAN_SIGTERM"], "1");

        let b = bridge(&c, &p, &plan(), dir);
        assert_eq!(b.args[0], "-u");
        assert_eq!(b.cwd, PathBuf::from("/opt/media/sender"));
        assert_eq!(b.env["CAM_DEVICE"], "/dev/video0");
        assert_eq!(b.env["BUFCTRL_TICKS_JSONL"], "/run/s1/ticks.jsonl");
        assert_eq!(b.env["SRT_MAXBW_CTL"], s.env["SRT_MAXBW_CTL"]);
        assert!(!b.env.contains_key("BUFCTRL_RUN_DIR"));
        assert_eq!(b.env["PREVIEW_SOCK"], "/run/ctrld/preview.sock");
        assert!(!s.env.contains_key("PREVIEW_SOCK"), "only the bridge copies the video");

        let mut fixed = plan();
        fixed.input = Input::File("/srv/a.y4m".into());
        fixed.manual_kbps = Some(2500);
        let b = bridge(&c, &p, &fixed, dir);
        assert_eq!(b.env["SOURCE_KIND"], "y4m");
        assert_eq!(b.env["SOURCE_Y4M"], "/srv/a.y4m");
        assert_eq!(b.env["INITIAL_TARGET"], "2500");
        assert_eq!(b.env["BUF_NODROP"], "0");
        assert!(!sender(&c, &p, &fixed, dir).env.contains_key("SRT_MAXBW_CTL"));
    }

    #[test]
    fn send_cap_in_the_sender_arguments() {
        let (c, dir) = (cfg(), Path::new("/run/s1"));
        let mut measured = plan();
        measured.maxbw_kbps = 5260;
        let p = Profile::standard();
        assert_eq!(sender(&c, &p, &measured, dir).args, ["-", "10.0.0.2", "60021", "8000", "live", "10000", "5260"]);
        // a device.toml that names the arguments itself: a literal keeps working, the placeholder anywhere
        let lit: ProfileConfig =
            toml::from_str("sender_args = [\"-\", \"{peer_host}\", \"{peer_port}\", \"8000\", \"live\", \"10000\", \"5000\"]\n").unwrap();
        let p = Profile::resolve(&lit).unwrap();
        assert!(!takes_send_cap(&p));
        assert_eq!(sender(&c, &p, &measured, dir).args, ["-", "10.0.0.2", "60021", "8000", "live", "10000", "5000"]);
        let own: ProfileConfig =
            toml::from_str("sender_args = [\"-\", \"{peer_host}\", \"{peer_port}\", \"6000\", \"live\", \"{maxbw_kbps}\"]\n").unwrap();
        let p = Profile::resolve(&own).unwrap();
        assert_eq!(sender(&c, &p, &measured, dir).args, ["-", "10.0.0.2", "60021", "6000", "live", "5260"]);
        // the receiver's arguments have no such placeholder
        assert_eq!(receiver(&c, &Profile::standard(), &measured, dir).args, ["/dev/null", "60021", "8000"]);
    }

    #[test]
    fn rx_commands() {
        let (c, p, dir) = (cfg(), Profile::standard(), Path::new("/run/s2"));
        let r = receiver(&c, &p, &plan(), dir);
        assert_eq!(r.args, ["/dev/null", "60021", "8000"]);
        assert_eq!(r.env["SRT_RCV_PIPE"], "/dev/fd/3");
        assert_eq!(r.env["SRT_RCV_STATS_JSONL"], "/run/s2/rcv.jsonl");
        let pl = player(&c, &p, dir);
        assert_eq!(pl.args[1], "/opt/media/receiver/scripts/ll_player.py");
        assert_eq!(stats_fd(&pl.args), Some(4));
        assert_eq!(stats_fd(&["--stats-fd=5".to_string()]), Some(5));
        assert_eq!(stats_fd(&["--out".to_string()]), None);
        assert_eq!(pl.args[pl.args.len() - 2..], ["--preview-sock", "/run/ctrld/preview.sock"]);
    }

    #[test]
    fn preview_command() {
        let c = cfg();
        let s = preview(&c);
        assert_eq!(s.program, PathBuf::from("python3"));
        assert_eq!(s.cwd, PathBuf::from("/run/ctrld"));
        let want = [
            "-u",
            "/opt/media/preview/preview_webrtc.py",
            "--sock",
            "/run/ctrld/preview.sock",
            "--width",
            "0",
            "--height",
            "0",
            "--h265-kbps",
            "16000",
            "--h264-kbps",
            "24000",
            "--keyint",
            "30",
            "--delay-ms",
            "800",
            "--max-viewers",
            "3",
            "--idle-exit-s",
            "30",
        ];
        assert_eq!(s.args, want);
        let mut rx = c.clone();
        rx.role = crate::config::Role::Rx;
        assert!(preview(&rx).args.windows(2).any(|w| w == ["--delay-ms", "300"]));
    }

    #[test]
    fn no_preview_no_copy() {
        let mut c = cfg();
        c.preview.enabled = false;
        let (p, dir) = (Profile::standard(), Path::new("/run/s3"));
        assert!(!bridge(&c, &p, &plan(), dir).env.contains_key("PREVIEW_SOCK"));
        let pl = player(&c, &p, dir);
        assert!(!pl.args.iter().any(|a| a == "--preview-sock"));
        // the old command line, then the control input
        assert_eq!(pl.args[pl.args.len() - 4..], ["--stats-fd", "4", "--ctl-fd", "5"]);
    }

    fn with(text: &str) -> Config {
        Config::parse(&format!(
            "id = \"TX-01\"\nrole = \"tx\"\nrun_dir = \"/run/ctrld\"\n[tx]\npeer_host = \"10.0.0.2\"\n[media]\nrepo = \"/opt/media\"\n{text}"
        ))
        .unwrap()
    }

    #[test]
    fn low_profile_is_layered_on_the_resolved_standard_one() {
        let p = Profiles::resolve(&cfg()).unwrap();
        let (s, l) = (&p.standard, &p.low);
        assert!(p.low_enabled, "offered by default");
        assert_eq!((s.delay, l.delay), (DelayProfile::Standard, DelayProfile::Low));
        assert_eq!(l.sender_env["SRT_SEND_PARTIAL"], "1");
        assert!(!s.sender_env.contains_key("SRT_SEND_PARTIAL"), "only in the low-delay profile");
        assert_eq!((l.bridge_env["SVT_PRESET"].as_str(), s.bridge_env["SVT_PRESET"].as_str()), ("10", "8"));
        assert_eq!(l.bridge_env["SVT_PARAMS"], "hierarchical-levels=3:lookahead=0");
        assert!(!s.bridge_env.contains_key("SVT_PARAMS"));
        for k in ["SRT_NODROP", "STREAM_ALIGN", "CAM_PTS"] {
            assert_eq!(l.bridge_env[k], s.bridge_env[k], "{k}");
        }
        assert_eq!(l.sender_env["SRT_NODROP"], "1");
        assert_eq!((l.sender_args.clone(), l.receiver_args.clone(), l.player_args.clone()), (s.sender_args.clone(), s.receiver_args.clone(), s.player_args.clone()));
        assert!(takes_send_cap(l));
        assert_eq!(l.player_ctl, ["ts-offset-ms 400", "adapt on"]);
        assert!(s.player_ctl.is_empty());
        assert_eq!((l.delay_target_ms, l.margin_warn_s, l.fixed_bitrate), (1500, None, true));
        assert_eq!((s.delay_target_ms, s.margin_warn_s, s.fixed_bitrate), (9500, Some(3.0), false));
        assert_eq!((p.in_effect(DelayProfile::Low), p.get(DelayProfile::Low).delay), (DelayProfile::Low, DelayProfile::Low));

        // [profile] reaches both, the low-delay changes come after it, [profile_low] last
        let p = Profiles::resolve(&with(
            "[profile]\ndelay_target_ms = 9000\n[profile.bridge_env]\nSVT_PRESET = 9\nHYSTER_UP_SEC = 30\n\
             [profile_low]\nenabled = false\ndelay_target_ms = 1800\ndelay_margin_warn_s = 0.5\n\
             player_ctl = [\"ts-offset-ms 600\", \" \", \"adapt off\"]\n\
             [profile_low.bridge_env]\nGOP = 30\nSTREAM_ALIGN = \"\"\n[profile_low.sender_env]\nSRT_SEND_PARTIAL = false\n",
        ))
        .unwrap();
        let (s, l) = (&p.standard, &p.low);
        assert_eq!((s.bridge_env["SVT_PRESET"].as_str(), l.bridge_env["SVT_PRESET"].as_str()), ("9", "10"));
        assert_eq!((s.bridge_env["HYSTER_UP_SEC"].as_str(), l.bridge_env["HYSTER_UP_SEC"].as_str()), ("30", "30"));
        assert_eq!((s.bridge_env["GOP"].as_str(), l.bridge_env["GOP"].as_str()), ("60", "30"));
        assert!(!l.bridge_env.contains_key("STREAM_ALIGN") && s.bridge_env.contains_key("STREAM_ALIGN"));
        assert_eq!(l.sender_env["SRT_SEND_PARTIAL"], "0");
        assert_eq!((s.delay_target_ms, l.delay_target_ms, l.margin_warn_s), (9000, 1800, Some(0.5)));
        assert_eq!(l.player_ctl, ["ts-offset-ms 600", "adapt off"], "trimmed, empty lines dropped");
        assert!(!p.low_enabled);
        assert_eq!(p.in_effect(DelayProfile::Low), DelayProfile::Standard, "turned off: a stored low is standard");

        // what does not load
        let err = Profiles::resolve(&with("[profile_low]\nplayer_ctl = [\"adapt on\\nadapt off\"]\n")).unwrap_err();
        assert!(err.contains("profile_low.player_ctl"), "{err}");
        let err = Profiles::resolve(&with("[profile_low.bridge_env]\nGOP = [1]\n")).unwrap_err();
        assert!(err.starts_with("profile_low:"), "{err}");
        assert!(Config::parse("id = \"T\"\nrole = \"rx\"\n[media]\nrepo = \"/m\"\n[profile_low]\nbogus = 1\n").is_err());
    }

    #[test]
    fn low_delay_session_commands() {
        let (c, dir) = (cfg(), Path::new("/run/s4"));
        let p = Profiles::resolve(&c).unwrap();
        let mut low = plan();
        low.delay = DelayProfile::Low;
        low.manual_kbps = Some(3000);
        let s = sender(&c, &p.low, &low, dir);
        assert_eq!(s.args, ["-", "10.0.0.2", "60021", "8000", "live", "10000", "5000"], "the send cap as in the standard profile");
        assert_eq!((s.env["SRT_SEND_PARTIAL"].as_str(), s.env["SRT_STREAMID"].as_str()), ("1", "delay=low"));
        assert_eq!(s.env["SRT_NODROP"], "1");
        assert!(!s.env.contains_key("SRT_MAXBW_CTL"), "no never-drop pacing with the fixed bitrate");
        let b = bridge(&c, &p.low, &low, dir);
        assert_eq!((b.env["SVT_PRESET"].as_str(), b.env["SVT_PARAMS"].as_str()), ("10", "hierarchical-levels=3:lookahead=0"));
        assert_eq!((b.env["INITIAL_TARGET"].as_str(), b.env["BUF_NODROP"].as_str(), b.env["BUF_ND_PACE"].as_str()), ("3000", "0", "0"));
        assert_eq!((b.env["DISABLE_UPSHIFT"].as_str(), b.env["SIGNIFICANT_DELTA_KBPS"].as_str()), ("1", "1000000"));
        assert_eq!((b.env["STREAM_ALIGN"].as_str(), b.env["CAM_PTS"].as_str(), b.env["SRT_NODROP"].as_str()), ("tu", "v4l2", "1"));
        // the fixed-bitrate environment even without a bitrate in the plan
        low.manual_kbps = None;
        let b = bridge(&c, &p.low, &low, dir);
        assert_eq!((b.env["BUF_NODROP"].as_str(), b.env["INITIAL_TARGET"].as_str()), ("0", "3500"));

        // the standard profile says so too, and sends no small messages at once
        let s = sender(&c, &p.standard, &plan(), dir);
        assert_eq!(s.env["SRT_STREAMID"], "delay=standard");
        assert!(!s.env.contains_key("SRT_SEND_PARTIAL"));
        assert_eq!(s.env["SRT_MAXBW_CTL"], "/run/s4/maxbw_ctl");
        let b = bridge(&c, &p.standard, &plan(), dir);
        assert_eq!((b.env["SVT_PRESET"].as_str(), b.env["BUF_NODROP"].as_str()), ("8", "1"));
        assert!(!b.env.contains_key("SVT_PARAMS"));
        // a stream id in the profile does not win over the daemon's
        let own = with("[profile_low.sender_env]\nSRT_STREAMID = \"x\"\n");
        let p = Profiles::resolve(&own).unwrap();
        assert_eq!(sender(&own, &p.low, &plan(), dir).env["SRT_STREAMID"], "delay=low");
    }

    #[test]
    fn the_player_control_input() {
        let (c, dir) = (cfg(), Path::new("/run/s5"));
        let p = Profiles::resolve(&c).unwrap();
        let pl = player(&c, &p.standard, dir);
        assert_eq!((stats_fd(&pl.args), ctl_fd(&pl.args)), (Some(4), Some(5)));
        assert_eq!(ctl_fd(&["--ctl-fd=7".to_string()]), Some(7));
        // the stats on 5: the next one; one named in the arguments is kept
        let args = |a: &[&str]| -> Profile { Profile { player_args: a.iter().map(|s| s.to_string()).collect(), ..Profile::standard() } };
        let pl = player(&c, &args(&["--stats-fd", "5"]), dir);
        assert_eq!((stats_fd(&pl.args), ctl_fd(&pl.args)), (Some(5), Some(6)));
        let pl = player(&c, &args(&["--stats-fd", "4", "--ctl-fd", "9"]), dir);
        assert_eq!(ctl_fd(&pl.args), Some(9));
        assert_eq!(pl.args.iter().filter(|a| *a == "--ctl-fd").count(), 1);

        // the display offset when the transmitter has just connected
        let start = &p.standard.player_args;
        assert_eq!(p.standard.initial_offset_ms(start), Some(7000.0));
        assert_eq!(p.low.initial_offset_ms(start), Some(400.0));
        let no_offset = Profile { player_ctl: vec!["adapt on".into()], ..p.low.clone() };
        assert_eq!(no_offset.initial_offset_ms(start), Some(7000.0), "the player keeps its start offset");
        assert_eq!(p.standard.initial_offset_ms(&["--ts-offset-ms=650".to_string()]), Some(650.0));
        assert_eq!(p.standard.initial_offset_ms(&[]), None);
    }
}
