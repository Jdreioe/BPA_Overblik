from __future__ import annotations

from importlib.resources import files
from typing import Any, Protocol
from urllib.parse import urlsplit

REQUEST_SCRIPT = files("teamup_shift_sync").joinpath("browser_request.js").read_text()


class DestinationError(RuntimeError):
    """A destination error that is safe to print without account data."""


class DestinationTransport(Protocol):
    def request(self, system: str, action: str, payload: dict[str, Any]) -> Any: ...


class BrowserTransport:
    """Attach to existing authenticated Chromium tabs without exporting sessions."""

    def __init__(self, endpoint: str):
        self.endpoint = endpoint

    def __enter__(self):
        try:
            from playwright.sync_api import Error as PlaywrightError
            from playwright.sync_api import sync_playwright
        except ImportError:
            raise DestinationError(
                "Install the browser extra: pip install -e '.[browser]'"
            ) from None
        self.runtime = sync_playwright().start()
        self.browser_error = PlaywrightError
        try:
            self.browser = self.runtime.chromium.connect_over_cdp(
                self.endpoint, timeout=15000
            )
            self.pages = {}
            for system, host in (
                ("mithf", "mithf.handicapformidlingen.dk"),
                ("duos", "mit.duos.dk"),
            ):
                pages = [
                    page
                    for context in self.browser.contexts
                    for page in context.pages
                    if urlsplit(page.url).hostname == host
                    and urlsplit(page.url).scheme == "https"
                ]
                if len(pages) != 1:
                    raise DestinationError(
                        f"Open exactly one authenticated {system} tab"
                    )
                self.pages[system] = pages[0]
        except DestinationError:
            self.runtime.stop()
            raise
        except PlaywrightError:
            self.runtime.stop()
            raise DestinationError(
                "Could not attach to authenticated destination tabs through CDP"
            ) from None
        return self

    def __exit__(self, *_):
        # Stop the client connection, not the user's browser or tabs.
        self.runtime.stop()

    def request(self, system: str, action: str, payload: dict[str, Any]) -> Any:
        try:
            result = self.pages[system].evaluate(
                REQUEST_SCRIPT, {"system": system, "action": action, "payload": payload}
            )
        except self.browser_error:
            raise DestinationError(
                "Browser request failed; reconcile before retrying"
            ) from None
        if not isinstance(result, dict) or "data" not in result:
            raise DestinationError(
                result.get("error", "Invalid browser response")
                if isinstance(result, dict)
                else "Invalid browser response"
            )
        return result["data"]
