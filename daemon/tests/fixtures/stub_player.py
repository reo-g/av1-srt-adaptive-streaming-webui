#!/usr/bin/env python3
"""Stand-in for the player in the daemon's smoke test (not a media program).

Reads its stdin and writes one stats line per second to --stats-fd N with the keys of the
phase-1 player hook: t, frames, fps, stalls, stall_ms, last_gap_ms, pts. The picture stops once
for 0.43 s in its 6th second. At the end of its input it plays out for 1 s, then exits.
--ctl-fd N: control lines as ll_player.py takes them (ts-offset-ms <ms>, adapt on|off,
display off|fullscreen|window), each answered on stdout "[player] ctl <line> ok" (or "[player] ctl
ignored: <line>"); the stats lines then carry "ts_offset_ms" and "display" (off, fullscreen,
window, or failed). With adapt on, the offset goes up by one frame (33.3 ms) every 5 s. The monitor
output is only pretended: "display fullscreen|window" fails while the control file
$STUB_CTL/display_fail exists (as the real player does without a desktop to show it on); the next
such line after it is removed succeeds.
"""
import argparse
import json
import os
import signal
import sys
import threading
import time

ap = argparse.ArgumentParser()
ap.add_argument("--out")
ap.add_argument("--dec")
ap.add_argument("--ts-offset-ms")
ap.add_argument("--no-record", action="store_true")
ap.add_argument("--stats-fd", type=int)
ap.add_argument("--ctl-fd", type=int)
a, _ = ap.parse_known_args()
CTL_DIR = os.environ.get("STUB_CTL", "/nonexistent")
print(f"[player] start dec={a.dec} ts_offset_ms={a.ts_offset_ms} no_record={a.no_record}", flush=True)
signal.signal(signal.SIGTERM, lambda *_: (print("[player] stopped by SIGTERM", flush=True), os._exit(0)))
st = {"bytes": 0, "eof": False}


def reader():
    while True:
        b = sys.stdin.buffer.read1(1 << 16)
        if not b:
            break
        st["bytes"] += len(b)
    st["eof"] = True


threading.Thread(target=reader, daemon=True).start()
ctl = {"off": float(a.ts_offset_ms or 0), "adapt": False, "display": "off"}


def control():
    with os.fdopen(a.ctl_fd, "r") as f:
        for raw in f:  # EOF ends the reading, never the player
            w = raw.split()
            if len(w) == 2 and w[0] == "ts-offset-ms":
                ctl["off"] = float(w[1])
            elif len(w) == 2 and w[0] == "adapt" and w[1] in ("on", "off"):
                ctl["adapt"] = w[1] == "on"
            elif len(w) == 2 and w[0] == "display" and w[1] in ("off", "fullscreen", "window"):
                fails = w[1] != "off" and os.path.exists(os.path.join(CTL_DIR, "display_fail"))
                ctl["display"] = "failed" if fails else w[1]
            elif w:
                print(f"[player] ctl ignored: {raw.strip()}", flush=True)
                continue
            if w:
                print(f"[player] ctl {raw.strip()} ok", flush=True)


if a.ctl_fd is not None:
    threading.Thread(target=control, daemon=True).start()
fd = os.fdopen(a.stats_fd, "w", buffering=1) if a.stats_fd else None
frames = stalls = stall_ms = 0
seen = 0
k = 0
end_at = None
while True:
    time.sleep(1.0)
    k += 1
    got = st["bytes"] > seen
    seen = st["bytes"]
    fps = 30 if got or (end_at and time.monotonic() < end_at) else 0
    gap = 34.0
    if fps and frames >= 150 and stalls == 0:  # one stop, once the picture has run for 5 s
        stalls, stall_ms, gap, fps = 1, 433, 466.3, 17
    frames += fps
    if ctl["adapt"] and k % 5 == 0:
        ctl["off"] += 1000 / 30
    if fd:
        row = {"t": round(time.monotonic(), 3), "frames": frames, "fps": float(fps), "stalls": stalls,
               "stall_ms": stall_ms, "last_gap_ms": gap, "pts": round(frames / 30, 3)}
        if a.ctl_fd is not None:
            row["ts_offset_ms"] = round(ctl["off"], 1)
            row["display"] = ctl["display"]
        fd.write(json.dumps(row) + "\n")
    if st["eof"] and end_at is None:
        end_at = time.monotonic() + 1.0
    if end_at and time.monotonic() >= end_at:
        break
print(f"[player] done frames={frames} stalls={stalls}", flush=True)
