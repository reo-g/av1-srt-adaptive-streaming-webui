//! Serde mirror of the contract (api/openapi.yaml, web/src/api/types.ts) for the objects the
//! daemon builds often, plus the stored settings. Rarely built objects (options, compare)
//! are written with json! where they are served.

use crate::config::Role;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub type Params = Map<String, Value>;

/// The object members of a json!({...}) value.
pub fn params(v: Value) -> Params {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

pub const TIMEZONE: &str = "Asia/Tokyo";
/// The only input format the transmitter takes (the bridge's caps are fixed).
pub const SIGNAL_FORMAT: &str = "3840×2160\u{3000}30p";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkState {
    Live,
    Connecting,
    Reconnecting,
    Listening,
    Stopped,
}

#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub id: String,
    pub role: Role,
    pub address: String,
    pub firmware: String,
    pub serial: String,
    pub timezone: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub level: &'static str,
    pub code: &'static str,
    pub since: String,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub params: Params,
}

#[derive(Debug, Clone, Serialize)]
pub struct Peer {
    pub id: Option<String>,
    pub address: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct Media {
    pub kind: &'static str,
    pub label: String,
    pub format: Option<String>,
    pub preview_fps: f64,
    pub audio: &'static str,
    pub black: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub server_time: String,
    pub state: LinkState,
    pub since: String,
    pub live_s: Option<u64>,
    pub attempt: Option<u32>,
    pub next_retry_s: Option<u64>,
    pub last_live_until: Option<String>,
    pub peer: Peer,
    pub listen_port: Option<u16>,
    pub alerts: Vec<Alert>,
    pub media: Media,
}

/// Fields that do not apply to the role, or cannot be measured now, are null.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub at: String,
    pub delay_profile: &'static str,
    pub delay_target_ms: u32,
    pub loss_pct: Option<f64>,
    pub loss_threshold_pct: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub last_rtt_ms: Option<f64>,
    pub retrans_pct: Option<f64>,
    pub send_mbps: Option<f64>,
    pub target_mbps: Option<f64>,
    pub actual_mbps: Option<f64>,
    pub bandwidth_mbps: Option<f64>,
    pub send_late_5m: Option<i64>,
    pub delay_margin_s: Option<f64>,
    pub delay_margin_warn_s: Option<f64>,
    pub recv_mbps: Option<f64>,
    pub stalls_5m: Option<i64>,
    pub stall_s_5m: Option<f64>,
    pub video_latency_ms: Option<f64>,
    pub unrecovered_5m: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceEvent {
    pub id: String,
    pub at: String,
    pub level: &'static str,
    pub code: &'static str,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub params: Params,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldError {
    pub field: &'static str,
    pub code: &'static str,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub params: Params,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Impact {
    pub level: &'static str,
    pub code: &'static str,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub params: Params,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub ok: bool,
    pub errors: Vec<FieldError>,
    pub impacts: Vec<Impact>,
}

// ------------------------------------------------------------------ stored settings

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputKind {
    Camera,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Auto,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DelayProfile {
    Standard,
    Low,
}

impl DelayProfile {
    pub fn name(self) -> &'static str {
        match self {
            DelayProfile::Standard => "standard",
            DelayProfile::Low => "low",
        }
    }
}

/// Serialized as the contract's VideoInput.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoInput {
    pub kind: InputKind,
    pub camera: String,
    pub file: String,
    #[serde(rename = "loop")]
    pub loop_: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoStored {
    pub version: u64,
    pub input: VideoInput,
    pub mode: Mode,
    pub manual_mbps: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransportStored {
    pub version: u64,
    pub mode: String,
    pub peer_host: String,
    pub peer_port: u16,
    pub listen_port: u16,
    pub delay: DelayProfile,
    pub cipher: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub video: VideoStored,
    pub transport: TransportStored,
}
