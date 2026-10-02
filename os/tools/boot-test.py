#!/usr/bin/env python3
"""Boot a C.O.R.E. OS image in QEMU and check it from the inside.

    boot-test.py IMAGE [--uefi] [--password core] [--new-password ...]

The serial console is driven like a user would: wait for the login prompt, log in
as root, change the first-login password, run checks, power off. Exits non-zero
if any step times out or a check fails. A transcript is written to IMAGE.boot.log.
"""

import argparse
import os
import re
import select
import subprocess
import sys
import time

# systemd's shell integration wraps each command in OSC 3008 context sequences;
# strip those and other terminal escapes before matching output.
ESCAPES = re.compile(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-9;?]*[ -/]*[@-~]")

# Combined code+variables images, which -bios can load.
OVMF = ["/usr/share/ovmf/OVMF.fd", "/usr/share/OVMF/OVMF.fd"]

CHECKS = [
    # (description, command, regex the output must match)
    ("kernel", "uname -r", r"7\.0\.0-core"),
    ("os-release", ". /etc/os-release; echo $NAME", r"C\.O\.R\.E\. OS"),
    ("systemd state", "systemctl is-system-running --wait", r"^(running|degraded)"),
    ("failed units", "systemctl --failed --no-legend --plain | wc -l; systemctl --failed --no-legend --plain", r"\A0\s*\Z"),
    ("journal errors (shown, not fatal)", "journalctl -b --no-pager -q -p err -o cat | tail -n 20; echo journal-ok", r"journal-ok"),
    ("root file system", "findmnt -no SOURCE,FSTYPE,OPTIONS /", r"ext4\s+rw"),
    ("memory", "free -m | awk '/Mem:/{print \"used_mb=\"$3}'", r"used_mb=\d+"),
    ("packages", "cpkg list | wc -l", r"^\s*81\s*$"),
    ("package integrity", "cpkg verify && echo verify-ok", r"verify-ok"),
    ("library closure", "cpkg why glibc | head -3; echo why-ok", r"why-ok"),
    ("C compiler, glibc and kernel headers",
     "printf '#include <errno.h>\\n#include <pthread.h>\\n#include <stdio.h>\\n#include <linux/limits.h>\\n"
     "int main(void){printf(\"hello from core %%d %%d\\\\n\", EINVAL, PATH_MAX);}\\n' > /tmp/t.c"
     " && gcc -Wall -Werror /tmp/t.c -o /tmp/t && /tmp/t", r"hello from core 22 4096"),
    ("C++ compiler and threads",
     "printf '#include <iostream>\\n#include <thread>\\nint main(){std::thread t([]{std::cout<<\"c++ ok\"<<std::endl;});t.join();}\\n'"
     " > /tmp/t.cc && g++ -Wall -Werror /tmp/t.cc -o /tmp/tcc && /tmp/tcc", r"^c\+\+ ok"),
    ("Graphite (isl)",
     "printf 'void f(int*a){for(int i=0;i<64;i++)for(int j=0;j<64;j++)a[i]+=j;}\\n' > /tmp/g.c"
     " && gcc -O2 -floop-nest-optimize -c /tmp/g.c -o /tmp/g.o && echo graphite-ok", r"graphite-ok"),
    ("debugger", "gdb -nx -batch -ex 'python print(\"gdb python ok\")'", r"gdb python ok"),
    ("FUSE", "test -c /dev/fuse && echo fuse-ok", r"fuse-ok"),
    ("python", "python3 -c 'import ssl, ctypes, bz2, lzma, zlib, readline; print(ssl.OPENSSL_VERSION)'", r"^OpenSSL 3\.5"),
    ("network", "networkctl --no-legend list | head -5; ip -4 -o addr show scope global | head -2", r"inet \d+\."),
    ("dns", "resolvectl status >/dev/null && echo resolved-ok", r"resolved-ok"),
    ("man pages", "man -w ls", r"/usr/share/man/man1/ls\.1"),
]


class Console:
    def __init__(self, cmd, log):
        self.proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        self.buf = b""
        self.log = open(log, "wb")

    def expect(self, pattern, timeout):
        rx = re.compile(pattern.encode() if isinstance(pattern, str) else pattern)
        end = time.time() + timeout
        while True:
            m = rx.search(self.buf)
            if m:
                out = self.buf[: m.end()]
                self.buf = self.buf[m.end():]
                return out.decode(errors="replace")
            left = end - time.time()
            if left <= 0 or self.proc.poll() is not None:
                tail = self.buf[-2000:].decode(errors="replace")
                raise TimeoutError(f"waiting for {pattern!r}; last output:\n{tail}")
            r, _, _ = select.select([self.proc.stdout], [], [], min(left, 1.0))
            if r:
                data = os.read(self.proc.stdout.fileno(), 65536)
                self.log.write(data)
                self.log.flush()
                self.buf += data

    def send(self, text):
        self.proc.stdin.write(text.encode())
        self.proc.stdin.flush()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("image")
    ap.add_argument("--uefi", action="store_true")
    ap.add_argument("--password", default="core")
    ap.add_argument("--new-password", default="Core-boot-test-1")
    ap.add_argument("--memory", default="8G")
    ap.add_argument("--boot-timeout", type=int, default=900)
    a = ap.parse_args()

    accel = ["-accel", "kvm"] if os.access("/dev/kvm", os.W_OK) else ["-accel", "tcg", "-cpu", "max"]
    cmd = ["qemu-system-x86_64", *accel, "-m", a.memory, "-smp", "4", "-display", "none",
           "-serial", "stdio", "-monitor", "none", "-no-reboot",
           "-drive", f"file={a.image},format=raw,if=virtio,snapshot=on",
           "-nic", "user,model=virtio-net-pci"]
    if a.uefi:
        fw = next((f for f in OVMF if os.path.exists(f)), None)
        if not fw:
            sys.exit("no OVMF firmware found")
        cmd += ["-bios", fw]
    con = Console(cmd, a.image + (".uefi" if a.uefi else "") + ".boot.log")
    t0 = time.time()
    ok = True
    try:
        con.expect(r"login: ", a.boot_timeout)
        print(f"login prompt after {time.time() - t0:.0f}s")
        con.send("root\n")
        con.expect(r"[Pp]assword: ", 60)
        con.send(a.password + "\n")
        # Administrator-enforced change on first login; some versions ask for the
        # current password again first.
        seen = con.expect(r"(?i)(current|new) password: ", 60)
        if re.search(r"(?i)current password: $", seen):
            con.send(a.password + "\n")
            con.expect(r"(?i)new password: ", 60)
        con.send(a.new_password + "\n")
        con.expect(r"(?i)(retype|re-enter).*password: ", 60)
        con.send(a.new_password + "\n")
        con.expect(r"# ", 120)
        print("logged in as root, password changed")
        # The prompt is spelled split in the command so its echo cannot match.
        con.send("stty -echo cols 200; export TERM=dumb PS1='CO''RE# '\n")
        con.expect(r"CORE# ", 30)
        for desc, command, want in CHECKS:
            con.send(command + "; echo __END__\n")
            out = con.expect(r"__END__\r?\n", 600)
            out = ESCAPES.sub("", out).replace("\r", "")
            out = out.rsplit("__END__", 1)[0].strip()
            good = re.search(want, out, re.M) is not None
            ok &= good
            print(f"[{'ok' if good else 'FAIL'}] {desc}:")
            print("    " + out[:1500].replace("\n", "\n    "))
            con.expect(r"CORE# ", 30)
        con.send("systemctl poweroff\n")
        con.proc.wait(timeout=300)
    except (TimeoutError, subprocess.TimeoutExpired) as e:
        print(f"FAIL: {e}")
        ok = False
    finally:
        if con.proc.poll() is None:
            con.proc.kill()
    print("PASS" if ok else "FAILED")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
