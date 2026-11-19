#!/usr/bin/python3
"""Keep the selected RPC port busy across exec of the real test engine."""
import json
import os
from pathlib import Path
import socket
import sys

root = Path(__file__).parent
journal = root / "attempts.jsonl"
previous = journal.read_text(encoding="utf-8").splitlines() if journal.exists() else []
for row in previous:
    assert not Path(f"/proc/{json.loads(row)['pid']}").exists(), "previous owned child was not reaped"
attempt = len(previous) + 1
port = int(next(arg.split("=", 1)[1] for arg in sys.argv[1:] if arg.startswith("--rpc-listen-port=")))
config = Path(sys.argv[sys.argv.index("--conf-path") + 1])
blocked = attempt <= int((root / "conflicts").read_text(encoding="utf-8"))
listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM) if blocked else None
if listener is not None:
    listener.bind(("127.0.0.1", port))
    listener.listen()
    listener.set_inheritable(True)
with journal.open("a", encoding="utf-8") as output:
    output.write(json.dumps({"attempt": attempt, "pid": os.getpid(), "port": port,
                             "blocked": blocked, "config": str(config), "old_children_absent": True}) + "\n")
# exec preserves the owned PID; the held socket makes real aria2 bind fail.
os.execv("/usr/bin/aria2c", ["aria2c", *sys.argv[1:]])
