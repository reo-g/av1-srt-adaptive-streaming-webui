# ctrld — device daemon

One binary per device (transmitter or receiver). It serves the web UI (`web/dist`, embedded at
build time) and the device API under `/api/v1` (contract: `api/openapi.yaml`,
`web/src/api/types.ts`; `web/mock/server.ts` is the executable reference), and it drives the
existing media programs as child processes (prototype phase, see `CLAUDE.md`).

## Build

```
cd web && npm install && npm run build     # web/dist must exist: it is embedded
cd ../daemon && cargo build --release      # → daemon/target/release/ctrld
cargo test
```

Rust 1.85 or newer (`resolver = "3"` picks dependency versions for `rust-version`).

## Run

```
ctrld --config ~/.config/ctrld/device.toml   # the default path; see device.example.toml
```

`device.toml` holds the identity (`id`, `role`, `serial`), the HTTP bind address and port
(default `0.0.0.0:8080`), the directories, the media program paths and the **profile**: the
environment and arguments of each child. The built-in defaults are the standard profile (the
current automatic mode, `src/profile.rs`); entries under `[profile.*_env]` replace a default
and an empty string removes it, so the controller settings being tuned are not fixed in the
daemon. The daemon adds the per-session file paths itself.

`tx.peer_host` / `tx.peer_port` are only the initial transport settings; after the first apply
the settings screen's values (in `state_dir/settings.json`) are used.

**Send cap** (送り出しの上限, `TransportSettings.send_limit`, tx): the sender's MAXBW at the start of
each transmission, the `{maxbw_kbps}` placeholder of `profile.sender_args` (its 7th argument;
`round(mbps × 1000)`). The controller's pacing (`BUF_ND_PACE`) takes its nominal MAXBW from the
sender's fast stats (`mbpsMaxBW`), goes below it while the line limits and probes back up to it,
so this is the only knob (`BUF_ND_MAXBW_KBPS` is not set). Default 5.0 Mbps (`source: default`),
1.0–20.0 with two decimals; set from a line-speed test (`POST /network-tests/{id}/use`, `measured`)
or in the transport settings (`manual`). It is fixed when a transmission starts (retries and
reconnects keep it): a change while live is stored and used from the next start. The developer
log has a line with the cap at each start. A `sender_args` in device.toml with a number in place
of the placeholder keeps working: that number is the cap and the setting is not used (the log
line says so). A `settings.json` from before the cap existed loads with the default; a value
outside the range (edited by hand) is replaced by the default.

**Environment of the children.** A child does not inherit the daemon's whole environment: only
`PATH`, `HOME`, `USER`, `LOGNAME`, the locale (`LANG`, `LANGUAGE`, `LC_*`, `TZ`), `TMPDIR`, the
`XDG_*` directories, `DBUS_SESSION_BUS_ADDRESS`, `LD_LIBRARY_PATH`, `PYTHONPATH`, the plugin
and driver paths (`GST_PLUGIN_PATH`, `GST_PLUGIN_SYSTEM_PATH`, `GST_REGISTRY`,
`LIBVA_DRIVER_NAME`, `LIBVA_DRIVERS_PATH`, `SVTAV1_GST_PLUGIN_DIR`, `SVTAV1_LIB_DIR`) and the
names in `media.pass_env` (a trailing `*` is a prefix, e.g. `["GST_DEBUG*"]`). Then the profile
is applied on top. Variables exported for experiments in the shell that started the daemon
(`LAT_LOG`, `SVT_PRED_STRUCT`, `BUF_*` …) therefore never reach the children; put a setting in
the profile instead.

### Directories

| Setting | Default | Holds |
| --- | --- | --- |
| `run_dir` | `$XDG_RUNTIME_DIR/ctrld` (tmpfs) | one directory per session: the children's stderr logs (`sender.log`, `bridge.log`, `receiver.log`, `player.log`, each starting with the environment and command line), the statistics files the daemon reads, `daemon.log` (the developer log ring when the session ended). Without an archive only the current and the last finished session are kept, a log is cut back to its header once it passes 16 MiB and a statistics file is emptied once it passes 64 MiB. Outside the sessions (one per device): `preview.sock` (the video copy for the preview) and `preview.log` (the preview program's stderr and events, cut back like the others; the previous instance's is kept as `preview.log.1`), and on rx `nettest-server.log` (the bandwidth tool server's stdout and stderr, cut back like the others; the previous instance's is kept as `nettest-server.log.1`). |
| `state_dir` | `~/.local/state/ctrld` | `settings.json` (0600, replaced atomically) |
| `archive_dir` | unset | when set, session directories are created here and kept, for `bufctrl_analyze.py` and the like. `archive_video = true` (rx) also keeps the received stream. |

None of these logs is shown in the UI.

### What runs

- **tx**: the camera is checked first (it must open and not be in use; else `input_lost` with a
  5/10/20/30 s backoff, no connect loop). The input counts as working (`input_lost` cleared,
  `input_detected`) only once the bridge reports frames (`@evt {"input":"ok"}`; after 10 s live
  when the bridge reports no input events at all), and the backoff starts from 5 s again only
  after 10 s of frames, so a camera that opens but fails once it streams waits longer each
  time. Then the sender (`- <peer> <port> 8000 live 10000 <send cap, kbps>`);
  when it reports the connection, the bridge (stdout → sender stdin, stdin = control input,
  cwd `<repo>/sender`). Stop: `{"cmd":"stop"}` to the bridge, the sender drains its input
  (≤ `SRT_DRAIN_MAX_MS`), SIGTERM to the process groups after 6 s, SIGKILL after 8 s.
  Sender exit → `reconnecting`, the next try 5 s after the previous one started.
- **rx**: the player first (stdin = a 1 MiB pipe, stats on fd 4), then the receiver with the
  pipe as its fd 3 (`SRT_RCV_PIPE=/dev/fd/3`). Receiver exit → the old player plays out what it
  holds (≤ 30 s) while a new pair starts listening 0.5 s later.
- **rx stops** come from the player's stats: a stop is followed while it lasts (the alert and
  the 5-minute totals grow with it) and `video_stalled{seconds}` is sent with its whole length
  once the picture moves again. Stops shorter than 0.2 s (one late or missing source frame)
  are not reported.
- **preview** (both roles, `[preview]`, on by default): the bridge gets
  `PREVIEW_SOCK=<run_dir>/preview.sock` and the player `--preview-sock <run_dir>/preview.sock`;
  they copy the stream they send / show to that socket only while something reads it, and
  never wait for it. The preview program (`python3 -u <repo>/preview/preview_webrtc.py --sock …
  --width --height --h265-kbps --h264-kbps --keyint --delay-ms --max-viewers --idle-exit-s`,
  cwd `run_dir`) is started on the first preview request, not with the link, and is not part
  of the link's state machine: whether it runs, fails or is missing never changes the
  transmission. The daemon talks to it in JSON lines (stdin: `offer` / `close` / `quit`;
  stdout: `ready` / `answer` / `error` / `closed` / `stats`; `src/preview.rs`), waits 10 s for
  `ready` and `answer_timeout_s` for each answer, counts viewers (offers still waiting
  included) against `max_viewers`, and closes a viewer whose request went away or timed out.
  The program exits by itself `idle_exit_s` after the last viewer left; the next request starts
  it again (also after a crash). An offer that reached it just as it left by itself (exit 0)
  gets one more try on a new program; after a crash it is refused. A ready program that writes
  nothing for 20 s (it writes stats every 5 s) is taken as hung and stopped like at shutdown,
  so the next request starts a new one. Daemon shutdown: `quit`, SIGTERM after 2 s, SIGKILL
  after 4 s, at the same time as the media programs are stopped (a second signal exits at once). The preview needs media programs that
  know the socket (an older copy of the player refuses the unknown argument): set
  `[preview] enabled = false` for one.
- **network tests** (`[network_test]`, on by default; `src/nettest.rs`), started from the UI and
  not part of the link's state machine. Every program gets the inherited variables that pass
  (above) plus `LC_ALL=C`, cwd `run_dir`, its own process group and PDEATHSIG.
  - *reachability* (both roles, any time, also while live): `ping -n -q -c 10 -i 0.2 -W 1 <addr>`
    (`network_test.ping`) to the peer and to each `[[network_test.hops]]`, all at once (about
    3 s). The peer is tx `transport.peer_host` (the applied setting); rx `rx.peer_host`, else the
    last transmitter that connected, else left out. The summary lines (`N packets transmitted, M
    received`, `rtt min/avg/max/mdev`) give `sent`, `received` and the times; a run without a
    summary (unknown host, no route) or not done in 10 s counts as 10 sent, 0 back. Times in ms
    with two decimals.
  - *throughput* (tx only): one saturation test. The video travels in UDP, so the test does too:
    `iperf3 -c <peer> -p <port> -u -b <saturate>M -l 1316 -t <dur> -i 1 --json-stream
    --forceflush --get-server-output --connect-timeout 3000 [-R]` (`network_test.iperf`; the
    tool's control connection is TCP on the same port). It sends more than the line carries
    (`network_test.saturate_mbps`, default 6, 2–50) in datagrams of the video's payload (1316
    bytes), so what arrives is the line's speed; a test that lost 1 % or more (`loss_pct` with two
    decimals) filled the line (`saturated: true`), otherwise the line is faster than the test.
    SIGTERM `duration + 10 s` after the start (the client then tells the server it leaves),
    SIGKILL 2 s later. Field mapping, checked against iperf3 3.18 on localhost
    (`tests/fixtures/iperf3_udp*.jsonl`): `throughput_mbps`, `loss_pct`, `jitter_ms` =
    `end.sum_received` (the receiver's numbers in both directions), `sent_mbps` =
    `end.sum_sent.bits_per_second` (`end.sum` mixes the sending rate with the receiver's loss
    and jitter: only a fallback). `per_second_mbps` is what arrived in each second: without `-R`
    the server's own report that `--get-server-output` adds before `end` (`server_output_text`
    from a server in text mode, as rx runs it: the `[  5]   0.00-1.00   sec   734 KBytes  6.01
    Mbits/sec …` lines before the `- - -` line, `[SUM]` lines when there are several streams;
    `server_output_json` from a server started with `-J`), with `-R` the client's own `interval`
    lines (without `-R` those are the rate sent). A last sliver of an interval (< 0.25 s) is
    left out. While the test runs only `progress` moves (`result` stays null). Errors: the
    tool's `error` line "the server is busy" →
    `peer_busy`; "unable to connect" / refused / timed out / no route / unreachable / unknown host
    → `peer_unreachable`; stopped at the deadline before the test began (no `start` line, e.g.
    UDP dropped on the way) → `peer_unreachable`; everything else (crash, missing program, cut off
    after the start) → `failed`. The tool's messages go to the developer log only. A finished
    test adds `network_test_done {mbps, direction}` (mbps = `throughput_mbps`, two decimals, as
    the mock).
    `network_test.steady_rate_mbps` of the old steady mode is still read (and ignored), so an
    older device.toml loads.
  - *server* (rx by default, `network_test.server`): `iperf3 -s -p <port>` started with the
    daemon, restarted 1/2/5/10/30 s after each exit (from 1 s again after it ran 60 s), stopped
    on shutdown with the preview (SIGTERM, SIGKILL after 2 s). It never looks at the link, and
    the link never waits for it.
- Every child has its own process group and `PR_SET_PDEATHSIG=SIGTERM`; the runtime is
  single-threaded so the forking thread lives as long as the daemon. During the daemon's own
  shutdown start and retry requests are ignored.

Phase 1 API: everything in the contract is answered. Settings can be applied while stopped
(the next start uses them); while running a `PUT` answers `409 busy` (the settings screens
then tell the operator to stop first; the check endpoint still shows the live impacts phase 2
will have), except a transport `PUT` that changes only `send_limit`: it is stored while the
link runs (the running transmission keeps its cap). `send_limit` may be left out of the input
(the applied value is kept); a value outside 1.0–20.0 or not a number (`null` too) →
`send_limit.mbps: out_of_range {min, max}`, listed first; a new value becomes `source: manual`
(two decimals), the same value keeps its source. Impacts: a new cap alone while the link runs →
`info takes_effect_next_start` only (also while connecting or reconnecting: a reconnect keeps
the cap the transmission started with); with other fields
changed, the existing ones (`reconnect_gap` while live). Events: a new cap alone →
`send_limit_changed {mbps, from_mbps, source}` only; with other fields,
`transport_settings_applied` then `send_limit_changed`. An input must be chosen: an empty camera or file is a field error, and with no
file chosen yet the first file offered is the default. `/transport/compare` has no peer data
yet (`verdict: unknown`).

Network tests (phase 4): `POST /network-tests {kind, direction?, duration_s?}` → `202` with the
running test (`mode` and `rate_mbps` of the old modes are ignored). Checks in order: not JSON →
`400 invalid` (no errors); `kind` missing → `400 invalid` (`kind: required`), unknown → (`kind:
invalid`); throughput on rx, or `[network_test] enabled = false` → `409 unsupported_role`;
`direction` not in the enum → `invalid`, `duration_s` not an integer 5–30 → `out_of_range {min,
max}` (`400 invalid`; reachability ignores these fields); then `409 busy`: throughput while the link is
not stopped, while a start is on its way or while another throughput test runs, reachability
while another reachability test runs. While a throughput test runs `POST /transport/start`
answers `409 busy` (stop and retry are never refused). `GET /network-tests` → the last 20 (memory
only, newest first; a running test is never dropped), `GET /network-tests/{id}` → one or `404
not_found`, `GET /network-tests/options` → `{duration_range_s: [5, 30], saturate_mbps}`. The
request is echoed as `{kind, direction, duration_s}` (null for reachability); a throughput result
is `{throughput_mbps, sent_mbps, loss_pct, jitter_ms, saturated, per_second_mbps}`. While running,
`progress` is the measured seconds / duration (throughput) or an estimate from the time
(reachability), and `result` is null. `POST /network-tests/{id}/use` (tx) makes a finished test
towards the peer the send cap: `send_limit = {mbps: throughput_mbps (saturate_mbps when not
saturated), kept within 1.0–20.0, two decimals; source: measured, measured_at: finished_at,
test_id}`, the transport settings version + 1, `send_limit_changed {mbps, from_mbps, source}`, a
`settings` message on the live channel, `200` with the new `TransportSettings`; allowed while live
(the next start uses it). Errors in order: rx → `409 unsupported_role`, unknown id → `404
not_found`, not a finished throughput test towards the peer (running, failed, reachability,
`from_peer`) → `409 not_applicable`.

Preview (phase 3): `POST /preview/sessions {"offer": "<sdp>"}` (offer ≤ 64 KB) → `201 {id, answer}`.
The checks in order: a body that is not JSON, has no or an empty `offer`, does not start with
`v=0` or is over 64 KB (also a body over the server's 2 MB limit) → `400 bad_offer`; preview turned off → `503 preview_unavailable`; the
link not live → `409 not_live`; viewers (and offers waiting) at `max_viewers` →
`429 too_many_viewers`; then the program's verdict: `400 no_common_codec`, `400 bad_offer`, or
`503 preview_unavailable` (it did not start, was not ready or did not answer in time, or
failed). The first request may take up to 10 s plus `answer_timeout_s` while the program
starts. `DELETE /preview/sessions/{id}` → `204`, also for an unknown id. The old
`/preview.mjpeg` is gone (404) and `Media` has no `preview_fps`.

## User service (systemd)

One-time steps by an administrator (the daemon itself never needs root):

```
sudo usermod -aG video,render <user>     # camera and hardware decoder without a desktop session
sudo loginctl enable-linger <user>       # start the user service at boot
```

`~/.config/systemd/user/ctrld.service`:

```ini
[Unit]
Description=Device daemon (web UI and API)
After=network-online.target

[Service]
ExecStart=%h/ctrld/bin/ctrld --config %h/.config/ctrld/device.toml
Restart=on-failure
RestartSec=2
# SIGTERM to the daemon only: it stops the media programs itself (≤ 6 s), then exits.
KillMode=mixed
TimeoutStopSec=10

[Install]
WantedBy=default.target
```

```
systemctl --user daemon-reload && systemctl --user enable --now ctrld
journalctl --user -u ctrld -f            # the developer log
```

Keep the media programs used by the daemon in their own copy (e.g. `~/ctrld/media`, with the
bridge's curve file under `sender/output/...`), apart from the experiment scripts.

## Tests

`cargo test` covers the check rules (table tests, the mock as the oracle), the state machine
against a fake media backend (`src/supervisor.rs`), the statistics parsers on rows in the
programs' formats (`tests/fixtures/*.jsonl`), the history buckets, the event ring, the profile,
the settings store and the preview (with the stub program, below).

`src/nettest.rs` and `src/api.rs` run `tests/fixtures/stub_iperf3.py` and `stub_ping.py` (the
real programs' arguments only; they replay output captured from iperf3 3.18 and iputils ping
on the dev machine: `iperf3_udp.jsonl` / `iperf3_udp_reverse.jsonl` (`-u -b 6M -l 1316
--get-server-output`, both directions, against a text-mode server), `iperf3_udp_server_json.jsonl`
(the same against a server started with `-J`), `iperf3_udp_lossy.jsonl` /
`iperf3_udp_lossy_reverse.jsonl` (6 s, real loss of about 8 %: the same command with `-w 2K`
while the receiving side was paused 12 ms in every 100 ms; parser tests only),
`iperf3_refused.jsonl`, `iperf3_busy.jsonl`, `ping_ok.txt`, `ping_lost.txt` (100 % loss);
edited captures, as the dev machine has no slow path without root:
`iperf3_udp_saturated.jsonl` is the to_peer capture with the radio link's
numbers in the server's report and `end` (6 Mbps sent, 5.26 Mbps arrived, 5.8 % lost) and
`ping_partial.txt` the ok capture with its counts edited; the peer or hop address picks the
case, see the docstrings) for the parsers, the field mapping, the busy rules (live, running, a
start during a test), rx `unsupported_role`, the timeouts, the server's restarts and stop, the
history cap, and `/use` (its rules, the settings version, the event, persistence across a
restart and the sender's argument at the next start). The send cap's check rules and the live
`PUT` rule are table-tested in `src/check.rs` and over HTTP in `src/api.rs`; `{maxbw_kbps}`
(and a literal in an overriding `sender_args`) in `src/profile.rs`.

`src/preview.rs` and `src/api.rs` run `tests/fixtures/stub_preview.py` (canned answers, the
real program's arguments only, markers in an offer make it fail, stay silent, answer late, hang or
exit; see its docstring) for the preview manager's lifecycle, id matching, timeouts, the
viewer limit, restarts and the HTTP codes.

`tests/fixtures/stub_*.py` stand in for the four media programs (they print the known stderr
lines and write statistics rows), for a smoke test of the real process handling without a
camera, encoder or network: point `[media]` at them (`repo = ".../daemon/tests/fixtures"`,
`sender = "stub_sender.py"`, …, `[preview] program = "stub_preview.py"`) and drive them with the
control files described in each stub.
