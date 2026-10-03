#!/usr/bin/env python3
"""Assign evaluation tasks to train/dev/heldout splits, freeze them, and guard them.

    split.py freeze [--salt S] [--heldout 0.2] [--dev 0.1]   write eval/splits.json (once)
    split.py add                     assign tasks that are new since the freeze
    split.py check                   fail if a held-out task changed, vanished or is unassigned
    split.py leak-check DATA.jsonl   fail if training data contains dev or held-out tasks
    split.py --self-test

A task's split is decided by a salted hash of its id, not by hand, so nobody picks
easy tasks for the held-out set. The manifest records each task file's SHA-256. A
held-out task may never change after the freeze (a changed task is a new task with a
new id); train and dev tasks may be edited, and `add` records their new hashes.
The split is frozen before any data is generated (docs/TRAINING.md).
"""

import argparse
import hashlib
import json
import os
import sys
import tempfile

sys.path.insert(0, os.path.dirname(__file__))
import tasks as taskspec  # noqa: E402

MANIFEST = os.path.join(taskspec.ROOT, "eval", "splits.json")


def assign(task_id, salt, heldout, dev):
    """A split from a salted hash: deterministic, and blind to the task's content."""
    h = int(hashlib.sha256(f"{salt}:{task_id}".encode()).hexdigest()[:12], 16) / float(16**12)
    if h < heldout:
        return "heldout"
    if h < heldout + dev:
        return "dev"
    return "train"


def freeze(task_list, salt, heldout, dev):
    return {
        "version": 1,
        "salt": salt,
        "ratios": {"heldout": heldout, "dev": dev},
        "tasks": {
            t["id"]: {"split": assign(t["id"], salt, heldout, dev), "file": t["_file"], "sha256": t["_sha256"]}
            for t in task_list
        },
    }


def check(manifest, task_list):
    """(errors, notes): errors fail the check."""
    errors, notes = [], []
    current = {t["id"]: t for t in task_list}
    r = manifest["ratios"]
    for tid, entry in manifest["tasks"].items():
        # The split comes from the hash, never from an edit to the manifest.
        expected = assign(tid, manifest["salt"], r["heldout"], r["dev"])
        if entry["split"] != expected:
            errors.append(f"task {tid} is recorded as {entry['split']}, its hash assigns {expected}")
        t = current.get(tid)
        if entry["split"] == "heldout":
            if t is None:
                errors.append(f"held-out task {tid} was removed")
            elif t["_sha256"] != entry["sha256"]:
                errors.append(f"held-out task {tid} changed after the freeze (make a new task instead)")
        elif t is not None and t["_sha256"] != entry["sha256"]:
            notes.append(f"{entry['split']} task {tid} changed since it was recorded (run `split.py add`)")
    for tid in sorted(set(current) - set(manifest["tasks"])):
        errors.append(f"task {tid} has no split (run `split.py add`)")
    return errors, notes


def add(manifest, task_list):
    """Assign new tasks and record new hashes of train/dev tasks. Held-out entries never change."""
    r = manifest["ratios"]
    for t in task_list:
        entry = manifest["tasks"].get(t["id"])
        if entry is None:
            manifest["tasks"][t["id"]] = {
                "split": assign(t["id"], manifest["salt"], r["heldout"], r["dev"]),
                "file": t["_file"],
                "sha256": t["_sha256"],
            }
        elif entry["split"] != "heldout":
            entry["sha256"] = t["_sha256"]
    return manifest


def leak_check(manifest, records):
    """Problems in a training dataset: tasks that are not train tasks (dev tasks are
    for choosing between models, held-out tasks for accepting one), or split labels
    that disagree with the manifest."""
    out = []
    for n, r in enumerate(records, 1):
        entry = manifest["tasks"].get(r.get("task"))
        if entry is None:
            out.append(f"record {n}: task {r.get('task')!r} is not in the split manifest")
        elif entry["split"] != "train":
            out.append(f"record {n}: {entry['split']} task {r['task']} must not be in training data")
        elif r.get("split") != entry["split"]:
            out.append(f"record {n}: task {r['task']} is {entry['split']}, record says {r.get('split')}")
    return out


def self_test():
    ts = [{"id": f"task-{i}", "_file": f"eval/tasks/task-{i}.toml", "_sha256": f"{i:064x}"} for i in range(200)]
    m = freeze(ts, "salt", 0.2, 0.1)
    counts = {s: sum(e["split"] == s for e in m["tasks"].values()) for s in ("train", "dev", "heldout")}
    assert 20 <= counts["heldout"] <= 60 and 5 <= counts["dev"] <= 40, counts
    assert freeze(ts, "salt", 0.2, 0.1) == m, "deterministic"
    assert check(m, ts) == ([], [])
    held = next(t for t in ts if m["tasks"][t["id"]]["split"] == "heldout")
    train = next(t for t in ts if m["tasks"][t["id"]]["split"] == "train")
    changed = [dict(t, _sha256="f" * 64) if t is held else t for t in ts]
    assert any("changed after the freeze" in e for e in check(m, changed)[0])
    assert any("was removed" in e for e in check(m, [t for t in ts if t is not held])[0])
    edited = [dict(t, _sha256="e" * 64) if t is train else t for t in ts]
    errors, notes = check(m, edited)
    assert not errors and notes, "train edits are notes, not errors"
    new = ts + [{"id": "new", "_file": "eval/tasks/new.toml", "_sha256": "1" * 64}]
    assert any("has no split" in e for e in check(m, new)[0])
    m2 = add(json.loads(json.dumps(m)), new)
    assert check(m2, new) == ([], []) and m2["tasks"][held["id"]] == m["tasks"][held["id"]]
    dev = next(t for t in ts if m["tasks"][t["id"]]["split"] == "dev")
    leaks = leak_check(m, [{"task": held["id"], "split": "train"}, {"task": train["id"], "split": "train"},
                           {"task": dev["id"], "split": "dev"}])
    assert len(leaks) == 2 and all("must not be in training data" in x for x in leaks), leaks
    moved = json.loads(json.dumps(m))
    moved["tasks"][held["id"]]["split"] = "train"
    assert any("its hash assigns heldout" in e for e in check(moved, ts)[0]), "hand-moved split is caught"
    print("split self-test ok")


def write(manifest):
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(MANIFEST))
    with os.fdopen(fd, "w") as f:
        json.dump(manifest, f, indent=2, sort_keys=True)
        f.write("\n")
    os.replace(tmp, MANIFEST)


def main():
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return 0
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("freeze")
    f.add_argument("--salt", default="core-eval-v1")
    f.add_argument("--heldout", type=float, default=0.2)
    f.add_argument("--dev", type=float, default=0.1)
    sub.add_parser("add")
    sub.add_parser("check")
    lk = sub.add_parser("leak-check")
    lk.add_argument("data")
    a = ap.parse_args()

    task_list = taskspec.load_all()
    if a.cmd == "freeze":
        if os.path.exists(MANIFEST):
            print(f"{MANIFEST} exists: the split is frozen (use `add` for new tasks)", file=sys.stderr)
            return 1
        write(freeze(task_list, a.salt, a.heldout, a.dev))
    else:
        with open(MANIFEST) as fh:
            manifest = json.load(fh)
        if a.cmd == "add":
            write(add(manifest, task_list))
        elif a.cmd == "check":
            errors, notes = check(manifest, task_list)
            for n in notes:
                print(f"note: {n}")
            for e in errors:
                print(f"error: {e}")
            if errors:
                return 1
        elif a.cmd == "leak-check":
            with open(a.data) as fh:
                records = [json.loads(line) for line in fh if line.strip()]
            leaks = leak_check(manifest, records)
            for p in leaks:
                print(p)
            print(f"{len(records)} records, {len(leaks)} problems")
            return 1 if leaks else 0
    with open(MANIFEST) as fh:
        m = json.load(fh)
    counts = {s: sorted(t for t, e in m["tasks"].items() if e["split"] == s) for s in ("train", "dev", "heldout")}
    for s, ids in counts.items():
        print(f"{s:8} {len(ids):3}  {' '.join(ids)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
