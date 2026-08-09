from __future__ import annotations

import contextlib
import copy
import importlib.util
import json
import os
import pathlib
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Iterator
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / "repository-fleets" / "hypesiege-streempilot.json"
PUBLISHER_PATH = ROOT / "scripts" / "publish_hypesiege_streempilot_fleet.py"
TEST_TOKEN = "ghs_" + "a" * 36


def load_module(name: str, path: pathlib.Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


PUBLISHER = load_module("fleet_publisher_hardening", PUBLISHER_PATH)


@contextlib.contextmanager
def running_server(
    handler: type[BaseHTTPRequestHandler],
) -> Iterator[ThreadingHTTPServer]:
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        thread.join(timeout=5)
        server.server_close()


def write_manifest(root: pathlib.Path, manifest: dict) -> pathlib.Path:
    path = root / "manifest.json"
    path.write_text(json.dumps(manifest), encoding="utf-8")
    return path


def repository_metadata(record: dict, *, require_settings: bool = True) -> dict:
    metadata = {
        "id": 12345,
        "owner": {"login": record["org"], "type": "Organization"},
        "full_name": record["full_name"],
        "name": record["name"],
        "visibility": record["visibility"],
        "private": record["visibility"] == "private",
        "clone_url": record["remote"],
        "html_url": f"https://github.com/{record['full_name']}",
        "fork": False,
        "archived": False,
        "disabled": False,
    }
    if require_settings:
        metadata.update(
            {
                "description": record["description"],
                "default_branch": "main",
                **PUBLISHER.REPOSITORY_SETTINGS,
            }
        )
    return metadata


class ManifestHardeningTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))

    def test_manifest_rejects_remote_substitution_before_selection(self) -> None:
        candidate = copy.deepcopy(self.manifest)
        candidate["repositories"][0]["remote"] = "https://attacker.example/repo.git"
        with tempfile.TemporaryDirectory(prefix="fleet-manifest-") as temp:
            path = write_manifest(pathlib.Path(temp), candidate)
            with self.assertRaisesRegex(
                PUBLISHER.PublicationError,
                "canonical GitHub HTTPS URL",
            ):
                PUBLISHER.load_manifest(path)

    def test_manifest_rejects_repository_path_traversal(self) -> None:
        candidate = copy.deepcopy(self.manifest)
        record = candidate["repositories"][0]
        record["name"] = "../escape"
        record["full_name"] = f"{record['org']}/{record['name']}"
        record["remote"] = f"https://github.com/{record['full_name']}.git"
        with tempfile.TemporaryDirectory(prefix="fleet-manifest-") as temp:
            path = write_manifest(pathlib.Path(temp), candidate)
            with self.assertRaisesRegex(
                PUBLISHER.PublicationError,
                "canonical lowercase GitHub name",
            ):
                PUBLISHER.load_manifest(path)

    def test_manifest_rejects_aggregate_drift_and_unknown_fields(self) -> None:
        candidate = copy.deepcopy(self.manifest)
        candidate["repositories"][0]["files"] += 1
        with tempfile.TemporaryDirectory(prefix="fleet-manifest-") as temp:
            path = write_manifest(pathlib.Path(temp), candidate)
            with self.assertRaisesRegex(
                PUBLISHER.PublicationError,
                "file counts",
            ):
                PUBLISHER.load_manifest(path)

        candidate = copy.deepcopy(self.manifest)
        candidate["unexpected"] = True
        with tempfile.TemporaryDirectory(prefix="fleet-manifest-") as temp:
            path = write_manifest(pathlib.Path(temp), candidate)
            with self.assertRaisesRegex(
                PUBLISHER.PublicationError,
                "unsupported or malformed",
            ):
                PUBLISHER.load_manifest(path)


class PublisherSecurityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest = PUBLISHER.load_manifest(MANIFEST_PATH)
        cls.record = next(
            record
            for record in cls.manifest["repositories"]
            if record["org"] == "hypesiege" and record["kind"] != "monorepo"
        )

    def test_token_and_api_path_validation_fail_closed(self) -> None:
        self.assertEqual(PUBLISHER.validate_token(TEST_TOKEN), TEST_TOKEN)
        for token in (None, "short", TEST_TOKEN + "\n", "ghs_invalid-token"):
            with self.subTest(token=token):
                with self.assertRaises(PUBLISHER.PublicationError):
                    PUBLISHER.validate_token(token)

        PUBLISHER.validate_api_path("/repos/hypesiege/example/commits/main")
        for path in (
            "repos/hypesiege/example",
            "//attacker.example/path",
            "/repos/../attacker",
            "/repos/%2e%2e/attacker",
            "/repos/example?token=x",
            "/repos/example#fragment",
            "/repos/example\\child",
        ):
            with self.subTest(path=path):
                with self.assertRaises(PUBLISHER.PublicationError):
                    PUBLISHER.validate_api_path(path)

    def test_git_environment_drops_inherited_execution_hooks(self) -> None:
        inherited = {
            "GIT_CONFIG_COUNT": "1",
            "GIT_SSH_COMMAND": "steal-token",
            "SSH_ASKPASS": "steal-token",
            "LD_PRELOAD": "/tmp/steal-token.so",
            "BASH_ENV": "/tmp/steal-token.sh",
            "HOME": "/tmp/untrusted-home",
            PUBLISHER.TOKEN_ENV: "old-token",
        }
        with mock.patch.dict(os.environ, inherited, clear=False):
            environment = PUBLISHER.sanitized_git_environment(
                TEST_TOKEN,
                pathlib.Path("/tmp/askpass"),
            )
        for key in inherited:
            if key == PUBLISHER.TOKEN_ENV:
                continue
            self.assertNotIn(key, environment)
        self.assertEqual(environment[PUBLISHER.TOKEN_ENV], TEST_TOKEN)
        self.assertEqual(environment["GIT_CONFIG_NOSYSTEM"], "1")
        self.assertEqual(environment["GIT_CONFIG_GLOBAL"], os.devnull)
        self.assertEqual(environment["GIT_PROTOCOL_FROM_USER"], "0")
        self.assertNotIn("https://", environment["PATH"])

    def test_local_git_configuration_rejects_execution_and_redirects(self) -> None:
        with tempfile.TemporaryDirectory(prefix="fleet-unsafe-config-") as temp:
            repository = pathlib.Path(temp) / "repo"
            repository.mkdir()
            PUBLISHER.run(["git", "init", "-b", "main"], repository)
            settings = (
                ("core.hooksPath", "/tmp/untrusted-hooks"),
                (
                    "url.https://attacker.example/.insteadOf",
                    "https://github.com/",
                ),
                ("remote.origin.proxy", "http://attacker.example"),
                ("credential.helper", "steal-token"),
                ("http.extraHeader", "Authorization: secret"),
                ("protocol.ext.allow", "always"),
            )
            for key, value in settings:
                PUBLISHER.run(["git", "config", key, value], repository)
            self.assertEqual(
                PUBLISHER.unsafe_local_git_config(repository),
                [
                    "core.hookspath",
                    "credential.helper",
                    "http.extraheader",
                    "protocol.ext.allow",
                    "remote.origin.proxy",
                    "url.https://attacker.example/.insteadof",
                ],
            )

    def test_git_storage_rejects_external_object_alternates(self) -> None:
        with tempfile.TemporaryDirectory(prefix="fleet-alternates-") as temp:
            repository = pathlib.Path(temp) / "repo"
            repository.mkdir()
            PUBLISHER.run(["git", "init", "-b", "main"], repository)
            info = repository / ".git" / "objects" / "info"
            info.mkdir(parents=True, exist_ok=True)
            (info / "alternates").write_text("/tmp/untrusted-objects\n", encoding="utf-8")
            with self.assertRaisesRegex(
                PUBLISHER.PublicationError,
                "object indirection",
            ):
                PUBLISHER.validate_git_storage(repository)

    def test_api_redirect_is_not_followed(self) -> None:
        class TargetHandler(BaseHTTPRequestHandler):
            hits = 0

            def log_message(self, format: str, *args) -> None:  # noqa: A002, ANN002
                return

            def do_GET(self) -> None:  # noqa: N802
                type(self).hits += 1
                payload = b'{"unexpected":true}'
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        class RedirectHandler(BaseHTTPRequestHandler):
            location = ""

            def log_message(self, format: str, *args) -> None:  # noqa: A002, ANN002
                return

            def do_GET(self) -> None:  # noqa: N802
                self.send_response(302)
                self.send_header("Location", type(self).location)
                self.end_headers()

        with running_server(TargetHandler) as target:
            RedirectHandler.location = (
                f"http://127.0.0.1:{target.server_port}/credential-target"
            )
            with running_server(RedirectHandler) as redirect:
                with mock.patch.object(
                    PUBLISHER,
                    "API_BASE",
                    f"http://127.0.0.1:{redirect.server_port}",
                ):
                    with self.assertRaisesRegex(
                        PUBLISHER.PublicationError,
                        "redirect rejected",
                    ):
                        PUBLISHER.request_json(
                            "GET",
                            "/repos/hypesiege/example",
                            TEST_TOKEN,
                        )
        self.assertEqual(TargetHandler.hits, 0)

    def test_api_error_redacts_token(self) -> None:
        class ErrorHandler(BaseHTTPRequestHandler):
            def log_message(self, format: str, *args) -> None:  # noqa: A002, ANN002
                return

            def do_GET(self) -> None:  # noqa: N802
                payload = json.dumps({"message": TEST_TOKEN}).encode("utf-8")
                self.send_response(500)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        with running_server(ErrorHandler) as server:
            with mock.patch.object(
                PUBLISHER,
                "API_BASE",
                f"http://127.0.0.1:{server.server_port}",
            ):
                with self.assertRaises(PUBLISHER.PublicationError) as raised:
                    PUBLISHER.request_json(
                        "GET",
                        "/repos/hypesiege/example",
                        TEST_TOKEN,
                    )
        self.assertNotIn(TEST_TOKEN, str(raised.exception))
        self.assertIn("[REDACTED]", str(raised.exception))

    def test_repository_metadata_is_bound_to_the_canonical_remote(self) -> None:
        metadata = repository_metadata(self.record)
        validated = PUBLISHER.validate_repository_metadata(
            self.record,
            metadata,
            require_settings=True,
        )
        self.assertEqual(validated["id"], 12345)
        metadata["clone_url"] = "https://attacker.example/repository.git"
        with self.assertRaisesRegex(
            PUBLISHER.PublicationError,
            "non-canonical clone URL",
        ):
            PUBLISHER.validate_repository_metadata(
                self.record,
                metadata,
                require_settings=True,
            )

    def test_divergent_remote_is_rejected_before_push(self) -> None:
        metadata = repository_metadata(self.record)
        with (
            mock.patch.object(PUBLISHER, "verify_monorepo_children"),
            mock.patch.object(
                PUBLISHER,
                "ensure_repository",
                return_value=(metadata, False),
            ),
            mock.patch.object(
                PUBLISHER,
                "remote_main_commit",
                return_value="f" * 40,
            ),
            mock.patch.object(PUBLISHER, "push_main") as push,
            mock.patch.object(PUBLISHER, "configure_repository") as configure,
        ):
            with self.assertRaisesRegex(
                PUBLISHER.PublicationError,
                "divergent remote main",
            ):
                PUBLISHER.publish_record(
                    self.manifest,
                    self.record,
                    pathlib.Path("/tmp/source"),
                    TEST_TOKEN,
                )
        push.assert_not_called()
        configure.assert_not_called()

    def test_exact_remote_is_idempotent_and_not_pushed_again(self) -> None:
        metadata = repository_metadata(self.record)
        with (
            mock.patch.object(PUBLISHER, "verify_monorepo_children"),
            mock.patch.object(
                PUBLISHER,
                "ensure_repository",
                return_value=(metadata, False),
            ),
            mock.patch.object(
                PUBLISHER,
                "remote_main_commit",
                side_effect=[self.record["commit"], self.record["commit"]],
            ),
            mock.patch.object(PUBLISHER, "push_main") as push,
            mock.patch.object(
                PUBLISHER,
                "configure_repository",
                return_value=metadata,
            ),
        ):
            result = PUBLISHER.publish_record(
                self.manifest,
                self.record,
                pathlib.Path("/tmp/source"),
                TEST_TOKEN,
            )
        push.assert_not_called()
        self.assertFalse(result["pushed"])
        self.assertTrue(result["verified"])
        self.assertEqual(result["commit"], self.record["commit"])


if __name__ == "__main__":
    unittest.main()
