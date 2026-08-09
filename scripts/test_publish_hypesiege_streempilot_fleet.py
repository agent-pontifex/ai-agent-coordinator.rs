from __future__ import annotations

import importlib.util
import json
import pathlib
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / "repository-fleets" / "hypesiege-streempilot.json"
PUBLISHER_PATH = ROOT / "scripts" / "publish_hypesiege_streempilot_fleet.py"
RECONSTRUCTOR_PATH = ROOT / "scripts" / "reconstruct_hypesiege_streempilot_fleet.py"
PAYLOAD_DIR = ROOT / "repository-fleets" / "hypesiege-streempilot"


def load_module(name: str, path: pathlib.Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


PUBLISHER = load_module("fleet_publisher", PUBLISHER_PATH)
RECONSTRUCTOR = load_module("fleet_reconstructor", RECONSTRUCTOR_PATH)


class FleetManifestTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
        cls.repositories = cls.manifest["repositories"]

    def test_fleet_shape_counts_and_generator_identity(self) -> None:
        self.assertEqual(self.manifest["schema_version"], 2)
        self.assertEqual(
            self.manifest["generator_sha256"],
            PUBLISHER.EXPECTED_GENERATOR_SHA256,
        )
        self.assertEqual(self.manifest["repository_count"], 32)
        self.assertEqual(self.manifest["total_tracked_files"], 888)
        self.assertEqual(self.manifest["total_gitlinks"], 30)
        self.assertEqual(
            self.manifest["organizations"],
            {"hypesiege": 15, "streempilot": 17},
        )
        self.assertEqual(len(self.repositories), 32)
        self.assertEqual(
            len({record["full_name"] for record in self.repositories}),
            32,
        )
        self.assertEqual(sum(record["files"] for record in self.repositories), 888)
        self.assertEqual(sum(record["gitlinks"] for record in self.repositories), 30)

    def test_records_are_explicit_and_deterministically_sealed(self) -> None:
        commit_pattern = re.compile(r"^[0-9a-f]{40}$")
        for record in self.repositories:
            with self.subTest(repository=record["full_name"]):
                expected_full_name = f"{record['org']}/{record['name']}"
                self.assertEqual(record["full_name"], expected_full_name)
                self.assertIn(record["org"], PUBLISHER.ALLOWED_ORGS)
                self.assertEqual(record["default_branch"], "main")
                self.assertIn(record["visibility"], {"public", "private"})
                self.assertGreater(record["files"], 0)
                self.assertRegex(record["commit"], commit_pattern)
                self.assertEqual(
                    record["remote"],
                    f"https://github.com/{expected_full_name}.git",
                )
                self.assertTrue(record["description"].strip())
                expected_gitlinks = 0
                if record["kind"] == "monorepo":
                    expected_gitlinks = 14 if record["org"] == "hypesiege" else 16
                self.assertEqual(record["gitlinks"], expected_gitlinks)

    def test_monorepositories_publish_last(self) -> None:
        for org in sorted(PUBLISHER.ALLOWED_ORGS):
            records = [record for record in self.repositories if record["org"] == org]
            self.assertEqual(records[-1]["kind"], "monorepo")
            self.assertEqual(records[-1]["name"], f"{org}-monorepo")
            self.assertTrue(
                all(record["kind"] != "monorepo" for record in records[:-1])
            )

    def test_payload_decodes_to_the_reviewed_generator(self) -> None:
        source = RECONSTRUCTOR.decode_generator(PAYLOAD_DIR)
        self.assertIn("def initialize_repo", source)
        self.assertIn("hypesiege-monorepo", source)
        self.assertIn("streempilot-monorepo", source)

    def test_loader_selector_and_current_commits(self) -> None:
        manifest = PUBLISHER.load_manifest(MANIFEST_PATH)
        selected = PUBLISHER.select_record(
            manifest,
            "hypesiege/hypesiege-api-server.rs",
        )
        self.assertEqual(
            selected["commit"],
            "8b1b00ecc3ef421d03db9f9231a703bfbec6df9f",
        )
        monorepo = PUBLISHER.select_record(
            manifest,
            "streempilot/streempilot-monorepo",
        )
        self.assertEqual(monorepo["gitlinks"], 16)
        self.assertEqual(
            monorepo["commit"],
            "e1527280d6a21f49fcdc08dc9efc00bedfbf718c",
        )
        with self.assertRaises(PUBLISHER.PublicationError):
            PUBLISHER.select_record(manifest, "other/example")

    def test_plan_mode_is_network_free(self) -> None:
        completed = subprocess.run(
            [
                sys.executable,
                str(PUBLISHER_PATH),
                "--manifest",
                str(MANIFEST_PATH),
                "--repository",
                "streempilot/streempilot-monorepo",
            ],
            cwd=ROOT,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        plan = json.loads(completed.stdout)
        self.assertEqual(plan["mode"], "plan")
        self.assertEqual(plan["gitlinks"], 16)
        self.assertEqual(
            plan["commit"],
            "e1527280d6a21f49fcdc08dc9efc00bedfbf718c",
        )

    def test_execute_requires_exact_confirmation_before_credentials(self) -> None:
        completed = subprocess.run(
            [
                sys.executable,
                str(PUBLISHER_PATH),
                "--manifest",
                str(MANIFEST_PATH),
                "--repository",
                "hypesiege/hypesiege-monorepo",
                "--execute",
                "--confirm-repository",
                "hypesiege/not-the-monorepo",
            ],
            cwd=ROOT,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn(
            "--confirm-repository must exactly equal",
            completed.stderr,
        )

    def test_preflight_accepts_the_reconstructed_monorepo(self) -> None:
        with tempfile.TemporaryDirectory(prefix="fleet-preflight-") as temp:
            target = pathlib.Path(temp) / "fleet"
            manifest = RECONSTRUCTOR.reconstruct(PAYLOAD_DIR, target)
            self.assertEqual(manifest, self.manifest)
            record = next(
                item
                for item in manifest["repositories"]
                if item["full_name"] == "hypesiege/hypesiege-monorepo"
            )
            repository = PUBLISHER.preflight_source(manifest, record, target)
            self.assertEqual(repository.name, "hypesiege-monorepo")
            self.assertEqual(len(PUBLISHER.staged_gitlinks(repository)), 14)


if __name__ == "__main__":
    unittest.main()
