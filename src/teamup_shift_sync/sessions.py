"""Own destination browsers on one thread; expose only safe session status."""

from __future__ import annotations

import os
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from contextlib import suppress
from pathlib import Path
from threading import Event, Lock
from urllib.parse import urlsplit

from .browser import REQUEST_SCRIPT, DestinationError


class BrowserUnavailable(Exception):
    """The app's own browser is missing and could not be prepared.

    Kept separate from an ordinary open failure: telling the user to close
    other windows is useless when the browser itself is not there.
    """


SERVICES = {
    "mithf": "https://mithf.handicapformidlingen.dk/index.html",
    "duos": "https://mit.duos.dk/usage/timeregistrations",
}

# Opening /vagtplan/index.php directly shows MitHF's "Gå til BPA-universet"
# interstitial instead of the calendar: entry requires the site's own
# "Åbn din vagtplan" action, which POSTs a single-use billet. Never goto()
# the calendar URL; click the site button so the ticket exchange runs.
ENTER_VAGTPLAN_SCRIPT = """() => {
  const visible = (el) => {
    if (!el || el.disabled) return false;
    try {
      if (!el.getClientRects || !el.getClientRects().length) return false;
      const style = getComputedStyle(el);
      if (style.visibility === 'hidden' || style.display === 'none') return false;
    } catch (e) { return false; }
    return true;
  };
  const selectors = ['#dsbVpKn', 'a[data-dsb="vagtplan"]', '#vagtplanFlise', '#vagtplanNav'];
  for (const selector of selectors) {
    const el = document.querySelector(selector);
    if (el && visible(el)) { el.click(); return true; }
  }
  const candidates = document.querySelectorAll('button, a');
  for (const el of candidates) {
    if ((el.textContent || '').includes('Åbn din vagtplan') && visible(el)) {
      el.click();
      return true;
    }
  }
  return false;
}"""


class BrowserSessions:
    def __init__(self, data_dir: Path):
        self.data_dir = data_dir
        self.executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="browser")
        self.lock = Lock()
        self.closing = Event()
        self.restore_pending = {
            service
            for service in SERVICES
            if (data_dir / "profiles" / service / "Default").is_dir()
        }
        self.states = {
            s: {
                "state": "connecting"
                if s in self.restore_pending
                else "sign_in_required",
                "message": "Kontrollerer gemt login …"
                if s in self.restore_pending
                else "Log ind for at kontrollere forbindelsen.",
            }
            for s in SERVICES
        }
        self.busy: set[str] = set()
        self.contexts = {}
        self.pages = {}
        self.headless = set()
        self.runtime = None

    def snapshot(self):
        with self.lock:
            return {s: dict(value) for s, value in self.states.items()}

    def _set(self, service, state, message):
        with self.lock:
            self.states[service] = {"state": state, "message": message}

    def submit(self, service: str, *, login: bool):
        if service not in SERVICES:
            raise ValueError("Unknown service")
        with self.lock:
            if service in self.busy:
                return
            self.busy.add(service)
            if login:
                self.states[service] = {
                    "state": "connecting",
                    "message": "Åbner login …",
                }
        self.executor.submit(self._run, service, login)

    def _run(self, service, login):
        try:
            if self.closing.is_set():
                return
            restore = service in self.restore_pending
            self.restore_pending.discard(service)
            if login:
                self._open(service)
            elif restore:
                self._open(service, headless=True)
            self._check(service)
        except BrowserUnavailable as error:
            self._report(service, error)
            self._set(
                service,
                "unavailable",
                "Appens egen browser mangler og kunne ikke hentes. Kontrollér internetforbindelsen, og prøv igen.",
            )
        except Exception as error:  # noqa: BLE001 - isolate failures and redact browser diagnostics
            # Playwright exceptions can contain URLs, DOM text and credentials.
            self._report(service, error)
            self._set(
                service,
                "unavailable",
                "Browseren eller tjenesten kunne ikke åbnes. Luk eventuelle andre app-vinduer, og prøv igen.",
            )
        finally:
            with self.lock:
                self.busy.discard(service)

    @staticmethod
    def _report(service, error: BaseException) -> None:
        """Name the failure on stderr for Help diagnostics.

        Only the service and the exception type: Playwright messages can
        carry capability URLs, DOM text and credentials.
        """
        cause = error.__cause__ or error
        print(f"session error [{service}] {type(cause).__name__}", file=sys.stderr)

    def _start(self, service):
        if self.runtime is not None:
            return
        try:
            from playwright.sync_api import sync_playwright
        except ImportError as error:
            raise BrowserUnavailable("browser runtime is not installed") from error

        self.data_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        if os.name != "nt":
            self.data_dir.chmod(0o700)
        browser_dir = self.data_dir / "browsers"
        browser_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        os.environ["PLAYWRIGHT_BROWSERS_PATH"] = str(browser_dir)
        runtime = sync_playwright().start()
        try:
            if not Path(runtime.chromium.executable_path).is_file():
                self._set(
                    service,
                    "connecting",
                    "Henter browser … Første gang kan det tage et par minutter.",
                )
                command = (
                    [sys.executable, "--install-browser"]
                    if getattr(sys, "frozen", False)
                    else [
                        sys.executable,
                        "-m",
                        "playwright",
                        "install",
                        "chromium",
                        "--no-shell",
                    ]
                )
                try:
                    subprocess.run(
                        command,
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                        check=True,
                        timeout=600,
                    )
                except (subprocess.SubprocessError, OSError) as error:
                    raise BrowserUnavailable("browser download failed") from error
            self.runtime = runtime
        except Exception:
            runtime.stop()
            raise

    def _open(self, service, *, headless=False):
        self._start(service)
        if self.closing.is_set():
            return
        self._set(
            service,
            "connecting",
            "Kontrollerer gemt login …" if headless else "Åbner tjenestens browser …",
        )
        context = self.contexts.get(service)
        page = self.pages.get(service)
        if (
            context is not None
            and page is not None
            and not page.is_closed()
            and (service in self.headless) == headless
        ):
            if urlsplit(page.url).scheme == "about":
                page.goto(
                    SERVICES[service], wait_until="domcontentloaded", timeout=30000
                )
            if not headless:
                page.bring_to_front()
            # Keep ongoing MFA and redirects intact on repeated clicks.
            return
        if context is not None:
            self.contexts.pop(service, None)
            # A crashed context may already be closed. Never print its diagnostics.
            with suppress(Exception):
                context.close()
        self.pages.pop(service, None)
        profile = self.data_dir / "profiles" / service
        profile.mkdir(parents=True, exist_ok=True, mode=0o700)
        if os.name != "nt":
            profile.parent.chmod(0o700)
            profile.chmod(0o700)
        context = self.runtime.chromium.launch_persistent_context(
            str(profile),
            headless=headless,
            channel="chromium",
            timeout=30000,
            chromium_sandbox=True,
            accept_downloads=False,
        )
        self.contexts[service] = context
        if headless:
            self.headless.add(service)
        else:
            self.headless.discard(service)
        page = context.pages[0] if context.pages else context.new_page()
        self.pages[service] = page
        page.goto(SERVICES[service], wait_until="domcontentloaded", timeout=30000)
        if not headless:
            page.bring_to_front()

    def _mithf_calendar_page(self, service):
        """Return the context tab already inside /vagtplan/, if any."""
        expected = urlsplit(SERVICES[service])
        candidates: list = []
        context = self.contexts.get(service)
        if context is not None:
            with suppress(Exception):
                candidates.extend(list(context.pages))
        page = self.pages.get(service)
        if page is not None and all(item is not page for item in candidates):
            candidates.insert(0, page)
        for candidate in candidates:
            with suppress(Exception):
                if candidate.is_closed():
                    continue
                actual = urlsplit(candidate.url)
                if (actual.scheme, actual.netloc) != (
                    expected.scheme,
                    expected.netloc,
                ):
                    continue
                if actual.path.startswith("/vagtplan/"):
                    return candidate
        return None

    def _auto_enter_mithf_calendar(self, page) -> bool:
        """Click the site's "Åbn din vagtplan" action and wait for entry.

        Returns True when the visible page (or a sibling tab) reached
        /vagtplan/. Never navigates to the calendar URL directly: that
        shows the "Gå til BPA-universet" interstitial instead.
        """
        try:
            wait_selector = getattr(page, "wait_for_selector", None)
            if callable(wait_selector):
                with suppress(Exception):
                    wait_selector(
                        "#dsbVpKn, a[data-dsb=vagtplan]",
                        timeout=5000,
                    )
            clicked = page.evaluate(ENTER_VAGTPLAN_SCRIPT)
        except Exception:  # noqa: BLE001 - isolate failures and redact browser diagnostics
            return False
        if not clicked:
            return False
        wait_for_url = getattr(page, "wait_for_url", None)
        if callable(wait_for_url):
            with suppress(Exception):
                wait_for_url("**/vagtplan/*", timeout=15000)
        try:
            if urlsplit(page.url).path.startswith("/vagtplan/"):
                return True
        except Exception:  # noqa: BLE001 - isolate failures and redact browser diagnostics
            return False
        # The site keeps calendar navigation in the same window, but adopt
        # a sibling tab if it opened one instead.
        return self._mithf_calendar_page("mithf") is not None

    def _check(self, service):
        page = self.pages.get(service)
        if page is None:
            return
        if page.is_closed():
            self._set(
                service, "sign_in_required", "Åbn login for at forbinde tjenesten."
            )
            return
        expected = urlsplit(SERVICES[service])
        actual = urlsplit(page.url)
        if (actual.scheme, actual.netloc) != (expected.scheme, expected.netloc):
            self._set(
                service,
                "sign_in_required",
                "Gennemfør login og eventuel totrinsbekræftelse i browseren.",
            )
            return
        if service == "mithf" and not actual.path.startswith("/vagtplan/"):
            calendar = self._mithf_calendar_page(service)
            if calendar is not None:
                self.pages[service] = calendar
                page = calendar
            elif self._auto_enter_mithf_calendar(page):
                calendar = self._mithf_calendar_page(service)
                if calendar is not None:
                    self.pages[service] = calendar
                    page = calendar
                else:
                    try:
                        actual = urlsplit(page.url)
                    except Exception:  # noqa: BLE001 - isolate failures and redact browser diagnostics
                        actual = urlsplit("")
                    if not actual.path.startswith("/vagtplan/"):
                        self._set(
                            service,
                            "sign_in_required",
                            "Log ind, og vælg “Åbn din vagtplan” på MitHF.",
                        )
                        return
            else:
                self._set(
                    service,
                    "sign_in_required",
                    "Log ind, og vælg “Åbn din vagtplan” på MitHF.",
                )
                return
        # Probe only read endpoints; never return their account data to the GUI.
        result = page.evaluate(
            REQUEST_SCRIPT,
            {
                "system": service,
                "action": "hjaelperliste" if service == "mithf" else "portfolios",
                "payload": {},
            },
        )
        if isinstance(result, dict) and "data" in result:
            self._set(service, "connected", "Forbindelsen er kontrolleret.")
        else:
            self._set(
                service,
                "sign_in_required",
                "Log ind i browseren, eller genindlæs siden, hvis login er udløbet.",
            )

    def request(self, system, action, payload):
        """Read only, on the Playwright owner thread. No session data leaves it."""
        allowed = {
            "mithf": {"hjaelperliste", "muligheder", "plan", "ekstra"},
            "duos": {"portfolios", "types", "employments", "search", "detail"},
        }
        if action not in allowed.get(system, set()):
            raise DestinationError("Setup supports read-only requests")

        def read():
            try:
                if system in self.restore_pending:
                    self._run(system, False)
                page = self.pages.get(system)
                if page is None or page.is_closed():
                    raise DestinationError("Log ind i begge tjenester først.")
                result = page.evaluate(
                    REQUEST_SCRIPT,
                    {"system": system, "action": action, "payload": payload},
                )
                if not isinstance(result, dict) or "data" not in result:
                    self._set(
                        system, "sign_in_required", "Log ind igen for at fortsætte."
                    )
                    raise DestinationError("Log ind igen for at læse tjenesten.")
                return result["data"]
            except DestinationError:
                raise
            except Exception:  # noqa: BLE001 - redact credential and browser exceptions
                raise DestinationError(
                    "Tjenesten kunne ikke læses. Prøv at logge ind igen."
                ) from None

        return self.executor.submit(read).result()

    def close(self):
        self.closing.set()

        def cleanup():
            for context in self.contexts.values():
                with suppress(Exception):
                    context.close()
            if self.runtime is not None:
                self.runtime.stop()

        self.executor.submit(cleanup)
        self.executor.shutdown(wait=True)
