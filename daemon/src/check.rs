//! Check rules for settings drafts: a port of checkVideo / checkTransport / checkNetwork /
//! checkOutput in web/mock/server.ts (the oracle), plus a check that the chosen input is one the
//! device offers, because the device really opens it. Request bodies are read leniently: a
//! missing, null or mistyped value becomes a FieldError, never a 422.

use crate::config::Hop;
use crate::model::{
    params, CheckResult, DelayProfile, DisplayMode, FieldError, Impact, InputKind, LinkState, Mode, NetworkStored,
    OutputStored, SendLimit, TransportStored, VideoInput, VideoStored, SEND_LIMIT_RANGE,
};
use crate::util::round;
use serde_json::{json, Value};
use std::net::Ipv4Addr;

pub const BITRATE_RANGE: (f64, f64) = (1.5, 4.0);
pub const MODES: [&str; 1] = ["outbound"];
pub const CIPHERS: [&str; 1] = ["none"];
const PASSPHRASE_LEN: (usize, usize) = (10, 79);

/// The value at a dotted path; None when missing or null.
fn at<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for k in path.split('.') {
        cur = cur.get(k)?;
    }
    (!cur.is_null()).then_some(cur)
}
fn text(v: &Value, path: &str) -> Option<String> {
    at(v, path)?.as_str().map(str::to_string)
}
fn num(v: &Value, path: &str) -> Option<f64> {
    at(v, path)?.as_f64()
}
fn flag(v: &Value, path: &str) -> Option<bool> {
    at(v, path)?.as_bool()
}

fn fe(field: impl Into<String>, code: &'static str, p: Value) -> FieldError {
    FieldError { field: field.into(), code, params: params(p) }
}
fn im(level: &'static str, code: &'static str, p: Value) -> Impact {
    Impact { level, code, params: params(p) }
}

/// Seconds the receiver's picture is gone while the link is set up again: about the profile's delay.
pub fn gap_s(target_ms: u32) -> i64 {
    (f64::from(target_ms) / 1000.0).round() as i64
}

// ------------------------------------------------------------------ video

#[derive(Debug, Clone, Default)]
pub struct VideoDraft {
    pub version: Option<f64>,
    pub kind: Option<String>,
    pub camera: Option<String>,
    pub file: Option<String>,
    pub loop_: Option<bool>,
    pub mode: Option<String>,
    pub bitrate: Option<f64>,
}

impl VideoDraft {
    pub fn parse(v: &Value) -> Self {
        VideoDraft {
            version: num(v, "version"),
            kind: text(v, "input.kind"),
            camera: text(v, "input.camera"),
            file: text(v, "input.file"),
            loop_: flag(v, "input.loop"),
            mode: text(v, "encode.mode"),
            bitrate: num(v, "encode.manual.bitrate_mbps"),
        }
    }
    fn kind(&self) -> Option<InputKind> {
        match self.kind.as_deref() {
            Some("camera") => Some(InputKind::Camera),
            Some("file") => Some(InputKind::File),
            _ => None,
        }
    }
    fn mode(&self) -> Option<Mode> {
        match self.mode.as_deref() {
            Some("auto") => Some(Mode::Auto),
            Some("manual") => Some(Mode::Manual),
            _ => None,
        }
    }
}

pub struct VideoCtx<'a> {
    pub applied: &'a VideoStored,
    pub state: LinkState,
    /// The bitrate in effect now (automatic target, or the fixed value).
    pub cur_mbps: f64,
    pub bandwidth_mbps: Option<f64>,
    pub loop_available: bool,
    pub gap_s: i64,
    pub cameras: &'a [String],
    pub files: &'a [String],
    /// The applied delay is low: the fixed bitrate (applied.manual_mbps) is sent whatever the mode
    /// says, and automatic mode cannot be in effect (VideoOptions.auto_available false).
    pub low_delay: bool,
}

pub fn check_video(d: &VideoDraft, c: &VideoCtx) -> CheckResult {
    let (min, max) = BITRATE_RANGE;
    let a = c.applied;
    let mut errors = Vec::new();
    let mut impacts = Vec::new();
    let manual = d.mode() == Some(Mode::Manual);
    let kind = d.kind();
    if d.mode().is_none() {
        errors.push(fe("encode.mode", "invalid", json!({})));
    }
    if kind.is_none() {
        errors.push(fe("input.kind", "invalid", json!({})));
    }
    let br_valid = d.bitrate.is_some_and(|b| (min..=max).contains(&b));
    if manual && !br_valid {
        errors.push(fe("encode.manual.bitrate_mbps", "out_of_range", json!({ "min": min, "max": max })));
    }
    let file = kind == Some(InputKind::File);
    if file && d.loop_ == Some(true) && !c.loop_available {
        errors.push(fe("input.loop", "invalid", json!({})));
    }
    // The device opens the input: it must be one it offers now, or the one applied (never empty:
    // an empty applied value means none was chosen yet).
    let offered = |v: &Option<String>, list: &[String], applied: &str| {
        v.as_deref().is_some_and(|x| !x.is_empty() && (x == applied || list.iter().any(|o| o == x)))
    };
    match kind {
        Some(InputKind::Camera) if !offered(&d.camera, c.cameras, &a.input.camera) => {
            errors.push(fe("input.camera", "invalid", json!({})))
        }
        Some(InputKind::File) if !offered(&d.file, c.files, &a.input.file) => {
            errors.push(fe("input.file", "invalid", json!({})))
        }
        _ => {}
    }

    let src_changed = kind != Some(a.input.kind)
        || if kind == Some(InputKind::Camera) {
            d.camera.as_deref() != Some(a.input.camera.as_str())
        } else {
            d.file.as_deref() != Some(a.input.file.as_str())
        };
    let mode_changed = d.mode() != Some(a.mode);
    let br_changed = manual && d.bitrate != Some(c.cur_mbps);
    let loop_changed = file && d.loop_.unwrap_or(false) != a.input.loop_;
    let any = src_changed || mode_changed || br_changed || loop_changed;
    // While the delay is low the fixed bitrate is sent in either mode: a new mode alone is stored
    // but changes nothing in the transmission.
    let sent_mode_changed = mode_changed && !c.low_delay;

    match c.state {
        LinkState::Live if src_changed => {
            // A new input restarts the chain: the link is set up again (the UI shows the pair as one line).
            impacts.push(im("warn", "input_switch_gap", json!({ "gap_s": c.gap_s })));
            impacts.push(im("warn", "reconnect_gap", json!({ "gap_s": c.gap_s })));
        }
        LinkState::Live if sent_mode_changed || br_changed => impacts.push(im("ok", "switch_no_gap", json!({}))),
        LinkState::Stopped if any => impacts.push(im("info", "saved_while_stopped", json!({}))),
        _ => {}
    }
    if mode_changed && manual {
        impacts.push(im("info", "manual_fixed", json!({})));
    }
    if sent_mode_changed && !manual {
        impacts.push(im("info", "auto_decides", json!({})));
    }
    if c.low_delay && d.mode() == Some(Mode::Auto) {
        // not an error: the mode is stored and comes into effect when the delay is standard again
        impacts.push(im("info", "low_delay_fixed", json!({ "bitrate_mbps": a.manual_mbps })));
    }
    let br = d.bitrate.unwrap_or(0.0);
    if br_changed && br_valid && br < c.cur_mbps {
        impacts.push(im("info", "bitrate_lower_quality", json!({})));
    }
    if let Some(bw) = c.bandwidth_mbps {
        if manual && (br_changed || mode_changed) && br_valid && br > bw * 0.8 {
            impacts.push(im("warn", "bitrate_near_bandwidth", json!({ "bandwidth_mbps": bw })));
        }
    }
    CheckResult { ok: errors.is_empty(), errors, impacts }
}

/// The settings a valid draft gives (call only when check_video found no errors).
pub fn apply_video(d: &VideoDraft, a: &VideoStored) -> VideoStored {
    let mode = d.mode().unwrap_or(a.mode);
    VideoStored {
        version: a.version + 1,
        input: VideoInput {
            kind: d.kind().unwrap_or(a.input.kind),
            camera: d.camera.clone().unwrap_or_else(|| a.input.camera.clone()),
            file: d.file.clone().unwrap_or_else(|| a.input.file.clone()),
            loop_: d.loop_.unwrap_or(false),
        },
        mode,
        manual_mbps: match (mode, d.bitrate) {
            (Mode::Manual, Some(b)) => b,
            _ => a.manual_mbps,
        },
    }
}

// ------------------------------------------------------------------ transport

/// The draft's send_limit: left out (or null) keeps the applied value.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum LimitDraft {
    #[default]
    Keep,
    Mbps(f64),
    /// send_limit given without a number in mbps
    Bad,
}

#[derive(Debug, Clone, Default)]
pub struct TransportDraft {
    pub version: Option<f64>,
    pub mode: Option<String>,
    pub host: Option<String>,
    pub port: Option<f64>,
    pub listen_port: Option<f64>,
    pub profile: Option<String>,
    pub cipher: Option<String>,
    pub passphrase: Option<String>,
    pub send_limit: LimitDraft,
}

impl TransportDraft {
    pub fn parse(v: &Value) -> Self {
        TransportDraft {
            version: num(v, "version"),
            mode: text(v, "mode"),
            host: text(v, "peer.host"),
            port: num(v, "peer.port"),
            listen_port: num(v, "listen_port"),
            profile: text(v, "delay.profile"),
            cipher: text(v, "encryption.cipher"),
            passphrase: text(v, "encryption.passphrase"),
            // left out keeps the applied value; anything else must be a number (null too, as the mock)
            send_limit: match v.get("send_limit") {
                None => LimitDraft::Keep,
                Some(_) => num(v, "send_limit.mbps").map_or(LimitDraft::Bad, LimitDraft::Mbps),
            },
        }
    }

    /// The valid new send cap (two decimals), if the draft gives one.
    fn limit(&self) -> Option<f64> {
        let (lo, hi) = SEND_LIMIT_RANGE;
        match self.send_limit {
            LimitDraft::Mbps(x) if (lo..=hi).contains(&x) => Some(round(x, 2)),
            _ => None,
        }
    }

    /// A valid send cap that differs from the applied one.
    fn limit_changed(&self, a: &TransportStored) -> Option<f64> {
        self.limit().filter(|x| *x != a.send_limit.mbps)
    }

    /// Anything but the send cap differs from the applied settings.
    fn others_changed(&self, a: &TransportStored) -> bool {
        self.mode.as_deref() != Some(a.mode.as_str())
            || self.host.as_deref() != Some(a.peer_host.as_str())
            || self.port != Some(f64::from(a.peer_port))
            || self.listen_port != Some(f64::from(a.listen_port))
            || self.profile.as_deref() != Some(a.delay.name())
            || self.cipher.as_deref() != Some(a.cipher.as_str())
            || (self.passphrase.is_some() && self.cipher.as_deref() != Some("none"))
    }

    /// A valid new send cap and nothing else changed.
    fn just_the_limit(&self, a: &TransportStored) -> bool {
        self.limit_changed(a).is_some() && !self.others_changed(a)
    }

    /// The draft changes the send cap and nothing else (valid or not): that may be applied while
    /// the link runs (an invalid value is then refused by the check), the running transmission
    /// keeps its cap.
    pub fn only_send_limit(&self, a: &TransportStored) -> bool {
        let differs = match self.send_limit {
            LimitDraft::Keep => false,
            LimitDraft::Mbps(x) => round(x, 2) != a.send_limit.mbps,
            LimitDraft::Bad => true,
        };
        differs && !self.others_changed(a)
    }
}

pub struct TransportCtx<'a> {
    pub applied: &'a TransportStored,
    pub state: LinkState,
    /// (profile, target_ms, available)
    pub profiles: &'a [(DelayProfile, u32, bool)],
    /// The applied video mode is automatic.
    pub video_auto: bool,
    /// The fixed bitrate of the video settings (encode.manual.bitrate_mbps): what low delay sends.
    pub fixed_mbps: f64,
}

fn port_ok(p: Option<f64>) -> bool {
    p.is_some_and(|p| p.fract() == 0.0 && (1.0..=65535.0).contains(&p))
}

/// Dotted IPv4 without leading zeros (the sender takes only a numeric address).
fn ipv4_ok(s: &str) -> bool {
    s.parse::<std::net::Ipv4Addr>().is_ok()
}

pub fn check_transport(d: &TransportDraft, c: &TransportCtx) -> CheckResult {
    let mut errors = Vec::new();
    let mut impacts = Vec::new();
    if d.send_limit != LimitDraft::Keep && d.limit().is_none() {
        let (lo, hi) = SEND_LIMIT_RANGE;
        errors.push(fe("send_limit.mbps", "out_of_range", json!({ "min": lo, "max": hi })));
    }
    let mode = d.mode.as_deref();
    if !mode.is_some_and(|m| MODES.contains(&m)) {
        errors.push(fe("mode", "invalid", json!({})));
    }
    if mode != Some("inbound") {
        if !d.host.as_deref().is_some_and(ipv4_ok) {
            errors.push(fe("peer.host", "invalid", json!({})));
        }
        if !port_ok(d.port) {
            errors.push(fe("peer.port", "out_of_range", json!({ "min": 1, "max": 65535 })));
        }
    }
    if mode != Some("outbound") && !port_ok(d.listen_port) {
        errors.push(fe("listen_port", "out_of_range", json!({ "min": 1, "max": 65535 })));
    }
    let cipher = d.cipher.as_deref();
    if !cipher.is_some_and(|x| CIPHERS.contains(&x)) {
        errors.push(fe("encryption.cipher", "invalid", json!({})));
    }
    if let Some(pw) = &d.passphrase {
        let (lo, hi) = PASSPHRASE_LEN;
        let n = pw.chars().count();
        if cipher != Some("none") && (n < lo || n > hi) {
            let code = if n < lo { "too_short" } else { "too_long" };
            errors.push(fe("encryption.passphrase", code, json!({ "min": lo, "max": hi })));
        }
    }
    let dp = c.profiles.iter().find(|p| Some(p.0.name()) == d.profile.as_deref());
    if !dp.is_some_and(|p| p.2) {
        errors.push(fe("delay.profile", "invalid", json!({})));
    }

    match c.state {
        LinkState::Stopped => impacts.push(im("info", "saved_while_stopped", json!({}))),
        // Only the send cap: nothing is set up again. It is read at the start of a transmission
        // (app.rs plan; retries and reconnects keep it): stored now, used from the next start,
        // in every state but stopped (live, connecting, reconnecting), as the mock.
        _ if d.just_the_limit(c.applied) => impacts.push(im("info", "takes_effect_next_start", json!({}))),
        LinkState::Live => {
            // After the reconnect the picture comes back after about the delay of the new profile.
            let target = match dp {
                Some(p) if p.2 => p.1,
                _ => c.profiles.iter().find(|p| p.0 == c.applied.delay).map_or(9500, |p| p.1),
            };
            impacts.push(im("warn", "reconnect_gap", json!({ "gap_s": gap_s(target) })));
        }
        _ => {
            impacts.push(im("info", "reconnect", json!({})));
            impacts.push(im("ok", "no_peer_effect", json!({})));
        }
    }
    // To low delay: automatic mode is not used while it is applied (the stored video mode is kept).
    if dp.is_some_and(|p| p.0 == DelayProfile::Low && p.2) && c.applied.delay != DelayProfile::Low {
        if c.video_auto {
            impacts.push(im("info", "low_delay_fixed", json!({ "bitrate_mbps": c.fixed_mbps })));
        }
        impacts.push(im("info", "low_delay_effects", json!({})));
    }
    CheckResult { ok: errors.is_empty(), errors, impacts }
}

/// The settings a valid draft gives (call only when check_transport found no errors).
pub fn apply_transport(d: &TransportDraft, a: &TransportStored) -> TransportStored {
    let port = |p: Option<f64>, old: u16| if port_ok(p) { p.unwrap_or(0.0) as u16 } else { old };
    TransportStored {
        version: a.version + 1,
        mode: d.mode.clone().unwrap_or_else(|| a.mode.clone()),
        peer_host: d.host.clone().unwrap_or_else(|| a.peer_host.clone()),
        peer_port: port(d.port, a.peer_port),
        listen_port: port(d.listen_port, a.listen_port),
        delay: d.profile.as_deref().and_then(DelayProfile::parse).unwrap_or(a.delay),
        cipher: d.cipher.clone().unwrap_or_else(|| a.cipher.clone()),
        // a new value is the operator's; the same value keeps where it came from
        send_limit: d.limit_changed(a).map_or_else(|| a.send_limit.clone(), SendLimit::manual),
    }
}

// ------------------------------------------------------------------ network (途中の無線機)

pub const HOPS_MAX: usize = 4;
/// Characters (code points) of a label after trimming.
pub const HOP_LABEL_MAX: usize = 24;

/// The address of a device on the way: a dotted-quad IPv4 address without leading zeros (no host
/// names: the radio line's filter needs addresses), not 0.0.0.0, 255.255.255.255 or 127.x.x.x.
pub fn hop_address(s: &str) -> Option<Ipv4Addr> {
    s.parse::<Ipv4Addr>().ok().filter(|a| !a.is_unspecified() && !a.is_broadcast() && !a.is_loopback())
}

/// A stored list that keeps the rules that do not depend on the moment (a settings.json edited by
/// hand may not): at most 4, trimmed labels of 1–24 characters, usable addresses, no duplicates.
pub fn hops_sane(hops: &[Hop]) -> bool {
    hops.len() <= HOPS_MAX
        && hops.iter().enumerate().all(|(i, h)| {
            let n = h.label.chars().count();
            h.label.trim() == h.label
                && (1..=HOP_LABEL_MAX).contains(&n)
                && hop_address(&h.address).is_some()
                && !hops[..i].iter().any(|e| e.address == h.address)
        })
}

#[derive(Debug, Clone, Default)]
pub struct NetworkDraft {
    pub version: Option<f64>,
    /// The list as it would be stored (labels trimmed; a missing or mistyped label or address is
    /// empty); None when the body has no list.
    pub hops: Option<Vec<Hop>>,
}

impl NetworkDraft {
    pub fn parse(v: &Value) -> Self {
        let hop = |h: &Value| Hop {
            label: text(h, "label").map(|l| l.trim().to_string()).unwrap_or_default(),
            address: text(h, "address").unwrap_or_default(),
        };
        NetworkDraft { version: num(v, "version"), hops: at(v, "hops").and_then(Value::as_array).map(|a| a.iter().map(hop).collect()) }
    }
}

pub struct NetworkCtx<'a> {
    pub applied: &'a NetworkStored,
    /// This device's own addresses (every interface).
    pub own: &'a [Ipv4Addr],
    /// The peer's address as known now (tx: the transport settings' host and what it resolved to;
    /// rx: the configured or the last connected transmitter); host names are ignored.
    pub peers: &'a [String],
}

/// One code per field; an address gets the first that applies: invalid → is_self → is_peer →
/// duplicate (of an earlier item). Allowed in every link state: nothing is set up again.
pub fn check_network(d: &NetworkDraft, c: &NetworkCtx) -> CheckResult {
    let mut errors = Vec::new();
    let mut impacts = Vec::new();
    let Some(hops) = &d.hops else {
        errors.push(fe("hops", "required", json!({})));
        return CheckResult { ok: false, errors, impacts };
    };
    if hops.len() > HOPS_MAX {
        errors.push(fe("hops", "too_many", json!({ "max": HOPS_MAX })));
    }
    let peers: Vec<Ipv4Addr> = c.peers.iter().filter_map(|p| p.parse().ok()).collect();
    for (i, h) in hops.iter().enumerate() {
        let n = h.label.chars().count();
        if n == 0 {
            errors.push(fe(format!("hops[{i}].label"), "required", json!({})));
        } else if n > HOP_LABEL_MAX {
            errors.push(fe(format!("hops[{i}].label"), "too_long", json!({ "max": HOP_LABEL_MAX })));
        }
        let code = match hop_address(&h.address) {
            None => Some("invalid"),
            Some(a) if c.own.contains(&a) => Some("is_self"),
            Some(a) if peers.contains(&a) => Some("is_peer"),
            Some(_) if hops[..i].iter().any(|e| e.address == h.address) => Some("duplicate"),
            Some(_) => None,
        };
        if let Some(code) = code {
            errors.push(fe(format!("hops[{i}].address"), code, json!({})));
        }
    }
    if *hops != c.applied.hops {
        impacts.push(im("info", "hops_changed", json!({})));
    }
    CheckResult { ok: errors.is_empty(), errors, impacts }
}

/// The settings a valid draft gives (call only when check_network found no errors).
pub fn apply_network(d: &NetworkDraft, a: &NetworkStored) -> NetworkStored {
    NetworkStored { version: a.version + 1, hops: d.hops.clone().unwrap_or_else(|| a.hops.clone()) }
}

// ------------------------------------------------------------------ output (受信機のモニター, rx)

#[derive(Debug, Clone, Default)]
pub struct OutputDraft {
    pub version: Option<f64>,
    /// None when missing, null, not text or not one of the modes.
    pub display: Option<DisplayMode>,
}

impl OutputDraft {
    pub fn parse(v: &Value) -> Self {
        OutputDraft { version: num(v, "version"), display: text(v, "display").as_deref().and_then(DisplayMode::parse) }
    }
}

/// One field, `display: invalid` when it is not one of the modes. Allowed in every link state: the
/// player follows at once. Impacts only for a change: display_on (to full screen or a window) or
/// display_off, then display_fullscreen_covers when the new value is full screen.
pub fn check_output(d: &OutputDraft, applied: &OutputStored) -> CheckResult {
    match d.display {
        Some(to) => CheckResult { ok: true, errors: Vec::new(), impacts: display_impacts(applied.display, to) },
        None => CheckResult { ok: false, errors: vec![fe("display", "invalid", json!({}))], impacts: Vec::new() },
    }
}

fn display_impacts(from: DisplayMode, to: DisplayMode) -> Vec<Impact> {
    let mut impacts = Vec::new();
    if from == to {
        return impacts;
    }
    impacts.push(im("info", if to.is_on() { "display_on" } else { "display_off" }, json!({})));
    if to == DisplayMode::Fullscreen {
        impacts.push(im("info", "display_fullscreen_covers", json!({})));
    }
    impacts
}

/// The body of POST /display (`{mode}`, the operation screen's button): the mode, or the field
/// error (`mode: invalid`) when it is not one of them.
pub fn display_request(v: &Value) -> Result<DisplayMode, Vec<FieldError>> {
    text(v, "mode").as_deref().and_then(DisplayMode::parse).ok_or_else(|| vec![fe("mode", "invalid", json!({}))])
}

/// The settings a new value gives: the version goes up also when it is the same (it is written
/// to the player again, which is a retry by hand).
pub fn apply_output(to: DisplayMode, a: &OutputStored) -> OutputStored {
    OutputStored { version: a.version + 1, display: to }
}

// ------------------------------------------------------------------ tests (the mock is the oracle)

#[cfg(test)]
mod tests {
    use super::*;
    use LinkState::*;

    fn video_applied(mode: Mode) -> VideoStored {
        VideoStored {
            version: 1,
            input: VideoInput {
                kind: InputKind::Camera,
                camera: "/dev/video0".into(),
                file: "/srv/media/a.y4m".into(),
                loop_: false,
            },
            mode,
            manual_mbps: 3.0,
        }
    }

    fn draft(kind: &str, camera: &str, file: &str, mode: &str, br: Option<f64>) -> Value {
        json!({ "version": 1, "input": { "kind": kind, "camera": camera, "file": file, "loop": false },
                "encode": { "mode": mode, "manual": { "bitrate_mbps": br } } })
    }

    fn codes(r: &CheckResult) -> (Vec<String>, Vec<String>) {
        (
            r.errors.iter().map(|e| format!("{}:{}", e.field, e.code)).collect(),
            r.impacts.iter().map(|i| format!("{}:{}", i.level, i.code)).collect(),
        )
    }

    /// (case, applied mode, state, cur target, bandwidth, draft, errors, impacts)
    #[test]
    fn video_rules() {
        let cams = vec!["/dev/video0".to_string()];
        let files = vec!["/srv/media/a.y4m".to_string(), "/srv/media/b.y4m".to_string()];
        let cam = |mode: &str, br: Option<f64>| draft("camera", "/dev/video0", "/srv/media/a.y4m", mode, br);
        let cases: Vec<(&str, Mode, LinkState, f64, Option<f64>, Value, &[&str], &[&str])> = vec![
            ("nothing changed, live", Mode::Auto, Live, 3.5, None, cam("auto", Some(3.0)), &[], &[]),
            ("nothing changed, stopped", Mode::Auto, Stopped, 3.5, None, cam("auto", Some(3.0)), &[], &[]),
            (
                "auto -> manual 2.5, live, lower",
                Mode::Auto,
                Live,
                3.5,
                None,
                cam("manual", Some(2.5)),
                &[],
                &["ok:switch_no_gap", "info:manual_fixed", "info:bitrate_lower_quality"],
            ),
            (
                "auto -> manual 3.5 near the capacity",
                Mode::Auto,
                Live,
                3.5,
                Some(4.0),
                cam("manual", Some(3.5)),
                &[],
                &["ok:switch_no_gap", "info:manual_fixed", "warn:bitrate_near_bandwidth"],
            ),
            (
                "manual -> auto, stopped",
                Mode::Manual,
                Stopped,
                3.0,
                None,
                cam("auto", Some(3.0)),
                &[],
                &["info:saved_while_stopped", "info:auto_decides"],
            ),
            (
                "manual bitrate out of range",
                Mode::Manual,
                Live,
                3.0,
                None,
                cam("manual", Some(4.5)),
                &["encode.manual.bitrate_mbps:out_of_range"],
                &["ok:switch_no_gap"],
            ),
            (
                "manual bitrate null",
                Mode::Manual,
                Stopped,
                3.0,
                None,
                cam("manual", None),
                &["encode.manual.bitrate_mbps:out_of_range"],
                &["info:saved_while_stopped"],
            ),
            (
                "auto ignores the bitrate field",
                Mode::Auto,
                Live,
                3.5,
                None,
                cam("auto", None),
                &[],
                &[],
            ),
            (
                "new input while live",
                Mode::Auto,
                Live,
                3.5,
                None,
                draft("file", "/dev/video0", "/srv/media/b.y4m", "auto", Some(3.0)),
                &[],
                &["warn:input_switch_gap", "warn:reconnect_gap"],
            ),
            (
                "new input while reconnecting",
                Mode::Auto,
                Reconnecting,
                3.5,
                None,
                draft("file", "/dev/video0", "/srv/media/b.y4m", "auto", Some(3.0)),
                &[],
                &[],
            ),
            (
                "unknown file",
                Mode::Auto,
                Stopped,
                3.5,
                None,
                draft("file", "/dev/video0", "/etc/passwd", "auto", Some(3.0)),
                &["input.file:invalid"],
                &["info:saved_while_stopped"],
            ),
            (
                "bad kind and mode",
                Mode::Auto,
                Stopped,
                3.5,
                None,
                draft("hdmi", "/dev/video0", "", "fast", Some(3.0)),
                &["encode.mode:invalid", "input.kind:invalid"],
                &["info:saved_while_stopped", "info:auto_decides"],
            ),
        ];
        for (name, mode, state, cur, bw, body, errs, imps) in cases {
            let applied = video_applied(mode);
            let ctx = VideoCtx {
                applied: &applied,
                state,
                cur_mbps: cur,
                bandwidth_mbps: bw,
                loop_available: false,
                gap_s: 10,
                cameras: &cams,
                files: &files,
                low_delay: false,
            };
            let r = check_video(&VideoDraft::parse(&body), &ctx);
            let (e, i) = codes(&r);
            assert_eq!(e, errs, "{name}: errors");
            assert_eq!(i, imps, "{name}: impacts");
            assert_eq!(r.ok, errs.is_empty(), "{name}: ok");
        }
    }

    #[test]
    fn video_input_must_be_chosen() {
        let mut applied = video_applied(Mode::Auto);
        applied.input.file = String::new();
        let files = vec!["/srv/media/a.y4m".to_string()];
        let cams = vec!["/dev/video0".to_string()];
        let ctx = VideoCtx {
            applied: &applied,
            state: Stopped,
            cur_mbps: 3.5,
            bandwidth_mbps: None,
            loop_available: false,
            gap_s: 10,
            cameras: &cams,
            files: &files,
            low_delay: false,
        };
        let check = |body: Value| codes(&check_video(&VideoDraft::parse(&body), &ctx)).0;
        // empty equals the applied (empty) file, still not a choice
        assert_eq!(check(draft("file", "/dev/video0", "", "auto", None)), ["input.file:invalid"]);
        let mut missing = draft("file", "/dev/video0", "", "auto", None);
        missing["input"].as_object_mut().unwrap().remove("file");
        assert_eq!(check(missing), ["input.file:invalid"]);
        assert_eq!(check(draft("file", "/dev/video0", "/srv/media/a.y4m", "auto", None)), Vec::<String>::new());
        let mut no_cam = applied.clone();
        no_cam.input.camera = String::new();
        let ctx = VideoCtx { applied: &no_cam, cameras: &[], ..ctx };
        let r = check_video(&VideoDraft::parse(&draft("camera", "", "", "auto", None)), &ctx);
        assert_eq!(codes(&r).0, ["input.camera:invalid"]);
    }

    #[test]
    fn video_params_and_apply() {
        let applied = video_applied(Mode::Auto);
        let ctx = VideoCtx {
            applied: &applied,
            state: Live,
            cur_mbps: 3.5,
            bandwidth_mbps: Some(2.8),
            loop_available: false,
            gap_s: 10,
            cameras: &[],
            files: &[],
            low_delay: false,
        };
        let body = json!({ "version": 1, "input": { "kind": "file", "camera": "/dev/video0", "file": "/srv/media/a.y4m", "loop": true },
                           "encode": { "mode": "manual", "manual": { "bitrate_mbps": 2.5 } } });
        let r = check_video(&VideoDraft::parse(&body), &ctx);
        assert_eq!(r.errors[0].field, "input.loop");
        let near = r.impacts.iter().find(|i| i.code == "bitrate_near_bandwidth").unwrap();
        assert_eq!(near.params["bandwidth_mbps"], 2.8);
        let gap = r.impacts.iter().find(|i| i.code == "input_switch_gap").unwrap();
        assert_eq!(gap.params["gap_s"], 10);

        let d = VideoDraft::parse(&draft("camera", "/dev/video0", "/srv/media/a.y4m", "manual", Some(2.0)));
        let next = apply_video(&d, &applied);
        assert_eq!((next.version, next.mode, next.manual_mbps), (2, Mode::Manual, 2.0));
        let back = apply_video(&VideoDraft::parse(&draft("camera", "/dev/video0", "", "auto", None)), &next);
        assert_eq!((back.mode, back.manual_mbps), (Mode::Auto, 2.0));
    }

    /// (case, applied mode, state, draft, impacts): the applied delay is low, the fixed bitrate 3.0.
    #[test]
    fn video_rules_while_low_delay_is_applied() {
        let cams = vec!["/dev/video0".to_string()];
        let cam = |mode: &str, br: Option<f64>| draft("camera", "/dev/video0", "/srv/media/a.y4m", mode, br);
        let cases: Vec<(&str, Mode, LinkState, Value, &[&str])> = vec![
            ("auto stored, nothing changed, live", Mode::Auto, Live, cam("auto", Some(3.0)), &["info:low_delay_fixed"]),
            ("auto stored, nothing changed, stopped", Mode::Auto, Stopped, cam("auto", None), &["info:low_delay_fixed"]),
            // the mode is stored, nothing changes in the transmission (no switch, no automatic steps)
            ("manual -> auto, live", Mode::Manual, Live, cam("auto", Some(3.0)), &["info:low_delay_fixed"]),
            ("manual -> auto, stopped", Mode::Manual, Stopped, cam("auto", Some(3.0)), &["info:saved_while_stopped", "info:low_delay_fixed"]),
            ("auto -> manual at the same value, live", Mode::Auto, Live, cam("manual", Some(3.0)), &["info:manual_fixed"]),
            (
                "auto -> manual lower, live",
                Mode::Auto,
                Live,
                cam("manual", Some(2.5)),
                &["ok:switch_no_gap", "info:manual_fixed", "info:bitrate_lower_quality"],
            ),
            ("manual, a new value, live", Mode::Manual, Live, cam("manual", Some(3.5)), &["ok:switch_no_gap"]),
            ("manual, nothing changed", Mode::Manual, Live, cam("manual", Some(3.0)), &[]),
        ];
        for (name, mode, state, body, imps) in cases {
            let applied = video_applied(mode);
            let ctx = VideoCtx {
                applied: &applied,
                state,
                cur_mbps: 3.0,
                bandwidth_mbps: None,
                loop_available: false,
                gap_s: 2,
                cameras: &cams,
                files: &[],
                low_delay: true,
            };
            let r = check_video(&VideoDraft::parse(&body), &ctx);
            let (e, i) = codes(&r);
            assert!(e.is_empty() && r.ok, "{name}: {e:?}");
            assert_eq!(i, imps, "{name}: impacts");
            if let Some(x) = r.impacts.iter().find(|x| x.code == "low_delay_fixed") {
                assert_eq!(serde_json::to_value(x).unwrap(), json!({ "level": "info", "code": "low_delay_fixed", "params": { "bitrate_mbps": 3.0 } }));
            }
        }
    }

    #[test]
    fn transport_rules_to_low_delay() {
        let profiles = [(DelayProfile::Standard, 9500, true), (DelayProfile::Low, 1500, true)];
        let low = || tdraft("outbound", "192.168.0.88", json!(60021), "low", "none");
        // (case, applied delay, video auto, state, draft, impacts)
        let cases: Vec<(&str, DelayProfile, bool, LinkState, Value, &[&str])> = vec![
            ("to low, auto, live", DelayProfile::Standard, true, Live, low(), &["warn:reconnect_gap", "info:low_delay_fixed", "info:low_delay_effects"]),
            ("to low, fixed, stopped", DelayProfile::Standard, false, Stopped, low(), &["info:saved_while_stopped", "info:low_delay_effects"]),
            (
                "to low, auto, reconnecting",
                DelayProfile::Standard,
                true,
                Reconnecting,
                low(),
                &["info:reconnect", "ok:no_peer_effect", "info:low_delay_fixed", "info:low_delay_effects"],
            ),
            ("low already, another peer, live", DelayProfile::Low, true, Live, tdraft("outbound", "192.168.0.89", json!(60021), "low", "none"), &["warn:reconnect_gap"]),
            ("back to standard, live", DelayProfile::Low, true, Live, tdraft("outbound", "192.168.0.88", json!(60021), "standard", "none"), &["warn:reconnect_gap"]),
        ];
        for (name, delay, auto, state, body, imps) in cases {
            let mut applied = transport_applied();
            applied.delay = delay;
            let ctx = TransportCtx { applied: &applied, state, profiles: &profiles, video_auto: auto, fixed_mbps: 2.5 };
            let r = check_transport(&TransportDraft::parse(&body), &ctx);
            let (e, i) = codes(&r);
            assert!(e.is_empty(), "{name}: {e:?}");
            assert_eq!(i, imps, "{name}: impacts");
        }
        // the gap is the new profile's delay; the fixed bitrate is the video settings'
        let applied = transport_applied();
        let ctx = TransportCtx { applied: &applied, state: Live, profiles: &profiles, video_auto: true, fixed_mbps: 2.5 };
        let r = check_transport(&TransportDraft::parse(&low()), &ctx);
        assert_eq!(
            serde_json::to_value(&r.impacts).unwrap(),
            json!([{ "level": "warn", "code": "reconnect_gap", "params": { "gap_s": 2 } },
                   { "level": "info", "code": "low_delay_fixed", "params": { "bitrate_mbps": 2.5 } },
                   { "level": "info", "code": "low_delay_effects" }])
        );
        let back = apply_transport(&TransportDraft::parse(&low()), &applied);
        assert_eq!(back.delay, DelayProfile::Low);
        // not offered (turned off in device.toml): refused, nothing about low delay
        let off = [(DelayProfile::Standard, 9500, true), (DelayProfile::Low, 1500, false)];
        let ctx = TransportCtx { profiles: &off, ..ctx };
        let r = check_transport(&TransportDraft::parse(&low()), &ctx);
        assert_eq!(codes(&r), (vec!["delay.profile:invalid".to_string()], vec!["warn:reconnect_gap".to_string()]));
    }

    fn transport_applied() -> TransportStored {
        TransportStored {
            version: 3,
            mode: "outbound".into(),
            peer_host: "192.168.0.88".into(),
            peer_port: 60021,
            listen_port: 60021,
            delay: DelayProfile::Standard,
            cipher: "none".into(),
            send_limit: SendLimit::default(),
        }
    }

    fn tdraft(mode: &str, host: &str, port: Value, profile: &str, cipher: &str) -> Value {
        json!({ "version": 3, "mode": mode, "peer": { "host": host, "port": port }, "listen_port": 9000,
                "delay": { "profile": profile }, "encryption": { "cipher": cipher } })
    }

    #[test]
    fn transport_rules() {
        let profiles = [(DelayProfile::Standard, 9500, true), (DelayProfile::Low, 1500, false)];
        let ok = || tdraft("outbound", "192.168.0.88", json!(60021), "standard", "none");
        let cases: Vec<(&str, LinkState, Value, &[&str], &[&str])> = vec![
            ("valid, live", Live, ok(), &[], &["warn:reconnect_gap"]),
            ("valid, stopped", Stopped, ok(), &[], &["info:saved_while_stopped"]),
            ("valid, reconnecting", Reconnecting, ok(), &[], &["info:reconnect", "ok:no_peer_effect"]),
            (
                "bad host and port",
                Stopped,
                tdraft("outbound", "192.168.0.300", json!(0), "standard", "none"),
                &["peer.host:invalid", "peer.port:out_of_range"],
                &["info:saved_while_stopped"],
            ),
            (
                "null port, leading zero",
                Stopped,
                tdraft("outbound", "10.0.0.01", Value::Null, "standard", "none"),
                &["peer.host:invalid", "peer.port:out_of_range"],
                &["info:saved_while_stopped"],
            ),
            ("port not an integer", Stopped, tdraft("outbound", "10.0.0.1", json!(60021.5), "standard", "none"), &["peer.port:out_of_range"], &["info:saved_while_stopped"]),
            (
                "mode, cipher and profile not offered",
                Live,
                tdraft("inbound", "", json!(1), "low", "aes-128"),
                // inbound would also need listen_port, which is valid here (9000)
                &["mode:invalid", "encryption.cipher:invalid", "delay.profile:invalid"],
                &["warn:reconnect_gap"],
            ),
        ];
        for (name, state, body, errs, imps) in cases {
            let applied = transport_applied();
            let ctx = TransportCtx { applied: &applied, state, profiles: &profiles, video_auto: true, fixed_mbps: 3.0 };
            let r = check_transport(&TransportDraft::parse(&body), &ctx);
            let (e, i) = codes(&r);
            assert_eq!(e, errs, "{name}: errors");
            assert_eq!(i, imps, "{name}: impacts");
        }
        // the gap follows the profile in effect when the requested one is not available
        let applied = transport_applied();
        let ctx = TransportCtx { applied: &applied, state: Live, profiles: &profiles, video_auto: true, fixed_mbps: 3.0 };
        let r = check_transport(&TransportDraft::parse(&tdraft("outbound", "10.0.0.1", json!(1), "low", "none")), &ctx);
        assert_eq!(r.impacts[0].params["gap_s"], 10);
    }

    #[test]
    fn transport_passphrase_and_apply() {
        let profiles = [(DelayProfile::Standard, 9500, true)];
        let applied = transport_applied();
        let ctx = TransportCtx { applied: &applied, state: Stopped, profiles: &profiles, video_auto: true, fixed_mbps: 3.0 };
        let mut body = tdraft("outbound", "10.0.0.5", json!(60030), "standard", "none");
        body["encryption"]["passphrase"] = json!("short");
        // ignored while the cipher is none
        assert!(check_transport(&TransportDraft::parse(&body), &ctx).ok);
        body["encryption"]["cipher"] = json!("aes-256");
        let r = check_transport(&TransportDraft::parse(&body), &ctx);
        assert_eq!(codes(&r).0, ["encryption.cipher:invalid", "encryption.passphrase:too_short"]);
        assert_eq!(r.errors[1].params["min"], 10);

        let d = TransportDraft::parse(&tdraft("outbound", "10.0.0.5", json!(60030), "standard", "none"));
        let next = apply_transport(&d, &applied);
        assert_eq!((next.version, next.peer_host.as_str(), next.peer_port, next.listen_port), (4, "10.0.0.5", 60030, 9000));
        assert_eq!(next.send_limit, applied.send_limit, "no send_limit in the draft: kept");
    }

    /// The applied settings as the screen sends them back, with a send_limit.
    fn same_as_applied(limit: Value) -> Value {
        let mut v = json!({ "version": 3, "mode": "outbound", "peer": { "host": "192.168.0.88", "port": 60021 },
                            "listen_port": 60021, "delay": { "profile": "standard" }, "encryption": { "cipher": "none" } });
        if !limit.is_null() {
            v["send_limit"] = limit;
        }
        v
    }

    #[test]
    fn send_limit_rules() {
        let profiles = [(DelayProfile::Standard, 9500, true)];
        let only = |mbps: Value| same_as_applied(json!({ "mbps": mbps }));
        let mut other = only(json!(5.26));
        other["peer"]["host"] = json!("192.168.0.89");
        let cases: Vec<(&str, LinkState, Value, &[&str], &[&str])> = vec![
            ("left out, live", Live, same_as_applied(Value::Null), &[], &["warn:reconnect_gap"]),
            ("the same value, live", Live, only(json!(5.0)), &[], &["warn:reconnect_gap"]),
            ("new value, stopped", Stopped, only(json!(5.26)), &[], &["info:saved_while_stopped"]),
            ("only the cap, live", Live, only(json!(5.26)), &[], &["info:takes_effect_next_start"]),
            ("only the cap, reconnecting", Reconnecting, only(json!(5.26)), &[], &["info:takes_effect_next_start"]),
            ("only the cap, connecting", Connecting, only(json!(1)), &[], &["info:takes_effect_next_start"]),
            ("the cap and the peer, live", Live, other, &[], &["warn:reconnect_gap"]),
            ("edges", Stopped, only(json!(20)), &[], &["info:saved_while_stopped"]),
            ("too low, live", Live, only(json!(0.99)), &["send_limit.mbps:out_of_range"], &["warn:reconnect_gap"]),
            ("too high, stopped", Stopped, only(json!(20.01)), &["send_limit.mbps:out_of_range"], &["info:saved_while_stopped"]),
            ("text", Live, only(json!("5.26")), &["send_limit.mbps:out_of_range"], &["warn:reconnect_gap"]),
            ("null mbps", Live, only(Value::Null), &["send_limit.mbps:out_of_range"], &["warn:reconnect_gap"]),
            ("no mbps", Live, same_as_applied(json!({})), &["send_limit.mbps:out_of_range"], &["warn:reconnect_gap"]),
            ("null", Stopped, same_as_applied(json!({ "x": 1 })), &["send_limit.mbps:out_of_range"], &["info:saved_while_stopped"]),
        ];
        for (name, state, body, errs, imps) in cases {
            let applied = transport_applied();
            let ctx = TransportCtx { applied: &applied, state, profiles: &profiles, video_auto: true, fixed_mbps: 3.0 };
            let r = check_transport(&TransportDraft::parse(&body), &ctx);
            let (e, i) = codes(&r);
            assert_eq!(e, errs, "{name}: errors");
            assert_eq!(i, imps, "{name}: impacts");
        }
        let applied = transport_applied();
        let ctx = TransportCtx { applied: &applied, state: Stopped, profiles: &profiles, video_auto: true, fixed_mbps: 3.0 };
        let r = check_transport(&TransportDraft::parse(&only(json!(25))), &ctx);
        assert_eq!(serde_json::to_value(&r.errors).unwrap()[0]["params"], json!({ "min": 1.0, "max": 20.0 }));
        // send_limit: null is not "left out"; the cap's error comes first (as the mock)
        let mut null = same_as_applied(Value::Null);
        null["send_limit"] = Value::Null;
        null["peer"]["host"] = json!("x");
        assert_eq!(codes(&check_transport(&TransportDraft::parse(&null), &ctx)).0, ["send_limit.mbps:out_of_range", "peer.host:invalid"]);

        // what may be applied while the link runs: a change of the cap alone (valid or not)
        let a = transport_applied();
        let d = |v: Value| TransportDraft::parse(&v);
        assert!(d(only(json!(5.26))).only_send_limit(&a));
        assert!(d(only(json!(30))).only_send_limit(&a), "refused by the check, not as busy");
        assert!(!d(only(json!(5.0))).only_send_limit(&a), "nothing changes");
        assert!(!d(only(json!(5.001))).only_send_limit(&a), "the same at two decimals");
        assert!(!d(same_as_applied(Value::Null)).only_send_limit(&a));
        let mut pw = only(json!(5.26));
        pw["encryption"] = json!({ "cipher": "aes-128", "passphrase": "0123456789" });
        assert!(!d(pw).only_send_limit(&a));
        let mut port = only(json!(5.26));
        port["listen_port"] = json!(9000);
        assert!(!d(port).only_send_limit(&a));
    }

    fn network_applied() -> NetworkStored {
        NetworkStored {
            version: 3,
            hops: vec![
                Hop { label: "無線機（送信機側）".into(), address: "192.168.0.65".into() },
                Hop { label: "無線機（受信機側）".into(), address: "192.168.0.56".into() },
            ],
        }
    }

    fn ndraft(hops: &[(&str, &str)]) -> Value {
        let hops: Vec<Value> = hops.iter().map(|(l, a)| json!({ "label": l, "address": a })).collect();
        json!({ "version": 3, "hops": hops })
    }

    /// (case, draft, errors, impacts); this device is 192.168.0.220, the peer 192.168.0.210.
    #[test]
    fn network_rules() {
        let applied = network_applied();
        let own = [Ipv4Addr::LOCALHOST, Ipv4Addr::new(192, 168, 0, 220), Ipv4Addr::new(100, 64, 1, 2)];
        let peers = ["192.168.0.210".to_string(), "rx-01.local".to_string()];
        let ctx = NetworkCtx { applied: &applied, own: &own, peers: &peers };
        let tx = "無線機（送信機側）";
        let rx = "無線機（受信機側）";
        let long = "長".repeat(25);
        let edge = "長".repeat(24);
        let five: Vec<(&str, &str)> = vec![("a", "10.0.0.1"), ("b", "10.0.0.2"), ("c", "10.0.0.3"), ("d", "10.0.0.4"), ("e", "10.0.0.5")];
        let cases: Vec<(&str, Value, &[&str], &[&str])> = vec![
            ("the applied list", ndraft(&[(tx, "192.168.0.65"), (rx, "192.168.0.56")]), &[], &[]),
            ("labels are trimmed", ndraft(&[(" 無線機（送信機側）\u{3000}", "192.168.0.65"), (rx, "192.168.0.56")]), &[], &[]),
            ("a new address", ndraft(&[(tx, "192.168.0.66"), (rx, "192.168.0.56")]), &[], &["info:hops_changed"]),
            ("another order", ndraft(&[(rx, "192.168.0.56"), (tx, "192.168.0.65")]), &[], &["info:hops_changed"]),
            ("a new label", ndraft(&[("MS", "192.168.0.65"), (rx, "192.168.0.56")]), &[], &["info:hops_changed"]),
            ("none", ndraft(&[]), &[], &["info:hops_changed"]),
            ("four, 24 characters", ndraft(&[(&edge, "10.0.0.1"), ("b", "10.0.0.2"), ("c", "10.0.0.3"), ("d", "10.0.0.4")]), &[], &["info:hops_changed"]),
            ("five", ndraft(&five), &["hops:too_many"], &["info:hops_changed"]),
            (
                "labels",
                ndraft(&[("", "10.0.0.1"), ("  ", "10.0.0.2"), (&long, "10.0.0.3")]),
                &["hops[0].label:required", "hops[1].label:required", "hops[2].label:too_long"],
                &["info:hops_changed"],
            ),
            (
                "not addresses",
                ndraft(&[("a", "radio.local"), ("b", "192.168.0.300"), ("c", "10.0.0.01"), ("d", " 10.0.0.1"), ("e", ""), ("f", "fe80::1")]),
                &[
                    "hops:too_many",
                    "hops[0].address:invalid",
                    "hops[1].address:invalid",
                    "hops[2].address:invalid",
                    "hops[3].address:invalid",
                    "hops[4].address:invalid",
                    "hops[5].address:invalid",
                ],
                &["info:hops_changed"],
            ),
            (
                "not a device",
                ndraft(&[("a", "0.0.0.0"), ("b", "255.255.255.255"), ("c", "127.0.0.1"), ("d", "127.1.2.3")]),
                &["hops[0].address:invalid", "hops[1].address:invalid", "hops[2].address:invalid", "hops[3].address:invalid"],
                &["info:hops_changed"],
            ),
            (
                "this device, the peer",
                ndraft(&[("a", "192.168.0.220"), ("b", "100.64.1.2"), ("c", "192.168.0.210")]),
                &["hops[0].address:is_self", "hops[1].address:is_self", "hops[2].address:is_peer"],
                &["info:hops_changed"],
            ),
            (
                "twice: the later one",
                ndraft(&[(tx, "192.168.0.65"), (rx, "192.168.0.65"), ("c", "192.168.0.210"), ("d", "192.168.0.210")]),
                &["hops[1].address:duplicate", "hops[2].address:is_peer", "hops[3].address:is_peer"],
                &["info:hops_changed"],
            ),
            (
                "a label and an address",
                ndraft(&[("", "x")]),
                &["hops[0].label:required", "hops[0].address:invalid"],
                &["info:hops_changed"],
            ),
            ("no list", json!({ "version": 3 }), &["hops:required"], &[]),
            ("null", json!({ "version": 3, "hops": null }), &["hops:required"], &[]),
            ("not a list", json!({ "version": 3, "hops": { "label": "a", "address": "10.0.0.1" } }), &["hops:required"], &[]),
            (
                "mistyped items",
                json!({ "version": 3, "hops": [5, { "label": 1, "address": 10 }, { "label": "a" }] }),
                &[
                    "hops[0].label:required",
                    "hops[0].address:invalid",
                    "hops[1].label:required",
                    "hops[1].address:invalid",
                    "hops[2].address:invalid",
                ],
                &["info:hops_changed"],
            ),
        ];
        for (name, body, errs, imps) in cases {
            let r = check_network(&NetworkDraft::parse(&body), &ctx);
            let (e, i) = codes(&r);
            assert_eq!(e, errs, "{name}: errors");
            assert_eq!(i, imps, "{name}: impacts");
            assert_eq!(r.ok, errs.is_empty(), "{name}: ok");
        }
        // the params, as the contract has them
        let r = check_network(&NetworkDraft::parse(&ndraft(&[(&long, "10.0.0.1"), ("b", "10.0.0.2"), ("c", "10.0.0.3"), ("d", "10.0.0.4"), ("e", "10.0.0.5")])), &ctx);
        assert_eq!(
            serde_json::to_value(&r.errors).unwrap(),
            json!([{ "field": "hops", "code": "too_many", "params": { "max": 4 } },
                   { "field": "hops[0].label", "code": "too_long", "params": { "max": 24 } }])
        );
        assert_eq!(serde_json::to_value(&r.impacts).unwrap(), json!([{ "level": "info", "code": "hops_changed" }]));
        // nothing known about the peer (rx before a transmitter connected)
        let ctx = NetworkCtx { applied: &applied, own: &[], peers: &[] };
        assert!(check_network(&NetworkDraft::parse(&ndraft(&[("a", "192.168.0.210")])), &ctx).ok);
    }

    #[test]
    fn network_apply() {
        let applied = network_applied();
        let d = NetworkDraft::parse(&ndraft(&[(" MS ", "192.168.0.66")]));
        assert_eq!(d.version, Some(3.0));
        let next = apply_network(&d, &applied);
        assert_eq!(next, NetworkStored { version: 4, hops: vec![Hop { label: "MS".into(), address: "192.168.0.66".into() }] });
        let next = apply_network(&NetworkDraft::parse(&ndraft(&[])), &next);
        assert_eq!((next.version, next.hops.len()), (5, 0));
        // what may be stored
        assert!(hops_sane(&applied.hops) && hops_sane(&[]));
        assert!(!hops_sane(&[Hop { label: "a".into(), address: "radio.local".into() }]));
        assert_eq!(hop_address("192.168.0.65"), Some(Ipv4Addr::new(192, 168, 0, 65)));
        for bad in ["0.0.0.0", "255.255.255.255", "127.0.0.53", "10.0.0.01", "1.2.3", "::1", "localhost"] {
            assert_eq!(hop_address(bad), None, "{bad}");
        }
    }

    /// (case, applied, draft, errors, impacts): the mock's checkOutput is the oracle.
    #[test]
    fn output_rules() {
        use DisplayMode::*;
        let d = |v: Value| json!({ "version": 2, "display": v });
        let cases: Vec<(&str, DisplayMode, Value, &[&str], &[&str])> = vec![
            ("off, unchanged", Off, d(json!("off")), &[], &[]),
            ("off → window", Off, d(json!("window")), &[], &["info:display_on"]),
            ("off → fullscreen", Off, d(json!("fullscreen")), &[], &["info:display_on", "info:display_fullscreen_covers"]),
            ("window → fullscreen", Window, d(json!("fullscreen")), &[], &["info:display_on", "info:display_fullscreen_covers"]),
            ("fullscreen → window", Fullscreen, d(json!("window")), &[], &["info:display_on"]),
            ("fullscreen → off", Fullscreen, d(json!("off")), &[], &["info:display_off"]),
            ("window → off", Window, d(json!("off")), &[], &["info:display_off"]),
            ("fullscreen, unchanged", Fullscreen, d(json!("fullscreen")), &[], &[]),
            ("not a mode", Off, d(json!("on")), &["display:invalid"], &[]),
            ("another case", Off, d(json!("Window")), &["display:invalid"], &[]),
            ("a number", Window, d(json!(1)), &["display:invalid"], &[]),
            ("null", Window, d(Value::Null), &["display:invalid"], &[]),
            ("missing", Window, json!({ "version": 2 }), &["display:invalid"], &[]),
            ("not an object", Window, json!("fullscreen"), &["display:invalid"], &[]),
        ];
        for (name, applied, body, errs, imps) in cases {
            let r = check_output(&OutputDraft::parse(&body), &OutputStored { version: 2, display: applied });
            let (e, i) = codes(&r);
            assert_eq!(e, errs, "{name}: errors");
            assert_eq!(i, imps, "{name}: impacts");
            assert_eq!(r.ok, errs.is_empty(), "{name}: ok");
        }
        let r = check_output(&OutputDraft::parse(&json!({ "display": "x" })), &OutputStored::default());
        assert_eq!(serde_json::to_value(&r).unwrap(), json!({ "ok": false, "errors": [{ "field": "display", "code": "invalid" }], "impacts": [] }));
        let r = check_output(&OutputDraft::parse(&json!({ "display": "fullscreen" })), &OutputStored::default());
        assert_eq!(
            serde_json::to_value(&r.impacts).unwrap(),
            json!([{ "level": "info", "code": "display_on" }, { "level": "info", "code": "display_fullscreen_covers" }])
        );
        assert_eq!(OutputDraft::parse(&json!({ "version": 7, "display": "window" })).version, Some(7.0));

        // the operation screen's button: {mode}
        assert_eq!(display_request(&json!({ "mode": "window" })), Ok(Window));
        assert_eq!(display_request(&json!({ "mode": "off", "display": "window" })), Ok(Off));
        for bad in [json!({}), json!({ "mode": "on" }), json!({ "mode": null }), json!({ "display": "window" }), json!([])] {
            let e = display_request(&bad).unwrap_err();
            assert_eq!(serde_json::to_value(&e).unwrap(), json!([{ "field": "mode", "code": "invalid" }]), "{bad}");
        }
        // the version goes up, also for the same value
        let a = OutputStored { version: 4, display: Window };
        assert_eq!(apply_output(Fullscreen, &a), OutputStored { version: 5, display: Fullscreen });
        assert_eq!(apply_output(Window, &a), OutputStored { version: 5, display: Window });
    }

    #[test]
    fn send_limit_apply() {
        let mut applied = transport_applied();
        applied.send_limit = SendLimit {
            mbps: 5.26,
            source: crate::model::LimitSource::Measured,
            measured_at: Some("2026-11-04T14:50:12+09:00".into()),
            test_id: Some("1-2".into()),
        };
        // the same value keeps its source; a new one is the operator's, two decimals
        let next = apply_transport(&TransportDraft::parse(&same_as_applied(json!({ "mbps": 5.26 }))), &applied);
        assert_eq!((next.version, &next.send_limit), (4, &applied.send_limit));
        let next = apply_transport(&TransportDraft::parse(&same_as_applied(json!({ "mbps": 4.567 }))), &applied);
        assert_eq!(next.send_limit, SendLimit::manual(4.57));
        assert_eq!(serde_json::to_value(&next.send_limit).unwrap(), json!({ "mbps": 4.57, "source": "manual", "measured_at": null, "test_id": null }));
        let next = apply_transport(&TransportDraft::parse(&same_as_applied(Value::Null)), &applied);
        assert_eq!(next.send_limit, applied.send_limit);
    }
}
