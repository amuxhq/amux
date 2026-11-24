#!/usr/bin/env python3
"""The JavaScript's Buck2 actions (js/BUCK, build/js.bzl), from the checkout's root.

    js.py install OUT
        `npm ci` from js/package-lock.json, packing js/node_modules into the
        tar OUT. npm checks every package against the lockfile's hash, so
        what it installs is a function of the lockfile (and the platform,
        whose binaries esbuild and Biome bring).
    js.py run MODULES [--collect PATH OUT] -- COMMAND...
        Unpack MODULES into js/node_modules and run COMMAND in js/; with
        --collect, then copy js/PATH (what the command built) to OUT.

On Linux the node, npm and npx on PATH are the Linux toolchain's; on a Mac
$NODE_BIN names the pinned Node's bin directory, which goes first on PATH.
"""

import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

ROOT = Path.cwd()
JS = ROOT / "js"
MANIFESTS = ["package.json", "package-lock.json", ".npmrc", "extension/package.json", "page-review/package.json"]


# Paths an action is given relative to the checkout's root, which the
# command, run in js/, gets as absolute ones.
PATHS = ["AMUX_BROWSER_CORE", "AMUX_CHROMIUM", "AMUX_EXTENSION", "NODE_BIN", "TESTNET_BINARIES"]


def environment() -> dict:
    env = dict(os.environ)
    for name in PATHS:
        if env.get(name):
            env[name] = str((ROOT / env[name]).absolute())
    # The Chromium a Linux worker's test drives: the Linux toolchain's
    # browser output, whose store path the file names.
    if env.get("AMUX_CHROMIUM_ROOT"):
        root = (ROOT / env.pop("AMUX_CHROMIUM_ROOT")).read_text().strip()
        env["AMUX_CHROMIUM"] = f"{root}/bin/chromium"
        env["FONTCONFIG_FILE"] = f"{root}/etc/fonts.conf"
    if env.get("NODE_BIN"):
        env["PATH"] = f"{env['NODE_BIN']}:{env['PATH']}"
    # npm keeps its cache under HOME, which an action's worker may not give
    # it, and never asks the registry about updates or funding here.
    scratch = Path(tempfile.mkdtemp(prefix="js-"))
    env.setdefault("HOME", str(scratch / "home"))
    env["npm_config_cache"] = str(scratch / "npm-cache")
    env["npm_config_update_notifier"] = "false"
    # Their logs are read as text.
    env["NO_COLOR"] = "1"
    env.pop("FORCE_COLOR", None)
    return env


def normalized(info: tarfile.TarInfo) -> tarfile.TarInfo:
    # The same install packs to the same bytes, whoever ran it and when.
    info.mtime = 0
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    return info


def install(out: Path) -> int:
    work = Path(tempfile.mkdtemp(prefix="npm-ci-"))
    for name in MANIFESTS:
        if (JS / name).exists():
            (work / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(JS / name, work / name)
    status = subprocess.run(
        ["npm", "ci", "--no-audit", "--no-fund", "--loglevel=error"], cwd=work, env=environment()
    ).returncode
    if status:
        return status
    modules = work / "node_modules"
    with tarfile.open(out, "w") as tar:
        for path in sorted(modules.rglob("*")):
            tar.add(path, arcname=path.relative_to(work).as_posix(), recursive=False, filter=normalized)
    return 0


def run(modules: Path, collect: tuple[str, Path] | None, command: list[str]) -> int:
    with tarfile.open(modules) as tar:
        tar.extractall(JS, filter="data")
    status = subprocess.run(command, cwd=JS, env=environment()).returncode
    if status or collect is None:
        return status
    built, out = collect
    shutil.copytree(JS / built, out, symlinks=True)
    return 0


def main(args: list[str]) -> int:
    if args[:1] == ["install"] and len(args) == 2:
        return install(Path(args[1]).resolve())
    if args[:1] == ["run"] and "--" in args:
        split = args.index("--")
        options, command = args[1:split], args[split + 1 :]
        modules = Path(options.pop(0)).resolve()
        collect = None
        if options[:1] == ["--collect"]:
            collect = (options[1], Path(options[2]).resolve())
        return run(modules, collect, command)
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
