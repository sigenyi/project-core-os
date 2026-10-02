#!/usr/bin/env python3
"""Print the [[source]] block for a recipe from a sources TSV made by ubuntu-orig.py.

    source-block.py SOURCES.tsv PACKAGE [dest=DIR] [strip=N] [inner=PATH]

The SHA-256 comes from Ubuntu's GPG-signed Sources index. When Ubuntu ships the
upstream release unchanged, the upstream URL is listed first; core-build verifies
the checksum whichever URL answers, so a differing upstream file is simply skipped.
Repacked tarballs (+dfsg, wrapped, native) list only the archive URL.
"""

import sys

GNU = "https://ftp.gnu.org/gnu"
UPSTREAM = {
    "binutils": GNU + "/binutils/binutils-with-gold-{v}.tar.xz",
    "glibc": GNU + "/glibc/glibc-{v}.tar.xz",
    "m4": GNU + "/m4/m4-{v}.tar.xz",
    "sed": GNU + "/sed/sed-{v}.tar.xz",
    "grep": GNU + "/grep/grep-{v}.tar.xz",
    "bash": GNU + "/bash/bash-{v}.tar.xz",
    "coreutils": GNU + "/coreutils/coreutils-{v}.tar.xz",
    "diffutils": GNU + "/diffutils/diffutils-{v}.tar.xz",
    "gawk": GNU + "/gawk/gawk-{v}.tar.xz",
    "findutils": GNU + "/findutils/findutils-{v}.tar.xz",
    "gzip": GNU + "/gzip/gzip-{v}.tar.xz",
    "patch": GNU + "/patch/patch-{v}.tar.xz",
    "texinfo": GNU + "/texinfo/texinfo-{v}.tar.xz",
    "gettext": GNU + "/gettext/gettext-{v}.tar.xz",
    "readline": GNU + "/readline/readline-{v}.tar.gz",
    "mpfr4": GNU + "/mpfr/mpfr-{v}.tar.xz",
    "mpclib3": GNU + "/mpc/mpc-{v}.tar.gz",
    "libtool": GNU + "/libtool/libtool-{v}.tar.xz",
    "gdbm": GNU + "/gdbm/gdbm-{v}.tar.gz",
    "gperf": GNU + "/gperf/gperf-{v}.tar.gz",
    "inetutils": GNU + "/inetutils/inetutils-{v}.tar.gz",
    "autoconf": GNU + "/autoconf/autoconf-{v}.tar.xz",
    "automake": GNU + "/automake/automake-{v}.tar.xz",
    "groff": GNU + "/groff/groff-{v}.tar.gz",
    "grub2": GNU + "/grub/grub-{v}.tar.xz",
    "nano": GNU + "/nano/nano-{v}.tar.xz",
    "libxcrypt": "https://github.com/besser82/libxcrypt/releases/download/v{v}/libxcrypt-{v}.tar.xz",
    "xz-utils": "https://github.com/tukaani-project/xz/releases/download/v{v}/xz-{v}.tar.xz",
    "attr": "https://download.savannah.gnu.org/releases/attr/attr-{v}.tar.xz",
    "acl": "https://download.savannah.gnu.org/releases/acl/acl-{v}.tar.xz",
    "shadow": "https://github.com/shadow-maint/shadow/releases/download/{v}/shadow-{v}.tar.xz",
    "util-linux": "https://www.kernel.org/pub/linux/utils/util-linux/v{mm}/util-linux-{v}.tar.xz",
    "e2fsprogs": "https://www.kernel.org/pub/linux/kernel/people/tytso/e2fsprogs/v{v}/e2fsprogs-{v}.tar.gz",
    "kmod": "https://www.kernel.org/pub/linux/utils/kernel/kmod/kmod-{v}.tar.xz",
    "iproute2": "https://www.kernel.org/pub/linux/utils/net/iproute2/iproute2-{v}.tar.xz",
    "kbd": "https://www.kernel.org/pub/linux/utils/kbd/kbd-{v}.tar.gz",
    "openssl": "https://github.com/openssl/openssl/releases/download/openssl-{v}/openssl-{v}.tar.gz",
    "perl": "https://www.cpan.org/src/5.0/perl-{v}.tar.xz",
    "python3.14": "https://www.python.org/ftp/python/{v}/Python-{v}.tar.xz",
    "elfutils": "https://sourceware.org/elfutils/ftp/{v}/elfutils-{v}.tar.bz2",
    "libffi": "https://github.com/libffi/libffi/releases/download/v{v}/libffi-{v}.tar.gz",
    "pkgconf": "https://distfiles.ariadne.space/pkgconf/pkgconf-{v}.tar.xz",
    "dbus": "https://dbus.freedesktop.org/releases/dbus/dbus-{v}.tar.xz",
    "man-db": "https://download.savannah.gnu.org/releases/man-db/man-db-{v}.tar.xz",
    "procps": "https://sourceforge.net/projects/procps-ng/files/Production/procps-ng-{v}.tar.xz",
    "psmisc": "https://sourceforge.net/projects/psmisc/files/psmisc/psmisc-{v}.tar.xz",
    "libpipeline": "https://download.savannah.gnu.org/releases/libpipeline/libpipeline-{v}.tar.gz",
    "less": "https://www.greenwoodsoftware.com/less/less-{v}.tar.gz",
    "file": "https://astron.com/pub/file/file-{v}.tar.gz",
    "lz4": "https://github.com/lz4/lz4/releases/download/v{v}/lz4-{v}.tar.gz",
    "meson": "https://github.com/mesonbuild/meson/releases/download/{v}/meson-{v}.tar.gz",
    "manpages": "https://www.kernel.org/pub/linux/docs/man-pages/man-pages-{v}.tar.xz",
    "libseccomp": "https://github.com/seccomp/libseccomp/releases/download/v{v}/libseccomp-{v}.tar.gz",
}


def upstream_version(v: str) -> str:
    v = v.split(":", 1)[-1]
    return v.rsplit("-", 1)[0] if "-" in v else v


def main() -> None:
    tsv, pkg, *opts = sys.argv[1:]
    for line in open(tsv):
        name, version, sha, _size, url = line.rstrip("\n").split("\t")[:5]
        if name != pkg:
            continue
        v = upstream_version(version)
        urls = []
        if name in UPSTREAM and "dfsg" not in url:
            urls.append(UPSTREAM[name].format(v=v, mm=".".join(v.split(".")[:2])))
        urls.append(url.replace("http://", "https://"))
        print("[[source]]")
        print(f'sha256 = "{sha}"')
        print("urls = [" + ",\n        ".join(f'"{u}"' for u in urls) + "]")
        if len(urls) > 1:
            print(f'file = "{url.rsplit("/", 1)[1]}"')
        for o in opts:
            k, val = o.split("=", 1)
            print(f"{k} = {val}" if k == "strip" else f'{k} = "{val}"')
        return
    sys.exit(f"{pkg} is not in {tsv}")


if __name__ == "__main__":
    main()
