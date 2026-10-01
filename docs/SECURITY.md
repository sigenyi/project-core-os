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
otherwise the Guardian refuses to start) controls which actions exist and the
risk ceiling for automatic approval. It also lists which services are protected
from being stopped (the Guardian, D-Bus, journald, logind, ...) and which units
service actions may never touch (power-state and rescue units, which have their own
confirmed actions), plus which packages may never be removed. Unit aliases are
resolved first (`autovt@tty1` is `getty@tty1`). Targets and mounts are not accepted
as services at all, because `start_service reboot.target` would otherwise be a
reboot without confirmation. Arguments can raise an action's risk: pinging an
unusually long or deep hostname (a DNS exfiltration channel) needs confirmation.

**4. Human confirmation.** Actions above the ceiling (by default: anything
persistent or destructive) wait for a human. The confirmation token is random,
single-use, expires after 120 s and is bound to the connection. The prompt is shown
by `core-shell` and answered from the keyboard, never by the model. `launch_program`
asks too when given options (`vim -c …`) or a URL (a way to send data out).

**5. No shell, minimal environment.** Every executed program comes from the
configured `[tools]` table (absolute paths), receives a discrete argv with `--`
before operands where supported, a scrubbed environment, `/dev/null` stdin, a
timeout enforced on its whole process group, and an output cap.

**6. Privilege separation.** Only actions that need root reach the Guardian.
Reading files and listing directories happen in the unprivileged agent, with the
user's own permissions, so the kernel decides what is readable. The Guardian never
opens a path chosen by the model; its only native read is a compile-time constant
(`/etc/resolv.conf`). In the agent, opens are non-blocking and refuse to follow a
final symlink, and the type of what was actually opened is checked (FIFOs, devices
and sockets are refused), so a read cannot hang or be redirected.

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

**9. Audit.** `/var/log/core/audit.jsonl` (0600) records every Guardian request:
peer uid and pid as reported by the kernel, action, redacted arguments, risk,
decision, per-step exit codes and duration. Read it with `core-ctl audit`.

**10. Robust transport.** The rate limit is per uid, so reconnecting does not reset
it. Idle connections time out, but not before any pending confirmation could. Only
changes are serialised, so reads never queue behind a long installation. The
client re-sends a request only when it provably never arrived, so an action is
never run twice.

**11. Secrets.** Beyond what file permissions already prevent, the agent's path
policy keeps credentials the user *can* read out of the model's context:
`~/.ssh`, `~/.gnupg`, keyrings, `.netrc`, `*.key`, `*.pem`, and on misconfigured
systems `/etc/shadow`, WireGuard and Bluetooth keys, and NetworkManager/iwd
connection stores. Kernel interfaces that block or stream forever (`/proc/kmsg`,
`trace_pipe`) are excluded too. Wi-Fi passphrases are a `Secret` type: never printed
by `Debug` and redacted in confirmations, reports and the audit log.

## Known limitations

* The Guardian runs as root: it installs packages and manages services, so its
  code (and the tools it runs) are in the trusted computing base. It is written in
  safe Rust apart from a few audited `libc` calls. An independent review of this
  code found and fixed: target units bypassing confirmation, alias names bypassing
  protection, a blocking read and a symlink race in root file reads (fixed by moving
  reads out of the Guardian), apt's `pkg-` removal syntax, option injection in apk
  and xbps searches, and double execution on client retry.
* Wi-Fi passphrases are passed to `nmcli`/`iwctl` as arguments and are briefly
  visible in `/proc/<pid>/cmdline` to other local users. C.O.R.E. is a single-user
  system; mounting `/proc` with `hidepid=invisible` closes this on multi-user hosts.
* The model can read the user's own files (as the user). Content read this way can
  contain injected instructions; layers 1-4 bound what such content can achieve.
  Channels that could carry data out (package installs, Wi-Fi changes, URLs,
  unusual hostnames) need confirmation. Plain pings to ordinary hostnames do not.
* The live ISO logs `core` in automatically, without a password. Installed systems
  should remove the autologin drop-in or set a password.
* The rescue planner matches keywords. It goes through exactly the same validation,
  policy and confirmation path as the model.

## Reporting

Please report vulnerabilities privately through GitHub security advisories on this
repository rather than in public issues.
