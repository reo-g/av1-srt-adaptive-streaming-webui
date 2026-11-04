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
        let (s, _, v) = post(json!({ "kind": "throughput", "mode": "steady", "rate_mbps": 0, "duration_s": 3 })).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(
            v["error"]["errors"],
            json!([
                { "field": "rate_mbps", "code": "out_of_range", "params": { "min": 1.0, "max": 20.0 } },
                { "field": "duration_s", "code": "out_of_range", "params": { "min": 5, "max": 30 } },
            ])
        );
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
            "rtt_min_ms": 0.0, "rtt_avg_ms": 0.01, "rtt_max_ms": 0.03,
        }));
        assert_eq!(v["finished_at"].as_str(), t.finished_at.as_deref());

        // throughput once stopped; meanwhile neither another one nor a transport start
        app.lock().link.state = LinkState::Stopped;
        app.lock().settings.transport.peer_host = "slow-10.0.0.2".into();
        let (s, _, v) = post(json!({ "kind": "throughput", "direction": "from_peer", "duration_s": 5 })).await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        assert_eq!(v["request"], json!({ "kind": "throughput", "direction": "from_peer", "mode": "max", "rate_mbps": null, "duration_s": 5 }));
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
        assert_eq!((r["rtt_ms"].clone(), r["retransmits"].clone()), (Value::Null, json!(0)), "{r}");
        assert_eq!(r["per_second_mbps"].as_array().unwrap().len(), 3);
        assert_eq!(app.lock().events.latest(1)[0].params["direction"], "from_peer");
        let (s, _, v) = call(&app, Method::GET, "/api/v1/network-tests/options", "").await;
        assert_eq!((s, v), (StatusCode::OK, json!({ "steady_rate_mbps": 6.0, "rate_range_mbps": [1.0, 20.0], "duration_range_s": [5, 30] })));
        for p in ["/api/v1/network-tests/nope", "/api/v1/network-tests/"] {
            let (s, _, v) = call(&app, Method::GET, p, "").await;
            assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "not_found"), "{p}");
        }
        assert_eq!(call(&app, Method::DELETE, &format!("/api/v1/network-tests/{id}"), "").await.0, StatusCode::NOT_FOUND);
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
