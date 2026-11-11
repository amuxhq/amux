#!/usr/bin/env python3
"""Holds the store's migrations to their two rules.

Every build opens every store, and a store is changed only by the ordered
migrations in schema.rs, so:

- A migration the newest release shipped is still there, unchanged, in the
  same place. A store that ran it is never given it again, so an edit would
  reach fresh stores only.
- The migrations added since only add. A table or column the release had
  is still there with the same type, and a new column is one an existing
  file can take, nullable or with a default. A build from before them, which
  a rollback starts, then still reads and writes the file.

The baseline is the newest release tag behind HEAD, or a ref named on the
command line. Releases before 0.9.0 predate the rules and are not checked
against.
"""

import re
import sqlite3
import subprocess
import sys

SCHEMA_FILE = "crates/store/src/schema.rs"
FROZEN_SINCE = (0, 9, 0)


def migrations(source):
    """The SQL text of each entry of the `MIGRATIONS` list in schema.rs.

    Every entry is a raw string, r#"..."#. Anything else in the list is
    refused rather than skipped: an entry this check cannot read is one it
    would pass without having looked at.
    """
    found = re.search(r"MIGRATIONS: &\[&str\] = &\[(.*?)\n\];", source, re.S)
    if found is None:
        raise SystemExit(f"{SCHEMA_FILE} holds no MIGRATIONS list")
    body = found.group(1)
    entry = re.compile(r'(?<![\w#])r#"(.*?)"#(?!#)', re.S)
    rest = re.sub(r"//[^\n]*", "", entry.sub("", body))
    rest = rest.replace(",", "").strip()
    if rest:
        raise SystemExit(
            f'{SCHEMA_FILE}: every MIGRATIONS entry must be a raw string, r#"..."#; '
            f"this check cannot read {rest.splitlines()[0].strip()[:60]!r}"
        )
    return entry.findall(body)


def edits(old, new):
    """Each released migration that `new` no longer holds as released."""
    found = []
    for index, text in enumerate(old):
        if index >= len(new):
            found.append(f"migration {index + 1} is gone")
        elif new[index] != text:
            found.append(
                f"migration {index + 1} changed after its release; "
                "append a new migration instead"
            )
    return found


def tables(entries):
    """Each table's columns, name to (type, not null, default), once the
    migrations have run in order."""
    database = sqlite3.connect(":memory:")
    for index, sql in enumerate(entries):
        try:
            database.executescript(sql)
        except sqlite3.Error as error:
            raise SystemExit(f"migration {index + 1} does not run: {error}")
    names = database.execute(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'"
    ).fetchall()
    return {
        table: {
            column: (kind, bool(required), default)
            for _, column, kind, required, default, _ in database.execute(
                f"SELECT * FROM pragma_table_info('{table}')"
            )
        }
        for (table,) in names
    }


def losses(old, new):
    """What `new` broke of `old`, each in words."""
    found = []
    for table, columns in old.items():
        if table not in new:
            found.append(f"table {table} is gone")
            continue
        for column, (kind, _, _) in columns.items():
            if column not in new[table]:
                found.append(f"column {table}.{column} is gone")
            elif new[table][column][0] != kind:
                found.append(
                    f"column {table}.{column} was {kind}, now {new[table][column][0]}"
                )
        for column, (_, required, default) in new[table].items():
            if column not in columns and required and default is None:
                found.append(
                    f"column {table}.{column} is new, NOT NULL and has no default, "
                    "so an existing file cannot take it"
                )
    return found


def git(*args):
    return subprocess.run(
        ["git", *args], check=True, capture_output=True, text=True
    ).stdout


def baseline(args):
    """The ref to compare with, or None when no release is under the rule."""
    if args:
        return args[0]
    tag = git("describe", "--tags", "--abbrev=0", "--match", "v[0-9]*").strip()
    version = tuple(int(part) for part in tag.removeprefix("v").split("."))
    if version < FROZEN_SINCE:
        print(f"the newest release, {tag}, predates the rule from 0.9.0; nothing to check against")
        return None
    return tag


def main(args):
    ref = baseline(args)
    if ref is None:
        return 0
    with open(SCHEMA_FILE, encoding="utf-8") as file:
        new = migrations(file.read())
    old = migrations(git("show", f"{ref}:{SCHEMA_FILE}"))
    found = edits(old, new) + losses(tables(old), tables(new))
    for loss in found:
        print(loss, file=sys.stderr)
    if found:
        print(f"{len(found)} break(s) against {ref}; see docs/JOURNAL_AND_STORE.md", file=sys.stderr)
        return 1
    print(f"the store's migrations only added since {ref}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
