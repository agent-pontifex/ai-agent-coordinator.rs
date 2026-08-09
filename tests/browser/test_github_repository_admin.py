#!/usr/bin/env python3
"""Browser-level contract tests for the repository-administration HTTP route.

The suite starts the real coordinator binary, drives it through Chromium, and
uses local deterministic GitHub API doubles. No live GitHub credential or
repository mutation is involved.
"""

from __future__ import annotations

import hmac
import json
import os
import socket
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

from playwright.sync_api import Browser, BrowserContext, Page, Playwright, sync_playwright

ROOT = Path(__file__).resolve().parents[2]
ARTIFACT_DIR = Path(
    os.environ.get("PLAYWRIGHT_ARTIFACT_DIR", ROOT / "artifacts/github-admin-browser")
)
API_TOKEN = "browser-test-api-token"
ADMIN_TOKEN = "browser-test-admin-token"
ORG = "browser-test-org"
REPOSITORY = "browser-test-repository"
FULL_NAME = f"{ORG}/{REPOSITORY}"


def reserve_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def repository_payload(*, full_name: str = FULL_NAME, private: bool = True) -> dict[str, Any]:
    return {
        "id": 991_001,
        "full_name": full_name,
        "html_url": f"https://untrusted.invalid/{full_name}",
        "private": private,
        "default_branch": "main",
    }


@dataclass
class RequestRecord:
    method: str
    path: str
    authorized: bool
    accept: str
    api_version: str
    user_agent: str
    body: dict[str, Any] | None


@dataclass
class GithubApiState:
    mode: str = "create"
    records: list[RequestRecord] = field(default_factory=list)
    repository_lookups: int = 0
    lock: threading.Lock = field(default_factory=threading.Lock)

    def reset(self, mode: str) -> None:
        with self.lock:
            self.mode = mode
            self.records.clear()
            self.repository_lookups = 0

    def append(self, record: RequestRecord) -> None:
        with self.lock:
            self.records.append(record)

    def snapshot(self) -> tuple[str, list[RequestRecord], int]:
        with self.lock:
            return self.mode, list(self.records), self.repository_lookups


@dataclass
class RedirectSinkState:
    hits: int = 0
    authorization_seen: bool = False
    lock: threading.Lock = field(default_factory=threading.Lock)

    def record(self, authorization_seen: bool) -> None:
        with self.lock:
            self.hits += 1
            self.authorization_seen = self.authorization_seen or authorization_seen

    def snapshot(self) -> tuple[int, bool]:
        with self.lock:
            return self.hits, self.authorization_seen


class QuietHandler(BaseHTTPRequestHandler):
    server_version = "browser-test-double"

    def log_message(self, _format: str, *_args: object) -> None:
        return

    def send_json(self, status: int, payload: dict[str, Any]) -> None:
        body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("content-type", "application/json; charset=utf-8")
        self.send_header("content-length", str(len(body)))
        self.send_header("cache-control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def read_json(self) -> dict[str, Any] | None:
        length = int(self.headers.get("content-length", "0"))
        if length > 64 * 1024:
            raise ValueError("mock request body exceeded 64 KiB")
        if length == 0:
            return None
        value = json.loads(self.rfile.read(length))
        if not isinstance(value, dict):
            raise ValueError("mock request body must be an object")
        return value


class GithubApiHandler(QuietHandler):
    server: ThreadingHTTPServer

    @property
    def state(self) -> GithubApiState:
        return getattr(self.server, "state")

    @property
    def redirect_url(self) -> str:
        return str(getattr(self.server, "redirect_url"))

    def record(self, body: dict[str, Any] | None) -> None:
        authorization = self.headers.get("authorization", "")
        self.state.append(
            RequestRecord(
                method=self.command,
                path=self.path,
                authorized=hmac.compare_digest(authorization, f"Bearer {ADMIN_TOKEN}"),
                accept=self.headers.get("accept", ""),
                api_version=self.headers.get("x-github-api-version", ""),
                user_agent=self.headers.get("user-agent", ""),
                body=body,
            )
        )

    def do_GET(self) -> None:  # noqa: N802 - stdlib handler contract
        self.record(None)
        if self.path != f"/repos/{FULL_NAME}":
            self.send_json(404, {"message": "Not Found"})
            return

        with self.state.lock:
            self.state.repository_lookups += 1
            lookup = self.state.repository_lookups
            mode = self.state.mode

        if mode == "redirect":
            self.send_response(302)
            self.send_header("location", self.redirect_url)
            self.send_header("cache-control", "no-store")
            self.end_headers()
        elif mode == "oversized":
            payload = repository_payload()
            payload["padding"] = "x" * (70 * 1024)
            self.send_json(200, payload)
        elif mode == "identity-mismatch":
            self.send_json(200, repository_payload(full_name="other-org/other-repository"))
        elif mode == "visibility-mismatch":
            self.send_json(200, repository_payload(private=False))
        elif mode == "existing":
            self.send_json(200, repository_payload())
        elif mode == "echo-token-error":
            self.send_json(500, {"message": f"credential {ADMIN_TOKEN}\nwas rejected"})
        elif mode == "race" and lookup >= 2:
            self.send_json(200, repository_payload())
        elif mode == "race-mismatch" and lookup >= 2:
            self.send_json(200, repository_payload(full_name="other-org/other-repository"))
        else:
            self.send_json(404, {"message": "Not Found"})

    def do_POST(self) -> None:  # noqa: N802 - stdlib handler contract
        try:
            body = self.read_json()
        except (ValueError, json.JSONDecodeError) as error:
            self.send_json(400, {"message": str(error)})
            return
        self.record(body)
        if self.path != f"/orgs/{ORG}/repos":
            self.send_json(404, {"message": "Not Found"})
            return

        mode, _, _ = self.state.snapshot()
        if mode in {"race", "race-mismatch"}:
            self.send_json(422, {"message": "name already exists on this account"})
        else:
            self.send_json(201, repository_payload())


class RedirectSinkHandler(QuietHandler):
    server: ThreadingHTTPServer

    @property
    def state(self) -> RedirectSinkState:
        return getattr(self.server, "state")

    def do_GET(self) -> None:  # noqa: N802 - stdlib handler contract
        self.state.record("authorization" in self.headers)
        self.send_json(200, repository_payload())


class ServerThread:
    def __init__(self, server: ThreadingHTTPServer) -> None:
        self.server = server
        self.thread = threading.Thread(target=server.serve_forever, daemon=True)

    def __enter__(self) -> ThreadingHTTPServer:
        self.thread.start()
        return self.server

    def __exit__(self, *_args: object) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def wait_for_health(base_url: str, process: subprocess.Popen[bytes], log_path: Path) -> None:
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if process.poll() is not None:
            tail = log_path.read_text(encoding="utf-8", errors="replace")[-4_000:]
            raise RuntimeError(f"coordinator exited before readiness\n{tail}")
        try:
            with urllib.request.urlopen(f"{base_url}/healthz", timeout=1) as response:
                if response.status == 200:
                    return
        except (OSError, urllib.error.URLError):
            time.sleep(0.2)
    raise TimeoutError("coordinator did not become ready within 30 seconds")


def write_config(destination: Path, port: int) -> None:
    source = (ROOT / "tests/browser/github-admin-coordinator.yaml").read_text(encoding="utf-8")
    expected = "bind: 127.0.0.1:8080"
    if source.count(expected) != 1:
        raise RuntimeError("browser coordinator fixture bind contract drifted")
    destination.write_text(source.replace(expected, f"bind: 127.0.0.1:{port}"), encoding="utf-8")


def browser_request(
    page: Page,
    body: dict[str, Any],
    *,
    token: str = API_TOKEN,
) -> dict[str, Any]:
    return page.evaluate(
        """
        async ({ token, body }) => {
          const response = await fetch('/v1/github/repositories', {
            method: 'POST',
            headers: {
              'authorization': `Bearer ${token}`,
              'content-type': 'application/json',
            },
            body: JSON.stringify(body),
          });
          const text = await response.text();
          let parsed;
          try {
            parsed = text ? JSON.parse(text) : null;
          } catch {
            parsed = { unparsed: text };
          }
          return {
            status: response.status,
            body: parsed,
            cacheControl: response.headers.get('cache-control'),
            requestId: response.headers.get('x-request-id'),
          };
        }
        """,
        {"token": token, "body": body},
    )


def request_body(*, dry_run: bool, confirmation: str | None = None) -> dict[str, Any]:
    body: dict[str, Any] = {
        "organization": ORG,
        "name": REPOSITORY,
        "visibility": "private",
        "initialization": "readme",
        "description": "Browser-verified repository administration contract",
        "dry_run": dry_run,
    }
    if confirmation is not None:
        body["confirm_repository"] = confirmation
    return body


def stop_process(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def close_browser_context(context: BrowserContext) -> None:
    try:
        context.tracing.stop(path=ARTIFACT_DIR / "trace.zip")
    finally:
        context.close()


class GithubRepositoryAdminBrowserTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        ARTIFACT_DIR.mkdir(parents=True, exist_ok=True)
        cls.temporary_directory = tempfile.TemporaryDirectory(prefix="github-admin-browser-")
        cls.addClassCleanup(cls.temporary_directory.cleanup)
        cls.temp = Path(cls.temporary_directory.name)

        binary = Path(
            os.environ.get(
                "COORDINATOR_BIN",
                ROOT / "target/debug/ai-agent-coordinator",
            )
        )
        if not binary.is_file():
            cls.temporary_directory.cleanup()
            raise FileNotFoundError(
                f"coordinator binary not found at {binary}; build it or set COORDINATOR_BIN"
            )

        cls.redirect_state = RedirectSinkState()
        redirect_server = ThreadingHTTPServer(("127.0.0.1", 0), RedirectSinkHandler)
        setattr(redirect_server, "state", cls.redirect_state)
        cls.redirect_server = ServerThread(redirect_server)
        cls.redirect_httpd = cls.redirect_server.__enter__()
        cls.addClassCleanup(cls.redirect_server.__exit__, None, None, None)
        redirect_port = int(cls.redirect_httpd.server_address[1])

        cls.github_state = GithubApiState()
        github_server = ThreadingHTTPServer(("127.0.0.1", 0), GithubApiHandler)
        setattr(github_server, "state", cls.github_state)
        setattr(github_server, "redirect_url", f"http://127.0.0.1:{redirect_port}/redirected")
        cls.github_server = ServerThread(github_server)
        cls.github_httpd = cls.github_server.__enter__()
        cls.addClassCleanup(cls.github_server.__exit__, None, None, None)
        github_port = int(cls.github_httpd.server_address[1])
        cls.github_base_url = f"http://127.0.0.1:{github_port}"

        coordinator_port = reserve_port()
        cls.base_url = f"http://127.0.0.1:{coordinator_port}"
        config_path = cls.temp / "coordinator.yaml"
        write_config(config_path, coordinator_port)

        cls.log_path = ARTIFACT_DIR / "coordinator.log"
        cls.log_file = cls.log_path.open("wb")
        cls.addClassCleanup(cls.log_file.close)
        environment = os.environ.copy()
        environment.update(
            {
                "COORDINATOR_API_TOKEN": API_TOKEN,
                "GITHUB_REPOSITORY_ADMIN_ENABLED": "true",
                "GITHUB_REPOSITORY_ADMIN_TOKEN": ADMIN_TOKEN,
                "GITHUB_REPOSITORY_ADMIN_ALLOWED_ORGS": ORG,
                "GITHUB_API_BASE_URL": cls.github_base_url,
                "GITHUB_API_USER_AGENT": "ai-agent-coordinator-browser-e2e",
                "LINEAR_DELIVERY_ENABLED": "false",
                "TELEMETRY_AUTOMATION_ENABLED": "false",
                "EMAIL_ATTENTION_ENABLED": "false",
                "RUST_LOG": "ai_agent_coordinator=info,tower_http=warn",
            }
        )
        cls.process = subprocess.Popen(
            [str(binary), "--config", str(config_path)],
            cwd=ROOT,
            env=environment,
            stdout=cls.log_file,
            stderr=subprocess.STDOUT,
        )
        cls.addClassCleanup(stop_process, cls.process)
        wait_for_health(cls.base_url, cls.process, cls.log_path)

        cls.playwright: Playwright = sync_playwright().start()
        cls.addClassCleanup(cls.playwright.stop)
        launch_options: dict[str, Any] = {"headless": True}
        executable = os.environ.get("PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH")
        if executable:
            launch_options["executable_path"] = executable
        cls.browser: Browser = cls.playwright.chromium.launch(**launch_options)
        cls.addClassCleanup(cls.browser.close)
        cls.context: BrowserContext = cls.browser.new_context()
        cls.addClassCleanup(close_browser_context, cls.context)
        cls.context.tracing.start(screenshots=True, snapshots=True, sources=True)
        cls.page: Page = cls.context.new_page()
        response = cls.page.goto(f"{cls.base_url}/healthz", wait_until="domcontentloaded")
        if response is None or response.status != 200:
            raise RuntimeError("Chromium could not navigate to the coordinator health endpoint")
        cls.health_navigation_status = response.status
        cls.health_navigation_headers = response.headers

    def setUp(self) -> None:
        self.github_state.reset("create")

    def assert_response_headers(self, result: dict[str, Any]) -> None:
        self.assertEqual(result["cacheControl"], "no-store")
        self.assertRegex(
            result["requestId"],
            r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
        )

    def test_repository_admin_route_through_chromium(self) -> None:
        self.assertEqual(self.health_navigation_status, 200)
        self.assertEqual(self.health_navigation_headers.get("cache-control"), "no-store")
        self.assertRegex(
            self.health_navigation_headers.get("x-request-id", ""),
            r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
        )

        unauthorized = browser_request(
            self.page,
            request_body(dry_run=True),
            token="wrong-api-token",
        )
        self.assertEqual(unauthorized["status"], 401)
        self.assertEqual(unauthorized["body"]["error"]["code"], "unauthorized")
        self.assert_response_headers(unauthorized)
        self.assertEqual(self.github_state.snapshot()[1], [])

        dry_run = browser_request(self.page, request_body(dry_run=True))
        self.assertEqual(dry_run["status"], 200)
        dry_run_repository = dry_run["body"]["repository"]
        self.assertTrue(dry_run_repository["dry_run"])
        self.assertFalse(dry_run_repository["created"])
        self.assertFalse(dry_run_repository["existing"])
        self.assert_response_headers(dry_run)
        self.assertEqual(self.github_state.snapshot()[1], [])

        invalid_confirmation = browser_request(
            self.page,
            request_body(dry_run=False, confirmation="wrong/repository"),
        )
        self.assertEqual(invalid_confirmation["status"], 400)
        self.assertEqual(invalid_confirmation["body"]["error"]["code"], "bad_request")
        self.assert_response_headers(invalid_confirmation)
        self.assertEqual(self.github_state.snapshot()[1], [])

        unlisted = request_body(dry_run=True)
        unlisted["organization"] = "unlisted-org"
        forbidden = browser_request(self.page, unlisted)
        self.assertEqual(forbidden["status"], 403)
        self.assertEqual(forbidden["body"]["error"]["code"], "forbidden")
        self.assert_response_headers(forbidden)
        self.assertEqual(self.github_state.snapshot()[1], [])

        created = browser_request(
            self.page,
            request_body(dry_run=False, confirmation=FULL_NAME),
        )
        self.assertEqual(created["status"], 201)
        created_repository = created["body"]["repository"]
        self.assertTrue(created_repository["created"])
        self.assertFalse(created_repository["existing"])
        self.assertEqual(created_repository["full_name"], FULL_NAME)
        self.assertEqual(
            created_repository["html_url"],
            f"{self.github_base_url}/{FULL_NAME}",
        )
        self.assertNotIn("untrusted.invalid", created_repository["html_url"])
        self.assert_response_headers(created)
        _, create_records, _ = self.github_state.snapshot()
        self.assertEqual([(record.method, record.path) for record in create_records], [
            ("GET", f"/repos/{FULL_NAME}"),
            ("POST", f"/orgs/{ORG}/repos"),
        ])
        self.assertTrue(all(record.authorized for record in create_records))
        self.assertTrue(
            all(record.accept == "application/vnd.github+json" for record in create_records)
        )
        self.assertTrue(all(record.api_version == "2022-11-28" for record in create_records))
        self.assertTrue(
            all(
                record.user_agent == "ai-agent-coordinator-browser-e2e"
                for record in create_records
            )
        )
        create_body = create_records[-1].body
        self.assertIsNotNone(create_body)
        assert create_body is not None
        self.assertEqual(create_body["name"], REPOSITORY)
        self.assertEqual(
            create_body["description"],
            "Browser-verified repository administration contract",
        )
        self.assertEqual(create_body["visibility"], "private")
        self.assertTrue(create_body["auto_init"])
        self.assertTrue(create_body["has_issues"])
        self.assertFalse(create_body["has_projects"])
        self.assertFalse(create_body["has_wiki"])
        self.assertTrue(create_body["allow_squash_merge"])
        self.assertTrue(create_body["allow_merge_commit"])
        self.assertFalse(create_body["allow_rebase_merge"])

        self.github_state.reset("existing")
        existing = browser_request(
            self.page,
            request_body(dry_run=False, confirmation=FULL_NAME),
        )
        self.assertEqual(existing["status"], 200)
        self.assertFalse(existing["body"]["repository"]["created"])
        self.assertTrue(existing["body"]["repository"]["existing"])
        self.assert_response_headers(existing)
        _, existing_records, existing_lookups = self.github_state.snapshot()
        self.assertEqual(existing_lookups, 1)
        self.assertEqual([record.method for record in existing_records], ["GET"])

        self.github_state.reset("race")
        raced = browser_request(
            self.page,
            request_body(dry_run=False, confirmation=FULL_NAME),
        )
        self.assertEqual(raced["status"], 200)
        raced_repository = raced["body"]["repository"]
        self.assertFalse(raced_repository["created"])
        self.assertTrue(raced_repository["existing"])
        self.assertEqual(raced_repository["full_name"], FULL_NAME)
        self.assert_response_headers(raced)
        _, race_records, lookups = self.github_state.snapshot()
        self.assertEqual(lookups, 2)
        self.assertEqual([record.method for record in race_records], ["GET", "POST", "GET"])

        self.github_state.reset("race-mismatch")
        race_mismatch = browser_request(
            self.page,
            request_body(dry_run=False, confirmation=FULL_NAME),
        )
        self.assertEqual(race_mismatch["status"], 400)
        self.assertEqual(race_mismatch["body"]["error"]["code"], "bad_request")
        self.assertIn("name already exists", race_mismatch["body"]["error"]["message"])
        self.assertNotIn("other-org", race_mismatch["body"]["error"]["message"])
        self.assert_response_headers(race_mismatch)
        _, race_mismatch_records, race_mismatch_lookups = self.github_state.snapshot()
        self.assertEqual(race_mismatch_lookups, 2)
        self.assertEqual(
            [record.method for record in race_mismatch_records],
            ["GET", "POST", "GET"],
        )

        self.github_state.reset("identity-mismatch")
        identity_mismatch = browser_request(
            self.page,
            request_body(dry_run=False, confirmation=FULL_NAME),
        )
        self.assertEqual(identity_mismatch["status"], 502)
        self.assertEqual(identity_mismatch["body"]["error"]["code"], "upstream_failure")
        self.assert_response_headers(identity_mismatch)

        self.github_state.reset("visibility-mismatch")
        visibility_mismatch = browser_request(
            self.page,
            request_body(dry_run=False, confirmation=FULL_NAME),
        )
        self.assertEqual(visibility_mismatch["status"], 400)
        self.assertEqual(visibility_mismatch["body"]["error"]["code"], "bad_request")
        self.assert_response_headers(visibility_mismatch)

        self.github_state.reset("oversized")
        oversized = browser_request(
            self.page,
            request_body(dry_run=False, confirmation=FULL_NAME),
        )
        self.assertEqual(oversized["status"], 502)
        self.assertEqual(oversized["body"]["error"]["code"], "upstream_failure")
        self.assert_response_headers(oversized)

        self.github_state.reset("echo-token-error")
        redacted = browser_request(
            self.page,
            request_body(dry_run=False, confirmation=FULL_NAME),
        )
        self.assertEqual(redacted["status"], 502)
        self.assertEqual(redacted["body"]["error"]["code"], "upstream_failure")
        redacted_message = redacted["body"]["error"]["message"]
        self.assertNotIn(ADMIN_TOKEN, redacted_message)
        self.assertIn("[REDACTED]", redacted_message)
        self.assertNotIn("\n", redacted_message)
        self.assert_response_headers(redacted)

        self.github_state.reset("redirect")
        redirected = browser_request(
            self.page,
            request_body(dry_run=False, confirmation=FULL_NAME),
        )
        self.assertEqual(redirected["status"], 502)
        self.assertEqual(redirected["body"]["error"]["code"], "upstream_failure")
        self.assert_response_headers(redirected)
        self.assertEqual(self.redirect_state.snapshot(), (0, False))

        self.page.screenshot(path=ARTIFACT_DIR / "repository-admin-contract.png", full_page=True)


if __name__ == "__main__":
    unittest.main(verbosity=2)
