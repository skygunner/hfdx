# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# How to run: python scripts/qa/round2_network_expiry.py LOG_PATH
"""Controlled expiry fixture built on the existing sanitized HTTP journal."""
import json
import signal
import sys
import threading
from pathlib import Path
from urllib.parse import quote

import fixture


class Handler(fixture.Handler):
    def route(self, path, query):
        if "/resolve/" not in path:
            return super().route(path, query)
        if self.server.role == "origin":
            with self.server.state.condition:
                self.server.state.resolves += 1
                generation = self.server.state.resolves
            self.reply(302, Location=self.server.state.origins["capture"] + quote(path, safe="/") + f"?signature=round2-{generation}")
            return
        if self.command == "GET" and query.get("signature") == ["round2-1"]:
            self.reply(403)
            return
        super().route(path, query)


state = fixture.State(Path(sys.argv[1]), 0)
state.resolves = 0
servers = []
threads = []
stop = threading.Event()
signal.signal(signal.SIGTERM, lambda *_: stop.set())
try:
    for role in ("origin", "capture"):
        server = fixture.Server(("127.0.0.1", 0), Handler)
        server.state, server.role = state, role
        state.origins[role] = f"http://127.0.0.1:{server.server_port}"
        servers.append(server)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        threads.append(thread)
    print(json.dumps(state.origins), flush=True)
    stop.wait()
finally:
    for server in servers:
        server.shutdown()
        server.server_close()
    for thread in threads:
        thread.join()
    print(json.dumps({"cleanup": "listeners closed", "resolve_count": state.resolves}), flush=True)
