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

import render_org_project_context as renderer  # noqa: E402
import validate_org_project_registry as registry_module  # noqa: E402
import verify_org_project_context as verifier  # noqa: E402


IMMUTABLE_TEST_REF = "1" * 40


class OrgProjectRegistryTest(unittest.TestCase):
    def setUp(self) -> None:
        self.registry = registry_module.load_registry(
            ROOT / "config" / "org-project-registry.yaml"
        )

    def test_checked_in_registry_is_valid(self) -> None:
        counts = registry_module.validate_registry(self.registry)
        self.assertEqual(counts["mappings"], 31)
        self.assertEqual(counts["repository_overrides"], 5)
        self.assertEqual(counts["runtime_routes"], 13)
        self.assertEqual(counts["unmapped"], 7)

    def test_duplicate_linear_project_id_fails_closed(self) -> None:
        changed = copy.deepcopy(self.registry)
        changed["mappings"][1]["linear"]["project_id"] = changed["mappings"][0][
            "linear"
        ]["project_id"]
        with self.assertRaisesRegex(
            registry_module.RegistryError, "duplicate owner-level Linear project ID"
        ):
            registry_module.validate_registry(changed)

    def test_duplicate_linear_project_url_fails_closed(self) -> None:
        changed = copy.deepcopy(self.registry)
        changed["mappings"][1]["linear"]["project_id"] = (
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        )
        changed["mappings"][1]["linear"]["project_url"] = changed["mappings"][0][
            "linear"
        ]["project_url"]
        with self.assertRaisesRegex(registry_module.RegistryError, "project URL"):
            registry_module.validate_registry(changed)

    def test_case_insensitive_alias_collision_fails_closed(self) -> None:
        changed = copy.deepcopy(self.registry)
        changed["mappings"][1]["github"]["aliases"].append(
            changed["mappings"][0]["github"]["aliases"][0].upper()
        )
        with self.assertRaisesRegex(registry_module.RegistryError, "maps to both"):
            registry_module.validate_registry(changed)

    def test_unmapped_owner_cannot_also_be_mapped(self) -> None:
        changed = copy.deepcopy(self.registry)
        changed["unmapped_installed_organizations"][0]["github"]["account_id"] = (
            changed["mappings"][0]["github"]["account_id"]
        )
        with self.assertRaisesRegex(registry_module.RegistryError, "account_id collides"):
            registry_module.validate_registry(changed)

    def test_repository_override_requires_mapped_owner(self) -> None:
        changed = copy.deepcopy(self.registry)
        changed["repository_overrides"][0]["repository"] = "unknown-owner/example"
        with self.assertRaisesRegex(registry_module.RegistryError, "unmapped GitHub owner"):
            registry_module.validate_registry(changed)

    def test_ambiguity_policy_cannot_be_weakened(self) -> None:
        changed = copy.deepcopy(self.registry)
        changed["resolution"]["on_ambiguous"] = "pick_first"
        with self.assertRaisesRegex(registry_module.RegistryError, "on_ambiguous"):
            registry_module.validate_registry(changed)

    def test_public_registry_rejects_credential_markers(self) -> None:
        changed = copy.deepcopy(self.registry)
        changed["mappings"][0]["linear"]["project_name"] = "ghp_not-a-real-value"
        with self.assertRaisesRegex(registry_module.RegistryError, "credential-like"):
            registry_module.validate_registry(changed)

    def test_public_registry_rejects_slack_token_markers(self) -> None:
        changed = copy.deepcopy(self.registry)
        changed["mappings"][0]["linear"]["project_name"] = "xoxb-not-a-real-value"
        with self.assertRaisesRegex(registry_module.RegistryError, "credential-like"):
            registry_module.validate_registry(changed)

    def test_observed_date_must_exist(self) -> None:
        changed = copy.deepcopy(self.registry)
        changed["observed_at"] = "2026-02-31"
        with self.assertRaisesRegex(registry_module.RegistryError, "real YYYY-MM-DD"):
            registry_module.validate_registry(changed)

    def test_linear_url_must_identify_project_in_workspace(self) -> None:
        changed = copy.deepcopy(self.registry)
        changed["mappings"][0]["linear"]["project_url"] = (
            "https://linear.app/other/project/not-denman"
        )
        with self.assertRaisesRegex(registry_module.RegistryError, "workspace denman"):
            registry_module.validate_registry(changed)

    def test_registry_loader_rejects_non_finite_json(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "registry.yaml"
            path.write_text('{"value": NaN}', encoding="utf-8")
            with self.assertRaisesRegex(registry_module.RegistryError, "non-finite"):
                registry_module.load_registry(path)

    def test_registry_loader_rejects_utf8_bom(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "registry.yaml"
            path.write_bytes(b"\xef\xbb\xbf{}")
            with self.assertRaisesRegex(registry_module.RegistryError, "byte-order"):
                registry_module.load_registry(path)

    def test_registry_loader_rejects_excessive_nesting(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "registry.yaml"
            path.write_text("[" * 2000 + "0" + "]" * 2000, encoding="utf-8")
            with self.assertRaisesRegex(registry_module.RegistryError, "too deep"):
                registry_module.load_registry(path)

    def test_registry_rejects_surrogate_text(self) -> None:
        changed = copy.deepcopy(self.registry)
        changed["mappings"][0]["linear"]["project_name"] = "invalid-\ud800"
        with self.assertRaisesRegex(registry_module.RegistryError, "forbidden"):
            registry_module.validate_registry(changed)

    def test_malformed_url_fails_with_registry_error(self) -> None:
        changed = copy.deepcopy(self.registry)
        changed["mappings"][0]["linear"]["project_url"] = "https://[linear.app/x"
        with self.assertRaisesRegex(registry_module.RegistryError, "well-formed"):
            registry_module.validate_registry(changed)

    def test_owner_alias_resolves_case_insensitively(self) -> None:
        project = registry_module.resolve_project(self.registry, "GITHUB.COM/STREEMPILOT")
        self.assertEqual(project["project_id"], "3f5bd157-4424-42cc-94d0-0bed993cdc1d")

    def test_exact_repository_override_precedes_owner_project(self) -> None:
        project = registry_module.resolve_project(
            self.registry, "ORESoftware", "ORESoftware/k8s-cluster"
        )
        self.assertEqual(project["project_id"], "18c58338-cf36-4fe6-8c71-245a795f8661")

    def test_fanwaave_push_server_uses_owner_project(self) -> None:
        project = registry_module.resolve_project(
            self.registry,
            "fanwaave",
            "fanwaave/push-notification-server.rs",
        )
        self.assertEqual(
            project["project_id"],
            "d765e227-5726-42c8-8643-a8bd9e5a9a8c",
        )

    def test_repository_cannot_escape_resolved_owner(self) -> None:
        with self.assertRaisesRegex(registry_module.RegistryError, "does not match"):
            registry_module.resolve_project(
                self.registry, "fiducia-cloud", "ORESoftware/k8s-cluster"
            )

    def test_repository_identity_rejects_control_characters(self) -> None:
        with self.assertRaisesRegex(registry_module.RegistryError, "forbidden"):
            registry_module.resolve_project(
                self.registry, "fiducia-cloud", "fiducia-cloud/repo\nother"
            )

    def test_runtime_repository_defaults_to_reviewed_allowlist(self) -> None:
        self.assertEqual(
            registry_module.resolve_runtime_repository(self.registry, "shared-auth"),
            "shared-auth/shared-auth-mcp-server.rs",
        )

    def test_runtime_repository_outside_allowlist_is_rejected(self) -> None:
        with self.assertRaisesRegex(registry_module.RegistryError, "allowlist"):
            registry_module.resolve_runtime_repository(
                self.registry, "shared-auth", "shared-auth/unreviewed-repository"
            )

    def test_owner_without_runtime_route_is_rejected_for_runtime(self) -> None:
        with self.assertRaisesRegex(registry_module.RegistryError, "no reviewed"):
            registry_module.resolve_runtime_repository(self.registry, "sonus-auris")

    def test_renderer_emits_public_safe_fiducia_bundle(self) -> None:
        bundle = renderer.render_bundle(
            self.registry, "fiducia-cloud", registry_ref=IMMUTABLE_TEST_REF
        )
        self.assertEqual(
            set(bundle),
            {
                "README.md",
                "project-context.yaml",
                "profile/README.md",
                "agents/org-context.agent.md",
                ".github/workflows/org-context-integrity.yml",
                "org-context-manifest.json",
            },
        )
        context = json.loads(bundle["project-context.yaml"])
        self.assertEqual(context["github"]["account_id"], 297262292)
        self.assertEqual(
            context["linear"]["project_id"],
            "d9e89bd3-19da-47f3-9bf7-6dc8cc910b70",
        )
        self.assertIsNone(context["runtime_route"])
        self.assertTrue(context["public_context_only"])
        self.assertTrue(context["generated_from"]["immutable"])
        self.assertEqual(context["generated_from"]["ref_type"], "commit")
        self.assertEqual(context["generated_from"]["ref"], IMMUTABLE_TEST_REF)

        manifest = json.loads(bundle["org-context-manifest.json"])
        self.assertEqual(manifest["registry_ref"], IMMUTABLE_TEST_REF)
        self.assertNotIn("org-context-manifest.json", manifest["files"])
        for path, digest in manifest["files"].items():
            self.assertEqual(
                digest, hashlib.sha256(bundle[path].encode("utf-8")).hexdigest()
            )

    def test_renderer_carries_reviewed_runtime_route(self) -> None:
        bundle = renderer.render_bundle(
            self.registry, "shared-auth", IMMUTABLE_TEST_REF
        )
        context = json.loads(bundle["project-context.yaml"])
        self.assertEqual(
            context["runtime_route"]["default_repository"],
            "shared-auth/shared-auth-mcp-server.rs",
        )

    def test_renderer_rejects_mutable_registry_ref(self) -> None:
        with self.assertRaisesRegex(renderer.RegistryError, "immutable"):
            renderer.render_bundle(self.registry, "fiducia-cloud", "main")

    def test_renderer_escapes_untrusted_markdown_text(self) -> None:
        changed = copy.deepcopy(self.registry)
        changed["mappings"][0]["linear"]["project_name"] = (
            "Unsafe [label] <script>alert(1)</script>"
        )
        bundle = renderer.render_bundle(changed, "3FA-app", IMMUTABLE_TEST_REF)
        profile = bundle["profile/README.md"]
        self.assertNotIn("<script>", profile)
        self.assertIn("\\[label\\]", profile)
        self.assertIn("&lt;script&gt;", profile)

    def test_rendered_integrity_workflow_is_pinned_and_read_only(self) -> None:
        bundle = renderer.render_bundle(
            self.registry, "fiducia-cloud", IMMUTABLE_TEST_REF
        )
        workflow = bundle[".github/workflows/org-context-integrity.yml"]
        self.assertIn(f'REGISTRY_REF: "{IMMUTABLE_TEST_REF}"', workflow)
        self.assertIn('EXPECTED_OWNER: "fiducia-cloud"', workflow)
        self.assertIn("permissions:\n  contents: read", workflow)
        self.assertIn("persist-credentials: false", workflow)
        self.assertIn("rhysd/actionlint@sha256:", workflow)

    def test_verifier_accepts_exact_bundle_and_rejects_drift(self) -> None:
        bundle = renderer.render_bundle(
            self.registry, "fiducia-cloud", IMMUTABLE_TEST_REF
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            renderer._write_bundle(root, bundle)
            evidence = verifier.verify_bundle(root, bundle)
            self.assertEqual(evidence["files"], len(bundle))
            profile = root / "profile" / "README.md"
            profile.write_text(profile.read_text(encoding="utf-8") + "drift\n")
            with self.assertRaisesRegex(verifier.RegistryError, "drifted"):
                verifier.verify_bundle(root, bundle)

    def test_verifier_rejects_managed_symlink(self) -> None:
        bundle = renderer.render_bundle(
            self.registry, "fiducia-cloud", IMMUTABLE_TEST_REF
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            renderer._write_bundle(root, bundle)
            profile = root / "profile" / "README.md"
            profile.unlink()
            profile.symlink_to(root / "README.md")
            with self.assertRaisesRegex(verifier.RegistryError, "symlink"):
                verifier.verify_bundle(root, bundle)

    def test_renderer_embeds_verbatim_semantic_conflict_directive(self) -> None:
        bundle = renderer.render_bundle(
            self.registry, "sonus-auris", IMMUTABLE_TEST_REF
        )
        directive = renderer.SEMANTIC_CONFLICT_DIRECTIVE
        self.assertIn(directive, bundle["agents/org-context.agent.md"])
        self.assertIn(directive, bundle["profile/README.md"])
        context = json.loads(bundle["project-context.yaml"])
        self.assertEqual(
            context["git_conflict_resolution"]["directive_verbatim"],
            directive,
        )

    def test_semantic_conflict_policy_requires_history_and_cross_org_context(self) -> None:
        context = json.loads(
            renderer.render_bundle(
                self.registry, "shared-auth", IMMUTABLE_TEST_REF
            )["project-context.yaml"]
        )
        policy = context["git_conflict_resolution"]
        self.assertEqual(policy["mode"], "semantic_conceptual_merge")
        self.assertEqual(
            policy["history_lookback_commits"],
            {
                "minimum": 3,
                "maximum": 10,
                "when_available": True,
                "inspect_both_sides": True,
                "inspect_merge_base": True,
                "path_scoped_history": True,
            },
        )
        self.assertIn(
            "same_github_organization_repositories",
            policy["context_scope"],
        )
        self.assertIn(
            "relevant_external_github_organization_repositories",
            policy["context_scope"],
        )
        self.assertEqual(
            {
                "wholesale_ours",
                "wholesale_theirs",
                "wholesale_current",
                "wholesale_incoming",
            }.issubset(policy["forbidden_shortcuts"]),
            True,
        )

    def test_unknown_owner_is_rejected(self) -> None:
        with self.assertRaisesRegex(registry_module.RegistryError, "0 matches"):
            registry_module.resolve_owner(self.registry, "not-a-mapped-owner")


if __name__ == "__main__":
    unittest.main()
