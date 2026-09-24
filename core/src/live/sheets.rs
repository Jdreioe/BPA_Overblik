//! Read-only spreadsheet sources: a Google Sheets tab, a shared link to a
//! workbook or CSV file, or a file on this computer. Every one becomes the
//! same grid of displayed text; see [`crate::workbook`].
//!
//! A share link acts as a capability, so it is kept in the OS keyring. So is
//! a local path, which can contain the user's name. Neither ever reaches
//! diagnostics.

use std::path::PathBuf;

use chrono::{NaiveDate, TimeZone};
use chrono_tz::Tz;
use reqwest::{Client, Url};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::LiveError;
use crate::{
    sheets::{parse_cells_with_standard, SheetIssue, SheetLayout},
    standard_time::StandardTimes,
    workbook, SourceShift,
};

/// Larger files are refused rather than read.
const MAX_BYTES: usize = 10_000_000;

#[derive(Clone)]
pub(crate) struct SheetAccess {
    pub location: Location,
    /// The workbook tab, by name. A Google tab is its `gid`; a CSV has none.
    pub tab: Option<String>,
    pub layout: SheetLayout,
}

/// Where the spreadsheet is read from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Location {
    /// A Google Sheets tab, read through its CSV export.
    Google { id: String, gid: String },
    /// A direct download link, already rewritten from its share link.
    Link(Url),
    /// A file on this computer, read again for every preview and transfer.
    File(PathBuf),
}

impl SheetAccess {
    /// The sync history scope. A Google tab keeps the id it always had, so
    /// existing history survives; moving a plan elsewhere starts a new one.
    pub fn source_id(&self) -> String {
        let mut hasher = Sha256::new();
        match &self.location {
            Location::Google { id, gid } => {
                hasher.update(id.as_bytes());
                hasher.update([0]);
                hasher.update(gid.as_bytes());
            }
            Location::Link(url) => {
                hasher.update(b"link\0");
                hasher.update(url.as_str().as_bytes());
            }
            Location::File(path) => {
                hasher.update(b"file\0");
                hasher.update(path.to_string_lossy().as_bytes());
            }
        }
        if !matches!(self.location, Location::Google { .. }) {
            hasher.update([0]);
            hasher.update(self.tab.as_deref().unwrap_or("").as_bytes());
        }
        format!("sheet-{:x}", hasher.finalize())[..30].into()
    }
}

/// The saved access for a connected sheet: `{"link": …}` or `{"file": …}`,
/// with the workbook tab if there is one.
pub(crate) fn access(secret: &Value, layout: SheetLayout) -> Result<SheetAccess, LiveError> {
    layout.validate().map_err(LiveError)?;
    let location = match (secret["link"].as_str(), secret["file"].as_str()) {
        (Some(link), None) => parse_link(link)?,
        (None, Some(path)) => parse_file(path)?,
        _ => return Err(LiveError("Regnearkets gemte forbindelse er ugyldig.")),
    };
    Ok(SheetAccess {
        location,
        tab: secret["tab"].as_str().map(str::to_owned),
        layout,
    })
}

pub(crate) fn parse_file(path: &str) -> Result<Location, LiveError> {
    let path = PathBuf::from(path.trim());
    if !path.is_absolute() {
        return Err(LiveError("Vælg regnearksfilen igen."));
    }
    Ok(Location::File(path))
}

/// Turn a share link into the link that downloads the file. Only `https://`
/// is fetched, and the known providers' share pages are rewritten to their
/// download address:
///
/// - Google Sheets: the tab's CSV export, or a "Publish to web" file.
/// - OneDrive: the anonymous shares download.
/// - SharePoint: `download=1`.
/// - Dropbox: `dl=1`.
/// - Nextcloud and ownCloud: the share's `/download`.
///
/// Any other link must already download the file itself.
pub(crate) fn parse_link(link: &str) -> Result<Location, LiveError> {
    let url = Url::parse(link.trim()).map_err(|_| LiveError("Indsæt et link til regnearket."))?;
    if url.scheme() != "https" {
        return Err(LiveError(
            "Linket er ikke krypteret. Brug regnearkets https://-link.",
        ));
    }
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let path: Vec<String> = url
        .path_segments()
        .map(|segments| segments.map(str::to_owned).collect())
        .unwrap_or_default();
    if host == "docs.google.com" && path.first().map(String::as_str) == Some("spreadsheets") {
        return google(&url, &path);
    }
    let mut url = url;
    if host == "1drv.ms" || host == "onedrive.live.com" {
        return onedrive(&url);
    }
    if host.ends_with(".sharepoint.com") {
        set_query(&mut url, "download", "1");
    } else if host == "dropbox.com" || host.ends_with(".dropbox.com") {
        set_query(&mut url, "dl", "1");
    } else if let Some(at) = path.iter().position(|segment| segment == "s") {
        // Nextcloud and ownCloud: /s/<token> or /index.php/s/<token>.
        if path.len() == at + 2 && !path[at + 1].is_empty() {
            url.path_segments_mut()
                .map_err(|_| LiveError("Ugyldigt regnearkslink."))?
                .push("download");
        }
    }
    Ok(Location::Link(url))
}

fn google(url: &Url, path: &[String]) -> Result<Location, LiveError> {
    // Published to the web: /spreadsheets/d/e/<id>/pub or /pubhtml.
    if path.get(1).map(String::as_str) == Some("d") && path.get(2).map(String::as_str) == Some("e")
    {
        let mut url = url.clone();
        if path.get(4).map(String::as_str) == Some("pubhtml") {
            let id = path[3].clone();
            url.set_path(&format!("/spreadsheets/d/e/{id}/pub"));
        }
        if !url.query_pairs().any(|(key, _)| key == "output") {
            set_query(&mut url, "output", "xlsx");
        }
        return Ok(Location::Link(url));
    }
    if path.len() < 4 || path[1] != "d" || path[3] != "edit" {
        return Err(LiveError("Indsæt linket fra regnearkets adressefelt."));
    }
    let id = &path[2];
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
    Ok(Location::Google {
        id: id.clone(),
        gid,
    })
}

/// OneDrive share links download through the shares API, addressed by the
/// share link itself in unpadded base64url.
fn onedrive(url: &Url) -> Result<Location, LiveError> {
    let encoded = base64url(url.as_str().as_bytes());
    Url::parse(&format!(
        "https://api.onedrive.com/v1.0/shares/u!{encoded}/root/content"
    ))
    .map(Location::Link)
    .map_err(|_| LiveError("Ugyldigt regnearkslink."))
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn set_query(url: &mut Url, key: &str, value: &str) {
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| k != key)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(kept)
        .append_pair(key, value);
}

impl Location {
    fn download_url(&self) -> Option<Url> {
        match self {
            Location::Google { id, gid } => {
                let mut url = Url::parse("https://docs.google.com/spreadsheets/d/")
                    .expect("constant Google Sheets URL");
                url.path_segments_mut()
                    .ok()?
                    .pop_if_empty()
                    .push(id)
                    .push("export");
                url.query_pairs_mut()
                    .append_pair("format", "csv")
                    .append_pair("gid", gid);
                Some(url)
            }
            Location::Link(url) => Some(url.clone()),
            Location::File(_) => None,
        }
    }
}

fn client() -> Result<Client, LiveError> {
    // Share links redirect through their provider's download service, but
    // never away from https.
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 || attempt.url().scheme() != "https" {
            attempt.stop()
        } else {
            attempt.follow()
        }
    });
    Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(policy)
        .user_agent("teamup-shift-sync/0.1")
        .build()
        .map_err(|_| LiveError("Regnearket kunne ikke åbnes."))
}

/// The file's bytes, from the link or from disk, at most [`MAX_BYTES`].
pub(crate) async fn fetch(location: &Location) -> Result<Vec<u8>, LiveError> {
    let too_large = LiveError("Regnearket er for stort til at blive læst sikkert.");
    let url = match location {
        Location::File(path) => {
            let missing = LiveError(
                "Regnearksfilen findes ikke længere. Vælg filen igen under Indstillinger → Udbydere.",
            );
            let size = tokio::fs::metadata(path)
                .await
                .map_err(|_| missing.clone())?
                .len();
            if size > MAX_BYTES as u64 {
                return Err(too_large);
            }
            return tokio::fs::read(path).await.map_err(|_| missing);
        }
        _ => location
            .download_url()
            .ok_or(LiveError("Ugyldigt regnearkslink."))?,
    };
    let google = matches!(location, Location::Google { .. });
    let mut response = client()?.get(url).send().await.map_err(|_| {
        LiveError("Regnearket kunne ikke hentes. Kontrollér forbindelsen og delingsadgangen.")
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(LiveError(match status.as_u16() {
            _ if google => "Google afviste regnearket. Kontrollér, at alle med linket må se det.",
            401 | 403 => "Linket kræver login. Del regnearket, så alle med linket kan se det.",
            404 | 410 => {
                "Regnearket findes ikke på linket længere. Del det igen, og indsæt det nye link."
            }
            _ => "Regnearket kunne ikke hentes. Prøv igen om lidt.",
        }));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BYTES as u64)
    {
        return Err(too_large);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| LiveError("Regnearket kunne ikke læses."))?
    {
        if bytes.len() + chunk.len() > MAX_BYTES {
            return Err(too_large);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// The tab names of what `location` holds. A CSV or a Google tab has none.
pub(crate) fn tabs(bytes: &[u8]) -> Result<Vec<String>, LiveError> {
    workbook::tabs(bytes).map_err(LiveError)
}

/// The displayed text of the chosen tab.
pub(crate) fn grid(access: &SheetAccess, bytes: &[u8]) -> Result<Vec<Vec<String>>, LiveError> {
    let tab = match access.location {
        Location::Google { .. } => None,
        _ => access.tab.as_deref(),
    };
    workbook::cells(bytes, tab).map_err(LiveError)
}

async fn read_grid(access: &SheetAccess) -> Result<Vec<Vec<String>>, LiveError> {
    let bytes = fetch(&access.location).await?;
    grid(access, &bytes)
}

pub(crate) async fn source_catalog(
    access: &SheetAccess,
    zone: Tz,
    standard: &StandardTimes,
) -> Result<(Value, Value, String), LiveError> {
    let grid = read_grid(access).await?;
    catalog(&grid, access, zone, standard)
}

/// Helpers and a counts-only notice from an already read grid.
pub(crate) fn catalog(
    grid: &[Vec<String>],
    access: &SheetAccess,
    zone: Tz,
    standard: &StandardTimes,
) -> Result<(Value, Value, String), LiveError> {
    let today = chrono::Utc::now().with_timezone(&zone).date_naive();
    let parsed = parse_cells_with_standard(
        grid,
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
    let grid = read_grid(access).await.map_err(|error| error.to_string())?;
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
    use crate::sheets::{csv_cells, parse_cells, CellOffset, IssueKind, TimeCells};

    /// Date, helper two rows below it, time three rows below it.
    fn layout() -> SheetLayout {
        SheetLayout {
            date_format: "%d/%m/%y".into(),
            sps_label: String::new(),
            helper: CellOffset { row: 2, column: 0 },
            time: Some(TimeCells::Range {
                cell: CellOffset { row: 3, column: 0 },
            }),
            sps: None,
            title: None,
        }
    }

    #[test]
    fn a_google_sheet_link_keeps_its_tab_and_its_history() {
        let location =
            parse_link("https://docs.google.com/spreadsheets/d/abc-123/edit#gid=42").unwrap();
        assert_eq!(
            location.download_url().unwrap().as_str(),
            "https://docs.google.com/spreadsheets/d/abc-123/export?format=csv&gid=42"
        );
        let access = SheetAccess {
            location,
            tab: None,
            layout: layout(),
        };
        // The id every existing Google Sheets setup already records history under.
        assert_eq!(access.source_id(), "sheet-b579fff245cb8254968be5df");
        for link in [
            "http://docs.google.com/spreadsheets/d/x/edit",
            "https://docs.google.com/spreadsheets/d/x/edit#gid=oops",
        ] {
            assert!(parse_link(link).is_err());
        }
    }

    #[test]
    fn share_links_become_download_links() {
        let download = |link: &str| match parse_link(link).unwrap() {
            Location::Link(url) => url.to_string(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            download("https://www.dropbox.com/scl/fi/abc/plan.xlsx?rlkey=k&dl=0"),
            "https://www.dropbox.com/scl/fi/abc/plan.xlsx?rlkey=k&dl=1"
        );
        assert_eq!(
            download("https://sky.example.dk/index.php/s/Tok3n"),
            "https://sky.example.dk/index.php/s/Tok3n/download"
        );
        assert_eq!(
            download("https://firma.sharepoint.com/:x:/s/team/EAbc?e=x1"),
            "https://firma.sharepoint.com/:x:/s/team/EAbc?e=x1&download=1"
        );
        assert_eq!(
            download("https://docs.google.com/spreadsheets/d/e/2PACX-1/pubhtml"),
            "https://docs.google.com/spreadsheets/d/e/2PACX-1/pub?output=xlsx"
        );
        // The shares API addresses a link by its unpadded base64url form.
        assert_eq!(
            download("https://1drv.ms/x/s!AbC"),
            "https://api.onedrive.com/v1.0/shares/u!aHR0cHM6Ly8xZHJ2Lm1zL3gvcyFBYkM/root/content"
        );
        assert_eq!(
            download("https://example.com/plan.csv"),
            "https://example.com/plan.csv"
        );
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
