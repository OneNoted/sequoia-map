# /// script
# requires-python = ">=3.13"
# dependencies = ["playwright==1.62.0"]
# ///
"""Smoke-test a running `mise run dev:full` stack, including shared claims components."""

import json
import os
from pathlib import Path
import subprocess
import sys

from playwright.sync_api import Error, sync_playwright


def main() -> int:
    if sys.argv[1:] == ["--install"]:
        return subprocess.call([sys.executable, "-m", "playwright", "install", "chromium"])
    if sys.argv[1:]:
        raise SystemExit("Usage: mise run smoke | mise run smoke:install")

    output = Path(".data/browser-smoke")
    output.mkdir(parents=True, exist_ok=True)
    cases = [
        ("map", "http://127.0.0.1:8081/"),
        ("claims", "http://127.0.0.1:8082/claims/new/blank"),
        ("claims-via-map", "http://127.0.0.1:8081/claims/new/blank"),
    ]
    results = []
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(
            executable_path=os.environ.get("SEQUOIA_BROWSER_EXECUTABLE"),
            headless=True,
            # Software rendering makes this a functional test, not a GPU benchmark.
            args=["--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"],
        )
        for name, url in cases:
            page = browser.new_page(viewport={"width": 1440, "height": 1000})
            errors = []
            page.on("pageerror", lambda error: errors.append(str(error)))
            page.on("console", lambda message: errors.append(message.text) if message.type == "error" else None)
            page.on("response", lambda response: errors.append(f"HTTP {response.status}: {response.url}") if response.status >= 400 else None)
            try:
                response = page.goto(url, wait_until="domcontentloaded", timeout=60000)
                if response.status != 200:
                    errors.append(f"Navigation returned {response.status}")
                page.locator("canvas").first.wait_for(state="visible", timeout=60000)
                # Allow async renderer initialization and initial API requests to settle.
                page.wait_for_timeout(3000)
            except Error as error:
                errors.append(str(error))
            page.screenshot(path=str(output / f"{name}.png"))
            results.append({"name": name, "url": url, "errors": errors})
            page.close()
        browser.close()

    report = json.dumps(results, indent=2)
    (output / "results.json").write_text(report + "\n")
    print(report)
    return int(any(result["errors"] for result in results))


if __name__ == "__main__":
    raise SystemExit(main())
