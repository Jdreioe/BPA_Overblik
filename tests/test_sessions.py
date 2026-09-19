import io
import sys
from contextlib import redirect_stderr
from pathlib import Path
from tempfile import TemporaryDirectory
from threading import Event
from unittest import TestCase
from unittest.mock import Mock, patch

from teamup_shift_sync.sessions import SERVICES, BrowserSessions


class SessionTests(TestCase):
    def setUp(self):
        self.directory = TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.sessions = BrowserSessions(Path(self.directory.name))
        self.addCleanup(lambda: self.sessions.close())

    def settle(self):
        self.sessions.executor.submit(lambda: None).result(timeout=2)

    def page(self, service):
        page = Mock()
        page.url = SERVICES[service]
        page.is_closed.return_value = False
        page.evaluate.return_value = {"data": []}
        self.sessions.pages[service] = page
        return page

    def test_startup_checks_saved_profile_without_login_click(self):
        profile = Path(self.directory.name) / "profiles" / "duos" / "Default"
        profile.mkdir(parents=True)
        self.sessions.close()
        self.sessions = BrowserSessions(Path(self.directory.name))
        page = Mock()
        page.url = SERVICES["duos"]
        page.is_closed.return_value = False
        page.evaluate.return_value = {"data": []}
        context = Mock(pages=[page])
        self.sessions.runtime = Mock()
        launch = self.sessions.runtime.chromium.launch_persistent_context
        launch.return_value = context
        self.sessions.submit("duos", login=False)
        self.settle()
        self.assertEqual(self.sessions.snapshot()["duos"]["state"], "connected")
        self.assertEqual(launch.call_args.args[0], str(profile.parent))
        self.assertTrue(launch.call_args.kwargs["headless"])
        page.bring_to_front.assert_not_called()

        self.sessions.submit("duos", login=True)
        self.settle()
        context.close.assert_called_once()
        self.assertFalse(launch.call_args.kwargs["headless"])

    def test_first_start_without_profile_does_not_launch_browser(self):
        self.sessions._open = Mock()
        self.sessions.submit("mithf", login=False)
        self.settle()
        self.sessions._open.assert_not_called()
        self.assertEqual(self.sessions.snapshot()["mithf"]["state"], "sign_in_required")

    def test_probe_reads_only_and_handles_expiry_without_account_data(self):
        page = self.page("duos")
        self.sessions.submit("duos", login=False)
        self.settle()
        self.assertEqual(self.sessions.snapshot()["duos"]["state"], "connected")
        self.assertEqual(page.evaluate.call_args.args[1]["action"], "portfolios")
        page.evaluate.return_value = {"error": "private session detail"}
        self.sessions.submit("duos", login=False)
        self.settle()
        self.assertEqual(self.sessions.snapshot()["duos"]["state"], "sign_in_required")
        self.assertNotIn("private", str(self.sessions.snapshot()))

    def test_mfa_redirect_never_runs_destination_script(self):
        page = self.page("mithf")
        page.url = "https://identity.example/mfa"
        self.sessions.submit("mithf", login=False)
        self.settle()
        page.evaluate.assert_not_called()
        self.assertEqual(self.sessions.snapshot()["mithf"]["state"], "sign_in_required")

    def test_failure_is_redacted_and_other_service_stays_connected(self):
        self.page("duos")
        self.sessions.submit("duos", login=False)
        self.settle()
        self.sessions._open = Mock(side_effect=RuntimeError("secret token"))
        self.sessions.submit("mithf", login=True)
        self.settle()
        self.sessions.submit("mithf", login=False)
        self.settle()
        status = self.sessions.snapshot()
        self.assertEqual(status["mithf"]["state"], "unavailable")
        self.assertEqual(status["duos"]["state"], "connected")
        self.assertNotIn("secret", str(status))
        self.sessions._open.side_effect = None
        self.page("mithf")
        self.sessions.submit("mithf", login=True)
        self.settle()
        self.assertEqual(self.sessions.snapshot()["mithf"]["state"], "connected")

    def test_missing_browser_runtime_says_so_instead_of_blaming_windows(self):
        """A missing browser is not a stuck window; closing windows cannot fix it."""
        diagnostics = io.StringIO()
        with (
            patch.dict(sys.modules, {"playwright": None, "playwright.sync_api": None}),
            redirect_stderr(diagnostics),
        ):
            self.sessions.submit("mithf", login=True)
            self.settle()
        status = self.sessions.snapshot()["mithf"]

        self.assertEqual(status["state"], "unavailable")
        self.assertIn("Appens egen browser mangler", status["message"])
        self.assertNotIn("app-vinduer", status["message"])
        self.assertEqual(
            "session error [mithf] ModuleNotFoundError",
            diagnostics.getvalue().strip(),
        )

    def test_duplicate_clicks_do_not_launch_more_browsers(self):
        entered, release = Event(), Event()

        def open_browser(service):
            entered.set()
            release.wait(timeout=2)

        self.sessions._open = Mock(side_effect=open_browser)
        self.sessions.submit("mithf", login=True)
        self.assertTrue(entered.wait(timeout=2))
        self.sessions.submit("mithf", login=True)
        self.sessions.submit("mithf", login=False)
        self.assertEqual(self.sessions.snapshot()["mithf"]["state"], "connecting")
        release.set()
        self.settle()
        self.sessions._open.assert_called_once_with("mithf")

    def test_closed_browser_requires_login_and_unknown_service_is_rejected(self):
        page = self.page("mithf")
        page.is_closed.return_value = True
        self.sessions.submit("mithf", login=False)
        self.settle()
        self.assertEqual(self.sessions.snapshot()["mithf"]["state"], "sign_in_required")
        with self.assertRaises(ValueError):
            self.sessions.submit("../other-profile", login=True)
