from __future__ import annotations

import copy
import email.message
import importlib.util
import io
import json
import pathlib
import subprocess
import tempfile
import unittest
import urllib.error
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / "repository-fleets" / "hypesiege-streempilot.json"
PUBLISHER_PATH = ROOT / "scripts" / "publish_hypesiege_streempilot_fleet.py"


def load_module(name: str, path: pathlib.Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


PUBLISHER = load_module("fleet_publisher_hardening", PUBLISHER_PATH)


class FakeHeaders(email.message.Message):
    def __init__(self, content_type: str = "application/json") -> None:
        super().__init__()
        self["Content-Type"] = content_type


class FakeResponse:
    def __init__(
        self,
        body: bytes,
        *,
        status: int = 200,
        content_type: str = "application/json",
    ) -> None:
        self.body = body
        self.status = status
        self.headers = FakeHeaders(content_type)

    def read(self, amount: int = -1) -> bytes:
        return self.body[:amount]

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, traceback) -> bool:
        return False


class FakeOpener:
    def __init__(self, result) -> None:
        self.result = result

    def open(self, request, timeout: int):
        if isinstance(self.result, BaseException):
            raise self.result
        return self.result


class ManifestHardeningTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))

    def write_manifest(self, directory: pathlib.Path, payload: object) -> pathlib.Path:
        path = directory / "manifest.json"
        path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
        return path

    def assert_rejected(self, mutate) -> None:
        with tempfile.TemporaryDirectory(prefix="fleet-manifest-negative-") as temp:
            payload = copy.deepcopy(self.manifest)
            mutate(payload)
            with self.assertRaises(PUBLISHER.PublicationError):
                PUBLISHER.load_manifest(self.write_manifest(pathlib.Path(temp), payload))

    def test_canonical_manifest_is_accepted(self) -> None:
        manifest = PUBLISHER.load_manifest(MANIFEST_PATH)
        self.assertEqual(manifest["repository_count"], 32)
        self.assertEqual(manifest["total_tracked_files"], 888)
        self.assertEqual(manifest["total_gitlinks"], 30)

    def test_duplicate_json_keys_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="fleet-duplicate-key-") as temp:
            raw = MANIFEST_PATH.read_text(encoding="utf-8").replace(
                '  "schema_version": 2,',
                '  "schema_version": 2,\n  "schema_version": 2,',
                1,
            )
            path = pathlib.Path(temp) / "manifest.json"
            path.write_text(raw, encoding="utf-8")
            with self.assertRaisesRegex(PUBLISHER.PublicationError, "duplicate JSON key"):
                PUBLISHER.load_manifest(path)

    def test_symlink_and_oversized_manifests_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="fleet-manifest-path-") as temp:
            root = pathlib.Path(temp)
            target = root / "target.json"
            target.write_text(MANIFEST_PATH.read_text(encoding="utf-8"), encoding="utf-8")
            link = root / "manifest.json"
            link.symlink_to(target)
            with self.assertRaisesRegex(PUBLISHER.PublicationError, "non-symlink"):
                PUBLISHER.load_manifest(link)
            huge = root / "huge.json"
            huge.write_bytes(b"{" + b" " * PUBLISHER.MAX_MANIFEST_BYTES + b"}")
            with self.assertRaisesRegex(PUBLISHER.PublicationError, "size"):
                PUBLISHER.load_manifest(huge)

    def test_unknown_fields_private_visibility_and_remote_credentials_are_rejected(self) -> None:
        mutations = (
            lambda value: value.__setitem__("credential", "never"),
            lambda value: value["repositories"][0].__setitem__("token", "never"),
            lambda value: value["repositories"][0].__setitem__("visibility", "private"),
            lambda value: value["repositories"][0].__setitem__(
                "remote", "https://user:secret@github.com/hypesiege/example.git"
            ),
            lambda value: value["repositories"][0].__setitem__("org", ["hypesiege"]),
        )
        for mutate in mutations:
            with self.subTest(mutate=mutate):
                self.assert_rejected(mutate)

    def test_aggregate_and_order_drift_are_rejected(self) -> None:
        self.assert_rejected(
            lambda value: value["repositories"][0].__setitem__(
                "files", value["repositories"][0]["files"] + 1
            )
        )

        def swap(value) -> None:
            value["repositories"][0], value["repositories"][1] = (
                value["repositories"][1],
                value["repositories"][0],
            )

        self.assert_rejected(swap)


class ApiHardeningTests(unittest.TestCase):
    token = "ghs_" + "a" * 40

    def test_token_shape_and_diagnostics_are_safe(self) -> None:
        for invalid in (None, "short", "ghs_" + "a" * 10, self.token + "\n"):
            with self.subTest(invalid=invalid):
                with self.assertRaises(PUBLISHER.PublicationError):
                    PUBLISHER.validate_token(invalid)
        self.assertEqual(PUBLISHER.validate_token(self.token), self.token)
        detail = PUBLISHER.sanitize_detail(
            f"Authorization: Bearer {self.token}; github_pat_ABC secret={self.token}\x00",
            token=self.token,
        )
        self.assertNotIn(self.token, detail)
        self.assertNotIn("github_pat_ABC", detail)
        self.assertNotIn("\x00", detail)

    def test_api_paths_and_request_bodies_are_bounded(self) -> None:
        for path in (
            "https://evil.example/repos/x",
            "/repos/x/../y",
            "/repos//x",
            "/repos/x?token=oops",
            "/repos/x\r\nInjected: true",
        ):
            with self.subTest(path=path):
                with self.assertRaises(PUBLISHER.PublicationError):
                    PUBLISHER.validate_api_path(path)
        with self.assertRaisesRegex(PUBLISHER.PublicationError, "must be an object"):
            PUBLISHER.request_json(
                "POST", "/orgs/hypesiege/repos", self.token, body=["not-an-object"]
            )
        with self.assertRaisesRegex(PUBLISHER.PublicationError, "64 KiB"):
            PUBLISHER.request_json(
                "POST",
                "/orgs/hypesiege/repos",
                self.token,
                body={"description": "x" * PUBLISHER.MAX_API_REQUEST_BYTES},
            )

    def test_api_responses_reject_redirects_non_json_oversize_and_duplicates(self) -> None:
        with self.assertRaisesRegex(PUBLISHER.PublicationError, "content type"):
            PUBLISHER.request_json(
                "GET",
                "/repos/hypesiege/example",
                self.token,
                opener=FakeOpener(FakeResponse(b"html", content_type="text/html")),
            )
        with self.assertRaisesRegex(PUBLISHER.PublicationError, "exceeded"):
            PUBLISHER.request_json(
                "GET",
                "/repos/hypesiege/example",
                self.token,
                opener=FakeOpener(
                    FakeResponse(b"{" + b" " * PUBLISHER.MAX_API_RESPONSE_BYTES + b"}")
                ),
            )
        with self.assertRaisesRegex(PUBLISHER.PublicationError, "duplicate JSON key"):
            PUBLISHER.request_json(
                "GET",
                "/repos/hypesiege/example",
                self.token,
                opener=FakeOpener(FakeResponse(b'{"id":1,"id":2}')),
            )
        redirect = urllib.error.HTTPError(
            "https://api.github.com/repos/hypesiege/example",
            302,
            f"redirect Bearer {self.token}",
            FakeHeaders(),
            io.BytesIO(f"Bearer {self.token}".encode()),
        )
        with self.assertRaises(PUBLISHER.PublicationError) as caught:
            PUBLISHER.request_json(
                "GET",
                "/repos/hypesiege/example",
                self.token,
                opener=FakeOpener(redirect),
            )
        self.assertNotIn(self.token, str(caught.exception))

    def test_repository_metadata_requires_complete_safe_shape(self) -> None:
        record = {"org": "hypesiege", "full_name": "hypesiege/example"}
        safe = {
            "id": 1,
            "full_name": "hypesiege/example",
            "owner": {"login": "hypesiege"},
            "visibility": "public",
            "private": False,
            "fork": False,
            "archived": False,
            "disabled": False,
        }
        self.assertEqual(PUBLISHER.verify_repository_metadata(record, safe), safe)
        for key in ("owner", "fork", "archived", "disabled"):
            malformed = dict(safe)
            malformed.pop(key)
            with self.subTest(key=key), self.assertRaises(PUBLISHER.PublicationError):
                PUBLISHER.verify_repository_metadata(record, malformed)


class PublicationStateMachineTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest = PUBLISHER.load_manifest(MANIFEST_PATH)
        cls.record = PUBLISHER.select_record(
            cls.manifest, "hypesiege/hypesiege-api-server.rs"
        )
        cls.token = "ghs_" + "a" * 40
        cls.current = {
            "id": 123,
            "full_name": cls.record["full_name"],
            "owner": {"login": cls.record["org"]},
            "visibility": "public",
            "private": False,
            "fork": False,
            "archived": False,
            "disabled": False,
            "default_branch": "main",
            "allow_rebase_merge": False,
            "delete_branch_on_merge": True,
        }

    def test_exact_remote_is_idempotent_and_never_pushed(self) -> None:
        with (
            mock.patch.object(PUBLISHER, "verify_monorepo_children"),
            mock.patch.object(
                PUBLISHER, "ensure_repository", return_value=(self.current, False)
            ),
            mock.patch.object(
                PUBLISHER, "remote_main_commit", return_value=self.record["commit"]
            ),
            mock.patch.object(
                PUBLISHER, "apply_repository_settings", return_value=self.current
            ),
            mock.patch.object(PUBLISHER, "push_main") as push,
        ):
            result = PUBLISHER.publish_repository(
                self.manifest,
                self.record,
                pathlib.Path("/sealed/repo"),
                self.token,
            )
        self.assertEqual(result["state"], "already_verified")
        self.assertFalse(result["pushed"])
        push.assert_not_called()

    def test_divergent_or_non_main_remote_history_is_rejected_before_push(self) -> None:
        with (
            mock.patch.object(PUBLISHER, "verify_monorepo_children"),
            mock.patch.object(
                PUBLISHER, "ensure_repository", return_value=(self.current, False)
            ),
            mock.patch.object(PUBLISHER, "remote_main_commit", return_value="f" * 40),
            mock.patch.object(PUBLISHER, "push_main") as push,
        ):
            with self.assertRaisesRegex(PUBLISHER.PublicationError, "divergent"):
                PUBLISHER.publish_repository(
                    self.manifest,
                    self.record,
                    pathlib.Path("/sealed/repo"),
                    self.token,
                )
        push.assert_not_called()

        with (
            mock.patch.object(PUBLISHER, "verify_monorepo_children"),
            mock.patch.object(
                PUBLISHER, "ensure_repository", return_value=(self.current, False)
            ),
            mock.patch.object(PUBLISHER, "remote_main_commit", return_value=None),
            mock.patch.object(PUBLISHER, "remote_branch_names", return_value=["master"]),
            mock.patch.object(PUBLISHER, "push_main") as push,
        ):
            with self.assertRaisesRegex(PUBLISHER.PublicationError, "non-main history"):
                PUBLISHER.publish_repository(
                    self.manifest,
                    self.record,
                    pathlib.Path("/sealed/repo"),
                    self.token,
                )
        push.assert_not_called()

    def test_git_push_disables_hooks_helpers_redirects_and_prompts(self) -> None:
        captured = {}

        def fake_run(args, **kwargs):
            captured["args"] = args
            captured["env"] = kwargs["env"]
            captured["askpass"] = pathlib.Path(
                kwargs["env"]["GIT_ASKPASS"]
            ).read_text(encoding="utf-8")
            return subprocess.CompletedProcess(args, 0, stdout="ok", stderr="")

        with mock.patch.object(PUBLISHER.subprocess, "run", side_effect=fake_run):
            PUBLISHER.push_main(pathlib.Path("/sealed/repo"), self.token)
        args = captured["args"]
        self.assertIn("credential.helper=", args)
        self.assertIn("core.hooksPath=/dev/null", args)
        self.assertIn("http.followRedirects=false", args)
        self.assertIn("HEAD:refs/heads/main", args)
        self.assertNotIn(self.token, " ".join(args))
        self.assertEqual(captured["env"]["GIT_TERMINAL_PROMPT"], "0")
        self.assertEqual(captured["env"]["GIT_CONFIG_NOSYSTEM"], "1")
        self.assertIn(PUBLISHER.TOKEN_ENV, captured["askpass"])
        self.assertNotIn(self.token, captured["askpass"])

    def test_atomic_report_refuses_symlink_destination(self) -> None:
        with tempfile.TemporaryDirectory(prefix="fleet-report-") as temp:
            root = pathlib.Path(temp)
            target = root / "target.json"
            target.write_text("{}", encoding="utf-8")
            link = root / "report.json"
            link.symlink_to(target)
            with self.assertRaisesRegex(PUBLISHER.PublicationError, "symlink"):
                PUBLISHER.write_json_atomic(link, {"verified": True})


if __name__ == "__main__":
    unittest.main()
