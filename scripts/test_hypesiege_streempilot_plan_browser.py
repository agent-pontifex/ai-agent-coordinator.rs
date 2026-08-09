from __future__ import annotations

import base64
import hashlib
import importlib.util
import json
import os
import pathlib
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / "repository-fleets" / "hypesiege-streempilot.json"
RENDERER_PATH = ROOT / "scripts" / "render_hypesiege_streempilot_plan.py"
MAX_DRIVER_RESPONSE_BYTES = 8 * 1024 * 1024


def load_module(name: str, path: pathlib.Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


RENDERER = load_module("fleet_plan_renderer", RENDERER_PATH)


class StrictPlanHandler(BaseHTTPRequestHandler):
    plan: bytes = b""
    requests: list[str] = []

    def log_message(self, format: str, *args) -> None:  # noqa: A002, ANN002
        return

    def do_GET(self) -> None:  # noqa: N802
        type(self).requests.append(self.path)
        if self.path == "/plan.html":
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(self.plan)))
            self.send_header("Content-Security-Policy", RENDERER.HEADER_CSP)
            self.send_header("Cache-Control", "no-store")
            self.send_header("Referrer-Policy", "no-referrer")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("Cross-Origin-Opener-Policy", "same-origin")
            self.send_header("Cross-Origin-Resource-Policy", "same-origin")
            self.end_headers()
            self.wfile.write(self.plan)
            return
        if self.path == "/favicon.ico":
            self.send_response(204)
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            return
        self.send_response(404)
        self.send_header("Cache-Control", "no-store")
        self.end_headers()


class WebDriverClient:
    def __init__(self, executable: str, chrome: str, profile: pathlib.Path) -> None:
        self.executable = executable
        self.chrome = chrome
        self.profile = profile
        self.process: subprocess.Popen[str] | None = None
        self.base_url = ""
        self.session_id = ""
        self.capabilities: dict[str, Any] = {}

    @staticmethod
    def _free_port() -> int:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
            listener.bind(("127.0.0.1", 0))
            return int(listener.getsockname()[1])

    def _request(
        self,
        method: str,
        path: str,
        body: dict[str, Any] | None = None,
    ) -> Any:
        data = None if body is None else json.dumps(body).encode("utf-8")
        request = urllib.request.Request(
            self.base_url + path,
            data=data,
            method=method,
            headers={"Content-Type": "application/json; charset=utf-8"},
        )
        try:
            with urllib.request.urlopen(request, timeout=20) as response:
                raw = response.read(MAX_DRIVER_RESPONSE_BYTES + 1)
        except urllib.error.HTTPError as error:
            detail = error.read(16 * 1024).decode("utf-8", errors="replace")
            raise AssertionError(
                f"WebDriver {method} {path} returned {error.code}: {detail}"
            ) from error
        if len(raw) > MAX_DRIVER_RESPONSE_BYTES:
            raise AssertionError("WebDriver response exceeded 8 MiB")
        payload = json.loads(raw) if raw else {"value": None}
        if not isinstance(payload, dict) or "value" not in payload:
            raise AssertionError(f"malformed WebDriver response for {method} {path}")
        value = payload["value"]
        if isinstance(value, dict) and value.get("error"):
            raise AssertionError(
                f"WebDriver {method} {path} failed: "
                f"{value.get('error')}: {value.get('message')}"
            )
        return value

    def start(self) -> None:
        port = self._free_port()
        self.base_url = f"http://127.0.0.1:{port}"
        self.process = subprocess.Popen(
            [
                self.executable,
                f"--port={port}",
                "--allowed-ips=127.0.0.1",
                "--log-level=WARNING",
            ],
            cwd=ROOT,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                stdout, stderr = self.process.communicate(timeout=5)
                raise AssertionError(
                    "ChromeDriver exited before becoming ready:\n"
                    f"{stdout[-4000:]}\n{stderr[-4000:]}"
                )
            try:
                status = self._request("GET", "/status")
                if isinstance(status, dict) and status.get("ready") is True:
                    break
            except (OSError, urllib.error.URLError, AssertionError, json.JSONDecodeError):
                time.sleep(0.1)
        else:
            raise AssertionError("ChromeDriver did not become ready")

        value = self._request(
            "POST",
            "/session",
            {
                "capabilities": {
                    "alwaysMatch": {
                        "browserName": "chrome",
                        "acceptInsecureCerts": False,
                        "goog:loggingPrefs": {
                            "browser": "ALL",
                            "performance": "ALL",
                        },
                        "goog:chromeOptions": {
                            "binary": self.chrome,
                            "args": [
                                "--headless=new",
                                "--no-sandbox",
                                "--disable-gpu",
                                "--disable-dev-shm-usage",
                                "--disable-background-networking",
                                "--disable-component-update",
                                "--disable-default-apps",
                                "--disable-extensions",
                                "--disable-sync",
                                "--metrics-recording-only",
                                "--no-default-browser-check",
                                "--no-first-run",
                                "--safebrowsing-disable-auto-update",
                                "--host-resolver-rules=MAP * 0.0.0.0, EXCLUDE 127.0.0.1",
                                f"--user-data-dir={self.profile}",
                                "--window-size=1440,1200",
                            ],
                        },
                    }
                }
            },
        )
        if not isinstance(value, dict):
            raise AssertionError("ChromeDriver did not return session metadata")
        session_id = value.get("sessionId")
        capabilities = value.get("capabilities")
        if not isinstance(session_id, str) or not session_id:
            raise AssertionError("ChromeDriver did not return a session ID")
        if not isinstance(capabilities, dict):
            raise AssertionError("ChromeDriver did not return capabilities")
        self.session_id = session_id
        self.capabilities = capabilities

    def command(
        self,
        method: str,
        suffix: str,
        body: dict[str, Any] | None = None,
    ) -> Any:
        if not self.session_id:
            raise AssertionError("WebDriver session is not active")
        return self._request(
            method,
            f"/session/{self.session_id}{suffix}",
            body,
        )

    def navigate(self, url: str) -> None:
        self.command("POST", "/url", {"url": url})
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.execute("return document.readyState") == "complete":
                return
            time.sleep(0.05)
        raise AssertionError("browser document did not reach complete state")

    def execute(self, script: str) -> Any:
        return self.command(
            "POST",
            "/execute/sync",
            {"script": script, "args": []},
        )

    def logs(self, log_type: str) -> list[dict[str, Any]]:
        value = self.command("POST", "/log", {"type": log_type})
        if not isinstance(value, list):
            raise AssertionError(f"WebDriver returned malformed {log_type} logs")
        return [entry for entry in value if isinstance(entry, dict)]

    def screenshot(self) -> bytes:
        value = self.command("GET", "/screenshot")
        if not isinstance(value, str):
            raise AssertionError("WebDriver returned a malformed screenshot")
        return base64.b64decode(value, validate=True)

    def set_window(self, width: int, height: int) -> None:
        self.command(
            "POST",
            "/window/rect",
            {"x": 0, "y": 0, "width": width, "height": height},
        )

    def close(self) -> None:
        if self.session_id:
            try:
                self._request("DELETE", f"/session/{self.session_id}")
            except Exception:
                pass
            self.session_id = ""
        if self.process is not None:
            self.process.terminate()
            try:
                self.process.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.communicate(timeout=5)
            self.process = None


class BrowserPlanTests(unittest.TestCase):
    def find_binary(self, environment_name: str, candidates: tuple[str, ...]) -> str:
        configured = os.environ.get(environment_name)
        if configured:
            path = pathlib.Path(configured).resolve()
            self.assertTrue(path.is_file(), f"{environment_name} does not exist: {path}")
            return str(path)
        for candidate in candidates:
            located = shutil.which(candidate)
            if located:
                return str(pathlib.Path(located).resolve())
        self.fail(f"{environment_name} or one of {candidates!r} is required")

    def test_atomic_writer_rejects_symbolic_link_output(self) -> None:
        with tempfile.TemporaryDirectory(prefix="fleet-plan-symlink-") as temp:
            root = pathlib.Path(temp)
            target = root / "target.html"
            target.write_text("untouched", encoding="utf-8")
            link = root / "plan.html"
            link.symlink_to(target)
            with self.assertRaisesRegex(RuntimeError, "symbolic-link"):
                RENDERER.atomic_write(link, "replacement")
            self.assertEqual(target.read_text(encoding="utf-8"), "untouched")

    def test_operator_plan_in_a_real_browser(self) -> None:
        chrome = self.find_binary(
            "CHROME_BIN",
            ("google-chrome", "google-chrome-stable", "chromium", "chromium-browser"),
        )
        chromedriver = self.find_binary("CHROMEDRIVER_BIN", ("chromedriver",))
        manifest = RENDERER.PUBLISHER.load_manifest(MANIFEST_PATH)
        artifact_root = os.environ.get("BROWSER_ARTIFACT_DIR")

        with tempfile.TemporaryDirectory(prefix="fleet-browser-") as temp:
            temporary = pathlib.Path(temp)
            plan_path = temporary / "plan.html"
            RENDERER.atomic_write(plan_path, RENDERER.render_plan(manifest))
            StrictPlanHandler.plan = plan_path.read_bytes()
            StrictPlanHandler.requests = []
            server = ThreadingHTTPServer(("127.0.0.1", 0), StrictPlanHandler)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            driver = WebDriverClient(chromedriver, chrome, temporary / "profile")
            try:
                driver.start()
                # Chrome may emit startup events before the test target exists.
                # Drain those entries so the network assertion covers only the
                # operator plan and cannot be polluted by browser initialization.
                driver.logs("performance")
                driver.logs("browser")
                url = f"http://127.0.0.1:{server.server_port}/plan.html"
                driver.navigate(url)
                snapshot = driver.execute(
                    """
                    const ids = {};
                    for (const element of document.querySelectorAll('[id]')) {
                      ids[element.id] = element.textContent.trim();
                    }
                    const allIds = [...document.querySelectorAll('[id]')].map((node) => node.id);
                    const tableRegion = document.querySelector('.table-scroll');
                    return {
                      ready: document.readyState,
                      title: document.title,
                      language: document.documentElement.lang,
                      digest: document.documentElement.dataset.fleetDigest,
                      ids,
                      uniqueIds: new Set(allIds).size === allIds.length,
                      repositories: [...document.querySelectorAll('[data-repository]')].map(
                        (node) => node.dataset.repository
                      ),
                      forbiddenCount: document.querySelectorAll(
                        'script, form, iframe, object, embed, '
                        + 'link[rel=stylesheet], img, video, audio'
                      ).length,
                      headingCount: document.querySelectorAll('h1').length,
                      columnHeaderCount: document.querySelectorAll('thead th[scope=col]').length,
                      rowHeaderCount: document.querySelectorAll('tbody th[scope=row]').length,
                      metaCsp: document.querySelector(
                        'meta[http-equiv="Content-Security-Policy"]'
                      ).content,
                      styleApplied: getComputedStyle(
                        document.querySelector('.notice')
                      ).borderTopStyle,
                      globalOverflow:
                        document.documentElement.scrollWidth
                        - document.documentElement.clientWidth,
                      tableRegionLabel: tableRegion.getAttribute('aria-label'),
                      webdriver: navigator.webdriver,
                    };
                    """
                )
                self.assertIsInstance(snapshot, dict)
                expected_repositories = [
                    record["full_name"] for record in manifest["repositories"]
                ]
                self.assertEqual(snapshot["ready"], "complete")
                self.assertEqual(
                    snapshot["title"],
                    "HypeSiege and StreemPilot publication plan",
                )
                self.assertEqual(snapshot["language"], "en")
                self.assertTrue(snapshot["uniqueIds"])
                self.assertTrue(snapshot["webdriver"])
                self.assertEqual(snapshot["forbiddenCount"], 0)
                self.assertEqual(snapshot["headingCount"], 1)
                self.assertEqual(snapshot["columnHeaderCount"], 7)
                self.assertEqual(snapshot["rowHeaderCount"], 32)
                self.assertEqual(snapshot["repositories"], expected_repositories)
                self.assertEqual(snapshot["ids"]["repository-count"], "32")
                self.assertEqual(snapshot["ids"]["tracked-file-count"], "888")
                self.assertEqual(snapshot["ids"]["gitlink-count"], "30")
                self.assertEqual(snapshot["ids"]["hypesiege-count"], "15")
                self.assertEqual(snapshot["ids"]["streempilot-count"], "17")
                self.assertEqual(snapshot["metaCsp"], RENDERER.META_CSP)
                self.assertEqual(snapshot["styleApplied"], "solid")
                self.assertLessEqual(snapshot["globalOverflow"], 1)
                self.assertEqual(
                    snapshot["tableRegionLabel"],
                    "Repository publication order",
                )
                expected_digest = RENDERER.canonical_manifest_digest(manifest)
                self.assertEqual(snapshot["digest"], expected_digest)
                self.assertEqual(snapshot["ids"]["manifest-digest"], expected_digest)

                driver.set_window(390, 844)
                mobile = driver.execute(
                    """
                    const region = document.querySelector('.table-scroll');
                    return {
                      globalOverflow:
                        document.documentElement.scrollWidth
                        - document.documentElement.clientWidth,
                      regionScrollable: region.scrollWidth > region.clientWidth,
                      viewportWidth: document.documentElement.clientWidth,
                    };
                    """
                )
                self.assertLessEqual(mobile["globalOverflow"], 1)
                self.assertTrue(mobile["regionScrollable"])
                self.assertLessEqual(mobile["viewportWidth"], 390)

                driver.set_window(1440, 1200)
                screenshot = driver.screenshot()
                self.assertTrue(screenshot.startswith(b"\x89PNG\r\n\x1a\n"))
                self.assertGreater(len(screenshot), 10_000)

                performance_logs = driver.logs("performance")
                requests: list[str] = []
                plan_headers: dict[str, str] = {}
                for entry in performance_logs:
                    message = entry.get("message")
                    if not isinstance(message, str):
                        continue
                    outer = json.loads(message)
                    event = outer.get("message", {})
                    method = event.get("method")
                    params = event.get("params", {})
                    if method == "Network.requestWillBeSent":
                        request_url = params.get("request", {}).get("url")
                        if isinstance(request_url, str):
                            requests.append(request_url)
                    if method == "Network.responseReceived":
                        response = params.get("response", {})
                        if response.get("url") == url:
                            headers = response.get("headers", {})
                            if isinstance(headers, dict):
                                plan_headers = {
                                    str(key).casefold(): str(value)
                                    for key, value in headers.items()
                                }
                self.assertTrue(requests)
                allowed_prefix = f"http://127.0.0.1:{server.server_port}/"
                unexpected = [
                    request
                    for request in requests
                    if not request.startswith(allowed_prefix)
                    and not request.startswith(("data:", "blob:", "about:"))
                ]
                self.assertEqual(unexpected, [])
                self.assertEqual(
                    plan_headers.get("content-security-policy"),
                    RENDERER.HEADER_CSP,
                )
                self.assertEqual(plan_headers.get("cache-control"), "no-store")
                self.assertEqual(plan_headers.get("referrer-policy"), "no-referrer")
                self.assertEqual(
                    plan_headers.get("x-content-type-options"),
                    "nosniff",
                )
                self.assertEqual(
                    plan_headers.get("cross-origin-opener-policy"),
                    "same-origin",
                )
                self.assertEqual(
                    plan_headers.get("cross-origin-resource-policy"),
                    "same-origin",
                )

                browser_logs = driver.logs("browser")
                severe = [
                    entry
                    for entry in browser_logs
                    if str(entry.get("level", "")).upper() == "SEVERE"
                ]
                self.assertEqual(severe, [])

                if artifact_root:
                    output = pathlib.Path(artifact_root)
                    output.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(plan_path, output / "plan.html")
                    (output / "plan.png").write_bytes(screenshot)
                    report = {
                        "browserName": driver.capabilities.get("browserName"),
                        "browserVersion": driver.capabilities.get("browserVersion"),
                        "chromedriverVersion": driver.capabilities.get(
                            "chrome", {}
                        ).get("chromedriverVersion"),
                        "manifestDigest": expected_digest,
                        "repositoryCount": len(expected_repositories),
                        "networkRequests": sorted(set(requests)),
                        "screenshotSha256": hashlib.sha256(screenshot).hexdigest(),
                    }
                    (output / "browser-report.json").write_text(
                        json.dumps(report, indent=2, sort_keys=True) + "\n",
                        encoding="utf-8",
                    )
            finally:
                driver.close()
                server.shutdown()
                thread.join(timeout=5)
                server.server_close()

            self.assertTrue(StrictPlanHandler.requests)
            self.assertTrue(
                set(StrictPlanHandler.requests).issubset(
                    {"/plan.html", "/favicon.ico"}
                ),
                StrictPlanHandler.requests,
            )


if __name__ == "__main__":
    unittest.main()
