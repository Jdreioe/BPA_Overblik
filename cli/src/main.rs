use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use clap::{Args, Parser, Subcommand};
use std::{
    error::Error,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};
use teamup_shift_sync_core::{
    apply_plan, build_plan,
    live::{
        load_saved_setup, read_shapes, read_teamup, read_week, BrowserSessions, LiveDestinations,
        Service, Visibility,
    },
    plan_digest, ApplyRequest, Outcome, PlanRequest, SyncPlan, SyncState,
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Parser)]
#[command(
    name = "teamup-shift-sync-rust",
    version,
    about = "Native weekly shift preview and approved transfer"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Preview a fixture or a live week. Makes no destination writes.
    DryRun {
        #[arg(long, required_unless_present = "live", conflicts_with = "live")]
        fixture: Option<PathBuf>,
        #[arg(long, requires = "data_dir")]
        live: bool,
        /// Existing TOML configuration, used only for fixture previews.
        #[arg(long, requires = "fixture", conflicts_with = "live")]
        config: Option<PathBuf>,
        /// SQLite file for fixture previews. Live runs use the saved account scope.
        #[arg(long, requires = "fixture", conflicts_with = "live")]
        state: Option<PathBuf>,
        /// Desktop data directory containing setup.json and browser profiles.
        #[arg(long, requires = "live", conflicts_with = "fixture")]
        data_dir: Option<PathBuf>,
        #[command(flatten)]
        dates: Dates,
        /// Fixed fixture evaluation time, including UTC offset.
        #[arg(long, requires = "fixture", conflicts_with = "live")]
        now: Option<DateTime<FixedOffset>>,
        /// Print the complete plan and digest as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Re-read and apply exactly the reviewed live plan, verifying every step.
    Apply {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        from: NaiveDate,
        #[arg(long)]
        to: NaiveDate,
        #[arg(long, value_parser = digest)]
        approve: String,
    },
    /// Open the separate Rust browser profiles for login, then check read access.
    Login {
        #[arg(long)]
        data_dir: PathBuf,
    },
    /// Record the JSON shape of every service read, for checking the native
    /// readers against reality. Writes no values, names or identifiers.
    Capture {
        #[arg(long)]
        data_dir: PathBuf,
        #[command(flatten)]
        dates: Dates,
        /// File to write. Defaults to standard output.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Forget local recovery records for explicitly named sources.
    Forget {
        #[arg(long)]
        state: PathBuf,
        #[arg(long, num_args = 1.., required = true)]
        shift: Vec<String>,
    },
}

#[derive(Args)]
struct Dates {
    #[arg(long)]
    from: Option<NaiveDate>,
    #[arg(long)]
    to: Option<NaiveDate>,
}

fn digest(value: &str) -> std::result::Result<String, String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        Ok(value.into())
    } else {
        Err("expected the 64-character lowercase digest from a live dry-run".into())
    }
}

struct Range {
    from: NaiveDate,
    to: NaiveDate,
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
}

impl Dates {
    fn resolve(&self, zone: Tz, now: DateTime<FixedOffset>) -> Result<Range> {
        let today = now.with_timezone(&zone).date_naive();
        let from = self
            .from
            .or_else(|| {
                today.checked_sub_signed(Duration::days(
                    today.weekday().num_days_from_monday().into(),
                ))
            })
            .ok_or("Start date is outside the supported range")?;
        let to = self
            .to
            .or_else(|| from.checked_add_signed(Duration::days(6)))
            .ok_or("End date is outside the supported range")?;
        if to < from {
            return Err("--to must be on or after --from".into());
        }
        let midnight = |date: NaiveDate| -> Result<_> {
            zone.from_local_datetime(&date.and_hms_opt(0, 0, 0).ok_or("Invalid date")?)
                .single()
                .map(|v| v.fixed_offset())
                .ok_or_else(|| "Range boundary is an ambiguous or nonexistent local time".into())
        };
        Ok(Range {
            from,
            to,
            start: midnight(from)?,
            end: midnight(
                to.succ_opt()
                    .ok_or("End date is outside the supported range")?,
            )?,
        })
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Could not start the asynchronous runtime".into())
        .and_then(|runtime| runtime.block_on(run(cli)));
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            // Display only curated errors, never Debug or chained service responses.
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

/// `Window` is for the interactive login command only; every other command
/// reuses the saved profiles without opening a browser window.
async fn browsers(data_dir: PathBuf, visibility: Visibility) -> Result<BrowserSessions> {
    let mut browser = tokio::task::spawn_blocking(move || BrowserSessions::new(data_dir))
        .await
        .map_err(|_| "Browser preparation failed")??;
    browser.open(Service::Mithf, visibility).await?;
    browser.open(Service::Duos, visibility).await?;
    Ok(browser)
}

async fn run(cli: Cli) -> Result<u8> {
    match cli.command {
        Command::DryRun {
            fixture: Some(fixture),
            config,
            state,
            dates,
            now,
            json,
            ..
        } => {
            let plan = tokio::task::spawn_blocking(move || {
                teamup_shift_sync_core::fixture::preview(
                    &config.unwrap_or_else(|| "config.toml".into()),
                    &fixture,
                    &state.unwrap_or_else(|| ".local/offline-rust.sqlite3".into()),
                    dates.from,
                    dates.to,
                    now.unwrap_or_else(|| Utc::now().fixed_offset()),
                )
            })
            .await
            .map_err(|_| "Fixture preview failed")??;
            report(&plan, json, false)
        }
        Command::DryRun {
            data_dir: Some(data_dir),
            dates,
            json,
            ..
        } => {
            let setup_dir = data_dir.clone();
            let config = tokio::task::spawn_blocking(move || load_saved_setup(&setup_dir))
                .await
                .map_err(|_| "Could not load setup")??;
            let range = dates.resolve(config.planning.timezone, Utc::now().fixed_offset())?;
            let browser = browsers(data_dir, Visibility::Background).await?;
            let now = Utc::now().fixed_offset();
            let (shifts, destination) = read_week(
                &browser,
                &config,
                range.from,
                range.to,
                range.start,
                range.end,
                now,
            )
            .await?;
            let plan = tokio::task::spawn_blocking(move || -> Result<_> {
                let state = SyncState::open(&config.state_path)?;
                Ok(build_plan(
                    &PlanRequest {
                        config: &config.planning,
                        shifts: &shifts,
                        destination: &destination,
                        range_start: range.start,
                        range_end: range.end,
                        now,
                        live: true,
                    },
                    &state,
                )?)
            })
            .await
            .map_err(|_| "Planning failed")??;
            report(&plan, json, true)
        }
        Command::DryRun { .. } => Err("Choose --fixture or --live with --data-dir".into()),
        Command::Apply {
            data_dir,
            from,
            to,
            approve,
        } => {
            let setup_dir = data_dir.clone();
            let config = tokio::task::spawn_blocking(move || load_saved_setup(&setup_dir))
                .await
                .map_err(|_| "Could not load setup")??;
            let range = Dates {
                from: Some(from),
                to: Some(to),
            }
            .resolve(config.planning.timezone, Utc::now().fixed_offset())?;
            let browser = browsers(data_dir, Visibility::Background).await?;
            // Never reuse source data from an earlier preview or accept an imported plan.
            let shifts = read_teamup(&config, from, to).await?;
            let now = Utc::now().fixed_offset();
            let mut destinations = LiveDestinations::connect(&browser, &config, now).await?;
            apply_plan(
                ApplyRequest {
                    config: config.planning.clone(),
                    shifts,
                    range_start: range.start,
                    range_end: range.end,
                    now,
                    expected_digest: approve,
                },
                config.state_path.clone(),
                &mut destinations,
                |_| {},
            )
            .await?;
            println!(
                "Synchronization finished; all selected operations were read back and verified."
            );
            Ok(0)
        }
        Command::Login { data_dir } => {
            let browser = browsers(data_dir, Visibility::Window).await?;
            eprintln!("Log in to both browser windows. In MitHF, open your shift calendar. Keep one service tab per window.");
            eprintln!("Press Enter when both logins are complete.");
            tokio::task::spawn_blocking(|| -> Result<()> {
                let mut line = String::new();
                if io::stdin()
                    .read_line(&mut line)
                    .map_err(|_| "Could not read login confirmation")?
                    == 0
                {
                    return Err("Login cancelled: standard input closed".into());
                }
                Ok(())
            })
            .await
            .map_err(|_| "Login confirmation failed")??;
            browser.check(Service::Mithf).await?;
            browser.check(Service::Duos).await?;
            println!("Both services are readable. The Rust browser profiles are saved for later commands.");
            Ok(0)
        }
        Command::Capture {
            data_dir,
            dates,
            out,
        } => {
            let setup_dir = data_dir.clone();
            let config = tokio::task::spawn_blocking(move || load_saved_setup(&setup_dir))
                .await
                .map_err(|_| "Could not load setup")??;
            let now = Utc::now().fixed_offset();
            let range = dates.resolve(config.planning.timezone, now)?;
            let browser = browsers(data_dir, Visibility::Background).await?;
            let today = now.with_timezone(&config.planning.timezone).date_naive();
            let recorded = read_shapes(&browser, &config, range.from, range.to, today).await?;
            let document = serde_json::to_string_pretty(&recorded)? + "\n";
            match out {
                Some(path) => std::fs::write(&path, document)
                    .map_err(|_| "Could not write the shape document")?,
                None => io::stdout().write_all(document.as_bytes())?,
            }
            Ok(0)
        }
        Command::Forget { state, shift } => tokio::task::spawn_blocking(move || -> Result<u8> {
            // A mistyped path should not silently create a new empty account database.
            if !state.is_file() {
                return Err("State file does not exist".into());
            }
            let mut state = SyncState::open(state)?;
            let _guard = state.exclusive_apply()?;
            let mut count = 0;
            for key in shift.into_iter().collect::<std::collections::BTreeSet<_>>() {
                count += state.forget_steps(&key)?.len();
            }
            println!("Forgot {count} local step records. Review a new dry-run before applying.");
            Ok(if count == 0 { 1 } else { 0 })
        })
        .await
        .map_err(|_| "Forgetting local records failed")?,
    }
}

fn report(plan: &SyncPlan, json: bool, live: bool) -> Result<u8> {
    let blocked = plan.items.iter().any(|item| {
        matches!(
            item.outcome,
            Outcome::Conflicted | Outcome::Failed | Outcome::Review | Outcome::PendingIntegration
        )
    });
    let digest = plan_digest(plan)?;
    let mut output = io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(
            &mut output,
            &serde_json::json!({
                "mode": if live { "live" } else { "fixture" },
                "plan": plan, "digest": digest, "has_blockers": blocked,
            }),
        )
        .map_err(|_| "Could not write preview")?;
        writeln!(output)?;
    } else {
        writeln!(output, "DRY RUN: no destination writes")?;
        writeln!(
            output,
            "Range: {} to {} (end exclusive)",
            plan.starts_at, plan.ends_at
        )?;
        let mut source = None;
        for item in &plan.items {
            // Quote untrusted strings so terminal controls cannot hide changes.
            if source != Some(&item.source_key) {
                writeln!(output, "\nSource: {}", serde_json::json!(item.source_key))?;
                source = Some(&item.source_key);
            }
            writeln!(
                output,
                "  {} {}: {}",
                serde_json::json!(item.outcome),
                serde_json::json!(item.step_key),
                serde_json::json!(item.summary),
            )?;
            if let Some(id) = &item.destination_id {
                writeln!(output, "    Destination: {}", serde_json::json!(id))?;
            }
            if !item.payload.is_empty() {
                writeln!(output, "    Values: {}", serde_json::json!(item.payload))?;
            }
        }
        if plan.items.is_empty() {
            writeln!(output, "No source shifts overlap this range.")?;
        }
        writeln!(output, "Plan digest: {digest}")?;
        if blocked {
            writeln!(output, "Resolve the blocked items before applying.")?;
        }
        if !live {
            writeln!(
                output,
                "Fixture preview only. Obtain a live dry-run before applying."
            )?;
        }
    }
    Ok(if blocked { 1 } else { 0 })
}
