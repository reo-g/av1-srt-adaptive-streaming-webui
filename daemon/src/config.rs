//! device.toml: who this device is, where the media programs are, and the profile
//! (the environment and arguments each child gets; see profile.rs for the defaults).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Tx,
    Rx,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub id: String,
    pub role: Role,
    #[serde(default)]
    pub serial: String,
    #[serde(default)]
    pub http: Http,
    /// Working files of each session (default $XDG_RUNTIME_DIR/ctrld, a tmpfs).
    pub run_dir: Option<PathBuf>,
    /// Applied settings (default $XDG_STATE_HOME/ctrld or ~/.local/state/ctrld).
    pub state_dir: Option<PathBuf>,
    /// When set, every session's files are written here and kept, for offline analysis.
    pub archive_dir: Option<PathBuf>,
    /// rx with archive_dir: also keep the received stream (about 1.8 GB per hour).
    #[serde(default)]
    pub archive_video: bool,
    pub media: Media,
    #[serde(default)]
    pub tx: Tx,
    #[serde(default)]
    pub rx: Rx,
    #[serde(default)]
    pub profile: ProfileConfig,
    #[serde(default)]
    pub preview: Preview,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Http {
    pub bind: String,
    pub port: u16,
}

impl Default for Http {
    fn default() -> Self {
        Http { bind: "0.0.0.0".into(), port: 8080 }
    }
}

/// The media programs. Relative paths are taken from `repo`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Media {
    /// A checkout of the system repo (or a copy with the same layout).
    pub repo: PathBuf,
    #[serde(default = "d_python")]
    pub python: String,
    #[serde(default = "d_sender")]
    pub sender: PathBuf,
    #[serde(default = "d_bridge")]
    pub bridge: PathBuf,
    #[serde(default = "d_receiver")]
    pub receiver: PathBuf,
    #[serde(default = "d_player")]
    pub player: PathBuf,
    /// Working directory of the bridge (default <repo>/sender: it opens its curve file by a relative path).
    pub bridge_cwd: Option<PathBuf>,
    /// More variables the children inherit from the daemon's environment, besides the built-in
    /// list in media.rs (a trailing '*' is a prefix). Everything else is not passed on.
    #[serde(default)]
    pub pass_env: Vec<String>,
}

fn d_python() -> String {
    "python3".into()
}
fn d_sender() -> PathBuf {
    "sender/scripts/srt_proper_sender".into()
}
fn d_bridge() -> PathBuf {
    "sender/scripts/encoder_restart_bridge.py".into()
}
fn d_receiver() -> PathBuf {
    "receiver/srt_proper_receiver".into()
}
fn d_player() -> PathBuf {
    "receiver/scripts/ll_player.py".into()
}

impl Media {
    pub fn path(&self, p: &Path) -> PathBuf {
        self.repo.join(p)
    }
    pub fn bridge_cwd(&self) -> PathBuf {
        self.bridge_cwd.clone().unwrap_or_else(|| self.repo.join("sender"))
    }
}

/// Transmitter: the camera and the initial peer (the settings screen changes the peer later).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Tx {
    pub camera: String,
    pub peer_host: String,
    pub peer_port: u16,
    /// Folder with the video files that can be chosen as input (4K30 y4m).
    pub media_dir: Option<PathBuf>,
}

impl Default for Tx {
    fn default() -> Self {
        Tx { camera: "/dev/video0".into(), peer_host: String::new(), peer_port: 60021, media_dir: None }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Rx {
    pub listen_port: u16,
}

impl Default for Rx {
    fn default() -> Self {
        Rx { listen_port: 60021 }
    }
}

/// The browser preview (preview.rs): a program started on demand that shows the browser the
/// picture being sent (tx) or shown (rx). Relative `program` paths are taken from `media.repo`;
/// it runs with `media.python`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Preview {
    pub enabled: bool,
    pub program: PathBuf,
    /// Scale the preview to this size (0 = the size of the video).
    pub width: u32,
    pub height: u32,
    /// Bitrates of the two forms the preview is offered in (the browser picks one).
    pub h265_kbps: u32,
    pub h264_kbps: u32,
    /// Frames between keyframes of the preview.
    pub keyint: u32,
    pub max_viewers: u32,
    /// Smoothing delay before frames are shown (default 800 ms tx, 300 ms rx).
    pub delay_ms: Option<u32>,
    /// The program exits this long after the last viewer left.
    pub idle_exit_s: u32,
    /// How long an offer may wait for the program's answer.
    pub answer_timeout_s: u32,
}

impl Default for Preview {
    fn default() -> Self {
        Preview {
            enabled: true,
            program: "preview/preview_webrtc.py".into(),
            width: 0,
            height: 0,
            h265_kbps: 16000,
            h264_kbps: 24000,
            keyint: 30,
            max_viewers: 3,
            delay_ms: None,
            idle_exit_s: 30,
            answer_timeout_s: 10,
        }
    }
}

impl Preview {
    pub fn delay_ms(&self, role: Role) -> u32 {
        self.delay_ms.unwrap_or(match role {
            Role::Tx => 800,
            Role::Rx => 300,
        })
    }
}

/// Changes to the standard profile. In the env maps a value replaces the default and an
/// empty string removes the variable; numbers and booleans are written as text (true = 1).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProfileConfig {
    pub sender_env: BTreeMap<String, toml::Value>,
    pub bridge_env: BTreeMap<String, toml::Value>,
    /// Added to the bridge's environment when the video mode is "fixed bitrate".
    pub bridge_manual_env: BTreeMap<String, toml::Value>,
    pub receiver_env: BTreeMap<String, toml::Value>,
    /// Placeholders: {peer_host} {peer_port}
    pub sender_args: Option<Vec<String>>,
    /// Placeholders: {out} {listen_port}
    pub receiver_args: Option<Vec<String>>,
    pub player_args: Option<Vec<String>>,
    pub delay_target_ms: Option<u32>,
    pub delay_margin_warn_s: Option<f64>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        let mut c: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        if c.id.trim().is_empty() {
            return Err("id must not be empty".into());
        }
        if c.role == Role::Tx && c.tx.peer_host.trim().is_empty() {
            return Err("tx.peer_host is required when role = \"tx\"".into());
        }
        if c.preview.enabled && (c.preview.max_viewers == 0 || c.preview.answer_timeout_s == 0) {
            return Err("preview.max_viewers and preview.answer_timeout_s must be at least 1".into());
        }
        c.media.repo = expand(&c.media.repo);
        c.media.bridge_cwd = c.media.bridge_cwd.as_deref().map(expand);
        c.run_dir = c.run_dir.as_deref().map(expand);
        c.state_dir = c.state_dir.as_deref().map(expand);
        c.archive_dir = c.archive_dir.as_deref().map(expand);
        c.tx.media_dir = c.tx.media_dir.as_deref().map(expand);
        c.preview.program = expand(&c.preview.program);
        Ok(c)
    }

    pub fn run_dir(&self) -> PathBuf {
        self.run_dir.clone().unwrap_or_else(|| match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(d) => PathBuf::from(d).join("ctrld"),
            None => home().join(".local/state/ctrld/run"),
        })
    }

    pub fn state_dir(&self) -> PathBuf {
        self.state_dir.clone().unwrap_or_else(|| match std::env::var_os("XDG_STATE_HOME") {
            Some(d) => PathBuf::from(d).join("ctrld"),
            None => home().join(".local/state/ctrld"),
        })
    }

    /// The socket the media programs copy the video to for the preview (one per device).
    pub fn preview_sock(&self) -> PathBuf {
        self.run_dir().join("preview.sock")
    }

    /// Where session directories are created: the archive if there is one, else the run dir.
    pub fn sessions_dir(&self) -> PathBuf {
        self.archive_dir.clone().unwrap_or_else(|| self.run_dir())
    }
}

pub fn default_config_path() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) => PathBuf::from(d).join("ctrld/device.toml"),
        None => home().join(".config/ctrld/device.toml"),
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// "~/x" → "$HOME/x".
fn expand(p: &Path) -> PathBuf {
    match p.strip_prefix("~") {
        Ok(rest) => home().join(rest),
        Err(_) => p.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_parses() {
        let c = Config::parse(include_str!("../device.example.toml")).unwrap();
        assert_eq!(c.role, Role::Tx);
        assert_eq!(c.http.port, 8080);
        assert_eq!(c.tx.peer_port, 60021);
        assert!(c.media.repo.is_absolute());
        assert!(c.preview.enabled);
        assert_eq!(c.preview.program, PathBuf::from("preview/preview_webrtc.py"));
        assert_eq!((c.preview.h265_kbps, c.preview.h264_kbps, c.preview.max_viewers), (16000, 24000, 3));
        assert_eq!(c.preview.delay_ms(c.role), 800);
    }

    #[test]
    fn preview_section() {
        let base = "id = \"RX-02\"\nrole = \"rx\"\nrun_dir = \"/run/c\"\n[media]\nrepo = \"/opt/m\"\n";
        let c = Config::parse(base).unwrap();
        assert!(c.preview.enabled, "on by default");
        assert_eq!(c.preview.delay_ms(Role::Rx), 300);
        assert_eq!(c.preview.delay_ms(Role::Tx), 800);
        assert_eq!(c.preview_sock(), PathBuf::from("/run/c/preview.sock"));
        let c = Config::parse(&format!("{base}[preview]\ndelay_ms = 500\nmax_viewers = 1\nwidth = 1920\nheight = 1080\n")).unwrap();
        assert_eq!((c.preview.delay_ms(Role::Rx), c.preview.max_viewers, c.preview.width), (500, 1, 1920));
        assert!(Config::parse(&format!("{base}[preview]\nmax_viewers = 0\n")).is_err());
        assert!(Config::parse(&format!("{base}[preview]\nenabled = false\nmax_viewers = 0\n")).is_ok());
        assert!(Config::parse(&format!("{base}[preview]\nfps = 5\n")).is_err());
    }

    #[test]
    fn rx_minimal_and_errors() {
        let c = Config::parse("id = \"RX-02\"\nrole = \"rx\"\n[media]\nrepo = \"/opt/m\"\n").unwrap();
        assert_eq!(c.rx.listen_port, 60021);
        assert_eq!(c.http.bind, "0.0.0.0");
        assert_eq!(c.media.bridge_cwd(), PathBuf::from("/opt/m/sender"));
        assert!(Config::parse("id = \"TX\"\nrole = \"tx\"\n[media]\nrepo = \"/m\"\n").unwrap_err().contains("peer_host"));
        assert!(Config::parse("id = \"RX\"\nrole = \"rx\"\nbogus = 1\n[media]\nrepo = \"/m\"\n").is_err());
    }
}
