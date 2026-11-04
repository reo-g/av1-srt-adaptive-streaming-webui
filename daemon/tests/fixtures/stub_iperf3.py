#!/usr/bin/env python3
"""Stand-in for the bandwidth tool (client and server) in the daemon's tests (src/nettest.rs).

Client: takes exactly the arguments the daemon gives the real client
  -c HOST -p PORT -u -b RATEM -l 1316 -t SECS -i 1 --json-stream --forceflush --get-server-output
  --connect-timeout 3000 [-R]
(anything else: exit 1 with a usage line on stderr) and replays the JSON-stream output captured
from iperf3 3.18 on localhost (iperf3_*.jsonl next to this file): iperf3_udp, with -R
iperf3_udp_reverse. HOST picks a case:
  lossy-*           the line was full: iperf3_udp_saturated.jsonl (the to_peer capture with the
                    radio link's numbers, 5.26 Mbps arrived, 5.8 % lost; without -R only)
  refused.example   the captured "Connection refused" error (iperf3_refused.jsonl), exit 1
  busy.example      the captured "server is busy" error (iperf3_busy.jsonl), exit 1
  hang.example      the start line, then nothing more (SIGTERM ends it)
  silent.example    nothing at all, as when no answer comes (SIGTERM ends it)
  stubborn.example  like silent, but SIGTERM is ignored (only SIGKILL ends it)
  crash.example     exits 1 at once, a message on stderr, nothing on stdout
  slow-*            the normal replay with 0.25 s between lines (else 0.02 s)
Every client start is appended to stub_iperf3.jsonl in the working directory ({"args": [...]}).

Server (-s -p PORT, nothing else): prints the real server's banner and waits until SIGTERM.
STUB_IPERF3_SERVER (env): "exit" exits 1 at once with the real "Address already in use" error;
"stubborn" ignores SIGTERM. Every server start and its end are appended to
stub_iperf3_server.jsonl in the working directory.
"""
import json
import os
import signal
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))


def log(name, obj):
    with open(name, "a") as f:
        f.write(json.dumps(obj) + "\n")


def usage():
    print("iperf3: parameter error - stub: unexpected arguments " + " ".join(sys.argv[1:]), file=sys.stderr)
    sys.exit(1)


def wait_forever():
    while True:
        time.sleep(1)


def server(args):
    if len(args) != 3 or args[1] != "-p" or not args[2].isdigit():
        usage()
    mode = os.environ.get("STUB_IPERF3_SERVER", "")
    log("stub_iperf3_server.jsonl", {"start": args, "pid": os.getpid()})
    if mode == "exit":
        print("iperf3: error - unable to start listener for connections: Address already in use", file=sys.stderr)
        print("iperf3: exiting", file=sys.stderr)
        log("stub_iperf3_server.jsonl", {"exit": 1})
        sys.exit(1)
    if mode == "stubborn":
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
    else:
        def term(*_):
            log("stub_iperf3_server.jsonl", {"term": True})
            sys.exit(0)
        signal.signal(signal.SIGTERM, term)
    print("-----------------------------------------------------------", flush=True)
    print(f"Server listening on {args[2]} (test #1)", flush=True)
    print("-----------------------------------------------------------", flush=True)
    wait_forever()


def client(args):
    # the daemon's order: the fixed part, then -R or nothing
    fixed = ["-c", None, "-p", None, "-u", "-b", None, "-l", "1316", "-t", None, "-i", "1", "--json-stream",
             "--forceflush", "--get-server-output", "--connect-timeout", "3000"]
    if len(args) not in (len(fixed), len(fixed) + 1):
        usage()
    if any(want is not None and got != want for want, got in zip(fixed, args)):
        usage()
    host, rest = args[1], args[len(fixed):]
    if not args[3].isdigit() or not args[10].isdigit() or not args[6].endswith("M"):
        usage()
    float(args[6][:-1])
    if rest not in ([], ["-R"]):
        usage()
    reverse = rest == ["-R"]
    log("stub_iperf3.jsonl", {"args": args})
    if host == "crash.example":
        print("iperf3: stub crash", file=sys.stderr)
        sys.exit(1)
    if host in ("silent.example", "stubborn.example"):
        if host == "stubborn.example":
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
        wait_forever()
    name = {"refused.example": "refused", "busy.example": "busy"}.get(host)
    if name is None:
        name = "udp_reverse" if reverse else "udp_saturated" if host.startswith("lossy-") else "udp"
    with open(os.path.join(HERE, f"iperf3_{name}.jsonl")) as f:
        lines = f.read().splitlines()
    pace = 0.25 if host.startswith("slow-") else 0.02
    for line in lines:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()
        if host == "hang.example":
            wait_forever()
        time.sleep(pace)
    sys.exit(1 if name in ("refused", "busy") else 0)


def main():
    args = sys.argv[1:]
    if args[:1] == ["-s"]:
        server(args)
    elif args[:1] == ["-c"]:
        client(args)
    else:
        usage()


main()
