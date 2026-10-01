# project-core-os
C.O.R.E. OS: An agentic operating system controlled purely through natural language.

C.O.R.E. OS (Conversational Orchestration & Routing Engine) is an experimental, Zero-UI Linux distribution. It replaces the traditional graphical desktop with a single conversational prompt, utilizing an embedded AI agent to autonomously troubleshoot drivers, install dependencies, launch programs, and manage the system based entirely on natural language intents.

```
C.O.R.E.  say what you need  (/help for commands)
› bluetooth isn't working
  → Check the status of bluetooth
  → Restart service bluetooth
The Bluetooth service had crashed. I restarted it and it is running again.
› install a terminal web browser
  → Search packages for "terminal web browser"
  → Install package w3m
  ⚠ Install package w3m  [high risk]
    Allow? [y/N] y
w3m is installed. Say "open w3m" to start it.
```
*(Illustrative session. Low-risk actions run immediately; anything persistent or
destructive asks you first.)*

## The idea

* **No UI.** No desktop, no launcher, no windows to manage. You log in to a text
  prompt (or press push-to-talk) and say what you want. When you ask for a
  graphical program, the AI brings up just that program, full screen, and you
  return to the conversation when you close it.
* **Its own distribution.** Built from source with its own builder (`core-build`)
  and package manager (`cpkg`), whose packages describe themselves to the AI:
  what they provide, how to launch them, and why they are installed.
* **Local AI.** A 4-8B parameter model runs on the machine through llama.cpp. No
  cloud: the OS has to be able to fix the network when there is no network.
* **Untrusted AI, trusted executor.** The model can only emit one JSON intent from
  a closed catalog, enforced at sampling time by a grammar. A separate root daemon,
  the **Guardian**, validates it, applies policy, asks *you* before anything risky,
  runs it without a shell, and audits it.
* **Self-correcting.** Command errors are fed back to the model, which diagnoses
  them and tries another approach, within strict bounds.

## Architecture in one picture

```
 you ──▶ core-shell ──▶ core-agent ──grammar-constrained──▶ llama.cpp (local model)
            ▲  ▲            │   ▲
 confirm ───┘  │     intent │   │ observation               core-sensed
               │            ▼   │                     (procfs/sysfs/kmsg → insights)
               │       core-guardian (root) ──argv──▶ systemctl, cpkg, networkctl, …
               └──────── validate · policy · confirm · plan · execute · audit
```

| Component | What it is |
|---|---|
| [`core-protocol`](crates/core-protocol) | Action catalog, typed validation, GBNF grammar generator, wire protocol |
| [`core-sense`](crates/core-sense) | Telemetry collectors, insight rules and the `core-sensed` daemon |
| [`core-guardian`](crates/core-guardian) | The only privileged component: the executor daemon |
| [`core-agent`](crates/core-agent) | The control loop, llama.cpp and rescue backends, prompts, voice |
| [`core-shell`](crates/core-shell) | The login shell: the system's entire interface |
| [`core-ctl`](crates/core-ctl) | Admin tool: `doctor`, `exec`, `audit`, `grammar`, `catalog`, … |
| [`core-pkg`](crates/core-pkg) | `cpkg`, the package manager: signed repositories, transactional installs, JSON output |
| [`core-build`](crates/core-build) | Builds the OS from pinned sources: cross toolchain, chroot builds, packaging |
| [`os/`](os) | Recipes for the bootstrap and the base system, image and boot-test tools |
| [`system/`](system) | systemd units and configuration for the C.O.R.E. services |

Read [docs/BASE-OS.md](docs/BASE-OS.md) for how the distribution is built,
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design and how it relates
to the original blueprint, [docs/SECURITY.md](docs/SECURITY.md) for the threat
model, [docs/MODELS.md](docs/MODELS.md) for model choice and sizing, and
[docs/ROADMAP.md](docs/ROADMAP.md) for what comes next.

## Try it

**On any Linux machine, safely** (no root; read-only actions run, changes are only
simulated):

```sh
cargo run -p core-shell -- --dev --backend rescue        # rule-based, no model needed
cargo run -p core-shell -- --dev --llama-url http://127.0.0.1:8080   # with llama-server
```

**Build the base OS from source** (Linux build host, root, about 25 GB of disk
and several hours; details in [docs/BASE-OS.md](docs/BASE-OS.md)):

```sh
cargo build --release -p core-build -p core-pkg
B="./target/release/core-build --work /var/tmp/core-build --cache /var/cache/core-build/sources"
$B fetch && $B bootstrap && $B world && $B index --key ~/core-keys/core.key
sudo os/tools/mkimage.sh --repo /var/tmp/core-build/repo --key ~/core-keys/core.pub --out core.img
os/tools/boot-test.py core.img
```

## Status

* The AI stack (protocol, Guardian, agent, shell, telemetry) is complete and
  tested, and its grammar is verified against llama.cpp.
* The base OS is built from source by `core-build` and booted in QEMU by
  `os/tools/boot-test.py`; see [docs/BASE-OS.md](docs/BASE-OS.md).
* Next: move the AI stack's package and network actions from the earlier
  Arch-based prototype (pacman, NetworkManager) to `cpkg` and systemd-networkd,
  package it for the base OS, and build the standalone graphical sessions. See the
  [roadmap](docs/ROADMAP.md).

## Development

```sh
cargo test --workspace
LLAMA_CPP_DIR=~/llama.cpp tools/gbnf-check/run.sh
LLAMA_CPP_DIR=~/llama.cpp tools/e2e/run.sh
```

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md), including how to add an action.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
