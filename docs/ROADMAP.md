# Roadmap

Status as of this milestone, and what comes next. Each phase leaves the system usable.

## Phase 1: foundation (done)

- [x] Action catalog with typed validation, GBNF grammar generation and a
      length-prefixed JSON wire protocol (`core-protocol`)
- [x] Grammar verified against llama.cpp's own GBNF engine (`tools/gbnf-check`)
- [x] Telemetry collectors, insight rules, compact summaries, `core-sensed` daemon
- [x] Guardian: peer authentication, policy, confirmations, per-distro planner,
      shell-free runner, native operations, audit log, socket activation
- [x] Agent control loop with bounded self-correction, context budgeting,
      conversation memory, and a rescue planner for when no model can run
- [x] llama.cpp backend (OpenAI-compatible endpoint with grammar); whisper.cpp voice
      input (push-to-talk)
- [x] Console login shell with human confirmations and a dev mode; `core-ctl doctor`
- [x] Hardened systemd units and configs
- [x] ~~archiso image build~~ (an Arch-based prototype, removed: C.O.R.E. OS is
      its own distribution, see phase 2)
- [x] End-to-end test against a live llama-server (`tools/e2e`)
- [x] CI: fmt, clippy, tests, shellcheck, grammar and e2e jobs

## Phase 2: the base OS, from source

See [BASE-OS.md](BASE-OS.md).

- [x] `cpkg`: package format, signed repositories, dependency resolution from ELF
      sonames, transactional installs, hooks, JSON output, AI metadata
- [x] `core-build`: pinned sources, cross toolchain, chroot builds, merged-/usr
      normalisation, packaging, resumable builds
- [x] Bootstrap recipes (cross toolchain `x86_64-core-linux-gnu`, temporary tools)
- [x] Base-system recipes: glibc, GCC, systemd, Linux 7.0, GRUB and 84 more
- [x] Toolchain test suites (glibc, binutils, GCC) run on every build with
      `--check`; every unexpected result fixed or documented with its cause
- [ ] Test suites for the rest of the base (Python, Perl, coreutils, ...)
- [x] Image assembly from packages only; boots in QEMU with BIOS and UEFI and
      passes `os/tools/boot-test.py`
- [x] Move the Guardian's package and network actions from pacman and
      NetworkManager to `cpkg` and systemd-networkd/resolved (Wi-Fi needs a daemon
      that is not in the base yet; see TRAINING.md, D11)
- [x] Package the C.O.R.E. services for the base OS (`core-os`), with an image that
      boots and passes the integration gate through the real Guardian
- [ ] Package llama.cpp and whisper.cpp (and a model, D8)
- [ ] Rust toolchain as an OS package, so C.O.R.E. builds itself
- [ ] A public package repository and `cpkg upgrade` against it
- [ ] Graphics stack for standalone app sessions: Mesa, Wayland, a kiosk
      compositor; the AI starts one program full screen and returns to the
      prompt when it exits
- [ ] Firmware and Wi-Fi tooling for real hardware; boot on two or three laptops

## Phase 3: the model

See [TRAINING.md](TRAINING.md): the integration gate and the trained model's product
acceptance gate are separate, and the open decisions (teacher, student, licenses,
data policy, budget, thresholds) are listed there.

- [x] Foundation: versioned action contract, sanitized trajectory format, held-out
      task specifications with frozen splits, disposable-VM task runner
- [ ] Training environment: the base OS in VMs, with fixtures for broken states
      (stopped services, missing packages, bad configuration)
- [ ] Pilot: a small validated dataset and evaluation with stop/go criteria, before
      costly generation or training
- [ ] Data: tasks and trajectories collected against real systems, checked by
      replaying them through the Guardian
- [ ] Distil a 4B-class model for the action catalog; evaluate it per task family
      on the frozen held-out set (product acceptance gate)
- [ ] Pin model checksums; ask llama-server for its real context size (`/props`)
- [ ] Stream model output so long answers start appearing immediately

## Phase 4: installing and updating

- [ ] `install_system` action: guided installation from the live medium to a disk
      (partitioning, bootloader, user creation), with every destructive step
      confirmed
- [ ] Immutable root with A/B updates (e.g. systemd-sysupdate) and rollback as an
      action ("undo the last update")
- [ ] Signed images and update channel
- [ ] Snapshot before high-risk actions on btrfs, with `rollback` as an action

## Phase 5: deeper perception

- [ ] eBPF collectors (aya) behind the existing `Collector` trait: OOM kills,
      process crash and exec-failure events, TCP retransmits and DNS failures,
      block I/O latency
- [ ] Event-driven telemetry refresh (netlink, udev, inotify) instead of polling
- [ ] Journal-aware insights (crash-looping units, coredumps)

## Phase 6: richer interaction, still no UI

- [ ] Spoken replies (local TTS such as Piper) so a voice-first session needs no
      screen at all
- [ ] Wake word / hands-free mode
- [ ] Multi-language prompts and rescue phrases
- [ ] More actions: Bluetooth pairing, printers, displays and brightness profiles,
      firewall rules, user accounts, backups, scheduled tasks, containers

## Phase 7: slimming the base

- [ ] Measure and reduce idle RAM so more of it goes to the model
