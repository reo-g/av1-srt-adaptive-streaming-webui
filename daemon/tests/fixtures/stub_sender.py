#!/usr/bin/env python3
"""Stand-in for the sender in the daemon's smoke test (not a media program).

Prints the sender's known stderr lines, reads the video from stdin and appends fast-stats rows
(the field names srt_proper_sender.c writes) to $SRT_FAST_STATS_JSONL every 100 ms. Ends at the
end of its input (a short drain) or at once on SIGTERM. Control files in $STUB_CTL:
  fail_connect  the connection fails after 1 s (exit 1)            (stays until removed)
  ack_stall     ACKs stop while data stays outstanding               (stays until removed)
  link_lost     the connection breaks (exit 1)                       (removed when used)
"""
import json
import os
import signal
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


host, port, lat = sys.argv[2], sys.argv[3], sys.argv[4]
log("[srt-sender] reading from stdin (live mode)")
log(f"[srt-sender] connecting to {host}:{port} (latency={lat} ms)")
if flag("fail_connect"):
    time.sleep(1.0)
    log("srt_connect: Connection setup failure: connection timed out")
    sys.exit(1)
time.sleep(0.3)
log("[srt-sender] nodrop: connected, tlpktdrop=0 snddropdelay=-1")
signal.signal(signal.SIGTERM, lambda *_: (log("[srt-sender] SIGTERM: closing the socket now"), os._exit(0)))

state = {"in": 0, "msgs": 0, "eof": False}


def reader():
    while True:
        b = sys.stdin.buffer.read1(1 << 16)
        if not b:
            break
        state["in"] += len(b)
        state["msgs"] += (len(b) + 1315) // 1316
    state["eof"] = True


threading.Thread(target=reader, daemon=True).start()
out = open(os.environ["SRT_FAST_STATS_JSONL"], "a", buffering=1)
t0 = time.monotonic()
seq = sent = retr = ack = bsent = 0
while not state["eof"]:
    time.sleep(0.1)
    if flag("link_lost", consume=True):
        log("[srt-sender] srt_send failed: Connection was broken")
        os._exit(1)  # not sys.exit: the stdin reader thread would block the interpreter shutdown
    seq += 1
    sent += 34
    retr += 1
    bsent += 34 * 1360
    stall = flag("ack_stall")
    if not stall:
        ack += 10
    now_us = int(time.monotonic() * 1e6)
    row = {
        "v": 1, "seq": seq, "t_us": now_us, "t_ms": int((time.monotonic() - t0) * 1000), "t0Us": int(t0 * 1e6),
        "rc": 0, "tries": 1, "buf_valid": 1, "consistent": 1, "glitch": 0,
        "pktSndBuf": 400 if stall else 120, "byteSndBuf": 157920, "msSndBuf": 310, "pktFlightSize": 95,
        "unsentPkts": 12, "byteAvailSndBuf": 33396080, "tLastUs": now_us - 2000, "aOldestMs": 312.0,
        "aOldestRingMs": 305.2, "inBytes": state["in"], "inMsgs": state["msgs"], "allMsgs": state["msgs"],
        "padMsgs": 0, "padBytes": 0, "firstMsgno": 1, "pktSentTotal": sent, "pktSentUniqueTotal": sent - retr,
        "pktRetransTotal": retr, "pktSndLossTotal": retr, "pktSndDropTotal": 0, "pktRecvACKTotal": ack,
        "pktRecvNAKTotal": retr, "byteSentTotal": bsent, "byteSentUniqueTotal": bsent - retr * 1360,
        "byteRetransTotal": retr * 1360, "byteSndDropTotal": 0, "msRTT": 0.7 + (seq % 5) * 0.1,
        "mbpsBandwidth": 912.345, "mbpsMaxBW": 5.4, "usPktSndPeriod": 1950.0, "pktFlowWindow": 25600,
        "msSndTsbPdDelay": 0,
    }
    out.write(json.dumps(row, separators=(",", ":")) + "\n")
drain_ms = int(os.environ.get("SRT_DRAIN_MAX_MS", "30000"))
log(f"[srt-sender] sent {state['in']} bytes; DRAIN (at most {drain_ms} ms)")
time.sleep(min(drain_ms, 500) / 1000)
log("[srt-sender] done")
os._exit(0)
