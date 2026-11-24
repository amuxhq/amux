"""The credentials release and QA recipes use, from a command the operator names.

`git config amux.credentials <command>` names it. Asked for one credential by
name (`<command> appstoreconnect/key-id`), the command prints its value and
exits 0, or exits non-zero when it holds none. Where the values are kept is
the operator's choice; docs/RELEASE.md lists the names.

A credential is read at the moment it is used and never printed. One a tool
needs as a file is written to a directory only this user can read, and
removed when the tool is done with it.
"""

from contextlib import contextmanager
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile

# Long enough for a person to answer whatever the command asks before it
# answers, such as an unlock prompt.
TIMEOUT = 120


class Missing(Exception):
    """A credential the command does not hold, or no command at all. The
    message names the credential and never contains a value."""


def command() -> list[str]:
    found = subprocess.run(["git", "config", "--get", "amux.credentials"],
                           capture_output=True, text=True, timeout=60)
    words = shlex.split(found.stdout.strip()) if found.returncode == 0 else []
    if not words:
        raise Missing("no credential command; set one with `git config "
                      "amux.credentials <command>` (docs/RELEASE.md)")
    return words


def read_bytes(name: str) -> bytes:
    try:
        found = subprocess.run([*command(), name], capture_output=True,
                               timeout=TIMEOUT)
    except FileNotFoundError as error:
        raise Missing(f"the credential command cannot run: {error.strerror}") from None
    if found.returncode != 0:
        # The command's own message says why (locked, declined, absent); it
        # is its job never to put a value there.
        reason = found.stderr.decode(errors="replace").strip().splitlines()
        raise Missing(f"the credential command has no {name}"
                      + (f" ({reason[0]})" if reason else ""))
    return found.stdout


def read(name: str) -> str:
    """A text credential, without the newline a command ends it with."""
    return read_bytes(name).decode().rstrip("\n")


def holds(name: str) -> bool:
    """Whether the credential can be read. It is read to find out, so this
    asks for whatever a read asks for; the value is dropped."""
    try:
        read_bytes(name)
    except Missing:
        return False
    return True


@contextmanager
def file(name: str, filename: str):
    """The credential as a file named `filename`, for as long as the `with`
    block lasts."""
    directory = Path(tempfile.mkdtemp(prefix="amux-credential-"))
    try:
        path = directory / filename
        path.touch(mode=0o600)
        path.write_bytes(read_bytes(name))
        yield path
    finally:
        shutil.rmtree(directory, ignore_errors=True)
