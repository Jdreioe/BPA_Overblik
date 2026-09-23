//! Anonymous, read-only CSV export from a link-shared Google Sheets tab.
//! The link acts as a capability, so its ID is kept in the OS keyring.

use chrono::{NaiveDate, TimeZone};
use chrono_tz::Tz;
use reqwest::{Client, Url};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::LiveError;
use crate::{
    sheets::{csv_cells, parse_cells_with_standard, SheetIssue, SheetLayout},
    standard_time::StandardTimes,
    SourceShift,
};

#[derive(Clone)]
pub(crate) struct SheetAccess {
    pub id: String,
    pub gid: String,
    pub layout: SheetLayout,
}

impl SheetAccess {
    pub fn source_id(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.id.as_bytes());
        hasher.update([0]);
        hasher.update(self.gid.as_bytes());
        format!("sheet-{:x}", hasher.finalize())[..30].into()
    }

    fn export_url(&self) -> Result<Url, LiveError> {
        let mut url = Url::parse("https://docs.google.com/spreadsheets/d/")
            .expect("constant Google Sheets URL");
        url.path_segments_mut()
            .map_err(|_| LiveError("Ugyldigt regnearkslink."))?
            .pop_if_empty()
            .push(&self.id)
            .push("export");
        url.query_pairs_mut()
            .append_pair("format", "csv")
            .append_pair("gid", &self.gid);
        Ok(url)
    }
}

/// Accept only a Google Sheets document link. Never fetch a caller-chosen host.
pub(crate) fn parse_link(link: &str, layout: SheetLayout) -> Result<SheetAccess, LiveError> {
    let url = Url::parse(link.trim()).map_err(|_| LiveError("Indsæt et Google Sheets-link."))?;
    if url.scheme() != "https" || url.host_str() != Some("docs.google.com") {
        return Err(LiveError(
            "Indsæt et Google Sheets-link fra docs.google.com.",
        ));
    }
    let path: Vec<_> = url
        .path_segments()
        .ok_or(LiveError("Ugyldigt regnearkslink."))?
        .collect();
    if path.len() < 4 || path[0] != "spreadsheets" || path[1] != "d" || path[3] != "edit" {
        return Err(LiveError("Indsæt linket fra regnearkets adressefelt."));
    }
    let id = path[2];
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(LiveError("Ugyldigt regnearkslink."));
    }
    let gid = url
        .query_pairs()
        .find(|(key, _)| key == "gid")
        .map(|(_, value)| value.into_owned())
        .or_else(|| {
            url.fragment()
                .and_then(|fragment| fragment.strip_prefix("gid="))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "0".into());
    if gid.is_empty() || !gid.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(LiveError(
            "Vælg en fane i regnearket, og kopiér dens link igen.",
        ));
    }
    layout.validate().map_err(LiveError)?;
    Ok(SheetAccess {
        id: id.into(),
        gid,
        layout,
    })
}

fn client() -> Result<Client, LiveError> {
    Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("teamup-shift-sync/0.1")
        .build()
        .map_err(|_| LiveError("Regnearket kunne ikke åbnes."))
}

async fn fetch(access: &SheetAccess) -> Result<Vec<Vec<String>>, LiveError> {
    let response = client()?
        .get(access.export_url()?)
        .send()
        .await
        .map_err(|_| {
            LiveError("Regnearket kunne ikke hentes. Kontrollér forbindelsen og delingsadgangen.")
        })?;
    if !response.status().is_success() {
        return Err(LiveError(
            "Google afviste regnearket. Kontrollér, at alle med linket må se det.",
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > 10_000_000)
    {
        return Err(LiveError(
            "Regnearket er for stort til at blive læst sikkert.",
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| LiveError("Regnearket kunne ikke læses."))?;
    if bytes.len() > 10_000_000
        || bytes.starts_with(b"<!DOCTYPE html")
        || bytes.starts_with(b"<html")
    {
        return Err(LiveError(
            "Google gav ikke et læsbart CSV-ark. Kontrollér delingsadgangen.",
        ));
    }
    csv_cells(&bytes).map_err(LiveError)
}

pub(crate) async fn source_catalog(
    access: &SheetAccess,
    zone: Tz,
    standard: &StandardTimes,
) -> Result<(Value, Value, String), LiveError> {
    let grid = fetch(access).await?;
    let today = chrono::Utc::now().with_timezone(&zone).date_naive();
    let parsed = parse_cells_with_standard(
        &grid,
        &access.layout,
        &access.source_id(),
        zone,
        today,
        standard,
    )
    .map_err(LiveError)?;
    if parsed.helpers.is_empty() {
        return Err(LiveError(
            "Regnearket indeholder ingen læsbare vagter. Kontrollér opsætningen.",
        ));
    }
    let choices = parsed
        .helpers
        .iter()
        .map(|name| json!({"id": name, "name": name}))
        .collect();
    // Counts only, so a partly read sheet is visible without showing cells.
    // Bad cells in other weeks must not block setup; reading a week still
    // refuses every issue that can touch it.
    let mut notice = format!(
        "Regnearket er læst: {} vagter og {} hjælpere.",
        parsed.shifts.len(),
        parsed.helpers.len()
    );
    if !parsed.issues.is_empty() {
        notice.push_str(&format!(
            " {} skal rettes, før deres uge kan overføres.",
            match parsed.issues.len() {
                1 => "1 vagt".to_owned(),
                count => format!("{count} vagter"),
            }
        ));
    }
    Ok((Value::Array(choices), json!({}), notice))
}

pub(crate) async fn read(
    access: &SheetAccess,
    zone: Tz,
    from: NaiveDate,
    to: NaiveDate,
    standard: &StandardTimes,
) -> Result<Vec<SourceShift>, String> {
    let grid = fetch(access).await.map_err(|error| error.to_string())?;
    let parsed = parse_cells_with_standard(
        &grid,
        &access.layout,
        &access.source_id(),
        zone,
        from,
        standard,
    )
    .map_err(str::to_owned)?;
    if let Some(issue) = parsed
        .issues
        .iter()
        .find(|issue| touches_week(issue, from, to))
    {
        return Err(format!(
            "{} Ret det i regnearket, så kan ugen overføres.",
            issue.message(&access.layout.sps_label)
        ));
    }
    week_shifts(parsed.shifts, zone, from, to)
}

/// A shift can end the day after its date, so the day before the week counts.
/// An unreadable date could belong to any week.
fn touches_week(issue: &SheetIssue, from: NaiveDate, to: NaiveDate) -> bool {
    issue
        .date
        .is_none_or(|date| date >= from.pred_opt().unwrap_or(from) && date <= to)
}

fn week_shifts(
    shifts: Vec<SourceShift>,
    zone: Tz,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<SourceShift>, String> {
    let start = zone
        .from_local_datetime(&from.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .ok_or("Ugens start kan ikke læses i den valgte tidszone.")?;
    let after = to
        .succ_opt()
        .and_then(|day| day.and_hms_opt(0, 0, 0))
        .and_then(|midnight| zone.from_local_datetime(&midnight).single())
        .ok_or("Ugens slutning kan ikke læses i den valgte tidszone.")?;
    Ok(shifts
        .into_iter()
        .filter(|shift| shift.ends_at > start && shift.starts_at < after)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheets::{parse_cells, CellOffset, IssueKind, TimeCells};

    /// Date, helper two rows below it, time three rows below it.
    fn layout() -> SheetLayout {
        SheetLayout {
            date_format: "%d/%m/%y".into(),
            sps_label: String::new(),
            helper: CellOffset { row: 2, column: 0 },
            time: TimeCells::Range {
                cell: CellOffset { row: 3, column: 0 },
            },
            sps: None,
            title: None,
        }
    }

    #[test]
    fn only_a_google_sheet_link_with_a_numeric_tab_is_accepted() {
        let layout = layout();
        let access = parse_link(
            "https://docs.google.com/spreadsheets/d/abc-123/edit#gid=42",
            layout.clone(),
        )
        .unwrap();
        assert_eq!(access.gid, "42");
        assert_eq!(
            access.export_url().unwrap().as_str(),
            "https://docs.google.com/spreadsheets/d/abc-123/export?format=csv&gid=42"
        );
        for link in [
            "http://docs.google.com/spreadsheets/d/x/edit",
            "https://evil.example/spreadsheets/d/x/edit",
            "https://docs.google.com/spreadsheets/d/x/edit#gid=oops",
        ] {
            assert!(parse_link(link, layout.clone()).is_err());
        }
    }

    #[test]
    fn reading_a_week_keeps_only_overlapping_shifts() {
        let layout = layout();
        let grid =
            csv_cells(b"02/11/26\nMandag\nAlex\n8-24\n\n09/11/26\nMandag\nJoe\n8-24\n").unwrap();
        let parsed = parse_cells(
            &grid,
            &layout,
            "sheet",
            chrono_tz::Europe::Copenhagen,
            NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
        )
        .unwrap();
        let week = week_shifts(
            parsed.shifts,
            chrono_tz::Europe::Copenhagen,
            NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
            NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
        )
        .unwrap();
        assert_eq!(week.len(), 1);
        assert_eq!(week[0].helper_key, "Alex");
    }

    #[test]
    fn only_issues_that_can_touch_the_week_block_it() {
        let day = |d| NaiveDate::from_ymd_opt(2026, 11, d);
        let issue = |date| SheetIssue {
            cell: "A1".into(),
            kind: IssueKind::MissingTime,
            date,
            helper: None,
        };
        let (from, to) = (day(9).unwrap(), day(15).unwrap());
        // Sunday's shift can run past midnight into Monday.
        assert!(touches_week(&issue(day(8)), from, to));
        assert!(touches_week(&issue(day(15)), from, to));
        assert!(touches_week(&issue(None), from, to));
        assert!(!touches_week(&issue(day(7)), from, to));
        assert!(!touches_week(&issue(day(16)), from, to));
    }
}
