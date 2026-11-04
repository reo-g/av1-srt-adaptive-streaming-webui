#!/usr/bin/env python3
"""Stand-in for the preview program in the daemon's tests (not a media program).

Speaks the daemon's protocol like preview/preview_webrtc.py: JSON lines on stdin
({"cmd":"offer","id","sdp"} / {"cmd":"close","id"} / {"cmd":"quit"}, the end of stdin = quit) and
on stdout ({"evt":"ready"} / "answer" / "error" / "closed" / "stats"); logs go to stderr. Takes the
real program's arguments (and only those). Answers are canned: an offer with "H265/90000" gets
codec h265, else one with "H264/90000" h264, else error no_common_codec; an offer without
"m=video" gets bad_offer; one beyond --max-viewers gets failed. It exits --idle-exit-s after the
last viewer left (or after it got ready, if none came), not while an offer is waiting.

Markers in an offer change what happens to it:
  a=x-stub:silent   never answered (until the daemon closes it)
  a=x-stub:slow     answered after 1.5 s
  a=x-stub:fail     error "failed"
  a=x-stub:exit     the program exits (rc 1) instead of answering
  a=x-stub:drop     answered, then the viewer goes away by itself 0.3 s later (closed, why=failed)
  a=x-stub:hang     the program hangs: it reads and writes nothing more (SIGTERM still ends it)
  a=x-stub:leave    the program exits 0 at once without answering, as when its idle time ran out
  a=x-stub:leave-once  the same, but only the first time in a directory (flag STUB_PREVIEW_LOG.left)
STUB_PREVIEW_START (env): "noready" never says ready; "crash" exits 1 at once; "stubborn" ignores
  quit, the end of stdin and SIGTERM (only SIGKILL ends it).
STUB_PREVIEW_LOG (env): every start, command and exit is appended here (one JSON object a line).
STUB_PREVIEW_STATS_S (env): seconds between stats lines (default 5, as the real program).
"""
import argparse
import json
import os
import queue
import signal
import sys
import threading
import time

ap = argparse.ArgumentParser()
ap.add_argument("--sock", required=True)
ap.add_argument("--width", type=int, default=0)
ap.add_argument("--height", type=int, default=0)
ap.add_argument("--h265-kbps", type=int, default=16000)
ap.add_argument("--h264-kbps", type=int, default=24000)
ap.add_argument("--keyint", type=int, default=30)
ap.add_argument("--delay-ms", type=int, default=800)
ap.add_argument("--max-delay-ms", type=int, default=5000)
ap.add_argument("--max-viewers", type=int, default=3)
ap.add_argument("--idle-exit-s", type=float, default=30)
a = ap.parse_args()

START = os.environ.get("STUB_PREVIEW_START", "")
LOG = os.environ.get("STUB_PREVIEW_LOG")
STATS_S = float(os.environ.get("STUB_PREVIEW_STATS_S", "5"))
out_lock = threading.Lock()


def record(**kv):
    if LOG:
        with open(LOG, "a") as f:
            f.write(json.dumps({"pid": os.getpid(), **kv}) + "\n")


def emit(**kv):
    with out_lock:
        sys.stdout.write(json.dumps(kv) + "\n")
        sys.stdout.flush()


def log(msg):
    print(f"[preview-stub] {msg}", file=sys.stderr, flush=True)


record(start=True, args=sys.argv[1:])
log(f"start sock={a.sock} delay_ms={a.delay_ms} max_viewers={a.max_viewers}")
if START == "crash":
    log("crash at start")
    record(exit=1)
    sys.exit(1)
if START == "stubborn":
    signal.signal(signal.SIGTERM, signal.SIG_IGN)

cmds = queue.Queue()


def reader():
    for line in sys.stdin:
        cmds.put(line)
    cmds.put(None)


threading.Thread(target=reader, daemon=True).start()

ANSWER = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=video 9 UDP/TLS/RTP/SAVPF {pt}\r\na=sendonly\r\n"
viewers = {}  # id -> codec
waiting = {}  # id -> (due, codec) answered later; due None = never
drops = {}  # id -> when the viewer goes away by itself
idle_since = None
last_stats = 0.0
quit_asked = False


def stats():
    n = {"h265": 0, "h264": 0}
    for c in viewers.values():
        n[c] += 1
    emit(evt="stats", viewers=n, in_fps=30.0, out_fps={k: (30.0 if v else 0.0) for k, v in n.items()}, connected=True,
         late_frames=0, delay_ms=a.delay_ms)


def offer(oid, sdp):
    marks = {l[len("a=x-stub:"):].strip() for l in sdp.splitlines() if l.startswith("a=x-stub:")}
    if len(viewers) + len(waiting) >= a.max_viewers:
        return emit(evt="error", id=oid, code="failed", msg="viewer limit")
    if "m=video" not in sdp:
        return emit(evt="error", id=oid, code="bad_offer", msg="no video section")
    if "fail" in marks:
        return emit(evt="error", id=oid, code="failed", msg="told to fail")
    if "exit" in marks:
        log("told to exit")
        record(exit=1)
        os._exit(1)
    if "hang" in marks:
        log("told to hang")
        record(hang=True)
        while True:
            time.sleep(1)
    once = "leave-once" in marks and LOG and not os.path.exists(LOG + ".left")
    if "leave" in marks or once:
        if once:
            open(LOG + ".left", "w").close()
        log("idle: exit (told to leave)")
        record(exit=0, why="idle")
        sys.exit(0)
    codec = "h265" if "H265/90000" in sdp else "h264" if "H264/90000" in sdp else None
    if codec is None:
        return emit(evt="error", id=oid, code="no_common_codec", msg="no usable codec")
    if "silent" in marks:
        waiting[oid] = (None, codec)
    elif "slow" in marks:
        waiting[oid] = (time.monotonic() + 1.5, codec)
    else:
        answer(oid, codec)
        if "drop" in marks:
            drops[oid] = time.monotonic() + 0.3


def answer(oid, codec):
    viewers[oid] = codec
    emit(evt="answer", id=oid, sdp=ANSWER.format(pt=96 if codec == "h265" else 102), codec=codec)


if START != "noready":
    emit(evt="ready")
    idle_since = time.monotonic()
while True:
    try:
        line = cmds.get(timeout=0.05)
    except queue.Empty:
        line = ""
    now = time.monotonic()
    if line is None:
        record(cmd="eof")
        if START != "stubborn":
            break
        line = ""
    if line.strip():
        try:
            c = json.loads(line)
        except ValueError:
            log(f"bad command line: {line!r}")
            continue
        record(cmd=c.get("cmd"), id=c.get("id"))
        if c.get("cmd") == "offer":
            offer(c.get("id"), c.get("sdp", ""))
        elif c.get("cmd") == "close":
            oid = c.get("id")
            if viewers.pop(oid, None) is not None or waiting.pop(oid, None) is not None:
                drops.pop(oid, None)
                emit(evt="closed", id=oid, why="closed")
        elif c.get("cmd") == "quit":
            quit_asked = True
            if START != "stubborn":
                break
    for oid, (due, codec) in list(waiting.items()):
        if due is not None and now >= due:
            del waiting[oid]
            answer(oid, codec)
    for oid, due in list(drops.items()):
        if now >= due:
            del drops[oid]
            if viewers.pop(oid, None) is not None:
                emit(evt="closed", id=oid, why="failed")
    if viewers or waiting:
        idle_since = None
    elif idle_since is None and START != "noready":
        idle_since = now
    if idle_since is not None and now - idle_since >= a.idle_exit_s:
        log("idle: exit")
        record(exit=0, why="idle")
        sys.exit(0)
    if START != "noready" and now - last_stats >= STATS_S:
        last_stats = now
        stats()
record(exit=0, why="quit" if quit_asked else "eof")
log("quit")
