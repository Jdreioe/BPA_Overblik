//! Supervisor for the single app-owned Python worker.
//!
//! The worker is a child process speaking the v1 JSON protocol over
//! stdin/stdout. All blocking I/O happens here — never on the Iced UI
//! thread. Callers share one [`WorkerHandle`]; requests run inside
//! `Task::perform` + `spawn_blocking`, so the UI stays responsive while the
//! worker reads TeamUp, browsers, or SQLite.
//!
//! Failure model (readable recovery states, not panics):
//! - spawn failure → `WorkerError::startup` with the OS error as detail.
//! - unexpected exit / unreadable line → `WorkerError::exited`, including the
//!   captured stderr tail as technical detail for the Help view.

use crate::protocol::{
    AppPaths, EmptyParams, FixturePreviewParams, HomeStatus, HomeStatusParams, PlanPreview,
    Request, Response, WorkerError,
};
use serde::Serialize;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

pub const WORKER_MODULE: &str = "teamup_shift_sync.worker";

/// How the GUI finds its worker.
///
/// 1. `TEAMUP_WORKER_CMD`: explicit override for development, e.g.
///    `.venv/bin/python -m teamup_shift_sync.worker` (split on whitespace).
/// 2. A bundled `teamup-shift-sync-worker` executable next to the GUI binary.
/// 3. Dev fallback: `python3 -m teamup_shift_sync.worker` on `PATH`.
#[derive(Debug, Clone)]
pub struct WorkerCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl WorkerCommand {
    pub fn resolve() -> Self {
        if let Ok(cmd) = std::env::var("TEAMUP_WORKER_CMD") {
            let mut parts = cmd.split_whitespace();
            if let Some(program) = parts.next() {
                return Self {
                    program: program.to_string(),
                    args: parts.map(str::to_string).collect(),
                };
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                for name in ["teamup-shift-sync-worker", "teamup-shift-sync-worker.exe"] {
                    let bundled: PathBuf = dir.join(name);
                    if bundled.is_file() {
                        return Self {
                            program: bundled.to_string_lossy().into_owned(),
                            args: Vec::new(),
                        };
                    }
                }
            }
        }
        Self {
            program: "python3".to_string(),
            args: vec!["-m".to_string(), WORKER_MODULE.to_string()],
        }
    }

    pub fn display(&self) -> String {
        std::iter::once(&self.program)
            .chain(self.args.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Per-user application data directory, mirroring `operations.app_data_dir`
/// in Python so both sides agree on where `sync.sqlite3` lives.
///
/// Existing CLI state in the repo `.local/` directory is preserved: the GUI
/// never migrates or deletes it, and the worker accepts an explicit
/// `state_path` so either location keeps working.
pub fn app_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TEAMUP_SHIFT_SYNC_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dirs) = directories::ProjectDirs::from("", "", "teamup-shift-sync") {
        return dirs.data_dir().to_path_buf();
    }
    PathBuf::from(".local")
}

/// Resolve the config/state/fixture paths for this launch.
///
/// - `TEAMUP_SHIFT_SYNC_CONFIG` wins; otherwise `config.toml` beside the
///   working directory (dev), otherwise `<data_dir>/config.toml`.
/// - State always lives in the per-user data dir.
/// - The fixture is development-only until live preview lands (#5); the UI
///   labels fixture data as test data so it can never be mistaken for live.
#[derive(Debug, Clone)]
pub struct AppFiles {
    pub config_path: PathBuf,
    pub state_path: PathBuf,
    pub fixture_path: PathBuf,
    pub is_fixture: bool,
}

impl AppFiles {
    pub fn resolve() -> Self {
        let data_dir = app_data_dir();
        let config_path = std::env::var("TEAMUP_SHIFT_SYNC_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let local = PathBuf::from("config.toml");
                if local.is_file() {
                    local
                } else {
                    data_dir.join("config.toml")
                }
            });
        let fixture_path = std::env::var("TEAMUP_FIXTURE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("fixtures/representative-week.json"));
        let is_fixture = fixture_path.is_file();
        Self {
            config_path,
            state_path: data_dir.join("sync.sqlite3"),
            fixture_path,
            is_fixture,
        }
    }
}

struct WorkerInner {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
    next_id: u64,
    command_display: String,
}

/// Cloneable handle to the single worker. Blocking calls run inside
/// `spawn_blocking`; the mutex only serializes whole request/response
/// round-trips so frames can never interleave.
#[derive(Clone)]
pub struct WorkerHandle {
    inner: Arc<Mutex<WorkerInner>>,
    files: AppFiles,
}

impl std::fmt::Debug for WorkerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerHandle")
            .field("next_id_known", &"child process (not printable)")
            .field("files", &self.files)
            .finish()
    }
}

impl WorkerHandle {
    /// Spawn the worker and verify it with `ping`. Runs on a blocking thread.
    pub fn spawn(files: &AppFiles) -> Result<Self, WorkerError> {
        let command = WorkerCommand::resolve();
        let mut child = Command::new(&command.program)
            .args(&command.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                WorkerError::startup(format!(
                    "Kunne ikke starte '{}': {e}. Geninstallér appen, hvis den medfølgende arbejder mangler.",
                    command.display()
                ))
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| WorkerError::startup("Baggrundsarbejderens stdin kunne ikke åbnes."))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| WorkerError::startup("Baggrundsarbejderens stdout kunne ikke åbnes."))?;
        let stderr = child.stderr.take();
        let handle = Self {
            inner: Arc::new(Mutex::new(WorkerInner {
                child,
                stdin,
                stdout: BufReader::new(stdout),
                stderr,
                next_id: 1,
                command_display: command.display(),
            })),
            files: files.clone(),
        };
        // The ping doubles as a protocol-version handshake: a worker that
        // answers with another version fails here, not mid-preview.
        handle.request("ping", EmptyParams {})?;
        Ok(handle)
    }

    fn request<P: Serialize>(&self, method: &str, params: P) -> Result<Value, WorkerError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| WorkerError::exited("Baggrundsarbejderens interne lås er ødelagt."))?;
        let id = inner.next_id.to_string();
        inner.next_id += 1;
        let frame = Request::new(id.clone(), method, params);
        let mut line = serde_json::to_string(&frame)
            .map_err(|e| WorkerError::exited(format!("Kunne ikke kode forespørgsel: {e}")))?;
        line.push('\n');
        if inner.stdin.write_all(line.as_bytes()).is_err() || inner.stdin.flush().is_err() {
            return Err(self.describe_exit(&mut inner, "skrivning til"));
        }
        let mut response_line = String::new();
        match inner.stdout.read_line(&mut response_line) {
            Ok(0) => Err(self.describe_exit(&mut inner, "læsning fra")),
            Ok(_) => {
                let response: Response = serde_json::from_str(&response_line).map_err(|e| {
                    WorkerError::exited(format!("Baggrundsarbejderen svarede ulæseligt: {e}"))
                })?;
                if response.protocol != crate::protocol::PROTOCOL_VERSION {
                    return Err(WorkerError::exited(format!(
                        "Protokol v{} blev svaret, men v{} forventes. Opdatér app og arbejder samlet.",
                        response.protocol,
                        crate::protocol::PROTOCOL_VERSION
                    )));
                }
                if response.event == "result" {
                    Ok(response.payload)
                } else {
                    Err(WorkerError::from_payload(&response.payload))
                }
            }
            Err(e) => Err(self.describe_exit(&mut inner, &format!("læsning fra ( {e} )"))),
        }
    }

    /// Build an "unexpected exit" error, attaching the stderr tail as
    /// technical detail. Stderr never contains tokens or shift text by
    /// worker construction — only exception summaries and paths.
    fn describe_exit(&self, inner: &mut WorkerInner, phase: &str) -> WorkerError {
        use std::io::Read as _;
        let mut detail = format!(
            "Kommunikationen med '{}' brød sammen under {}.",
            inner.command_display, phase
        );
        if let Some(stderr) = inner.stderr.as_mut() {
            let mut tail = String::new();
            // Best effort: the child has exited or is exiting; do not block.
            let _ = stderr.read_to_string(&mut tail);
            let trimmed: String = tail
                .lines()
                .rev()
                .take(8)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            if !trimmed.trim().is_empty() {
                detail.push_str("\nTeknisk detalje:\n");
                detail.push_str(trimmed.trim());
            }
        }
        if let Ok(Some(status)) = inner.child.try_wait() {
            detail.push_str(&format!("\nAfslutningskode: {status}"));
        }
        WorkerError::exited(detail)
    }

    /// Query the worker's view of the app-owned paths. Used by the
    /// Help/diagnostics view (#7); kept here so both sides stay in sync.
    #[allow(dead_code)]
    pub fn app_paths(&self) -> Result<AppPaths, WorkerError> {
        let payload = self.request("app_paths", EmptyParams {})?;
        serde_json::from_value(payload)
            .map_err(|e| WorkerError::exited(format!("Ugyldigt app_paths-svar: {e}")))
    }

    pub fn home_status(&self, now_iso: Option<&str>) -> Result<HomeStatus, WorkerError> {
        let payload = self.request(
            "home_status",
            HomeStatusParams {
                config_path: &self.files.config_path,
                state_path: &self.files.state_path,
                now: now_iso,
            },
        )?;
        serde_json::from_value(payload)
            .map_err(|e| WorkerError::exited(format!("Ugyldigt home_status-svar: {e}")))
    }

    pub fn preview_fixture(&self, from: &str, to: &str) -> Result<PlanPreview, WorkerError> {
        let payload = self.request(
            "preview_fixture",
            FixturePreviewParams {
                config_path: &self.files.config_path,
                fixture_path: &self.files.fixture_path,
                state_path: &self.files.state_path,
                from,
                to,
            },
        )?;
        serde_json::from_value(payload)
            .map_err(|e| WorkerError::exited(format!("Ugyldigt preview-svar: {e}")))
    }
}
