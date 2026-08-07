#!/usr/bin/env python3
from __future__ import annotations

from copy import deepcopy
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import urllib.error
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / "repository-fleets/hypesiege-streempilot.json"
PUBLISHER_PATH = ROOT / "scripts/publish_hypesiege_streempilot_fleet.py"
SPEC = importlib.util.spec_from_file_location("sealed_fleet_publisher", PUBLISHER_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"unable to load {PUBLISHER_PATH}")
PUBLISHER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = PUBLISHER
SPEC.loader.exec_module(PUBLISHER)


def private_manifest() -> dict[str, object]:
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    for record in manifest["repositories"]:
        record["visibility"] = "private"
    return manifest


class FakeResponse:
    def __init__(self, status: int, body: bytes) -> None:
        self.status = status
        self.body = body

    def __enter__(self) -> FakeResponse:
        return self

    def __exit__(self, *_args: object) -> None:
        return None

    def read(self, _limit: int) -> bytes:
        return self.body


class SealedFleetPublisherExecutionContracts(unittest.TestCase):
    def record(self, full_name: str = "hypesiege/hypesiege-api-server.rs") -> dict[str, object]:
        manifest = private_manifest()
        return next(
            record
            for record in manifest["repositories"]
            if record["full_name"] == full_name
        )

    def test_existing_exact_private_repository_is_reused_without_mutation(self) -> None:
        record = self.record()
        metadata = {
            "id": 123,
            "full_name": record["full_name"],
            "visibility": "private",
        }

        with mock.patch.object(
            PUBLISHER, "request_json", return_value=(200, metadata)
        ) as request_json:
            self.assertEqual(
                PUBLISHER.ensure_repository(record, "ephemeral-token"), metadata
            )

        request_json.assert_called_once_with(
            "GET", f"/repos/{record['full_name']}", "ephemeral-token"
        )

    def test_missing_repository_uses_one_bounded_private_create_request(self) -> None:
        record = self.record()
        metadata = {
            "id": 456,
            "full_name": record["full_name"],
            "visibility": "private",
        }
        calls: list[tuple[object, ...]] = []

        def request_json(
            method: str,
            path: str,
            token: str,
            body: dict[str, object] | None = None,
        ) -> tuple[int, dict[str, object] | None]:
            calls.append((method, path, token, body))
            return (404, None) if method == "GET" else (201, metadata)

        with mock.patch.object(PUBLISHER, "request_json", side_effect=request_json):
            self.assertEqual(
                PUBLISHER.ensure_repository(record, "ephemeral-token"), metadata
            )

        self.assertEqual([call[0] for call in calls], ["GET", "POST"])
        self.assertNotIn("PATCH", [call[0] for call in calls])
        method, path, token, body = calls[1]
        self.assertEqual(method, "POST")
        self.assertEqual(path, f"/orgs/{record['org']}/repos")
        self.assertEqual(token, "ephemeral-token")
        assert isinstance(body, dict)
        self.assertEqual(body["name"], record["name"])
        self.assertEqual(body["description"], record["description"])
        self.assertIs(body["private"], True)
        self.assertIs(body["auto_init"], False)
        self.assertIs(body["allow_rebase_merge"], False)
        self.assertIs(body["delete_branch_on_merge"], True)

    def test_repository_identity_visibility_and_metadata_fail_closed(self) -> None:
        record = self.record()
        bad_metadata = (
            {
                "full_name": "hypesiege/different-repository",
                "visibility": "private",
            },
            {
                "full_name": record["full_name"],
                "visibility": "public",
            },
            None,
        )
        for metadata in bad_metadata:
            with self.subTest(metadata=metadata):
                with (
                    mock.patch.object(
                        PUBLISHER, "request_json", return_value=(200, metadata)
                    ),
                    self.assertRaises(PUBLISHER.PublicationError),
                ):
                    PUBLISHER.ensure_repository(record, "ephemeral-token")

    def test_remote_main_commit_requires_a_string_sha(self) -> None:
        full_name = "hypesiege/hypesiege-api-server.rs"
        for payload in (None, {}, {"sha": None}, {"sha": 42}):
            with self.subTest(payload=payload):
                with (
                    mock.patch.object(
                        PUBLISHER, "request_json", return_value=(200, payload)
                    ),
                    self.assertRaisesRegex(
                        PUBLISHER.PublicationError, "invalid main-commit metadata"
                    ),
                ):
                    PUBLISHER.remote_main_commit(full_name, "ephemeral-token")

        with mock.patch.object(
            PUBLISHER,
            "request_json",
            return_value=(200, {"sha": "a" * 40}),
        ):
            self.assertEqual(
                PUBLISHER.remote_main_commit(full_name, "ephemeral-token"),
                "a" * 40,
            )

        with mock.patch.object(
            PUBLISHER, "request_json", return_value=(404, None)
        ):
            self.assertIsNone(
                PUBLISHER.remote_main_commit(full_name, "ephemeral-token")
            )

    def test_monorepo_requires_every_child_at_the_exact_remote_sha(self) -> None:
        manifest = private_manifest()
        monorepo = next(
            record
            for record in manifest["repositories"]
            if record["full_name"] == "hypesiege/hypesiege-monorepo"
        )
        children = [
            record
            for record in manifest["repositories"]
            if record["org"] == "hypesiege" and record["kind"] != "monorepo"
        ]
        expected = {
            str(record["full_name"]): str(record["commit"]) for record in children
        }

        with mock.patch.object(
            PUBLISHER,
            "remote_main_commit",
            side_effect=lambda full_name, _token: expected[full_name],
        ) as remote:
            PUBLISHER.verify_monorepo_children(
                manifest, monorepo, "ephemeral-token"
            )

        self.assertEqual(remote.call_count, 14)
        self.assertEqual(
            [call.args[0] for call in remote.call_args_list],
            [record["full_name"] for record in children],
        )

        first = children[0]
        with (
            mock.patch.object(
                PUBLISHER,
                "remote_main_commit",
                side_effect=lambda full_name, _token: (
                    "f" * 40
                    if full_name == first["full_name"]
                    else expected[full_name]
                ),
            ),
            self.assertRaisesRegex(
                PUBLISHER.PublicationError, str(first["full_name"])
            ),
        ):
            PUBLISHER.verify_monorepo_children(
                manifest, monorepo, "ephemeral-token"
            )

    def test_github_transport_is_bounded_and_get_404_is_the_only_soft_404(self) -> None:
        observed: dict[str, object] = {}

        def urlopen(request: object, timeout: int) -> FakeResponse:
            observed["request"] = request
            observed["timeout"] = timeout
            return FakeResponse(201, b'{"full_name":"hypesiege/example"}')

        with mock.patch.object(PUBLISHER.urllib.request, "urlopen", side_effect=urlopen):
            status, payload = PUBLISHER.request_json(
                "POST",
                "/orgs/hypesiege/repos",
                "ephemeral-token",
                {"name": "example", "private": True},
            )

        self.assertEqual(status, 201)
        self.assertEqual(payload, {"full_name": "hypesiege/example"})
        self.assertEqual(observed["timeout"], 30)
        request = observed["request"]
        self.assertEqual(request.get_method(), "POST")
        self.assertEqual(request.full_url, "https://api.github.com/orgs/hypesiege/repos")
        self.assertEqual(request.get_header("Authorization"), "Bearer ephemeral-token")
        self.assertEqual(request.get_header("Content-type"), "application/json")

        too_large = FakeResponse(200, b"x" * (256 * 1024 + 1))
        with (
            mock.patch.object(
                PUBLISHER.urllib.request, "urlopen", return_value=too_large
            ),
            self.assertRaisesRegex(
                PUBLISHER.PublicationError, "exceeded 256 KiB"
            ),
        ):
            PUBLISHER.request_json(
                "GET", "/repos/hypesiege/example", "ephemeral-token"
            )

        get_404 = urllib.error.HTTPError(
            "https://api.github.com/repos/hypesiege/example",
            404,
            "not found",
            None,
            io.BytesIO(b"{}"),
        )
        with mock.patch.object(
            PUBLISHER.urllib.request, "urlopen", side_effect=get_404
        ):
            self.assertEqual(
                PUBLISHER.request_json(
                    "GET", "/repos/hypesiege/example", "ephemeral-token"
                ),
                (404, None),
            )

        post_404 = urllib.error.HTTPError(
            "https://api.github.com/orgs/hypesiege/repos",
            404,
            "not found",
            None,
            io.BytesIO(b"{}"),
        )
        with (
            mock.patch.object(
                PUBLISHER.urllib.request, "urlopen", side_effect=post_404
            ),
            self.assertRaisesRegex(PUBLISHER.PublicationError, "GitHub API 404"),
        ):
            PUBLISHER.request_json(
                "POST",
                "/orgs/hypesiege/repos",
                "ephemeral-token",
                {"name": "example"},
            )

    def test_push_uses_ephemeral_noninteractive_askpass_and_never_forces(self) -> None:
        repository = Path("/tmp/sealed-fleet-source")
        token = "test-only-ephemeral-token"
        captured: dict[str, object] = {}

        with tempfile.TemporaryDirectory() as parent:
            askpass_directory = Path(parent) / "askpass"
            askpass_directory.mkdir()

            def fake_run(
                args: list[str],
                *,
                cwd: Path,
                env: dict[str, str],
                check: bool,
                text: bool,
                stdout: object,
                stderr: object,
            ) -> subprocess.CompletedProcess[str]:
                captured["args"] = list(args)
                captured["cwd"] = cwd
                captured["env"] = dict(env)
                captured["askpass"] = Path(env["GIT_ASKPASS"]).read_text(
                    encoding="utf-8"
                )
                return subprocess.CompletedProcess(args, 0, "ok", "")

            with (
                mock.patch.object(
                    PUBLISHER.tempfile,
                    "mkdtemp",
                    return_value=str(askpass_directory),
                ),
                mock.patch.object(PUBLISHER.subprocess, "run", side_effect=fake_run),
            ):
                PUBLISHER.push_main(repository, token)

            self.assertFalse(askpass_directory.exists())

        args = captured["args"]
        assert isinstance(args, list)
        self.assertEqual(
            args,
            ["git", "push", "--porcelain", "--set-upstream", "origin", "main"],
        )
        self.assertNotIn("--force", args)
        self.assertNotIn("-f", args)
        self.assertEqual(captured["cwd"], repository)

        environment = captured["env"]
        assert isinstance(environment, dict)
        self.assertEqual(environment[PUBLISHER.TOKEN_ENV], token)
        self.assertEqual(environment["GIT_TERMINAL_PROMPT"], "0")
        self.assertEqual(environment["GIT_ASKPASS_REQUIRE"], "force")

        askpass = captured["askpass"]
        assert isinstance(askpass, str)
        self.assertNotIn(token, askpass)
        self.assertIn(PUBLISHER.TOKEN_ENV, askpass)
        self.assertIn("x-access-token", askpass)

    def test_source_has_no_patch_or_force_mutation_path(self) -> None:
        source = PUBLISHER_PATH.read_text(encoding="utf-8")

        self.assertNotIn('"PATCH"', source)
        self.assertNotIn("'PATCH'", source)
        self.assertNotIn("--force", source)
        self.assertIn(
            '["git", "push", "--porcelain", "--set-upstream", "origin", "main"]',
            source,
        )


if __name__ == "__main__":
    unittest.main()
