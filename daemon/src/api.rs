//! HTTP: the embedded UI, the device API under /api/v1 (one dispatcher, like web/mock/server.ts)
//! and the live channel. API responses carry Cache-Control: no-store; no Server header is sent.

use crate::app::{base_name, App, CmdKind, Inner};
use crate::check::{
    apply_network, apply_transport, apply_video, check_network, check_transport, check_video, gap_s, NetworkCtx,
    NetworkDraft, TransportCtx, TransportDraft, VideoCtx, VideoDraft, BITRATE_RANGE, CIPHERS, MODES,
};
use crate::config::Role;
use crate::events::Ev;
use crate::history::INTERVAL_S;
use crate::media;
use crate::model::{DelayProfile, LimitSource, LinkState, Mode, NetworkStored, SendLimit, TransportStored, SIGNAL_FORMAT};
use crate::nettest;
use crate::preview::{Refusal, OFFER_LIMIT};
use crate::util::{iso_jst, now_ms};
use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use rust_embed::RustEmbed;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

#[derive(RustEmbed)]
#[folder = "../web/dist/"]
struct Dist;

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/api/v1/live", any(live))
        .route("/api", any(api))
        .route("/api/{*rest}", any(api))
        .fallback(ui)
        .with_state(app)
}

// ------------------------------------------------------------------ responses

fn json_response<T: Serialize>(code: StatusCode, body: &T) -> Response {
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    (code, [(CONTENT_TYPE, "application/json; charset=utf-8"), (CACHE_CONTROL, "no-store")], bytes).into_response()
}

fn ok<T: Serialize>(body: &T) -> Response {
    json_response(StatusCode::OK, body)
}

fn error(code: StatusCode, name: &str) -> Response {
    json_response(code, &json!({ "error": { "code": name } }))
}

fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "not_found")
}

/// A body that is not JSON at all. Anything that parses is checked field by field instead.
fn bad_body() -> Response {
    json_response(StatusCode::BAD_REQUEST, &json!({ "error": { "code": "invalid", "errors": [] } }))
}

fn body_json(body: &Bytes) -> Option<Value> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Some(json!({}));
    }
    serde_json::from_slice(body).ok()
}

fn query(uri: &Uri, key: &str) -> Option<f64> {
    uri.query()?
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
}

// ------------------------------------------------------------------ the API

async fn api(State(app): State<Arc<App>>, method: Method, uri: Uri, body: Result<Bytes, BytesRejection>) -> Response {
    let Some(path) = uri.path().strip_prefix("/api/v1") else { return not_found() };
    if path == "/preview/sessions" && method == Method::POST {
        // a body over the server's limit (2 MB) is a bad offer too, not a bare 413
        return match body {
            Ok(b) => start_preview(&app, &b).await,
            Err(_) => error(StatusCode::BAD_REQUEST, "bad_offer"),
        };
    }
    let body = match body {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    if let Some(id) = path.strip_prefix("/preview/sessions/").filter(|id| !id.is_empty() && !id.contains('/')) {
        if method == Method::DELETE {
            app.preview.close(id);
            return (StatusCode::NO_CONTENT, [(CACHE_CONTROL, "no-store")]).into_response();
        }
    }
    if path == "/network-tests" || path.starts_with("/network-tests/") {
        return network_tests(&app, method.as_str(), path, &body);
    }
    let command = match (method.as_str(), path) {
        ("POST", "/transport/start") => Some(CmdKind::Start),
        ("POST", "/transport/stop") => Some(CmdKind::Stop),
        ("POST", "/transport/retry") => Some(CmdKind::Retry),
        _ => None,
    };
    match command {
        // Not while a throughput test runs (it needs the line to itself); while the start is on
        // its way, no such test starts either.
        Some(CmdKind::Start) => match app.nettest.hold_start() {
            Some(_hold) => ok(&app.command(CmdKind::Start).await),
            None => error(StatusCode::CONFLICT, "busy"),
        },
        // the supervisor answers once the state has changed; returns the new status
        Some(k) => ok(&app.command(k).await),
        None => request(&app, method.as_str(), path, &uri, &body),
    }
}

/// The network tests (nettest.rs). POST answers at once with the running test.
fn network_tests(app: &Arc<App>, method: &str, path: &str, body: &Bytes) -> Response {
    let use_id = path
        .strip_prefix("/network-tests/")
        .and_then(|p| p.strip_suffix("/use"))
        .filter(|id| !id.is_empty() && !id.contains('/'));
    if let (Some(id), "POST") = (use_id, method) {
        return use_test(app, id);
    }
    match (method, path) {
        ("POST", "/network-tests") => {
            let Some(v) = body_json(body) else { return bad_body() };
            match nettest::start(app, &v) {
                Ok(t) => json_response(StatusCode::ACCEPTED, &t),
                Err(nettest::Refusal::Invalid(errors)) => {
                    json_response(StatusCode::BAD_REQUEST, &json!({ "error": { "code": "invalid", "errors": errors } }))
                }
                Err(nettest::Refusal::Busy) => error(StatusCode::CONFLICT, "busy"),
                Err(nettest::Refusal::UnsupportedRole) => error(StatusCode::CONFLICT, "unsupported_role"),
            }
        }
        ("GET", "/network-tests") => ok(&json!({ "items": app.nettest.list() })),
        ("GET", "/network-tests/options") => ok(&app.nettest.options()),
        ("GET", p) => match p.strip_prefix("/network-tests/").and_then(|id| app.nettest.get(id)) {
            Some(t) => ok(&t),
            None => not_found(),
        },
        _ => not_found(),
    }
}

/// POST /network-tests/{id}/use (tx): a finished test towards the peer becomes the send cap. Stored
/// now, also while live: the next transmission starts with it (the running one keeps its cap).
fn use_test(app: &App, id: &str) -> Response {
    if app.cfg.role != Role::Tx {
        return error(StatusCode::CONFLICT, "unsupported_role");
    }
    // the book's lock is released before the settings' (never both at once)
    let (mbps, at) = match app.nettest.send_limit_from(id) {
        Ok(v) => v,
        Err(nettest::UseRefusal::NotFound) => return not_found(),
        Err(nettest::UseRefusal::NotApplicable) => return error(StatusCode::CONFLICT, "not_applicable"),
    };
    let limit = SendLimit { mbps, source: LimitSource::Measured, measured_at: Some(at), test_id: Some(id.to_string()) };
    let mut i = app.lock();
    let mut next = i.settings.clone();
    let from = std::mem::replace(&mut next.transport.send_limit, limit);
    next.transport.version += 1;
    if let Err(e) = app.save_settings(&next) {
        dlog!("settings: cannot save: {e}");
    }
    i.settings = next;
    dlog!("send cap {mbps} Mbps from network test {id} (was {} Mbps, {})", from.mbps, from.source.name());
    send_limit_changed(app, &mut i, &from);
    app.send(&json!({ "type": "settings", "section": "transport", "version": i.settings.transport.version }));
    ok(&transport_view(app, &i))
}

fn send_limit_changed(app: &App, i: &mut Inner, from: &SendLimit) {
    let l = &i.settings.transport.send_limit;
    let ev = Ev::info("send_limit_changed").with(json!({ "mbps": l.mbps, "from_mbps": from.mbps, "source": l.source }));
    app.emit(i, ev);
}

/// Everything except the commands: answered from the current snapshot, without waiting.
fn request(app: &App, method: &str, path: &str, uri: &Uri, body: &Bytes) -> Response {
    let tx = app.cfg.role == Role::Tx;
    match (method, path) {
        ("GET", "/device") => ok(&app.device()),
        ("GET", "/status") => ok(&app.status(&app.lock())),
        ("GET", "/stats") => ok(&app.lock().stats),
        ("GET", "/stats/history") => {
            let points = query(uri, "window_s").map_or(60, |w| w as usize / INTERVAL_S);
            let (end, series) = app.lock().history.series(points);
            ok(&json!({ "interval_s": INTERVAL_S, "end": iso_jst(end.unwrap_or_else(now_ms)), "series": series }))
        }
        ("GET", "/events") => {
            let limit = query(uri, "limit").map_or(20, |l| (l as usize).min(crate::events::CAPACITY));
            ok(&app.lock().events.latest(limit))
        }
        // 途中の無線機: both roles
        (m, p) if p == "/settings/network" || p.starts_with("/settings/network/") => network_settings(app, m, p, body),
        // both roles: on rx the delay follows the transmitter (follows_peer) and is ignored in drafts
        (m, p) if p == "/settings/transport" || p.starts_with("/settings/transport/") => transport_settings(app, m, p, body),
        (m, p) if p.starts_with("/settings/") || p == "/transport/compare" => {
            if !tx {
                return error(StatusCode::CONFLICT, "unsupported_role");
            }
            settings(app, m, p, body)
        }
        _ => not_found(),
    }
}

/// A browser asks for the preview with its offer; the answer comes from the preview program
/// (it may have to start first: up to its ready time plus the answer timeout).
async fn start_preview(app: &App, body: &Bytes) -> Response {
    // the body is larger than the offer it carries (every line break is escaped)
    let offer = Some(body)
        .filter(|b| b.len() <= 4 * OFFER_LIMIT)
        .and_then(|b| serde_json::from_slice::<Value>(b).ok())
        .and_then(|v| v.get("offer")?.as_str().map(str::to_owned))
        .filter(|s| s.len() <= OFFER_LIMIT && s.trim_start().starts_with("v=0"));
    let Some(offer) = offer else { return error(StatusCode::BAD_REQUEST, "bad_offer") };
    if !app.preview.enabled() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "preview_unavailable");
    }
    if app.lock().link.state != LinkState::Live {
        return error(StatusCode::CONFLICT, "not_live");
    }
    match app.preview.offer(offer).await {
        Ok(a) => json_response(StatusCode::CREATED, &json!({ "id": a.id, "answer": a.sdp })),
        Err(r) => {
            let status = match r {
                Refusal::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
                Refusal::TooManyViewers => StatusCode::TOO_MANY_REQUESTS,
                Refusal::BadOffer | Refusal::NoCommonCodec => StatusCode::BAD_REQUEST,
            };
            error(status, r.code())
        }
    }
}

fn settings(app: &App, method: &str, path: &str, body: &Bytes) -> Response {
    match (method, path) {
        ("GET", "/settings/video") => ok(&video_view(app, &app.lock())),
        ("GET", "/settings/video/options") => ok(&video_options(app)),
        ("POST", "/settings/video/check") => {
            let Some(v) = body_json(body) else { return bad_body() };
            let inputs = Inputs::read(app);
            let i = app.lock();
            ok(&check_video(&VideoDraft::parse(&v), &video_ctx(app, &i, &inputs)))
        }
        ("PUT", "/settings/video") => put_video(app, body),
        ("GET", "/transport/compare") => ok(&compare(&app.lock().settings.transport)),
        _ => not_found(),
    }
}

/// The transport settings (both roles). The receiver does not choose the delay: it follows the
/// transmitter it is connected to (`follows_peer`), so a draft's delay is ignored there.
fn transport_settings(app: &App, method: &str, path: &str, body: &Bytes) -> Response {
    match (method, path) {
        ("GET", "/settings/transport") => ok(&transport_view(app, &app.lock())),
        ("GET", "/settings/transport/options") => ok(&transport_options(app)),
        ("POST", "/settings/transport/check") => {
            let Some(v) = body_json(body) else { return bad_body() };
            let i = app.lock();
            let profiles = profiles(app);
            ok(&check_transport(&transport_draft(app, &i, &v), &transport_ctx(&i, &profiles)))
        }
        ("PUT", "/settings/transport") => put_transport(app, body),
        _ => not_found(),
    }
}

/// A transport draft as the rules see it: on rx the delay is the stored one (never an error,
/// never a change).
fn transport_draft(app: &App, i: &Inner, v: &Value) -> TransportDraft {
    let mut d = TransportDraft::parse(v);
    if app.cfg.role == Role::Rx {
        d.profile = Some(i.settings.transport.delay.name().into());
    }
    d
}

/// 途中の無線機 (both roles). Allowed in every link state: applying stores the list, the next
/// reachability test checks it and link_filter.json follows it; the transmission is not touched.
fn network_settings(app: &App, method: &str, path: &str, body: &Bytes) -> Response {
    match (method, path) {
        ("GET", "/settings/network") => ok(&network_view(&app.network(&app.lock()))),
        ("POST", "/settings/network/check") => {
            let Some(v) = body_json(body) else { return bad_body() };
            let own = crate::util::local_ipv4s();
            let i = app.lock();
            let (applied, peers) = (app.network(&i), app.peer_addresses(&i));
            ok(&check_network(&NetworkDraft::parse(&v), &NetworkCtx { applied: &applied, own: &own, peers: &peers }))
        }
        ("PUT", "/settings/network") => put_network(app, body),
        _ => not_found(),
    }
}

fn network_view(n: &NetworkStored) -> Value {
    json!({ "version": n.version, "hops": n.hops })
}

/// 409 version_conflict → 400 invalid; answers the new NetworkSettings.
fn put_network(app: &App, body: &Bytes) -> Response {
    let Some(v) = body_json(body) else { return bad_body() };
    let d = NetworkDraft::parse(&v);
    let own = crate::util::local_ipv4s();
    let mut i = app.lock();
    let applied = app.network(&i);
    if d.version != Some(applied.version as f64) {
        return error(StatusCode::CONFLICT, "version_conflict");
    }
    let peers = app.peer_addresses(&i);
    let result = check_network(&d, &NetworkCtx { applied: &applied, own: &own, peers: &peers });
    if !result.ok {
        return json_response(StatusCode::BAD_REQUEST, &json!({ "error": { "code": "invalid", "errors": result.errors } }));
    }
    let mut next = i.settings.clone();
    next.network = Some(apply_network(&d, &applied));
    if let Err(e) = app.save_settings(&next) {
        dlog!("settings: cannot save: {e}");
    }
    i.settings = next;
    let n = app.network(&i);
    let list: Vec<String> = n.hops.iter().map(|h| format!("{} {}", h.label, h.address)).collect();
    dlog!("network settings v{}: [{}]", n.version, list.join(", "));
    app.emit(&mut i, Ev::info("network_settings_applied"));
    app.send(&json!({ "type": "settings", "section": "network", "version": n.version }));
    app.update_link_filter(&i);
    ok(&network_view(&n))
}

/// The inputs the device offers now (read from the system, outside the state lock).
struct Inputs {
    cameras: Vec<String>,
    files: Vec<media::MediaFile>,
    file_paths: Vec<String>,
}

impl Inputs {
    fn read(app: &App) -> Inputs {
        let cam = &app.cfg.tx.camera;
        let cameras = if Path::new(cam).exists() { vec![cam.clone()] } else { Vec::new() };
        let files = media::media_files(app.cfg.tx.media_dir.as_deref());
        let file_paths = files.iter().map(|f| f.path.clone()).collect();
        Inputs { cameras, files, file_paths }
    }
}

fn video_ctx<'a>(app: &App, i: &'a Inner, inputs: &'a Inputs) -> VideoCtx<'a> {
    VideoCtx {
        applied: &i.settings.video,
        state: i.link.state,
        cur_mbps: app.current_mbps(i),
        bandwidth_mbps: i.stats.bandwidth_mbps,
        loop_available: false,
        // a new input sets the link up again: the picture comes back after about the delay
        gap_s: gap_s(app.applied_profile(i).delay_target_ms),
        cameras: &inputs.cameras,
        files: &inputs.file_paths,
        low_delay: !app.auto_available(i),
    }
}

fn profiles(app: &App) -> [(DelayProfile, u32, bool); 2] {
    let p = &app.profiles;
    [(DelayProfile::Standard, p.standard.delay_target_ms, true), (DelayProfile::Low, p.low.delay_target_ms, p.low_enabled)]
}

fn transport_ctx<'a>(i: &'a Inner, profiles: &'a [(DelayProfile, u32, bool)]) -> TransportCtx<'a> {
    let v = &i.settings.video;
    TransportCtx {
        applied: &i.settings.transport,
        state: i.link.state,
        profiles,
        video_auto: v.mode == Mode::Auto,
        fixed_mbps: v.manual_mbps,
    }
}

fn video_view(app: &App, i: &Inner) -> Value {
    let v = &i.settings.video;
    json!({
        "version": v.version,
        "input": v.input,
        "encode": { "mode": v.mode, "manual": { "bitrate_mbps": v.manual_mbps }, "auto": { "bitrate_mbps": app.auto_mbps(i) } },
        "audio": { "enabled": false },
    })
}

fn video_options(app: &App) -> Value {
    let inputs = Inputs::read(app);
    let cam = &app.cfg.tx.camera;
    let signal = (!inputs.cameras.is_empty()).then_some(SIGNAL_FORMAT);
    let files: Vec<Value> = inputs
        .files
        .iter()
        .map(|f| json!({ "path": f.path, "format": f.format(), "duration_s": f.duration_s }))
        .collect();
    let i = app.lock();
    let (bandwidth, auto_available, keyframe_s) =
        (i.stats.bandwidth_mbps, app.auto_available(&i), app.applied_profile(&i).keyframe_s());
    drop(i);
    json!({
        "cameras": [{ "device": cam, "label": media::camera_label(cam), "signal": signal }],
        "files": files,
        "loop_available": false,
        "auto_available": auto_available,
        "bitrate_range_mbps": [BITRATE_RANGE.0, BITRATE_RANGE.1],
        "encoding": { "codec": "av1", "resolution": "source", "framerate": "source", "keyframe_s": keyframe_s },
        "bandwidth_mbps": bandwidth,
    })
}

/// TransportSettings. tx: the delay chosen here; rx: the transmitter's of the current or last
/// connection (standard before any), which it follows.
fn transport_view(app: &App, i: &Inner) -> Value {
    let t = &i.settings.transport;
    let rx = app.cfg.role == Role::Rx;
    let delay = if rx { i.link.delay } else { t.delay };
    json!({
        "version": t.version,
        "mode": t.mode,
        "peer": { "host": t.peer_host, "port": t.peer_port },
        "listen_port": t.listen_port,
        "delay": { "profile": delay, "follows_peer": rx },
        "encryption": { "cipher": t.cipher, "passphrase_set": false, "passphrase_rejected_at": null },
        "send_limit": t.send_limit,
    })
}

fn transport_options(app: &App) -> Value {
    let profiles: Vec<Value> = profiles(app)
        .iter()
        .map(|(p, ms, avail)| json!({ "profile": p, "target_ms": ms, "available": avail }))
        .collect();
    json!({ "modes": MODES, "ciphers": CIPHERS, "delay_profiles": profiles })
}

/// The peer's settings come with the device-to-device link (phase 4): unknown until then.
fn compare(t: &TransportStored) -> Value {
    json!({
        "received_at": null,
        "peer_id": null,
        "rows": [
            { "key": "mode", "local": t.mode, "peer": null, "verdict": "unknown" },
            { "key": "endpoint", "local": { "host": t.peer_host, "port": t.peer_port }, "peer": null, "verdict": "unknown" },
            { "key": "delay", "local": t.delay, "peer": null, "verdict": "unknown" },
            { "key": "cipher", "local": t.cipher, "peer": null, "verdict": "unknown" },
            { "key": "passphrase", "local_set": false, "verdict": "unknown", "reason": null },
        ],
    })
}

/// Phase 1: settings are applied only while stopped (the next start uses them).
fn put_video(app: &App, body: &Bytes) -> Response {
    let Some(v) = body_json(body) else { return bad_body() };
    let d = VideoDraft::parse(&v);
    let inputs = Inputs::read(app);
    let mut i = app.lock();
    if d.version != Some(i.settings.video.version as f64) {
        return error(StatusCode::CONFLICT, "version_conflict");
    }
    if i.link.state != LinkState::Stopped {
        return error(StatusCode::CONFLICT, "busy");
    }
    let result = check_video(&d, &video_ctx(app, &i, &inputs));
    if !result.ok {
        return json_response(StatusCode::BAD_REQUEST, &json!({ "error": { "code": "invalid", "errors": result.errors } }));
    }
    let mut next = i.settings.clone();
    next.video = apply_video(&d, &i.settings.video);
    if let Err(e) = app.save_settings(&next) {
        dlog!("settings: cannot save: {e}");
    }
    i.settings = next;
    dlog!("video settings {:?} (input {})", i.settings.video, base_name(&i.settings.video.input.file));
    app.emit(&mut i, Ev::info("video_settings_applied"));
    app.send(&json!({ "type": "settings", "section": "video", "version": i.settings.video.version }));
    app.push_status(&i);
    ok(&json!({ "settings": video_view(app, &i), "applied_at": iso_jst(now_ms()), "impacts": result.impacts }))
}

/// As put_video, except that a change of the send cap alone is also applied while the link runs
/// (stored now; the running transmission keeps the cap it started with).
fn put_transport(app: &App, body: &Bytes) -> Response {
    let Some(v) = body_json(body) else { return bad_body() };
    let profiles = profiles(app);
    let mut i = app.lock();
    let d = transport_draft(app, &i, &v);
    if d.version != Some(i.settings.transport.version as f64) {
        return error(StatusCode::CONFLICT, "version_conflict");
    }
    // while running, a change of the send cap alone is stored (the next start uses it)
    if i.link.state != LinkState::Stopped && !d.only_send_limit(&i.settings.transport) {
        return error(StatusCode::CONFLICT, "busy");
    }
    let result = check_transport(&d, &transport_ctx(&i, &profiles));
    if !result.ok {
        return json_response(StatusCode::BAD_REQUEST, &json!({ "error": { "code": "invalid", "errors": result.errors } }));
    }
    let mut next = i.settings.clone();
    next.transport = apply_transport(&d, &i.settings.transport);
    if let Err(e) = app.save_settings(&next) {
        dlog!("settings: cannot save: {e}");
    }
    let only_limit = d.only_send_limit(&i.settings.transport);
    let from = std::mem::replace(&mut i.settings, next).transport.send_limit;
    dlog!("transport settings {:?}", i.settings.transport);
    // a change of the cap alone has its own event only (as the mock)
    if !only_limit {
        app.emit(&mut i, Ev::info("transport_settings_applied"));
    }
    if i.settings.transport.send_limit != from {
        send_limit_changed(app, &mut i, &from);
    }
    app.send(&json!({ "type": "settings", "section": "transport", "version": i.settings.transport.version }));
    app.push_status(&i);
    // the radio line's filter follows the peer (host and port)
    app.update_link_filter(&i);
    ok(&json!({ "settings": transport_view(app, &i), "applied_at": iso_jst(now_ms()), "impacts": result.impacts }))
}

// ------------------------------------------------------------------ live channel

async fn live(State(app): State<Arc<App>>, ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>) -> Response {
    match ws {
        Ok(ws) => ws.on_upgrade(move |socket| live_session(app, socket)),
        Err(_) => not_found(),
    }
}

/// hello first, then everything the supervisor pushes (stats and status every second, status
/// on change, events, settings). The browser never sends anything that matters.
async fn live_session(app: Arc<App>, mut socket: WebSocket) {
    let mut rx = app.subscribe();
    if socket.send(Message::Text(app.hello().into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(text) => {
                    if socket.send(Message::Text(text.to_string().into())).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
}

// ------------------------------------------------------------------ the UI (web/dist, embedded)

async fn ui(method: Method, uri: Uri) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    // Routes are in the hash; any other path without an extension gets the page itself.
    let (name, file) = match Dist::get(path) {
        Some(f) => (path, f),
        None if !path.contains('.') => match Dist::get("index.html") {
            Some(f) => ("index.html", f),
            None => return StatusCode::NOT_FOUND.into_response(),
        },
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    let cache = if name.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    (StatusCode::OK, [(CONTENT_TYPE, mime(name)), (CACHE_CONTROL, cache)], Body::from(file.data)).into_response()
}

fn mime(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, e)| e) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_app;
    use crate::config::Config;
    use crate::preview::{stub_launch, Preview};
    use axum::http::HeaderMap;

    const H264: &str = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\na=rtpmap:102 H264/90000\r\na=fmtp:102 packetization-mode=1\r\n";
    const VP8: &str = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=rtpmap:96 VP8/90000\r\n";

    /// The request through the same body extractor as the router (its 2 MB limit included).
    async fn call(app: &Arc<App>, method: Method, path: &str, body: impl Into<Bytes>) -> (StatusCode, HeaderMap, Value) {
        let req = axum::http::Request::builder().method(method.clone()).uri(path).body(Body::from(body.into())).unwrap();
        let body = <Result<Bytes, BytesRejection> as axum::extract::FromRequest<()>>::from_request(req, &()).await;
        let r = api(State(app.clone()), method, path.parse().unwrap(), body.unwrap()).await;
        let (status, headers) = (r.status(), r.headers().clone());
        let bytes = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        (status, headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn offer(app: &Arc<App>, sdp: &str) -> (StatusCode, Value) {
        let (s, _, v) = call(app, Method::POST, "/api/v1/preview/sessions", json!({ "offer": sdp }).to_string()).await;
        (s, v)
    }

    fn code(v: &Value) -> &str {
        v["error"]["code"].as_str().unwrap_or_default()
    }

    fn stub_app(name: &str, state: LinkState) -> (Arc<App>, Preview, std::path::PathBuf) {
        let mut cfg = None;
        let (l, dir) = stub_launch(name, "", |c| cfg = Some(c.clone()), 2);
        let preview = Preview::start(Some(l));
        (test_app(cfg.unwrap(), preview.clone(), state), preview, dir)
    }

    #[tokio::test]
    async fn preview_sessions() {
        let (app, preview, dir) = stub_app("http", LinkState::Live);
        let path = "/api/v1/preview/sessions";
        for body in [
            "".to_string(),
            "not json".into(),
            "{}".into(),
            json!({ "offer": "" }).to_string(),
            json!({ "offer": 5 }).to_string(),
            json!({ "offer": "m=video 9 RTP/AVP 96" }).to_string(),
            json!({ "offer": format!("{H264}{}", "a=x\r\n".repeat(14_000)) }).to_string(),
            // over the server's own body limit (2 MB)
            json!({ "offer": format!("{H264}{}", "a=x\r\n".repeat(500_000)) }).to_string(),
        ] {
            let (s, _, v) = call(&app, Method::POST, path, body.clone()).await;
            assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "bad_offer"), "{}", &body[..body.len().min(40)]);
        }
        assert_eq!(offer(&app, VP8).await, (StatusCode::BAD_REQUEST, json!({ "error": { "code": "no_common_codec" } })));
        let (s, h, v) = call(&app, Method::POST, path, json!({ "offer": H264 }).to_string()).await;
        assert_eq!(s, StatusCode::CREATED);
        assert_eq!(h[CACHE_CONTROL], "no-store");
        let id = v["id"].as_str().unwrap().to_string();
        assert!(v["answer"].as_str().unwrap().starts_with("v=0\r\n"));
        assert_eq!(v.as_object().unwrap().len(), 2, "{v}");
        // just under the limit: the body itself is larger (escaped line breaks)
        let long = format!("{H264}{}", "a=x\r\n".repeat(12_000));
        assert!(long.len() < OFFER_LIMIT && json!({ "offer": long }).to_string().len() > OFFER_LIMIT);
        let (s, _) = offer(&app, &long).await;
        assert_eq!(s, StatusCode::CREATED);
        let (s, v) = offer(&app, H264).await;
        assert_eq!((s, code(&v)), (StatusCode::TOO_MANY_REQUESTS, "too_many_viewers"));

        let (s, h, _) = call(&app, Method::DELETE, &format!("{path}/{id}"), "").await;
        assert_eq!((s, &h[CACHE_CONTROL]), (StatusCode::NO_CONTENT, &"no-store".parse::<axum::http::HeaderValue>().unwrap()));
        assert_eq!(call(&app, Method::DELETE, &format!("{path}/unknown"), "").await.0, StatusCode::NO_CONTENT);
        assert_eq!(offer(&app, H264).await.0, StatusCode::CREATED, "the closed session's place is free");
        assert_eq!(call(&app, Method::GET, path, "").await.0, StatusCode::NOT_FOUND);
        assert_eq!(call(&app, Method::DELETE, path, "").await.0, StatusCode::NOT_FOUND);
        assert_eq!(call(&app, Method::GET, "/api/v1/preview.mjpeg", "").await.0, StatusCode::NOT_FOUND);
        let (s, _, v) = call(&app, Method::GET, "/api/v1/status", "").await;
        assert_eq!(s, StatusCode::OK);
        assert!(v["media"].get("preview_fps").is_none(), "{}", v["media"]);

        // the program fails on an offer, then is gone
        preview.stop().await.unwrap();
        let (s, v) = offer(&app, H264).await;
        assert_eq!((s, code(&v)), (StatusCode::SERVICE_UNAVAILABLE, "preview_unavailable"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn network_tests_over_http() {
        use crate::config::Role;
        use crate::nettest::{stub_app, wait_done};
        let (app, dir) = stub_app("http", Role::Tx, LinkState::Live, "");
        let post = |body: Value| call(&app, Method::POST, "/api/v1/network-tests", body.to_string());

        // the order of the checks: kind → role → fields → busy
        let (s, h, v) = call(&app, Method::POST, "/api/v1/network-tests", "not json").await;
        assert_eq!((s, &v), (StatusCode::BAD_REQUEST, &json!({ "error": { "code": "invalid", "errors": [] } })));
        assert_eq!(h[CACHE_CONTROL], "no-store");
        let (s, _, v) = call(&app, Method::POST, "/api/v1/network-tests", "").await;
        assert_eq!((s, &v["error"]["errors"]), (StatusCode::BAD_REQUEST, &json!([{ "field": "kind", "code": "required" }])));
        // the old modes' fields are ignored
        let (s, _, v) = post(json!({ "kind": "throughput", "mode": "steady", "rate_mbps": 0, "duration_s": 3 })).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"]["errors"], json!([{ "field": "duration_s", "code": "out_of_range", "params": { "min": 5, "max": 30 } }]));
        let (s, _, v) = post(json!({ "kind": "throughput" })).await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "busy"), "the link is live");

        // reachability while live
        let (s, _, v) = post(json!({ "kind": "reachability" })).await;
        assert_eq!(s, StatusCode::ACCEPTED);
        assert_eq!((v["kind"].as_str(), v["state"].as_str(), &v["result"]), (Some("reachability"), Some("running"), &Value::Null));
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["error", "finished_at", "id", "kind", "progress", "request", "result", "started_at", "state"]);
        assert!(v["started_at"].as_str().unwrap().ends_with("+09:00"));
        let reach = v["id"].as_str().unwrap().to_string();
        let t = wait_done(&app, &reach).await;
        let (s, _, v) = call(&app, Method::GET, &format!("/api/v1/network-tests/{reach}"), "").await;
        assert_eq!((s, v["state"].as_str()), (StatusCode::OK, Some("done")));
        assert_eq!(v["result"]["targets"][0], json!({
            "kind": "peer", "label": null, "address": "10.0.0.2", "sent": 10, "received": 10,
            "rtt_min_ms": 0.01, "rtt_avg_ms": 0.02, "rtt_max_ms": 0.05,
        }));
        assert_eq!(v["finished_at"].as_str(), t.finished_at.as_deref());

        // throughput once stopped; meanwhile neither another one nor a transport start
        app.lock().link.state = LinkState::Stopped;
        app.lock().settings.transport.peer_host = "slow-10.0.0.2".into();
        let (s, _, v) = post(json!({ "kind": "throughput", "direction": "from_peer", "duration_s": 5 })).await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        assert_eq!(v["request"], json!({ "kind": "throughput", "direction": "from_peer", "duration_s": 5 }));
        assert_eq!(v["result"], Value::Null);
        let id = v["id"].as_str().unwrap().to_string();
        let (s, _, v) = post(json!({ "kind": "throughput" })).await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "busy"));
        let (s, _, v) = call(&app, Method::POST, "/api/v1/transport/start", "").await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "busy"));
        let (s, _, _) = call(&app, Method::POST, "/api/v1/transport/stop", "").await;
        assert_eq!(s, StatusCode::OK, "stop is never refused");
        let (s, _, _) = post(json!({ "kind": "reachability" })).await;
        assert_eq!(s, StatusCode::ACCEPTED, "a reachability test may run meanwhile");
        wait_done(&app, &id).await;
        let (s, _, _) = call(&app, Method::POST, "/api/v1/transport/start", "").await;
        assert_eq!(s, StatusCode::OK);

        // the list (newest first), one test, the options
        let (s, _, v) = call(&app, Method::GET, "/api/v1/network-tests", "").await;
        let kinds: Vec<&str> = v["items"].as_array().unwrap().iter().map(|t| t["kind"].as_str().unwrap()).collect();
        assert_eq!((s, kinds), (StatusCode::OK, vec!["reachability", "throughput", "reachability"]));
        assert_eq!(v["items"][1]["id"], id.as_str());
        let r = &v["items"][1]["result"];
        assert_eq!(
            r,
            &json!({ "throughput_mbps": 6.0, "sent_mbps": 6.0, "loss_pct": 0.0, "jitter_ms": 0.0, "saturated": false, "per_second_mbps": [6.01, 6.0, 6.0] })
        );
        assert_eq!(app.lock().events.latest(1)[0].params["direction"], "from_peer");
        let (s, _, v) = call(&app, Method::GET, "/api/v1/network-tests/options", "").await;
        assert_eq!((s, v), (StatusCode::OK, json!({ "duration_range_s": [5, 30], "saturate_mbps": 6.0 })));
        for p in ["/api/v1/network-tests/nope", "/api/v1/network-tests/"] {
            let (s, _, v) = call(&app, Method::GET, p, "").await;
            assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "not_found"), "{p}");
        }
        assert_eq!(call(&app, Method::DELETE, &format!("/api/v1/network-tests/{id}"), "").await.0, StatusCode::NOT_FOUND);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_measured_speed_becomes_the_send_cap() {
        use crate::config::Role;
        use crate::nettest::{stub_app, wait_done};
        let (app, dir) = stub_app("http-use", Role::Tx, LinkState::Stopped, "");
        let post = |path: String| {
            let app = app.clone();
            async move { call(&app, Method::POST, &path, "").await }
        };
        let test = |body: Value| {
            let app = app.clone();
            async move {
                let (s, _, v) = call(&app, Method::POST, "/api/v1/network-tests", body.to_string()).await;
                assert_eq!(s, StatusCode::ACCEPTED, "{v}");
                wait_done(&app, v["id"].as_str().unwrap()).await.id
            }
        };
        let (s, _, v) = call(&app, Method::GET, "/api/v1/settings/transport", "").await;
        assert_eq!((s, &v["send_limit"]), (StatusCode::OK, &json!({ "mbps": 5.0, "source": "default", "measured_at": null, "test_id": null })));
        assert_eq!(v["version"], 1);

        // what cannot be used
        let (s, _, v) = post("/api/v1/network-tests/nope/use".into()).await;
        assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "not_found"));
        let reach = test(json!({ "kind": "reachability" })).await;
        let back = test(json!({ "kind": "throughput", "direction": "from_peer" })).await;
        app.lock().settings.transport.peer_host = "refused.example".into();
        let failed = test(json!({ "kind": "throughput" })).await;
        for id in [&reach, &back, &failed] {
            let (s, _, v) = post(format!("/api/v1/network-tests/{id}/use")).await;
            assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "not_applicable"), "{id}");
        }
        assert_eq!(call(&app, Method::GET, &format!("/api/v1/network-tests/{back}/use"), "").await.0, StatusCode::NOT_FOUND);

        // a full line towards the receiver, used while live: stored now, for the next start
        app.lock().settings.transport.peer_host = "lossy-10.0.0.2".into();
        let full = test(json!({ "kind": "throughput" })).await;
        let finished_at = app.nettest.get(&full).unwrap().finished_at.unwrap();
        app.lock().link.state = LinkState::Live;
        let (s, h, v) = post(format!("/api/v1/network-tests/{full}/use")).await;
        assert_eq!((s, &h[CACHE_CONTROL]), (StatusCode::OK, &"no-store".parse::<axum::http::HeaderValue>().unwrap()));
        assert_eq!(v["send_limit"], json!({ "mbps": 5.26, "source": "measured", "measured_at": finished_at, "test_id": full }));
        assert_eq!((v["version"].clone(), v["peer"]["host"].clone()), (json!(2), json!("lossy-10.0.0.2")), "the rest as it was");
        let ev = app.lock().events.latest(1);
        assert_eq!((ev[0].code, Value::Object(ev[0].params.clone())), ("send_limit_changed", json!({ "mbps": 5.26, "from_mbps": 5.0, "source": "measured" })));
        let (_, _, now) = call(&app, Method::GET, "/api/v1/settings/transport", "").await;
        assert_eq!(now, v);
        // the next start's sender gets it
        let plan = app.plan(&app.lock());
        assert_eq!(plan.maxbw_kbps, 5260);
        let spec = crate::profile::sender(&app.cfg, app.profiles.get(plan.delay), &plan, &dir);
        assert_eq!(spec.args.last().map(String::as_str), Some("5260"));

        // kept across a restart
        let saved = crate::store::load(&dir.join("settings.json")).unwrap();
        assert_eq!(serde_json::to_value(&saved.transport.send_limit).unwrap(), v["send_limit"]);
        let again = crate::app::test_app_with(
            app.cfg.clone(),
            Preview::start(None),
            LinkState::Stopped,
            crate::nettest::NetTests::new(&app.cfg, crate::nettest::Server::start(None)),
        );
        let (_, _, after) = call(&again, Method::GET, "/api/v1/settings/transport", "").await;
        assert_eq!(after, v);
        assert_eq!(again.plan(&again.lock()).maxbw_kbps, 5260);

        // a line faster than the test: the test's rate
        app.lock().link.state = LinkState::Stopped;
        app.lock().settings.transport.peer_host = "10.0.0.2".into();
        let free = test(json!({ "kind": "throughput" })).await;
        let (s, _, v) = post(format!("/api/v1/network-tests/{free}/use")).await;
        assert_eq!((s, &v["send_limit"]["mbps"], &v["version"]), (StatusCode::OK, &json!(6.0), &json!(3)));
        assert_eq!(app.lock().events.latest(1)[0].params["from_mbps"], 5.26);
        std::fs::remove_dir_all(&dir).unwrap();

        // the receiver has no send cap
        let (app, dir) = stub_app("http-use-rx", Role::Rx, LinkState::Listening, "");
        let (s, _, v) = call(&app, Method::POST, "/api/v1/network-tests/nope/use", "").await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "unsupported_role"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn the_send_cap_in_the_transport_settings() {
        use crate::config::Role;
        let (app, dir) = crate::nettest::stub_app("http-limit", Role::Tx, LinkState::Live, "");
        let (_, _, view) = call(&app, Method::GET, "/api/v1/settings/transport", "").await;
        // the screen sends the applied values back
        let draft = |limit: Option<f64>| {
            let mut v = json!({ "version": view["version"], "mode": view["mode"], "peer": view["peer"], "listen_port": view["listen_port"],
                                "delay": view["delay"], "encryption": { "cipher": view["encryption"]["cipher"] } });
            if let Some(m) = limit {
                v["send_limit"] = json!({ "mbps": m });
            }
            v
        };
        let check = |body: Value| call(&app, Method::POST, "/api/v1/settings/transport/check", body.to_string());
        let put = |body: Value| call(&app, Method::PUT, "/api/v1/settings/transport", body.to_string());
        let (_, _, v) = check(draft(Some(5.6))).await;
        assert_eq!(v, json!({ "ok": true, "errors": [], "impacts": [{ "level": "info", "code": "takes_effect_next_start" }] }));
        let (_, _, v) = check(draft(Some(25.0))).await;
        assert_eq!(v["errors"], json!([{ "field": "send_limit.mbps", "code": "out_of_range", "params": { "min": 1.0, "max": 20.0 } }]));

        // while live: the cap alone is applied, anything else waits for a stop
        let mut other = draft(Some(5.6));
        other["peer"]["host"] = json!("10.0.0.3");
        let (s, _, v) = put(other).await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "busy"));
        let (s, _, v) = put(draft(None)).await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "busy"), "nothing to apply while live");
        let (s, _, v) = put(draft(Some(25.0))).await;
        assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "invalid"));
        let (s, _, v) = put(draft(Some(5.555))).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["settings"]["send_limit"], json!({ "mbps": 5.56, "source": "manual", "measured_at": null, "test_id": null }));
        assert_eq!((v["settings"]["version"].clone(), v["impacts"].clone()), (json!(2), json!([{ "level": "info", "code": "takes_effect_next_start" }])));
        let codes: Vec<&str> = app.lock().events.latest(5).iter().map(|e| e.code).collect();
        assert_eq!(codes, ["send_limit_changed"], "only its own event");
        assert_eq!(
            Value::Object(app.lock().events.latest(1)[0].params.clone()),
            json!({ "mbps": 5.56, "from_mbps": 5.0, "source": "manual" })
        );
        assert_eq!(crate::store::load(&dir.join("settings.json")).unwrap().transport.send_limit.mbps, 5.56);
        let (s, _, v) = put(draft(Some(5.6))).await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "version_conflict"));

        // stopped: everything, and the same cap keeps where it came from
        app.lock().link.state = LinkState::Stopped;
        let (_, _, view) = call(&app, Method::GET, "/api/v1/settings/transport", "").await;
        let mut body = json!({ "version": view["version"], "mode": "outbound", "peer": { "host": "10.0.0.3", "port": 60021 },
                               "listen_port": view["listen_port"], "delay": { "profile": "standard" }, "encryption": { "cipher": "none" },
                               "send_limit": { "mbps": 5.56 } });
        let (s, _, v) = put(body.clone()).await;
        assert_eq!((s, &v["settings"]["send_limit"]["source"], &v["settings"]["peer"]["host"]), (StatusCode::OK, &json!("manual"), &json!("10.0.0.3")));
        assert_eq!(app.lock().events.latest(1)[0].code, "transport_settings_applied");
        // the peer and the cap together: both events
        body["version"] = v["settings"]["version"].clone();
        body["peer"]["host"] = json!("10.0.0.4");
        body["send_limit"]["mbps"] = json!(4.0);
        let (s, _, v) = put(body.clone()).await;
        assert_eq!((s, &v["settings"]["send_limit"]["mbps"]), (StatusCode::OK, &json!(4.0)));
        let codes: Vec<&str> = app.lock().events.latest(2).iter().map(|e| e.code).collect();
        assert_eq!(codes, ["send_limit_changed", "transport_settings_applied"]);
        body["version"] = v["settings"]["version"].clone();
        body.as_object_mut().unwrap().remove("send_limit");
        let (s, _, v) = put(body).await;
        assert_eq!((s, &v["settings"]["send_limit"]["mbps"]), (StatusCode::OK, &json!(4.0)), "left out: kept");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The applied transport settings as the screen sends them back, with another delay.
    fn transport_draft_of(view: &Value, delay: &str) -> Value {
        json!({ "version": view["version"], "mode": view["mode"], "peer": view["peer"], "listen_port": view["listen_port"],
                "delay": { "profile": delay }, "encryption": { "cipher": view["encryption"]["cipher"] } })
    }

    #[tokio::test]
    async fn low_delay_on_the_transmitter() {
        use crate::config::Role;
        let (app, dir) = crate::nettest::stub_app("http-low", Role::Tx, LinkState::Stopped, "");
        let (_, _, opts) = call(&app, Method::GET, "/api/v1/settings/transport/options", "").await;
        assert_eq!(
            opts["delay_profiles"],
            json!([{ "profile": "standard", "target_ms": 9500, "available": true }, { "profile": "low", "target_ms": 1500, "available": true }])
        );
        let (_, _, view) = call(&app, Method::GET, "/api/v1/settings/transport", "").await;
        assert_eq!(view["delay"], json!({ "profile": "standard", "follows_peer": false }));
        let (_, _, v) = call(&app, Method::GET, "/api/v1/settings/video/options", "").await;
        assert_eq!(v["auto_available"], true);

        // the check: the stored mode is automatic, so the fixed bitrate is named
        let (_, _, v) = call(&app, Method::POST, "/api/v1/settings/transport/check", transport_draft_of(&view, "low").to_string()).await;
        assert_eq!(
            v["impacts"],
            json!([{ "level": "info", "code": "saved_while_stopped" }, { "level": "info", "code": "low_delay_fixed", "params": { "bitrate_mbps": 3.0 } },
                   { "level": "info", "code": "low_delay_effects" }])
        );
        app.lock().link.state = LinkState::Live;
        let (_, _, v) = call(&app, Method::POST, "/api/v1/settings/transport/check", transport_draft_of(&view, "low").to_string()).await;
        assert_eq!(v["impacts"][0], json!({ "level": "warn", "code": "reconnect_gap", "params": { "gap_s": 2 } }));
        app.lock().link.state = LinkState::Stopped;
        let (s, _, v) = call(&app, Method::PUT, "/api/v1/settings/transport", transport_draft_of(&view, "low").to_string()).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["settings"]["delay"], json!({ "profile": "low", "follows_peer": false }));

        // the video settings are not touched; automatic mode is not in effect while low is applied
        let (_, _, video) = call(&app, Method::GET, "/api/v1/settings/video", "").await;
        assert_eq!((video["encode"]["mode"].clone(), video["encode"]["manual"]["bitrate_mbps"].clone()), (json!("auto"), json!(3.0)));
        let (_, _, v) = call(&app, Method::GET, "/api/v1/settings/video/options", "").await;
        assert_eq!(v["auto_available"], false);
        assert_eq!(app.current_mbps(&app.lock()), 3.0);
        let vdraft = |mode: &str, br: f64| {
            json!({ "version": video["version"], "input": video["input"], "encode": { "mode": mode, "manual": { "bitrate_mbps": br } } })
        };
        let (_, _, v) = call(&app, Method::POST, "/api/v1/settings/video/check", vdraft("auto", 3.0).to_string()).await;
        assert_eq!(v, json!({ "ok": true, "errors": [], "impacts": [{ "level": "info", "code": "low_delay_fixed", "params": { "bitrate_mbps": 3.0 } }] }));
        let (_, _, v) = call(&app, Method::POST, "/api/v1/settings/video/check", vdraft("manual", 2.5).to_string()).await;
        let imps: Vec<&str> = v["impacts"].as_array().unwrap().iter().map(|i| i["code"].as_str().unwrap()).collect();
        assert_eq!(imps, ["saved_while_stopped", "manual_fixed", "bitrate_lower_quality"]);

        // the next start: the low-delay commands, the fixed bitrate although the stored mode is automatic
        let plan = app.plan(&app.lock());
        assert_eq!((plan.delay, plan.manual_kbps, plan.maxbw_kbps), (DelayProfile::Low, Some(3000), 5000));
        let p = app.profiles.get(plan.delay);
        let sender = crate::profile::sender(&app.cfg, p, &plan, &dir);
        assert_eq!((sender.env["SRT_STREAMID"].as_str(), sender.env["SRT_SEND_PARTIAL"].as_str()), ("delay=low", "1"));
        let bridge = crate::profile::bridge(&app.cfg, p, &plan, &dir);
        assert_eq!((bridge.env["INITIAL_TARGET"].as_str(), bridge.env["SVT_PRESET"].as_str(), bridge.env["BUF_NODROP"].as_str()), ("3000", "10", "0"));

        // back to standard: the stored automatic mode again
        let (_, _, view) = call(&app, Method::GET, "/api/v1/settings/transport", "").await;
        let (_, _, v) = call(&app, Method::POST, "/api/v1/settings/transport/check", transport_draft_of(&view, "standard").to_string()).await;
        assert_eq!(v["impacts"], json!([{ "level": "info", "code": "saved_while_stopped" }]));
        let (s, _, _) = call(&app, Method::PUT, "/api/v1/settings/transport", transport_draft_of(&view, "standard").to_string()).await;
        assert_eq!(s, StatusCode::OK);
        let plan = app.plan(&app.lock());
        assert_eq!((plan.delay, plan.manual_kbps), (DelayProfile::Standard, None));
        let sender = crate::profile::sender(&app.cfg, app.profiles.get(plan.delay), &plan, &dir);
        assert_eq!(sender.env["SRT_STREAMID"], "delay=standard");
        assert!(!sender.env.contains_key("SRT_SEND_PARTIAL"));
        let (_, _, v) = call(&app, Method::GET, "/api/v1/settings/video/options", "").await;
        assert_eq!(v["auto_available"], true);
        std::fs::remove_dir_all(&dir).unwrap();

        // turned off in device.toml: not offered, refused as before; a stored low is standard
        let (app, dir) = crate::nettest::stub_app("http-low-off", Role::Tx, LinkState::Stopped, "[profile_low]\nenabled = false\n");
        let (_, _, opts) = call(&app, Method::GET, "/api/v1/settings/transport/options", "").await;
        assert_eq!(opts["delay_profiles"][1], json!({ "profile": "low", "target_ms": 1500, "available": false }));
        let (_, _, view) = call(&app, Method::GET, "/api/v1/settings/transport", "").await;
        let (s, _, v) = call(&app, Method::PUT, "/api/v1/settings/transport", transport_draft_of(&view, "low").to_string()).await;
        assert_eq!((s, &v["error"]["errors"]), (StatusCode::BAD_REQUEST, &json!([{ "field": "delay.profile", "code": "invalid" }])));
        app.lock().settings.transport.delay = DelayProfile::Low;
        assert_eq!(app.plan(&app.lock()).delay, DelayProfile::Standard);
        let (_, _, v) = call(&app, Method::GET, "/api/v1/settings/video/options", "").await;
        assert_eq!(v["auto_available"], true);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn the_receiver_follows_the_transmitters_delay() {
        use crate::config::Role;
        let (app, dir) = crate::nettest::stub_app("http-low-rx", Role::Rx, LinkState::Stopped, "");
        let (s, _, view) = call(&app, Method::GET, "/api/v1/settings/transport", "").await;
        assert_eq!((s, &view["delay"]), (StatusCode::OK, &json!({ "profile": "standard", "follows_peer": true })), "before any connection");
        // a low-delay transmitter connected (supervisor.rs): what it uses, kept after it went away
        app.lock().link.delay = DelayProfile::Low;
        let (_, _, view) = call(&app, Method::GET, "/api/v1/settings/transport", "").await;
        assert_eq!(view["delay"], json!({ "profile": "low", "follows_peer": true }));
        // the plan stays standard (the player follows when the transmitter says so)
        let plan = app.plan(&app.lock());
        assert_eq!((plan.delay, plan.low_ctl.clone()), (DelayProfile::Standard, vec!["ts-offset-ms 400".to_string(), "adapt on".to_string()]));
        // a draft's delay is ignored: no error for one that is not offered, nothing stored
        for delay in ["standard", "low", "fast"] {
            let (_, _, v) = call(&app, Method::POST, "/api/v1/settings/transport/check", transport_draft_of(&view, delay).to_string()).await;
            assert_eq!(v, json!({ "ok": true, "errors": [], "impacts": [{ "level": "info", "code": "saved_while_stopped" }] }), "{delay}");
        }
        let mut other = transport_draft_of(&view, "fast");
        other["peer"]["port"] = json!(60031);
        let (s, _, v) = call(&app, Method::PUT, "/api/v1/settings/transport", other.to_string()).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["settings"]["delay"], json!({ "profile": "low", "follows_peer": true }));
        assert_eq!(app.lock().settings.transport.delay, DelayProfile::Standard, "the stored delay is not the receiver's to set");
        let (_, _, v) = call(&app, Method::GET, "/api/v1/settings/transport/options", "").await;
        assert_eq!(v["delay_profiles"][1]["profile"], "low");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    const RADIOS: &str = "[[network_test.hops]]\nlabel = \"無線機（送信機側）\"\naddress = \"198.51.100.65\"\n\
                          [[network_test.hops]]\nlabel = \"無線機（受信機側）\"\naddress = \"198.51.100.56\"\n";

    fn filter_file(dir: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join("link_filter.json")).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn network_settings_on_the_transmitter() {
        use crate::config::Role;
        let (app, dir) = crate::nettest::stub_app("http-net", Role::Tx, LinkState::Live, RADIOS);
        let check = |body: Value| call(&app, Method::POST, "/api/v1/settings/network/check", body.to_string());
        let put = |body: Value| call(&app, Method::PUT, "/api/v1/settings/network", body.to_string());
        let hops = |list: &[(&str, &str)]| -> Value { list.iter().map(|(l, a)| json!({ "label": l, "address": a })).collect() };
        let configured = hops(&[("無線機（送信機側）", "198.51.100.65"), ("無線機（受信機側）", "198.51.100.56")]);

        // nothing applied yet: device.toml's list, version 1; the filter file was written at the start
        let (s, h, v) = call(&app, Method::GET, "/api/v1/settings/network", "").await;
        assert_eq!((s, &h[CACHE_CONTROL]), (StatusCode::OK, &"no-store".parse::<axum::http::HeaderValue>().unwrap()));
        assert_eq!(v, json!({ "version": 1, "hops": configured }));
        assert_eq!(
            filter_file(&dir),
            json!({ "version": 1, "role": "tx", "peer": "10.0.0.2", "video_port": 60021, "test_port": 60022, "hosts": ["198.51.100.65", "198.51.100.56"] })
        );

        // check: changes nothing; the peer and this device's own addresses are refused
        let (s, _, v) = check(json!({ "version": 1, "hops": configured })).await;
        assert_eq!((s, v), (StatusCode::OK, json!({ "ok": true, "errors": [], "impacts": [] })));
        let draft = hops(&[("MS", "198.51.100.66"), ("受信機", "10.0.0.2")]);
        let (_, _, v) = check(json!({ "version": 1, "hops": draft })).await;
        assert_eq!(
            v,
            json!({ "ok": false, "errors": [{ "field": "hops[1].address", "code": "is_peer" }], "impacts": [{ "level": "info", "code": "hops_changed" }] })
        );
        if let Some(own) = crate::util::local_ipv4s().into_iter().find(|a| !a.is_loopback()) {
            let (_, _, v) = check(json!({ "version": 1, "hops": hops(&[("自分", &own.to_string())]) })).await;
            assert_eq!(v["errors"], json!([{ "field": "hops[0].address", "code": "is_self" }]), "{own}");
        }
        let (s, _, v) = check(json!("x")).await;
        assert_eq!((s, &v["errors"]), (StatusCode::OK, &json!([{ "field": "hops", "code": "required" }])));
        let (s, _, v) = call(&app, Method::POST, "/api/v1/settings/network/check", "not json").await;
        assert_eq!((s, v), (StatusCode::BAD_REQUEST, json!({ "error": { "code": "invalid", "errors": [] } })));
        assert_eq!(call(&app, Method::GET, "/api/v1/settings/network", "").await.2["version"], 1);

        // apply while live: the transmission is not touched (no busy), the list is stored
        let mut live = app.subscribe();
        let (s, _, v) = put(json!({ "version": 1, "hops": draft })).await;
        assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "invalid"));
        assert_eq!(v["error"]["errors"], json!([{ "field": "hops[1].address", "code": "is_peer" }]));
        let next = hops(&[(" MS ", "198.51.100.66"), ("BS", "198.51.100.56"), ("中継", "203.0.113.9")]);
        let (s, _, v) = put(json!({ "version": 1, "hops": next })).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let applied = json!({ "version": 2, "hops": hops(&[("MS", "198.51.100.66"), ("BS", "198.51.100.56"), ("中継", "203.0.113.9")]) });
        assert_eq!(v, applied, "the new NetworkSettings, labels trimmed");
        assert_eq!(call(&app, Method::GET, "/api/v1/settings/network", "").await.2, applied);
        assert_eq!(app.lock().link.state, LinkState::Live);
        let ev = app.lock().events.latest(1);
        assert_eq!((ev[0].code, ev[0].level, ev[0].params.is_empty()), ("network_settings_applied", "info", true));
        let mut msgs = Vec::new();
        while let Ok(m) = live.try_recv() {
            msgs.push(serde_json::from_str::<Value>(&m).unwrap());
        }
        assert!(msgs.contains(&json!({ "type": "settings", "section": "network", "version": 2 })), "{msgs:?}");
        // the filter file follows
        assert_eq!(filter_file(&dir)["hosts"], json!(["198.51.100.66", "198.51.100.56", "203.0.113.9"]));
        // a draft made from the old version
        let (s, _, v) = put(json!({ "version": 1, "hops": [] })).await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "version_conflict"));
        let (s, _, v) = put(json!({ "hops": [] })).await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "version_conflict"), "the version first");

        // kept across a restart: the stored list wins over device.toml's
        let saved = crate::store::load(&dir.join("settings.json")).unwrap();
        assert_eq!(serde_json::to_value(&saved.network).unwrap(), applied);
        let again = crate::app::test_app_with(
            app.cfg.clone(),
            Preview::start(None),
            LinkState::Stopped,
            crate::nettest::NetTests::new(&app.cfg, crate::nettest::Server::start(None)),
        );
        assert_eq!(call(&again, Method::GET, "/api/v1/settings/network", "").await.2, applied);
        assert_eq!(filter_file(&dir)["hosts"], json!(["198.51.100.66", "198.51.100.56", "203.0.113.9"]));
        // the next reachability test checks the stored list
        let (_, _, t) = call(&again, Method::POST, "/api/v1/network-tests", json!({ "kind": "reachability" }).to_string()).await;
        let t = crate::nettest::wait_done(&again, t["id"].as_str().unwrap()).await;
        let checked: Vec<String> = match serde_json::to_value(&t.result).unwrap()["targets"].as_array() {
            Some(a) => a.iter().map(|t| t["address"].as_str().unwrap().to_string()).collect(),
            None => panic!("{t:?}"),
        };
        assert_eq!(checked, ["10.0.0.2", "198.51.100.66", "198.51.100.56", "203.0.113.9"]);
        // an empty list is a list too
        let (s, _, v) = put(json!({ "version": 2, "hops": [] })).await;
        assert_eq!((s, v), (StatusCode::OK, json!({ "version": 3, "hops": [] })));
        assert_eq!(filter_file(&dir)["hosts"], json!([]));

        // a new peer in the transport settings: the filter follows (host and port)
        app.lock().link.state = LinkState::Stopped;
        let (_, _, view) = call(&app, Method::GET, "/api/v1/settings/transport", "").await;
        let body = json!({ "version": view["version"], "mode": "outbound", "peer": { "host": "10.0.0.3", "port": 60031 },
                           "listen_port": view["listen_port"], "delay": { "profile": "standard" }, "encryption": { "cipher": "none" } });
        let (s, _, v) = call(&app, Method::PUT, "/api/v1/settings/transport", body.to_string()).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(
            filter_file(&dir),
            json!({ "version": 1, "role": "tx", "peer": "10.0.0.3", "video_port": 60031, "test_port": 60022, "hosts": [] })
        );
        // and the check now refuses the new peer
        let (_, _, v) = check(json!({ "version": 3, "hops": hops(&[("x", "10.0.0.3"), ("y", "10.0.0.2")]) })).await;
        assert_eq!(v["errors"], json!([{ "field": "hops[0].address", "code": "is_peer" }]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn network_settings_on_the_receiver() {
        use crate::config::Role;
        let (app, dir) = crate::nettest::stub_app("http-net-rx", Role::Rx, LinkState::Listening, RADIOS);
        // both roles (the video settings and the comparison stay the transmitter's)
        let (s, _, v) = call(&app, Method::GET, "/api/v1/settings/network", "").await;
        assert_eq!((s, v["version"].clone(), v["hops"].as_array().map(Vec::len)), (StatusCode::OK, json!(1), Some(2)));
        let (s, _, v) = call(&app, Method::GET, "/api/v1/settings/video", "").await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "unsupported_role"));
        let (s, _, v) = call(&app, Method::GET, "/api/v1/transport/compare", "").await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "unsupported_role"));
        // no transmitter yet: no peer in the filter; the listen port is the video's
        assert_eq!(
            filter_file(&dir),
            json!({ "version": 1, "role": "rx", "peer": null, "video_port": 60021, "test_port": 60022, "hosts": ["198.51.100.65", "198.51.100.56"] })
        );
        let put = |body: Value| call(&app, Method::PUT, "/api/v1/settings/network", body.to_string());
        let (s, _, v) = put(json!({ "version": 1, "hops": [{ "label": "BS", "address": "192.168.0.220" }] })).await;
        assert_eq!(s, StatusCode::OK, "nothing known about the transmitter yet: {v}");
        // one connects (what publish does on a status change): the filter lets it through, and it is the peer
        app.lock().link.peer_addr = Some(("192.168.0.221".into(), 51234));
        app.update_link_filter(&app.lock());
        assert_eq!((filter_file(&dir)["peer"].clone(), filter_file(&dir)["hosts"].clone()), (json!("192.168.0.221"), json!(["192.168.0.220"])));
        let (s, _, v) = put(json!({ "version": 2, "hops": [{ "label": "TX", "address": "192.168.0.221" }] })).await;
        assert_eq!((s, &v["error"]["errors"]), (StatusCode::BAD_REQUEST, &json!([{ "field": "hops[0].address", "code": "is_peer" }])));
        // after a restart, before it connects again: still that one (from the previous file)
        let again = crate::app::test_app_with(
            app.cfg.clone(),
            Preview::start(None),
            LinkState::Listening,
            crate::nettest::NetTests::new(&app.cfg, crate::nettest::Server::start(None)),
        );
        assert_eq!(filter_file(&dir)["peer"], "192.168.0.221");
        let (_, _, v) = call(&again, Method::POST, "/api/v1/settings/network/check", json!({ "version": 2, "hops": [{ "label": "TX", "address": "192.168.0.221" }] }).to_string()).await;
        assert_eq!(v["errors"][0]["code"], "is_peer");
        std::fs::remove_dir_all(&dir).unwrap();

        // the configured transmitter wins over the one that connected
        let (app, dir) = crate::nettest::stub_app("http-net-rx-peer", Role::Rx, LinkState::Live, "[rx]\npeer_host = \"192.168.0.230\"\nlisten_port = 61000\n");
        app.lock().link.peer_addr = Some(("192.168.0.221".into(), 51234));
        app.update_link_filter(&app.lock());
        assert_eq!(
            filter_file(&dir),
            json!({ "version": 1, "role": "rx", "peer": "192.168.0.230", "video_port": 61000, "test_port": 60022, "hosts": [] })
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn network_tests_on_the_receiver() {
        use crate::config::Role;
        let (app, dir) = crate::nettest::stub_app("http-rx", Role::Rx, LinkState::Listening, "");
        let post = |body: Value| call(&app, Method::POST, "/api/v1/network-tests", body.to_string());
        let (s, _, v) = post(json!({ "kind": "throughput", "duration_s": "x" })).await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "unsupported_role"), "before the fields");
        let (s, _, v) = post(json!({ "kind": "nope" })).await;
        assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "invalid"));
        // no transmitter known and no hops: nothing to check, done at once; the last 20 are kept
        let mut ids = Vec::new();
        for _ in 0..22 {
            let (s, _, v) = post(json!({ "kind": "reachability" })).await;
            assert_eq!((s, v["state"].as_str(), &v["result"]), (StatusCode::ACCEPTED, Some("done"), &json!({ "targets": [] })));
            ids.push(v["id"].as_str().unwrap().to_string());
        }
        let (_, _, v) = call(&app, Method::GET, "/api/v1/network-tests", "").await;
        let listed: Vec<&str> = v["items"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
        let want: Vec<&str> = ids.iter().rev().take(20).map(String::as_str).collect();
        assert_eq!(listed, want);
        assert_eq!(call(&app, Method::GET, &format!("/api/v1/network-tests/{}", ids[0]), "").await.0, StatusCode::NOT_FOUND);
        assert_eq!(call(&app, Method::POST, "/api/v1/transport/start", "").await.0, StatusCode::OK);
        std::fs::remove_dir_all(&dir).unwrap();

        // turned off in device.toml: not offered
        let (app, dir) = crate::nettest::stub_app("http-off", Role::Tx, LinkState::Stopped, "enabled = false\n");
        let (s, _, v) = call(&app, Method::POST, "/api/v1/network-tests", json!({ "kind": "reachability" }).to_string()).await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "unsupported_role"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn preview_needs_a_live_link_and_the_program() {
        let (app, preview, dir) = stub_app("http-idle", LinkState::Listening);
        let (s, v) = offer(&app, H264).await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "not_live"));
        assert!(!dir.join("stub.jsonl").exists(), "the program is not started for it");
        preview.stop().await.unwrap();

        let (app, preview, dir2) = stub_app("http-fail", LinkState::Live);
        let (s, v) = offer(&app, &format!("{H264}a=x-stub:fail\r\n")).await;
        assert_eq!((s, code(&v)), (StatusCode::SERVICE_UNAVAILABLE, "preview_unavailable"));
        preview.stop().await.unwrap();

        let mut cfg = Config::parse("id = \"RX-01\"\nrole = \"rx\"\n[media]\nrepo = \"/m\"\n[preview]\nenabled = false\n").unwrap();
        cfg.state_dir = Some(dir.clone());
        let app = test_app(cfg, Preview::start(None), LinkState::Live);
        let (s, v) = offer(&app, H264).await;
        assert_eq!((s, code(&v)), (StatusCode::SERVICE_UNAVAILABLE, "preview_unavailable"));
        let (s, _, _) = call(&app, Method::DELETE, "/api/v1/preview/sessions/abc", "").await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&dir2).unwrap();
    }
}
