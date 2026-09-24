//! Resumable setup. Secrets never enter the setup document.
//!
//! The document records only confirmed choices and the handle its TeamUp
//! credentials are stored under in the OS keyring. Every stage is saved, so a
//! failed network call can be retried after a restart without redoing earlier
//! work. Nothing here writes to a destination.

use std::path::{Path, PathBuf};

use chrono::{Datelike, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use serde_json::{json, Map, Value};

use super::{
    config::{ical_access, read_credential, store_credential},
    destinations::build_catalog,
    ical, rows, sheets, teamup, text, BrowserSessions, LiveError, INVALID,
};
use crate::standard_time::StandardTimes;

const UNREADABLE: LiveError =
    LiveError("Den gemte opsætning kunne ikke læses. Gendan setup.json fra din sikkerhedskopi.");
const CHANGED: &str =
    "Navne, ansættelser eller kontovalg er ændret. Hent mulighederne og bekræft opsætningen igen.";

/// Resolve a TeamUp share link to its calendar key (`ks…`).
fn calendar_reference(link: &str) -> Result<String, LiveError> {
    const BAD: LiveError = LiveError("Indsæt et TeamUp-kalenderlink, der starter med https://teamup.com/ks. Brug et delt link med adgang til vagter og kommentarer.");
    let url = reqwest::Url::parse(link.trim()).map_err(|_| BAD)?;
    if url.scheme() != "https" || !matches!(url.host_str(), Some("teamup.com" | "www.teamup.com")) {
        return Err(BAD);
    }
    let key = url.path().trim_matches('/').split('/').next().unwrap_or("");
    // The same shape Python accepts: `ks` then one or more alphanumerics.
    if key.len() < 3
        || !key.starts_with("ks")
        || !key[2..].bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err(BAD);
    }
    Ok(key.to_owned())
}

/// The single choice whose name matches, or nothing. A suggestion is never a
/// confirmation: an ambiguous name must be resolved by the person setting up.
fn suggested(name: &str, choices: &Value) -> String {
    let target = name.to_lowercase();
    let matching: Vec<&str> = choices
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|row| {
            row["name"]
                .as_str()
                .is_some_and(|n| n.to_lowercase() == target)
        })
        .filter_map(|row| row["id"].as_str())
        .collect();
    if matching.len() == 1 {
        matching[0].to_owned()
    } else {
        String::new()
    }
}

fn selected<'a>(choices: &'a Value, identifier: &Value) -> Result<&'a Value, LiveError> {
    let matching: Vec<_> = rows(choices)?
        .iter()
        .filter(|row| row["id"] == *identifier)
        .collect();
    if matching.len() != 1 {
        return Err(LiveError(
            "Et valg mangler eller er ikke længere tilgængeligt. Hent mulighederne igen.",
        ));
    }
    Ok(matching[0])
}

/// A shared calendar's helpers are read from a moving window, so a helper
/// whose last shift has passed would drop out and reset every mapping. Keep
/// helpers already listed, under the name they were listed with, and add the
/// new ones. Reconnecting the calendar starts the list afresh.
fn keep_known_helpers(known: &Value, found: Value) -> Value {
    let mut helpers = std::collections::BTreeMap::new();
    // Known rows go in last, so their names win.
    let found = found.as_array().into_iter().flatten();
    for row in found.chain(known.as_array().into_iter().flatten()) {
        if let Some(id) = row["id"].as_str() {
            helpers.insert(id.to_owned(), row.clone());
        }
    }
    Value::Array(helpers.into_values().collect())
}

fn defaults() -> Value {
    json!({
        "version": 1, "stage": "source", "source": "teamup", "credential": "", "calendars": [], "colors": {},
        "duos_enabled": true,
        "standard_times": {"everyday": "", "weekdays": {}},
        "markers": super::config::DEFAULT_MARKERS,
        "arrangements": [], "types": [], "mithf": [], "duos": [], "mappings": [],
        "arrangement": "", "registration_type": "", "account": "",
    })
}

/// A spreadsheet as the person gave it: a share link, or a file they chose.
#[derive(Clone, Debug)]
pub enum SheetSource {
    Link(String),
    File(String),
}

/// The setup document and the operations that advance it.
///
/// Every mutating operation saves before returning, so an interrupted setup
/// resumes where it stopped. Operations that contact a service are async and
/// must not run on the UI thread.
pub struct Setup {
    path: PathBuf,
    data: Value,
}

impl Setup {
    fn standard_times(&self) -> Result<StandardTimes, LiveError> {
        let standard: StandardTimes =
            serde_json::from_value(self.data["standard_times"].clone())
                .map_err(|_| LiveError("De gemte standardtider er ugyldige."))?;
        standard.validate().map_err(LiveError)?;
        Ok(standard)
    }

    pub fn set_standard_times(&mut self, standard: StandardTimes) -> Result<(), LiveError> {
        standard.validate().map_err(LiveError)?;
        self.data["standard_times"] = serde_json::to_value(standard).map_err(|_| INVALID)?;
        self.save()
    }
    /// Replace the titles that count as calendar markers.
    pub fn set_markers(&mut self, titles: &[String]) -> Result<(), LiveError> {
        self.data["markers"] = json!(crate::marker_titles(titles).map_err(LiveError)?);
        self.save()
    }
    pub fn duos_enabled(&self) -> bool {
        self.data["duos_enabled"].as_bool().unwrap_or(true)
    }

    pub fn choose_duos(&mut self, enabled: bool) -> Result<(), LiveError> {
        if self.duos_enabled() != enabled {
            self.data["duos_enabled"] = json!(enabled);
            self.data["catalog"] = Value::Null;
            self.data["mappings"] = json!([]);
            self.data["arrangement"] = json!("");
            self.data["registration_type"] = json!("");
            self.data["stage"] = json!("destinations");
            self.save()?;
        }
        Ok(())
    }
    /// Load the saved document, or start a new one. Run on a blocking thread.
    pub fn load(data_dir: &Path) -> Result<Self, LiveError> {
        let path = data_dir.join("setup.json");
        let mut data = defaults();
        if path.exists() {
            let saved: Value = std::fs::read_to_string(&path)
                .ok()
                .and_then(|document| serde_json::from_str(&document).ok())
                .ok_or(UNREADABLE)?;
            if saved["version"] != 1 {
                return Err(UNREADABLE);
            }
            for (key, value) in saved.as_object().ok_or(UNREADABLE)? {
                // Setups saved with only two sources kept the one left behind
                // under `other_source`, or `trial` while it was being tried.
                if matches!(key.as_str(), "other_source" | "trial") {
                    if let Some(source) = value["source"].as_str() {
                        let source = source.to_owned();
                        if !data["other_sources"].is_object() {
                            data["other_sources"] = json!({});
                        }
                        data["other_sources"][source] = value.clone();
                    }
                    continue;
                }
                data[key.as_str()] = value.clone();
            }
        }
        Ok(Self { path, data })
    }

    fn timezone(&self) -> Tz {
        self.data["timezone"]
            .as_str()
            .unwrap_or("Europe/Copenhagen")
            .parse()
            .unwrap_or(chrono_tz::Europe::Copenhagen)
    }
    fn today(&self) -> NaiveDate {
        Utc::now().with_timezone(&self.timezone()).date_naive()
    }
    fn credential(&self) -> Result<&str, LiveError> {
        let credential = text(&self.data["credential"])?;
        if credential.is_empty() {
            return Err(LiveError("Tilslut vagtkilden først."));
        }
        Ok(credential)
    }

    /// Everything the setup screen may display. Credentials, the recorded
    /// catalog and account ids are internal and never leave here.
    pub fn view(&self) -> Value {
        let today = self.today();
        let monday = today - Duration::days(today.weekday().num_days_from_monday().into());
        let mut view = Map::new();
        for (key, value) in self.data.as_object().into_iter().flatten() {
            if matches!(
                key.as_str(),
                "credential" | "catalog" | "imported" | "account_ids" | "other_sources" | "notice"
            ) {
                continue;
            }
            view.insert(key.clone(), value.clone());
        }
        view.insert("week_start".into(), json!(monday.to_string()));
        // Importing a legacy TOML configuration is not part of the native flow.
        view.insert("can_import".into(), json!(false));
        view.insert("has_credentials".into(), json!(self.credential().is_ok()));
        let mut connected: Vec<&str> = self.data["other_sources"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, kept)| kept["credential"].as_str().is_some_and(|c| !c.is_empty()))
            .map(|(source, _)| source.as_str())
            .collect();
        if self.credential().is_ok() {
            connected.extend(self.data["source"].as_str());
        }
        connected.sort_unstable();
        view.insert("connected_sources".into(), json!(connected));
        Value::Object(view)
    }

    /// Replace the document atomically, so an interrupted write cannot leave a
    /// half-saved setup behind. Run on a blocking thread.
    fn save(&self) -> Result<(), LiveError> {
        let parent = self.path.parent().ok_or(INVALID)?;
        std::fs::create_dir_all(parent).map_err(|_| INVALID)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| INVALID)?;
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| INVALID)?;
        let temporary = parent.join(format!(
            ".setup-{}-{}",
            std::process::id(),
            stamp.as_nanos()
        ));
        let outcome = (|| -> Result<(), LiveError> {
            use std::io::Write;
            let mut file = std::fs::File::create(&temporary).map_err(|_| INVALID)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))
                    .map_err(|_| INVALID)?;
            }
            file.write_all(self.data.to_string().as_bytes())
                .map_err(|_| INVALID)?;
            file.sync_all().map_err(|_| INVALID)?;
            drop(file);
            std::fs::rename(&temporary, &self.path).map_err(|_| INVALID)
        })();
        if outcome.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        outcome.map_err(|_| {
            LiveError("Opsætningen kunne ikke gemmes. Kontrollér diskplads og rettigheder.")
        })
    }

    /// Return to the connection step without discarding saved choices. The
    /// stage it came from is remembered, so a setup kept by `choose_source`
    /// comes back where it was, not half-edited.
    pub fn edit_source(&mut self) -> Result<(), LiveError> {
        if self.data["stage"] != "source" {
            self.data["resume_stage"] = self.data["stage"].clone();
        }
        self.data["stage"] = json!("source");
        self.save()
    }

    /// Switch sources. Each source keeps its own setup: the one left behind
    /// is kept under `other_sources` and comes back exactly as it was when
    /// that source is chosen again. A source chosen for the first time starts
    /// from the connection step, without the old source's mappings or approval.
    pub fn choose_source(&mut self, source: &str) -> Result<(), LiveError> {
        if !matches!(source, "teamup" | "sheets" | "ical") {
            return Err(LiveError("Vælg TeamUp, Regneark eller en iCal-kalender."));
        }
        if self.data["source"] == source {
            self.data["stage"] = json!("source");
            return self.save();
        }
        let object = self.data.as_object_mut().ok_or(INVALID)?;
        let mut kept = match object.remove("other_sources") {
            Some(Value::Object(kept)) => kept,
            _ => Map::new(),
        };
        let other = kept
            .remove(source)
            .filter(|other| other["source"] == source);
        let resume = object.remove("resume_stage");
        let mut left = self.data.clone();
        let standard_times = left["standard_times"].clone();
        let markers = left.get("markers").cloned();
        if let Some(stage) = resume {
            left["stage"] = stage;
        }
        match other {
            Some(other) => self.data = other,
            None => {
                self.data["source"] = json!(source);
                self.data["credential"] = json!("");
                self.data["calendars"] = json!([]);
                self.data["mappings"] = json!([]);
                self.data["catalog"] = Value::Null;
                self.data["stage"] = json!("source");
            }
        }
        self.data["standard_times"] = standard_times;
        if let Some(markers) = markers {
            self.data["markers"] = markers;
        }
        if left["credential"].as_str().is_some_and(|c| !c.is_empty()) {
            if let Some(name) = left["source"].as_str() {
                kept.insert(name.to_owned(), left);
            }
        }
        if !kept.is_empty() {
            self.data["other_sources"] = Value::Object(kept);
        }
        self.save()
    }

    /// Store new TeamUp credentials and read the calendars they expose.
    /// Returns the Danish result of the check, like [`Self::refresh_source`].
    ///
    /// The credentials are persisted before the first request, so a failed
    /// network call can be retried later instead of asking for them again.
    pub async fn connect(&mut self, link: &str, api_key: &str) -> Result<String, LiveError> {
        if self.data["source"] != "teamup" {
            return Err(LiveError("Vælg TeamUp som kilde først."));
        }
        let secret = json!({"calendar": calendar_reference(link)?, "api_key": api_key.trim()});
        if api_key.trim().is_empty() {
            return Err(LiveError("Indsæt din TeamUp API-nøgle. Du kan få hjælp af appens vedligeholder til at anmode om den."));
        }
        let credential = uuid::Uuid::new_v4().simple().to_string();
        store_credential(&credential, &secret)?;
        self.data["credential"] = json!(credential);
        self.data["stage"] = json!("source");
        self.data["mappings"] = json!([]);
        self.data["catalog"] = Value::Null;
        self.save()?;
        self.refresh_source().await
    }

    /// Connect a spreadsheet. The link or path and the tab stay in the OS
    /// keyring; only the cell mapping and the workbook's tab names are stored
    /// in setup.json.
    ///
    /// A workbook with several tabs needs `tab`. Without one this records the
    /// tab names, returns a notice asking for the choice, and connects nothing.
    pub async fn connect_sheets(
        &mut self,
        source: &SheetSource,
        tab: Option<&str>,
        layout: crate::sheets::SheetLayout,
    ) -> Result<String, LiveError> {
        if self.data["source"] != "sheets" {
            return Err(LiveError("Vælg Regneark som kilde først."));
        }
        let (location, mut secret) = match source {
            SheetSource::Link(link) => (sheets::parse_link(link)?, json!({"link": link.trim()})),
            SheetSource::File(path) => (sheets::parse_file(path)?, json!({"file": path.trim()})),
        };
        let bytes = sheets::fetch(&location).await?;
        let tabs = match location {
            sheets::Location::Google { .. } => Vec::new(),
            _ => sheets::tabs(&bytes)?,
        };
        self.data["sheet_tabs"] = json!(tabs);
        let tab = match (tab, tabs.as_slice()) {
            (_, []) => None,
            (Some(tab), _) if tabs.iter().any(|name| name == tab) => Some(tab.to_owned()),
            (None, [only]) => Some(only.clone()),
            _ => {
                self.save()?;
                return Ok("Vælg fanen med vagtplanen, og tilslut igen.".into());
            }
        };
        if let Some(tab) = &tab {
            secret["tab"] = json!(tab);
        }
        let access = sheets::SheetAccess {
            location,
            tab,
            layout: layout.clone(),
        };
        let grid = sheets::grid(&access, &bytes)?;
        let (calendars, colors, notice) =
            sheets::catalog(&grid, &access, self.timezone(), &self.standard_times()?)?;
        let credential = uuid::Uuid::new_v4().simple().to_string();
        store_credential(&credential, &secret)?;
        self.data["credential"] = json!(credential);
        self.data["sheet_layout"] = serde_json::to_value(layout).map_err(|_| INVALID)?;
        self.apply_source(calendars, colors)?;
        Ok(notice)
    }

    /// Connect one or more iCal feeds. The links stay in the OS keyring; only
    /// how helpers are found is stored in setup.json.
    pub async fn connect_ical(
        &mut self,
        links: &[String],
        rule: crate::ical::HelperRule,
    ) -> Result<String, LiveError> {
        if self.data["source"] != "ical" {
            return Err(LiveError("Vælg iCal-kalender som kilde først."));
        }
        let access = ical::access(links, rule.clone(), Default::default())?;
        let (calendars, colors, notice) = ical::source_catalog(
            &access,
            self.timezone(),
            self.today(),
            &self.standard_times()?,
        )
        .await?;
        let feeds: Vec<&str> = access.feeds.iter().map(|feed| feed.url.as_str()).collect();
        let credential = uuid::Uuid::new_v4().simple().to_string();
        store_credential(&credential, &json!({ "feeds": feeds }))?;
        self.data["credential"] = json!(credential);
        self.data["ical_rule"] = serde_json::to_value(rule).map_err(|_| INVALID)?;
        self.apply_source(calendars, colors)?;
        Ok(notice)
    }

    /// Read the saved source's helper choices, colours and a Danish notice
    /// about what was checked.
    async fn read_source_catalog(&self) -> Result<(Value, Value, String), LiveError> {
        let secret = read_credential(self.credential()?)?;
        match self.data["source"].as_str() {
            Some("sheets") => {
                let layout: crate::sheets::SheetLayout =
                    serde_json::from_value(self.data["sheet_layout"].clone())
                        .map_err(|_| LiveError("Regnearkets gemte opsætning er ugyldig."))?;
                let access = sheets::access(&secret, layout)?;
                sheets::source_catalog(&access, self.timezone(), &self.standard_times()?).await
            }
            Some("ical") => {
                let access = ical_access(&self.data, &secret)?;
                let (found, colors, notice) = ical::source_catalog(
                    &access,
                    self.timezone(),
                    self.today(),
                    &self.standard_times()?,
                )
                .await?;
                let calendars = match access.rule {
                    crate::ical::HelperRule::Feed => found,
                    crate::ical::HelperRule::Title { .. } => {
                        keep_known_helpers(&self.data["calendars"], found)
                    }
                };
                Ok((calendars, colors, notice))
            }
            _ => {
                let access = teamup::Access {
                    calendar: text(&secret["calendar"])?,
                    api_key: text(&secret["api_key"])?,
                    bearer: secret["bearer"].as_str().unwrap_or(""),
                };
                teamup::source_catalog(&access, self.timezone(), self.today()).await
            }
        }
    }

    /// Re-read the source for the saved credentials. Confirmed mappings
    /// survive unless the calendars or helpers themselves changed.
    ///
    /// Returns what the check found, in Danish, for the caller to show once.
    /// It is a result, not setup, so it is never saved in the document.
    pub async fn refresh_source(&mut self) -> Result<String, LiveError> {
        let (calendars, colors, notice) = self.read_source_catalog().await?;
        self.apply_source(calendars, colors)?;
        Ok(notice)
    }

    /// Record freshly read TeamUp calendars. Split from the request so the
    /// state transition can be checked against Python without a network call.
    pub(crate) fn apply_source(
        &mut self,
        calendars: Value,
        colors: Value,
    ) -> Result<(), LiveError> {
        if calendars != self.data["calendars"] {
            self.data["mappings"] = json!([]);
            self.data["catalog"] = Value::Null;
        }
        self.data["calendars"] = calendars;
        self.data["colors"] = colors;
        // Documents saved before check results became return values.
        if let Some(object) = self.data.as_object_mut() {
            object.remove("notice");
        }
        self.data["stage"] = json!("destinations");
        // The connection moved on, so there is no earlier stage to return to.
        if let Some(object) = self.data.as_object_mut() {
            object.remove("resume_stage");
        }
        self.save()
    }

    /// Re-read the source, read MitHF and DUOS, and propose helper mappings
    /// for review.
    ///
    /// The source is read too, so helpers added to a sheet after connecting
    /// appear here. `arrangement` selects a DUOS arrangement; `None` keeps the
    /// saved one. A single available arrangement is chosen automatically.
    /// Proposals are suggestions only and always have to be confirmed.
    pub async fn discover(
        &mut self,
        browser: &BrowserSessions,
        arrangement: Option<&str>,
    ) -> Result<(), LiveError> {
        self.refresh_source().await?;
        let today = self.today();
        let mut catalog = build_catalog(browser, "", today, self.duos_enabled()).await?;
        let target = self.resolve_arrangement(&catalog, arrangement)?;
        if !target.is_empty() {
            catalog = build_catalog(browser, &target, today, self.duos_enabled()).await?;
        }
        self.apply_catalog(catalog, &target)
    }

    /// Choose which arrangement to read in full: the requested one, the saved
    /// one while it is still offered, or the only one available.
    pub(crate) fn resolve_arrangement(
        &self,
        catalog: &Value,
        requested: Option<&str>,
    ) -> Result<String, LiveError> {
        if !self.duos_enabled() {
            return Ok(String::new());
        }
        let mut target = requested
            .map(str::to_owned)
            .unwrap_or_else(|| self.data["arrangement"].as_str().unwrap_or("").to_owned());
        if !target.is_empty() && selected(&catalog["arrangements"], &json!(target)).is_err() {
            target.clear();
        }
        if target.is_empty() {
            let available = rows(&catalog["arrangements"])?;
            if available.len() == 1 {
                target = text(&available[0]["id"])?.to_owned();
            }
        }
        Ok(target)
    }

    /// Record a freshly built destination catalog and propose mappings. Split
    /// from the requests so the state transition can be checked against Python.
    pub(crate) fn apply_catalog(&mut self, catalog: Value, target: &str) -> Result<(), LiveError> {
        let previous = self.data["catalog"].clone();
        for (key, value) in catalog.as_object().ok_or(INVALID)? {
            self.data[key] = value.clone();
        }
        self.data["arrangement"] = json!(target);
        self.data["catalog"] = catalog.clone();
        self.data["stage"] = json!(if target.is_empty() && self.duos_enabled() {
            "destinations"
        } else {
            "helpers"
        });
        if self.duos_enabled()
            && selected(&self.data["types"], &self.data["registration_type"]).is_err()
        {
            // Registration is always the ordinary shift type unless the person
            // says otherwise elsewhere, so prefer it by name. A single offered
            // type is still chosen on its own.
            let types = rows(&catalog["types"])?;
            let mut chosen = String::new();
            if types.len() == 1 {
                chosen = text(&types[0]["id"])?.to_owned();
            } else if let Some(common) = types.iter().find(|row| {
                row["name"]
                    .as_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case("almindelig"))
            }) {
                chosen = text(&common["id"])?.to_owned();
            }
            self.data["registration_type"] = json!(chosen);
        }
        // Keep reviewed edits only while every identity and account choice is
        // unchanged; otherwise propose again from the fresh catalog.
        if previous != catalog || rows(&self.data["mappings"])?.is_empty() {
            let mut mappings = Vec::new();
            for source in rows(&self.data["calendars"])? {
                let name = text(&source["name"])?;
                mappings.push(json!({
                    "source": text(&source["id"])?,
                    "mithf": suggested(name, &catalog["mithf"]),
                    "duos": if self.duos_enabled() { suggested(name, &catalog["duos"]) } else { String::new() },
                    "excluded": false,
                }));
            }
            self.data["mappings"] = Value::Array(mappings);
        }
        self.save()
    }

    /// Apply one reviewed choice: a registration type, or a calendar's helper
    /// assignment or exclusion.
    pub fn edit(&mut self, params: &Value) -> Result<(), LiveError> {
        if let Some(chosen) = params.get("registration_type") {
            selected(&self.data["types"], chosen)?;
            self.data["registration_type"] = chosen.clone();
        } else {
            let source = params.get("source").ok_or(INVALID)?;
            let position = rows(&self.data["mappings"])?
                .iter()
                .position(|row| row["source"] == *source)
                .ok_or(LiveError(
                    "Kalenderen findes ikke i opsætningen. Hent mulighederne igen.",
                ))?;
            if let Some(excluded) = params.get("excluded") {
                if !excluded.is_boolean() {
                    return Err(LiveError("Ugyldigt kalendervalg."));
                }
                self.data["mappings"][position]["excluded"] = excluded.clone();
            }
            for service in ["mithf", "duos"] {
                if let Some(chosen) = params.get(service) {
                    selected(&self.data[service], chosen)?;
                    self.data["mappings"][position][service] = chosen.clone();
                }
            }
        }
        self.data["stage"] = json!("helpers");
        self.save()
    }

    /// Check that every calendar is reviewed and every chosen identity is
    /// unambiguous. Makes no request.
    pub fn validate_choices(&self) -> Result<(), LiveError> {
        let reviewed: std::collections::BTreeSet<_> = rows(&self.data["mappings"])?
            .iter()
            .map(|row| row["source"].clone().to_string())
            .collect();
        let calendars: std::collections::BTreeSet<_> = rows(&self.data["calendars"])?
            .iter()
            .map(|row| row["id"].clone().to_string())
            .collect();
        if reviewed != calendars {
            return Err(LiveError(
                "Gennemgå alle kalendere, og vælg hjælpere eller udelad dem.",
            ));
        }
        if self.duos_enabled() {
            selected(&self.data["arrangements"], &self.data["arrangement"])?;
            selected(&self.data["types"], &self.data["registration_type"])?;
        }
        let included: Vec<_> = rows(&self.data["mappings"])?
            .iter()
            .filter(|row| row["excluded"] != json!(true))
            .collect();
        if included.is_empty() {
            return Err(LiveError("Vælg mindst én hjælperkalender."));
        }
        for service in if self.duos_enabled() {
            vec!["mithf", "duos"]
        } else {
            vec!["mithf"]
        } {
            let mut chosen = std::collections::BTreeSet::new();
            for row in &included {
                let person = selected(&self.data[service], &row[service])?;
                // MitHF read-back identifies assignments by name, so duplicate
                // names cannot be reconciled even with a stable id chosen here.
                if service == "mithf"
                    && rows(&self.data["mithf"])?
                        .iter()
                        .filter(|other| other["name"] == person["name"])
                        .count()
                        != 1
                {
                    return Err(LiveError("MitHF har flere hjælpere med samme navn. Få navnene gjort entydige i MitHF, før de kan overføres sikkert."));
                }
                if !chosen.insert(person["id"].to_string()) {
                    return Err(LiveError("Flere kalendere er valgt til samme hjælper. Ret valgene, eller udelad en kalender."));
                }
            }
        }
        Ok(())
    }

    /// Re-read both services and require that every confirmed choice still
    /// resolves to the same identity. A recolour is adopted silently; anything
    /// else sends the setup back for review.
    pub async fn revalidate(&mut self, browser: &BrowserSessions) -> Result<(), LiveError> {
        let (calendars, colors, _) = self.read_source_catalog().await?;
        if colors != self.data["colors"] {
            // Colour is appearance, not identity: adopt it without asking.
            self.data["colors"] = colors;
            self.save()?;
        }
        let arrangement = self.data["arrangement"].as_str().unwrap_or("").to_owned();
        let fresh = build_catalog(browser, &arrangement, self.today(), self.duos_enabled()).await?;
        if calendars != self.data["calendars"] || fresh != self.data["catalog"] {
            self.data["calendars"] = calendars;
            self.data["stage"] = json!("destinations");
            self.data["catalog"] = Value::Null;
            self.data["mappings"] = json!([]);
            self.save()?;
            return Err(LiveError(CHANGED));
        }
        self.validate_choices()
    }

    /// Confirm the reviewed setup after a fresh check of both services.
    pub async fn confirm(&mut self, browser: &BrowserSessions) -> Result<(), LiveError> {
        self.revalidate(browser).await?;
        self.data["stage"] = json!("ready");
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Replay the recorded Python-era scenarios against the Rust state machine.
    ///
    /// Only the two catalog readers are stubbed, exactly as the oracle stubs
    /// them, so what is compared is the state machine: which choices survive a
    /// refresh, what is proposed, and what resets.
    mod parity {
        use super::*;

        fn oracle() -> Value {
            // Frozen at the Python removal cutover: 5 recorded scenarios.
            serde_json::from_str(include_str!("../../tests/goldens/setup-scenarios.json"))
                .expect("setup golden")
        }

        fn replay(scenario: &Value, setup: &mut Setup) {
            for step in scenario["steps"].as_array().expect("steps") {
                let call = &step["call"];
                let expected = &step["data"];
                let reported = apply(setup, call, expected)
                    .err()
                    .map(|error| error.0.to_owned());
                let name = &scenario["scenario"];
                assert_eq!(
                    reported.as_deref(),
                    step["error"].as_str(),
                    "{name} / {call}: error text differs",
                );
                let mut legacy = setup.data.clone();
                let object = legacy.as_object_mut().expect("setup object");
                object.remove("source");
                object.remove("duos_enabled");
                object.remove("sheet_layout");
                object.remove("standard_times");
                object.remove("markers");
                // Check results are returned, no longer saved; see refresh_source.
                let mut expected = expected.clone();
                if let Some(object) = expected.as_object_mut() {
                    object.remove("notice");
                }
                assert_eq!(legacy, expected, "{name} / {call}: document differs");
            }
        }

        /// Run the recorded call, taking the catalog it produced from the
        /// document Python recorded. Reading is stubbed; deciding is compared.
        fn apply(setup: &mut Setup, call: &Value, expected: &Value) -> Result<(), LiveError> {
            match call["op"].as_str().expect("op") {
                "refresh" => {
                    setup.apply_source(expected["calendars"].clone(), expected["colors"].clone())
                }
                "validate" => setup.validate_choices(),
                "edit" => setup.edit(&call["params"]),
                "discover" => {
                    let full = json!({
                        "mithf": expected["mithf"], "arrangements": expected["arrangements"],
                        "types": expected["types"], "duos": expected["duos"],
                        "account": expected["account"], "account_ids": expected["account_ids"],
                    });
                    // Before an arrangement is chosen the reader returns neither.
                    let mut offered = full.clone();
                    offered["types"] = json!([]);
                    offered["duos"] = json!([]);
                    let target =
                        setup.resolve_arrangement(&offered, call["arrangement"].as_str())?;
                    setup.apply_catalog(if target.is_empty() { offered } else { full }, &target)
                }
                other => panic!("unknown step {other}"),
            }
        }

        #[test]
        fn the_setup_state_machine_matches_recorded_scenarios() {
            let recorded = oracle();
            for scenario in recorded["scenarios"].as_array().expect("scenarios") {
                let directory = tempfile::tempdir().expect("temp dir");
                let mut setup = Setup::load(directory.path()).expect("load");
                setup.data["credential"] = json!("test-credential");
                replay(scenario, &mut setup);
            }
        }

        #[test]
        fn the_account_scope_matches_recorded_identity() {
            let recorded = oracle();
            let expected = &recorded["account_scope"];
            let identity = &expected["identity"];
            let mut setup = json!({
                "account_ids": identity["account_ids"], "arrangement": identity["arrangement"],
                "registration_type": identity["registration_type"], "mappings": identity["mappings"],
            });
            let calendar = identity["calendar"].as_str().unwrap();
            let scope = crate::live::config::account_scope(&setup, calendar).expect("scope");
            // A different scope would orphan an existing sync-<scope>.sqlite3.
            assert_eq!(json!(scope), expected["scope"]);
            // Confirming helpers again must keep the history of transferred shifts.
            setup["mappings"][0]["mithf"] = json!("m9");
            setup["arrangement"] = json!("a2");
            let remapped = crate::live::config::account_scope(&setup, calendar).expect("scope");
            assert_eq!(remapped, scope);
        }
    }

    #[test]
    fn calendar_links_resolve_only_for_teamup_share_keys() {
        assert_eq!(
            calendar_reference(" https://teamup.com/ksAbc123/ ").unwrap(),
            "ksAbc123"
        );
        assert_eq!(
            calendar_reference("https://www.teamup.com/ks9/events").unwrap(),
            "ks9"
        );
        for rejected in [
            "http://teamup.com/ksAbc123",
            "https://evil.example/ksAbc123",
            "https://teamup.com/abc123",
            "https://teamup.com/ks",
            "https://teamup.com/ks-abc",
            "not a url",
        ] {
            assert!(
                calendar_reference(rejected).is_err(),
                "{rejected} should be rejected"
            );
        }
    }

    #[test]
    fn a_suggestion_needs_exactly_one_name_match() {
        let choices = json!([
            {"id": "1", "name": "Ida Å"}, {"id": "2", "name": "ida å"}, {"id": "3", "name": "Bo"},
        ]);
        assert_eq!(suggested("Bo", &choices), "3");
        assert_eq!(suggested("bo", &choices), "3");
        // Two calendars share the name, so neither may be proposed.
        assert_eq!(suggested("Ida Å", &choices), "");
        assert_eq!(suggested("Nobody", &choices), "");
    }

    #[test]
    fn a_new_document_saves_and_reloads_without_a_credential() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut setup = Setup::load(dir.path()).expect("load");
        assert_eq!(setup.view()["stage"], "source");
        assert_eq!(setup.view()["has_credentials"], false);
        setup.data["registration_type"] = json!("7");
        setup.save().expect("save");

        let reloaded = Setup::load(dir.path()).expect("reload");
        assert_eq!(reloaded.data["registration_type"], "7");
        let view = reloaded.view();
        // Internal fields never reach the screen.
        for hidden in ["credential", "catalog", "imported", "account_ids"] {
            assert!(view.get(hidden).is_none(), "{hidden} must stay internal");
        }
    }

    #[test]
    fn standard_times_are_validated_and_saved_for_both_sources() {
        let dir = tempfile::tempdir().unwrap();
        let mut setup = Setup::load(dir.path()).unwrap();
        assert!(setup
            .set_standard_times(StandardTimes {
                everyday: "tomorrow".into(),
                ..Default::default()
            })
            .is_err());
        setup
            .set_standard_times(StandardTimes {
                everyday: "6-22".into(),
                ..Default::default()
            })
            .unwrap();
        setup.choose_source("sheets").unwrap();
        assert_eq!(setup.view()["standard_times"]["everyday"], "6-22");
        let restored = Setup::load(dir.path()).unwrap();
        assert_eq!(restored.view()["standard_times"]["everyday"], "6-22");
    }

    /// A workbook with several tabs is not read until one is chosen, and the
    /// choice is offered by name. Nothing is stored in the keyring yet.
    #[test]
    fn a_workbook_with_several_tabs_asks_for_one_before_connecting() {
        let dir = tempfile::tempdir().unwrap();
        let mut setup = Setup::load(dir.path()).unwrap();
        setup.choose_source("sheets").unwrap();
        let file = format!(
            "{}/tests/sheets/libreoffice.xlsx",
            env!("CARGO_MANIFEST_DIR")
        );
        let layout: crate::sheets::SheetLayout = serde_json::from_value(json!({
            "date_format": "%d-%m-%y", "sps_label": "",
            "helper": {"row": 2, "column": 0},
            "time": {"kind": "range", "cell": {"row": 3, "column": 0}},
        }))
        .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let found = runtime
            .block_on(setup.connect_sheets(&SheetSource::File(file), None, layout))
            .unwrap();
        assert_eq!(found, "Vælg fanen med vagtplanen, og tilslut igen.");
        assert_eq!(setup.view()["sheet_tabs"], json!(["Uge", "Tider"]));
        assert_eq!(setup.data["credential"], "");
        assert_eq!(setup.data["stage"], "source");
    }

    #[test]
    fn editing_rejects_unknown_calendars_and_non_boolean_exclusions() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut setup = Setup::load(dir.path()).expect("load");
        setup.data["mithf"] = json!([{"id": "m1", "name": "Ida"}]);
        setup.data["mappings"] =
            json!([{"source": "c1", "mithf": "", "duos": "", "excluded": false}]);

        assert!(setup
            .edit(&json!({"source": "unknown", "mithf": "m1"}))
            .is_err());
        assert!(setup
            .edit(&json!({"source": "c1", "excluded": "yes"}))
            .is_err());
        assert!(setup
            .edit(&json!({"source": "c1", "mithf": "absent"}))
            .is_err());

        setup
            .edit(&json!({"source": "c1", "mithf": "m1"}))
            .expect("edit");
        assert_eq!(setup.data["mappings"][0]["mithf"], "m1");
        assert_eq!(setup.data["stage"], "helpers");
    }

    #[test]
    fn confirmation_requires_every_calendar_reviewed_and_unambiguous() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut setup = Setup::load(dir.path()).expect("load");
        setup.data["calendars"] = json!([{"id": "c1", "name": "Ida"}, {"id": "c2", "name": "Bo"}]);
        setup.data["arrangements"] = json!([{"id": "a1", "name": "SPS"}]);
        setup.data["arrangement"] = json!("a1");
        setup.data["types"] = json!([{"id": "t1", "name": "Type"}]);
        setup.data["registration_type"] = json!("t1");
        setup.data["mithf"] = json!([{"id": "m1", "name": "Ida"}, {"id": "m2", "name": "Bo"}]);
        setup.data["duos"] = json!([{"id": "d1", "name": "Ida"}, {"id": "d2", "name": "Bo"}]);

        // One calendar left unreviewed.
        setup.data["mappings"] =
            json!([{"source": "c1", "mithf": "m1", "duos": "d1", "excluded": false}]);
        assert!(setup.validate_choices().is_err());

        // Both calendars pointing at the same helper.
        setup.data["mappings"] = json!([
            {"source": "c1", "mithf": "m1", "duos": "d1", "excluded": false},
            {"source": "c2", "mithf": "m1", "duos": "d1", "excluded": false},
        ]);
        assert!(setup.validate_choices().is_err());

        setup.data["mappings"] = json!([
            {"source": "c1", "mithf": "m1", "duos": "d1", "excluded": false},
            {"source": "c2", "mithf": "m2", "duos": "d2", "excluded": false},
        ]);
        setup.validate_choices().expect("reviewed setup is valid");

        // Excluding every calendar leaves nothing to transfer.
        setup.data["mappings"][0]["excluded"] = json!(true);
        setup.data["mappings"][1]["excluded"] = json!(true);
        assert!(setup.validate_choices().is_err());
    }

    #[test]
    fn duplicate_mithf_names_block_confirmation() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut setup = Setup::load(dir.path()).expect("load");
        setup.data["calendars"] = json!([{"id": "c1", "name": "Ida"}]);
        setup.data["arrangements"] = json!([{"id": "a1", "name": "SPS"}]);
        setup.data["arrangement"] = json!("a1");
        setup.data["types"] = json!([{"id": "t1", "name": "Type"}]);
        setup.data["registration_type"] = json!("t1");
        setup.data["mithf"] = json!([{"id": "m1", "name": "Ida"}, {"id": "m2", "name": "Ida"}]);
        setup.data["duos"] = json!([{"id": "d1", "name": "Ida"}]);
        setup.data["mappings"] =
            json!([{"source": "c1", "mithf": "m1", "duos": "d1", "excluded": false}]);
        assert!(setup.validate_choices().is_err());
    }

    #[test]
    fn each_source_keeps_its_setup_when_switching_back_and_forth() {
        let dir = tempfile::tempdir().unwrap();
        let mut setup = Setup::load(dir.path()).unwrap();
        setup.data["credential"] = json!("teamup-handle");
        setup.data["mappings"] =
            json!([{"source": "c1", "mithf": "m1", "duos": "d1", "excluded": false}]);
        setup.data["stage"] = json!("ready");
        let teamup = setup.data.clone();

        // The app opens the connection step with Skift vagtplan first.
        setup.edit_source().unwrap();
        setup.choose_source("sheets").unwrap();
        assert_eq!(setup.data["mappings"], json!([]));
        assert_eq!(setup.data["stage"], "source");
        // The kept setup holds a credential handle; it never reaches the view.
        assert!(setup.view().get("other_sources").is_none());
        assert_eq!(setup.view()["has_credentials"], false);
        assert_eq!(setup.view()["connected_sources"], json!(["teamup"]));
        setup.data["credential"] = json!("sheet-handle");
        setup.data["stage"] = json!("ready");
        let mut sheets = setup.data.clone();

        // A third source keeps both of the others.
        setup.edit_source().unwrap();
        setup.choose_source("ical").unwrap();
        assert_eq!(setup.data["mappings"], json!([]));
        assert_eq!(
            setup.view()["connected_sources"],
            json!(["sheets", "teamup"])
        );
        setup.data["credential"] = json!("ical-handle");
        setup.data["stage"] = json!("ready");
        assert_eq!(
            setup.view()["connected_sources"],
            json!(["ical", "sheets", "teamup"])
        );

        setup.edit_source().unwrap();
        setup.choose_source("teamup").unwrap();
        assert_eq!(setup.view()["has_credentials"], true);
        let mut restored = setup.data.clone();
        restored.as_object_mut().unwrap().remove("other_sources");
        assert_eq!(restored, teamup);
        assert_eq!(
            setup.data["other_sources"]["sheets"]["credential"],
            "sheet-handle"
        );
        assert_eq!(
            setup.data["other_sources"]["ical"]["credential"],
            "ical-handle"
        );

        setup.edit_source().unwrap();
        setup.choose_source("sheets").unwrap();
        sheets.as_object_mut().unwrap().remove("other_sources");
        let mut now = Setup::load(dir.path()).unwrap().data;
        now.as_object_mut().unwrap().remove("other_sources");
        assert_eq!(now, sheets);
        assert!(setup.choose_source("outlook").is_err());
    }

    #[test]
    fn a_shared_calendar_keeps_helpers_whose_shifts_have_passed() {
        let known = json!([{"id": "anna", "name": "Anna"}, {"id": "carl", "name": "Carl"}]);
        let found = json!([{"id": "anna", "name": "anna"}, {"id": "bo", "name": "Bo"}]);
        assert_eq!(
            keep_known_helpers(&known, found.clone()),
            json!([
                {"id": "anna", "name": "Anna"},
                {"id": "bo", "name": "Bo"},
                {"id": "carl", "name": "Carl"},
            ])
        );
        // Nothing known yet: what was found, in order.
        assert_eq!(
            keep_known_helpers(&json!([]), found),
            json!([{"id": "anna", "name": "anna"}, {"id": "bo", "name": "Bo"}])
        );
    }

    #[test]
    fn a_setup_saved_with_two_sources_keeps_the_other_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("setup.json"),
            json!({
                "version": 1, "stage": "ready", "source": "teamup", "credential": "teamup-handle",
                "other_source": {"version": 1, "stage": "ready", "source": "sheets", "credential": "sheet-handle"},
            })
            .to_string(),
        )
        .unwrap();
        let mut setup = Setup::load(dir.path()).unwrap();
        assert!(setup.data.get("other_source").is_none());
        assert_eq!(
            setup.view()["connected_sources"],
            json!(["sheets", "teamup"])
        );
        setup.edit_source().unwrap();
        setup.choose_source("sheets").unwrap();
        assert_eq!(setup.data["credential"], "sheet-handle");
        assert_eq!(setup.data["stage"], "ready");
    }

    #[test]
    fn the_ordinary_shift_type_is_preferred_when_several_are_offered() {
        let dir = tempfile::tempdir().unwrap();
        let mut setup = Setup::load(dir.path()).unwrap();
        let catalog = json!({
            "arrangements": [], "types": [
                {"id": "t9", "name": "Sygdom"},
                {"id": "t1", "name": "Almindelig"},
            ],
            "mithf": [], "duos": [], "account": "", "account_ids": {},
        });
        setup.apply_catalog(catalog.clone(), "").unwrap();
        assert_eq!(setup.data["registration_type"], "t1");

        // Without an ordinary type nothing is guessed; the person chooses.
        let mut setup = Setup::load(dir.path()).unwrap();
        let catalog = json!({
            "arrangements": [], "types": [
                {"id": "t9", "name": "Sygdom"},
                {"id": "t8", "name": "Ferie"},
            ],
            "mithf": [], "duos": [], "account": "", "account_ids": {},
        });
        setup.apply_catalog(catalog, "").unwrap();
        assert_eq!(setup.data["registration_type"], "");

        // A kept choice is never overwritten by the preference.
        let mut setup = Setup::load(dir.path()).unwrap();
        setup.data["types"] = json!([{"id": "t9", "name": "Sygdom"}]);
        setup.data["registration_type"] = json!("t9");
        let catalog = json!({
            "arrangements": [], "types": [
                {"id": "t9", "name": "Sygdom"},
                {"id": "t1", "name": "Almindelig"},
            ],
            "mithf": [], "duos": [], "account": "", "account_ids": {},
        });
        setup.apply_catalog(catalog, "").unwrap();
        assert_eq!(setup.data["registration_type"], "t9");
    }

    #[test]
    fn duos_can_be_disabled_without_a_duos_account_or_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let mut setup = Setup::load(dir.path()).unwrap();
        setup.choose_duos(false).unwrap();
        setup.data["calendars"] = json!([{"id": "helper", "name": "Alex"}]);
        setup.data["mithf"] = json!([{"id": "mithf-helper", "name": "Alex"}]);
        setup.data["mappings"] = json!([{
            "source": "helper", "mithf": "mithf-helper", "duos": "", "excluded": false
        }]);
        setup.validate_choices().unwrap();
        assert!(!Setup::load(dir.path()).unwrap().duos_enabled());
    }
}
