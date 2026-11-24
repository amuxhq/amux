#!/usr/bin/env python3
"""Write what the repository checks read from outside the checkout's files.

A Buck2 action sees only the files its target declares, and no git history.
Three checks need more than that, so this writes it into build/checks/inputs/
(not committed), where the check targets in the root BUCK file declare it:

- `tracked`: every file the checkout tracks, or would (untracked but not
  ignored), one path a line. The docs check resolves links against it, so
  it reruns when a file is added or removed, not whenever one changes.
- `store-baseline/`: the newest release tag behind HEAD (`ref`) and that
  release's crates/store/src/schema.rs, which the store check compares.
- `proto-baseline/`: the same tag and that release's proto/ tree, which the
  protocol check compares.

A file is written only when its content changes. The `just` recipes that run
the check targets run this first (`just checks`, `just test`, `just
premerge`); `scripts/buck2 test` alone uses whatever was last written.

    inputs.py
"""

import io
import os
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "build" / "checks" / "inputs"
SCHEMA = "crates/store/src/schema.rs"


def git(*args: str, binary: bool = False):
    result = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, check=False)
    if result.returncode != 0:
        return None
    return result.stdout if binary else result.stdout.decode()


def write(path: Path, content: bytes) -> None:
    if path.is_file() and path.read_bytes() == content:
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_bytes(content)
    os.replace(temporary, path)


def tracked() -> bytes:
    listing = git("ls-files", "-z", "--cached", "--others", "--exclude-standard", binary=True)
    if listing is None:
        sys.exit("check inputs: git ls-files failed; the checks need a git checkout")
    paths = sorted({path for path in listing.decode().split("\0") if path and (ROOT / path).is_file()})
    return "".join(f"{path}\n" for path in paths).encode()


def release() -> str:
    """The newest release tag behind HEAD, or "" in a clone without tags."""
    return (git("describe", "--tags", "--abbrev=0", "--match", "v[0-9]*") or "").strip()


def store_baseline(tag: str) -> None:
    out = OUT / "store-baseline"
    write(out / "ref", f"{tag}\n".encode())
    schema = git("show", f"{tag}:{SCHEMA}", binary=True) if tag else None
    if schema is None:
        (out / "schema.rs").unlink(missing_ok=True)
    else:
        write(out / "schema.rs", schema)


def proto_baseline(tag: str) -> None:
    out = OUT / "proto-baseline"
    ref = out / "ref"
    if ref.is_file() and ref.read_text() == f"{tag}\n" and ((out / "proto").is_dir() or not tag):
        return
    shutil.rmtree(out / "proto", ignore_errors=True)
    archive = git("archive", "--format=tar", tag, "proto", binary=True) if tag else None
    if archive is not None:
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            tar.extractall(out, filter="data")
    write(ref, f"{tag}\n".encode())


def main() -> None:
    write(OUT / "tracked", tracked())
    tag = release()
    store_baseline(tag)
    proto_baseline(tag)


if __name__ == "__main__":
    main()
