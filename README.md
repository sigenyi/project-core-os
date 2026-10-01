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

* **No UI.** No display server, no desktop, no windows. You log in to a text
  prompt (or press push-to-talk) and say what you want.
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
               │       core-guardian (root) ──argv──▶ systemctl, pacman, nmcli, …
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
| [`system/`](system) | systemd units, configs, installer for existing distros |
| [`image/`](image) | archiso-based ISO builder (portable llama.cpp/whisper.cpp, models) |

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design and how it relates
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

**On an Arch Linux VM** (or another systemd distro). This makes `core` a
conversational user and boots to the console:

```sh
sudo system/install.sh --user core --autologin --disable-gui \
     --with-llama --with-whisper --with-models --allow-unpinned
core-ctl doctor
```

**As a bootable ISO** (Arch host, or any host with podman/docker):

```sh
sudo image/build-iso.sh --allow-unpinned        # or: image/build-in-container.sh
image/run-qemu.sh                               # boots straight into the prompt
```

## Status

The foundation is complete and tested: all components, the hardened services, the
installer and the image build. The grammar is verified against llama.cpp's own
engine, and an end-to-end test drives the full loop through a live llama-server.
The ISO has not been booted on real hardware yet; that is the next phase. See the
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
