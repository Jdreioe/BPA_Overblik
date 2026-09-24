//! Anonymous, read-only iCal (ICS) feeds from any calendar service.
//! A feed link usually carries a secret token, so the links are kept in the
//! OS keyring; only the helper rule is stored in setup.json.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use chrono_tz::Tz;
use futures_util::future::try_join_all;
use reqwest::{Client, StatusCode, Url};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{LiveError, SourceReadError};
use crate::{
    ical::{parse_feed, FeedIssue, HelperRule, ParsedFeed},
    standard_time::StandardTimes,
    SourceShift,
};

/// More than any real shift calendar, small enough to read safely.
const MAX_BYTES: usize = 10_000_000;

#[derive(Clone)]
pub(crate) struct Feed {
    pub url: Url,
    /// Stable, non-secret name for this feed in setup and sync history.
    pub id: String,
}

#[derive(Clone)]
pub(crate) struct IcalAccess {
    pub feeds: Vec<Feed>,
    pub rule: HelperRule,
    /// Helpers setup explicitly left out. Their events are skipped rather
    /// than reported as unmapped.
    pub excluded: BTreeSet<String>,
}

impl IcalAccess {
    /// Names the whole connection in the account scope.
    pub fn source_id(&self) -> String {
        let mut hasher = Sha256::new();
        for feed in &self.feeds {
            hasher.update(feed.url.as_str().as_bytes());
            hasher.update([0]);
        }
        format!("ical-{:x}", hasher.finalize())[..30].into()
    }
}

/// Accept an `https://` or `webcal://` feed link from any host.
pub(crate) fn parse_link(link: &str) -> Result<Feed, LiveError> {
    const BAD: LiveError = LiveError(
        "Indsæt kalenderens iCal-link. Det starter med https:// eller webcal:// og ender ofte på .ics.",
    );
    let link = link.trim();
    let mut url = Url::parse(link).map_err(|_| BAD)?;
    if url.scheme() == "webcal" {
        // A scheme can only be swapped for a special one by reparsing.
        url = Url::parse(&format!("https{}", &link[link.find(':').ok_or(BAD)?..]))
            .map_err(|_| BAD)?;
    }
    if url.scheme() == "http" {
        return Err(LiveError(
            "Linket er ikke krypteret (http://). Brug kalenderens https://-link.",
        ));
    }
    if url.scheme() != "https" || url.host_str().is_none_or(str::is_empty) {
        return Err(BAD);
    }
    url.set_fragment(None);
    let id = format!("ical-{:x}", Sha256::digest(url.as_str().as_bytes()))[..30].into();
    Ok(Feed { url, id })
}

/// Parse and de-duplicate the links a person entered.
pub(crate) fn parse_links(links: &[String]) -> Result<Vec<Feed>, LiveError> {
    let mut feeds: Vec<Feed> = Vec::new();
    for link in links.iter().filter(|link| !link.trim().is_empty()) {
        let feed = parse_link(link)?;
        if !feeds.iter().any(|known| known.id == feed.id) {
            feeds.push(feed);
        }
    }
    if feeds.is_empty() {
        return Err(LiveError("Indsæt mindst ét iCal-link."));
    }
    Ok(feeds)
}

pub(crate) fn access(
    links: &[String],
    rule: HelperRule,
    excluded: BTreeSet<String>,
) -> Result<IcalAccess, LiveError> {
    rule.validate().map_err(LiveError)?;
    Ok(IcalAccess {
        feeds: parse_links(links)?,
        rule,
        excluded,
    })
}

fn client() -> Result<Client, LiveError> {
    Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("teamup-shift-sync/0.1")
        .build()
        .map_err(|_| LiveError("Kalenderen kunne ikke åbnes."))
}

async fn fetch(client: &Client, feed: &Feed) -> Result<String, LiveError> {
    let response = client
        .get(feed.url.clone())
        .header("Accept", "text/calendar, */*;q=0.5")
        .send()
        .await
        .map_err(|_| {
            LiveError("Kalenderen kunne ikke hentes. Kontrollér forbindelsen og linket.")
        })?;
    match response.status() {
        status if status.is_success() => {}
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            return Err(LiveError(
                "Kalenderen afviste linket. Kontrollér, at kalenderen er delt, og at linket ikke er nulstillet.",
            ))
        }
        StatusCode::NOT_FOUND | StatusCode::GONE => {
            return Err(LiveError(
                "Kalenderen findes ikke på linket. Kopiér iCal-linket igen.",
            ))
        }
        _ => {
            return Err(LiveError(
                "Kalendertjenesten svarede ikke som forventet. Prøv igen om lidt.",
            ))
        }
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BYTES as u64)
    {
        return Err(LiveError(
            "Kalenderen er for stor til at blive læst sikkert.",
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| LiveError("Kalenderen kunne ikke læses."))?;
    calendar_text(&bytes)
}

/// Refuse anything that is not an iCalendar document, such as a login page.
fn calendar_text(bytes: &[u8]) -> Result<String, LiveError> {
    if bytes.len() > MAX_BYTES {
        return Err(LiveError(
            "Kalenderen er for stor til at blive læst sikkert.",
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| NOT_A_CALENDAR)?;
    let start = text.trim_start_matches('\u{feff}').trim_start();
    if !start
        .get(..15)
        .is_some_and(|head| head.eq_ignore_ascii_case("BEGIN:VCALENDAR"))
    {
        return Err(NOT_A_CALENDAR);
    }
    Ok(text.to_owned())
}

const NOT_A_CALENDAR: LiveError = LiveError(
    "Linket gav ikke en kalender. Brug kalenderens iCal-link (.ics), ikke linket til webvisningen.",
);

async fn read_feeds(
    access: &IcalAccess,
    zone: Tz,
    from: NaiveDate,
    to: NaiveDate,
    standard: &StandardTimes,
    feeds: &[&Feed],
) -> Result<Vec<ParsedFeed>, LiveError> {
    let client = client()?;
    let texts = try_join_all(feeds.iter().map(|feed| fetch(&client, feed))).await?;
    feeds
        .iter()
        .zip(texts)
        .map(|(feed, text)| {
            parse_feed(&text, &feed.id, &access.rule, zone, from, to, standard).map_err(LiveError)
        })
        .collect()
}

/// List what setup maps: one choice per feed, or one per helper named in a
/// shared feed from four weeks back to half a year ahead.
pub(crate) async fn source_catalog(
    access: &IcalAccess,
    zone: Tz,
    today: NaiveDate,
    standard: &StandardTimes,
) -> Result<(Value, Value, String), LiveError> {
    let from = today - chrono::Duration::weeks(4);
    let to = today + chrono::Duration::weeks(26);
    let feeds: Vec<_> = access.feeds.iter().collect();
    let parsed = read_feeds(access, zone, from, to, standard, &feeds).await?;
    let shifts: usize = parsed.iter().map(|feed| feed.shifts.len()).sum();
    let issues: usize = parsed.iter().map(|feed| feed.issues.len()).sum();
    let choices: Vec<Value> = match access.rule {
        HelperRule::Feed => access
            .feeds
            .iter()
            .zip(&parsed)
            .enumerate()
            .map(|(index, (feed, parsed))| {
                let name = parsed
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("Kalender {}", index + 1));
                json!({"id": feed.id, "name": name})
            })
            .collect(),
        HelperRule::Title { .. } => {
            let mut helpers = std::collections::BTreeMap::new();
            for feed in &parsed {
                for (key, name) in &feed.helpers {
                    helpers
                        .entry(key.clone())
                        .and_modify(|shown: &mut String| {
                            if name < shown {
                                shown.clone_from(name);
                            }
                        })
                        .or_insert_with(|| name.clone());
                }
            }
            if helpers.is_empty() {
                return Err(LiveError(
                    "Kalenderen nævner ingen hjælpere i titlerne. Kontrollér, hvor navnet står i titlen.",
                ));
            }
            helpers
                .into_iter()
                .map(|(key, name)| json!({"id": key, "name": name}))
                .collect()
        }
    };
    // Counts only, so the result can be shown without naming anyone.
    let mut notice = format!(
        "Kalenderen er læst: {} vagter{}.",
        shifts,
        match access.rule {
            HelperRule::Feed => String::new(),
            HelperRule::Title { .. } => format!(" og {} hjælpere", choices.len()),
        }
    );
    if issues > 0 {
        notice.push_str(&format!(
            " {} skal rettes, før deres uge kan overføres.",
            match issues {
                1 => "1 vagt".to_owned(),
                count => format!("{count} vagter"),
            }
        ));
    }
    Ok((Value::Array(choices), json!({}), notice))
}

/// Read one week. Under [`HelperRule::Feed`] only feeds mapped to a helper
/// are fetched; a shared feed skips explicitly excluded helpers, and the
/// planner flags any name setup has not seen.
pub(crate) async fn read(
    access: &IcalAccess,
    zone: Tz,
    from: NaiveDate,
    to: NaiveDate,
    standard: &StandardTimes,
    helpers: &std::collections::BTreeMap<String, crate::HelperMapping>,
) -> Result<Vec<SourceShift>, SourceReadError> {
    let feeds: Vec<_> = access
        .feeds
        .iter()
        .filter(|feed| match access.rule {
            HelperRule::Feed => helpers.contains_key(&feed.id),
            HelperRule::Title { .. } => true,
        })
        .collect();
    let parsed = read_feeds(access, zone, from, to, standard, &feeds).await?;
    let mut shifts = Vec::new();
    for feed in parsed {
        if let Some(issue) = feed.issues.iter().find(|issue| !skipped(access, issue)) {
            return Err(SourceReadError::Review(format!(
                "{} Ret det i kalenderen, så kan ugen overføres.",
                issue.message()
            )));
        }
        shifts.extend(
            feed.shifts
                .into_iter()
                .filter(|shift| !access.excluded.contains(&shift.helper_key)),
        );
    }
    shifts.sort_by_key(|shift| shift.starts_at);
    Ok(shifts)
}

/// An issue on an excluded helper's event does not block the week.
fn skipped(access: &IcalAccess, issue: &FeedIssue) -> bool {
    matches!(access.rule, HelperRule::Title { .. })
        && issue
            .helper
            .as_deref()
            .is_some_and(|name| access.excluded.contains(&crate::ical::helper_key(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_and_webcal_links_from_any_host_are_accepted() {
        let feed = parse_link(" webcal://p42-caldav.icloud.com/published/2/abc ").unwrap();
        assert_eq!(
            feed.url.as_str(),
            "https://p42-caldav.icloud.com/published/2/abc"
        );
        let google = parse_link(
            "https://calendar.google.com/calendar/ical/x%40group.calendar.google.com/private-abc/basic.ics",
        )
        .unwrap();
        assert!(google.id.starts_with("ical-"));
        assert_eq!(google.id.len(), 30);
        // The id never reveals the link's token.
        assert!(!google.id.contains("abc"));
        for link in [
            "http://example.com/cal.ics",
            "ftp://example.com/cal.ics",
            "file:///etc/passwd",
            "not a link",
            "https://",
        ] {
            assert!(parse_link(link).is_err(), "{link} should be rejected");
        }
    }

    #[test]
    fn the_same_link_twice_is_one_feed() {
        let links = vec![
            "https://example.com/a.ics".to_owned(),
            " https://example.com/a.ics#x".to_owned(),
            String::new(),
            "https://example.com/b.ics".to_owned(),
        ];
        assert_eq!(parse_links(&links).unwrap().len(), 2);
        assert!(parse_links(&[String::new()]).is_err());
    }

    #[test]
    fn only_calendar_documents_are_read() {
        assert!(calendar_text(b"\xef\xbb\xbfBEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").is_ok());
        assert!(calendar_text(b"begin:vcalendar\nend:vcalendar\n").is_ok());
        assert!(calendar_text(b"<!DOCTYPE html><html>Log ind</html>").is_err());
        assert!(calendar_text(&[0xff, 0xfe, 0x00]).is_err());
    }
}
