#!/usr/bin/env python3
"""The integration gate: C.O.R.E. on its own OS, through the real Guardian.

    integration-gate.py --image IMAGE --repo REPO --out DIR [--self-test]

Boots one disposable copy-on-write snapshot of an image that has the core-os
package and an interactive user (`mkimage.sh --user core`), and checks the real
path: the user's process, the agent loop in core-shell, the Guardian's socket,
its peer check, policy, confirmations, the runner, the audit log and systemd's
socket activation. Nothing is replayed as root on the host's behalf: requests
come from the non-root user `core`, whose only way to change the system is the
Guardian. Intents come from the rescue planner or from scripts (`core-shell
--backend script`, docs/TRAINING.md D9); no model is used, so this gate says
nothing about a trained model (that is the product acceptance gate).

Every check is judged on its own and recorded in DIR/results.json with what it
saw. Also written: the console log, the Guardian's audit log, and a catalog
availability table (which actions this image can actually carry out). Exits 0
only if every check passed.
"""

import argparse
import importlib.util
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))
MNT = "/mnt/gate"
GUARDIAN_CONFIG = "/etc/core/guardian.toml"
AUDIT = "/var/log/core/audit.jsonl"
USER = "core"
INTRUDER = "intruder"


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


vmrun = load_module("vmrun", os.path.join(HERE, "vm-run.py"))

# Scripted episodes for core-shell (`--backend script`): each line is one intent.
RESPOND = {"action": "respond", "args": {"message": "Done."}}
SCRIPTS = {
    "install-nano": [{"action": "install_package", "args": {"package": "nano"}}, RESPOND],
    "enable-timesyncd": [{"action": "enable_service", "args": {"service": "systemd-timesyncd", "now": True}},
                         RESPOND],
    "start-timesyncd": [{"action": "start_service", "args": {"service": "systemd-timesyncd"}}, RESPOND],
    "remove-glibc": [{"action": "remove_package", "args": {"package": "glibc"}}, RESPOND],
    "start-missing": [{"action": "start_service", "args": {"service": "no-such-unit-gate"}}, RESPOND],
    # Each refused intent is followed by a harmless one, so the audit log proves the
    # script ran on past it (and that the refused intent itself never got there).
    "read-shadow": [{"action": "read_file", "args": {"path": "/etc/shadow"}},
                    {"action": "disk_usage", "args": {}}, RESPOND],
    "invalid-package": [{"action": "install_package", "args": {"package": "-rf"}},
                        {"action": "disk_usage", "args": {}}, RESPOND],
    "timezone-berlin": [{"action": "set_timezone", "args": {"timezone": "Europe/Berlin"}}, RESPOND],
}

# Arguments for checking each Guardian action against this image (availability).
# Changes are only planned, never run; read-only actions are also executed.
EXAMPLES = {
    "read_logs": {"lines": 5}, "read_kernel_log": {"lines": 5}, "disk_usage": {}, "list_processes": {"limit": 5},
    "service_status": {"service": "systemd-journald"}, "list_services": {"state": "running"},
    "restart_service": {"service": "systemd-timesyncd"}, "start_service": {"service": "systemd-timesyncd"},
    "stop_service": {"service": "systemd-timesyncd"}, "enable_service": {"service": "systemd-timesyncd"},
    "disable_service": {"service": "systemd-timesyncd"}, "search_packages": {"query": "editor"},
    "package_info": {"package": "bash"}, "install_package": {"package": "nano"},
    "remove_package": {"package": "nano"}, "update_system": {}, "network_status": {}, "wifi_scan": {},
    "ping_host": {"host": "127.0.0.1", "count": 1}, "wifi_connect": {"ssid": "Home", "passphrase": "gate-passphrase"},
    "set_link": {"interface": "lo", "up": True}, "list_block_devices": {}, "list_hardware": {"bus": "pci"},
    "set_volume": {"percent": 40}, "set_mute": {"muted": True}, "set_brightness": {"percent": 50},
    "load_kernel_module": {"module": "loop"}, "unload_kernel_module": {"module": "loop"},
    "kill_process": {"pid": 4242}, "set_hostname": {"hostname": "core-lab"}, "set_timezone": {"timezone": "UTC"},
    "configure_swap": {"size_mb": 64}, "reboot": {}, "poweroff": {},
}


class Gate:
    def __init__(self, vm, out):
        self.vm = vm
        self.out = out
        self.checks = []
        self.transcript = []

    def sh(self, command, timeout=None):
        out, rc = self.vm.run(command, timeout)
        self.transcript.append({"command": command, "rc": rc, "output": out[-4000:]})
        return out, rc

    def as_user(self, user, command, stdin=None, timeout=None):
        inner = f"cd /tmp && {command}"
        if stdin is not None:
            inner = f"printf {shlex.quote(stdin)} | ( {command} )"
            inner = f"cd /tmp && {inner}"
        return self.sh(f"su -s /bin/sh {user} -c {shlex.quote(inner)}", timeout)

    def client(self, user, *args, timeout=None):
        out, rc = self.as_user(user, " ".join(["python3", f"{MNT}/gate-client.py", *map(shlex.quote, args)]),
                               timeout=timeout)
        rows = []
        for line in out.splitlines():
            line = line.strip()
            if line.startswith("{"):
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
        return rows, out

    def check(self, group, name, passed, **seen):
        self.checks.append({"group": group, "name": name, "passed": bool(passed), "seen": seen})
        print(f"[{'ok' if passed else 'FAIL'}] {group}: {name}", flush=True)
        return passed

    def audit_len(self):
        out, _ = self.sh(f"wc -l < {AUDIT} 2>/dev/null || echo 0")
        return int(out.split()[-1]) if out.split() else 0

    def audit_since(self, n):
        out, _ = self.sh(f"tail -n +{n + 1} {AUDIT}")
        entries = []
        for line in out.splitlines():
            line = line.strip()
            if line.startswith("{"):
                entries.append(json.loads(line))
        return entries

    def value(self, command):
        return self.sh(command)[0].strip()

    def timezone(self):
        return self.value("timedatectl show -p Timezone --value")


def decisions(entries):
    return [e.get("decision") for e in entries]


def response_of(rows, step):
    return next((r.get("response", {}) for r in rows if r.get("step") == step), {})


def interactive_login(vm, password, new_password, boot_timeout):
    """`core` logs in on the serial console like a person, changes the expired
    password, and gets core-shell. One request needs confirmation: answer yes."""
    con = vm.con
    seen = {}
    con.expect(r"login: ", boot_timeout)
    con.send(f"{USER}\n")
    con.expect(r"[Pp]assword: ", 60)
    con.send(password + "\n")
    # A user (unlike root) is asked for the old password first.
    prompt = con.expect(r"(?i)(current|old|new) password: ", 60)
    seen["forced_password_change"] = True
    if re.search(r"(?i)(current|old) password: $", prompt):
        con.send(password + "\n")
        con.expect(r"(?i)new password: ", 60)
    con.send(new_password + "\n")
    con.expect(r"(?i)(retype|re-enter).*password: ", 60)
    con.send(new_password + "\n")
    banner = con.expect(r"say what you need[\s\S]*?(›|>) ", 120)
    seen["banner"] = vmrun.parse_output(banner + "\n__RC=0\n", vm.bt)[0][-600:]
    con.send("hostname core-lab\r")
    asked = con.expect(r"Allow\? \[y/N\] ", 180)
    seen["confirmation"] = vmrun.parse_output(asked + "\n__RC=0\n", vm.bt)[0][-600:]
    con.send("y\r")
    after = con.expect(r"(›|>) ", 180)
    seen["after"] = vmrun.parse_output(after + "\n__RC=0\n", vm.bt)[0][-600:]
    con.send("/exit\r")
    # getty prints a new login prompt; VM.login (root) consumes it.
    return seen


def run_gate(a, gate):
    g = gate
    fp_host = a.contract
    uid = None

    # ---- the image as installed ------------------------------------------------
    g.sh(f"mkdir -p {MNT} && mount -o ro /dev/vdb {MNT}")
    g.sh(f"printf '[[repo]]\\nlocation = \"{MNT}/repo\"\\n' >> /etc/cpkg/repos.toml")
    fp_image = g.value("core-ctl contract | sed -n 's/^fingerprint: *//p'")
    g.check("image", "the image's contract fingerprint matches core-ctl built from this checkout",
            fp_image == fp_host, image=fp_image, host=fp_host, head=a.head)
    g.check("image", "core-os is installed and intact",
            g.sh("cpkg verify core-os")[1] == 0, info=g.value("cpkg info core-os | head -3"))
    g.check("image", "core-guardian.socket is enabled and listening",
            g.value("systemctl is-enabled core-guardian.socket; systemctl is-active core-guardian.socket")
            == "enabled\nactive")
    g.check("image", "core-sensed is running and publishing",
            g.sh("systemctl is-active --quiet core-sensed && test -s /run/core-sense/telemetry.json")[1] == 0)
    stat = g.value("stat -c '%a %U %G' /run/core/guardian.sock")
    g.check("image", "the socket is 0660 root:core", stat == "660 root core", stat=stat)
    stat = g.value(f"stat -c '%a %U %G' {GUARDIAN_CONFIG}")
    g.check("image", "the policy file is 0644 root:root", stat == "644 root root", stat=stat)
    ids = g.value(f"id -nG {USER}")
    uid = int(g.value(f"id -u {USER}"))
    g.check("image", f"{USER} is a non-root member of core", "core" in ids.split() and uid != 0, groups=ids, uid=uid)
    g.check("image", "no autologin is configured anywhere systemd reads units from",
            g.sh("! grep -rqs -- --autologin /etc/systemd /usr/lib/systemd /run/systemd")[1] == 0)
    g.sh(f"useradd --create-home {INTRUDER}")
    ids = g.value(f"id -nG {INTRUDER}")
    g.check("image", f"{INTRUDER} (test fixture) is not in core", "core" not in ids.split(), groups=ids)

    # ---- the normal login path (done before root logged in) -----------------------
    n0 = 0
    entries = [e for e in g.audit_since(n0) if e.get("action") == "set_hostname"]
    g.check("login", f"{USER} logged in with a password and had to change it (normal login, no autologin)",
            a.login_seen.get("forced_password_change") is True, banner=a.login_seen.get("banner"))
    g.check("login", "core-shell asked the human to confirm the hostname change, at the terminal",
            "Allow?" in a.login_seen.get("confirmation", ""), confirmation=a.login_seen.get("confirmation"))
    g.check("login", "the hostname changed after the human said yes", g.value("hostname") == "core-lab",
            hostname=g.value("hostname"))
    g.check("login", "the audit log has the confirmation and the confirmed change, from the user's uid",
            [e.get("decision") for e in entries] == ["confirmation_required", "confirmed"]
            and all(e["peer"]["uid"] == uid for e in entries) and entries[-1].get("success") is True,
            entries=entries)

    # ---- the agent path: core-shell as the user ------------------------------------
    def shell(script, request, answers=None, backend="script"):
        n = g.audit_len()
        args = ["core-shell", "--backend", backend]
        if backend == "script":
            args += ["--script", f"{MNT}/scripts/{script}.jsonl"]
        args += ["-c", request]
        out, rc = g.as_user(USER, " ".join(map(shlex.quote, args)), stdin=answers if answers is not None else "")
        return out, rc, g.audit_since(n)

    g.sh("cpkg remove -- nano")
    out, rc, au = shell("install-nano", "Install nano.", "y\n")
    # Without a terminal the shell does not print "Allow? [y/N]"; it prints the action
    # and its risk ("! Install package nano  [high risk]") and reads the answer.
    g.check("agent", "high-risk install asks for confirmation; yes installs it from the signed repository",
            "[high risk]" in out and g.sh("test -x /usr/bin/nano && cpkg verify nano")[1] == 0
            and decisions(au) == ["confirmation_required", "confirmed"] and au[-1].get("success") is True,
            rc=rc, decisions=decisions(au), output=out[-800:])

    g.sh("systemctl disable --now systemd-timesyncd")
    out, rc, au = shell("enable-timesyncd", "Keep the clock synchronised.", "n\n")
    state = g.value("systemctl is-enabled systemd-timesyncd; systemctl is-active systemd-timesyncd")
    g.check("agent", "medium-risk change declined: nothing runs",
            "[medium risk]" in out and state == "disabled\ninactive" and decisions(au) == ["confirmation_required", "declined"],
            state=state, decisions=decisions(au), output=out[-600:])

    out, rc, au = shell("timezone-berlin", "Use Berlin time.", "")
    g.check("agent", "end of input at the confirmation prompt means no",
            g.timezone() != "Europe/Berlin" and decisions(au) == ["confirmation_required", "declined"],
            timezone=g.timezone(), decisions=decisions(au))

    out, rc, au = shell("start-timesyncd", "Start time sync.", "")
    g.check("agent", "low-risk start runs without asking and the service is active",
            "risk]" not in out and g.value("systemctl is-active systemd-timesyncd") == "active"
            and decisions(au) == ["allowed"] and au[-1].get("success") is True,
            decisions=decisions(au), output=out[-600:])

    out, rc, au = shell("remove-glibc", "Remove glibc.", "y\n")
    g.check("agent", "removing glibc is denied by policy, whatever the answer; glibc stays intact",
            "risk]" not in out and decisions(au) == ["denied"] and g.sh("cpkg verify glibc")[1] == 0,
            decisions=decisions(au), reason=au[-1].get("reason") if au else None)

    out, rc, au = shell("start-missing", "Start the gate service.", "")
    codes = [s.get("exit") for s in (au[-1].get("steps", []) if au else [])]
    g.check("agent", "a failing action is reported as failed, with its exit code, in the audit log",
            decisions(au) == ["allowed"] and au[-1].get("success") is False and any(c not in (0, None) for c in codes),
            exit_codes=codes, entries=au, output=out[-600:])

    out, rc, au = shell("read-shadow", "Show /etc/shadow.", "")
    g.check("agent", "the agent refuses to read /etc/shadow for the model, says so, never asks the Guardian, "
            "and carries on",
            rc == 0 and "off limits" in out and not re.search(r"root:[^:\s]*\$", out)
            and [(e.get("action"), e.get("decision")) for e in au] == [("disk_usage", "allowed")],
            rc=rc, entries=au, output=out[-600:])

    out, rc, au = shell("invalid-package", "Install -rf.", "y\n")
    g.check("agent", "an invalid intent is rejected by the agent before it reaches the Guardian; the next one runs",
            rc == 0 and "risk]" not in out
            and [(e.get("action"), e.get("decision")) for e in au] == [("disk_usage", "allowed")],
            rc=rc, entries=au, output=out[-600:])

    out, rc, au = shell(None, "network", None, backend="rescue")
    g.check("agent", "rescue planner: 'network' runs network_status (read-only, no question)",
            decisions(au) == ["allowed"] and au[-1].get("action") == "network_status" and "routable" in out,
            decisions=decisions(au), output=out[-600:])
    out, rc, au = shell(None, "remove glibc", "y\n", backend="rescue")
    g.check("agent", "rescue planner: 'remove glibc' is denied", decisions(au) == ["denied"], decisions=decisions(au))

    # ---- the protocol, directly: tokens and sessions ------------------------------
    n = g.audit_len()
    rows, _ = g.client(USER, "ping")
    g.check("protocol", "an authorised user gets hello and pong",
            response_of(rows, "hello").get("type") == "hello" and response_of(rows, "ping").get("type") == "pong",
            rows=rows)
    rows, _ = g.client(USER, "cross-session", "set_timezone", '{"timezone": "Europe/Paris"}')
    g.check("protocol", "a token cannot be redeemed from another connection",
            response_of(rows, "approve on B").get("kind") == "expired", rows=rows)
    g.check("protocol", "the session that asked can redeem it once; the change happens",
            response_of(rows, "approve on A").get("success") is True and g.timezone() == "Europe/Paris",
            timezone=g.timezone())
    g.check("protocol", "a redeemed token cannot be replayed", response_of(rows, "replay on A").get("kind") == "expired")
    rows, _ = g.client(USER, "forged")
    g.check("protocol", "a forged token is refused", response_of(rows, "forged token").get("kind") == "expired",
            rows=rows)
    rows, _ = g.client(USER, "decline", "set_timezone", '{"timezone": "Asia/Tokyo"}')
    g.check("protocol", "declining runs nothing, and a declined token is spent",
            response_of(rows, "decline").get("kind") == "declined"
            and response_of(rows, "approve after decline").get("kind") == "expired"
            and g.timezone() == "Europe/Paris", rows=rows, timezone=g.timezone())
    rows, _ = g.client(USER, "expire", "set_timezone", '{"timezone": "Asia/Tokyo"}', "125", timeout=400)
    g.check("protocol", "a confirmation expires (120 s): approving later runs nothing",
            response_of(rows, "approve after 125s").get("kind") == "expired" and g.timezone() == "Europe/Paris",
            rows=rows, timezone=g.timezone())
    rows, _ = g.client(USER, "exec", "install_package", '{"package": "-rf"}')
    g.check("protocol", "the Guardian validates intents itself (invalid package name)",
            response_of(rows, "execute").get("kind") == "invalid", rows=rows)
    rows, _ = g.client(USER, "exec", "rm_rf", "{}")
    g.check("protocol", "an action outside the catalog is invalid", response_of(rows, "execute").get("kind") == "invalid",
            rows=rows)
    rows, _ = g.client(USER, "exec", "read_file", '{"path": "/etc/shadow"}')
    g.check("protocol", "agent-side actions are never run by the root Guardian",
            response_of(rows, "execute").get("kind") == "not_privileged", rows=rows)
    rows, _ = g.client(USER, "exec", "remove_package", '{"package": "glibc"}')
    g.check("protocol", "the denial does not depend on the agent", response_of(rows, "execute").get("kind") == "denied",
            rows=rows)
    rows, _ = g.client(USER, "oversized")
    g.check("protocol", "an oversized frame is refused and the connection closed",
            response_of(rows, "oversized frame").get("type") == "error"
            and response_of(rows, "after the refusal").get("type") == "closed", rows=rows)
    au = g.audit_since(n)
    g.check("protocol", "the protocol checks left the matching audit entries",
            {"confirmation_required", "confirmed", "declined", "confirmation_expired", "denied", "invalid"}
            <= set(decisions(au)),
            decisions=decisions(au))

    # ---- who may connect ----------------------------------------------------------
    rows, _ = g.client(INTRUDER, "ping")
    g.check("peers", "a user outside core cannot open the socket (file mode)",
            any(r.get("error") == "PermissionError" for r in rows), rows=rows)
    intruder_uid = int(g.value(f"id -u {INTRUDER}"))
    since = g.value("date '+%Y-%m-%d %H:%M:%S'")
    g.sh(f"setfacl -m u:{INTRUDER}:rw /run/core/guardian.sock")
    rows, _ = g.client(INTRUDER, "ping")
    g.sh(f"setfacl -x u:{INTRUDER} /run/core/guardian.sock")
    journal = g.value(f"journalctl -u core-guardian --since '{since}' --no-pager -o cat | grep -c 'refused connection from uid {intruder_uid}'")
    g.check("peers", "given access to the socket file, the Guardian itself still refuses the peer (SO_PEERCRED)",
            response_of(rows, "hello").get("message") == "not authorised" and journal not in ("", "0"),
            rows=rows, journal_matches=journal)
    g.check("peers", f"{USER} cannot read the audit log",
            g.as_user(USER, f"cat {AUDIT} >/dev/null")[1] != 0)
    g.check("peers", f"{USER} cannot change the policy file",
            g.as_user(USER, f"echo '# x' >> {GUARDIAN_CONFIG}")[1] != 0)

    # ---- restarts and socket activation ---------------------------------------------
    def pong():
        rows, _ = g.client(USER, "ping")
        return response_of(rows, "ping").get("type") == "pong"

    g.sh("systemctl restart core-guardian.service")
    g.check("recovery", "after a service restart the next request works", pong())
    pid = g.value("systemctl show -p MainPID --value core-guardian.service")
    restarts = g.value("systemctl show -p NRestarts --value core-guardian.service")
    if pid.isdigit() and int(pid) > 1:
        g.sh(f"kill -9 {pid}; sleep 5")
    # Read systemd's view before any request, which would socket-activate it anyway.
    after_pid = g.value("systemctl show -p MainPID --value core-guardian.service")
    after_restarts = g.value("systemctl show -p NRestarts --value core-guardian.service")
    g.check("recovery", "a killed Guardian is restarted by systemd (Restart=on-failure), then answers",
            pid.isdigit() and int(pid) > 1 and after_pid not in ("0", pid) and after_restarts.isdigit()
            and restarts.isdigit() and int(after_restarts) == int(restarts) + 1 and pong(),
            killed=pid, new_pid=after_pid, restarts=[restarts, after_restarts])
    g.sh("systemctl stop core-guardian.service")
    stopped = g.value("systemctl is-active core-guardian.service")
    g.check("recovery", "a stopped Guardian is started again by its socket on the next request",
            stopped != "active" and pong() and g.value("systemctl is-active core-guardian.service") == "active",
            after_stop=stopped)
    g.sh("systemctl restart core-guardian.socket")
    g.check("recovery", "after the socket unit is restarted requests work",
            pong() and g.value("stat -c '%a %U %G' /run/core/guardian.sock") == "660 root core")
    g.sh("rm -f /tmp/gate-token /tmp/gate-go")
    holder = (f"python3 {MNT}/gate-client.py hold set_timezone '{{\"timezone\": \"Asia/Tokyo\"}}' "
              "/tmp/gate-token /tmp/gate-go > /tmp/gate-hold.out 2>&1")
    g.sh(f"su -s /bin/sh {USER} -c {shlex.quote('cd /tmp && ' + holder)} &")
    g.sh("for i in $(seq 1 100); do test -e /tmp/gate-token && break; sleep 0.2; done")
    g.sh("systemctl restart core-guardian.service; touch /tmp/gate-go")
    g.sh("for i in $(seq 1 100); do grep -q 'new connection' /tmp/gate-hold.out 2>/dev/null && break; sleep 0.2; done")
    rows = [json.loads(line) for line in g.value("cat /tmp/gate-hold.out").splitlines() if line.startswith("{")]
    old = response_of(rows, "approve on the old connection").get("type")
    g.check("recovery", "a restart cuts the connection holding a pending confirmation, and nothing runs",
            old in ("closed", "connection error")
            and response_of(rows, "approve on a new connection").get("kind") == "expired"
            and g.timezone() == "Europe/Paris", rows=rows, timezone=g.timezone())
    g.sh(f"chmod 664 {GUARDIAN_CONFIG}; systemctl restart core-guardian.service; sleep 1")
    refused = not pong()
    log = g.value("journalctl -u core-guardian -n 20 --no-pager -o cat")
    g.sh(f"chmod 644 {GUARDIAN_CONFIG}; systemctl reset-failed core-guardian.service core-guardian.socket; "
         "systemctl restart core-guardian.socket")
    g.check("recovery", "the Guardian refuses to run with a group-writable policy file, and recovers once fixed",
            refused and "writable" in log and pong(), journal=log[-600:])

    # ---- rate limit -------------------------------------------------------------------
    rows, _ = g.client(USER, "flood", "disk_usage", "{}", "61", timeout=600)
    counts = rows[-1].get("counts", {}) if rows else {}
    g.check("limits", "more than 60 requests a minute from one user are refused",
            counts.get("rate_limited", 0) >= 1 and counts.get("executed", 0) <= 60, counts=counts)

    # ---- the audit log ----------------------------------------------------------------
    audit = g.audit_since(0)
    g.audit = audit
    bad = [e for e in audit if e.get("contract") != fp_host]
    g.check("audit", "every audit entry names the action contract", audit and not bad,
            entries=len(audit), without_contract=len(bad))
    peers = sorted({e["peer"]["uid"] for e in audit if "peer" in e})
    g.check("audit", "audit entries come from the user; a refused peer is cut off before any request is read",
            intruder_uid not in peers and uid in peers, peer_uids=peers)
    kinds = sorted(set(decisions(audit)))
    g.check("audit", "the audit log records every kind of decision the gate made",
            {"allowed", "confirmation_required", "confirmed", "declined", "denied", "confirmation_expired",
             "rate_limited", "invalid"} <= set(kinds), decisions=kinds)
    return uid


def availability(g, catalog):
    """What this image can carry out, per Guardian action. Statuses, strongest first:
    "works" (read-only, run here as the user), "executed in the gate" (a change the
    gate made and the audit log shows succeeding), "planned only" (the programs
    are present; the change was not run), "native, not run" (a Guardian operation
    that needs a device, checked for presence), "missing programs", "fails on this
    image", "unavailable" (the planner refuses it on this system)."""
    g.sh("sleep 61")
    executed = {e["action"] for e in g.audit if e.get("decision") in ("allowed", "confirmed") and e.get("success")}
    table = []
    for spec in catalog:
        if spec["executor"] != "guardian":
            continue
        name = spec["name"]
        intent = json.dumps({"action": name, "args": EXAMPLES.get(name, {})})
        out, rc = g.sh(f"core-guardian --config {GUARDIAN_CONFIG} --plan {shlex.quote(intent)}")
        row = {"action": name, "risk": spec["risk"]}
        if rc != 0:
            row.update(status="unavailable", why=out.strip().splitlines()[-1][:300] if out.strip() else "")
        else:
            plan = json.loads(out[out.index("{"):])
            programs = [s["run"]["program"] for s in plan.get("steps", []) if "run" in s]
            native = [s["native"] for s in plan.get("steps", []) if "native" in s]
            missing = [p for p in programs if g.sh(f"test -x {shlex.quote(p)}")[1] != 0]
            row.update(programs=programs, native=native, missing=missing)
            if missing:
                row["status"] = "missing programs"
            elif spec["risk"] != "observe" and name in executed:
                row["status"] = "executed in the gate"
            elif native:
                row["status"] = "native, not run"
                if name == "set_brightness":
                    row["device"] = g.sh("ls /sys/class/backlight | grep -q .")[1] == 0
            else:
                row["status"] = "planned only"
        if spec["risk"] == "observe" and row["status"] == "planned only":
            rows, _ = g.client(USER, "exec", name, json.dumps(EXAMPLES.get(name, {})))
            r = response_of(rows, "execute")
            row["ran"] = r.get("success")
            row["failure"] = r.get("failure")
            row["status"] = "works" if r.get("success") else "fails on this image"
        table.append(row)
    g.audit = g.audit_since(0)  # the whole log, including the availability runs
    return table


def self_test():
    for name, lines in SCRIPTS.items():
        for intent in lines:
            assert set(intent) == {"action", "args"}, name
    assert response_of([{"step": "a", "response": {"kind": "x"}}], "a") == {"kind": "x"}
    assert decisions([{"decision": "allowed"}, {}]) == ["allowed", None]
    print("integration-gate self-test ok")


def main():
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return 0
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--image", required=True)
    ap.add_argument("--repo", required=True, help="signed repository the image was built from")
    ap.add_argument("--out", required=True)
    ap.add_argument("--core-ctl", default=os.path.join(ROOT, "target", "release", "core-ctl"))
    ap.add_argument("--password", default="core")
    ap.add_argument("--root-password", default="Core-boot-test-1")
    ap.add_argument("--user-password", default="Core-gate-user-1")
    ap.add_argument("--memory", default="4G")
    a = ap.parse_args()

    os.makedirs(a.out, exist_ok=True)
    info = subprocess.run([a.core_ctl, "contract"], capture_output=True, text=True, check=True).stdout
    a.head = subprocess.run(["git", "-C", ROOT, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    a.contract = re.search(r"^fingerprint:\s*(sha256:[0-9a-f]{64})$", info, re.M).group(1)
    catalog = json.loads(subprocess.run([a.core_ctl, "catalog", "--json"], capture_output=True, text=True,
                                        check=True).stdout)
    image_sha = vmrun.sha256_file(a.image)
    bt = vmrun.boot_test()

    with tempfile.TemporaryDirectory(dir=a.out) as tmp:
        stage = os.path.join(tmp, "stage")
        os.makedirs(os.path.join(stage, "scripts"))
        shutil.copytree(a.repo, os.path.join(stage, "repo"))
        shutil.copy(os.path.join(HERE, "gate-client.py"), stage)
        for name, lines in SCRIPTS.items():
            with open(os.path.join(stage, "scripts", f"{name}.jsonl"), "w") as f:
                f.writelines(json.dumps(i) + "\n" for i in lines)
        disk = os.path.join(tmp, "gate.img")
        vmrun.make_repo_disk(stage, disk)
        vm = vmrun.VM(bt, a.image, os.path.join(a.out, "console.log"), disk, a.memory, 600)
        g = Gate(vm, a.out)
        g.audit = []
        table = []
        try:
            a.login_seen = interactive_login(vm, a.password, a.user_password, 900)
            vm.login(a.password, a.root_password, 900)
            run_gate(a, g)
            table = availability(g, catalog)
        except Exception as e:  # recorded, never hidden: the gate fails
            g.check("gate", "the gate ran to the end", False, error=f"{type(e).__name__}: {e}")
        finally:
            vm.close()

    passed = sum(c["passed"] for c in g.checks)
    result = {
        "image": os.path.abspath(a.image),
        "image_sha256": image_sha,
        "image_unchanged": vmrun.sha256_file(a.image) == image_sha,
        "contract": a.contract,
        "head": a.head,
        "accel": "kvm" if os.access("/dev/kvm", os.W_OK) else "tcg",
        "checks_passed": passed,
        "checks_total": len(g.checks),
        "checks": g.checks,
        "availability": table,
    }
    with open(os.path.join(a.out, "results.json"), "w") as f:
        json.dump(result, f, indent=2)
    with open(os.path.join(a.out, "audit.jsonl"), "w") as f:
        f.writelines(json.dumps(e, sort_keys=True) + "\n" for e in g.audit)
    with open(os.path.join(a.out, "transcript.json"), "w") as f:
        json.dump(g.transcript, f, indent=1)
    counts = {}
    for row in table:
        counts[row["status"]] = counts.get(row["status"], 0) + 1
    print(f"availability: {counts}")
    print(f"image unchanged: {result['image_unchanged']}")
    print(f"{passed}/{len(g.checks)} checks passed")
    return 0 if passed == len(g.checks) and g.checks and result["image_unchanged"] else 1


if __name__ == "__main__":
    sys.exit(main())
