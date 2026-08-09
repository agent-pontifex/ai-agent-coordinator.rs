from __future__ import annotations

import os
import pathlib
import re
import unittest

from playwright.sync_api import sync_playwright

REPORT = pathlib.Path(
    os.environ.get(
        "DEN877_AUDIT_REPORT",
        "artifacts/hypesiege-streempilot-audit-report.html",
    )
).resolve()
SCREENSHOTS = pathlib.Path(
    os.environ.get("DEN877_BROWSER_ARTIFACTS", "artifacts/browser")
).resolve()
SECRET_RE = re.compile(r"(?:github_pat_|gh[pousr]_)[A-Za-z0-9_]+")


class FleetAuditBrowserTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if not REPORT.is_file():
            raise AssertionError(f"missing rendered report: {REPORT}")
        SCREENSHOTS.mkdir(parents=True, exist_ok=True)
        cls.report_html = REPORT.read_text(encoding="utf-8")
        cls.playwright = sync_playwright().start()
        executable = os.environ.get("DEN877_CHROMIUM_EXECUTABLE")
        options = {"headless": True}
        if executable:
            options["executable_path"] = executable
        cls.browser = cls.playwright.chromium.launch(**options)

    @classmethod
    def tearDownClass(cls) -> None:
        cls.browser.close()
        cls.playwright.stop()

    def open_page(self, *, width: int, height: int):
        context = self.browser.new_context(
            viewport={"width": width, "height": height},
            color_scheme="light",
            reduced_motion="reduce",
        )
        page = context.new_page()
        requests: list[str] = []
        console_errors: list[str] = []
        page.on("request", lambda request: requests.append(request.url))
        page.on(
            "console",
            lambda message: console_errors.append(message.text)
            if message.type == "error"
            else None,
        )
        page.set_content(self.report_html, wait_until="load")
        return context, page, requests, console_errors

    def test_desktop_report_is_complete_static_and_credential_free(self) -> None:
        context, page, requests, console_errors = self.open_page(width=1440, height=1000)
        try:
            self.assertEqual(page.title(), "DEN-877 repository fleet audit")
            self.assertIn("HypeSiege and StreemPilot", page.locator("h1").inner_text())
            main = page.locator("main")
            self.assertEqual(main.get_attribute("data-repository-count"), "32")
            self.assertEqual(main.get_attribute("data-tracked-files"), "888")
            self.assertEqual(main.get_attribute("data-gitlinks"), "30")
            self.assertRegex(
                main.get_attribute("data-manifest-sha256") or "",
                r"^[0-9a-f]{64}$",
            )
            rows = page.locator("tbody tr")
            self.assertEqual(rows.count(), 32)
            self.assertEqual(page.locator('tbody tr[data-kind="monorepo"]').count(), 2)
            self.assertEqual(page.locator("tbody a").count(), 32)
            self.assertEqual(page.locator("script").count(), 0)
            self.assertEqual(page.locator("img").count(), 0)
            self.assertEqual(console_errors, [])
            self.assertEqual(requests, [])
            text = page.locator("body").inner_text()
            self.assertIsNone(SECRET_RE.search(text))
            self.assertNotIn("GITHUB_REPOSITORY_ADMIN_TOKEN", text)
            self.assertIn("does not claim remote publication", text)
            for index in range(rows.count()):
                link = rows.nth(index).locator("a")
                self.assertRegex(
                    link.get_attribute("href") or "",
                    r"^https://github\.com/(?:hypesiege|streempilot)/[a-z0-9._-]+\.git$",
                )
                self.assertEqual(link.get_attribute("rel"), "noopener noreferrer")
            page.screenshot(
                path=str(SCREENSHOTS / "den877-audit-desktop.png"),
                full_page=True,
            )
        finally:
            context.close()

    def test_mobile_layout_preserves_boundary_and_scrollable_table(self) -> None:
        context, page, requests, console_errors = self.open_page(width=390, height=844)
        try:
            self.assertEqual(console_errors, [])
            self.assertEqual(requests, [])
            boundary = page.locator(".boundary")
            self.assertTrue(boundary.is_visible())
            self.assertIn("one explicitly confirmed repository", boundary.inner_text())
            table_wrap = page.locator(".table-wrap")
            measurements = table_wrap.evaluate(
                "element => ({clientWidth: element.clientWidth, scrollWidth: element.scrollWidth})"
            )
            self.assertGreater(measurements["scrollWidth"], measurements["clientWidth"])
            self.assertLessEqual(
                page.evaluate("document.documentElement.scrollWidth"),
                page.evaluate("window.innerWidth"),
            )
            page.screenshot(
                path=str(SCREENSHOTS / "den877-audit-mobile.png"),
                full_page=True,
            )
        finally:
            context.close()


if __name__ == "__main__":
    unittest.main()
