# Developing C.O.R.E.

## Layout

```
crates/
  core-protocol/   action catalog, validation, grammar, wire protocol (no I/O)
  core-sense/      telemetry collectors, insights, core-sensed daemon
  core-guardian/   privileged executor daemon
  core-agent/      orchestrator library: control loop, backends, prompts, voice
  core-shell/      the login shell (console UI)
  core-ctl/        admin and diagnostics CLI
  core-pkg/        cpkg, the package manager
  core-build/      builds the OS from recipes
os/bootstrap/      recipes: cross toolchain and temporary tools
os/recipes/        recipes: the base system packages
os/tools/          source lookup, image assembly, QEMU boot test
system/            units and configuration for the C.O.R.E. services
tools/gbnf-check/  validates the grammar with llama.cpp's GBNF engine
tools/e2e/         end-to-end test against a live llama-server
tools/eval/        evaluation tasks: validation, frozen splits, disposable-VM runner
eval/              evaluation task specifications and the frozen split manifest
docs/              architecture, security, models, roadmap
```

## Everyday commands

```sh
cargo test --workspace                       # unit and integration tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all

# Try the system on any Linux machine: no root, nothing is changed
# (read-only actions run for real; changes are simulated).
cargo run -p core-shell -- --dev --backend rescue
cargo run -p core-shell -- --dev --llama-url http://127.0.0.1:8080   # with a model
cargo run -p core-shell -- --dev --backend script --script intents.jsonl  # replay intents, no model

cargo run -p core-sense --bin core-sensed -- --once --summary        # what the AI sees
cargo run -p core-ctl -- catalog                                     # every action
cargo run -p core-ctl -- grammar                                     # the GBNF grammar
cargo run -p core-ctl -- validate '{"action":"restart_service","args":{"service":"-x"}}'
cargo run -p core-ctl -- contract                                    # action contract fingerprint
cargo run -p core-guardian -- --config system/etc/core/guardian.toml \
    --plan '{"action":"install_package","args":{"package":"nano"}}'  # policy and plan, nothing runs
```

Evaluation tasks (docs/TRAINING.md):

```sh
python3 tools/eval/tasks.py            # list and validate eval/tasks/*.toml
python3 tools/eval/split.py check      # held-out tasks unchanged since the freeze
python3 tools/eval/split.py add        # assign new tasks to splits
cargo build -p core-guardian -p core-ctl
tools/eval/vm-run.py --image image/core.img --repo REPO_DIR --out /tmp/eval-run [TASK ...]
```

With a llama.cpp checkout that has `llama-server` built:

```sh
LLAMA_CPP_DIR=~/llama.cpp tools/gbnf-check/run.sh   # grammar vs llama.cpp's parser
LLAMA_CPP_DIR=~/llama.cpp tools/e2e/run.sh          # live server, tiny synthetic model
```

## Running the real services locally

```sh
cargo build --release
sudo ./target/release/core-guardian --dry-run --socket /tmp/core/guardian.sock &
./target/release/core-sensed --output /tmp/core/telemetry.json &
cat > /tmp/core/agent.toml <<'EOF'
[guardian]
socket = "/tmp/core/guardian.sock"
[telemetry]
path = "/tmp/core/telemetry.json"
EOF
./target/release/core-shell --config /tmp/core/agent.toml
./target/release/core-ctl --config /tmp/core/agent.toml doctor
```

The Guardian only accepts root, `allowed_uids` and members of `allowed_groups`
(default: `core`). Add your uid to a test config if needed.

## Adding an action

Say you want `flush_dns` (restart the resolver's cache).

1. **Catalog** (`crates/core-protocol/src/catalog.rs`): add an entry with a name,
   one-line summary (the model reads it), parameters, risk, executor and an example.
2. **Typed action** (`crates/core-protocol/src/action.rs`): add a variant to
   `Action`, a `describe()` line, and a `from_intent` arm that reads arguments
   through the typed accessors. New argument types also need a validator
   (`validate.rs`), a `ParamKind`, and a grammar rule (`grammar.rs`).
3. **Execution**:
   * Guardian actions: add a `Planner::plan` arm that builds `CommandSpec`s or
     `NativeOp`s. Any new program must be added to `default_tools()`.
   * Agent actions: handle the variant in `Agent::perform`.
4. **Tests run themselves.** Every catalog example is validated, planned and
   checked against llama.cpp's grammar engine. Add focused tests for the new
   planner arm and for invalid arguments.
5. **Rescue mode** (optional): teach `backend/rescue.rs` a phrase for it.

Choose the risk level honestly: it decides whether a human is asked first.

## Adding a telemetry collector or insight

Implement `core_sense::Collector` (fill your part of the snapshot from the
`Sysroot`) or `core_sense::InsightRule` (derive findings from a snapshot), register
it in `default_collectors()` or `default_rules()`, and add a fixture-based test. The
`Sysroot` abstraction means tests describe machines as directory trees. See
`crates/core-sense/tests/fixture.rs`.

## Conventions

* Anything the model emits is untrusted until `ValidatedAction::from_intent`.
* Observations are data; guidance for the model goes on its own line using the
  `GUIDE_*` constants so user-facing output can strip it.
* No shell, ever: build argv vectors, and put `--` before operands when the tool
  supports it.
* Keep the system prompt static so llama.cpp's prompt cache stays warm. Put
  per-request data in the user message.
