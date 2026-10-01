# The C.O.R.E. base OS

C.O.R.E. OS is its own distribution, built from source. It is not derived from any
other distribution, and it uses no other distribution's binaries or repositories.
Every file in a C.O.R.E. image belongs to a package. Each package was built by
`core-build` from a pinned recipe in `os/recipes/` and installed by `cpkg`, the
C.O.R.E. package manager. The base OS is also the environment the C.O.R.E. model
will be trained and evaluated in.

## Decisions

| Area | Choice | Why |
|---|---|---|
| Method | Bootstrap from a cross toolchain (the Linux From Scratch method), then build everything natively | Full control and provenance: nothing is inherited from a host distribution |
| Package manager | **cpkg**, our own (Rust) | Packages carry machine-readable metadata the AI needs: what binaries, libraries, services and config files a package provides, how to launch it, and why it is installed. Every query has JSON output |
| Builder | **core-build**, our own (Rust) | Declarative recipes, pinned sources, chroot builds, automatic runtime dependencies from ELF headers |
| C library | glibc 2.43 | GPU drivers (Mesa, NVIDIA) and ML runtimes target glibc; musl would cost compatibility for a small memory saving |
| Toolchain | GCC 15.2, binutils 2.46 | Current stable releases |
| Kernel | Linux 7.0, our own configuration | Root-disk drivers are built in, so the base needs no initramfs. Graphics, sound and network drivers are modules loaded by udev |
| Init and services | systemd 259 | Rich, introspectable service state (units, journal, D-Bus) for the AI. Socket activation and sandboxing are used by the C.O.R.E. daemons. Models already understand it well |
| Boot loader | GRUB 2.14 | Boots both BIOS and UEFI machines |
| Filesystem layout | Merged `/usr` (`/bin`, `/sbin`, `/lib` are symlinks into `/usr`) | One place for every program, which makes the system simpler to reason about |
| Graphical programs | **Standalone app sessions** | The OS has no desktop. When the user asks for a graphical program, the AI starts a single-application Wayland compositor (a kiosk session) showing only that program, full screen, and returns to the conversation when it exits. The graphics stack (Mesa, Wayland, a kiosk compositor) is built after the base |
| Memory target | 8 GB RAM | Budget: under 300 MB for the idle OS, about 4.5 GB for a 4B-parameter model at 8K context, and the rest for the user's programs |

## Source provenance

Recipes pin every source by SHA-256. The primary URL is always the upstream release.
Mirrors are listed after it. The build machine used so far cannot reach most upstream
hosts, so the mirror used in practice is Ubuntu's archive copy of the same upstream
release tarball (`<package>_<version>.orig.tar.*`, listed in its GPG-signed index).
`os/tools/ubuntu-orig.py` looks those up. Only the upstream tarball is used: no
Debian or Ubuntu patches, packaging or binaries.

## Build stages

1. **Cross toolchain** (`os/bootstrap/`). binutils, GCC, the Linux API headers and
   glibc are built for a separate target triplet in `$ROOT/tools`, so nothing from
   the host can leak into the new system.
2. **Temporary tools.** The minimum set of utilities, cross-compiled into `$ROOT`,
   then a few more inside a chroot. These are discarded at the end.
3. **Final packages** (`os/recipes/`). Built inside the chroot by `core-build`,
   packaged as `.cpk` files and installed with `cpkg`, in dependency order.
4. **Image.** A fresh root filesystem is assembled with `cpkg install --root` from
   the packages alone, so no build leftovers can reach it. Then it is configured,
   written to an ext4 partition, and made bootable with GRUB.

## Not in the base yet

* An initramfs generator, for encrypted or unusual root devices.
* The Rust toolchain as an OS package. C.O.R.E.'s own programs are compiled on the
  build host for now. They need only glibc ≥ 2.39, so they run unchanged on the new
  system.
* Firmware for real hardware (`linux-firmware`), Wi-Fi tooling, and the graphics
  stack for standalone app sessions.
* eBPF with BTF type information, which needs `pahole` at kernel build time.
