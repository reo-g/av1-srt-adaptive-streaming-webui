//! Check rules for settings drafts: a port of checkVideo / checkTransport in web/mock/server.ts
//! (the oracle), plus a check that the chosen input is one the device offers, because the
//! device really opens it. Request bodies are read leniently: a missing, null or mistyped
//! value becomes a FieldError, never a 422.

use crate::model::{
    params, CheckResult, DelayProfile, FieldError, Impact, InputKind, LinkState, Mode, SendLimit, TransportStored,
    VideoInput, VideoStored, SEND_LIMIT_RANGE,
};
use crate::util::round;
use serde_json::{json, Value};

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

fn fe(field: &'static str, code: &'static str, p: Value) -> FieldError {
    FieldError { field, code, params: params(p) }
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

    match c.state {
        LinkState::Live if src_changed => {
            // A new input restarts the chain: the link is set up again (the UI shows the pair as one line).
            impacts.push(im("warn", "input_switch_gap", json!({ "gap_s": c.gap_s })));
            impacts.push(im("warn", "reconnect_gap", json!({ "gap_s": c.gap_s })));
        }
        LinkState::Live if mode_changed || br_changed => impacts.push(im("ok", "switch_no_gap", json!({}))),
        LinkState::Stopped if any => impacts.push(im("info", "saved_while_stopped", json!({}))),
        _ => {}
    }
    if mode_changed && manual {
        impacts.push(im("info", "manual_fixed", json!({})));
    }
    if mode_changed && !manual {
        impacts.push(im("info", "auto_decides", json!({})));
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
        delay: match d.profile.as_deref() {
            Some("low") => DelayProfile::Low,
            Some("standard") => DelayProfile::Standard,
            _ => a.delay,
        },
        cipher: d.cipher.clone().unwrap_or_else(|| a.cipher.clone()),
        // a new value is the operator's; the same value keeps where it came from
        send_limit: d.limit_changed(a).map_or_else(|| a.send_limit.clone(), SendLimit::manual),
    }
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
            let ctx = TransportCtx { applied: &applied, state, profiles: &profiles };
            let r = check_transport(&TransportDraft::parse(&body), &ctx);
            let (e, i) = codes(&r);
            assert_eq!(e, errs, "{name}: errors");
            assert_eq!(i, imps, "{name}: impacts");
        }
        // the gap follows the profile in effect when the requested one is not available
        let applied = transport_applied();
        let ctx = TransportCtx { applied: &applied, state: Live, profiles: &profiles };
        let r = check_transport(&TransportDraft::parse(&tdraft("outbound", "10.0.0.1", json!(1), "low", "none")), &ctx);
        assert_eq!(r.impacts[0].params["gap_s"], 10);
    }

    #[test]
    fn transport_passphrase_and_apply() {
        let profiles = [(DelayProfile::Standard, 9500, true)];
        let applied = transport_applied();
        let ctx = TransportCtx { applied: &applied, state: Stopped, profiles: &profiles };
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
            let ctx = TransportCtx { applied: &applied, state, profiles: &profiles };
            let r = check_transport(&TransportDraft::parse(&body), &ctx);
            let (e, i) = codes(&r);
            assert_eq!(e, errs, "{name}: errors");
            assert_eq!(i, imps, "{name}: impacts");
        }
        let applied = transport_applied();
        let ctx = TransportCtx { applied: &applied, state: Stopped, profiles: &profiles };
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
