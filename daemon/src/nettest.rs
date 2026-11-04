//! Network tests (phase 4), started from the UI: つながりの確認 (reachability) and 回線の速さ
//! (throughput). They are not part of the link's state machine; the only coupling is the busy
//! rules (a throughput test needs a stopped link, and the link is not started while one runs).
//!
//! reachability (both roles, any time, one at a time): the echo program (`network_test.ping`) to
//!   the peer and to 途中の無線機 (the applied network settings, else device.toml's hops), all at
//!   once: `ping -n -c 10 -i 0.2 -W 1 -w 4 <addr>` with LC_ALL=C (about 2 s, at most 4 s). Without
//!   `-w` iputils waits only two round trips after the last request once replies came, so a late
//!   last reply was counted lost; `-w 4` waits up to 4 s for ten replies. With a deadline iputils
//!   also goes on sending past the count while replies are missing (`20 packets transmitted` from
//!   a silent address, 11 for one lost reply) and stops early at an error (`6 packets
//!   transmitted, +5 errors`), so the test is the first ten requests: `sent` is always 10 and
//!   `received` the number of distinct requests (icmp_seq 1..10) that got a reply line
//!   (duplicates, `DUP!`, ignored; replies to the extra requests not counted); the summary line
//!   gives the round-trip times. A run with a request missing, a reply slower than twice the
//!   median or a duplicate gets one developer-log line with the missing requests and every time,
//!   so that a late reply and a lost one can be told apart. A run that prints no summary (unknown
//!   host, no route) or does not end in `ping_timeout` counts as nothing back.
//! throughput (tx only, link stopped, one at a time): one saturation test. The video travels in
//!   UDP, so the test does too: the bandwidth tool's client (`network_test.iperf`) sends to its
//!   server on the receiver at more than the line carries (`network_test.saturate_mbps`, default
//!   6), in datagrams of the video's packet payload, and what arrives is the line's speed:
//!   `-c <peer> -p <port> -u -b <saturate>M -l 1316 -t <dur> -i 1 --json-stream --forceflush
//!   --get-server-output --connect-timeout 3000 [-R]` (the tool's control connection is TCP on the
//!   same port). Its stdout has one JSON object a line (`start`, `interval`, `server_output_*`,
//!   `end`, `error`). The client gets SIGTERM `duration + 10 s` after it started (it then tells
//!   the server it is leaving) and SIGKILL 2 s later. A test that lost SATURATED_LOSS_PCT or more
//!   filled the line (`saturated`); otherwise the line is faster than the test.
//! The receiver runs the tool's server (`-s -p <port>`) as a long-lived child (`Server`): its own
//!   process group with PDEATHSIG, stdout and stderr into run_dir/nettest-server.log, started
//!   again with a backoff when it exits, stopped on shutdown. It never looks at the link.
//!
//! Where the tool puts which numbers (iperf3 3.18, checked on localhost; the captures are
//! tests/fixtures/iperf3_udp*.jsonl):
//! - `interval` lines are measured by the client: without -R the sending side (only the rate
//!   sent), with -R the receiving side (the rate that arrived, loss, jitter).
//! - `--get-server-output` adds one line before `end` with the server's own report of the test:
//!   `server_output_text` (the server in its normal text mode, as the receiver runs it: the
//!   per-second lines `[  5]   0.00-1.00   sec   734 KBytes  6.01 Mbits/sec  0.001 ms  0/571 (0%)`
//!   up to a `- - -` line and the summary) or `server_output_json` (a server started with -J:
//!   `intervals[].sum`). Without -R these are the receiving side's seconds.
//! - `end.sum_received` has the rate that arrived, `lost_percent` and `jitter_ms`, measured by the
//!   receiver in both directions; `end.sum_sent.bits_per_second` is the rate sent. `end.sum` is
//!   ambiguous (the sending rate with the receiver's loss and jitter): only a fallback.
//! - So per_second_mbps (what arrived each second) is the server's report without -R and the
//!   client's own intervals with -R. While the test runs only `progress` moves (the client's
//!   seconds so far / duration); the result comes with the end.

use crate::app::App;
use crate::config::{address_ok, Config, Role};
use crate::events::Ev;
use crate::media;
use crate::model::{params, FieldError, LinkState, SEND_LIMIT_RANGE};
use crate::profile::{Env, Spec};
use crate::util::{iso_jst, now_ms, round};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::{mpsc, oneshot};

/// Tests kept for GET /network-tests (memory only).
pub const HISTORY: usize = 20;
/// network_test.saturate_mbps (device.toml only).
pub const SATURATE_RANGE: (f64, f64) = (2.0, 50.0);
pub const DURATION_RANGE: (u32, u32) = (5, 30);
const DEFAULT_DURATION_S: u32 = 10;
/// The video's packet payload (bytes, the sender's live mode): the test's datagrams are as large.
const PAYLOAD: u32 = 1316;
/// A test that lost this share (%) or more filled the line: what arrived is its speed.
pub const SATURATED_LOSS_PCT: f64 = 1.0;
/// A shorter interval (the last sliver of a test) says nothing about the line.
const MIN_INTERVAL_S: f64 = 0.25;
const PING_COUNT: u32 = 10;
/// About how long one echo run takes (10 × 0.2 s and the last answer; with a reply missing it
/// ends at its 4 s deadline).
const PING_EXPECT: Duration = Duration::from_millis(3000);
const SERVER_BACKOFF_S: [u64; 5] = [1, 2, 5, 10, 30];
/// A server that ran this long starts its backoff from the first step again.
const SERVER_STABLE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestKind {
    Reachability,
    Throughput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// This device → the peer (on tx the video's direction).
    ToPeer,
    FromPeer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestState {
    Running,
    Done,
    Failed,
}

/// The API's NetworkTestError.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    PeerUnreachable,
    PeerBusy,
    Failed,
}

/// Why a POST is refused (api.rs makes the response).
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    Invalid(Vec<FieldError>),
    UnsupportedRole,
    Busy,
}

/// Why POST /network-tests/{id}/use is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseRefusal {
    NotFound,
    /// Not a finished throughput test towards the peer.
    NotApplicable,
}

/// The request as it runs (the contract's NetworkTestParams): defaults filled in, null where a
/// field does not apply.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Request {
    pub kind: TestKind,
    pub direction: Option<Direction>,
    pub duration_s: Option<u32>,
}

impl Request {
    const REACHABILITY: Request = Request { kind: TestKind::Reachability, direction: None, duration_s: None };
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Target {
    /// "peer" | "hop"
    pub kind: &'static str,
    pub label: Option<String>,
    pub address: String,
    pub sent: u32,
    pub received: u32,
    pub rtt_min_ms: Option<f64>,
    pub rtt_avg_ms: Option<f64>,
    pub rtt_max_ms: Option<f64>,
}

/// The contract's ThroughputResult.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Throughput {
    /// 届いた速さ: the rate that arrived (end.sum_received).
    pub throughput_mbps: f64,
    pub sent_mbps: f64,
    pub loss_pct: f64,
    pub jitter_ms: f64,
    /// loss_pct ≥ SATURATED_LOSS_PCT: the line was full and throughput_mbps is its speed.
    pub saturated: bool,
    /// What arrived in each second.
    pub per_second_mbps: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Outcome {
    Reachability { targets: Vec<Target> },
    Throughput(Throughput),
}

/// The contract's NetworkTest.
#[derive(Debug, Clone, Serialize)]
pub struct NetworkTest {
    pub id: String,
    pub kind: TestKind,
    pub state: TestState,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub progress: f64,
    pub request: Request,
    pub result: Option<Outcome>,
    pub error: Option<Failure>,
}

/// How long the programs may take (shorter in the tests).
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// The client gets SIGTERM `duration × per_second + grace` after it started.
    pub per_second: Duration,
    pub grace: Duration,
    /// SIGKILL this long after SIGTERM.
    pub kill_after: Duration,
    /// One echo run that has not ended by then is stopped (nothing back).
    pub ping_timeout: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            per_second: Duration::from_secs(1),
            grace: Duration::from_secs(10),
            kill_after: Duration::from_secs(2),
            ping_timeout: Duration::from_secs(10),
        }
    }
}

// ------------------------------------------------------------------ requests

fn fe(field: &'static str, code: &'static str, p: Value) -> FieldError {
    FieldError { field: field.into(), code, params: params(p) }
}

/// A POST body, checked in the contract's order: kind → role → fields. Missing or null fields
/// take the defaults; fields of the old modes (mode, rate_mbps) are ignored.
pub fn parse(v: &Value, role: Role, enabled: bool) -> Result<Request, Refusal> {
    let field = |k: &str| v.get(k).filter(|x| !x.is_null());
    let kind = match field("kind").map(Value::as_str) {
        Some(Some("reachability")) => TestKind::Reachability,
        Some(Some("throughput")) => TestKind::Throughput,
        None => return Err(Refusal::Invalid(vec![fe("kind", "required", json!({}))])),
        Some(_) => return Err(Refusal::Invalid(vec![fe("kind", "invalid", json!({}))])),
    };
    // tests turned off in device.toml: this device does not offer them
    if !enabled || (kind == TestKind::Throughput && role != Role::Tx) {
        return Err(Refusal::UnsupportedRole);
    }
    if kind == TestKind::Reachability {
        return Ok(Request::REACHABILITY);
    }
    let mut errors = Vec::new();
    let direction = match field("direction").map(Value::as_str) {
        None => Some(Direction::ToPeer),
        Some(Some("to_peer")) => Some(Direction::ToPeer),
        Some(Some("from_peer")) => Some(Direction::FromPeer),
        Some(_) => {
            errors.push(fe("direction", "invalid", json!({})));
            None
        }
    };
    let (dmin, dmax) = DURATION_RANGE;
    let duration_s = match field("duration_s") {
        None => Some(DEFAULT_DURATION_S),
        Some(d) => match d.as_f64().filter(|d| d.fract() == 0.0 && (f64::from(dmin)..=f64::from(dmax)).contains(d)) {
            Some(d) => Some(d as u32),
            None => {
                errors.push(fe("duration_s", "out_of_range", json!({ "min": dmin, "max": dmax })));
                None
            }
        },
    };
    if !errors.is_empty() {
        return Err(Refusal::Invalid(errors));
    }
    Ok(Request { kind, direction, duration_s })
}

/// The client's command line (after the program): UDP at `saturate_mbps`.
pub fn client_args(peer: &str, port: u16, saturate_mbps: f64, r: &Request) -> Vec<String> {
    let duration = r.duration_s.unwrap_or(DEFAULT_DURATION_S).to_string();
    let mut a: Vec<String> = vec!["-c".into(), peer.into(), "-p".into(), port.to_string()];
    a.extend(["-u".into(), "-b".into(), format!("{saturate_mbps}M"), "-l".into(), PAYLOAD.to_string()]);
    a.extend(["-t".into(), duration]);
    a.extend(["-i", "1", "--json-stream", "--forceflush", "--get-server-output", "--connect-timeout", "3000"].map(String::from));
    if r.direction == Some(Direction::FromPeer) {
        a.push("-R".into());
    }
    a
}

/// Per reply lines (no -q) and a 4 s deadline for all ten (see the module comment).
fn ping_args(addr: &str) -> Vec<String> {
    let mut a: Vec<String> = ["-n", "-c", "10", "-i", "0.2", "-W", "1", "-w", "4"].map(String::from).to_vec();
    debug_assert_eq!(a[2], PING_COUNT.to_string());
    a.push(addr.to_string());
    a
}

// ------------------------------------------------------------------ parsing the programs' output

#[derive(Debug, Clone, PartialEq)]
pub struct PingSummary {
    /// The test's requests: always PING_COUNT (see parse_ping).
    pub sent: u32,
    /// Distinct requests (1..=sent) that got a reply; the summary's count when the output has no
    /// reply lines.
    pub received: u32,
    /// min / avg / max in ms (only when something came back)
    pub rtt: Option<(f64, f64, f64)>,
    /// (icmp_seq, ms) of the first reply to each request 1..=sent, by seq.
    pub replies: Vec<(u32, f64)>,
    /// Requests 1..=sent that got more than one reply (`DUP!`), by seq.
    pub dups: Vec<u32>,
    /// (icmp_seq, ms) of the replies to requests past the count (sent because of -w), by seq.
    pub extra: Vec<(u32, f64)>,
}

/// One reply line (iputils, LC_ALL=C), in the order the replies arrived:
///   64 bytes from 192.168.0.56: icmp_seq=4 ttl=64 time=612 ms
///   64 bytes from 192.168.0.56: icmp_seq=3 ttl=64 time=1.02 ms (DUP!)
/// busybox numbers its requests from 0 (`seq=0`): taken as 1.. as well. Error lines ("From …
/// icmp_seq=3 Destination Host Unreachable") have no time and are not replies.
fn reply_line(line: &str) -> Option<(u32, f64)> {
    if !line.contains(" bytes from ") {
        return None;
    }
    let value = |key: &str| {
        let (_, rest) = line.split_once(key)?;
        rest.split(|c: char| c.is_whitespace()).next()
    };
    let seq = match value(" icmp_seq=") {
        Some(q) => q.parse::<u32>().ok()?,
        None => value(" seq=")?.parse::<u32>().ok()? + 1,
    };
    let ms = value(" time=")?.parse::<f64>().ok()?;
    Some((seq, ms))
}

/// The output of `ping` without -q (iputils, LC_ALL=C): the reply lines (reply_line), then
///   11 packets transmitted, 10 received, 9.09091% packet loss, time 2003ms   (also "+N errors", "+N duplicates")
///   rtt min/avg/max/mdev = 74.600/128.890/612.000/161.037 ms                  (busybox: "round-trip min/avg/max = …")
/// The test is the first PING_COUNT requests. With a deadline (-w) iputils does not stop sending
/// at the count while replies are missing: it goes on until it has that many replies or the
/// deadline passes (a silent address: 20 transmitted in 4 s; one lost reply: an 11th request whose
/// reply ends the run), and an error ends it early (6 transmitted, +5 errors). So `sent` is
/// PING_COUNT whatever the summary says, `received` counts the requests 1..=PING_COUNT answered,
/// and the replies to the extra requests are kept apart (`extra`, for the log line only). The
/// round-trip times are the summary's (the extra replies are samples of the same path).
pub fn parse_ping(out: &str) -> Option<PingSummary> {
    let mut counts = None;
    let mut rtt = None;
    let mut replies: Vec<(u32, f64)> = Vec::new();
    let mut dups: Vec<u32> = Vec::new();
    for line in out.lines().map(str::trim) {
        if let Some((seq, ms)) = reply_line(line) {
            if replies.iter().any(|r| r.0 == seq) {
                if !dups.contains(&seq) {
                    dups.push(seq);
                }
            } else {
                replies.push((seq, ms));
            }
        } else if let Some((sent, rest)) = line.split_once(" packets transmitted, ") {
            let received = rest.split([' ', ',']).next()?.parse::<u32>().ok()?;
            counts = Some((sent.trim().parse::<u32>().ok()?, received));
        } else if let Some((name, values)) = line.split_once(" = ") {
            if ["min/avg/max/mdev", "min/avg/max", "min/avg/max/stddev"].iter().any(|k| name.ends_with(k)) {
                let v: Vec<f64> =
                    values.trim_end_matches("ms").split('/').filter_map(|x| x.trim().parse().ok()).collect();
                if v.len() >= 3 {
                    rtt = Some((v[0], v[1], v[2]));
                }
            }
        }
    }
    let (_transmitted, summary_received) = counts?;
    let sent = PING_COUNT;
    let had_replies = !replies.is_empty();
    let (mut replies, mut extra): (Vec<(u32, f64)>, Vec<(u32, f64)>) =
        replies.into_iter().partition(|r| (1..=sent).contains(&r.0));
    replies.sort_by_key(|r| r.0);
    extra.sort_by_key(|r| r.0);
    dups.retain(|q| (1..=sent).contains(q));
    dups.sort_unstable();
    let received = if had_replies { replies.len() as u32 } else { summary_received.min(sent) };
    Some(PingSummary { sent, received, rtt: rtt.filter(|_| received > 0), replies, dups, extra })
}

/// The developer-log line for a run with a request missing, a reply slower than twice the median
/// or a duplicate (None for a clean run, or output without reply lines that says nothing more):
///   reach 192.168.0.56: 9/10 missing seq [7] late seq [4] rtt_ms [75.2, 74.8, 76.1, 612.0, …] extra seq [11] extra_rtt_ms [75.1]
/// rtt_ms lists every reply's time by seq (the first reply of each request 1..10); the replies to
/// requests past the count come last (an answered extra request after a missing one: the line was
/// up, so that one was lost, or later than the extra reply).
pub fn reply_note(addr: &str, s: &PingSummary) -> Option<String> {
    if s.replies.is_empty() && s.received > 0 {
        return None;
    }
    let missing: Vec<u32> = (1..=s.sent).filter(|q| !s.replies.iter().any(|r| r.0 == *q)).collect();
    let mut times: Vec<f64> = s.replies.iter().map(|r| r.1).collect();
    times.sort_by(f64::total_cmp);
    let median = match times.len() {
        0 => 0.0,
        n if n % 2 == 1 => times[n / 2],
        n => (times[n / 2 - 1] + times[n / 2]) / 2.0,
    };
    let late: Vec<u32> = s.replies.iter().filter(|r| r.1 > 2.0 * median).map(|r| r.0).collect();
    if missing.is_empty() && late.is_empty() && s.dups.is_empty() {
        return None;
    }
    let mut line = format!("reach {addr}: {}/{} missing seq {missing:?}", s.received, s.sent);
    if !late.is_empty() {
        line.push_str(&format!(" late seq {late:?}"));
    }
    if !s.dups.is_empty() {
        line.push_str(&format!(" dup seq {:?}", s.dups));
    }
    let rtts: Vec<f64> = s.replies.iter().map(|r| r.1).collect();
    line.push_str(&format!(" rtt_ms {rtts:?}"));
    if !s.extra.is_empty() {
        let seqs: Vec<u32> = s.extra.iter().map(|r| r.0).collect();
        let times: Vec<f64> = s.extra.iter().map(|r| r.1).collect();
        line.push_str(&format!(" extra seq {seqs:?} extra_rtt_ms {times:?}"));
    }
    Some(line)
}

/// What the client's JSON-stream lines said so far.
#[derive(Debug, Default)]
pub struct Stream {
    /// -R: the client receives, so its own seconds are what arrived.
    reverse: bool,
    /// A `start` line came: the client reached the server and the test began.
    pub connected: bool,
    /// Seconds measured so far (the end of the client's last interval).
    pub elapsed_s: f64,
    /// The client's seconds: the rate sent without -R, the rate that arrived with -R.
    pub client_seconds: Vec<f64>,
    /// The server's seconds from its report (--get-server-output): what arrived without -R.
    pub server_seconds: Vec<f64>,
    pub result: Option<Throughput>,
    /// The tool's own message from an `error` line.
    pub error: Option<String>,
}

impl Stream {
    pub fn new(reverse: bool) -> Stream {
        Stream { reverse, ..Stream::default() }
    }

    /// One stdout line; true when the client measured another second.
    pub fn line(&mut self, line: &str) -> bool {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return false };
        let data = &v["data"];
        match v["event"].as_str() {
            Some("start") => self.connected = true,
            Some("interval") => {
                let sum = &data["sum"];
                if sum["omitted"].as_bool() == Some(true) {
                    return false;
                }
                if let Some(end) = sum["end"].as_f64() {
                    self.elapsed_s = end;
                }
                if let Some(mbps) = interval_mbps(sum) {
                    self.client_seconds.push(mbps);
                    return true;
                }
            }
            Some("server_output_text") => self.server_seconds = server_text_seconds(data.as_str().unwrap_or_default()),
            Some("server_output_json") => {
                self.server_seconds = data["intervals"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|iv| &iv["sum"])
                    .filter(|sum| sum["omitted"].as_bool() != Some(true))
                    .filter_map(interval_mbps)
                    .collect()
            }
            Some("end") => {
                if let Some(r) = summary(data) {
                    self.result = Some(r);
                }
            }
            Some("error") => self.error = Some(data.as_str().unwrap_or_default().to_string()),
            _ => {}
        }
        false
    }

    /// What arrived in each second: the server's report without -R, the client's own with -R.
    pub fn received_seconds(&self) -> &[f64] {
        if self.reverse {
            &self.client_seconds
        } else {
            &self.server_seconds
        }
    }
}

/// Mbps of one JSON interval (`bits_per_second` over `seconds`); None for a last sliver.
fn interval_mbps(sum: &Value) -> Option<f64> {
    let secs = sum["seconds"].as_f64().unwrap_or(0.0);
    sum["bits_per_second"].as_f64().filter(|_| secs >= MIN_INTERVAL_S).map(|bps| round(bps / 1e6, 2))
}

/// The per-second rates of the server's text report (iperf3 3.18 / 3.20), Mbps:
///   [  5]   0.00-1.00   sec   734 KBytes  6.01 Mbits/sec  0.001 ms  0/571 (0%)
///   [  5]   0.00-1.00   sec   734 KBytes  6.01 Mbits/sec  571            (the sending side)
/// up to the `- - -` line before the summary; the `[SUM]` lines instead when there are several
/// streams. Rates are in bits (K/M/G = 1000s); a last sliver of an interval is left out.
pub fn server_text_seconds(text: &str) -> Vec<f64> {
    let (mut streams, mut sums) = (Vec::new(), Vec::new());
    for line in text.lines().map(str::trim) {
        if line.starts_with("- -") {
            break;
        }
        let Some((id, rest)) = line.strip_prefix('[').and_then(|l| l.split_once(']')) else { continue };
        let id = id.trim();
        let is_sum = id == "SUM";
        if !is_sum && id.parse::<u32>().is_err() {
            continue;
        }
        let t: Vec<&str> = rest.split_whitespace().collect();
        if t.len() < 4 || t[1] != "sec" {
            continue;
        }
        let Some((a, b)) = t[0].split_once('-') else { continue };
        let (Ok(a), Ok(b)) = (a.parse::<f64>(), b.parse::<f64>()) else { continue };
        let Some(i) = t.iter().position(|w| w.ends_with("bits/sec")).filter(|i| *i > 2) else { continue };
        let scale = match t[i].strip_suffix("bits/sec") {
            Some("") => 1.0,
            Some("K") => 1e3,
            Some("M") => 1e6,
            Some("G") => 1e9,
            Some("T") => 1e12,
            _ => continue,
        };
        let Ok(rate) = t[i - 1].parse::<f64>() else { continue };
        // the times have two decimals: 3.00-3.23 is a sliver of 0.23 s
        if b - a + 1e-6 < MIN_INTERVAL_S {
            continue;
        }
        let mbps = round(rate * scale / 1e6, 2);
        if is_sum {
            sums.push(mbps);
        } else {
            streams.push(mbps);
        }
    }
    if sums.is_empty() {
        streams
    } else {
        sums
    }
}

/// The result from the `end` line (see the module comment for the fields); per_second_mbps is
/// filled in by the caller.
fn summary(end: &Value) -> Option<Throughput> {
    let got = if end["sum_received"].is_object() { &end["sum_received"] } else { &end["sum"] };
    let bps = got["bits_per_second"].as_f64()?;
    let sent = end["sum_sent"]["bits_per_second"].as_f64().or_else(|| end["sum"]["bits_per_second"].as_f64()).unwrap_or(bps);
    let loss = got["lost_percent"]
        .as_f64()
        .or_else(|| {
            let (lost, all) = (got["lost_packets"].as_f64()?, got["packets"].as_f64()?);
            (all > 0.0).then(|| 100.0 * lost / all)
        })
        .unwrap_or(0.0);
    let loss_pct = round(loss, 2);
    Some(Throughput {
        throughput_mbps: round(bps / 1e6, 2),
        sent_mbps: round(sent / 1e6, 2),
        loss_pct,
        jitter_ms: round(got["jitter_ms"].as_f64().unwrap_or(0.0), 2),
        saturated: loss_pct >= SATURATED_LOSS_PCT,
        per_second_mbps: Vec::new(),
    })
}

/// The tool's error message → the API's error code.
pub fn classify(msg: &str) -> Failure {
    let m = msg.to_ascii_lowercase();
    if m.contains("server is busy") {
        Failure::PeerBusy
    } else if [
        "unable to connect",
        "connection refused",
        "timed out",
        "no route",
        "unreachable",
        "resolve",
        "name or service not known",
    ]
    .iter()
    .any(|k| m.contains(k))
    {
        Failure::PeerUnreachable
    } else {
        Failure::Failed
    }
}

// ------------------------------------------------------------------ the tests kept

struct Record {
    t: NetworkTest,
    started: Instant,
}

impl Record {
    fn view(&self, now: Instant) -> NetworkTest {
        let mut t = self.t.clone();
        if t.state == TestState::Running && t.kind == TestKind::Reachability {
            // the echo runs say nothing until they end: an estimate from the time
            let est = now.saturating_duration_since(self.started).as_secs_f64() / PING_EXPECT.as_secs_f64();
            t.progress = round(t.progress.max(est.min(0.95)), 2);
        }
        t
    }

    fn finish(&mut self, state: TestState, result: Option<Outcome>, error: Option<Failure>) {
        self.t.state = state;
        self.t.finished_at = Some(iso_jst(now_ms()));
        if state == TestState::Done {
            self.t.progress = 1.0;
        }
        self.t.result = result;
        self.t.error = error;
    }
}

#[derive(Default)]
struct Book {
    /// Newest first.
    items: VecDeque<Record>,
    seq: u64,
    /// Transport start requests on their way to the supervisor (a throughput test waits for none).
    starting: usize,
}

impl Book {
    fn running(&self, kind: TestKind) -> bool {
        self.items.iter().any(|r| r.t.kind == kind && r.t.state == TestState::Running)
    }

    /// The oldest finished tests make room; a running one is never dropped.
    fn add(&mut self, r: Record) {
        self.items.push_front(r);
        while self.items.len() > HISTORY {
            match self.items.iter().rposition(|r| r.t.state != TestState::Running) {
                Some(i) => {
                    self.items.remove(i);
                }
                None => break,
            }
        }
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut Record> {
        self.items.iter_mut().find(|r| r.t.id == id)
    }
}

/// Held while a transport start is on its way to the supervisor (api.rs).
pub struct StartHold<'a>(&'a NetTests);

impl Drop for StartHold<'_> {
    fn drop(&mut self) {
        let mut b = self.0.book();
        b.starting = b.starting.saturating_sub(1);
    }
}

/// The network tests of this device (in App).
pub struct NetTests {
    enabled: bool,
    port: u16,
    saturate_mbps: f64,
    rx_peer: Option<String>,
    /// The programs: args empty (made per run), the environment with LC_ALL=C, cwd run_dir.
    ping: Spec,
    iperf: Spec,
    pub timing: Timing,
    boot: i64,
    book: Mutex<Book>,
    server: Server,
}

/// What every program of the tests gets: the inherited variables that pass (media.rs) and the C locale.
fn program(cfg: &Config, path: &std::path::Path, args: Vec<String>) -> Spec {
    let mut env = Env::new();
    env.insert("LC_ALL".into(), "C".into());
    let env = media::child_env(media::inherited(), &cfg.media.pass_env, &env);
    Spec { program: path.to_path_buf(), args, env, cwd: cfg.run_dir() }
}

impl NetTests {
    pub fn new(cfg: &Config, server: Server) -> NetTests {
        let n = &cfg.network_test;
        NetTests {
            enabled: n.enabled,
            port: n.port,
            saturate_mbps: n.saturate_mbps,
            rx_peer: cfg.rx.peer_host.clone(),
            ping: program(cfg, &n.ping, Vec::new()),
            iperf: program(cfg, &n.iperf, Vec::new()),
            timing: Timing::default(),
            boot: now_ms() / 1000,
            book: Mutex::new(Book::default()),
            server,
        }
    }

    fn book(&self) -> MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// GET /network-tests/options
    pub fn options(&self) -> Value {
        json!({
            "duration_range_s": [DURATION_RANGE.0, DURATION_RANGE.1],
            "saturate_mbps": self.saturate_mbps,
        })
    }

    /// POST /network-tests/{id}/use: the send cap a finished throughput test towards the peer
    /// gives, and when it finished. A test that did not fill the line says only that the line
    /// carries the test's rate: then that rate. Kept within the send cap's range.
    pub fn send_limit_from(&self, id: &str) -> Result<(f64, String), UseRefusal> {
        let book = self.book();
        let t = &book.items.iter().find(|r| r.t.id == id).ok_or(UseRefusal::NotFound)?.t;
        match (t.kind, t.state, t.request.direction, &t.result, &t.finished_at) {
            (TestKind::Throughput, TestState::Done, Some(Direction::ToPeer), Some(Outcome::Throughput(r)), Some(at)) => {
                let mbps = if r.saturated { r.throughput_mbps } else { self.saturate_mbps };
                let (lo, hi) = SEND_LIMIT_RANGE;
                Ok((round(mbps, 2).clamp(lo, hi), at.clone()))
            }
            _ => Err(UseRefusal::NotApplicable),
        }
    }

    /// Newest first.
    pub fn list(&self) -> Vec<NetworkTest> {
        let now = Instant::now();
        self.book().items.iter().map(|r| r.view(now)).collect()
    }

    pub fn get(&self, id: &str) -> Option<NetworkTest> {
        let now = Instant::now();
        self.book().items.iter().find(|r| r.t.id == id).map(|r| r.view(now))
    }

    #[cfg(test)]
    pub fn throughput_running(&self) -> bool {
        self.book().running(TestKind::Throughput)
    }

    /// None while a throughput test runs (the start is refused); else the start may go on, and
    /// no throughput test starts until the guard is dropped.
    pub fn hold_start(&self) -> Option<StartHold<'_>> {
        let mut b = self.book();
        if b.running(TestKind::Throughput) {
            return None;
        }
        b.starting += 1;
        Some(StartHold(self))
    }

    /// Stop the bandwidth tool's server (shutdown); resolves once it has exited.
    pub fn stop_server(&self) -> oneshot::Receiver<()> {
        self.server.stop()
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Record)) {
        if let Some(r) = self.book().get_mut(id) {
            f(r);
        }
    }

    fn finish(&self, id: &str, state: TestState, result: Option<Outcome>, error: Option<Failure>) {
        self.update(id, |r| r.finish(state, result, error));
    }
}

/// One address to check.
#[derive(Debug, Clone)]
struct Aim {
    kind: &'static str,
    label: Option<String>,
    address: String,
}

impl Aim {
    fn target(self, s: Option<PingSummary>) -> Target {
        let s = s.unwrap_or(PingSummary {
            sent: PING_COUNT,
            received: 0,
            rtt: None,
            replies: Vec::new(),
            dups: Vec::new(),
            extra: Vec::new(),
        });
        let ms = |v: f64| round(v, 2);
        Target {
            kind: self.kind,
            label: self.label,
            address: self.address,
            sent: s.sent,
            received: s.received,
            rtt_min_ms: s.rtt.map(|r| ms(r.0)),
            rtt_avg_ms: s.rtt.map(|r| ms(r.1)),
            rtt_max_ms: s.rtt.map(|r| ms(r.2)),
        }
    }
}

/// POST /network-tests: checks the request and the busy rules, keeps the test and starts it.
pub fn start(app: &Arc<App>, body: &Value) -> Result<NetworkTest, Refusal> {
    let n = &app.nettest;
    let req = parse(body, app.cfg.role, n.enabled)?;
    // Read from the link before the book is locked (the two locks are never held together). The
    // link leaves `stopped` only by a start, and a start holds the book's `starting` count. The
    // hops are the list applied now (a later change does not touch a running test).
    let (state, peer, hops) = {
        let i = app.lock();
        let peer = match app.cfg.role {
            Role::Tx => Some(i.settings.transport.peer_host.clone()),
            Role::Rx => n.rx_peer.clone().or_else(|| i.link.peer_addr.as_ref().map(|(a, _)| a.clone())),
        };
        (i.link.state, peer.filter(|p| !p.is_empty()), app.network(&i).hops)
    };
    let now = Instant::now();
    let mut book = n.book();
    let busy = match req.kind {
        TestKind::Throughput => state != LinkState::Stopped || book.starting > 0 || book.running(TestKind::Throughput),
        TestKind::Reachability => book.running(TestKind::Reachability),
    };
    if busy {
        return Err(Refusal::Busy);
    }
    book.seq += 1;
    let id = format!("{}-{}", n.boot, book.seq);
    let mut rec = Record {
        t: NetworkTest {
            id: id.clone(),
            kind: req.kind,
            state: TestState::Running,
            started_at: iso_jst(now_ms()),
            finished_at: None,
            progress: 0.0,
            request: req.clone(),
            result: None,
            error: None,
        },
        started: now,
    };
    let mut run: Option<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>> = None;
    match req.kind {
        TestKind::Reachability => {
            let mut aims: Vec<Aim> = Vec::new();
            match peer {
                Some(p) if address_ok(&p) => aims.push(Aim { kind: "peer", label: None, address: p }),
                Some(p) => dlog!("network test {id}: the peer's address {p:?} is not usable; left out"),
                None => {}
            }
            aims.extend(hops.into_iter().map(|h| Aim { kind: "hop", label: Some(h.label), address: h.address }));
            let list: Vec<&str> = aims.iter().map(|a| a.address.as_str()).collect();
            let what = if list.is_empty() { "nothing (no address known)".to_string() } else { list.join(", ") };
            dlog!("network test {id}: reachability of {what}");
            if aims.is_empty() {
                rec.finish(TestState::Done, Some(Outcome::Reachability { targets: Vec::new() }), None);
            } else {
                run = Some(Box::pin(run_reachability(app.clone(), id.clone(), aims)));
            }
        }
        TestKind::Throughput => {
            let peer = peer.unwrap_or_default();
            if address_ok(&peer) {
                let args = client_args(&peer, n.port, n.saturate_mbps, &req);
                dlog!("network test {id}: throughput {} {}", n.iperf.program.display(), args.join(" "));
                run = Some(Box::pin(run_throughput(app.clone(), id.clone(), req.clone(), args)));
            } else {
                dlog!("network test {id}: the receiver's address {peer:?} is not usable");
                rec.finish(TestState::Failed, None, Some(Failure::Failed));
            }
        }
    }
    let view = rec.view(now);
    book.add(rec);
    drop(book);
    if let Some(run) = run {
        // on the daemon's single runtime thread: the programs are forked from it (PDEATHSIG)
        tokio::spawn(run);
    }
    Ok(view)
}

// ------------------------------------------------------------------ running the programs

/// The program in its own process group with PDEATHSIG (media.rs), stdout and stderr piped.
fn command(s: &Spec, args: &[String]) -> tokio::process::Command {
    let mut cmd = std::process::Command::new(&s.program);
    cmd.args(args)
        .env_clear()
        .envs(&s.env)
        .current_dir(&s.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let parent = std::process::id();
    // SAFETY: child_setup only makes async-signal-safe calls.
    unsafe {
        cmd.pre_exec(move || media::child_setup(parent, &[]));
    }
    cmd.process_group(0);
    tokio::process::Command::from(cmd)
}

fn spawn(s: &Spec, args: &[String]) -> io::Result<tokio::process::Child> {
    command(s, args).spawn().map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", s.program.display())))
}

fn signal(pid: i32, sig: i32) {
    if pid > 0 {
        // SAFETY: signals the program's own process group (process_group(0) → pgid = pid).
        unsafe { libc::kill(-pid, sig) };
    }
}

async fn sleep_until(t: Option<Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t.into()).await,
        None => std::future::pending().await,
    }
}

/// Lines of a pipe into a channel, until its end.
async fn lines_to(r: impl AsyncRead + Unpin, tx: mpsc::UnboundedSender<String>) {
    let mut lines = BufReader::new(r).lines();
    while let Ok(Some(l)) = lines.next_line().await {
        if tx.send(l).is_err() {
            break;
        }
    }
}

/// One echo run; None when it printed no summary or did not end in time.
async fn ping(spec: Spec, addr: String, timeout: Duration) -> io::Result<Option<PingSummary>> {
    let child = spawn(&spec, &ping_args(&addr))?;
    let pid = child.id().map_or(0, |p| p as i32);
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => {
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                dlog!("reachability {addr}: {}", err.trim().replace('\n', " / "));
            }
            let s = parse_ping(&String::from_utf8_lossy(&out.stdout));
            match &s {
                Some(s) => {
                    if let Some(note) = reply_note(&addr, s) {
                        dlog!("{note}");
                    }
                }
                None => {
                    let code = out.status.code().map_or("signal".into(), |c| c.to_string());
                    dlog!("reachability {addr}: no summary (exit {code})");
                }
            }
            Ok(s)
        }
        Ok(Err(e)) => Err(e),
        Err(_) => {
            signal(pid, libc::SIGKILL);
            dlog!("reachability {addr}: not done in {timeout:?}; stopped");
            Ok(None)
        }
    }
}

async fn run_reachability(app: Arc<App>, id: String, aims: Vec<Aim>) {
    let n = &app.nettest;
    let jobs: Vec<_> =
        aims.iter().map(|a| tokio::spawn(ping(n.ping.clone(), a.address.clone(), n.timing.ping_timeout))).collect();
    let mut targets = Vec::new();
    let mut broken = None;
    for (a, job) in aims.into_iter().zip(jobs) {
        match job.await {
            Ok(Ok(s)) => targets.push(a.target(s)),
            Ok(Err(e)) => {
                broken.get_or_insert(e.to_string());
            }
            Err(e) => {
                broken.get_or_insert(e.to_string());
            }
        }
    }
    match broken {
        Some(e) => {
            dlog!("network test {id}: failed: {e}");
            n.finish(&id, TestState::Failed, None, Some(Failure::Failed));
        }
        None => {
            let summary: Vec<String> =
                targets.iter().map(|t| format!("{} {}/{}", t.address, t.received, t.sent)).collect();
            dlog!("network test {id}: done: {}", summary.join(", "));
            n.finish(&id, TestState::Done, Some(Outcome::Reachability { targets }), None);
        }
    }
}

async fn run_throughput(app: Arc<App>, id: String, req: Request, args: Vec<String>) {
    let n = &app.nettest;
    let t = n.timing;
    let duration_s = req.duration_s.unwrap_or(DEFAULT_DURATION_S);
    let deadline = Instant::now() + t.per_second * duration_s + t.grace;
    let mut child = match spawn(&n.iperf, &args) {
        Ok(c) => c,
        Err(e) => {
            dlog!("network test {id}: cannot start: {e}");
            n.finish(&id, TestState::Failed, None, Some(Failure::Failed));
            return;
        }
    };
    let pid = child.id().map_or(0, |p| p as i32);
    let (tx, mut out) = mpsc::unbounded_channel();
    if let Some(so) = child.stdout.take() {
        tokio::spawn(lines_to(so, tx));
    }
    if let Some(se) = child.stderr.take() {
        let (etx, mut erx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(lines_to(se, etx));
        let id = id.clone();
        tokio::spawn(async move {
            while let Some(l) = erx.recv().await {
                dlog!("network test {id}: {l}");
            }
        });
    }
    let mut s = Stream::new(req.direction == Some(Direction::FromPeer));
    let mut timed_out = false;
    let mut kill_at = None;
    // until stdout ends (the client is leaving), with SIGTERM at the deadline and SIGKILL after
    loop {
        let wake = if timed_out { kill_at } else { Some(deadline) };
        tokio::select! {
            l = out.recv() => match l {
                Some(l) => {
                    let had_error = s.error.is_some();
                    let measured = s.line(&l);
                    if let Some(e) = s.error.as_deref().filter(|_| !had_error) {
                        dlog!("network test {id}: {e}");
                    }
                    if measured {
                        // only the progress: the client's own seconds are the rate sent (without -R)
                        let progress = round((s.elapsed_s / f64::from(duration_s)).clamp(0.0, 0.99), 2);
                        n.update(&id, |r| r.t.progress = progress);
                    }
                }
                None => break,
            },
            _ = sleep_until(wake) => {
                if timed_out {
                    signal(pid, libc::SIGKILL);
                    kill_at = None;
                } else {
                    let after = t.per_second * duration_s + t.grace;
                    dlog!("network test {id}: not done {after:?} after it started; stopping it");
                    timed_out = true;
                    signal(pid, libc::SIGTERM);
                    kill_at = Some(Instant::now() + t.kill_after);
                }
            }
        }
    }
    let status = match tokio::time::timeout(t.kill_after, child.wait()).await {
        Ok(st) => st.ok(),
        Err(_) => {
            signal(pid, libc::SIGKILL);
            child.wait().await.ok()
        }
    };
    let code = status.and_then(|s| s.code().or_else(|| s.signal().map(|g| 128 + g)));
    let outcome = match s.result.take() {
        Some(mut r) => {
            r.per_second_mbps = s.received_seconds().to_vec();
            if r.per_second_mbps.is_empty() {
                dlog!("network test {id}: no per-second report from the receiving side");
            }
            Ok(r)
        }
        // never reached the server in time (no answer, or a path that drops the test's packets)
        None if timed_out && !s.connected => Err(Failure::PeerUnreachable),
        None if timed_out => Err(Failure::Failed),
        None => Err(s.error.as_deref().map_or(Failure::Failed, classify)),
    };
    match outcome {
        Ok(r) => {
            let mbps = r.throughput_mbps;
            dlog!(
                "network test {id}: done: {mbps} Mbps arrived, {} sent, loss {} %, jitter {} ms, saturated {} (exit {})",
                r.sent_mbps,
                r.loss_pct,
                r.jitter_ms,
                r.saturated,
                code.map_or("?".into(), |c| c.to_string())
            );
            n.finish(&id, TestState::Done, Some(Outcome::Throughput(r)), None);
            // what arrived, two decimals as in the result (the event list shows 届いた速さ 5.26 Mbps)
            let ev = Ev::info("network_test_done").with(json!({ "mbps": mbps, "direction": req.direction }));
            app.emit(&mut app.lock(), ev);
        }
        Err(f) => {
            dlog!("network test {id}: failed ({f:?}, exit {})", code.map_or("?".into(), |c| c.to_string()));
            n.finish(&id, TestState::Failed, None, Some(f));
        }
    }
}

// ------------------------------------------------------------------ the server (rx)

/// How the bandwidth tool's server is run.
#[derive(Debug, Clone)]
pub struct ServerLaunch {
    pub spec: Spec,
    pub log: std::path::PathBuf,
    pub backoff: Vec<Duration>,
    pub stable_after: Duration,
    pub kill_after: Duration,
}

impl ServerLaunch {
    /// None when this device does not run it (`network_test.server`, default only on rx).
    pub fn from_config(cfg: &Config) -> Option<ServerLaunch> {
        let n = &cfg.network_test;
        if !n.runs_server(cfg.role) {
            return None;
        }
        Some(ServerLaunch {
            spec: program(cfg, &n.iperf, vec!["-s".into(), "-p".into(), n.port.to_string()]),
            log: cfg.nettest_server_log(),
            backoff: SERVER_BACKOFF_S.iter().map(|s| Duration::from_secs(*s)).collect(),
            stable_after: SERVER_STABLE,
            kill_after: Duration::from_secs(2),
        })
    }
}

/// The server's manager (a task of the runtime thread); the handle only stops it.
#[derive(Clone)]
pub struct Server {
    tx: Option<mpsc::UnboundedSender<oneshot::Sender<()>>>,
}

impl Server {
    pub fn start(launch: Option<ServerLaunch>) -> Server {
        let Some(l) = launch else { return Server { tx: None } };
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(serve(l, rx));
        Server { tx: Some(tx) }
    }

    /// SIGTERM to the server's group (SIGKILL after kill_after) and no restart; resolves once it
    /// has exited.
    pub fn stop(&self) -> oneshot::Receiver<()> {
        let (done, rx) = oneshot::channel();
        match &self.tx {
            Some(tx) => {
                if let Err(mpsc::error::SendError(done)) = tx.send(done) {
                    let _ = done.send(());
                }
            }
            None => {
                let _ = done.send(());
            }
        }
        rx
    }
}

async fn serve(l: ServerLaunch, mut reqs: mpsc::UnboundedReceiver<oneshot::Sender<()>>) {
    let mut waiters = Vec::new();
    let mut fails = 0usize;
    let mut open = true;
    loop {
        let started = Instant::now();
        let mut stopping = false;
        match start_server(&l) {
            Ok((pid, mut exit)) => {
                dlog!("network test server: started, pid {pid}");
                let mut kill_at = None;
                let code = loop {
                    tokio::select! {
                        c = &mut exit => break c.ok().flatten(),
                        r = reqs.recv(), if open => {
                            match r {
                                Some(w) => waiters.push(w),
                                None => open = false,
                            }
                            if !stopping {
                                stopping = true;
                                signal(pid, libc::SIGTERM);
                                kill_at = Some(Instant::now() + l.kill_after);
                            }
                        }
                        _ = sleep_until(kill_at) => {
                            signal(pid, libc::SIGKILL);
                            dlog!("network test server: SIGKILL sent");
                            kill_at = None;
                        }
                    }
                };
                dlog!("network test server: exited ({})", code.map_or("?".into(), |c| c.to_string()));
            }
            Err(e) => dlog!("network test server: cannot start: {e}"),
        }
        if stopping || !open {
            break;
        }
        if started.elapsed() >= l.stable_after {
            fails = 0;
        }
        let wait = l.backoff.get(fails).or(l.backoff.last()).copied().unwrap_or(Duration::from_secs(1));
        fails += 1;
        dlog!("network test server: starting again in {wait:?}");
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            r = reqs.recv() => {
                waiters.extend(r);
                break;
            }
        }
    }
    for w in waiters {
        let _ = w.send(());
    }
}

/// Spawns the server with its stdout and stderr into the log (the previous instance's is kept as
/// nettest-server.log.1); the receiver gets its exit code.
fn start_server(l: &ServerLaunch) -> io::Result<(i32, oneshot::Receiver<Option<i32>>)> {
    if let Some(d) = l.log.parent() {
        fs::create_dir_all(d)?;
    }
    let mut old = l.log.clone().into_os_string();
    old.push(".1");
    let _ = fs::rename(&l.log, &old);
    let mut log = OpenOptions::new().create(true).append(true).open(&l.log)?;
    let s = &l.spec;
    for (k, v) in &s.env {
        writeln!(log, "# env {k}={v}")?;
    }
    writeln!(log, "# exec {} {}", s.program.display(), s.args.join(" "))?;
    let header = log.stream_position()?;
    let mut child = spawn(s, &s.args)?;
    let pid = child.id().map_or(0, |p| p as i32);
    let (tx, lines) = mpsc::unbounded_channel();
    if let Some(so) = child.stdout.take() {
        tokio::spawn(lines_to(so, tx.clone()));
    }
    if let Some(se) = child.stderr.take() {
        tokio::spawn(lines_to(se, tx));
    }
    let (done, exit) = oneshot::channel();
    tokio::spawn(log_child(child, lines, log, header, done));
    Ok((pid, exit))
}

async fn log_child(
    mut child: tokio::process::Child,
    mut lines: mpsc::UnboundedReceiver<String>,
    mut log: File,
    header: u64,
    done: oneshot::Sender<Option<i32>>,
) {
    let limit = Some((media::LOG_CAP, header));
    let write = |log: &mut File, l: &str| {
        let _ = writeln!(log, "{l}");
        media::cap_log(log, limit);
    };
    let status = loop {
        tokio::select! {
            Some(l) = lines.recv() => write(&mut log, &l),
            st = child.wait() => break st,
        }
    };
    // what is still in the pipes
    let _ = tokio::time::timeout(Duration::from_millis(300), async {
        while let Some(l) = lines.recv().await {
            write(&mut log, &l);
        }
    })
    .await;
    let code = status.ok().and_then(|s| s.code().or_else(|| s.signal().map(|g| 128 + g)));
    let _ = writeln!(log, "# exit {}", code.map_or("?".into(), |c| c.to_string()));
    let _ = done.send(code);
}

// ------------------------------------------------------------------ tests

/// An App whose tests run the stub programs (tests/fixtures/stub_iperf3.py, stub_ping.py), with
/// the run_dir (their working directory) in a fresh temporary directory and short waits. `extra`
/// is more of the [network_test] table (an `iperf =` or `ping =` there replaces the stub).
#[cfg(test)]
pub fn stub_app(name: &str, role: Role, state: LinkState, extra: &str) -> (Arc<App>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("ctrld-nettest-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    let mut programs = String::new();
    for (key, stub) in [("iperf", "stub_iperf3.py"), ("ping", "stub_ping.py")] {
        if !extra.contains(&format!("{key} = ")) {
            programs.push_str(&format!("{key} = \"{fixtures}/{stub}\"\n"));
        }
    }
    let text = format!(
        "id = \"DEV-01\"\nrole = \"{}\"\nrun_dir = \"{d}\"\nstate_dir = \"{d}\"\n[tx]\npeer_host = \"10.0.0.2\"\n\
         [media]\nrepo = \"/m\"\n[preview]\nenabled = false\n[network_test]\n{programs}{extra}",
        if role == Role::Tx { "tx" } else { "rx" },
        d = dir.display(),
    );
    let cfg = Config::parse(&text).unwrap();
    let mut n = NetTests::new(&cfg, Server::start(None));
    // the client is stopped 0.1 s × duration + 1.5 s after it started (2 s for the shortest test)
    n.timing = Timing {
        per_second: Duration::from_millis(100),
        grace: Duration::from_millis(1500),
        kill_after: Duration::from_millis(400),
        ping_timeout: Duration::from_millis(800),
    };
    (crate::app::test_app_with(cfg, crate::preview::Preview::start(None), state, n), dir)
}

/// Polls until the test is no longer running.
#[cfg(test)]
pub async fn wait_done(app: &App, id: &str) -> NetworkTest {
    let t0 = Instant::now();
    loop {
        let t = app.nettest.get(id).expect("kept");
        if t.state != TestState::Running {
            return t;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "still running: {t:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    fn fixture(name: &str) -> String {
        fs::read_to_string(Path::new(FIX).join(name)).unwrap()
    }

    fn replay(name: &str, reverse: bool) -> Stream {
        let mut s = Stream::new(reverse);
        for l in fixture(name).lines() {
            s.line(l);
        }
        s
    }

    fn codes(r: Result<Request, Refusal>) -> Vec<String> {
        match r {
            Err(Refusal::Invalid(e)) => e.iter().map(|e| format!("{}:{}", e.field, e.code)).collect(),
            other => panic!("{other:?}"),
        }
    }

    /// What a stub saw: its argv per start (stub_*.jsonl in the run_dir).
    fn starts(dir: &Path, stub: &str) -> Vec<Vec<String>> {
        fs::read_to_string(dir.join(format!("{stub}.jsonl")))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| serde_json::from_value(v["args"].clone()).ok())
            .collect()
    }

    /// The list of 途中の無線機 as if applied (any address: the stub picks its case by it).
    fn set_hops(app: &App, hops: &[(&str, &str)]) {
        let hops = hops.iter().map(|(l, a)| crate::config::Hop { label: l.to_string(), address: a.to_string() }).collect();
        app.lock().settings.network = Some(crate::model::NetworkStored { version: 2, hops });
    }

    #[test]
    fn ping_outputs() {
        let seqs = |s: &PingSummary| s.replies.iter().map(|r| r.0).collect::<Vec<_>>();
        // the real capture: a reply line each
        let ok = parse_ping(&fixture("ping_ok.txt")).unwrap();
        assert_eq!((ok.sent, ok.received, ok.rtt), (10, 10, Some((0.005, 0.024, 0.046))));
        assert_eq!(seqs(&ok), (1..=10).collect::<Vec<_>>());
        assert_eq!((ok.replies[0], ok.replies[9], ok.dups.len()), ((1, 0.027), (10, 0.043), 0));
        assert_eq!(reply_note("127.0.0.1", &ok), None, "nothing to say");
        // real captures with the daemon's arguments: a silent address (iputils goes on sending until
        // the deadline: "20 packets transmitted") and one that is not there (an error ends the run
        // early: "6 packets transmitted, +5 errors"); either way the test's ten requests, none back
        let lost = parse_ping(&fixture("ping_lost.txt")).unwrap();
        assert!(fixture("ping_lost.txt").contains("20 packets transmitted, 0 received"));
        assert_eq!(lost, PingSummary { sent: 10, received: 0, rtt: None, replies: vec![], dups: vec![], extra: vec![] });
        assert_eq!(reply_note("10.0.0.9", &lost).unwrap(), "reach 10.0.0.9: 0/10 missing seq [1, 2, 3, 4, 5, 6, 7, 8, 9, 10] rtt_ms []");
        let unreachable = fixture("ping_unreachable.txt");
        assert!(unreachable.contains("6 packets transmitted, 0 received, +5 errors"));
        let s = parse_ping(&unreachable).unwrap();
        assert_eq!(s, PingSummary { sent: 10, received: 0, rtt: None, replies: vec![], dups: vec![], extra: vec![] });
        // three lost: iputils sends three more (11–13) whose replies make up its count of ten and
        // end the run; they are not the test's
        let part = parse_ping(&fixture("ping_partial.txt")).unwrap();
        assert_eq!((part.sent, part.received, part.rtt), (10, 7, Some((0.005, 0.025, 0.046))));
        assert_eq!(part.extra, [(11, 0.031), (12, 0.022), (13, 0.019)]);
        assert_eq!(
            reply_note("10.0.0.9", &part).unwrap(),
            "reach 10.0.0.9: 7/10 missing seq [3, 6, 9] rtt_ms [0.027, 0.015, 0.014, 0.005, 0.046, 0.033, 0.043] \
             extra seq [11, 12, 13] extra_rtt_ms [0.031, 0.022, 0.019]"
        );

        // the radio line: a late reply (printed out of order) is received, the lost one is not;
        // the log line tells them apart
        let late = parse_ping(&fixture("ping_late.txt")).unwrap();
        assert_eq!((late.sent, late.received), (10, 9), "not 10/11 (the summary's count includes the 11th)");
        assert_eq!(seqs(&late), [1, 2, 3, 4, 5, 6, 8, 9, 10]);
        assert_eq!(late.replies[3], (4, 612.0));
        assert_eq!(late.rtt, Some((74.6, 128.89, 612.0)));
        assert_eq!(
            reply_note("192.168.0.56", &late).unwrap(),
            "reach 192.168.0.56: 9/10 missing seq [7] late seq [4] rtt_ms [75.2, 74.8, 76.1, 612.0, 75.0, 74.6, 75.9, 74.9, 75.3] \
             extra seq [11] extra_rtt_ms [75.1]"
        );
        // late alone (nothing missing) is said too; twice the median exactly is not late
        let lines = |times: &[(u32, &str)], received: u32| {
            let mut out = String::from("PING 10.0.0.9 (10.0.0.9) 56(84) bytes of data.\n");
            for (q, t) in times {
                out.push_str(&format!("64 bytes from 10.0.0.9: icmp_seq={q} ttl=64 time={t} ms\n"));
            }
            out + &format!("\n--- 10.0.0.9 ping statistics ---\n10 packets transmitted, {received} received, 0% packet loss, time 1803ms\nrtt min/avg/max/mdev = 1.000/2.000/3.000/0.500 ms\n")
        };
        let all: Vec<(u32, &str)> = (1..=10).map(|q| (q, if q == 5 { "151" } else { "75.0" })).collect();
        let s = parse_ping(&lines(&all, 10)).unwrap();
        assert!(reply_note("10.0.0.9", &s).unwrap().contains(": 10/10 missing seq [] late seq [5] rtt_ms [75.0, 75.0, 75.0, 75.0, 151.0,"));
        let all: Vec<(u32, &str)> = (1..=10).map(|q| (q, if q == 5 { "150" } else { "75.0" })).collect();
        assert_eq!(reply_note("10.0.0.9", &parse_ping(&lines(&all, 10)).unwrap()), None);

        // duplicates count once and are noted; error lines and other noise are not replies
        let dup = "PING 192.168.0.65 (192.168.0.65) 56(84) bytes of data.\n\
                   64 bytes from 192.168.0.65: icmp_seq=1 ttl=64 time=1.02 ms\n\
                   64 bytes from 192.168.0.65: icmp_seq=1 ttl=64 time=1.40 ms (DUP!)\n\
                   64 bytes from 192.168.0.65: icmp_seq=2 ttl=64 time=0.98 ms\n\
                   64 bytes from 192.168.0.65: icmp_seq=2 ttl=64 time=1.10 ms (DUP!)\n\
                   64 bytes from 192.168.0.65: icmp_seq=2 ttl=64 time=1.30 ms (DUP!)\n\
                   From 192.168.0.220 icmp_seq=3 Destination Host Unreachable\n\
                   ping: Warning: time of day goes back (-12us), taking countermeasures\n\
                   64 bytes from 192.168.0.65: icmp_seq=11 ttl=64 time=0.9 ms\n\n\
                   --- 192.168.0.65 ping statistics ---\n\
                   10 packets transmitted, 2 received, +3 duplicates, +1 errors, 80% packet loss, time 1803ms\n\
                   rtt min/avg/max/mdev = 0.980/1.160/1.400/0.160 ms\n";
        let s = parse_ping(dup).unwrap();
        assert_eq!((s.received, s.replies.clone(), s.dups.clone()), (2, vec![(1, 1.02), (2, 0.98)], vec![1, 2]));
        assert_eq!(
            reply_note("192.168.0.65", &s).unwrap(),
            "reach 192.168.0.65: 2/10 missing seq [3, 4, 5, 6, 7, 8, 9, 10] dup seq [1, 2] rtt_ms [1.02, 0.98] extra seq [11] extra_rtt_ms [0.9]"
        );
        // only the extra requests answered (the line came up during the test): nothing of the test's
        let s = parse_ping(&lines(&[(11, "75.0"), (12, "75.1")], 2)).unwrap();
        assert_eq!((s.received, s.rtt, s.extra.len()), (0, None, 2));
        // the summary's count alone (output without reply lines, e.g. -q): nothing to say about them
        let quiet = "10 packets transmitted, 10 received, +2 duplicates, 0% packet loss, time 1800ms\n\
                     rtt min/avg/max/mdev = 74.912/75.334/76.021/0.301 ms\n";
        let s = parse_ping(quiet).unwrap();
        assert_eq!((s.received, s.rtt, reply_note("x", &s)), (10, Some((74.912, 75.334, 76.021)), None));
        let errs = "\n--- 192.168.0.65 ping statistics ---\n10 packets transmitted, 0 received, +10 errors, 100% packet loss, time 9001ms\npipe 3\n";
        assert_eq!(parse_ping(errs).map(|s| (s.sent, s.received, s.rtt)), Some((10, 0, None)));
        // busybox: seq from 0
        let busybox = "64 bytes from 10.0.0.1: seq=0 ttl=64 time=0.100 ms\n64 bytes from 10.0.0.1: seq=2 ttl=64 time=0.300 ms\n\
                       10 packets transmitted, 2 packets received, 80% packet loss\nround-trip min/avg/max = 0.1/0.2/0.3 ms\n";
        let s = parse_ping(busybox).unwrap();
        assert_eq!((s.sent, s.received, s.rtt, s.replies.clone()), (10, 2, Some((0.1, 0.2, 0.3)), vec![(1, 0.1), (3, 0.3)]));
        assert_eq!(parse_ping(""), None);
        assert_eq!(parse_ping("ping: nosuchhost.invalid: Name or service not known\n"), None);
        // reply lines without a summary (cut off): nothing
        assert_eq!(parse_ping("64 bytes from 10.0.0.1: icmp_seq=1 ttl=64 time=0.1 ms\n"), None);
    }

    #[test]
    fn client_streams_to_the_peer() {
        // the real capture (6 Mbps over localhost: nothing lost)
        let s = replay("iperf3_udp.jsonl", false);
        assert!(s.connected && s.error.is_none());
        assert!((s.elapsed_s - 3.001316).abs() < 1e-9);
        assert_eq!(s.client_seconds, [6.01, 6.0, 6.0], "the rate sent");
        assert_eq!(s.server_seconds, [6.01, 6.0, 6.0], "the receiver's report");
        assert_eq!(s.received_seconds(), [6.01, 6.0, 6.0]);
        let r = s.result.clone().unwrap();
        assert_eq!(
            r,
            Throughput { throughput_mbps: 6.0, sent_mbps: 6.0, loss_pct: 0.0, jitter_ms: 0.0, saturated: false, per_second_mbps: vec![] }
        );
        // the radio link's numbers in the same capture: sum_received is what arrived, the
        // receiver's seconds without the 0.23 s tail, the line was full
        let s = replay("iperf3_udp_saturated.jsonl", false);
        assert_eq!(s.received_seconds(), [5.29, 5.24, 5.25]);
        assert_eq!(s.client_seconds, [6.01, 6.0, 6.0]);
        let r = s.result.unwrap();
        assert_eq!((r.throughput_mbps, r.sent_mbps, r.loss_pct, r.jitter_ms, r.saturated), (5.26, 6.0, 5.79, 2.87, true));
        // a server started with -J reports in JSON
        let s = replay("iperf3_udp_server_json.jsonl", false);
        assert_eq!((s.server_seconds.as_slice(), s.client_seconds.len()), ([6.01, 6.0].as_slice(), 2));
        assert_eq!(s.result.unwrap().throughput_mbps, round(6004509.712477088 / 1e6, 2));
    }

    #[test]
    fn client_streams_from_the_peer() {
        // -R: the client receives; its own seconds are what arrived (the server's are the rate sent)
        let s = replay("iperf3_udp_reverse.jsonl", true);
        assert_eq!(s.client_seconds, [6.01, 6.0, 6.0]);
        assert_eq!(s.server_seconds, [6.01, 6.0, 6.0], "the sending side's text form is read too");
        assert_eq!(s.received_seconds(), s.client_seconds.as_slice());
        let r = s.result.unwrap();
        assert_eq!((r.throughput_mbps, r.sent_mbps, r.loss_pct, r.saturated), (round(6002426.507511944 / 1e6, 2), 6.0, 0.0, false));
        let mut lone = Stream::new(true);
        lone.line(&fixture("iperf3_udp_reverse.jsonl").lines().nth(1).unwrap().replace("6007330.867411932", "5260000"));
        assert_eq!(lone.received_seconds(), [5.26]);
    }

    #[test]
    fn client_streams_on_a_lossy_line() {
        // real captures with real loss (localhost, the receiving side paused 12 ms in every 100 ms
        // with a 2 KB socket buffer): 6 Mbps sent, about 5.5 arrived, 8 % lost
        let s = replay("iperf3_udp_lossy.jsonl", false);
        assert_eq!(s.client_seconds, [6.01, 6.0, 6.0, 5.99, 6.01, 6.0], "the rate sent");
        assert_eq!(s.received_seconds(), [5.52, 5.49, 5.48, 5.5, 5.48, 5.48], "the server's report: what arrived");
        let r = s.result.unwrap();
        assert_eq!((r.throughput_mbps, r.sent_mbps, r.loss_pct, r.jitter_ms, r.saturated), (5.49, 6.0, 8.3, 0.05, true));
        // -R: the client's seconds arrived; the server's report is the rate sent, its 0.12 s tail left out
        let s = replay("iperf3_udp_lossy_reverse.jsonl", true);
        assert_eq!(s.received_seconds(), [5.68, 5.5, 5.47, 5.5, 5.47, 5.51]);
        assert_eq!(s.server_seconds, [6.01, 6.0, 6.0, 6.0, 6.0, 6.0]);
        let r = s.result.unwrap();
        assert_eq!((r.throughput_mbps, r.sent_mbps, r.loss_pct, r.jitter_ms, r.saturated), (5.52, 6.0, 7.97, 0.11, true));
    }

    #[test]
    fn end_lines() {
        let end = |data: Value| json!({ "event": "end", "data": data }).to_string();
        let mut s = Stream::new(false);
        s.line(&end(json!({
            "sum": { "bits_per_second": 6.0e6, "jitter_ms": 3.1234, "lost_percent": 12.3456 },
            "sum_sent": { "bits_per_second": 5.9876e6, "jitter_ms": 0, "lost_percent": 0 },
            "sum_received": { "bits_per_second": 5.2649e6, "jitter_ms": 3.1234, "lost_percent": 12.3456 },
        })));
        let r = s.result.unwrap();
        assert_eq!((r.throughput_mbps, r.sent_mbps, r.loss_pct, r.jitter_ms, r.saturated), (5.26, 5.99, 12.35, 3.12, true));
        // a tool without sum_received / sum_sent: the ambiguous sum, the loss from the counts
        let mut s = Stream::new(false);
        s.line(&end(json!({ "sum": { "bits_per_second": 5.5e6, "jitter_ms": 2.0, "lost_packets": 3, "packets": 400 } })));
        let r = s.result.unwrap();
        assert_eq!((r.throughput_mbps, r.sent_mbps, r.loss_pct, r.saturated), (5.5, 5.5, 0.75, false));
        // just at the line: 1 % (two decimals) is full
        for (loss, full) in [(0.994, false), (0.995, true), (1.0, true)] {
            let mut s = Stream::new(false);
            s.line(&end(json!({ "sum_received": { "bits_per_second": 6e6, "lost_percent": loss } })));
            assert_eq!(s.result.unwrap().saturated, full, "{loss}");
        }
        // the end line after an error carries nothing
        let mut s = Stream::new(false);
        s.line(&end(json!({})));
        assert!(s.result.is_none());
        // omitted and sliver intervals are not seconds; other lines are ignored
        let iv = |secs: f64, omitted: bool| {
            json!({ "event": "interval", "data": { "sum": { "end": 9.0, "seconds": secs, "bits_per_second": 1e6, "omitted": omitted } } }).to_string()
        };
        let mut s = Stream::new(true);
        assert!(!s.line(&iv(1.0, true)) && !s.line(&iv(0.01, false)) && s.line(&iv(1.0, false)));
        assert!(!s.line("not json") && !s.line("{}"));
        assert_eq!((s.client_seconds.as_slice(), s.elapsed_s), ([1.0].as_slice(), 9.0));
        // the server's JSON report: the same rules
        let mut s = Stream::new(false);
        let sj = json!({ "event": "server_output_json", "data": { "intervals": [
            { "sum": { "seconds": 1.0, "bits_per_second": 5.31e6 } },
            { "sum": { "seconds": 1.0, "bits_per_second": 4e6, "omitted": true } },
            { "sum": { "seconds": 0.1, "bits_per_second": 4e6 } },
        ] } });
        assert!(!s.line(&sj.to_string()));
        assert_eq!(s.received_seconds(), [5.31]);
    }

    #[test]
    fn server_text_reports() {
        let t = "Accepted connection from 192.168.0.220, port 46772\n\
                 [  5] local 192.168.0.210 port 60022 connected to 192.168.0.220 port 57407\n\
                 [ ID] Interval           Transfer     Bitrate         Jitter    Lost/Total Datagrams\n\
                 [  5]   0.00-1.00   sec   645 KBytes  5.29 Mbits/sec  2.514 ms  33/535 (6.2%)  \n\
                 [  5]   1.00-2.00   sec  98.7 KBytes   809 Kbits/sec  9.100 ms  400/477 (84%)  \n\
                 [  5]   9.00-10.00  sec  0.00 Bytes  0.00 bits/sec  9.100 ms  0/0 (0%)  \n\
                 [  5]  10.00-11.00  sec   134 MBytes  1.12 Gbits/sec  0.010 ms  0/9 (0%)  \n\
                 [  5]  11.00-11.20  sec   129 KBytes  5.28 Mbits/sec  2.871 ms  0/100 (0%)  \n\
                 - - - - - - - - - - - - - - - - - - - - - - - - -\n\
                 [ ID] Interval           Transfer     Bitrate         Jitter    Lost/Total Datagrams\n\
                 [  5]   0.00-11.20  sec  2.02 MBytes  5.26 Mbits/sec  2.871 ms  99/1711 (5.8%)  receiver\n";
        assert_eq!(server_text_seconds(t), [5.29, 0.81, 0.0, 1120.0]);
        // several streams: the [SUM] lines
        let multi = "[  5]   0.00-1.00   sec   300 KBytes  2.46 Mbits/sec  1.0 ms  0/10 (0%)\n\
                     [  7]   0.00-1.00   sec   300 KBytes  2.46 Mbits/sec  1.0 ms  0/10 (0%)\n\
                     [SUM]   0.00-1.00   sec   600 KBytes  4.92 Mbits/sec  1.0 ms  0/20 (0%)\n";
        assert_eq!(server_text_seconds(multi), [4.92]);
        // the server's banner, an error, nothing
        assert!(server_text_seconds("-----\nServer listening on 60022 (test #2)\n-----\n").is_empty());
        assert!(server_text_seconds("iperf3: error - the client has terminated\n").is_empty());
        assert!(server_text_seconds("").is_empty());
        assert!(server_text_seconds("[  5]   0.00-1.00   sec   645 KBytes  5.29 Xbits/sec\n[  5] x-y sec 1 KBytes 1 Mbits/sec\n").is_empty());
    }

    #[test]
    fn client_errors() {
        let s = replay("iperf3_refused.jsonl", false);
        assert!(!s.connected && s.result.is_none());
        assert!(s.error.as_deref().unwrap().contains("Connection refused"));
        assert_eq!(classify(s.error.as_deref().unwrap()), Failure::PeerUnreachable);
        let s = replay("iperf3_busy.jsonl", false);
        assert_eq!(classify(s.error.as_deref().unwrap()), Failure::PeerBusy);
        for (msg, want) in [
            ("unable to connect to server - server may have stopped running or use a different port, firewall issue, etc.: Connection timed out", Failure::PeerUnreachable),
            ("unable to connect to server - server may have stopped running or use a different port, firewall issue, etc.: No route to host", Failure::PeerUnreachable),
            ("unable to connect to server - server may have stopped running or use a different port, firewall issue, etc.: Network is unreachable", Failure::PeerUnreachable),
            ("unable to resolve host", Failure::PeerUnreachable),
            ("control socket has closed unexpectedly", Failure::Failed),
            ("unable to receive results: Connection reset by peer", Failure::Failed),
            ("", Failure::Failed),
        ] {
            assert_eq!(classify(msg), want, "{msg}");
        }
    }

    #[test]
    fn requests() {
        let tx = |v: Value| parse(&v, Role::Tx, true);
        let r = tx(json!({ "kind": "throughput" })).unwrap();
        assert_eq!(r, Request { kind: TestKind::Throughput, direction: Some(Direction::ToPeer), duration_s: Some(10) });
        let r = tx(json!({ "kind": "throughput", "direction": "from_peer", "duration_s": 30 })).unwrap();
        assert_eq!((r.direction, r.duration_s), (Some(Direction::FromPeer), Some(30)));
        let r = tx(json!({ "kind": "throughput", "duration_s": 5.0, "direction": null })).unwrap();
        assert_eq!((r.duration_s, r.direction), (Some(5), Some(Direction::ToPeer)));
        // the old modes' fields are ignored, whatever they hold
        let old = tx(json!({ "kind": "throughput", "mode": "steady", "rate_mbps": 99 })).unwrap();
        assert_eq!(old, tx(json!({ "kind": "throughput" })).unwrap());
        assert!(tx(json!({ "kind": "throughput", "mode": 1, "rate_mbps": "x" })).is_ok());
        assert_eq!(tx(json!({ "kind": "reachability", "direction": "x", "duration_s": 99 })).unwrap(), Request::REACHABILITY);
        assert_eq!(
            serde_json::to_value(tx(json!({ "kind": "throughput" })).unwrap()).unwrap(),
            json!({ "kind": "throughput", "direction": "to_peer", "duration_s": 10 })
        );

        assert_eq!(codes(tx(json!({}))), ["kind:required"]);
        assert_eq!(codes(tx(json!({ "kind": null }))), ["kind:required"]);
        assert_eq!(codes(tx(json!([1]))), ["kind:required"]);
        assert_eq!(codes(tx(json!({ "kind": "bandwidth" }))), ["kind:invalid"]);
        assert_eq!(codes(tx(json!({ "kind": 1 }))), ["kind:invalid"]);
        assert_eq!(codes(tx(json!({ "kind": "throughput", "direction": "up", "duration_s": 3 }))), ["direction:invalid", "duration_s:out_of_range"]);
        for d in [json!(4), json!(31), json!(10.5), json!("10")] {
            assert_eq!(codes(tx(json!({ "kind": "throughput", "duration_s": d }))), ["duration_s:out_of_range"]);
        }
        let Err(Refusal::Invalid(e)) = tx(json!({ "kind": "throughput", "duration_s": 1 })) else { panic!() };
        assert_eq!(serde_json::to_value(&e).unwrap()[0]["params"], json!({ "min": 5, "max": 30 }));

        // the role before the fields; tests turned off: not offered at all
        assert_eq!(parse(&json!({ "kind": "throughput", "duration_s": 1 }), Role::Rx, true), Err(Refusal::UnsupportedRole));
        assert_eq!(parse(&json!({ "kind": "reachability" }), Role::Rx, true), Ok(Request::REACHABILITY));
        assert_eq!(parse(&json!({ "kind": "reachability" }), Role::Tx, false), Err(Refusal::UnsupportedRole));
        assert_eq!(codes(parse(&json!({ "kind": "x" }), Role::Rx, false)), ["kind:invalid"]);
    }

    #[test]
    fn command_lines() {
        let r = parse(&json!({ "kind": "throughput" }), Role::Tx, true).unwrap();
        assert_eq!(
            client_args("192.168.0.210", 60022, 6.0, &r).join(" "),
            "-c 192.168.0.210 -p 60022 -u -b 6M -l 1316 -t 10 -i 1 --json-stream --forceflush --get-server-output --connect-timeout 3000"
        );
        let r = parse(&json!({ "kind": "throughput", "direction": "from_peer", "duration_s": 20 }), Role::Tx, true).unwrap();
        assert_eq!(
            client_args("192.168.0.210", 60022, 7.5, &r).join(" "),
            "-c 192.168.0.210 -p 60022 -u -b 7.5M -l 1316 -t 20 -i 1 --json-stream --forceflush --get-server-output --connect-timeout 3000 -R"
        );
        assert_eq!(ping_args("192.168.0.65").join(" "), "-n -c 10 -i 0.2 -W 1 -w 4 192.168.0.65");

        let cfg = Config::parse("id = \"RX\"\nrole = \"rx\"\nrun_dir = \"/run/c\"\n[media]\nrepo = \"/m\"\n").unwrap();
        let l = ServerLaunch::from_config(&cfg).unwrap();
        assert_eq!((l.spec.program.as_path(), l.spec.args.join(" ")), (Path::new("iperf3"), "-s -p 60022".into()));
        assert_eq!((l.spec.env["LC_ALL"].as_str(), l.spec.cwd.as_path()), ("C", Path::new("/run/c")));
        assert_eq!(l.log, Path::new("/run/c/nettest-server.log"));
        let tx = Config::parse("id = \"TX\"\nrole = \"tx\"\n[tx]\npeer_host = \"10.0.0.2\"\n[media]\nrepo = \"/m\"\n").unwrap();
        assert!(ServerLaunch::from_config(&tx).is_none(), "the transmitter runs no server by default");
    }

    #[test]
    fn history_keeps_the_last_20_and_never_a_running_one() {
        let rec = |id: u64, state: TestState| Record {
            t: NetworkTest {
                id: id.to_string(),
                kind: TestKind::Reachability,
                state,
                started_at: String::new(),
                finished_at: None,
                progress: 0.0,
                request: Request::REACHABILITY,
                result: None,
                error: None,
            },
            started: Instant::now(),
        };
        let mut b = Book::default();
        b.add(rec(0, TestState::Running));
        for i in 1..=25 {
            b.add(rec(i, TestState::Done));
        }
        let ids: Vec<&str> = b.items.iter().map(|r| r.t.id.as_str()).collect();
        assert_eq!(ids.len(), HISTORY);
        assert_eq!(&ids[..3], ["25", "24", "23"], "newest first");
        assert_eq!(ids[HISTORY - 2..], ["7", "0"], "the old running test stays, the oldest finished ones go");
        assert!(b.running(TestKind::Reachability) && !b.running(TestKind::Throughput));
    }

    #[tokio::test]
    async fn throughput_to_the_peer() {
        let (app, dir) = stub_app("tput", Role::Tx, LinkState::Stopped, "port = 60022\n");
        app.lock().settings.transport.peer_host = "lossy-10.0.0.2".into();
        let t = start(&app, &json!({ "kind": "throughput" })).unwrap();
        assert_eq!((t.state, t.progress, t.finished_at.as_deref()), (TestState::Running, 0.0, None));
        assert!(t.result.is_none() && t.error.is_none());
        let t = wait_done(&app, &t.id).await;
        assert_eq!(t.state, TestState::Done, "{t:?}");
        assert_eq!(t.progress, 1.0);
        assert!(t.finished_at.is_some());
        let Some(Outcome::Throughput(r)) = &t.result else { panic!("{t:?}") };
        assert_eq!(
            r,
            &Throughput {
                throughput_mbps: 5.26,
                sent_mbps: 6.0,
                loss_pct: 5.79,
                jitter_ms: 2.87,
                saturated: true,
                per_second_mbps: vec![5.29, 5.24, 5.25],
            }
        );
        let args = starts(&dir, "stub_iperf3");
        assert_eq!(args.len(), 1);
        assert_eq!(
            args[0].join(" "),
            "-c lossy-10.0.0.2 -p 60022 -u -b 6M -l 1316 -t 10 -i 1 --json-stream --forceflush --get-server-output --connect-timeout 3000"
        );
        let ev = app.lock().events.latest(5);
        assert_eq!(ev[0].code, "network_test_done");
        assert_eq!(Value::Object(ev[0].params.clone()), json!({ "mbps": 5.26, "direction": "to_peer" }));
        // what the API shows
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v["request"], json!({ "kind": "throughput", "direction": "to_peer", "duration_s": 10 }));
        let keys: Vec<&str> = v["result"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["jitter_ms", "loss_pct", "per_second_mbps", "saturated", "sent_mbps", "throughput_mbps"]);
        assert_eq!(v["error"], Value::Null);

        // the other way: the client's own seconds; the line was not full
        app.lock().settings.transport.peer_host = "10.0.0.2".into();
        let t = start(&app, &json!({ "kind": "throughput", "direction": "from_peer", "mode": "max", "duration_s": 5 })).unwrap();
        let t = wait_done(&app, &t.id).await;
        let Some(Outcome::Throughput(r)) = &t.result else { panic!("{t:?}") };
        assert_eq!((r.throughput_mbps, r.loss_pct, r.saturated), (6.0, 0.0, false));
        assert_eq!(r.per_second_mbps, [6.01, 6.0, 6.0]);
        assert_eq!(starts(&dir, "stub_iperf3")[1][10..].join(" "), "5 -i 1 --json-stream --forceflush --get-server-output --connect-timeout 3000 -R");
        assert_eq!(app.lock().events.latest(1)[0].params["direction"], "from_peer");
        assert_eq!(app.nettest.list().len(), 2);
        assert_eq!(app.nettest.options(), json!({ "duration_range_s": [5, 30], "saturate_mbps": 6.0 }));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn throughput_progress_while_running() {
        let (app, dir) = stub_app("progress", Role::Tx, LinkState::Stopped, "saturate_mbps = 8\n");
        app.lock().settings.transport.peer_host = "slow-10.0.0.2".into();
        let t = start(&app, &json!({ "kind": "throughput", "duration_s": 5 })).unwrap();
        // the stub writes a line every 0.25 s: the progress moves, the result waits for the end
        let mid = loop {
            let mid = app.nettest.get(&t.id).unwrap();
            if mid.progress > 0.0 || mid.state != TestState::Running {
                break mid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(mid.state, TestState::Running);
        assert!(mid.progress > 0.0 && mid.progress < 1.0, "{}", mid.progress);
        assert!(mid.result.is_none(), "{mid:?}");
        assert!(app.nettest.throughput_running());
        assert!(app.nettest.hold_start().is_none(), "no transport start meanwhile");
        let t = wait_done(&app, &t.id).await;
        assert_eq!(t.state, TestState::Done);
        assert!(app.nettest.hold_start().is_some());
        assert_eq!(starts(&dir, "stub_iperf3")[0][6], "8M", "the configured rate");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn which_tests_give_the_send_cap() {
        let (app, dir) = stub_app("use", Role::Tx, LinkState::Stopped, "saturate_mbps = 6.5\n");
        let n = &app.nettest;
        assert_eq!(n.send_limit_from("nope"), Err(UseRefusal::NotFound));
        // full line: what arrived, with the time it finished
        app.lock().settings.transport.peer_host = "lossy-10.0.0.2".into();
        let full = wait_done(&app, &start(&app, &json!({ "kind": "throughput" })).unwrap().id).await;
        assert_eq!(n.send_limit_from(&full.id), Ok((5.26, full.finished_at.clone().unwrap())));
        // not full: the line carries at least the test's rate
        app.lock().settings.transport.peer_host = "10.0.0.2".into();
        let free = wait_done(&app, &start(&app, &json!({ "kind": "throughput" })).unwrap().id).await;
        assert_eq!(n.send_limit_from(&free.id).unwrap().0, 6.5);
        // the other way, a failure, a running test, a reachability test: not a send cap
        let back = wait_done(&app, &start(&app, &json!({ "kind": "throughput", "direction": "from_peer" })).unwrap().id).await;
        assert_eq!(n.send_limit_from(&back.id), Err(UseRefusal::NotApplicable));
        app.lock().settings.transport.peer_host = "refused.example".into();
        let failed = wait_done(&app, &start(&app, &json!({ "kind": "throughput" })).unwrap().id).await;
        assert_eq!((failed.state, n.send_limit_from(&failed.id)), (TestState::Failed, Err(UseRefusal::NotApplicable)));
        app.lock().settings.transport.peer_host = "slow-10.0.0.2".into();
        let running = start(&app, &json!({ "kind": "throughput" })).unwrap();
        assert_eq!(n.send_limit_from(&running.id), Err(UseRefusal::NotApplicable));
        let reach = start(&app, &json!({ "kind": "reachability" })).unwrap();
        wait_done(&app, &reach.id).await;
        assert_eq!(n.send_limit_from(&reach.id), Err(UseRefusal::NotApplicable));
        wait_done(&app, &running.id).await;
        // kept within the cap's range
        let mut b = n.book();
        let r = b.get_mut(&full.id).unwrap();
        let Some(Outcome::Throughput(t)) = r.t.result.as_mut() else { panic!() };
        t.throughput_mbps = 0.42;
        drop(b);
        assert_eq!(n.send_limit_from(&full.id).unwrap().0, 1.0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn throughput_failures() {
        let (app, dir) = stub_app("tfail", Role::Tx, LinkState::Stopped, "");
        for (host, want) in [
            ("refused.example", Failure::PeerUnreachable),
            ("busy.example", Failure::PeerBusy),
            ("crash.example", Failure::Failed),
        ] {
            app.lock().settings.transport.peer_host = host.into();
            let t = start(&app, &json!({ "kind": "throughput", "duration_s": 5 })).unwrap();
            let t = wait_done(&app, &t.id).await;
            assert_eq!((t.state, t.error, t.result.is_none()), (TestState::Failed, Some(want), true), "{host}");
            assert!(t.finished_at.is_some());
        }
        assert!(app.lock().events.latest(10).iter().all(|e| e.code != "network_test_done"), "no event without a result");

        // no end in time: stopped; never connected → unreachable, connected → failed
        for (host, want) in [
            ("silent.example", Failure::PeerUnreachable),
            ("hang.example", Failure::Failed),
            ("stubborn.example", Failure::PeerUnreachable),
        ] {
            app.lock().settings.transport.peer_host = host.into();
            let t0 = Instant::now();
            let t = start(&app, &json!({ "kind": "throughput", "duration_s": 5 })).unwrap();
            let t = wait_done(&app, &t.id).await;
            let took = t0.elapsed();
            assert_eq!((t.state, t.error), (TestState::Failed, Some(want)), "{host}");
            // SIGTERM at 0.1 s × 5 + 1.5 s, SIGKILL 0.4 s later
            let min = if host == "stubborn.example" { 2400 } else { 2000 };
            assert!(took >= Duration::from_millis(min) && took < Duration::from_millis(min + 1500), "{host}: {took:?}");
        }
        // missing program, unusable address
        let (app2, dir2) = stub_app("tmissing", Role::Tx, LinkState::Stopped, "iperf = \"/nonexistent/bw\"\n");
        let t = start(&app2, &json!({ "kind": "throughput" })).unwrap();
        assert_eq!(wait_done(&app2, &t.id).await.error, Some(Failure::Failed));
        app2.lock().settings.transport.peer_host = "-R".into();
        let t = start(&app2, &json!({ "kind": "throughput" })).unwrap();
        assert_eq!((t.state, t.error), (TestState::Failed, Some(Failure::Failed)), "refused at once, nothing run");
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&dir2).unwrap();
    }

    #[tokio::test]
    async fn busy_rules() {
        let (app, dir) = stub_app("busy", Role::Tx, LinkState::Live, "");
        set_hops(&app, &[("遠い", "slow.example")]);
        assert_eq!(start(&app, &json!({ "kind": "throughput" })).unwrap_err(), Refusal::Busy, "the link is live");
        let reach = start(&app, &json!({ "kind": "reachability" })).unwrap();
        assert_eq!(start(&app, &json!({ "kind": "reachability" })).unwrap_err(), Refusal::Busy, "one at a time");
        app.lock().link.state = LinkState::Stopped;
        // a start on its way to the supervisor
        let hold = app.nettest.hold_start().unwrap();
        assert_eq!(start(&app, &json!({ "kind": "throughput" })).unwrap_err(), Refusal::Busy);
        drop(hold);
        app.lock().settings.transport.peer_host = "slow-10.0.0.2".into();
        let t = start(&app, &json!({ "kind": "throughput" })).unwrap();
        assert_eq!(start(&app, &json!({ "kind": "throughput" })).unwrap_err(), Refusal::Busy, "one at a time");
        assert!(app.nettest.hold_start().is_none());
        // the reachability test runs on meanwhile and ends by itself
        assert_eq!(wait_done(&app, &reach.id).await.state, TestState::Done);
        assert_eq!(wait_done(&app, &t.id).await.state, TestState::Done);
        assert!(start(&app, &json!({ "kind": "reachability" })).is_ok());
        let kinds: Vec<TestKind> = app.nettest.list().iter().map(|t| t.kind).collect();
        assert_eq!(kinds, [TestKind::Reachability, TestKind::Throughput, TestKind::Reachability], "newest first");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn reachability_of_the_peer_and_the_hops() {
        // device.toml's list until one is applied; then the applied one (the stub picks its case by
        // the address, so these are set as if applied)
        let (app, dir) = stub_app("reach", Role::Tx, LinkState::Live, "[[network_test.hops]]\nlabel = \"設定の無線機\"\naddress = \"10.0.0.65\"\n");
        set_hops(
            &app,
            &[
                ("無線機（送信機側）", "10.0.0.65"),
                ("無線機（受信機側）", "partial.example"),
                ("止まった機器", "lost.example"),
                ("名前違い", "unknown.invalid"),
                ("応答なし", "hang.example"),
                ("遅れて届く", "late.example"),
            ],
        );
        let t0 = Instant::now();
        let t = start(&app, &json!({ "kind": "reachability", "mode": "bogus" })).unwrap();
        assert_eq!(serde_json::to_value(&t.request).unwrap(), json!({ "kind": "reachability", "direction": null, "duration_s": null }));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let p = app.nettest.get(&t.id).unwrap().progress;
        assert!(p > 0.0 && p < 1.0, "an estimate from the time: {p}");
        let t = wait_done(&app, &t.id).await;
        assert!(t0.elapsed() < Duration::from_millis(1600), "all at once: {:?}", t0.elapsed());
        assert_eq!((t.state, t.progress), (TestState::Done, 1.0));
        let Some(Outcome::Reachability { targets }) = &t.result else { panic!("{t:?}") };
        let got: Vec<(&str, Option<&str>, &str, u32, u32, Option<f64>)> = targets
            .iter()
            .map(|t| (t.kind, t.label.as_deref(), t.address.as_str(), t.sent, t.received, t.rtt_avg_ms))
            .collect();
        assert_eq!(
            got,
            [
                ("peer", None, "10.0.0.2", 10, 10, Some(0.02)),
                ("hop", Some("無線機（送信機側）"), "10.0.0.65", 10, 10, Some(0.02)),
                ("hop", Some("無線機（受信機側）"), "partial.example", 10, 7, Some(0.03)),
                ("hop", Some("止まった機器"), "lost.example", 10, 0, None),
                ("hop", Some("名前違い"), "unknown.invalid", 10, 0, None),
                ("hop", Some("応答なし"), "hang.example", 10, 0, None),
                ("hop", Some("遅れて届く"), "late.example", 10, 9, Some(128.89)),
            ]
        );
        // two decimals (ping writes three): 0.005 / 0.046 ms
        assert_eq!((targets[0].rtt_min_ms, targets[0].rtt_max_ms), (Some(0.01), Some(0.05)));
        assert_eq!((targets[3].rtt_min_ms, targets[3].rtt_max_ms), (None, None));
        let mut args: Vec<String> = starts(&dir, "stub_ping").iter().map(|a| a.join(" ")).collect();
        args.sort();
        assert_eq!(args.len(), 7);
        assert_eq!(args[0], "-n -c 10 -i 0.2 -W 1 -w 4 10.0.0.2");
        // the developer log has the late and the missing replies
        let log = crate::util::log_dump();
        assert!(log.contains("reach late.example: 9/10 missing seq [7] late seq [4] rtt_ms [75.2, 74.8, 76.1, 612.0,"), "{log}");
        assert!(log.contains("reach partial.example: 7/10 missing seq [3, 6, 9] rtt_ms ["));
        assert!(!log.contains("reach 10.0.0.65:"), "a clean run says nothing");
        assert!(app.lock().events.latest(5).iter().all(|e| e.code != "network_test_done"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn reachability_on_the_receiver() {
        let hop = "[[network_test.hops]]\nlabel = \"無線機\"\naddress = \"10.0.0.56\"\n";
        let peers = |t: &NetworkTest| match &t.result {
            Some(Outcome::Reachability { targets }) => targets.iter().map(|t| format!("{}:{}", t.kind, t.address)).collect::<Vec<_>>(),
            _ => panic!("{t:?}"),
        };
        // no transmitter known: only the hops (device.toml's, nothing applied yet)
        let (app, dir) = stub_app("rx-reach", Role::Rx, LinkState::Listening, hop);
        let t = start(&app, &json!({ "kind": "reachability" })).unwrap();
        assert_eq!(peers(&wait_done(&app, &t.id).await), ["hop:10.0.0.56"]);
        // an applied list wins, an empty one too
        set_hops(&app, &[]);
        let t = start(&app, &json!({ "kind": "reachability" })).unwrap();
        assert_eq!(peers(&t), Vec::<String>::new(), "nothing to check: done at once");
        set_hops(&app, &[("無線機", "10.0.0.57")]);
        let t = start(&app, &json!({ "kind": "reachability" })).unwrap();
        assert_eq!(peers(&wait_done(&app, &t.id).await), ["hop:10.0.0.57"]);
        set_hops(&app, &[("無線機", "10.0.0.56")]);
        // the last transmitter that connected
        app.lock().link.peer_addr = Some(("192.168.0.220".into(), 51234));
        let t = start(&app, &json!({ "kind": "reachability" })).unwrap();
        assert_eq!(peers(&wait_done(&app, &t.id).await), ["peer:192.168.0.220", "hop:10.0.0.56"]);
        assert_eq!(start(&app, &json!({ "kind": "throughput" })).unwrap_err(), Refusal::UnsupportedRole);
        fs::remove_dir_all(&dir).unwrap();
        // the configured address wins
        let (app, dir) = stub_app("rx-peer", Role::Rx, LinkState::Live, &format!("[rx]\npeer_host = \"192.168.0.221\"\n{hop}"));
        app.lock().link.peer_addr = Some(("192.168.0.220".into(), 51234));
        let t = start(&app, &json!({ "kind": "reachability" })).unwrap();
        assert_eq!(peers(&wait_done(&app, &t.id).await)[0], "peer:192.168.0.221");
        fs::remove_dir_all(&dir).unwrap();
        // nothing to check: done at once
        let (app, dir) = stub_app("rx-none", Role::Rx, LinkState::Stopped, "");
        let t = start(&app, &json!({ "kind": "reachability" })).unwrap();
        assert_eq!((t.state, t.progress), (TestState::Done, 1.0));
        assert_eq!(serde_json::to_value(&t.result).unwrap(), json!({ "targets": [] }));
        fs::remove_dir_all(&dir).unwrap();
        // the program is missing
        let (app, dir) = stub_app("rx-noping", Role::Rx, LinkState::Stopped, &format!("ping = \"/nonexistent/echo\"\n{hop}"));
        let t = start(&app, &json!({ "kind": "reachability" })).unwrap();
        let t = wait_done(&app, &t.id).await;
        assert_eq!((t.state, t.error, t.result.is_none()), (TestState::Failed, Some(Failure::Failed), true));
        fs::remove_dir_all(&dir).unwrap();
    }

    // -------------------------------------------------------------- the server

    fn server_launch(name: &str, mode: &str) -> (ServerLaunch, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("ctrld-nettest-server-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let text = format!(
            "id = \"RX\"\nrole = \"rx\"\nrun_dir = \"{}\"\n[media]\nrepo = \"/m\"\n[network_test]\niperf = \"{FIX}/stub_iperf3.py\"\nport = 18123\n",
            dir.display()
        );
        let mut l = ServerLaunch::from_config(&Config::parse(&text).unwrap()).unwrap();
        l.spec.env.insert("STUB_IPERF3_SERVER".into(), mode.into());
        l.backoff = vec![Duration::from_millis(100), Duration::from_millis(200)];
        l.kill_after = Duration::from_millis(500);
        (l, dir)
    }

    fn server_seen(dir: &Path) -> Vec<Value> {
        fs::read_to_string(dir.join("stub_iperf3_server.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn server_starts(dir: &Path) -> usize {
        server_seen(dir).iter().filter(|v| v.get("start").is_some()).count()
    }

    #[tokio::test]
    async fn server_runs_until_stopped() {
        let (l, dir) = server_launch("run", "");
        let s = Server::start(Some(l));
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(server_starts(&dir), 1);
        assert_eq!(server_seen(&dir)[0]["start"], json!(["-s", "-p", "18123"]));
        let t = Instant::now();
        s.stop().await.unwrap();
        assert!(t.elapsed() < Duration::from_millis(450), "SIGTERM ends it: {:?}", t.elapsed());
        assert!(server_seen(&dir).iter().any(|v| v["term"] == true));
        let log = fs::read_to_string(dir.join("nettest-server.log")).unwrap();
        assert!(log.contains("# env LC_ALL=C\n") && log.contains("stub_iperf3.py -s -p 18123\n"), "{log}");
        assert!(log.contains("Server listening on 18123") && log.ends_with("# exit 0\n"), "{log}");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(server_starts(&dir), 1, "not started again after a stop");
        s.stop().await.unwrap();
        Server::start(None).stop().await.unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn server_restarts_with_a_backoff() {
        let (l, dir) = server_launch("restart", "exit");
        let s = Server::start(Some(l));
        // starts at 0, 0.1, 0.3, 0.5, 0.7 s …
        tokio::time::sleep(Duration::from_millis(650)).await;
        let n = server_starts(&dir);
        assert!((3..=5).contains(&n), "{n}");
        let log = fs::read_to_string(dir.join("nettest-server.log")).unwrap();
        assert!(log.contains("Address already in use") && log.contains("# exit 1"), "{log}");
        assert!(dir.join("nettest-server.log.1").exists(), "the previous instance's log");
        let t = Instant::now();
        s.stop().await.unwrap();
        assert!(t.elapsed() < Duration::from_millis(100), "stopped while waiting: {:?}", t.elapsed());
        let n = server_starts(&dir);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(server_starts(&dir), n);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn server_stop_escalates_to_kill() {
        let (l, dir) = server_launch("stubborn", "stubborn");
        let s = Server::start(Some(l));
        tokio::time::sleep(Duration::from_millis(400)).await;
        let t = Instant::now();
        s.stop().await.unwrap();
        let took = t.elapsed();
        assert!(took >= Duration::from_millis(450) && took < Duration::from_millis(1500), "{took:?}");
        assert!(fs::read_to_string(dir.join("nettest-server.log")).unwrap().ends_with("# exit 137\n"));
        // a program that is not there: tried again, stopped all the same
        let (mut l, dir2) = server_launch("missing", "");
        l.spec.program = "/nonexistent/bw".into();
        let s = Server::start(Some(l));
        tokio::time::sleep(Duration::from_millis(250)).await;
        s.stop().await.unwrap();
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&dir2).unwrap();
    }
}
