#!/usr/bin/env python3
"""A raw Guardian protocol client for the integration gate (tools/eval/integration-gate.py).

Runs inside the test VM as an ordinary user. It speaks the wire protocol directly
(a 4-byte big-endian length, then JSON; core_protocol::wire) so the gate can do
what the agent never does: reuse confirmation tokens across connections, replay or
forge them, send invalid intents, flood the rate limit. Each command prints one
JSON object per line. Standard library only, because the image has no extra Python
packages.

    gate-client.py ping
    gate-client.py exec ACTION ARGS_JSON
    gate-client.py cross-session ACTION ARGS_JSON      token used from another connection, then replayed
    gate-client.py forged                             a token the Guardian never issued
    gate-client.py decline ACTION ARGS_JSON
    gate-client.py expire ACTION ARGS_JSON SECONDS    approve after waiting SECONDS
    gate-client.py hold ACTION ARGS_JSON TOKENFILE GOFILE   park a confirmation; approve on a new
                                                      connection once GOFILE exists
    gate-client.py flood ACTION ARGS_JSON COUNT
    gate-client.py oversized                          a frame over the 1 MiB limit
"""

import json
import os
import socket
import struct
import sys
import time

SOCKET = os.environ.get("CORE_GUARDIAN_SOCKET", "/run/core/guardian.sock")


class Conn:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.settimeout(120)
        self.s.connect(SOCKET)
        self.next_id = 1

    def send(self, msg):
        body = json.dumps(msg).encode()
        self.s.sendall(struct.pack(">I", len(body)) + body)

    def recv(self):
        head = self._exact(4)
        if head is None:
            return None
        (n,) = struct.unpack(">I", head)
        body = self._exact(n)
        return None if body is None else json.loads(body)

    def _exact(self, n):
        buf = b""
        while len(buf) < n:
            chunk = self.s.recv(n - len(buf))
            if not chunk:
                return None
            buf += chunk
        return buf

    def call(self, msg):
        try:
            self.send(msg)
        except (BrokenPipeError, ConnectionResetError):
            # The Guardian may answer and close before reading anything (a refused
            # peer): what it wrote is still there to read.
            pass
        return self.recv()

    def execute(self, action, args):
        rid = self.next_id
        self.next_id += 1
        return self.call({"type": "execute", "id": rid, "intent": {"action": action, "args": args}})

    def confirm(self, token, approve):
        return self.call({"type": "confirm", "token": token, "approve": approve})

    def close(self):
        self.s.close()


def out(**kv):
    print(json.dumps(kv, sort_keys=True), flush=True)


def summary(r):
    """The parts of a response the gate judges on (outputs can be long)."""
    if r is None:
        return {"type": "closed"}
    keep = {k: r[k] for k in ("type", "kind", "reason", "risk", "message", "server", "protocol") if k in r}
    if "token" in r:
        keep["has_token"] = True
    if r.get("type") == "executed":
        rep = r["report"]
        keep["success"] = rep["success"]
        keep["action"] = rep["action"]
        keep["dry_run"] = rep["dry_run"]
        fail = next((s for s in rep["steps"] if not s["success"]), None)
        if fail:
            keep["failure"] = {"description": fail["description"], "exit_code": fail.get("exit_code"),
                               "stderr": fail.get("stderr", "")[-300:]}
    return keep


def main(argv):
    cmd = argv[0]
    try:
        if cmd == "ping":
            c = Conn()
            out(step="hello", response=summary(c.call({"type": "hello", "client": "gate", "protocol": 1})))
            out(step="ping", response=summary(c.call({"type": "ping"})))
        elif cmd == "exec":
            c = Conn()
            out(step="execute", response=summary(c.execute(argv[1], json.loads(argv[2]))))
        elif cmd == "cross-session":
            a, b = Conn(), Conn()
            r = a.execute(argv[1], json.loads(argv[2]))
            out(step="request on A", response=summary(r))
            token = r.get("token")
            out(step="approve on B", response=summary(b.confirm(token, True)))
            out(step="approve on A", response=summary(a.confirm(token, True)))
            out(step="replay on A", response=summary(a.confirm(token, True)))
        elif cmd == "forged":
            c = Conn()
            out(step="forged token", response=summary(c.confirm("0" * 64, True)))
        elif cmd == "decline":
            c = Conn()
            r = c.execute(argv[1], json.loads(argv[2]))
            out(step="request", response=summary(r))
            out(step="decline", response=summary(c.confirm(r.get("token"), False)))
            out(step="approve after decline", response=summary(c.confirm(r.get("token"), True)))
        elif cmd == "expire":
            c = Conn()
            r = c.execute(argv[1], json.loads(argv[2]))
            out(step="request", response=summary(r), expires_in_secs=r.get("expires_in_secs"))
            time.sleep(float(argv[3]))
            out(step=f"approve after {argv[3]}s", response=summary(c.confirm(r.get("token"), True)))
        elif cmd == "hold":
            c = Conn()
            r = c.execute(argv[1], json.loads(argv[2]))
            out(step="request", response=summary(r))
            with open(argv[3], "w") as f:
                f.write("parked\n")
            while not os.path.exists(argv[4]):
                time.sleep(0.2)
            try:
                old = summary(c.confirm(r.get("token"), True))
            except OSError as e:
                old = {"type": "connection error", "error": type(e).__name__}
            out(step="approve on the old connection", response=old)
            out(step="approve on a new connection", response=summary(Conn().confirm(r.get("token"), True)))
        elif cmd == "flood":
            # Two connections taking turns: the limit is per user, not per connection.
            conns = [Conn(), Conn()]
            kinds = {}
            for i in range(int(argv[3])):
                s = summary(conns[i % 2].execute(argv[1], json.loads(argv[2])))
                key = s.get("kind") or s.get("type")
                kinds[key] = kinds.get(key, 0) + 1
            out(step="flood", counts=kinds)
        elif cmd == "oversized":
            c = Conn()
            c.s.sendall(struct.pack(">I", (1 << 20) + 1) + b"{")
            out(step="oversized frame", response=summary(c.recv()))
            try:
                after = summary(c.recv())
            except ConnectionResetError:
                # Closing with our unread bytes still queued makes the kernel reset
                # the connection instead of ending it cleanly: closed either way.
                after = {"type": "reset"}
            out(step="after the refusal", response=after)
        else:
            out(error=f"unknown command {cmd}")
            return 2
    except OSError as e:
        out(step=cmd, error=type(e).__name__, errno=e.errno, message=str(e))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
