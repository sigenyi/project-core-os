#!/usr/bin/env python3
"""Locate pristine upstream source tarballs in Ubuntu's archive.

Debian/Ubuntu keep each project's upstream release tarball as `<pkg>_<ver>.orig.tar.*`
and record its SHA-256 in the (GPG-signed) Sources index. When upstream hosts are
unreachable from the build machine, recipes list that copy as a mirror. This tool
prints the URL and checksum for a source package, for writing recipes.

    os/tools/ubuntu-orig.py [--suite resolute] PACKAGE...
"""
import argparse, lzma, os, re, sys, urllib.request

ARCHIVE = "http://archive.ubuntu.com/ubuntu"

def index(suite, cache):
    pkgs = {}
    for comp in ("main", "universe"):
        path = os.path.join(cache, f"{suite}-{comp}-Sources.xz")
        if not os.path.exists(path):
            urllib.request.urlretrieve(f"{ARCHIVE}/dists/{suite}/{comp}/source/Sources.xz", path)
        for block in lzma.open(path).read().decode("utf-8", "replace").split("\n\n"):
            name = re.search(r"^Package: (.+)$", block, re.M)
            if not name or name.group(1) in pkgs:
                continue
            directory = re.search(r"^Directory: (.+)$", block, re.M).group(1)
            version = re.search(r"^Version: (.+)$", block, re.M).group(1)
            sha = block.split("Checksums-Sha256:")[1] if "Checksums-Sha256:" in block else ""
            files = re.findall(r"^ ([0-9a-f]{64}) (\d+) (\S+)$", sha, re.M)
            pkgs[name.group(1)] = (version, directory, files)
    return pkgs

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--suite", default="resolute")
    ap.add_argument("--cache", default=os.path.expanduser("~/.cache/core-os"))
    ap.add_argument("packages", nargs="+")
    a = ap.parse_args()
    os.makedirs(a.cache, exist_ok=True)
    pkgs = index(a.suite, a.cache)
    for p in a.packages:
        if p not in pkgs:
            print(f"# {p}: not found", file=sys.stderr)
            continue
        version, directory, files = pkgs[p]
        tarballs = [f for f in files if not f[2].endswith((".asc", ".dsc")) and (".orig" in f[2] or "debian" not in f[2])]
        for sha, size, name in tarballs:
            print(f"{p}\t{version}\t{sha}\t{size}\t{ARCHIVE}/{directory}/{name}")

if __name__ == "__main__":
    main()
