#!/usr/bin/env python3
"""Regression checks for repository-owned GHCR publication."""
from __future__ import annotations

import unittest
from pathlib import Path

WORKFLOW = Path(__file__).resolve().parents[1] / ".github/workflows/oci.yml"


class OciWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.text = WORKFLOW.read_text(encoding="utf-8")
        cls.lowered = cls.text.lower()

    def test_image_tracks_current_repository_owner(self) -> None:
        self.assertIn(
            "IMAGE_NAME: ghcr.io/${{ github.repository_owner }}/ai-agent-coordinator",
            self.text,
        )

    def test_former_owner_is_not_a_registry_destination(self) -> None:
        self.assertNotIn("ghcr.io/oresoftware/", self.lowered)
        self.assertNotIn("ghcr.io/fiducia-cloud/", self.lowered)

    def test_publish_is_main_push_only(self) -> None:
        publish = self.text.split("\n  publish:\n", 1)[1]
        self.assertIn(
            "if: github.event_name == 'push' && github.ref == 'refs/heads/main'",
            publish,
        )

    def test_publish_has_package_write_permission(self) -> None:
        publish = self.text.split("\n  publish:\n", 1)[1]
        permissions = publish.split("\n    steps:\n", 1)[0]
        self.assertIn("packages: write", permissions)

    def test_publish_retains_sbom_provenance_and_immutable_tag(self) -> None:
        publish = self.text.split("\n  publish:\n", 1)[1]
        self.assertIn("--provenance=mode=max", publish)
        self.assertIn("--sbom=true", publish)
        self.assertIn('--tag "${IMAGE_NAME}:sha-${GITHUB_SHA}"', publish)

    def test_oracle_retriggers_and_runs_in_verify_job(self) -> None:
        self.assertIn("- 'scripts/test-oci-workflow.py'", self.text)
        verify = self.text.split("\n  verify:\n", 1)[1].split("\n  publish:\n", 1)[0]
        self.assertIn("python3 scripts/test-oci-workflow.py", verify)


if __name__ == "__main__":
    unittest.main(verbosity=2)
