use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use super::{
    id, rows,
    sheets::{parse_link, SheetAccess},
    text, LiveError, INVALID,
};
use crate::standard_time::StandardTimes;
use crate::{HelperMapping, PlanningConfig};
use serde_json::{json, Value};

// No Debug/Serialize: credentials must never enter diagnostics or UI messages.
#[derive(Clone)]
pub struct LiveConfig {
    pub planning: PlanningConfig,
    pub(crate) calendar: String,
    pub(crate) api_key: String,
    pub(crate) bearer: String,
    pub(crate) sheet: Option<SheetAccess>,
    pub(crate) setup: Value,
    pub(crate) lookback_days: i64,
    pub state_path: PathBuf,
    pub helper_names: BTreeMap<String, String>,
    pub helper_colors: BTreeMap<String, String>,
    pub standard_times: StandardTimes,
}

impl LiveConfig {
    pub fn source_is_sheets(&self) -> bool {
        self.sheet.is_some()
    }
}

/// Read the established desktop setup and OS vault without changing either.
/// Run on a blocking thread; keyring access may wait for the OS to unlock it.
pub fn load_saved_setup(data_dir: &Path) -> Result<LiveConfig, LiveError> {
    let data = std::fs::read_to_string(data_dir.join("setup.json")).map_err(|_| {
        LiveError("Ingen gemt opsætning. Gør opsætningen færdig under Indstillinger.")
    })?;
    let setup: Value = serde_json::from_str(&data).map_err(|_| {
        LiveError("Den gemte opsætning kunne ikke læses. Originalfilen er ikke ændret.")
    })?;
    if setup["version"] != 1 || setup["stage"] != "ready" {
        return Err(LiveError(
            "Opsætningen er ikke bekræftet. Gør den færdig under Indstillinger → Hjælpere.",
        ));
    }
    let secret = read_credential(text(&setup["credential"])?)?;
    config_from_setup(setup, &secret, data_dir)
}

/// Read TeamUp credentials from the OS keyring. The setup document itself
/// never holds a secret, only the handle they are stored under.
pub(crate) fn read_credential(credential: &str) -> Result<Value, LiveError> {
    let entry = keyring::Entry::new("teamup-shift-sync", credential).map_err(|_| {
        LiveError("Computerens nøglering kunne ikke åbnes. Lås den op, og prøv igen.")
    })?;
    let secret = entry.get_password().map_err(|_| {
        LiveError("De gemte TeamUp-oplysninger kunne ikke læses. Tilslut kalenderen igen.")
    })?;
    serde_json::from_str(&secret).map_err(|_| INVALID)
}

/// Store TeamUp credentials under a fresh handle. Run on a blocking thread.
pub(crate) fn store_credential(credential: &str, secret: &Value) -> Result<(), LiveError> {
    let entry = keyring::Entry::new("teamup-shift-sync", credential).map_err(|_| {
        LiveError("Computerens nøglering kunne ikke åbnes. Lås den op, og prøv igen.")
    })?;
    entry
        .set_password(&secret.to_string())
        .map_err(|_| LiveError("TeamUp-oplysningerne kunne ikke gemmes i computerens nøglering."))
}

pub(crate) fn selected<'a>(choices: &'a Value, identifier: &Value) -> Result<&'a Value, LiveError> {
    let matches: Vec<_> = rows(choices)?
        .iter()
        .filter(|row| row["id"] == *identifier)
        .collect();
    if matches.len() != 1 {
        return Err(LiveError(
            "Et gemt valg er ikke entydigt. Bekræft hjælperne igen under Indstillinger → Hjælpere.",
        ));
    }
    Ok(matches[0])
}

/// The account scope that names the synchronization database.
///
/// A complete, confirmed account/mapping combination gets its own history, so
/// this must keep matching Python exactly: a different scope would orphan the
/// existing `sync-<scope>.sqlite3` and re-submit work already transferred.
pub(crate) fn account_scope(setup: &Value, calendar: &str) -> Result<String, LiveError> {
    let identity = json!({
        "account_ids": setup["account_ids"], "arrangement": setup["arrangement"],
        "registration_type": setup["registration_type"], "mappings": setup["mappings"], "calendar": calendar,
    });
    let hash = crate::approval::json_digest(&identity).map_err(|_| INVALID)?;
    Ok(hash[..24].to_owned())
}

fn config_from_setup(
    setup: Value,
    secret: &Value,
    data_dir: &Path,
) -> Result<LiveConfig, LiveError> {
    let mut helpers = BTreeMap::new();
    let mut names = BTreeMap::new();
    let duos_enabled = setup["duos_enabled"].as_bool().unwrap_or(true);
    for mapping in rows(&setup["mappings"])? {
        if mapping["excluded"].as_bool().ok_or(INVALID)? {
            continue;
        }
        let source = selected(&setup["calendars"], &mapping["source"])?;
        let mithf = selected(&setup["mithf"], &mapping["mithf"])?;
        let duos = if duos_enabled {
            Some(selected(&setup["duos"], &mapping["duos"])?)
        } else {
            None
        };
        let key = id(&source["id"])?;
        if helpers.contains_key(&key) {
            return Err(INVALID);
        }
        names.insert(key.clone(), text(&source["name"])?.into());
        helpers.insert(
            key,
            HelperMapping {
                mithf_name: text(&mithf["name"])?.into(),
                duos_employee_number: duos
                    .map(|duos| id(&duos["id"]))
                    .transpose()?
                    .unwrap_or_default(),
            },
        );
    }
    if helpers.is_empty() {
        return Err(LiveError("Opsætningen mangler bekræftede hjælpere."));
    }
    let sheet = match setup["source"].as_str().unwrap_or("teamup") {
        "teamup" => None,
        "sheets" => {
            let layout: crate::sheets::SheetLayout =
                serde_json::from_value(setup["sheet_layout"].clone())
                    .map_err(|_| LiveError("Regnearkets gemte opsætning er ugyldig."))?;
            let link = text(&secret["link"])?;
            Some(parse_link(link, layout)?)
        }
        _ => return Err(LiveError("Ukendt kilde i opsætningen.")),
    };
    let (calendar, api_key, bearer, scope_source) = if let Some(sheet) = &sheet {
        (
            String::new(),
            String::new(),
            String::new(),
            sheet.source_id(),
        )
    } else {
        let calendar = text(&secret["calendar"])?.to_owned();
        let api_key = text(&secret["api_key"])?.to_owned();
        if calendar.is_empty() || api_key.is_empty() {
            return Err(LiveError("TeamUp-forbindelsen mangler en nøgle."));
        }
        (
            calendar.clone(),
            api_key,
            secret["bearer"].as_str().unwrap_or("").into(),
            calendar,
        )
    };
    let hash = account_scope(&setup, &scope_source)?;
    let standard_times: StandardTimes = serde_json::from_value(
        setup
            .get("standard_times")
            .cloned()
            .unwrap_or_else(|| json!({})),
    )
    .map_err(|_| LiveError("De gemte standardtider er ugyldige."))?;
    standard_times.validate().map_err(LiveError)?;
    let planning = PlanningConfig {
        timezone: setup["timezone"]
            .as_str()
            .unwrap_or("Europe/Copenhagen")
            .parse()
            .map_err(|_| INVALID)?,
        default_helper_count: 1,
        duos_arrangement_id: if duos_enabled {
            id(&setup["arrangement"])?
        } else {
            String::new()
        },
        duos_registration_type: if duos_enabled {
            id(&setup["registration_type"])?
        } else {
            String::new()
        },
        duos_enabled,
        helpers,
    };
    let lookback_days = setup["lookback_days"].as_i64().unwrap_or(7);
    if !(1..=366).contains(&lookback_days) {
        return Err(INVALID);
    }
    let helper_colors = names
        .keys()
        .map(|key| {
            let color = setup["colors"][key]
                .as_u64()
                .and_then(|id| id.checked_sub(1))
                .and_then(|index| TEAMUP_COLORS.get(index as usize))
                .copied()
                .unwrap_or("");
            (key.clone(), color.to_owned())
        })
        .collect();
    Ok(LiveConfig {
        helper_colors,
        standard_times,
        planning,
        calendar,
        api_key,
        bearer,
        sheet,
        lookback_days,
        setup,
        helper_names: names,
        state_path: data_dir.join(format!("sync-{hash}.sqlite3")),
    })
}

// Existing TeamUp palette, indexed by API colour id minus one. Unknown ids are neutral.
const TEAMUP_COLORS: [&str; 48] = [
    "#f2665b", "#cf2424", "#a01a1a", "#7e3838", "#ca7609", "#f16c20", "#f58a4b", "#d2b53b",
    "#d96fbf", "#b84e9d", "#9d3283", "#7a0f60", "#542382", "#7742a9", "#8763ca", "#b586e2",
    "#668cb3", "#4770d8", "#2951b9", "#133897", "#1a5173", "#1a699c", "#0080a6", "#4aaace",
    "#88b347", "#5a8121", "#2d850e", "#176413", "#0f4c30", "#386651", "#00855b", "#4fb5a1",
    "#553711", "#724f22", "#9c6013", "#f6c811", "#ce1212", "#b20d47", "#d8135a", "#e81f78",
    "#f5699a", "#5c1c1c", "#a55757", "#c37070", "#000000", "#383838", "#757575", "#a3a3a3",
];
