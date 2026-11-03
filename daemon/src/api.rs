//! HTTP: the embedded UI, the device API under /api/v1 (one dispatcher, like web/mock/server.ts)
//! and the live channel. API responses carry Cache-Control: no-store; no Server header is sent.

use crate::app::{base_name, App, CmdKind, Inner};
use crate::check::{
    apply_transport, apply_video, check_transport, check_video, gap_s, TransportCtx, TransportDraft, VideoCtx,
    VideoDraft, BITRATE_RANGE, CIPHERS, MODES,
};
use crate::config::Role;
use crate::events::Ev;
use crate::history::INTERVAL_S;
use crate::media;
use crate::model::{DelayProfile, LinkState, TransportStored, SIGNAL_FORMAT};
use crate::util::{iso_jst, now_ms};
use axum::body::{Body, Bytes};
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

async fn api(State(app): State<Arc<App>>, method: Method, uri: Uri, body: Bytes) -> Response {
    let Some(path) = uri.path().strip_prefix("/api/v1") else { return not_found() };
    let command = match (method.as_str(), path) {
        ("POST", "/transport/start") => Some(CmdKind::Start),
        ("POST", "/transport/stop") => Some(CmdKind::Stop),
        ("POST", "/transport/retry") => Some(CmdKind::Retry),
        _ => None,
    };
    match command {
        // the supervisor answers once the state has changed; returns the new status
        Some(k) => ok(&app.command(k).await),
        None => request(&app, method.as_str(), path, &uri, &body),
    }
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
        // GET /preview.mjpeg comes in phase 3 (not_found until then)
        // the network test comes in phase 4
        ("POST", "/network-tests") => error(StatusCode::CONFLICT, if tx { "busy" } else { "unsupported_role" }),
        (m, p) if p.starts_with("/settings/") || p == "/transport/compare" => {
            if !tx {
                return error(StatusCode::CONFLICT, "unsupported_role");
            }
            settings(app, m, p, body)
        }
        _ => not_found(),
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
        ("GET", "/settings/transport") => ok(&transport_view(&app.lock().settings.transport)),
        ("GET", "/settings/transport/options") => ok(&transport_options(app)),
        ("POST", "/settings/transport/check") => {
            let Some(v) = body_json(body) else { return bad_body() };
            let i = app.lock();
            let profiles = profiles(app);
            ok(&check_transport(&TransportDraft::parse(&v), &transport_ctx(&i, &profiles)))
        }
        ("PUT", "/settings/transport") => put_transport(app, body),
        ("GET", "/transport/compare") => ok(&compare(&app.lock().settings.transport)),
        _ => not_found(),
    }
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
        gap_s: gap_s(app.prof.delay_target_ms),
        cameras: &inputs.cameras,
        files: &inputs.file_paths,
    }
}

fn profiles(app: &App) -> [(DelayProfile, u32, bool); 2] {
    [(DelayProfile::Standard, app.prof.delay_target_ms, true), (DelayProfile::Low, 1500, false)]
}

fn transport_ctx<'a>(i: &'a Inner, profiles: &'a [(DelayProfile, u32, bool)]) -> TransportCtx<'a> {
    TransportCtx { applied: &i.settings.transport, state: i.link.state, profiles }
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
    let bandwidth = app.lock().stats.bandwidth_mbps;
    json!({
        "cameras": [{ "device": cam, "label": media::camera_label(cam), "signal": signal }],
        "files": files,
        "loop_available": false,
        "bitrate_range_mbps": [BITRATE_RANGE.0, BITRATE_RANGE.1],
        "encoding": { "codec": "av1", "resolution": "source", "framerate": "source", "keyframe_s": app.prof.keyframe_s() },
        "bandwidth_mbps": bandwidth,
    })
}

fn transport_view(t: &TransportStored) -> Value {
    json!({
        "version": t.version,
        "mode": t.mode,
        "peer": { "host": t.peer_host, "port": t.peer_port },
        "listen_port": t.listen_port,
        "delay": { "profile": t.delay },
        "encryption": { "cipher": t.cipher, "passphrase_set": false, "passphrase_rejected_at": null },
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

fn put_transport(app: &App, body: &Bytes) -> Response {
    let Some(v) = body_json(body) else { return bad_body() };
    let d = TransportDraft::parse(&v);
    let profiles = profiles(app);
    let mut i = app.lock();
    if d.version != Some(i.settings.transport.version as f64) {
        return error(StatusCode::CONFLICT, "version_conflict");
    }
    if i.link.state != LinkState::Stopped {
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
    i.settings = next;
    dlog!("transport settings {:?}", i.settings.transport);
    app.emit(&mut i, Ev::info("transport_settings_applied"));
    app.send(&json!({ "type": "settings", "section": "transport", "version": i.settings.transport.version }));
    app.push_status(&i);
    ok(&json!({ "settings": transport_view(&i.settings.transport), "applied_at": iso_jst(now_ms()), "impacts": result.impacts }))
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
