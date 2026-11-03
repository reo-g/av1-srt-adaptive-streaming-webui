#!/usr/bin/env python3
"""Stand-in for the player in the daemon's smoke test (not a media program).

Reads its stdin and writes one stats line per second to --stats-fd N with the keys of the
phase-1 player hook: t, frames, fps, stalls, stall_ms, last_gap_ms, pts. The picture stops once
for 0.43 s in its 6th second. At the end of its input it plays out for 1 s, then exits.
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
a, _ = ap.parse_known_args()
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
    if fd:
        fd.write(json.dumps({"t": round(time.monotonic(), 3), "frames": frames, "fps": float(fps), "stalls": stalls,
                             "stall_ms": stall_ms, "last_gap_ms": gap, "pts": round(frames / 30, 3)}) + "\n")
    if st["eof"] and end_at is None:
        end_at = time.monotonic() + 1.0
    if end_at and time.monotonic() >= end_at:
        break
print(f"[player] done frames={frames} stalls={stalls}", flush=True)
