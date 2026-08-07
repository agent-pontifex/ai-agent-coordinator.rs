from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import pathlib
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / "repository-fleets" / "hypesiege-streempilot.json"
RENDERER_PATH = ROOT / "scripts" / "render_hypesiege_streempilot_audit_report.py"
SCRIPTS_DIR = ROOT / "scripts"
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))


def load_module(name: str, path: pathlib.Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


RENDERER = load_module("fleet_audit_renderer", RENDERER_PATH)


class AuditReportRendererTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
        cls.digest = hashlib.sha256(MANIFEST_PATH.read_bytes()).hexdigest()

    def test_report_is_deterministic_complete_and_static(self) -> None:
        first = RENDERER.render_report(self.manifest, digest=self.digest)
        self.assertEqual(first, RENDERER.render_report(self.manifest, digest=self.digest))
        self.assertEqual(first.count("<tr data-repository="), 32)
        self.assertEqual(first.count('data-kind="monorepo"'), 2)
        self.assertIn('data-repository-count="32"', first)
        self.assertIn('data-tracked-files="888"', first)
        self.assertIn('data-gitlinks="30"', first)
        self.assertIn(self.digest, first)
        self.assertIn("does not claim remote publication", first)
        self.assertNotIn("<script", first.casefold())
        self.assertNotIn("<img", first.casefold())
        self.assertIn("default-src &#x27;none&#x27;", first)
        self.assertEqual(first.count('rel="noopener noreferrer"'), 32)

    def test_report_escapes_manifest_text(self) -> None:
        manifest = copy.deepcopy(self.manifest)
        manifest["repositories"][0]["description"] = '<script>alert("x")</script>'
        report = RENDERER.render_report(manifest, digest=self.digest)
        self.assertNotIn("<script>", report)
        self.assertIn("&lt;script&gt;", report)

    def test_atomic_writer_refuses_symlink_destination(self) -> None:
        with tempfile.TemporaryDirectory(prefix="fleet-html-report-") as temp:
            root = pathlib.Path(temp)
            target = root / "target.html"
            target.write_text("safe", encoding="utf-8")
            link = root / "report.html"
            link.symlink_to(target)
            with self.assertRaisesRegex(Exception, "symlink"):
                RENDERER.write_text_atomic(link, "safe")


if __name__ == "__main__":
    unittest.main()
