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
| `run_dir` | `$XDG_RUNTIME_DIR/ctrld` (tmpfs) | one directory per session: the children's stderr logs (`sender.log`, `bridge.log`, `receiver.log`, `player.log`, each starting with the environment and command line), the statistics files the daemon reads, `daemon.log` (the developer log ring when the session ended). Without an archive only the current and the last finished session are kept, a log is cut back to its header once it passes 16 MiB and a statistics file is emptied once it passes 64 MiB. |
| `state_dir` | `~/.local/state/ctrld` | `settings.json` (0600, replaced atomically) |
| `archive_dir` | unset | when set, session directories are created here and kept, for `bufctrl_analyze.py` and the like. `archive_video = true` (rx) also keeps the received stream. |

None of these logs is shown in the UI.

### What runs

- **tx**: the camera is checked first (it must open and not be in use; else `input_lost` with a
  5/10/20/30 s backoff, no connect loop). The input counts as working (`input_lost` cleared,
  `input_detected`) only once the bridge reports frames (`@evt {"input":"ok"}`; after 10 s live
  when the bridge reports no input events at all), and the backoff starts from 5 s again only
  after 10 s of frames, so a camera that opens but fails once it streams waits longer each
  time. Then the sender (`- <peer> <port> 8000 live 10000 5000`);
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
- Every child has its own process group and `PR_SET_PDEATHSIG=SIGTERM`; the runtime is
  single-threaded so the forking thread lives as long as the daemon. During the daemon's own
  shutdown start and retry requests are ignored.

Phase 1 API: everything in the contract is answered. Settings can be applied while stopped
(the next start uses them); while running a `PUT` answers `409 busy` (the settings screens
then tell the operator to stop first; the check endpoint still shows the live impacts phase 2
will have). An input must be chosen: an empty camera or file is a field error, and with no
file chosen yet the first file offered is the default. `/preview.mjpeg` is 404
until phase 3, `/network-tests` answers `409 busy` until phase 4, `/transport/compare` has
no peer data yet (`verdict: unknown`).

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
programs' formats (`tests/fixtures/*.jsonl`), the history buckets, the event ring, the profile
and the settings store.

`tests/fixtures/stub_*.py` stand in for the four media programs (they print the known stderr
lines and write statistics rows), for a smoke test of the real process handling without a
camera, encoder or network: point `[media]` at them (`repo = ".../daemon/tests/fixtures"`,
`sender = "stub_sender.py"`, …) and drive them with the control files described in each stub.
