#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT_PATH = Path(__file__).with_name("validate_schema.py")
SPEC = importlib.util.spec_from_file_location("memebank_validate_schema", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)

ROOT = Path(__file__).resolve().parents[1]


class SchemaValidationTests(unittest.TestCase):
    def copy_root(self) -> Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        destination = Path(directory.name) / "database"
        shutil.copytree(ROOT, destination)
        return destination

    def replace_once(self, path: Path, old: str, new: str) -> None:
        text = path.read_text(encoding="utf-8")
        self.assertIn(old, text)
        path.write_text(text.replace(old, new, 1), encoding="utf-8")

    def test_valid_contract_is_deterministic(self) -> None:
        first = MODULE.validate_root(ROOT)
        second = MODULE.validate_root(ROOT)
        self.assertEqual(first, second)
        self.assertEqual(first["status"], "valid")
        self.assertEqual(first["table_count"], 27)
        self.assertGreaterEqual(first["policy_count"], 40)
        self.assertEqual(first["vector_dimensions"], [384, 768, 1024])
        self.assertTrue(first["real_database_execution_required"])

    def test_unordered_schema_file_is_rejected(self) -> None:
        root = self.copy_root()
        (root / "schema" / "080_unreviewed.sql").write_text(
            "BEGIN;\nSELECT 1;\nCOMMIT;\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(MODULE.SchemaValidationError, "missing from order.txt"):
            MODULE.validate_root(root)

    def test_missing_forced_rls_is_rejected(self) -> None:
        root = self.copy_root()
        path = root / "schema" / "070_rls_and_grants.sql"
        self.replace_once(
            path,
            "ALTER TABLE memebank.assets FORCE ROW LEVEL SECURITY;\n",
            "",
        )
        with self.assertRaisesRegex(MODULE.SchemaValidationError, "force RLS"):
            MODULE.validate_root(root)

    def test_dimensionless_vector_is_rejected(self) -> None:
        root = self.copy_root()
        path = root / "schema" / "040_enrichment_and_search.sql"
        self.replace_once(path, "embedding vector(768) NOT NULL", "embedding vector NOT NULL")
        with self.assertRaisesRegex(MODULE.SchemaValidationError, "vector\(768\)"):
            MODULE.validate_root(root)

    def test_worker_bypassrls_is_rejected(self) -> None:
        root = self.copy_root()
        path = root / "bootstrap" / "roles.sql"
        self.replace_once(
            path,
            "ALTER ROLE mb_worker NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS;",
            "ALTER ROLE mb_worker NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT BYPASSRLS;",
        )
        with self.assertRaisesRegex(MODULE.SchemaValidationError, "bootstrap is not fail closed"):
            MODULE.validate_root(root)

    def test_secret_bearing_schema_field_is_rejected(self) -> None:
        root = self.copy_root()
        path = root / "schema" / "030_storage.sql"
        self.replace_once(
            path,
            "    secret_ref text CHECK (",
            "    access_token text,\n    secret_ref text CHECK (",
        )
        with self.assertRaisesRegex(MODULE.SchemaValidationError, "secret-bearing field"):
            MODULE.validate_root(root)

    def test_search_weight_drift_is_rejected(self) -> None:
        root = self.copy_root()
        path = root / "schema" / "040_enrichment_and_search.sql"
        self.replace_once(
            path,
            "coalesce(selected_caption_text, '')), 'C')",
            "coalesce(selected_caption_text, '')), 'A')",
        )
        with self.assertRaisesRegex(MODULE.SchemaValidationError, "selected_caption_text"):
            MODULE.validate_root(root)

    def test_unscoped_worker_policy_is_rejected(self) -> None:
        root = self.copy_root()
        path = root / "schema" / "070_rls_and_grants.sql"
        self.replace_once(
            path,
            "USING (memebank_private.worker_has_library_access(library_id))",
            "USING (true)",
        )
        with self.assertRaisesRegex(MODULE.SchemaValidationError, "not library scoped"):
            MODULE.validate_root(root)


if __name__ == "__main__":
    unittest.main()
