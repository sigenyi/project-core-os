# C.O.R.E. OS architecture

C.O.R.E. (Conversational Orchestration & Routing Engine) is a Linux distribution with
no user interface beyond a conversation. The user types or speaks a request; a local
language model works out what to do; a privileged executor does it, within strict
limits. This document describes the system as built and why it is shaped this way.

## The one rule

**There is no UI.** No display server, no window manager, no widgets, no graphics
toolkit. The console prompt (`core-shell`) is the only interface, and it is plain
text on the Linux virtual console. The kernel still uses KMS/DRM, but only to give
the console a native-resolution framebuffer.

## Components

```
                        ┌──────────────────────── user (uid 1000) ───────────────────────┐
  keyboard / mic ──────▶│ core-shell   console prompt, confirmations, push-to-talk        │
                        │   │                                                             │
                        │   ▼                                                             │
                        │ core-agent   control loop · prompt builder · rescue planner     │
                        │   │      ▲                      │                     ▲         │
                        └───┼──────┼──────────────────────┼─────────────────────┼─────────┘
              intent (JSON) │      │ observation           │ HTTP (loopback)     │ snapshot (JSON)
                            ▼      │                      ▼                     │
  ┌──────────── root ───────────────────┐   ┌──── DynamicUser, no network ────┐  ┌── core-sense, CAP_SYSLOG ──┐
  │ core-guardian                       │   │ core-inference                   │  │ core-sensed                │
  │ validate · policy · confirm · plan  │   │ llama.cpp llama-server + GBNF    │  │ procfs/sysfs/kmsg probes   │
  │ execute (no shell) · audit          │   └──────────────────────────────────┘  │ insights → /run/core-sense │
  └─────────────────────────────────────┘                                         └────────────────────────────┘
                 │ argv + scrubbed env                      (optional: core-whisper, whisper.cpp speech-to-text)
                 ▼
     systemctl · pacman · nmcli · wpctl · modprobe · …
```

| Crate / unit | Runs as | Responsibility |
|---|---|---|
| `core-protocol` (library) | — | The shared contract: action catalog, typed validation, GBNF grammar generation, wire protocol |
| `core-sense` + `core-sensed` | `core-sense`, `CAP_SYSLOG` only | Perception: collectors, insight rules, compact summaries |
| `core-guardian` | root (socket-activated) | The only privileged component: validation, policy, confirmation, planning, execution, audit |
| `core-agent` (library) | the user | Orchestration: context assembly, inference, routing, self-correction |
| `core-shell` | the user (login shell) | The interface: prompt, rendering, confirmations, voice capture |
| `core-ctl` | admin | Inspection and diagnostics (`doctor`, `exec`, `audit`, `grammar`, ...) |
| `core-inference.service` | `DynamicUser`, loopback only | llama.cpp `llama-server` serving the local model |

## The action catalog: one source of truth

Everything the AI can do is an entry in `core_protocol::CATALOG`: name, typed
parameters, risk level, executor (agent or Guardian), and an example. From that one
table the system derives:

* the **GBNF grammar** handed to llama.cpp, so the model can only sample a
  well-formed intent naming an enabled action with correctly-typed arguments;
* the **action reference** in the system prompt;
* the **generic argument checks** performed before typed validation;
* the **Guardian capabilities** (which actions policy enables, and which need
  confirmation), which in turn narrow the grammar and the prompt.

Adding a capability means adding a catalog entry, a variant of `Action` with its
validation, and a planner arm (or an agent handler). Tests fail if any of these
disagree: every catalog example must validate, plan, and match the grammar in
llama.cpp's own engine.

## Intents

The model's entire output is one JSON object:

```json
{"thought": "The Wi-Fi card has no driver; check the kernel log.", "action": "read_kernel_log", "args": {"errors_only": true}}
```

`thought` is a short reasoning field. It improves small models' choices and is shown
with `/verbose`, but nothing acts on it. `Intent::parse` is lenient (it extracts the
first balanced object, so backends without grammar support still work), and
`ValidatedAction::from_intent` is strict: unknown actions or arguments, missing
arguments, wrong types, out-of-range numbers, option-injection (`--foo`), path
traversal (`..`) and shell metacharacters are all rejected with messages written so
the model can correct itself.

## The control loop (`core_agent::Agent::handle`)

1. **Context assembly.** The system prompt (rules, action reference, examples) is
   static, so llama.cpp's prompt cache makes it nearly free after the first request.
   It is followed by up to `history_turns` earlier exchanges, then the request,
   prefixed by a compact summary of live telemetry.
2. **Inference.** One grammar-constrained completion from `llama-server`
   (`/v1/chat/completions` with `grammar`). If the server is unreachable, the
   deterministic rescue planner answers instead, so the machine stays operable.
3. **Routing.** `respond`/`ask_user` end the turn. `get_telemetry`,
   `read_file`, `list_directory` and `launch_program` need no privilege and are
   handled by the agent, as the user. Everything else goes to the Guardian.
4. **Observation.** The result (stdout/stderr, exit status, denial, or the user's
   refusal) is added to the transcript as an `OBSERVATION` and the loop repeats.
5. **Self-correction, bounded.** Failures carry their error output back to the
   model with an instruction to find the cause and try another approach. The loop
   stops after `max_steps` model calls or `max_consecutive_failures` failures; an
   action that already failed with identical arguments is never re-run. When it
   stops, the grammar is narrowed to `respond` alone, so the user always gets an
   explanation rather than silence.

Context is budgeted against the model's window: the oldest history goes first, then
older observations are clipped, while the latest observation is always kept whole.

## Perception (`core-sense`)

Collectors read `/proc`, `/sys`, `/etc` and `/dev/kmsg` through a relocatable
`Sysroot`, so the whole pipeline is tested against synthetic machines (for example,
a laptop whose Intel Wi-Fi card has no driver because its firmware failed to load).
Collected sections: host, CPU, memory, filesystems and block devices, network
(interfaces, addresses, routes, DNS), PCI/USB devices with bound drivers and names
from `pci.ids`, sound cards, power, thermal zones, failed units and notable kernel
messages.

**Insight rules** turn readings into prioritised findings with hints, such as
"iwlwifi: firmware failed to load" or "a link is up but there is no default route".
Small models reason far better over these than over raw sysfs. The prompt receives
a dozen-line text summary; the model can drill into any section as JSON with
`get_telemetry`.

`core-sensed` publishes snapshots atomically every 5 s. The agent uses them while
fresh and otherwise collects live (unprivileged, without the kernel log).

## The Guardian (`core-guardian`)

The pipeline for every request, in `Guardian::handle`:

1. **Authentication** of the peer by the kernel (`SO_PEERCRED`, `SO_PEERGROUPS`):
   root, listed uids, or members of the `core` group.
2. **Rate limiting** per connection.
3. **Validation** of the raw intent, again. The Guardian never trusts the client.
4. **Policy:** disabled actions, forbidden units (power and rescue) and protected
   ones (checked under their resolved names, so aliases do not help), protected
   packages, argument-raised risk (exfiltration-shaped hostnames), and the risk
   ceiling for automatic approval.
5. **Confirmation:** risky actions are parked under a random single-use token bound
   to the connection. The shell asks the human directly; the model never sees or
   produces tokens.
6. **Planning** for the configured distribution (pacman/apt/dnf/zypper/apk/xbps,
   PipeWire/PulseAudio/ALSA, NetworkManager/iwd) into fixed steps.
7. **Execution:** programs come only from the configured `[tools]` table and get
   argv vectors (never a shell) with option parsing terminated by `--` where
   supported. Each runs with a scrubbed environment, `/dev/null` stdin, its own
   process group (killed whole on timeout), and capped output that keeps head and
   tail. Per-user services (PipeWire) are reached by dropping to the peer's uid.
   Brightness, signals (thread IDs resolved to their process first), swap-file and
   fstab edits are implemented natively. No native operation takes a path from the
   model. Only changes are serialised; reads never wait behind them.
8. **Audit:** every decision and outcome is appended to `/var/log/core/audit.jsonl`
   with secrets redacted.

Dry-run mode simulates every change but still runs read-only actions for real,
which is what `core-shell --dev` uses on development machines.

## Boot and session

```
firmware → kernel + initramfs (archiso, kms hook) → systemd
   ├─ core-guardian.socket   (root-owned socket, group core, 0660)
   ├─ core-sensed.service    (telemetry)
   ├─ core-inference.service (llama-server, loopback only)
   ├─ NetworkManager.service
   └─ getty@tty1 → autologin core → /usr/bin/core-shell
```

The kernel's console log level is lowered (`kernel.printk = 3 4 1 3`) so driver
messages do not overwrite the conversation. They still reach the journal and
`core-sensed`.

## SOLID, concretely

* **Single responsibility:** perception, reasoning, authorisation and execution are
  separate processes with separate privileges.
* **Open/closed:** new actions, collectors and insight rules are added by
  registration. The loop, policy engine and executor do not change.
* **Liskov:** every `InferenceBackend` (llama.cpp, rescue, test scripts),
  `GuardianClient` (socket, in-process), `TelemetryProvider` and `Frontend` is
  interchangeable, and the integration tests run the real loop over doubles.
* **Interface segregation:** small traits such as `Collector`, `InsightRule`,
  `SystemProbe`, `CommandRunner` and `Transcriber`.
* **Dependency inversion:** `Agent::new` takes trait objects; only the binaries
  choose concrete implementations.

## Where this diverges from the original blueprint, and why

| Blueprint | Built | Reason |
|---|---|---|
| Slint UI rendered via KMS/DRM | Plain console text, no graphics stack | The project's one rule is no UI. KMS is still used for the framebuffer console. |
| Linux From Scratch base | Arch Linux via archiso for v1 | "Install a web browser" needs a package manager and maintained repositories. An LFS-derived minimal base is on the roadmap. |
| eBPF telemetry | procfs/sysfs/kmsg collectors behind a `Collector` trait | Covers the hardware, driver and log state the agent needs today without BTF/`CAP_BPF` requirements. eBPF event collectors (OOM kills, exec failures, packet drops) plug into the same trait later. |
| Telemetry JSON appended to the prompt | Compact text summary plus on-demand JSON sections | Small context windows. The summary costs about 200 tokens; detail is fetched only when needed. |
| Retry "until the issue is resolved" | Bounded retries, no identical repeats, forced final explanation | Unbounded autonomous retries by a fallible model are a reliability and safety hazard. |
| Fully autonomous execution | Human confirmation above a configurable risk ceiling | The model is treated as untrusted. Prompt injection via logs, file contents or Wi-Fi names is a real threat. |
| (none) | Rescue planner | The machine must remain operable when the model cannot run. |
