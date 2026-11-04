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
(default `0.0.0.0:8080`), the directories, the media program paths and the **profiles**: the
environment and arguments of each child. The built-in defaults are the standard profile (the
current automatic mode, `src/profile.rs`); entries under `[profile.*_env]` replace a default
and an empty string removes it, so the controller settings being tuned are not fixed in the
daemon. The low-delay profile is layered on the resolved standard one and changed by
`[profile_low]` (below). The daemon adds the per-session file paths itself.

`tx.peer_host` / `tx.peer_port` are only the initial transport settings; after the first apply
the settings screen's values (in `state_dir/settings.json`) are used.

**途中の無線機** (`NetworkSettings`, both roles): the devices on the way to the peer (the radios of
the WRAN link). つながりの確認 checks them after the peer, and the radio line may carry, besides the
video, only the traffic with them (`link_filter.json`, below). The `[[network_test.hops]]` of
device.toml are the initial value while nothing is stored yet (`GET /settings/network` answers
them with version 1, and a change in device.toml takes effect at the next start); once a list
is applied in the UI it is stored in `settings.json` and **the stored list wins** over
device.toml from then on (also across restarts; an empty list too). To go back to device.toml's
list, remove the `network` member from `settings.json` while the daemon is stopped. The same
rules apply to both: at most 4, a label of 1–24 characters after trimming (stored trimmed), an
IPv4 address in dotted-quad form (no host names: the filter needs addresses), not 0.0.0.0,
255.255.255.255 or 127.x.x.x, no address twice; a device.toml that breaks them does not load,
and a `network` in `settings.json` edited by hand that breaks them is replaced by device.toml's
list. The screen also refuses this device's own addresses (every interface) and the peer's.

**Radio line filter** (`state_dir/link_filter.json`, `src/link_filter.rs`). The daemon keeps this
file up to date for a root-owned helper on the device (not in this repo; installed separately)
that watches it and reloads the packet filter of the WRAN port from it. The daemon itself never
needs root and never touches the filter.

```json
{
  "version": 1,
  "role": "tx",
  "peer": "192.168.0.210",
  "video_port": 60021,
  "test_port": 60022,
  "hosts": ["192.168.0.65", "192.168.0.56"]
}
```

- `version`: the file's format, 1. `role`: `tx` | `rx`.
- `peer`: the other device's IPv4 address, or null. tx: the transport settings' peer host; an
  IPv4 literal as it is, a host name (only possible from `tx.peer_host` in device.toml before
  the first apply: the screen takes addresses only) is looked up once with the system resolver
  in the background; until it answers, or when it fails (logged), the previous file's peer is
  kept, and a failed lookup is tried again at the first update a minute or more later (e.g. a
  resolver not ready yet at boot). rx: `rx.peer_host` from device.toml if set (a host name is
  looked up the same way; rx updates come with every change of the link), else the address of
  the last transmitter that connected (after a restart the previous file's, until one
  connects), else null.
- `video_port`: tx the peer port of the transport settings, rx `rx.listen_port`.
- `test_port`: `network_test.port` (the line-speed test: UDP, and TCP for its control connection).
- `hosts`: the addresses of 途中の無線機 (the applied list, else device.toml's), in their order.

Written at start, and again whenever its content changes: after `PUT /settings/network`, after
`PUT /settings/transport` (a new peer host or port), on rx when a transmitter connects, and when
a peer host name was looked up. Unchanged content is not rewritten. The file is written to
`link_filter.json.tmp` in the same directory, mode 0644 (whatever the umask), fsynced and renamed
over `link_filter.json`, so a reader never sees half a file; a watcher should react to the rename
(inotify `IN_MOVED_TO` on the directory, not `IN_MODIFY` on the file). Each write has a line in
the developer log.

**Send cap** (送り出しの上限, `TransportSettings.send_limit`, tx): the sender's MAXBW at the start of
each transmission, the `{maxbw_kbps}` placeholder of `profile.sender_args` (its 7th argument;
`round(mbps × 1000)`). The controller's pacing (`BUF_ND_PACE`) takes its nominal MAXBW from the
sender's fast stats (`mbpsMaxBW`), goes below it while the line limits and probes back up to it,
so this is the only knob (`BUF_ND_MAXBW_KBPS` is not set). Default 5.0 Mbps (`source: default`),
1.0–20.0 with two decimals; set from a line-speed test (`POST /network-tests/{id}/use`, `measured`)
or in the transport settings (`manual`). It is fixed when a transmission starts (retries and
reconnects keep it): a change while live is stored and used from the next start. The developer
log has a line with the profile, the cap and the bitrate at each start. A `sender_args` in device.toml with a number in place
of the placeholder keeps working: that number is the cap and the setting is not used (the log
line says so). A `settings.json` from before the cap existed loads with the default; a value
outside the range (edited by hand) is replaced by the default.

**Delay profiles** (遅延, `TransportSettings.delay`: `standard` / `low`, `src/profile.rs`). The
transmitter's transport settings choose one; every session of a transmission uses the profile of
the applied settings at its start (retries and reconnects keep it), for its commands, its
statistics (delay target, margin warning) and the developer log line at the start (`start:
profile low, send cap 5000 kbps (default), fixed 3000 kbps`). The low-delay profile (system repo
docs/12) is the resolved standard profile (defaults + `[profile]`) with these changes, then
`[profile_low]`:

| | standard | low |
| --- | --- | --- |
| sender | — | `SRT_SEND_PARTIAL=1` (small messages are sent at once; only here: in the standard profile it makes keyframes burst past the send cap, docs/12 §12.6) |
| bridge | automatic (`bridge_env`) | `SVT_PRESET=10`, `SVT_PARAMS=hierarchical-levels=3:lookahead=0`, always the fixed bitrate (`bridge_manual_env`: no automatic steps, no never-drop controller) with `INITIAL_TARGET` = the video settings' fixed bitrate (`encode.manual.bitrate_mbps`), whatever `encode.mode` says |
| player (rx) | the arguments (`--ts-offset-ms 7000`) | `player_ctl = ["ts-offset-ms 400", "adapt on"]`, written to the player when a low-delay transmitter connects |
| `delay_target_ms` | 9500 | 1500 |
| margin warning | `delay_margin_warn_s = 3.0` | none (`Stats.delay_margin_s` and `delay_margin_warn_s` null, `delay_margin_low` never raised) |

Unchanged in low: `SRT_NODROP=1`, `STREAM_ALIGN=tu`, `CAM_PTS=v4l2`, the send cap as the
sender's MAXBW (`{maxbw_kbps}`), the receiver's environment and arguments. `[profile_low]` takes
the same keys as `[profile]` (env values replace, an empty string removes; `sender_args` etc.
replace; `delay_target_ms`; `delay_margin_warn_s` turns a margin warning on) plus `enabled`
(default true; false: `low` is offered with `available: false`, a `PUT` with it is refused as
`delay.profile: invalid`, and a stored `low` runs as standard, which the start line says) and
`player_ctl` (a list of single lines; empty ones are dropped). `receiver_env`, `receiver_args`
and `player_args` of `[profile_low]` are not used: the receiver starts before it knows the
transmitter's profile. The video settings are never changed by the delay: while low is applied
`VideoOptions.auto_available` is false and a stored automatic mode applies again after going back
to standard.

The daemon (not the profile) gives every sender `SRT_STREAMID=delay=standard` or `delay=low`.
The receiver prints the caller's stream id (`[srt-recv] stream id <text>`) right before its
`accepted from` line; the daemon reads `delay=<profile>` from it (tokens separated by `;` or white
space, the first `delay=` counts, unknown tokens are ignored; missing or unknown: standard). The
player always starts with the standard profile's arguments plus `--ctl-fd 5` (a pipe; 6 when the
stats use 5; not added when `player_args` name one), whose write end the daemon keeps per session
(non-blocking; closed with the session, which the player takes as the end of control only). For
`low` it writes each `player_ctl` line there, before any video arrives (the player answers `[player]
ctl <line> ok` in `player.log`), and the session's statistics switch to the low profile; each step
has a developer-log line. rx `TransportSettings.delay` is `{profile: <the current or last
connection's>, follows_peer: true}` (standard before any; memory only); on rx a transport check
or `PUT` ignores `delay` (the stored value is kept). rx `Stats.delay_target_ms` = the profile's
target + max(0, the player's `ts_offset_ms` − the profile's first offset), to 100 ms (the player's
margin watch raises its offset when frames come late, so this is the delay aimed at now; the
first offset is the last `ts-offset-ms` line of `player_ctl`, else `--ts-offset-ms` of the
arguments; only while live, and only from stats lines read since the profile was set).

**受信機のモニター** (`OutputSettings`, rx; system repo `ll_player.py --ctl-fd`). The player can show
the picture itself in its own window on the receiver's desktop (`waylandsink`), about 0.5 s sooner
than the browser preview. `display` is `off` (default), `fullscreen` or `window`; it is applied at
once in every link state (no `busy`) and kept in `settings.json` (`output`; absent until the first
apply, version 1 `off` before; a member edited by hand that does not read is dropped, the rest of
the file kept). The daemon writes `display <mode>` to every player right after it starts (also
`off`; before any line a transmitter's stream id brings) and, on every apply, to the current
player (also the same value again: a retry by hand); a player playing out an earlier connection
has no control input any more and keeps its window until it exits. The player answers `[player]
ctl display <mode> ok` and reports `display` (`off` / `fullscreen` / `window` / `failed`) in its
stats lines; `failed` (no desktop to show it on, or it broke) while the setting is on makes the
daemon write the line again 30 s after it was last written, then every 30 s while it stays
`failed`; each change of what the player reports has a developer-log line. `Stats.display` is the
current player's report as `on` (full screen or a window) / `off` / `failed`, null on tx, while no
player runs (stopped, between two pairs) and until the player has said. HTTP: `GET
/settings/output` → `{version, display}`; `POST /settings/output/check` with `{version, display}` →
`CheckResult`; `PUT /settings/output` → `200` with the new `OutputSettings`; `POST /display
{"mode"}` (the operation screen's 低遅延表示 button: no version, no draft) → `200` with the new
`OutputSettings`. Checks in order: tx → `409 unsupported_role` (all four, whatever the body); not
JSON → `400 invalid` (no errors); PUT with another `version` → `409 version_conflict`; a value that
is not one of the three → `400 invalid` (`display: invalid`, for /display `mode: invalid`). Impacts
(only when the value changes): `info display_on` (to `fullscreen` or `window`) or `info
display_off`, then `info display_fullscreen_covers` when the new value is `fullscreen`. Both
changes bump the version (also without a change), store it, add the event `output_settings_applied
{display}`, push `{type: settings, section: output, version}` and answer once the supervisor loop
has given the player the line.

**Environment of the children.** A child does not inherit the daemon's whole environment: only
`PATH`, `HOME`, `USER`, `LOGNAME`, the locale (`LANG`, `LANGUAGE`, `LC_*`, `TZ`), `TMPDIR`, the
`XDG_*` directories, `DBUS_SESSION_BUS_ADDRESS`, `LD_LIBRARY_PATH`, `PYTHONPATH`, the plugin
and driver paths (`GST_PLUGIN_PATH`, `GST_PLUGIN_SYSTEM_PATH`, `GST_REGISTRY`,
`LIBVA_DRIVER_NAME`, `LIBVA_DRIVERS_PATH`, `SVTAV1_GST_PLUGIN_DIR`, `SVTAV1_LIB_DIR`) and the
names in `media.pass_env` (a trailing `*` is a prefix, e.g. `["GST_DEBUG*"]`). The player also
keeps the desktop's display (`WAYLAND_DISPLAY`, `DISPLAY`, `XAUTHORITY`) for the monitor output;
no other child gets them. A daemon started before the desktop session has none of them: the
player then finds `$XDG_RUNTIME_DIR/wayland-*` itself. Then the profile is applied on top. Variables exported for experiments in the shell that started the daemon
(`LAT_LOG`, `SVT_PRED_STRUCT`, `BUF_*` …) therefore never reach the children; put a setting in
the profile instead.

### Directories

| Setting | Default | Holds |
| --- | --- | --- |
| `run_dir` | `$XDG_RUNTIME_DIR/ctrld` (tmpfs) | one directory per session: the children's stderr logs (`sender.log`, `bridge.log`, `receiver.log`, `player.log`, each starting with the environment and command line), the statistics files the daemon reads, `daemon.log` (the developer log ring when the session ended). Without an archive only the current and the last finished session are kept, a log is cut back to its header once it passes 16 MiB and a statistics file is emptied once it passes 64 MiB. Outside the sessions (one per device): `preview.sock` (the video copy for the preview) and `preview.log` (the preview program's stderr and events, cut back like the others; the previous instance's is kept as `preview.log.1`), and on rx `nettest-server.log` (the bandwidth tool server's stdout and stderr, cut back like the others; the previous instance's is kept as `nettest-server.log.1`). |
| `state_dir` | `~/.local/state/ctrld` | `settings.json` (0600, replaced atomically), `link_filter.json` (0644, replaced atomically; see above) |
| `archive_dir` | unset | when set, session directories are created here and kept, for `bufctrl_analyze.py` and the like. `archive_video = true` (rx) also keeps the received stream. |

None of these logs is shown in the UI.

### What runs

- **tx**: the camera is checked first (it must open and not be in use; else `input_lost` with a
  5/10/20/30 s backoff, no connect loop). The input counts as working (`input_lost` cleared,
  `input_detected`) only once the bridge reports frames (`@evt {"input":"ok"}`; after 10 s live
  when the bridge reports no input events at all), and the backoff starts from 5 s again only
  after 10 s of frames, so a camera that opens but fails once it streams waits longer each
  time. Then the sender (`- <peer> <port> 8000 live 10000 <send cap, kbps>`, `SRT_STREAMID=delay=<profile>`);
  when it reports the connection, the bridge (stdout → sender stdin, stdin = control input,
  cwd `<repo>/sender`). Stop: `{"cmd":"stop"}` to the bridge, the sender drains its input
  (≤ `SRT_DRAIN_MAX_MS`), SIGTERM to the process groups after 6 s, SIGKILL after 8 s.
  Sender exit → `reconnecting`, the next try 5 s after the previous one started.
- **rx**: the player first (stdin = a 1 MiB pipe, stats on fd 4, control on fd 5, where the monitor output's
  `display <mode>` line comes first), then the receiver with the pipe as its fd 3 (`SRT_RCV_PIPE=/dev/fd/3`). Receiver exit → the old player plays out what it
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
  - *reachability* (both roles, any time, also while live): `ping -n -c 10 -i 0.2 -W 1 -w 4
    <addr>` (`network_test.ping`) to the peer and to each of 途中の無線機 (the applied list, else
    device.toml's hops; the list in effect when the test starts), all at once (about 2 s, at most
    4 s). The peer is tx `transport.peer_host` (the applied setting); rx `rx.peer_host`, else the
    last transmitter that connected, else left out. Why `-w 4` and no `-q`: once replies came,
    iputils waits only two round trips after the last request (`-W` applies only while nothing
    came back), so a late last reply on the radio line (RTT ≈ 75 ms, a reply now and then much
    later) was counted lost and a target showed 9/10; `-w 4` waits up to 4 s for ten replies.
    With a deadline iputils also goes on sending past the count while replies are missing (a
    silent address: `20 packets transmitted, 0 received`; one lost reply: an 11th request whose
    reply ends the run, `11 packets transmitted, 10 received`) and an error ends the run early
    (`6 packets transmitted, 0 received, +5 errors`). So the test is the first ten requests:
    `sent` is always 10, and `received` is the number of distinct requests (`icmp_seq` 1..10)
    with a reply line (duplicates, `DUP!`, count once; error lines such as "Destination Host
    Unreachable" are not replies; replies to the extra requests do not count). The times
    (`rtt min/avg/max/mdev`) come from the summary. A late reply is still counted lost if the
    reply to an extra request arrives before it (iputils then has its ten and stops); on a radio
    queue that keeps the order this does not happen. A run with a request missing, a reply slower
    than twice the median of its replies, or a duplicate gets one developer-log line, e.g. `reach
    192.168.0.56: 9/10 missing seq [7] late seq [4] rtt_ms [75.2, 74.8, 76.1, 612.0, 75.0, 74.6,
    75.9, 74.9, 75.3] extra seq [11] extra_rtt_ms [75.1]` (every reply's time by seq; `dup seq
    [..]` when there were duplicates; the replies to the extra requests last), so that a late
    reply and a lost one can be told apart; nothing changes in the API's result. A run without a
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
link runs (the running transmission keeps its cap), and the network and output settings (途中の
無線機, 受信機のモニター), which never touch the transmission and are applied in every state. `send_limit` may be left out of the input
(the applied value is kept); a value outside 1.0–20.0 or not a number (`null` too) →
`send_limit.mbps: out_of_range {min, max}`, listed first; a new value becomes `source: manual`
(two decimals), the same value keeps its source. Impacts: a new cap alone while the link runs →
`info takes_effect_next_start` only (also while connecting or reconnecting: a reconnect keeps
the cap the transmission started with); with other fields
changed, the existing ones (`reconnect_gap` while live). Events: a new cap alone →
`send_limit_changed {mbps, from_mbps, source}` only; with other fields,
`transport_settings_applied` then `send_limit_changed`. An input must be chosen: an empty camera or file is a field error, and with no
file chosen yet the first file offered is the default. `/transport/compare` has no peer data
yet (`verdict: unknown`). The transport settings are answered on rx too (its screen is not
designed yet): the same object and rules, except that `delay` follows the transmitter
(`follows_peer: true`) and is ignored in a check or `PUT`; the video settings and
`/transport/compare` stay `409 unsupported_role` there. Delay impacts: to `low` → `info
low_delay_fixed {bitrate_mbps}` (the fixed bitrate, while the applied video mode is auto) and
`info low_delay_effects`, after the usual ones (`reconnect_gap` with the new profile's delay,
`gap_s` 2 for low). Video check while low is applied: mode auto → `info low_delay_fixed
{bitrate_mbps}` (not an error), and a new mode alone gives no `switch_no_gap` / `auto_decides`
(nothing sent changes).

Network settings (途中の無線機, both roles, allowed in every link state: applying does not touch
the transmission): `GET /settings/network` → `{version, hops: [{label, address}]}`; `POST
/settings/network/check` with `{version, hops}` (the whole list) → `CheckResult`; `PUT
/settings/network` → `200` with the new `NetworkSettings`. Checks in order: not JSON → `400
invalid` (no errors); PUT with another `version` → `409 version_conflict`; then the field errors
(`400 invalid`, the same as /check): no list (missing, null, not an array) → `hops: required`;
more than 4 → `hops: too_many {max: 4}`; `hops[i].label` (i from 0) empty after trimming →
`required`, over 24 characters → `too_long {max: 24}`; `hops[i].address`, the first that
applies: `invalid` (not a dotted quad, or 0.0.0.0, 255.255.255.255, 127.x.x.x; a host name too)
→ `is_self` (an address of any of this device's interfaces) → `is_peer` (tx: the transport
settings' peer host and what the filter file has for it; rx: the configured or the last
connected transmitter) → `duplicate` (the same address as an earlier item). Impact: `info
hops_changed` when the list (labels trimmed) differs from the applied one, in any order or
content. A PUT bumps the version (also without a change), stores the list, adds the event
`network_settings_applied` (no params), pushes `{type: settings, section: network, version}` on
the live channel and updates `link_filter.json`.

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
`iperf3_refused.jsonl`, `iperf3_busy.jsonl`, and with iputils 20240905 and the daemon's
arguments `ping_ok.txt` (127.0.0.1: a reply line each), `ping_lost.txt` (192.0.2.1, which nothing
answers: 20 sent until the deadline, none back) and `ping_unreachable.txt` (an address that is
not there: "Destination Host Unreachable", 6 sent, parser test only);
edited captures, as the dev machine has no slow path without root:
`iperf3_udp_saturated.jsonl` is the to_peer capture with the radio link's
numbers in the server's report and `end` (6 Mbps sent, 5.26 Mbps arrived, 5.8 % lost),
`ping_partial.txt` the ok capture without three replies, plus the three extra requests iputils
sends for them, and its summary computed to match, and `ping_late.txt` the radio line in the
same format (request 7 lost, the reply to 4 late and out of order, an 11th request answered);
the peer or hop address picks the case, see the docstrings) for the parsers (reply
lines, duplicates, error lines, the late/missing log line), the field mapping, the busy rules
(live, running, a start during a test), rx `unsupported_role`, the timeouts, the server's
restarts and stop, the history cap, and `/use` (its rules, the settings version, the event,
persistence across a restart and the sender's argument at the next start).

途中の無線機: the rules are table-tested in `src/check.rs` (the mock's checkNetwork is the
oracle), device.toml's rules in `src/config.rs`, the stored list (and one edited by hand) in
`src/store.rs`, and over HTTP in `src/api.rs` (both roles, while live, version conflict, the
event and the live message, the defaults from device.toml, persistence across a restart, the
reachability test using the stored list). `src/link_filter.rs` tests the file (content and field
order, mode 0644 under any umask, the atomic replace, no rewrite without a change, the rx peer
across a restart, a peer host name looked up once with a fake resolver, a failed lookup keeping
the previous peer and tried again later); `src/api.rs` its content for tx and rx and its rewrite after `PUT
/settings/network` and `PUT /settings/transport` (a new peer host and port). The send cap's check rules and the live
`PUT` rule are table-tested in `src/check.rs` and over HTTP in `src/api.rs`; `{maxbw_kbps}`
(and a literal in an overriding `sender_args`) in `src/profile.rs`.

Delay profiles: `src/profile.rs` the low profile's layering (`[profile]` under it,
`[profile_low]` over it, `enabled`, `player_ctl`) and a low session's commands (stream id, small
messages, preset and parameters, the fixed-bitrate environment, the player's `--ctl-fd`),
`src/supervisor.rs` the stream id rules and the receiver following a transmitter (the lines
written to the player through the fake backend, standard without a stream id),
`src/stats.rs` the rx delay target from `ts_offset_ms` and tx without a margin, `src/check.rs`
the impacts, `src/api.rs` both roles over HTTP (options, `auto_available`, the plan's fixed
bitrate while the stored mode is auto, `enabled = false`, rx `follows_peer` and a draft's delay
ignored).

受信機のモニター: `src/check.rs` the rules (table, the mock's checkOutput as the oracle) and the
body of /display, `src/store.rs` the setting on disk (absent, kept, one edited by hand dropped),
`src/supervisor.rs` the lines through the fake backend (each new player gets it right after its
start and before the low-delay lines, an apply reaches the current player at once, nothing between
two pairs or while stopped, the 30 s retry while the player says `failed`, none on tx),
`src/stats.rs` `Stats.display` from the player's lines (the current player only, null before it
said and after it exited, null on tx), `src/media.rs` that only the player keeps `WAYLAND_DISPLAY`,
`DISPLAY` and `XAUTHORITY`, and `src/api.rs` both roles over HTTP (the order of the checks, the
version, the event, the live message, the command to the supervisor loop with the stored value,
persistence across a restart, tx refused).

`src/preview.rs` and `src/api.rs` run `tests/fixtures/stub_preview.py` (canned answers, the
real program's arguments only, markers in an offer make it fail, stay silent, answer late, hang or
exit; see its docstring) for the preview manager's lifecycle, id matching, timeouts, the
viewer limit, restarts and the HTTP codes.

`tests/fixtures/stub_*.py` stand in for the four media programs (they print the known stderr
lines and write statistics rows), for a smoke test of the real process handling without a
camera, encoder or network: point `[media]` at them (`repo = ".../daemon/tests/fixtures"`,
`sender = "stub_sender.py"`, …, `[preview] program = "stub_preview.py"`) and drive them with the
control files described in each stub (`media.pass_env = ["STUB_CTL"]`). `rx_stream_id` (its
content, e.g. `delay=low`) makes the stub receiver print a transmitter's stream id, and the stub
player follows its `--ctl-fd` lines as the real one does (and reports `ts_offset_ms` and `display`;
while `display_fail` exists, `display fullscreen|window` reports `failed`).
