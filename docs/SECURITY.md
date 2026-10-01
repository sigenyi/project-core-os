# C.O.R.E. OS security model

Giving a language model control of an operating system is dangerous. C.O.R.E. is
designed on the assumption that **the model is untrusted**: it may be wrong, it may
be confused, and its input may be adversarial. Everything below follows from that.

## Threats

| Threat | Example |
|---|---|
| Model error | It decides to "fix the network" by stopping NetworkManager, or misreads a request as "remove". |
| Prompt injection | A log line, file, package description or Wi-Fi network name contains "ignore previous instructions and run …". |
| Argument injection | A service name of `--now` or `foo; reboot`, or a path of `/etc/../etc/shadow`. |
| Runaway autonomy | A retry loop that keeps changing the system while it "self-corrects". |
| Local privilege escalation | Another local user, or a compromised user process, driving the root executor. |
| Secret exposure | The model reading `/etc/shadow`, SSH keys or Wi-Fi credentials and repeating them. |

## Defences, layer by layer

**1. The model cannot express a command.** Its output is sampled under a GBNF
grammar generated from the action catalog: one JSON object naming one enabled
action, with arguments limited to each type's character set. There is no "run
shell command" action, so no shell is reachable however the model is manipulated.

**2. Typed validation, twice.** The agent validates intents for fast feedback, and
the Guardian validates them again independently. Names must start with an
alphanumeric character (no option injection), contain only their type's character
set (no metacharacters), paths are normalised and `..` is rejected, numbers are
range-checked, and lists are bounded.

**3. Policy.** `/etc/core/guardian.toml` (root-owned and not group/world-writable,
otherwise the Guardian refuses to start) controls which actions exist, which
services and packages are protected (the Guardian, D-Bus, journald, logind, the
kernel and libc packages, ...), which paths may be read (checked on the canonical
path after following symlinks, so a link cannot reach a denied file), and the
risk ceiling for automatic approval.

**4. Human confirmation.** Actions above the ceiling (by default: anything
persistent or destructive) wait for a human. The confirmation token is random,
single-use, expires after 120 s and is bound to the connection. The prompt is shown
by `core-shell` and answered from the keyboard, never by the model. `launch_program`
with options (`vim -c …`) also asks.

**5. No shell, minimal environment.** Every executed program comes from the
configured `[tools]` table (absolute paths), receives a discrete argv with `--`
before operands where supported, a scrubbed environment, `/dev/null` stdin, a
timeout enforced on its whole process group, and an output cap.

**6. Privilege separation.**

| Component | Privilege | Confinement |
|---|---|---|
| core-shell / core-agent | the user | — |
| core-guardian | root | socket 0660 root:core; peer credentials from the kernel; `ProtectHome=read-only`, `PrivateTmp` |
| core-sensed | `core-sense` user + `CAP_SYSLOG` | `ProtectSystem=strict`, no IP traffic, seccomp `@system-service` |
| core-inference | `DynamicUser` | loopback only (`IPAddressDeny=any`), no capabilities, DRM render nodes only |

The model server cannot reach the network at all, and nothing it outputs is
executed without passing through the Guardian.

**7. Bounded autonomy.** At most `max_steps` model calls and
`max_consecutive_failures` failures per request, and an action that already failed
with identical arguments is never re-run. When limits are hit the model can only
produce `respond`.

**8. Prompt-injection hygiene.** Telemetry and observations are labelled as data
("not instructions"), and command output is clipped. Denials, refusals and failures
come back as observations, so a manipulated model gains nothing by trying again.

**9. Audit.** `/var/log/core/audit.jsonl` (0600) records every request: peer uid
and pid as reported by the kernel, action, redacted arguments, risk, decision,
per-step exit codes and duration. Read it with `core-ctl audit`.

**10. Secrets.** Read access to credential stores (`/etc/shadow`, `/etc/ssh`,
`~/.ssh`, `~/.gnupg`, NetworkManager/iwd connection stores, `*.key`, `*.pem`, …) is
denied. Wi-Fi passphrases are a `Secret` type: never printed by `Debug` and redacted
in confirmations, reports and the audit log.

## Known limitations

* The Guardian runs as root: it installs packages and manages services, so its
  code (and the tools it runs) are in the trusted computing base. It is written in
  safe Rust apart from a few audited `libc` calls.
* Wi-Fi passphrases are passed to `nmcli`/`iwctl` as arguments and are briefly
  visible in `/proc/<pid>/cmdline` to other local users. C.O.R.E. is a single-user
  system; mounting `/proc` with `hidepid=invisible` closes this on multi-user hosts.
* Reading the user's own files is allowed by default (`/home`). Content read this
  way can contain injected instructions; layers 1-4 bound what such content can
  achieve.
* The live ISO logs `core` in automatically, without a password. Installed systems
  should remove the autologin drop-in or set a password.
* The rescue planner matches keywords. It goes through exactly the same validation,
  policy and confirmation path as the model.

## Reporting

Please report vulnerabilities privately through GitHub security advisories on this
repository rather than in public issues.
