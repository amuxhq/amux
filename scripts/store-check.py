#!/usr/bin/env python3
"""Holds the store to its one rule: it only grows.

Every build opens every store, so nothing a released store has may go or
change: a table or column the newest release had must still be there with
the same type, and a column added since must be one an existing file can
take, nullable or with a default. The baseline is the newest release tag
behind HEAD, or a ref named on the command line. Releases before 0.9.0
predate the rule and are not checked against.
"""

import re
import sqlite3
import subprocess
import sys

SCHEMA_FILE = "crates/store/src/schema.rs"
FROZEN_SINCE = (0, 9, 0)


def schema_sql(source):
    """The SQL text of the `SCHEMA` constant in schema.rs."""
    found = re.search(r'SCHEMA: &str = r#"(.*?)"#', source, re.S)
    if found is None:
        raise SystemExit(f"{SCHEMA_FILE} holds no SCHEMA constant")
    return found.group(1)


def tables(sql):
    """Each table's columns: name to (type, not null, default)."""
    database = sqlite3.connect(":memory:")
    database.executescript(sql)
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
        new = tables(schema_sql(file.read()))
    old = tables(schema_sql(git("show", f"{ref}:{SCHEMA_FILE}")))
    found = losses(old, new)
    for loss in found:
        print(loss, file=sys.stderr)
    if found:
        print(f"{len(found)} break(s) against {ref}; see docs/JOURNAL_AND_STORE.md", file=sys.stderr)
        return 1
    print(f"the store only grew since {ref}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
