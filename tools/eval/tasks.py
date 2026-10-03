"""Evaluation task specifications (eval/tasks/*.toml): loading and validation.

A task is a fixture that breaks (or leaves alone) a fresh C.O.R.E. OS machine, the
user's request, a reference solution written as catalog intents, and how to judge
the result:

  kind = "repair"   checks on the resulting state must fail after the fixture and
                    pass after the solution
  kind = "observe"  read-only; the solution's output must contain [expect].output
  kind = "refuse"   the Guardian's policy must deny the solution; nothing runs, and
                    the checks must pass before and after (the system is unchanged)

`baseline` names the reference a trained model is compared with on this task:
"rescue" where the deterministic rescue planner covers it (the model must match
it, not beat it), "model" where only a model can (docs/TRAINING.md).
"""

import hashlib
import os
import tomllib

ROOT = os.path.normpath(os.path.join(os.path.dirname(__file__), "..", ".."))
TASK_DIR = os.path.join(ROOT, "eval", "tasks")

FAMILIES = {"packages", "services", "network", "system", "hardware", "safety", "conversation"}
KINDS = {"repair", "observe", "refuse"}
BASELINES = {"rescue", "model"}
TOP_KEYS = {"id", "family", "kind", "request", "baseline", "fixture", "solution", "check", "expect"}


def problems(task, filename):
    """Everything wrong with a parsed task (empty list if it is valid)."""
    out = []
    unknown = {k for k in task if not k.startswith("_")} - TOP_KEYS
    if unknown:
        out.append(f"unknown keys {sorted(unknown)}")
    tid = task.get("id", "")
    if os.path.basename(filename) != f"{tid}.toml":
        out.append(f"id {tid!r} does not match the file name")
    if task.get("family") not in FAMILIES:
        out.append(f"family must be one of {sorted(FAMILIES)}")
    kind = task.get("kind")
    if kind not in KINDS:
        out.append(f"kind must be one of {sorted(KINDS)}")
    if task.get("baseline") not in BASELINES:
        out.append(f"baseline must be one of {sorted(BASELINES)}")
    if not str(task.get("request", "")).strip():
        out.append("request is empty")
    fixture = task.get("fixture", {})
    if set(fixture) - {"repo", "commands"}:
        out.append(f"unknown fixture keys {sorted(set(fixture) - {'repo', 'commands'})}")
    if not all(isinstance(c, str) and c.strip() for c in fixture.get("commands", [])):
        out.append("fixture commands must be non-empty strings")
    solution = task.get("solution", [])
    if not solution or not all(isinstance(s, dict) and s.get("action") for s in solution):
        out.append("a reference solution (one or more intents) is required")
    for s in solution:
        if set(s) - {"action", "args"}:
            out.append(f"unknown solution keys {sorted(set(s) - {'action', 'args'})}")
    checks = task.get("check", [])
    if not all(c.get("name") and c.get("command") for c in checks):
        out.append("every check needs a name and a command")
    expect = task.get("expect", {}).get("output", [])
    if kind in ("repair", "refuse") and not checks:
        out.append(f"a {kind} task needs state checks")
    if kind == "repair" and not fixture.get("commands") and not checks:
        out.append("a repair task needs a fixture or checks that fail on a fresh machine")
    if kind == "observe" and not expect:
        out.append("an observe task needs [expect] output")
    return out


def load(path):
    with open(path, "rb") as f:
        raw = f.read()
    task = tomllib.loads(raw.decode())
    task["_file"] = os.path.relpath(path, ROOT)
    task["_sha256"] = hashlib.sha256(raw).hexdigest()
    return task


def load_all(task_dir=TASK_DIR):
    """All tasks, sorted by id. Raises ValueError listing every invalid task."""
    tasks, errors = [], []
    for name in sorted(os.listdir(task_dir)):
        if not name.endswith(".toml"):
            continue
        path = os.path.join(task_dir, name)
        try:
            task = load(path)
        except (OSError, tomllib.TOMLDecodeError) as e:
            errors.append(f"{name}: {e}")
            continue
        errors += [f"{name}: {p}" for p in problems(task, path)]
        tasks.append(task)
    ids = [t.get("id") for t in tasks]
    for dup in {i for i in ids if ids.count(i) > 1}:
        errors.append(f"duplicate task id {dup}")
    if errors:
        raise ValueError("\n".join(errors))
    return sorted(tasks, key=lambda t: t["id"])


def self_test():
    good = {
        "id": "t", "family": "packages", "kind": "repair", "request": "fix it", "baseline": "rescue",
        "fixture": {"commands": ["cpkg remove -- nano"]},
        "solution": [{"action": "install_package", "args": {"package": "nano"}}],
        "check": [{"name": "ok", "command": "true"}],
    }
    assert problems(good, "t.toml") == [], problems(good, "t.toml")
    assert problems(good, "other.toml"), "id must match the file"
    for key, bad in [("kind", "fix"), ("family", "misc"), ("baseline", "none"), ("request", " ")]:
        assert problems({**good, key: bad}, "t.toml"), key
    assert problems({**good, "check": []}, "t.toml"), "repair needs checks"
    assert problems({**good, "kind": "observe"}, "t.toml"), "observe needs expected output"
    assert problems({**good, "solution": []}, "t.toml"), "solution required"
    assert problems({**good, "extra": 1}, "t.toml"), "unknown keys rejected"
    print("tasks self-test ok")


if __name__ == "__main__":
    import sys

    if sys.argv[1:] == ["--self-test"]:
        self_test()
    else:
        for t in load_all():
            print(f"{t['id']:28} {t['family']:10} {t['kind']:8} baseline={t['baseline']}")
