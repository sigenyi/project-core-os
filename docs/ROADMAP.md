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
- [x] Hardened systemd units, configs, installer for existing distros
- [x] archiso image build with portable llama.cpp/whisper.cpp and model fetching
- [x] End-to-end test against a live llama-server (`tools/e2e`)
- [x] CI: fmt, clippy, tests, shellcheck, grammar and e2e jobs

## Phase 2: first boot on real hardware

- [ ] Build and boot the ISO in QEMU and on two or three laptops; fix what breaks
- [ ] Pin model checksums in `image/models.conf`
- [ ] Evaluation harness: a suite of realistic requests ("my wifi doesn't work"
      with a fixture machine plus scripted Guardian responses) scored per model,
      to choose the default model and tune the prompt
- [ ] Ask llama-server for its real context size (`/props`) instead of duplicating it
- [ ] Stream model output so long answers start appearing immediately

## Phase 3: installing and updating

- [ ] `install_system` action: guided installation from the live medium to a disk
      (partitioning, bootloader, user creation), with every destructive step
      confirmed
- [ ] Immutable root with A/B updates (e.g. systemd-sysupdate) and rollback as an
      action ("undo the last update")
- [ ] Signed images and update channel
- [ ] Snapshot before high-risk actions on btrfs, with `rollback` as an action

## Phase 4: deeper perception

- [ ] eBPF collectors (aya) behind the existing `Collector` trait: OOM kills,
      process crash and exec-failure events, TCP retransmits and DNS failures,
      block I/O latency
- [ ] Event-driven telemetry refresh (netlink, udev, inotify) instead of polling
- [ ] Journal-aware insights (crash-looping units, coredumps)

## Phase 5: richer interaction, still no UI

- [ ] Spoken replies (local TTS such as Piper) so a voice-first session needs no
      screen at all
- [ ] Wake word / hands-free mode
- [ ] Multi-language prompts and rescue phrases
- [ ] More actions: Bluetooth pairing, printers, displays and brightness profiles,
      firewall rules, user accounts, backups, scheduled tasks, containers

## Phase 6: slimming the base

- [ ] Minimal LFS-derived (or Arch-derived, stripped) base built around the
      package needs measured in phases 2-5
- [ ] Measure and reduce idle RAM so more of it goes to the model
