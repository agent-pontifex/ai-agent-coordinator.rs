from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

import audit_org_context_rollout as rollout_audit  # noqa: E402
import render_org_project_context as context_renderer  # noqa: E402
import render_org_repository_relationships as relationship_renderer  # noqa: E402
import validate_org_project_registry as registry_module  # noqa: E402

IMMUTABLE_TEST_REF = "3" * 40


class OrgRepositoryRelationshipTest(unittest.TestCase):
    def setUp(self) -> None:
        self.registry = registry_module.load_registry(
            ROOT / "config" / "org-project-registry.yaml"
        )
        self.inventory = rollout_audit.load_inventory(
            ROOT / "config" / "org-context-rollout-inventory.json"
        )

    def test_checked_in_inventory_is_exact_and_public(self) -> None:
        rollout_audit.validate_inventory(self.inventory)
        repositories = self.inventory["repositories"]
        self.assertEqual(len(repositories), 3)
        self.assertEqual(
            {repository["full_name"] for repository in repositories},
            {
                "fiducia-cloud/.github",
                "sonus-auris/.github",
                "shared-auth/.github",
            },
        )
        self.assertTrue(
            all(repository["visibility"] == "public" for repository in repositories)
        )

    def test_rollout_audit_distinguishes_generated_created_and_excluded(self) -> None:
        report = rollout_audit.build_rollout_audit(
            self.registry, self.inventory, IMMUTABLE_TEST_REF
        )
        self.assertEqual(
            report["summary"],
            {
                "eligible_organizations": 30,
                "existing_public": 3,
                "missing": 27,
                "visibility_mismatch": 0,
                "unsupported_account_type": 1,
                "excluded_unmapped": 7,
                "complete": False,
            },
        )
        self.assertEqual(report["registry"]["mapped_owners"], 31)
        self.assertFalse(
            report["bootstrap_contract"]["live_creation_authorized_by_this_artifact"]
        )

    def test_missing_organization_request_is_dry_run_and_exact(self) -> None:
        report = rollout_audit.build_rollout_audit(
            self.registry, self.inventory, IMMUTABLE_TEST_REF
        )
        owner = next(
            item for item in report["owners"] if item["github"]["login"] == "3FA-app"
        )
        self.assertEqual(owner["status"], "missing")
        request = owner["bootstrap_dry_run"]
        self.assertEqual(request["method"], "POST")
        self.assertEqual(request["path"], "/v1/github/repositories")
        self.assertEqual(
            request["body"],
            {
                "organization": "3FA-app",
                "name": ".github",
                "visibility": "public",
                "initialization": "readme",
                "description": (
                    "Public organization-wide GitHub and Linear context for 3FA-app"
                ),
                "dry_run": True,
            },
        )
        self.assertNotIn("confirm_repository", request["body"])

    def test_user_account_is_never_sent_to_organization_creation_endpoint(self) -> None:
        report = rollout_audit.build_rollout_audit(
            self.registry, self.inventory, IMMUTABLE_TEST_REF
        )
        owner = next(
            item
            for item in report["owners"]
            if item["github"]["login"] == "ORESoftware"
        )
        self.assertEqual(owner["github"]["account_type"], "User")
        self.assertEqual(owner["status"], "unsupported_account_type")
        self.assertIsNone(owner["bootstrap_dry_run"])
        self.assertNotIn(
            "ORESoftware", report["bootstrap_contract"]["missing_owner_allowlist"]
        )

    def test_unmapped_installed_organizations_remain_fail_closed(self) -> None:
        report = rollout_audit.build_rollout_audit(
            self.registry, self.inventory, IMMUTABLE_TEST_REF
        )
        excluded = report["excluded_unmapped_organizations"]
        self.assertEqual(len(excluded), 7)
        self.assertTrue(all(not item["eligible_for_bootstrap"] for item in excluded))
        excluded_logins = {item["github"]["login"] for item in excluded}
        allowlist = set(report["bootstrap_contract"]["missing_owner_allowlist"])
        self.assertTrue(excluded_logins.isdisjoint(allowlist))

    def test_shared_auth_relationships_carry_reviewed_runtime_route(self) -> None:
        manifest = relationship_renderer.build_relationship_manifest(
            self.registry, "shared-auth", IMMUTABLE_TEST_REF
        )
        selection = manifest["repository_selection"]
        self.assertEqual(
            selection["default_repository"],
            "shared-auth/shared-auth-mcp-server.rs",
        )
        self.assertIn(
            "shared-auth/shared-auth-mcp-server.rs",
            selection["runtime_allowlist"],
        )
        kinds = {edge["kind"] for edge in manifest["relationships"]}
        self.assertIn("defaults_runtime_routing_to", kinds)
        self.assertIn("permits_runtime_routing_to", kinds)

    def test_owner_without_route_does_not_gain_an_invented_default(self) -> None:
        manifest = relationship_renderer.build_relationship_manifest(
            self.registry, "sonus-auris", IMMUTABLE_TEST_REF
        )
        self.assertIsNone(manifest["repository_selection"]["default_repository"])
        self.assertEqual(manifest["repository_selection"]["runtime_allowlist"], [])
        self.assertNotIn(
            "defaults_runtime_routing_to",
            {edge["kind"] for edge in manifest["relationships"]},
        )

    def test_repository_overrides_are_declared_without_becoming_runtime_routes(self) -> None:
        manifest = relationship_renderer.build_relationship_manifest(
            self.registry, "ORESoftware", IMMUTABLE_TEST_REF
        )
        overrides = manifest["repository_selection"]["linear_project_overrides"]
        self.assertEqual(len(overrides), 5)
        self.assertEqual(
            {override["repository"] for override in overrides},
            {
                "ORESoftware/ai-agent-bridge.rs",
                "ORESoftware/ai-agent-coordinator.rs",
                "ORESoftware/k8s-cluster",
                "ORESoftware/mcp-rust-libs",
                "ORESoftware/mip-solver-node.rs",
            },
        )
        self.assertIsNone(manifest["repository_selection"]["default_repository"])
        self.assertEqual(manifest["repository_selection"]["runtime_allowlist"], [])

    def test_relationship_renderer_covers_every_mapped_owner_deterministically(self) -> None:
        first = relationship_renderer.render_all_relationships(
            self.registry, IMMUTABLE_TEST_REF
        )
        second = relationship_renderer.render_all_relationships(
            self.registry, IMMUTABLE_TEST_REF
        )
        self.assertEqual(first, second)
        index = json.loads(first["repository-relationships-index.json"])
        self.assertEqual(index["owner_count"], 31)
        self.assertEqual(len(index["files"]), 31)
        for path, digest in index["files"].items():
            self.assertEqual(
                digest, hashlib.sha256(first[path].encode("utf-8")).hexdigest()
            )

    def test_relationship_manifest_preserves_conflict_contract_and_precedence(self) -> None:
        manifest = relationship_renderer.build_relationship_manifest(
            self.registry, "fiducia-cloud", IMMUTABLE_TEST_REF
        )
        conflict = manifest["git_conflict_resolution"]
        self.assertEqual(
            conflict["directive_verbatim"],
            context_renderer.SEMANTIC_CONFLICT_DIRECTIVE,
        )
        self.assertEqual(
            conflict["history_lookback_commits"]["minimum"], 3
        )
        self.assertEqual(
            conflict["history_lookback_commits"]["maximum"], 10
        )
        self.assertIn(
            "relevant_external_github_organization_repositories",
            conflict["context_scope"],
        )
        self.assertFalse(
            manifest["governance"]["automatic_agent_instruction_inheritance"]
        )
        self.assertTrue(
            manifest["governance"]["repository_local_instruction_mirror_required"]
        )
        self.assertEqual(
            manifest["repository_selection"]["unregistered_dependencies"],
            "unknown_not_assumed",
        )

    def test_mutable_registry_ref_is_rejected(self) -> None:
        with self.assertRaisesRegex(registry_module.RegistryError, "immutable"):
            relationship_renderer.build_relationship_manifest(
                self.registry, "fiducia-cloud", "main"
            )
        with self.assertRaisesRegex(registry_module.RegistryError, "immutable"):
            rollout_audit.build_rollout_audit(
                self.registry, self.inventory, "main"
            )

    def test_inventory_duplicate_and_account_drift_fail_closed(self) -> None:
        duplicate = copy.deepcopy(self.inventory)
        duplicated_repository = copy.deepcopy(duplicate["repositories"][0])
        duplicated_repository["repository_id"] += 1000
        duplicate["repositories"].append(duplicated_repository)
        with self.assertRaisesRegex(registry_module.RegistryError, "duplicate"):
            rollout_audit.validate_inventory(duplicate)

        drifted = copy.deepcopy(self.inventory)
        drifted["repositories"][0]["owner_account_id"] += 1
        with self.assertRaisesRegex(registry_module.RegistryError, "account ID drift"):
            rollout_audit.build_rollout_audit(
                self.registry, drifted, IMMUTABLE_TEST_REF
            )

    def test_private_existing_repository_is_a_visibility_blocker(self) -> None:
        changed = copy.deepcopy(self.inventory)
        changed["repositories"][0]["visibility"] = "private"
        report = rollout_audit.build_rollout_audit(
            self.registry, changed, IMMUTABLE_TEST_REF
        )
        self.assertEqual(report["summary"]["visibility_mismatch"], 1)
        self.assertEqual(report["summary"]["existing_public"], 2)
        self.assertFalse(report["summary"]["complete"])

    def test_relationship_writer_rejects_symlink_substitution(self) -> None:
        files = relationship_renderer.render_all_relationships(
            self.registry, IMMUTABLE_TEST_REF
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            owner_dir = root / "fiducia-cloud"
            owner_dir.mkdir()
            target = root / "target.json"
            target.write_text("{}\n", encoding="utf-8")
            (owner_dir / "repository-relationships.json").symlink_to(target)
            with self.assertRaisesRegex(registry_module.RegistryError, "symlink"):
                relationship_renderer._write_files(root, files)


if __name__ == "__main__":
    unittest.main()
