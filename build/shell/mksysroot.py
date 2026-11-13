#!/usr/bin/env python3
"""Build an armv7h sysroot for cross-compiling rtshell / anvil, entirely on the host (no qemu, no root).

Fetches the ALARM armv7h repo databases, resolves the dependency closure of the libraries in
DESKTOP-SHELL.md section 4, downloads the packages into build/shell/cache/ and unpacks them
(usr/ only) into build/shell/sysroot/. mesa-grate comes from the local [surface] repo.

Usage: ./mksysroot.py [--refresh]   (--refresh re-downloads the repo databases)
"""
import os, re, sys, tarfile, subprocess, urllib.request, shutil, glob

HERE = os.path.dirname(os.path.abspath(__file__))
CACHE = os.path.join(HERE, "cache")
SYSROOT = os.path.join(HERE, "sysroot")
LOCAL_REPO = os.path.normpath(os.path.join(HERE, "..", "repo", "surface"))
MIRROR = "http://mirror.archlinuxarm.org/armv7h"
REPOS = ["core", "extra", "alarm"]

ROOTS = """glibc linux-api-headers gcc-libs libinput systemd-libs libdrm libglvnd libxkbcommon
seatd wayland pixman libffi mtdev libevdev libdisplay-info fontconfig freetype2""".split()
# Pulled in by deps but not needed to link anything; skipping keeps the sysroot small.
SKIP = set("""filesystem bash tzdata iana-etc linux-api-headers-skip llvm-libs python perl
xkeyboard-config hwdata libwacom ca-certificates systemd""".split())
# Partial extractions: only these paths (crt*.o and libgcc.a for clang's link step).
PARTIAL = {"gcc": "usr/lib/gcc"}
# Supplied by mesa-grate (local) instead of the mirror's mesa.
LOCAL = {"mesa-grate": None}


def fetch(url, dest):
    if not os.path.exists(dest):
        tmp = dest + ".part"
        with urllib.request.urlopen(url) as r, open(tmp, "wb") as f:
            shutil.copyfileobj(r, f)
        os.rename(tmp, dest)
    return dest


def parse_db(path, repo, pkgs, provides):
    # ALARM uses the old layout: <pkg>/desc plus <pkg>/depends (DEPENDS/PROVIDES live there).
    entries = {}
    with tarfile.open(path) as t:
        for m in t:
            d, _, f = m.name.partition("/")
            if f not in ("desc", "depends"):
                continue
            fields, key = entries.setdefault(d, {}), None
            for line in t.extractfile(m).read().decode().splitlines():
                if line.startswith("%") and line.endswith("%"):
                    key = line[1:-1]; fields[key] = []
                elif line and key:
                    fields[key].append(line)
    for fields in entries.values():
        name = fields["NAME"][0]
        pkgs[name] = dict(repo=repo, file=fields["FILENAME"][0], version=fields["VERSION"][0],
                          depends=[re.split(r"[<>=]", d)[0] for d in fields.get("DEPENDS", [])])
        for p in fields.get("PROVIDES", []):
            provides.setdefault(re.split(r"[<>=]", p)[0], name)


def main():
    os.makedirs(CACHE, exist_ok=True)
    refresh = "--refresh" in sys.argv
    pkgs, provides = {}, {}
    for repo in REPOS:
        db = os.path.join(CACHE, repo + ".db")
        if refresh and os.path.exists(db):
            os.remove(db)
        fetch(f"{MIRROR}/{repo}/{repo}.db", db)
        parse_db(db, repo, pkgs, provides)

    mesa = sorted(glob.glob(os.path.join(LOCAL_REPO, "mesa-grate-*-armv7h.pkg.tar.*")),
                  key=lambda p: [int(x) if x.isdigit() else x for x in re.split(r"(\d+)", p)])[-1]
    # mesa-grate provides/replaces mesa, libgbm, libegl etc.; read its .PKGINFO for deps/provides.
    info = subprocess.run(["bsdtar", "-xOf", mesa, ".PKGINFO"], capture_output=True, text=True, check=True).stdout
    mdeps = [re.split(r"[<>=]", l.split(" = ", 1)[1])[0] for l in info.splitlines() if l.startswith("depend = ")]
    for l in info.splitlines():
        if l.startswith("provides = ") or l.startswith("conflict = "):
            provides[re.split(r"[<>=]", l.split(" = ", 1)[1])[0]] = "mesa-grate"
    provides["mesa"] = "mesa-grate"
    pkgs["mesa-grate"] = dict(repo="local", file=mesa, version="local", depends=mdeps)

    want, todo = set(), ROOTS + ["mesa-grate"]
    while todo:
        n = todo.pop()
        n = n if n in pkgs and n not in ("mesa",) else provides.get(n, n)
        if n in want or n in SKIP:
            continue
        if n not in pkgs:
            print("warning: unresolved dependency", n); continue
        want.add(n)
        todo += pkgs[n]["depends"]

    for n in PARTIAL:
        want.add(n)

    if os.path.exists(SYSROOT):
        shutil.rmtree(SYSROOT)
    os.makedirs(SYSROOT)
    for n in sorted(want):
        p = pkgs[n]
        path = p["file"] if p["repo"] == "local" else fetch(f"{MIRROR}/{p['repo']}/{p['file']}", os.path.join(CACHE, p["file"]))
        r = subprocess.run(["bsdtar", "-xf", path, "-C", SYSROOT, "--exclude", ".*", PARTIAL.get(n, "usr")], capture_output=True, text=True)
        if r.returncode and "Not found in archive" not in r.stderr:
            sys.exit(f"extract failed: {path}\n{r.stderr}")
    # ALARM merges /lib into /usr/lib; libc.so linker scripts use absolute /usr/lib paths (fine with --sysroot).
    for d in ("lib", "bin", "sbin"):
        os.symlink("usr/" + ("bin" if d == "sbin" else d), os.path.join(SYSROOT, d))
    with open(os.path.join(SYSROOT, "PACKAGES"), "w") as f:
        f.writelines(f"{n} {pkgs[n]['version']}\n" for n in sorted(want))
    print(f"{len(want)} packages -> {SYSROOT}")


if __name__ == "__main__":
    main()
