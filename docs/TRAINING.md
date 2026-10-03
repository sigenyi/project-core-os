# Training the C.O.R.E. model

C.O.R.E. ships a local model distilled for its action catalog (roadmap, phase 3).
Training and distilling a 4B-class model is required product scope. This document
is the plan: the phases, the two gates that must not be confused, what is already
built, and the decisions that are still open. Nothing here selects a teacher, a
student, a data policy or a budget; those are listed as open decisions.

## Two gates

**Integration gate.** The AI stack runs on C.O.R.E. OS and acts on it correctly:
the Guardian's plans use `cpkg` and systemd-networkd/resolved, the services are
packaged, and requests go end to end through the Guardian with authorization,
confirmations, peer checks and the audit log intact. It may be passed with the
deterministic rescue planner or an off-the-shelf model. **Passing it says nothing
about the trained model.**

**Product acceptance gate (trained model).** The distilled 4B-class model, quantized
as shipped and running on the 8 GB target, meets the acceptance thresholds on the
frozen held-out set, run in disposable VMs and judged by the resulting system state.
Only this gate accepts a model for the product. An off-the-shelf model that passes
the integration gate is not a substitute.

## Phases

1. **Foundation** (this branch). Guardian backends for C.O.R.E. OS, a versioned
   action contract, a sanitized trajectory format, held-out task specifications with
   frozen splits, and a disposable-VM runner that checks resulting state.
2. **Integration.** Package `core-guardian`, `core-agent`, `core-sensed`,
   `core-shell`, `core-ctl` and llama.cpp; the Guardian answers on the image; the
   integration gate runs in the VM (rescue planner, or an off-the-shelf model if one
   is approved).
3. **Training environment.** A fixture library per task family, reset by booting a
   disposable copy of the image; the runner driving the real Guardian; baselines.
4. **Pilot.** A small validated dataset and evaluation, with stop/go criteria (below),
   before any costly generation or training.
5. **Data generation and distillation**, after the pilot passes and the open
   decisions are made.
6. **Product acceptance** of the trained model, then packaging it (pinned SHA-256).

Graphics, Wi-Fi hardware, eBPF collectors, public package hosting and a self-hosted
Rust toolchain are deferred: none of them is on the model's critical path.

## What must come first

- Before data generation: the integration gate; the action contract pinned for the
  dataset; the fixture library and disposable-VM reset; the replay validator; the
  held-out split frozen; the teacher, student and data-rights decisions made.
- Before training: a validated dataset whose records all carry the pinned contract,
  and baseline results on the held-out set.
- In parallel with integration: the open decisions, the task taxonomy, fixture
  design, the evaluation specification and the budget.

## The action contract

The model is trained against a contract: the action catalog (names, parameters, risk,
executor), the grammar generated from it, and the observation format the agent
feeds back. `core_protocol::contract` identifies it with a version and a SHA-256
fingerprint (`core-ctl contract`) of the catalog, including the summaries, parameter
docs and examples shown in the prompt, and of the grammar as the agent generates it.
What the hash cannot see is versioned by hand: the agent's prompt and observation
text (`OBSERVATION_FORMAT_VERSION`) and validation rules that change no parameter
kind (`CONTRACT_VERSION`). Every Guardian audit entry and every trajectory record
carries the fingerprint.

The contract is **pinned per dataset, not frozen forever**: a dataset manifest names
the fingerprint its records were made with, and a validator rejects records made
with another one. Changing the catalog is allowed; it starts a new dataset version,
and old data is either regenerated or explicitly migrated.

## Trajectories

A trajectory is one episode: the fixture, the user's request, every step (the
model's intent, the Guardian's decision, the observation) and the resulting-state
checks. The format is defined in `core_protocol::trajectory` and checked by
`core-ctl trajectory check FILE --contract FINGERPRINT`:

- every intent must validate against the catalog;
- secret parameters (Wi-Fi passphrases) must be redacted, and their values are
  scrubbed wherever they were echoed. This only works on the raw episode: records
  must be sanitized before anything else redacts them;
- the request, thoughts, observations and free-text arguments are sanitized: MAC
  addresses, non-loopback IP addresses (also in URLs, with ports, IPv4-mapped) and
  user names in home paths become placeholders; typed arguments get reserved
  documentation values (192.0.2.1, 2001:db8::1, /home/user) so they still
  validate. A four-part version number reads as an IPv4 address and is scrubbed
  too;
- every record names its contract fingerprint, fixture, split and outcome.

Dry runs and replays in a planner prove only that a plan was made. A repair is
validated only by running it in a disposable VM and checking the resulting state.

## Held-out evaluation

Tasks live in `eval/tasks/*.toml`. Each names a family, a fixture (commands that
break a fresh VM), the user's request, a reference solution as catalog intents, and
checks on the resulting state. The checks must fail on the broken fixture and pass
after the reference solution, in a real VM, or the task is invalid.

`tools/eval/split.py freeze` assigns each task to `train`, `dev` or `heldout` by a
salted hash and writes `eval/splits.json` with every task file's SHA-256.
`split.py check` refuses any change to a frozen held-out task and any split that
differs from the one its hash assigns, so a task cannot be moved by editing the
manifest. `split.py leak-check DATA.jsonl` refuses training data that contains dev
or held-out tasks. The split is frozen before any generation; held-out tasks are
never used to generate training data or prompts.

`tools/eval/vm-run.py` runs tasks in disposable VMs and judges them by resulting
state:

```sh
cargo build -p core-guardian -p core-ctl
tools/eval/vm-run.py --image image/core.img --repo REPO_DIR --out RUN_DIR [--jobs 2] [TASK ...]
```

Each task boots its own copy-on-write snapshot of the image (QEMU `snapshot=on`; the
image's SHA-256 is compared before and after the run), logs in on the serial
console, attaches the signed package repository as a read-only disk when the task
needs one, and runs the fixture. Then it runs the state checks, which for a repair
task must not all pass, and warns about any check that passed on the broken system.
Next it runs the reference solution, whose required steps must all exit with one
of their success codes, and the checks again, which must all pass. For
an observe task it checks for the expected output instead; for a refuse task the
Guardian must deny the intent and nothing runs. It writes a console log per task,
`results.json`, and one reference trajectory per task, sanitized and checked with
`core-ctl trajectory`.

Until the Guardian is packaged on the image (phase 2), the reference solution is
executed as the exact command lines the Guardian plans for it on the host
(`core-guardian --plan`, with the shipped `guardian.toml`), run as root in the VM.
That tests the planner's command lines on the real OS and records the policy
decision. It does **not** exercise peer authentication, confirmation, the
Guardian's output limits or the audit log, and the observation text is
reconstructed by the runner in the agent's format rather than produced by the
agent. Runs in this mode validate tasks and fixtures; they count toward neither
gate. Steps that run as the peer or are native Guardian operations cannot be
replayed this way and fail the task.

### Pilot task set

Nine tasks: package repair (nano, bc), service repair (systemd-resolved stopped with
its sockets, systemd-timesyncd disabled), set hostname, network status, package info
and search, and one refusal (removing glibc). They were split by `split.py freeze`
before any trajectory was made: held-out `pkg-info-bash`, `pkg-missing-bc`,
`pkg-search-editor`; dev `pkg-missing-nano`, `safety-remove-glibc`; the rest train.

On the clean 04d33bf image (TCG, no KVM), all nine pass in VMs, about 40 to 50
seconds each: every repair check failed after its fixture and passed after the
reference solution, the glibc removal was denied and glibc verified intact, and the
nine reference trajectories validate against the contract. The first run found a
weak check: `resolvectl query` succeeded with systemd-resolved stopped, because it
starts the service through D-Bus. That train task now stops the service's sockets
too and checks the stub resolver on 127.0.0.53 directly. The runner now warns about
checks like that.

### Baselines

Each family has its own baseline, recorded with the results: the deterministic
rescue planner where it covers the family, the untrained student base, and the
teacher. The trained model is not required to beat the rescue planner on narrow
tasks the planner already solves; it must match it there, and beat the untrained
base across families.

The task files name which applies (`baseline = "rescue"` or `"model"`). The rescue
planner was measured on the pilot requests with `core-shell --dev --backend rescue`.
It handles `net-status` and `safety-remove-glibc`, so those are `rescue` tasks. On
the other seven it does not reach the reference: it does not understand five
requests, answers "Websites won't resolve" with `network_status` instead of starting
systemd-resolved, and reads "Which version of bash is installed?" as a lookup of a
package called "installed". That last one is a rescue-planner bug, recorded here and
not fixed in this change.

## Pilot (phase 4)

A small run, on the order of tens of tasks per family, before costly generation:

- **Go** when: every pilot task passes its broken/fixed check in a VM; every
  generated trajectory validates and passes the sanitizer audit; the replayed
  trajectories reach the expected state in VMs at an agreed rate; the cost per
  validated trajectory is measured and within budget; no dev or held-out task leaks
  into training data (`split.py leak-check`).
- **Stop** and revise when: fixtures are flaky (a check passes before repair or
  fails after the reference solution), validated trajectories are too rare for the
  budget, the sanitizer misses identifying data, or the teacher's actions are unsafe
  at a rate the Guardian would have to refuse often.

## Product acceptance criteria (thresholds open)

Measured on the frozen held-out set, model quantized as shipped, in disposable VMs
on the 8 GB target, judged by resulting state:

- task success per family, at or above thresholds set per family;
- no high-risk action executed without confirmation (enforced by the Guardian;
  attempts are counted), and a bounded rate of change actions proposed for
  read-only requests;
- correct refusal or clarifying question on ambiguous or dangerous requests;
- recovery after an injected first failure within the step limit;
- robustness to paraphrased requests and unseen fixture combinations;
- resident memory within the model budget (about 4.5 GB, `BASE-OS.md`) and a
  time-to-first-action bound on CPU;
- reproducible: dataset manifest hash, training configuration, base checkpoint and
  output GGUF all pinned.

## Open decisions

None of these is decided. Each needs Joel's decision before the phase that needs it.

| # | Decision | Needed before | Notes |
|---|---|---|---|
| D1 | Teacher model(s) and access | data generation | No teacher is selected. If a hosted model is used, its provider's terms on using outputs to train other models must be checked first. |
| D2 | Student base checkpoint | training | "4B-class" is set (roadmap); `MODELS.md` lists runtime candidates, which is not a choice of base. |
| D3 | Licenses of student base, teacher outputs and dataset | data generation | Must allow fine-tuning and redistributing derived weights with a GPL-3.0 distribution. |
| D4 | Data provenance policy | data generation | Synthetic fixtures only, or also real machines (consent, retention, sanitizer coverage). |
| D5 | Distillation method | training | SFT, preference tuning, LoRA or full fine-tune; quantization-aware evaluation. |
| D6 | Compute and spending caps | pilot | Teacher calls and training runs; per-run ceilings. |
| D7 | Acceptance thresholds per family | product gate | Success rates, safety rates, latency and memory bounds. |
| D8 | Model distribution | packaging | In the image as a cpkg package, or a separate download. |
| D9 | Model for the integration gate | integration | Rescue planner only, or also an off-the-shelf model. |
| D10 | Other distributions' backends | integration | Keep pacman/apt/... in the Guardian for development, or remove them. |
| D11 | Wi-Fi daemon | later | iwd or wpa_supplicant; neither is in the base. Wi-Fi actions currently report that no Wi-Fi daemon is configured. |
| D12 | Package release numbers | integration | Bump the release on every content change (recommended) or not. |
