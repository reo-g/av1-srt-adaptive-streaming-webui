#!/usr/bin/env python3
"""Stand-in for the echo program (iputils ping) in the daemon's tests (src/nettest.rs).

Takes exactly the daemon's arguments `-n -c 10 -i 0.2 -W 1 -w 4 ADDR` (anything else: exit 2 with a
usage line on stderr) and prints what iputils ping (20240905) printed on the dev machine
(LC_ALL=C), with ADDR put in:
  ping_ok.txt       10 of 10 back, a reply line each (captured with these arguments: 127.0.0.1),
                    exit 0
  ping_lost.txt     none back (captured with these arguments: 192.0.2.1, which nothing answers;
                    with a deadline iputils goes on sending until it passes: "20 packets
                    transmitted, 0 received"), exit 1
  ping_partial.txt  7 of the 10 back: ping_ok.txt without the replies to 3, 6 and 9 and, as iputils
                    does with a deadline, three more requests (11-13) whose replies make up its
                    count of ten; summary computed to match (the dev machine has no lossy path
                    without root), exit 1 as iputils does when a reply is missing
  ping_late.txt     the radio line (about 75 ms): request 7 lost, the reply to 4 late (612 ms,
                    printed after the one to 6), an 11th request answered; written in the format
                    of ping_ok.txt with the summary computed to match, exit 1
(ping_unreachable.txt, a real capture of an address that is not there, "Destination Host
Unreachable", is read by the parser test only.)
ADDR picks the case: lost* → ping_lost, partial* → ping_partial, late* → ping_late,
unknown.invalid → the real "Name or service not known" error on stderr and exit 2, hang* → sleeps
a minute first, slow* → answers after 1 s; anything else → ping_ok after 0.1 s.
Every start is appended to stub_ping.jsonl in the working directory ({"args": [...]}).
"""
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))

args = sys.argv[1:]
if len(args) != 10 or args[:9] != ["-n", "-c", "10", "-i", "0.2", "-W", "1", "-w", "4"]:
    print("ping: stub: unexpected arguments " + " ".join(args), file=sys.stderr)
    sys.exit(2)
addr = args[9]
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
    name, code, captured = "lost", 1, "192.0.2.1"
elif addr.startswith("partial"):
    name, code = "partial", 1
elif addr.startswith("late"):
    name, code, captured = "late", 1, "192.168.0.56"
with open(os.path.join(HERE, f"ping_{name}.txt")) as f:
    sys.stdout.write(f.read().replace(captured, addr))
sys.exit(code)
