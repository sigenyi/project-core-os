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
`os/tools/ubuntu-orig.py` looks those up, and `os/tools/source-block.py` turns them
into a recipe's `[[source]]` block. Only the upstream tarball is used: no Debian or
Ubuntu patches, packaging or binaries.

A few of those tarballs are repacked by Debian, not byte-identical to upstream:

* `+dfsg` releases (gmp, make, tar, bison) and gawk drop the GNU Free Documentation
  License manuals. Their recipes say so and build without the Texinfo manual (man
  pages are kept). gmp and make lose their `doc/` directory entirely.
* GCC is wrapped: the orig tarball contains the upstream `gcc-15.2.0.tar.xz`, which
  recipes unpack with `inner`.
* expat, libxcrypt, libffi and kbd are snapshots of the upstream git tag; their
  recipes generate `configure` with the project's own `buildconf.sh`/`autogen.sh`.

Those recipes list only the archive URL, since the upstream file has a different
checksum.

## Fixes to upstream sources

| Package | Problem | Fix |
|---|---|---|
| glibc 2.43 | `<sys/mount.h>` redefines `OPEN_TREE_CLONE`, which Linux 7.0's `<linux/mount.h>` now spells `(1 << 0)`; glibc builds with `-Werror` | Define the `open_tree` flags only when the kernel header has not |
| GCC 15.2 (libgomp) | glibc 2.43's `strchr` returns `const char *` for a `const` argument (C23); libgomp builds with `-Werror` | Declare the read-only result `const` |
| libxcrypt 4.5.1 | Same `strchr` change, triggered by a needless `const` cast on a writable buffer | Drop the cast |
| elfutils 0.194 | Same change in the RISC-V disassembler | Only `libelf` is built, which is all the base needs |
| bc 1.07.1 | Post-processes its math library with `ed`, which the base does not ship | The same edits with `sed`; the recipe checks bc's output |
| GRUB 2.14 (BIOS) | With binutils 2.46, `--image-base` places the ELF headers at the base address, so `kernel.img` starts 0x74 bytes late and `grub-mkimage` rejects it | Use GRUB's `-Ttext` fallback for the BIOS build; the recipe checks the entry point is 0x9000 |
| GRUB 2.14 | `grub-core/extra_deps.lst` is missing from the release archive | Recreate it (its content in GRUB's repository) |
| procps-ng 4.0.4 | Includes `<ncursesw/ncurses.h>`, a Debian layout | Our wide-only ncurses installs `<ncurses.h>` |
| kbd 2.7.1 | `autogen.sh` needs `which` | `autoreconf -fi` |
| binutils 2.46 (temporary) | libtool would link libctf against the host's libraries | Drop `$add_dir` in `ltmain.sh`, as Linux From Scratch does |

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

## Building it

Requirements on the build machine: root, a C/C++ compiler, GNU make, bison, gawk,
m4, perl, python3, texinfo, xz, and for the image `sfdisk`, `mkfs.ext4`,
`mkfs.vfat` and mtools. About 25 GB of disk.

```sh
cargo build --release -p core-build -p core-pkg
B="./target/release/core-build --work /var/tmp/core-build --cache /var/cache/core-build/sources"
$B fetch          # download and verify every source
$B bootstrap      # cross toolchain + temporary tools (os/bootstrap)
$B world          # every package of the base system (os/recipes)
$B index --key /path/outside/the/repo/core.key   # sign the repository index
sudo os/tools/mkimage.sh --repo /var/tmp/core-build/repo \
     --key /path/outside/the/repo/core.pub --out core.img
os/tools/boot-test.py core.img            # BIOS
os/tools/boot-test.py core.img --uefi     # UEFI
```

`core-build` skips recipes whose stamp matches the recipe (and any files it brings
in), so an interrupted build resumes where it stopped; `--force` rebuilds. Logs go
to `<work>/logs/<recipe>.log`.

Recipe scripts are TOML literal strings (`'''`), so shell line continuations and
backslashes reach bash unchanged.

## The image

`mkimage.sh` installs the packages into an empty directory with `cpkg --root`,
which runs the package hooks (library cache, system users, hardware database,
service presets). It then adds what is specific to one machine: the trusted
repository key, network configuration (DHCP on wired interfaces through
systemd-networkd), an empty machine ID, and the root password. That password must
be changed at first login. The disk is GPT with a BIOS boot partition, an EFI
system partition holding GRUB as `\EFI\BOOT\BOOTX64.EFI`, and the ext4 root
partition, typed as the x86-64 root partition so systemd can discover it. The
kernel mounts it by PARTUUID.

## Verified

The image built from these recipes boots under QEMU (8 GB RAM) with both BIOS
and UEFI firmware, and `os/tools/boot-test.py` passes on both. The test reaches a
login prompt in about 25 seconds (software emulation, no KVM), logs in as root
with the forced password change, and checks:

* Linux 7.0.0-core is running and systemd reports `running` with no failed units
  and no errors in the journal.
* The root file system is mounted read-write from the GPT root partition.
* Idle memory use is about 250 MB, within the 300 MB budget for an 8 GB machine.
* All 81 packages are installed, and `cpkg verify` and `cpkg why` work on the
  running system.
* The native GCC compiles and runs a program, and Python has ssl, ctypes and the
  compression modules.
* systemd-networkd gets an address by DHCP, and systemd-resolved answers.
* Manual pages are installed.

## Not in the base yet

* An initramfs generator, for encrypted or unusual root devices.
* Test suites: recipes do not run `make check` yet. That is the next hardening
  step for the toolchain packages (glibc, GCC, binutils).
* `sqlite`, `curl`, `which` and other common tools; the package set is the
  minimum for a self-hosting, bootable, networked system.
* The Rust toolchain as an OS package. C.O.R.E.'s own programs are compiled on the
  build host for now. They need only glibc ≥ 2.39, so they run unchanged on the new
  system.
* Firmware for real hardware (`linux-firmware`), Wi-Fi tooling, and the graphics
  stack for standalone app sessions.
* eBPF with BTF type information, which needs `pahole` at kernel build time.
