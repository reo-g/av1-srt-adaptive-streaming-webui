#!/usr/bin/env python3
"""Stand-in for the echo program (iputils ping) in the daemon's tests (src/nettest.rs).

Takes exactly the daemon's arguments `-n -q -c 10 -i 0.2 -W 1 ADDR` (anything else: exit 2 with a
usage line on stderr) and prints what iputils ping printed for them on the dev machine
(LC_ALL=C), with ADDR put in:
  ping_ok.txt       10 of 10 back (captured: 127.0.0.1), exit 0
  ping_lost.txt     none back, 100 % packet loss (captured: a broadcast ping on loopback, which the
                    kernel does not answer), exit 1
  ping_partial.txt  7 of 10 back: ping_ok.txt with the counts and times edited (the dev machine
                    has no lossy path without root), exit 0 as iputils does when some came back
ADDR picks the case: lost* → ping_lost, partial* → ping_partial, unknown.invalid → the real
"Name or service not known" error on stderr and exit 2, hang* → sleeps a minute first, slow* →
answers after 1 s; anything else → ping_ok after 0.1 s.
Every start is appended to stub_ping.jsonl in the working directory ({"args": [...]}).
"""
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))

args = sys.argv[1:]
if len(args) != 9 or args[:8] != ["-n", "-q", "-c", "10", "-i", "0.2", "-W", "1"]:
    print("ping: stub: unexpected arguments " + " ".join(args), file=sys.stderr)
    sys.exit(2)
addr = args[8]
with open("stub_ping.jsonl", "a") as f:
    f.write(json.dumps({"args": args}) + "\n")

if addr == "unknown.invalid":
    print(f"ping: {addr}: Name or service not known", file=sys.stderr)
    sys.exit(2)
if addr.startswith("hang"):
    time.sleep(60)
time.sleep(1.0 if addr.startswith("slow") else 0.1)
name, code, captured = "ok", 0, "127.0.0.1"
if addr.startswith("lost"):
    name, code, captured = "lost", 1, "127.255.255.255"
elif addr.startswith("partial"):
    name = "partial"
with open(os.path.join(HERE, f"ping_{name}.txt")) as f:
    sys.stdout.write(f.read().replace(captured, addr))
sys.exit(code)
