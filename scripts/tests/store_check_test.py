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
        return check.losses(check.tables(OLD), check.tables(new))

    def test_the_checked_in_schema_loads(self):
        source = (ROOT / check.SCHEMA_FILE).read_text(encoding="utf-8")
        tables = check.tables(check.schema_sql(source))
        self.assertIn("agents", tables)
        self.assertEqual(check.losses(tables, tables), [])

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
