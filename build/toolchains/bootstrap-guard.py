"""Run one of the prelude's bootstrap Python scripts, and fail a compile that
the prelude would report as a success although rustc failed.

    python3 bootstrap-guard.py SCRIPT [ARGS...]

Every prelude tool written in Python runs through this (the bootstrap
toolchain's interpreter, python_bootstrap.bzl), and all but one run as they
would under the interpreter alone. The one is rustc_action.py in the
prelude's failure-filter mode, which the Rust rules use for a library's
`[check]` output and its `[diag.*]` and `[clippy.*]` subtargets: when rustc
fails with an error, the script still exits 0, recording rustc's status in a
build-status file, and a later action fails the build from that file. A
remote worker stores any action that exits 0 as a success, so the failure is
kept in the action cache and replayed until the compile's inputs change. A
code error replays as itself, but a failure of the machine does too: on
2026-11-24 the build host's disk filled, three such compiles could not write
their outputs, and every CI run replayed "No space left on device" from the
cache until their crates were edited.

So when the build-status file records a failure, this exits non-zero
instead: the compile fails where it ran, with rustc's errors in its output,
and nothing stores it. Nothing in amux reads a failing crate's diagnostics
from those subtargets, which is all the failure filter is for.
"""

from __future__ import annotations

import argparse
import json
import os
import runpy
import sys

RUSTC_ACTION = "rustc_action.py"


def build_status(args: list[str]) -> str | None:
    """Where rustc_action.py writes its build status, if it was given a
    `--failure-filter`: read as rustc_action.py reads its own arguments,
    from an argument file named with `@` and before `--program-args`."""
    if "--program-args" in args:
        args = args[: args.index("--program-args")]
    parser = argparse.ArgumentParser(fromfile_prefix_chars="@", add_help=False, allow_abbrev=False)
    parser.add_argument("--failure-filter")
    try:
        known, _ = parser.parse_known_args(args)
    except SystemExit:
        # rustc_action.py parses the same arguments and reports the error.
        return None
    return known.failure_filter


def failed(status_path: str) -> str | None:
    """Why the compile failed, when its build status says it did."""
    try:
        with open(status_path) as file:
            status = json.load(file)["status"]
    except (OSError, ValueError, KeyError, TypeError) as error:
        return f"its build status ({status_path}) can't be read: {error}"
    if status != 0:
        return f"rustc exited with status {status}"
    return None


def main() -> None:
    script, args = sys.argv[1], sys.argv[2:]
    status_path = build_status(args) if os.path.basename(script) == RUSTC_ACTION else None
    # As `python3 SCRIPT ARGS...` would run it.
    sys.argv = [script, *args]
    sys.path[0] = os.path.dirname(os.path.abspath(script))
    code: object = 0
    try:
        runpy.run_path(script, run_name="__main__")
    except SystemExit as exit:
        code = exit.code
    if code in (None, 0) and status_path is not None:
        why = failed(status_path)
        if why is not None:
            print(
                f"bootstrap-guard: the compile failed ({why}); failing the action rather than"
                " letting the cache keep the failure as a success (build/toolchains/bootstrap-guard.py)",
                file=sys.stderr,
                flush=True,
            )
            code = 1
    sys.exit(code)


if __name__ == "__main__":
    main()
