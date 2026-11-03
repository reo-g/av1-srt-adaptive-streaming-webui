#!/usr/bin/env python3
"""Stand-in for the encoder bridge in the daemon's smoke test (not a media program).

Writes about the target bitrate of filler to stdout (the sender's input), controller ticks and
events (the field names bufctrl.py writes) to $BUFCTRL_TICKS_JSONL / $BRIDGE_EVENTS_JSONL, and
follows the control input when BRIDGE_CTL_STDIN=1 ({"cmd":"stop"} or the end of stdin: finish).
Scripted line: at 8 s the controller steps down for delay (3.5 → 2.5, complete 2 s later); from
12 s to 30 s the line is crowded (capacity measured, the margin falls below 3 s); at 40 s it
steps back up (→ 3.0). With BRIDGE_CTL_STDIN=1 it reports its input as the real bridge does:
@evt input ok with the first frame, then lost / ok as frames stop and come back. Control files in $STUB_CTL:
  input_lost  the camera delivers no frames (@evt input lost / ok)    (stays until removed)
  src_err     the camera fails: [SRC ERR] and the bridge quits        (removed when used)
"""
import json
import os
import sys
import threading
import time

CTL = os.environ.get("STUB_CTL", "/nonexistent")


def flag(name, consume=False):
    p = os.path.join(CTL, name)
    if not os.path.exists(p):
        return False
    if consume:
        os.unlink(p)
    return True


def log(msg):
    print(msg, file=sys.stderr, flush=True)


env = os.environ.get
log(f"[bridge] SOURCE_KIND={env('SOURCE_KIND')} CAM_DEVICE={env('CAM_DEVICE')} SOURCE_Y4M={env('SOURCE_Y4M')} "
    f"INITIAL_TARGET={env('INITIAL_TARGET')} BUF_NODROP={env('BUF_NODROP')} TOTAL_SECONDS={env('TOTAL_SECONDS')} "
    f"cwd={os.getcwd()}")
stop = threading.Event()


def control():
    for line in sys.stdin:
        try:
            cmd = json.loads(line)
        except ValueError:
            log('@ctl {"ok":false,"error":"bad json"}')
            continue
        log("@ctl " + json.dumps({"ok": True, "cmd": cmd.get("cmd")}))
        if cmd.get("cmd") == "stop":
            break
    stop.set()  # a stop command or the end of the control input


if env("BRIDGE_CTL_STDIN") == "1":
    threading.Thread(target=control, daemon=True).start()

ticks = open(env("BUFCTRL_TICKS_JSONL"), "a", buffering=1)
events = open(env("BRIDGE_EVENTS_JSONL"), "a", buffering=1)
cur = int(env("INITIAL_TARGET", "3500"))
script = [(8.0, 2500, "nd-budget"), (40.0, 3000, "nd-up")]
pending = None
cmd_id = 0
input_state = None
t0 = time.monotonic()
last_status = 0.0


def head_delay(t):
    """H: capture age of the oldest unacknowledged frame (about 3 s normally)."""
    if 12 <= t < 18:
        return 3.1 + 0.7 * (t - 12)
    if 18 <= t < 22:
        return 7.3
    if 22 <= t < 28:
        return 7.3 - 0.7 * (t - 22)
    return 3.1


while not stop.is_set():
    time.sleep(0.1)
    t = time.monotonic() - t0
    if flag("src_err", consume=True):
        log("[SRC ERR] Could not read from resource.")
        os._exit(0)  # not sys.exit: the control reader thread would block the interpreter shutdown
    try:
        sys.stdout.buffer.write(b"\0" * (cur * 1000 // 8 // 10))
        sys.stdout.buffer.flush()
    except BrokenPipeError:
        log("[bridge] output closed")
        os._exit(1)
    want = "lost" if flag("input_lost") else "ok"
    if env("BRIDGE_CTL_STDIN") == "1" and want != input_state:
        input_state = want
        log("@evt " + json.dumps({"input": want}))
    if script and t >= script[0][0] and pending is None:
        _, to, cause = script.pop(0)
        cmd_id += 1
        events.write(json.dumps({"ev": "decide", "cmd": cmd_id, "cause": cause, "first_actor": "nd",
                                 "t": time.monotonic(), "wall_rel": t, "R_old": cur, "R_new": to,
                                 "state": "NORMAL", "raw": 0}) + "\n")
        log(f"[RATE] cmd={cmd_id} cause={cause} {cur}->{to}")
        pending = (t + 2.0, to, cmd_id)
    if pending and t >= pending[0]:
        cur = pending[1]
        events.write(json.dumps({"ev": "complete", "cmd": pending[2], "t": time.monotonic(), "wall_rel": t,
                                 "H_cmd": 2.3}) + "\n")
        log(f"[SWITCH @t={t:.1f} target={cur}]")
        pending = None
    H = head_delay(t)
    crowded = 12 <= t < 30
    ticks.write(json.dumps({"t": time.monotonic(), "t_rel": t, "state": "NORMAL", "cur": cur,
                            "pending": pending[1] if pending else None, "k": 0.93, "nd_H": round(H, 4),
                            "nd_c": 2900.0 if crowded else None, "nd_clb": None,
                            "nd_budget": round(9.5 - H, 4)}, separators=(",", ":")) + "\n")
    if t - last_status >= 5:
        last_status = t
        log(f"[STATUS t={t:.0f}s] target={cur} kbps")
log("[bridge] stop: finishing without the tail pad")
time.sleep(0.3)
log("[bridge] EOS, done")
os._exit(0)
