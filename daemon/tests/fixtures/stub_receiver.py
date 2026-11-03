#!/usr/bin/env python3
"""Stand-in for the receiver in the daemon's smoke test (not a media program).

Opens $SRT_RCV_PIPE (the player's input), "listens", accepts a caller after 1 s, then copies about
3.5 Mbps of filler to the pipe and appends one stats row per second (the field names
srt_proper_receiver.c writes, including a failed-stats row with null counters) to
$SRT_RCV_STATS_JSONL. Control files in $STUB_CTL:
  rx_no_caller  nobody calls                          (stays until removed)
  rx_drop       the transmitter goes away: exit       (removed when used)
"""
import json
import os
import signal
import sys
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


out, port, lat = sys.argv[1], sys.argv[2], sys.argv[3]
log("[srt-recv] init")
path = os.environ["SRT_RCV_PIPE"]
pipe = open(path, "wb", buffering=0)
log(f"[srt-recv] pipe copy: {path} (unbuffered)")
signal.signal(signal.SIGTERM, lambda *_: (log("[srt-recv] stopped by signal 15"), os._exit(0)))
log(f"[srt-recv] listening on :{port} (latency={lat} ms)")
time.sleep(1.0)
while flag("rx_no_caller"):
    time.sleep(0.2)
log("[srt-recv] accepted from 127.0.0.1:51234")
stats = open(os.environ["SRT_RCV_STATS_JSONL"], "a", buffering=1)
seq = recv = loss = retr = byte = 0
total = 0
k = 0
while True:
    time.sleep(0.1)
    k += 1
    if flag("rx_drop", consume=True):
        log(f"[srt-recv] peer disconnected (Connection was broken) after {total} bytes")
        break
    chunk = b"\0" * 43750
    try:
        pipe.write(chunk)
    except BrokenPipeError:
        log("[srt-recv] pipe copy: write failed (Broken pipe), closing it")
        break
    total += len(chunk)
    if k % 10:
        continue
    seq += 1
    recv += 340
    loss += 7
    retr += 7
    byte += 462400
    if seq % 7 == 0:  # a failed stats call: the counters are null
        row = {"v": 1, "seq": seq, "t_us": int(time.monotonic() * 1e6), "t_ms": seq * 1000, "rc": -1, "tries": 1,
               "rcv_buf_valid": 0, "glitch": 0, "pktRecvTotal": None, "pktRcvLossTotal": None, "msRTT": None,
               "appMsgs": recv, "appBytes": total, "msgnoGapTotal": 0}
    else:
        row = {"v": 1, "seq": seq, "t_us": int(time.monotonic() * 1e6), "t_ms": seq * 1000, "rc": 0, "tries": 1,
               "rcv_buf_valid": 1, "glitch": 0, "pktRecvTotal": recv, "pktRecvUniqueTotal": recv - retr,
               "pktRcvLossTotal": loss, "pktRcvDropTotal": 0, "pktRcvUndecryptTotal": 0, "pktRcvRetrans": retr,
               "pktRcvBelated": 0, "pktRcvAvgBelatedTime": 0.0, "pktSentACKTotal": recv // 10,
               "pktSentNAKTotal": loss, "byteRecvTotal": byte, "byteRecvUniqueTotal": byte - retr * 1360,
               "byteRcvLossTotal": loss * 1360, "byteRcvDropTotal": 0, "msRcvBuf": 0, "pktRcvBuf": 0,
               "byteRcvBuf": 0, "byteAvailRcvBuf": 33554432, "msRcvTsbPdDelay": 0, "msRTT": 0.6,
               "mbpsRecvRate": 3.61, "appMsgs": recv, "appBytes": total, "padMsgs": 0, "padBytes": 0,
               "msgnoGapTotal": 0, "lastMsgno": recv, "lastPktseq": recv, "lastTsbpdUs": 0,
               "lastDeliverUs": int(time.monotonic() * 1e6), "lastLateUs": 0, "maxLateUs": None}
    stats.write(json.dumps(row, separators=(",", ":")) + "\n")
pipe.close()
log(f"[srt-recv] done, wrote {total} bytes")
