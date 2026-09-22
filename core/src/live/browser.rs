use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::process::{Child, Command};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::{rows, text, LiveError, INVALID};

const REQUEST: &str = include_str!("browser_request.js");

fn write_allowed(service: Service, action: &str) -> bool {
    match service {
        Service::Mithf => matches!(
            action,
            "opret" | "rettid" | "book" | "tilfoejreg" | "retreg"
        ),
        // The citizen saves a pending registration. Only the helper can accept it,
        // and this transport exposes no DUOS acceptance action.
        Service::Duos => action == "register",
    }
}

/// Opening `/vagtplan/index.php` directly shows MitHF's "Gå til BPA-universet"
/// interstitial instead of the calendar: entry requires the site's own
/// "Åbn din vagtplan" action, which POSTs a single-use ticket. Never navigate
/// to the calendar URL; click the site button so the ticket exchange runs.
const ENTER_CALENDAR: &str = r#"() => {
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
  for (const el of document.querySelectorAll('button, a')) {
    if ((el.textContent || '').includes('Åbn din vagtplan') && visible(el)) { el.click(); return true; }
  }
  return false;
}"#;

/// Whether the DUOS app has restored the session the profile already holds.
/// Presence only: no token, role or account value leaves the page.
const DUOS_SESSION: &str = r#"() => localStorage.getItem('role') === 'citizen'
  && !!localStorage.getItem('token')"#;

/// Whether MitHF's calendar page has parsed the request token its own API
/// calls carry. Navigation commits before the document finishes loading, so a
/// matching URL alone does not mean a request can be served. Presence only:
/// the token never leaves the page.
const MITHF_SESSION: &str = r#"() => [...document.scripts].filter(s => !s.src)
  .some(s => /var TOK=("[^"]*"|'[^']*')/.test(s.textContent))"#;

/// Chromium visibility for a launch. Interactive login and its two-factor step
/// need a window; every other launch reuses the saved profile without one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Visibility {
    Window,
    Background,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Service {
    Mithf,
    Duos,
}
impl Service {
    pub fn key(self) -> &'static str {
        match self {
            Self::Mithf => "mithf",
            Self::Duos => "duos",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Mithf => "MitHF",
            Self::Duos => "DUOS",
        }
    }
    fn origin(self) -> &'static str {
        match self {
            Self::Mithf => "https://mithf.handicapformidlingen.dk",
            Self::Duos => "https://mit.duos.dk",
        }
    }
    fn entry(self) -> &'static str {
        match self {
            Self::Mithf => "https://mithf.handicapformidlingen.dk/index.html",
            Self::Duos => "https://mit.duos.dk/usage/timeregistrations",
        }
    }
}
struct Session {
    child: Child,
    endpoint: String,
    visibility: Visibility,
}

/// Own only browsers launched for this native preview. Profiles are separate
/// from the Python app, so concurrent testing cannot change its active sessions.
pub struct BrowserSessions {
    data_dir: PathBuf,
    sessions: BTreeMap<Service, Session>,
    client: reqwest::Client,
    /// The destination catalog check this session has already passed, as an
    /// opaque digest of what was checked. It lives and dies with the session,
    /// so a new login always checks again.
    catalog: std::sync::Mutex<Option<[u8; 32]>>,
    _lock: std::fs::File,
}

impl BrowserSessions {
    /// Filesystem work: construct on a blocking thread.
    pub fn new(data_dir: PathBuf) -> Result<Self, LiveError> {
        let root = data_dir.join("rust-preview");
        private_dir(&root)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("browser.lock"))
            .map_err(|_| INVALID)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| {
            LiveError("Rust-forhåndsvisningen kører allerede. Luk det andet vindue først.")
        })?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| INVALID)?;
        Ok(Self {
            data_dir,
            sessions: BTreeMap::new(),
            client,
            catalog: std::sync::Mutex::new(None),
            _lock: lock,
        })
    }

    /// Reuses a live session unless a background one has to become visible for
    /// login. A background request never replaces a window already in use.
    pub async fn open(
        &mut self,
        service: Service,
        visibility: Visibility,
    ) -> Result<(), LiveError> {
        if let Some(session) = self.sessions.get_mut(&service) {
            let alive = session.child.try_wait().map_err(|_| INVALID)?.is_none();
            let usable =
                visibility == Visibility::Background || session.visibility == Visibility::Window;
            if alive && usable {
                // Preserve ongoing MFA/navigation; repeated clicks never restart login.
                if visibility == Visibility::Window {
                    if let Ok(page) = self.page(service, false).await {
                        let _ = cdp(&page, "Page.bringToFront", json!({})).await;
                    }
                }
                return Ok(());
            }
        }
        self.sessions.remove(&service);
        let data_dir = self.data_dir.clone();
        let (executable, profile) = tokio::task::spawn_blocking(move || {
            let executable = browser_executable(&data_dir)?;
            let profile = data_dir.join("rust-preview/profiles").join(service.key());
            private_dir(profile.parent().ok_or(INVALID)?)?;
            private_dir(&profile)?;
            let marker = profile.join("DevToolsActivePort");
            if marker.exists() {
                std::fs::remove_file(marker).map_err(|_| INVALID)?;
            }
            Ok::<_, LiveError>((executable, profile))
        })
        .await
        .map_err(|_| INVALID)??;
        let mut command = Command::new(executable);
        command
            .arg(format!("--user-data-dir={}", profile.display()))
            .args([
                "--remote-debugging-port=0",
                "--remote-debugging-address=127.0.0.1",
                "--no-first-run",
                "--no-default-browser-check",
            ]);
        if visibility == Visibility::Background {
            command.arg("--headless=new");
        }
        command
            .arg(service.entry())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| {
            LiveError("Browseren kunne ikke startes. Kontrollér browserinstallationen.")
        })?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if child.try_wait().map_err(|_| INVALID)?.is_some()
                || tokio::time::Instant::now() >= deadline
            {
                return Err(LiveError("Browseren svarede ikke. Luk eventuelle andre Rust-forhåndsvisninger, og prøv igen."));
            }
            if let Ok(marker) = tokio::fs::read_to_string(profile.join("DevToolsActivePort")).await
            {
                if let Some(port) = marker
                    .lines()
                    .next()
                    .and_then(|s| s.parse::<u16>().ok())
                    .filter(|p| *p != 0)
                {
                    self.sessions.insert(
                        service,
                        Session {
                            child,
                            endpoint: format!("http://127.0.0.1:{port}"),
                            visibility,
                        },
                    );
                    if service == Service::Duos && visibility == Visibility::Background {
                        self.await_duos_session().await;
                    }
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Whether this session already validated exactly this catalog check.
    pub(crate) fn catalog_validated(&self, stamp: &[u8; 32]) -> bool {
        self.catalog
            .lock()
            .is_ok_and(|remembered| remembered.as_ref() == Some(stamp))
    }
    /// Record a passed catalog check, so a repeated read can skip it.
    pub(crate) fn remember_catalog(&self, stamp: [u8; 32]) {
        if let Ok(mut remembered) = self.catalog.lock() {
            *remembered = Some(stamp);
        }
    }

    /// Give a freshly launched DUOS browser time to restore its saved session.
    ///
    /// A launch is debuggable before the app has read its profile, so a request
    /// sent straight afterwards sees no session and is rejected as a missing
    /// login. MitHF waits for its calendar page through `enter_calendar`; this
    /// is the same handshake for DUOS.
    ///
    /// Best effort on purpose: waiting cannot create a session, so a profile
    /// that really is signed out falls through to the ordinary login error
    /// instead of a new one.
    async fn await_duos_session(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(page) = self.page(Service::Duos, false).await {
                if present(&page, DUOS_SESSION).await {
                    return;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn page(&self, service: Service, require_calendar: bool) -> Result<String, LiveError> {
        let session = self
            .sessions
            .get(&service)
            .ok_or(LiveError("Åbn login til begge tjenester først."))?;
        let pages: Value = self
            .client
            .get(format!("{}/json/list", session.endpoint))
            .send()
            .await
            .map_err(|_| {
                LiveError("Browseren er lukket eller kunne ikke kontaktes. Åbn login igen.")
            })?
            .json()
            .await
            .map_err(|_| INVALID)?;
        let candidates: Vec<_> = rows(&pages)?
            .iter()
            .filter(|page| {
                if page["type"] != "page" {
                    return false;
                }
                let Ok(url) = reqwest::Url::parse(page["url"].as_str().unwrap_or("")) else {
                    return false;
                };
                url.origin().ascii_serialization() == service.origin()
                    && (!require_calendar
                        || service != Service::Mithf
                        || url.path().starts_with("/vagtplan/"))
            })
            .collect();
        if candidates.len() != 1 {
            return Err(LiveError("Gennemfør login i browseren. På MitHF skal du vælge Åbn din vagtplan. Behold én kalenderfane pr. tjeneste."));
        }
        let socket = text(&candidates[0]["webSocketDebuggerUrl"])?;
        let url = reqwest::Url::parse(socket).map_err(|_| INVALID)?;
        if url.scheme() != "ws" || !matches!(url.host_str(), Some("127.0.0.1" | "localhost")) {
            return Err(INVALID);
        }
        Ok(socket.into())
    }

    pub async fn check(&self, service: Service) -> Result<(), LiveError> {
        self.request(
            service,
            if service == Service::Mithf {
                "hjaelperliste"
            } else {
                "portfolios"
            },
            json!({}),
        )
        .await?;
        Ok(())
    }

    /// Reads only. Nothing reachable from here can change a destination.
    pub async fn request(
        &self,
        service: Service,
        action: &str,
        payload: Value,
    ) -> Result<Value, LiveError> {
        let allowed = match service {
            Service::Mithf => matches!(action, "hjaelperliste" | "muligheder" | "plan" | "ekstra"),
            Service::Duos => matches!(
                action,
                "portfolios" | "types" | "employments" | "search" | "detail"
            ),
        };
        if !allowed {
            return Err(LiveError("Rust-forhåndsvisningen tillader kun læsning."));
        }
        self.call(service, action, payload).await
    }

    /// The one path that changes a destination. Crate-private on purpose: only
    /// the transfer adapter reaches it, and only for an approved plan item.
    pub(crate) async fn submit(
        &self,
        service: Service,
        action: &str,
        payload: Value,
    ) -> Result<Value, LiveError> {
        if !write_allowed(service, action) {
            return Err(LiveError("Handlingen er ikke en understøttet overførsel."));
        }
        self.call(service, action, payload).await
    }

    /// Reach MitHF's shift calendar through the site's own entry action, so a
    /// restored session does not depend on someone clicking it in a window.
    ///
    /// A freshly launched browser is debuggable before its entry page has
    /// rendered, so the action is retried until the calendar appears rather
    /// than given up on the first attempt. The entry action is only repeated
    /// after the previous one has had time to navigate, so a ticket exchange
    /// already under way is never cut short.
    async fn enter_calendar(&self) -> Result<(), LiveError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut entered_at: Option<tokio::time::Instant> = None;
        loop {
            // The calendar page has to be able to serve a request, not merely
            // exist: it carries the token every MitHF call sends.
            if let Ok(page) = self.page(Service::Mithf, true).await {
                if present(&page, MITHF_SESSION).await {
                    return Ok(());
                }
            }
            if entered_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(3)) {
                if let Ok(page) = self.page(Service::Mithf, false).await {
                    let result = cdp(
                        &page,
                        "Runtime.evaluate",
                        json!({"expression": format!("({ENTER_CALENDAR})()"), "returnByValue": true}),
                    )
                    .await;
                    if result.is_ok_and(|result| result["result"]["value"] == json!(true)) {
                        entered_at = Some(tokio::time::Instant::now());
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(INVALID);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    async fn call(
        &self,
        service: Service,
        action: &str,
        payload: Value,
    ) -> Result<Value, LiveError> {
        let page = match self.page(service, true).await {
            Ok(page) => page,
            // Only MitHF hides its calendar behind an entry action.
            Err(error) if service == Service::Mithf => {
                self.enter_calendar().await.map_err(|_| error)?;
                self.page(service, true).await?
            }
            Err(error) => return Err(error),
        };
        let input = json!({"system": service.key(), "action": action, "payload": payload});
        let expression = format!("({REQUEST})({input})");
        let result = cdp(
            &page,
            "Runtime.evaluate",
            json!({"expression": expression, "awaitPromise": true, "returnByValue": true}),
        )
        .await?;
        if result.get("exceptionDetails").is_some() {
            return Err(INVALID);
        }
        let result = &result["result"]["value"];
        // Name the service: the person has two logins and has to know which
        // one to renew.
        result.get("data").cloned().ok_or(match service {
            Service::Mithf => {
                LiveError("MitHF kunne ikke læses. Log ind i MitHF igen, og prøv igen.")
            }
            Service::Duos => LiveError("DUOS kunne ikke læses. Log ind i DUOS igen, og prøv igen."),
        })
    }
}

/// Answer a page-side presence probe. Any failure counts as "not ready", so a
/// caller can poll this without turning a slow page into an error of its own.
async fn present(page: &str, probe: &str) -> bool {
    cdp(
        page,
        "Runtime.evaluate",
        json!({"expression": format!("({probe})()"), "returnByValue": true}),
    )
    .await
    .is_ok_and(|result| result["result"]["value"] == json!(true))
}

async fn cdp(socket: &str, method: &str, params: Value) -> Result<Value, LiveError> {
    tokio::time::timeout(Duration::from_secs(55), async {
        let (mut connection, _) = connect_async(socket).await.map_err(|_| INVALID)?;
        connection
            .send(Message::Text(
                json!({"id": 1, "method": method, "params": params})
                    .to_string()
                    .into(),
            ))
            .await
            .map_err(|_| INVALID)?;
        while let Some(message) = connection.next().await {
            let message = message.map_err(|_| INVALID)?;
            if let Message::Text(text) = message {
                let response: Value = serde_json::from_str(&text).map_err(|_| INVALID)?;
                if response["id"] == 1 {
                    return response.get("result").cloned().ok_or(INVALID);
                }
            }
        }
        Err(INVALID)
    })
    .await
    .map_err(|_| LiveError("Browserforespørgslen tog for lang tid. Prøv igen."))?
}

/// Forget the saved MitHF and DUOS logins by removing the app's own browser
/// profiles. Nothing is sent to either service, and no synchronization record
/// is touched: this only clears local session data, so the next login starts
/// clean. Close the sessions first. Run on a blocking thread.
pub fn forget_logins(data_dir: &Path) -> Result<(), LiveError> {
    let profiles = data_dir.join("rust-preview/profiles");
    if !profiles.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(&profiles).map_err(|_| {
        LiveError("De gemte logins kunne ikke fjernes. Luk appens browservinduer, og prøv igen.")
    })
}

fn private_dir(path: &Path) -> Result<(), LiveError> {
    std::fs::create_dir_all(path).map_err(|_| INVALID)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| INVALID)?;
    }
    Ok(())
}
pub(super) fn browser_executable(data_dir: &Path) -> Result<PathBuf, LiveError> {
    if let Some(path) = std::env::var_os("TEAMUP_BROWSER_PATH") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(LiveError(
            "TEAMUP_BROWSER_PATH peger ikke på en browserfil.",
        ));
    }
    // Prefer the app's existing Chromium, preserving the tested browser version.
    let mut roots: Vec<_> = std::fs::read_dir(data_dir.join("browsers"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    roots.sort();
    roots.reverse();
    for root in roots {
        for suffix in ["chrome-linux64/chrome", "chrome-linux/chrome", "chrome-win64/chrome.exe", "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"] {
            let path = root.join(suffix);
            if path.is_file() { return Ok(path); }
        }
    }
    for path in [
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
        "/opt/google/chrome/chrome",
    ] {
        if Path::new(path).is_file() {
            return Ok(path.into());
        }
    }
    Err(LiveError("Chromium mangler. Installér Chromium, eller angiv browserfilen i TEAMUP_BROWSER_PATH, og prøv igen."))
}

#[cfg(test)]
mod tests {
    use super::{write_allowed, Service};

    #[test]
    fn forgetting_logins_removes_only_the_app_profiles() {
        let dir = tempfile::tempdir().expect("temp dir");
        let profiles = dir.path().join("rust-preview/profiles/mithf");
        std::fs::create_dir_all(&profiles).expect("profile");
        std::fs::write(dir.path().join("sync-abc.sqlite3"), b"history").expect("history");

        super::forget_logins(dir.path()).expect("forget");
        // Logging out twice is not an error.
        super::forget_logins(dir.path()).expect("forget again");

        assert!(!dir.path().join("rust-preview/profiles").exists());
        assert!(dir.path().join("sync-abc.sqlite3").is_file());
    }

    #[test]
    fn duos_transport_can_save_but_cannot_accept_registrations() {
        assert!(write_allowed(Service::Duos, "register"));
        assert!(!write_allowed(Service::Duos, "accept"));
        assert!(!write_allowed(Service::Duos, "approve"));
    }
}
