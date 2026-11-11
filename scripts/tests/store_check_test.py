import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "store_check", ROOT / "scripts" / "store-check.py"
)
assert SPEC is not None and SPEC.loader is not None
check = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check)

OLD = "CREATE TABLE agents (id BLOB NOT NULL, name TEXT NOT NULL) STRICT;"


class StoreCheckTest(unittest.TestCase):
    def losses(self, new):
        return check.losses(check.tables([OLD]), check.tables([new]))

    def test_the_checked_in_migrations_load(self):
        source = (ROOT / check.SCHEMA_FILE).read_text(encoding="utf-8")
        entries = check.migrations(source)
        self.assertGreaterEqual(len(entries), 1)
        tables = check.tables(entries)
        self.assertIn("agents", tables)
        self.assertEqual(check.edits(entries, entries), [])
        self.assertEqual(check.losses(tables, tables), [])

    def test_every_entry_of_the_list_is_read(self):
        source = (
            "pub(crate) const MIGRATIONS: &[&str] = &[\n"
            '    // 1\n    r#"CREATE TABLE a (x);"#,\n'
            '    // 2\n    r#"ALTER TABLE a ADD COLUMN y;\n"#,\n'
            "];\n"
        )
        self.assertEqual(
            check.migrations(source),
            ["CREATE TABLE a (x);", "ALTER TABLE a ADD COLUMN y;\n"],
        )

    def test_appending_a_migration_is_allowed(self):
        new = [OLD, "ALTER TABLE agents ADD COLUMN note TEXT;"]
        self.assertEqual(check.edits([OLD], new), [])
        self.assertEqual(check.losses(check.tables([OLD]), check.tables(new)), [])

    def test_editing_a_released_migration_is_a_break(self):
        self.assertEqual(
            check.edits([OLD], [OLD + " -- tidied"]),
            ["migration 1 changed after its release; append a new migration instead"],
        )

    def test_removing_a_released_migration_is_a_break(self):
        self.assertEqual(check.edits([OLD, "CREATE TABLE b (x);"], [OLD]), ["migration 2 is gone"])

    def test_a_migration_that_drops_is_a_break(self):
        new = [OLD, "ALTER TABLE agents DROP COLUMN name;"]
        self.assertEqual(
            check.losses(check.tables([OLD]), check.tables(new)),
            ["column agents.name is gone"],
        )

    def test_a_migration_that_cannot_run_is_named(self):
        with self.assertRaises(SystemExit) as refused:
            check.tables([OLD, "ALTER TABLE missing ADD COLUMN x TEXT;"])
        self.assertIn("migration 2 does not run", str(refused.exception))

    # SQLite takes this on the empty tables the check builds and refuses it
    # on a store that holds rows, so the check has to name it itself.
    def test_a_required_column_added_by_a_migration_is_a_break(self):
        new = [OLD, "ALTER TABLE agents ADD COLUMN must TEXT NOT NULL;"]
        self.assertEqual(
            check.losses(check.tables([OLD]), check.tables(new)),
            [
                "column agents.must is new, NOT NULL and has no default, "
                "so an existing file cannot take it"
            ],
        )

    def test_growing_is_allowed(self):
        self.assertEqual(
            self.losses(
                "CREATE TABLE agents (id BLOB NOT NULL, name TEXT NOT NULL,"
                " note TEXT, seen INTEGER NOT NULL DEFAULT 0) STRICT;"
                "CREATE TABLE more (id BLOB) STRICT;"
            ),
            [],
        )

    def test_a_table_gone_is_a_break(self):
        self.assertEqual(
            self.losses("CREATE TABLE others (id BLOB) STRICT;"),
            ["table agents is gone"],
        )

    def test_a_column_gone_is_a_break(self):
        self.assertEqual(
            self.losses("CREATE TABLE agents (id BLOB NOT NULL) STRICT;"),
            ["column agents.name is gone"],
        )

    def test_a_column_retyped_is_a_break(self):
        self.assertEqual(
            self.losses(
                "CREATE TABLE agents (id BLOB NOT NULL, name INTEGER NOT NULL) STRICT;"
            ),
            ["column agents.name was TEXT, now INTEGER"],
        )

    def test_a_required_new_column_is_a_break(self):
        self.assertEqual(
            self.losses(
                "CREATE TABLE agents (id BLOB NOT NULL, name TEXT NOT NULL,"
                " must TEXT NOT NULL) STRICT;"
            ),
            [
                "column agents.must is new, NOT NULL and has no default, "
                "so an existing file cannot take it"
            ],
        )


if __name__ == "__main__":
    unittest.main()
