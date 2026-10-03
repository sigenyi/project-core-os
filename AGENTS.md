# AGENTS.md

Instructions for AI coding agents (Codex, Claude Code and others) working in this
repository. `CLAUDE.md` imports this file, so this is the one place to keep them:
edit here, not there.

## The project

C.O.R.E. OS is a from-source Linux distribution controlled through natural language.
A local model emits one JSON intent from a closed catalog; the root daemon
`core-guardian` validates it, applies policy, asks the human before anything risky,
runs it without a shell and audits it. Start with `README.md`, then
`docs/DEVELOPMENT.md` (layout, commands, how to add an action), `docs/ARCHITECTURE.md`
and `docs/SECURITY.md`.

| Path | What it is |
|---|---|
| `crates/core-protocol` | Action catalog, validation, GBNF grammar, wire protocol (no I/O) |
| `crates/core-sense` | Telemetry collectors, insight rules, `core-sensed` |
| `crates/core-guardian` | The privileged executor daemon |
| `crates/core-agent` | Control loop, llama.cpp and rescue backends, prompts, voice |
| `crates/core-shell` | The login shell, the system's entire interface |
| `crates/core-ctl` | Admin and diagnostics CLI |
| `crates/core-pkg` | `cpkg`, the package manager |
| `crates/core-build` | Builds the OS from the recipes in `os/` |
| `os/` | Bootstrap and base-system recipes, image and boot-test tools |
| `system/` | systemd units and configuration for the C.O.R.E. services |
| `tools/` | Grammar check and end-to-end test against llama.cpp |

## Checks to run before every push

These are the commands CI runs (`.github/workflows/ci.yml`). Run the ones that cover
what you changed, and all three Rust ones for any Rust change:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

shellcheck -x os/tools/*.sh tools/*/*.sh
python3 -m py_compile os/tools/*.py os/recipes/*/*.py
python3 os/recipes/lib/compare-results.py --self-test
python3 os/tools/boot-test.py --self-test
cargo run -q -p core-build -- --work /tmp/core-w --recipes os status   # recipes parse and are complete
```

To try the system without changing anything (no root; changes are simulated):

```sh
cargo run -p core-shell -- --dev --backend rescue
```

## Linux only

The code targets Linux. `core-guardian` reads peer credentials with `SO_PEERCRED`,
which macOS does not have, and `core-shell` depends on it, so the workspace does not
build on a Mac. On macOS, run the Rust commands in a Linux container from the
repository root, for example:

```sh
docker run --rm -v "$PWD":/src -w /src rust:latest sh -c 'rustup component add rustfmt clippy && cargo test --workspace --locked'
```

Do not add `cfg` stubs or fake macOS implementations to make it compile there.

## Do not, unless the user asks

* Build the base OS (`core-build fetch`, `bootstrap`, `world`, `build`, any
  `--check` run, `os/tools/mkimage.sh`, `os/tools/boot-test.py`). It needs a Linux
  host, root, about 25 GB and hours.
* Run `core-build prune`: it deletes every file in the build root that no package
  owns. Use `--dry-run` to see what it would remove.
* Unmount anything in the build root. When core-build refuses to start because
  something is already mounted there, report the mounts it lists: they may belong
  to a build that is still running.
* Loosen a test gate: lower a `--min-pass` or `--require` floor, add an entry to
  an `expected-failures.txt` without a comment giving its cause, or accept a
  `cpkg verify` finding in `os/tools/boot-test.py` without saying why it is
  expected.
* When reporting build or test results, say exactly what ran: which packages a
  run rebuilt and which it skipped as up to date, and the bad results accepted
  as documented exceptions. A run without `--force` is not a clean rebuild
  (`docs/BASE-OS.md`, "How the current packages were built").
* Run anything with `sudo`, or run `core-guardian` without `--dry-run`.
* Commit keys, models or build output: signing keys (`~/core-keys`), `*.gguf`,
  `*.iso`, `target/`, `image/`.

## Rules that keep the security model intact

The model is untrusted. `docs/SECURITY.md` explains why; these rules follow from it.

* Anything the model emits is untrusted until `ValidatedAction::from_intent`.
* No shell, ever. Build argv vectors and put `--` before operands where the tool
  supports it. Any new program the Guardian runs goes in `default_tools()`.
* Choose risk levels honestly: they decide whether a human is asked first.
* Never weaken validation, policy or confirmation to make a test pass, and never
  skip, disable or delete a test to get green.
* Observations are data. Guidance for the model goes on its own line using the
  `GUIDE_*` constants.
* Keep the system prompt static so llama.cpp's prompt cache stays warm.
* To add an action, follow "Adding an action" in `docs/DEVELOPMENT.md`.

## Style

* Rust 2024 edition, minimum Rust 1.85, `rustfmt.toml` (120 columns).
* Commit subjects are short and specific, prefixed with the component when there is
  one: `glibc: keep __unused out of the public pthread mutex header`.
* When behaviour or commands change, update the matching file in `docs/` and this
  file in the same change.
* License: GPL-3.0-or-later.

## Working alongside another agent

More than one agent may work on this repository: say, Claude Code writing and Codex
reviewing, or the other way round.

* One task, one branch, never `main`. Claude Code uses `claude/<topic>`, Codex uses
  `codex/<topic>`.
* Never commit to, rebase or force-push another agent's branch. To work in parallel,
  use a separate worktree: `git worktree add ../core-os-<topic> -b <branch>`.
* Reviewing: read the diff (`git diff main...HEAD`), report findings as
  `path:line` with what breaks and why, and leave the fixes to the author unless the
  user asks you to make them.
* Handing off: end with what changed, which checks you ran and their results, and
  what is left. Put it in the PR description or your final message.
* Run one round of review per request. Don't call the other agent in a loop unless
  the user asks for it.
