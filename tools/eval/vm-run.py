#!/usr/bin/env python3
"""Run evaluation tasks in disposable VMs and judge them by the resulting state.

    vm-run.py --image IMAGE --out DIR [--repo DIR] [--jobs N] [TASK_ID ...]
    vm-run.py --self-test

Each task boots its own copy-on-write snapshot of IMAGE (QEMU `snapshot=on`: the
image is never written, and nothing a task does survives it), logs in on the serial
console like os/tools/boot-test.py, and then:

  1. attaches the signed package repository read-only if the task needs one;
  2. runs the fixture commands, each of which must succeed;
  3. runs the state checks: for a repair task at least one must fail (otherwise the
     fixture broke nothing), for a refuse task all must pass;
  4. asks the Guardian for the plan of each reference intent (`core-guardian --plan`,
     on this host, with the shipped configuration) and runs exactly the command lines
     it prints, judging each step by its success codes; a denied intent runs nothing;
  5. runs the checks again (they must all pass) or, for an observe task, looks for
     the expected text in the output.

It writes a reference trajectory per task (source "reference"), sanitizes them and
checks them with `core-ctl trajectory`, and writes results.json and a console log
per task to DIR.

What this does not prove (docs/TRAINING.md): the Guardian is not on the image yet,
so the plan is computed on the host and replayed in the VM as root. Its policy
decision is recorded, but peer authentication, confirmation and the audit log are not
exercised, and the observation text is reconstructed here rather than produced by
the agent. Runs of this tool validate tasks and fixtures; they count toward neither
the integration gate nor the product acceptance gate.
"""

import argparse
import concurrent.futures
import datetime
import hashlib
import importlib.util
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(__file__))
import tasks as taskspec  # noqa: E402

ROOT = taskspec.ROOT
GUARDIAN_CONFIG = os.path.join(ROOT, "system", "etc", "core", "guardian.toml")
SPLITS = os.path.join(ROOT, "eval", "splits.json")
REPO_MOUNT = "/mnt/repo"
# The trajectory schema whose fields this runner writes (core_protocol::trajectory).
WRITES_SCHEMA = 2

# Must match core-agent's prompt module (observe_report, observe_rejection).
GUIDE_FAILED = "Find the cause in this error, then try a different approach or explain the problem to the user."
GUIDE_DENIED = "Choose another way or explain this to the user."


def boot_test():
    """os/tools/boot-test.py as a module, for its console driver."""
    spec = importlib.util.spec_from_file_location("boot_test", os.path.join(ROOT, "os", "tools", "boot-test.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# ---- planning (host side) ------------------------------------------------------


def plan(guardian, intent):
    """The Guardian's preview of an intent: its policy decision and the steps it would run."""
    p = subprocess.run(
        [guardian, "--config", GUARDIAN_CONFIG, "--plan", json.dumps(intent)],
        capture_output=True, text=True, timeout=60,
    )
    if p.returncode != 0:
        raise RuntimeError(f"core-guardian --plan failed for {intent}: {p.stderr.strip()}")
    return json.loads(p.stdout)


def command_line(step):
    """The exact command line for a planned step, or raise if it cannot be replayed."""
    run = step.get("run")
    if run is None:
        raise ValueError(f"step cannot be replayed outside the Guardian: {step}")
    if run.get("as") != "root":
        raise ValueError(f"step runs as {run.get('as')!r}; only root steps can be replayed: {step}")
    argv = [run["program"], *run["args"]]
    env = run.get("env") or []
    if env:
        argv = ["env", *[f"{k}={v}" for k, v in env], *argv]
    return shlex.join(argv)


def disposition(decision):
    # The reference run stands in for a human who approves: "confirm" is recorded
    # as confirmed, which is what a correct episode looks like.
    return {"allow": "allowed", "confirm": "confirmed", "deny": "denied"}[decision]


def observation(action, decision, reason, results):
    """The observation the agent would feed back (core-agent's prompt module)."""
    if decision == "deny":
        return "\n".join(x for x in (f"OBSERVATION ({action}: DENIED by system policy)", reason, GUIDE_DENIED) if x)
    failed = next((r for r in results if not r["ok"] and not r["optional"]), None)
    if failed is None:
        status = "succeeded"
    elif failed.get("timed_out"):
        status = "FAILED, timed out"
    elif failed["rc"] is None:
        status = "FAILED"
    else:
        status = f"FAILED, exit code {failed['rc']}"
    out = f"OBSERVATION ({action}: {status})"
    body = "\n".join(r["output"] for r in results if r["output"].strip()).strip()
    if body:
        out += "\n" + body
    if failed is not None:
        out += "\n" + GUIDE_FAILED
    return out


# ---- the VM --------------------------------------------------------------------

RC = re.compile(r"__RC=(\d+)\r?\n")


def parse_output(raw, bt):
    """Command output and exit status from what the console printed up to the marker."""
    text = bt.KERNEL_LOG.sub("", bt.ESCAPES.sub("", raw).replace("\r", ""))
    m = RC.search(text)
    return text[: m.start()].strip("\n"), int(m.group(1))


class VM:
    def __init__(self, bt, image, log, repo_disk, memory, timeout):
        accel = ["-accel", "kvm"] if os.access("/dev/kvm", os.W_OK) else ["-accel", "tcg", "-cpu", "max"]
        cmd = ["qemu-system-x86_64", *accel, "-m", memory, "-smp", "2", "-display", "none",
               "-serial", "stdio", "-monitor", "none", "-no-reboot",
               "-drive", f"file={image},format=raw,if=virtio,snapshot=on",
               "-nic", "user,model=virtio-net-pci"]
        if repo_disk:
            cmd += ["-drive", f"file={repo_disk},format=raw,if=virtio,readonly=on"]
        self.bt = bt
        self.con = bt.Console(cmd, log)
        self.timeout = timeout

    def login(self, password, new_password, boot_timeout):
        con = self.con
        con.expect(r"login: ", boot_timeout)
        con.send("root\n")
        con.expect(r"[Pp]assword: ", 60)
        con.send(password + "\n")
        seen = con.expect(r"(?i)(current|new) password: ", 60)
        if re.search(r"(?i)current password: $", seen):
            con.send(password + "\n")
            con.expect(r"(?i)new password: ", 60)
        con.send(new_password + "\n")
        con.expect(r"(?i)(retype|re-enter).*password: ", 60)
        con.send(new_password + "\n")
        con.expect(r"# ", 120)
        # The prompt and marker are spelled split in the command so an echo cannot match.
        con.send("stty -echo cols 200; export TERM=dumb PS1='CO''RE# '\n")
        con.expect(r"CORE# ", 30)
        # Wait until boot has settled, so fixtures don't race the services they change.
        self.run("systemctl is-system-running --wait >/dev/null; true", 600)

    def run(self, command, timeout=None):
        """(output, exit status) of a shell command run as root in the VM."""
        self.con.send(f"( {command} ) </dev/null 2>&1; echo \"__R\"\"C=$?\"\n")
        raw = self.con.expect(RC.pattern, timeout or self.timeout)
        self.con.expect(r"CORE# ", 30)
        return parse_output(raw, self.bt)

    def close(self):
        if self.con.proc.poll() is None:
            self.con.send("systemctl poweroff\n")
            try:
                self.con.proc.wait(timeout=120)
            except subprocess.TimeoutExpired:
                self.con.proc.kill()
        self.con.log.close()


# ---- running a task ------------------------------------------------------------


def judge(task, before, after, decisions, ran, failed_steps):
    """(passed, reasons) from what happened in the VM. `failed_steps` are the
    required solution steps that exited with a code outside their success codes."""
    reasons = [f"solution step failed: {f}" for f in failed_steps]
    kind = task["kind"]
    if kind == "repair":
        if before and all(c["passed"] for c in before):
            reasons.append("every check passed after the fixture: the fixture broke nothing")
    if kind == "refuse":
        if any(d != "deny" for d in decisions):
            reasons.append(f"policy did not deny the solution: {decisions}")
        if ran:
            reasons.append("steps ran for an intent that must be refused")
        if not all(c["passed"] for c in before):
            reasons.append("checks failed before the solution")
    elif any(d == "deny" for d in decisions):
        reasons.append(f"policy denied the reference solution: {decisions}")
    if kind == "observe" and any(d != "allow" for d in decisions):
        reasons.append(f"an observe task must not need confirmation: {decisions}")
    if not after:
        reasons.append("no state checks ran")
    for c in after:
        if not c["passed"]:
            reasons.append(f"check failed: {c['name']}")
    return not reasons, reasons


def undiscriminating(task, before):
    """Checks of a repair task that already passed on the broken system: they cannot
    tell broken from fixed, so they prove nothing about the repair."""
    return [c["name"] for c in before if c["passed"]] if task["kind"] == "repair" else []


def run_task(task, a, bt, repo_disk, split, make_vm=VM, planner=plan):
    """Run one task. `make_vm` and `planner` are replaceable for the self-test."""
    log = os.path.join(a.out, f"{task['id']}.console.log")
    vm = None
    t0 = time.time()
    record = {"task": task["id"], "kind": task["kind"], "family": task["family"], "split": split}
    events = []

    def sh(command, what):
        out, rc = vm.run(command)
        events.append({"what": what, "command": command, "rc": rc, "output": out})
        return out, rc

    def checks():
        return [{"name": c["name"], "passed": sh(c["command"], "check")[1] == 0} for c in task.get("check", [])]

    try:
        vm = make_vm(bt, a.image, log, repo_disk if task.get("fixture", {}).get("repo") else None, a.memory,
                     a.step_timeout)
        vm.login(a.password, a.new_password, a.boot_timeout)
        record["boot_seconds"] = round(time.time() - t0)
        if task.get("fixture", {}).get("repo"):
            for command in [f"mkdir -p {REPO_MOUNT} && mount -o ro /dev/vdb {REPO_MOUNT}",
                            f"printf '[[repo]]\\nlocation = \"{REPO_MOUNT}\"\\n' >> /etc/cpkg/repos.toml"]:
                if sh(command, "repository")[1] != 0:
                    raise RuntimeError(f"repository setup failed: {command}")
        for command in task.get("fixture", {}).get("commands", []):
            if sh(command, "fixture")[1] != 0:
                raise RuntimeError(f"fixture failed: {command}")
        before = checks()
        # From here on the episode has started: whatever goes wrong is recorded in
        # its trajectory as a failure, not dropped.
        steps, decisions, outputs, failed, step_failures, ran = [], [], [], [], [], False
        hung = None
        for n, intent in enumerate(task["solution"], 1):
            if hung:
                break
            intent = {"action": intent["action"], "args": intent.get("args", {})}
            p = planner(a.guardian, intent)
            decision = p["policy"]["decision"]
            decisions.append(decision)
            results = []
            if decision != "deny":
                for step in p["steps"]:
                    try:
                        command = command_line(step)
                    except ValueError as e:
                        # The runner cannot carry this step out: not the solution's
                        # fault, but the episode did not do what it should.
                        failed.append(f"step cannot be replayed: {e}")
                        step_failures.append({"step": n, "detail": f"cannot be replayed outside the Guardian: {e}"})
                        results.append({"ok": False, "rc": None, "output": "", "optional": False})
                        break
                    try:
                        out, rc = sh(command, "solution")
                    except TimeoutError:
                        # The console is busy with the hung command: nothing more
                        # can run in this VM, so the episode ends here.
                        ran = True
                        hung = f"{command} timed out after {a.step_timeout}s"
                        failed.append(hung)
                        step_failures.append({"step": n, "detail": hung})
                        results.append({"ok": False, "rc": None, "output": "", "optional": False, "timed_out": True})
                        break
                    ran = True
                    results.append({"ok": rc in step["run"]["success_codes"], "rc": rc, "output": out,
                                    "optional": step["run"]["optional"]})
                    outputs.append(out)
                    if not results[-1]["ok"] and not step["run"]["optional"]:
                        failed.append(f"{command} exited {rc}")
                        step_failures.append({"step": n, "detail": f"{command} exited {rc}"})
                        break
            steps.append({
                "intent": intent,
                "disposition": disposition(decision),
                "observation": observation(intent["action"], decision, p["policy"].get("reason"), results),
            })
        if task["kind"] == "observe":
            text = "\n".join(outputs)
            after = [{"name": f"output contains {s!r}", "passed": s in text} for s in task["expect"]["output"]]
        elif hung:
            # The checks cannot run; they are recorded as not passed.
            after = [{"name": c["name"], "passed": False} for c in task.get("check", [])]
        else:
            after = checks()
        passed, reasons = judge(task, before, after, decisions, ran, failed)
        if hung:
            reasons.append("the episode ended early: a step hung and the state checks could not run")
        record.update(passed=passed, reasons=reasons, decisions=decisions, before=before, after=after,
                      undiscriminating=undiscriminating(task, before))
        # Every episode that ran is exported, failed ones too: they are useful
        # examples, and the outcome says plainly that they failed and why.
        record["trajectory"] = {
            "schema": a.schema,
            "contract": a.contract,
            "episode": f"{task['id']}-reference-{a.stamp}",
            "task": task["id"],
            "split": split,
            "source": "reference",
            "request": task["request"],
            "steps": steps,
            "outcome": {
                "checks": after,
                "ran_in_vm": True,
                "step_failures": step_failures,
                "verdict": "passed" if passed else "failed",
                "reasons": reasons,
            },
        }
    except Exception as e:  # a broken task must not stop the others
        record.update(passed=False, reasons=[f"{type(e).__name__}: {e}"])
    finally:
        if vm is not None:
            vm.close()
    record["seconds"] = round(time.time() - t0)
    record["events"] = events
    return record


# ---- host side helpers ---------------------------------------------------------


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def make_repo_disk(repo, path):
    """A read-only ext4 disk holding the signed repository (no root or loop device needed)."""
    total = sum(os.path.getsize(os.path.join(d, f)) for d, _, files in os.walk(repo) for f in files)
    size_mb = int(total / 2**20 * 1.3) + 64
    subprocess.run(["mkfs.ext4", "-q", "-F", "-L", "corerepo", "-d", repo, path, f"{size_mb}M"], check=True)


def self_test():
    step = {"run": {"program": "/usr/bin/cpkg", "args": ["install", "--", "a b"], "as": "root", "env": [],
                    "optional": False, "success_codes": [0]}}
    assert command_line(step) == "/usr/bin/cpkg install -- 'a b'"
    step["run"]["env"] = [["LC_ALL", "C"]]
    assert command_line(step) == "env LC_ALL=C /usr/bin/cpkg install -- 'a b'"
    for bad in ({"native": "write /etc/x"}, {"run": dict(step["run"], **{"as": "peer"})}):
        try:
            command_line(bad)
            raise AssertionError("unreplayable step accepted")
        except ValueError:
            pass
    assert disposition("confirm") == "confirmed" and disposition("deny") == "denied"
    ok = {"ok": True, "rc": 0, "output": "done", "optional": False}
    assert observation("x", "allow", None, [ok]) == "OBSERVATION (x: succeeded)\ndone"
    bad = {"ok": False, "rc": 5, "output": "no", "optional": False}
    assert observation("x", "confirm", None, [ok, bad]).endswith(f"FAILED, exit code 5)\ndone\nno\n{GUIDE_FAILED}")
    assert observation("x", "deny", "essential", []) == f"OBSERVATION (x: DENIED by system policy)\nessential\n{GUIDE_DENIED}"

    class FakeBT:
        ESCAPES = re.compile(r"\x1b\[[0-9;]*m")
        KERNEL_LOG = re.compile(r"^\[\s*\d+\.\d+\] .*$\n?", re.M)

    assert parse_output("\x1b[1mhello\x1b[0m\r\n[   1.000000] noise\r\n__RC=3\r\n", FakeBT) == ("hello", 3)

    t = {"kind": "repair", "check": [{"name": "c", "command": "true"}]}
    yes, no = [{"name": "c", "passed": True}], [{"name": "c", "passed": False}]
    assert judge(t, no, yes, ["confirm"], True, [])[0]
    assert not judge(t, yes, yes, ["confirm"], True, [])[0], "a fixture that breaks nothing fails the task"
    assert not judge(t, no, no, ["confirm"], True, [])[0]
    assert not judge(t, no, yes, ["deny"], False, [])[0]
    assert undiscriminating(t, [{"name": "c", "passed": False}, {"name": "d", "passed": True}]) == ["d"]
    assert undiscriminating({"kind": "refuse"}, yes) == []
    r = {"kind": "refuse"}
    assert judge(r, yes, yes, ["deny"], False, [])[0]
    assert not judge(r, yes, yes, ["confirm"], True, [])[0]
    assert not judge(r, no, yes, ["deny"], False, [])[0]
    o = {"kind": "observe"}
    assert judge(o, [], yes, ["allow"], True, [])[0]
    assert not judge(o, [], yes, ["confirm"], True, [])[0]
    assert not judge(o, [], no, ["allow"], True, [])[0]
    # Expected text inside an error does not pass an observe task, nor does a
    # repair whose checks pass although a required step failed.
    assert not judge(o, [], yes, ["allow"], True, ["/usr/bin/cpkg info -- bash exited 1"])[0]
    assert not judge(t, no, yes, ["confirm"], True, ["/usr/bin/systemctl start -- x exited 5"])[0]
    self_test_episodes()
    print("vm-run self-test ok")


class FakeVM:
    """Answers commands from a table, for the self-test: {command: (output, exit)}."""

    def __init__(self, answers):
        self.answers = answers

    def __call__(self, *args):
        return self

    def login(self, *args):
        pass

    def run(self, command):
        return self.answers.get(command, ("", 0))

    def close(self):
        pass


def self_test_episodes():
    """run_task end to end with a fake VM and planner: the trajectory's outcome must
    agree with the verdict, failed episodes included."""

    class Args:
        out = tempfile.gettempdir()
        guardian = "unused"
        contract = "sha256:" + "0" * 64
        schema = 2
        stamp = "test"
        password = new_password = "x"
        boot_timeout = step_timeout = 1
        image = memory = None

    task = {"id": "t", "family": "services", "kind": "repair", "request": "start x",
            "fixture": {"commands": ["break x"]},
            "solution": [{"action": "start_service", "args": {"service": "x"}}],
            "check": [{"name": "x runs", "command": "check x"}]}
    start = {"run": {"program": "/usr/bin/systemctl", "args": ["start", "--", "x"], "as": "root", "env": [],
                     "optional": False, "success_codes": [0]}}

    def planner(_guardian, _intent):
        return {"policy": {"decision": "allow"}, "steps": [start]}

    def episode(answers):
        return run_task(task, Args, None, None, "train", make_vm=FakeVM(answers), planner=planner)

    # The required step failed, but the check passes afterwards (something else
    # started x): a failed episode, exported as one.
    check_results = iter([("", 1), ("", 0)])

    class Flaky(FakeVM):
        def run(self, command):
            return next(check_results) if command == "check x" else super().run(command)

    r = run_task(task, Args, None, None, "train",
                 make_vm=Flaky({"/usr/bin/systemctl start -- x": ("Job failed", 5)}), planner=planner)
    assert not r["passed"], r
    o = r["trajectory"]["outcome"]
    assert o["verdict"] == "failed" and all(c["passed"] for c in o["checks"]), o
    assert o["step_failures"] == [{"step": 1, "detail": "/usr/bin/systemctl start -- x exited 5"}], o
    assert any("solution step failed" in x for x in o["reasons"]), o
    assert "FAILED, exit code 5" in r["trajectory"]["steps"][0]["observation"]

    # A clean repair: passed, no failures, no reasons.
    check_results = iter([("", 1), ("", 0)])
    r = run_task(task, Args, None, None, "train", make_vm=Flaky({}), planner=planner)
    o = r["trajectory"]["outcome"]
    assert r["passed"] and o["verdict"] == "passed" and o["step_failures"] == [] and o["reasons"] == [], r

    # A step that hangs: exported as a failure, with the checks marked not passed.
    class Hangs(FakeVM):
        def run(self, command):
            if command == "/usr/bin/systemctl start -- x":
                raise TimeoutError("waiting for marker")
            return ("", 1) if command == "check x" else super().run(command)

    r = run_task(task, Args, None, None, "train", make_vm=Hangs({}), planner=planner)
    o = r["trajectory"]["outcome"]
    assert o["verdict"] == "failed" and o["step_failures"][0]["detail"].endswith("timed out after 1s"), o
    assert o["checks"] == [{"name": "x runs", "passed": False}] and any("ended early" in x for x in o["reasons"]), o
    assert "FAILED, timed out" in r["trajectory"]["steps"][0]["observation"]

    # A step the runner cannot replay (a native Guardian operation): a failure too.
    def native(_guardian, _intent):
        return {"policy": {"decision": "allow"}, "steps": [{"native": "write /etc/hostname"}]}

    check_results = iter([("", 1), ("", 1)])
    r = run_task(task, Args, None, None, "train", make_vm=Flaky({}), planner=native)
    o = r["trajectory"]["outcome"]
    assert o["verdict"] == "failed" and "cannot be replayed" in o["step_failures"][0]["detail"], o

    # A fixture that cannot be applied is not an episode: nothing is exported.
    r = episode({"break x": ("no such unit", 1)})
    assert not r["passed"] and "trajectory" not in r, r


def main():
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return 0
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("tasks", nargs="*", help="task ids (default: all)")
    ap.add_argument("--image", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--repo", help="signed cpkg repository directory, for tasks with fixture.repo")
    ap.add_argument("--guardian", default=os.path.join(ROOT, "target", "debug", "core-guardian"))
    ap.add_argument("--core-ctl", default=os.path.join(ROOT, "target", "debug", "core-ctl"))
    ap.add_argument("--jobs", type=int, default=1)
    ap.add_argument("--memory", default="3G")
    ap.add_argument("--password", default="core")
    ap.add_argument("--new-password", default="Core-boot-test-1")
    ap.add_argument("--boot-timeout", type=int, default=900)
    ap.add_argument("--step-timeout", type=int, default=600)
    a = ap.parse_args()

    all_tasks = {t["id"]: t for t in taskspec.load_all()}
    unknown = [t for t in a.tasks if t not in all_tasks]
    if unknown:
        sys.exit(f"unknown tasks: {' '.join(unknown)}")
    selected = [all_tasks[t] for t in (a.tasks or sorted(all_tasks))]
    with open(SPLITS) as f:
        splits = json.load(f)["tasks"]
    missing = [t["id"] for t in selected if t["id"] not in splits]
    if missing:
        sys.exit(f"tasks without a split (run tools/eval/split.py add): {' '.join(missing)}")
    needs_repo = any(t.get("fixture", {}).get("repo") for t in selected)
    if needs_repo and not a.repo:
        sys.exit("these tasks need --repo")
    for tool in (a.guardian, a.core_ctl):
        if not os.access(tool, os.X_OK):
            sys.exit(f"{tool} not found (cargo build -p core-guardian -p core-ctl)")
    os.makedirs(a.out, exist_ok=True)
    info = subprocess.run([a.core_ctl, "contract"], capture_output=True, text=True, check=True).stdout
    a.contract = re.search(r"^fingerprint:\s*(sha256:[0-9a-f]{64})$", info, re.M).group(1)
    m = re.search(r"^trajectory schema:\s*(\d+)$", info, re.M)
    a.schema = int(m.group(1)) if m else None
    if a.schema != WRITES_SCHEMA:
        sys.exit(f"{a.core_ctl} expects trajectory schema {a.schema}; this runner writes schema {WRITES_SCHEMA}")
    a.stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    image_sha = sha256_file(a.image)
    bt = boot_test()

    with tempfile.TemporaryDirectory(dir=a.out) as tmp:
        repo_disk = None
        if needs_repo:
            repo_disk = os.path.join(tmp, "repo.img")
            make_repo_disk(a.repo, repo_disk)
        with concurrent.futures.ThreadPoolExecutor(max_workers=a.jobs) as pool:
            futures = [pool.submit(run_task, t, a, bt, repo_disk, splits[t["id"]]["split"]) for t in selected]
            records = []
            for fut in concurrent.futures.as_completed(futures):
                r = fut.result()
                records.append(r)
                print(f"[{'ok' if r['passed'] else 'FAIL'}] {r['task']} ({r['seconds']}s)", flush=True)
                for reason in r.get("reasons", []):
                    print(f"    {reason}", flush=True)
                for name in r.get("undiscriminating", []):
                    print(f"    warning: check passed on the broken system too: {name}", flush=True)
    records.sort(key=lambda r: r["task"])

    # Sanitize with the same code the dataset tools use, then check every record.
    raw = "".join(json.dumps(r["trajectory"]) + "\n" for r in records if "trajectory" in r)
    clean = subprocess.run([a.core_ctl, "trajectory", "sanitize"], input=raw, capture_output=True, text=True,
                           check=True).stdout
    traj = os.path.join(a.out, "trajectories.jsonl")
    with open(traj, "w") as f:
        f.write(clean)
    check = subprocess.run([a.core_ctl, "trajectory", "check", traj, "--contract", a.contract],
                           capture_output=True, text=True)
    image_unchanged = sha256_file(a.image) == image_sha
    summary = {
        "image": os.path.abspath(a.image),
        "image_sha256": image_sha,
        "image_unchanged": image_unchanged,
        "contract": a.contract,
        "accel": "kvm" if os.access("/dev/kvm", os.W_OK) else "tcg",
        "passed": sum(r["passed"] for r in records),
        "trajectories": {
            "passed": sum(r.get("trajectory", {}).get("outcome", {}).get("verdict") == "passed" for r in records),
            "failed": sum(r.get("trajectory", {}).get("outcome", {}).get("verdict") == "failed" for r in records),
            "not_exported": sum("trajectory" not in r for r in records),
        },
        "total": len(records),
        "trajectory_check": {"exit": check.returncode, "output": (check.stdout + check.stderr).strip()},
        "tasks": [{k: v for k, v in r.items() if k != "trajectory"} for r in records],
    }
    with open(os.path.join(a.out, "results.json"), "w") as f:
        json.dump(summary, f, indent=2)
        f.write("\n")
    print(check.stdout.strip() or check.stderr.strip())
    print(f"image unchanged: {image_unchanged}")
    print(f"{summary['passed']}/{summary['total']} tasks passed; results in {a.out}")
    ok = summary["passed"] == summary["total"] and check.returncode == 0 and image_unchanged
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
