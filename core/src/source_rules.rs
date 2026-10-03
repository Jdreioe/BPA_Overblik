use std::sync::LazyLock;

use regex::Regex;

static NAME_SEPARATOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/|\s\+\s").expect("valid name separator regex"));

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeetingCategory {
    pub name: &'static str,
    pub code: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceTitle {
    Shift,
    Reminder,
    Meeting(MeetingCategory),
}

/// Whether an event is a calendar marker: a note such as a day-off wish that is
/// shown beside the week but never transferred. "Husk at checke" reminders
/// always are; `markers` adds the titles chosen in setup. A title matches a
/// marker as a whole or as its first words, ignoring case and spacing, so
/// "Ønsker fri" also matches "ønsker  fri - hele dagen" but not "Ønsker fridag".
pub fn is_marker(title: &str, markers: &[String]) -> bool {
    if classify_source_title(title) == SourceTitle::Reminder {
        return true;
    }
    let title = words(title);
    markers.iter().any(|marker| {
        let marker = words(marker);
        !marker.is_empty()
            && title
                .strip_prefix(&marker)
                .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric()))
    })
}

/// Clean the marker titles typed in setup: trimmed, single-spaced, without
/// empty entries or case-insensitive repeats, in the order given.
pub fn marker_titles(titles: &[String]) -> Result<Vec<String>, &'static str> {
    let mut cleaned: Vec<String> = Vec::new();
    for title in titles {
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.chars().count() > 80 {
            return Err("En markering må højst være 80 tegn.");
        }
        if !title.is_empty() && !cleaned.iter().any(|seen| words(seen) == words(&title)) {
            cleaned.push(title);
        }
    }
    if cleaned.len() > 50 {
        return Err("Der kan højst være 50 markeringer.");
    }
    Ok(cleaned)
}

/// The helpers a shift names, in order: `Anna / Bo` and `Anna + Bo` are two
/// helpers working the same shift. Empty and repeated names are dropped.
pub fn helper_names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in NAME_SEPARATOR.split(text).map(str::trim) {
        if !name.is_empty() && !names.iter().any(|seen| words(seen) == words(name)) {
            names.push(name.to_owned());
        }
    }
    names
}

fn words(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Classifies title-only TeamUp rules before helper matching or SPS parsing.
pub fn classify_source_title(title: &str) -> SourceTitle {
    let normalized = title.trim().to_lowercase();
    let reminder_prefix = "husk at checke";

    if normalized == reminder_prefix
        || normalized.starts_with("husk at checke ")
        || normalized.starts_with("husk at checke...")
    {
        SourceTitle::Reminder
    } else if normalized == "p-møde" {
        SourceTitle::Meeting(MeetingCategory {
            name: "Vagtmøde",
            code: "4:1",
        })
    } else {
        SourceTitle::Shift
    }
}

#[cfg(test)]
mod tests {
    use super::{helper_names, is_marker, marker_titles};

    #[test]
    fn a_slash_or_spaced_plus_separates_helpers() {
        assert_eq!(helper_names("Anna / Bo"), ["Anna", "Bo"]);
        assert_eq!(helper_names("Anna/Bo + Cai"), ["Anna", "Bo", "Cai"]);
        assert_eq!(helper_names(" Anna A "), ["Anna A"]);
        // A plus inside a name is not a separator.
        assert_eq!(helper_names("Anna+"), ["Anna+"]);
        assert_eq!(helper_names("Anna / anna / "), ["Anna"]);
        assert!(helper_names(" / ").is_empty());
    }

    #[test]
    fn a_marker_matches_whole_words_at_the_start_of_the_title() {
        let markers = vec!["Ønsker fri".to_owned()];
        assert!(is_marker("ønsker  FRI", &markers));
        assert!(is_marker("Ønsker fri - hele dagen", &markers));
        assert!(!is_marker("Ønsker fridag", &markers));
        assert!(!is_marker("Anna ønsker fri", &markers));
        assert!(is_marker("Husk at checke medicin", &[]));
        assert!(!is_marker("Dagvagt", &markers));
    }

    #[test]
    fn marker_titles_are_trimmed_and_deduplicated() {
        let typed = ["  Ønsker  fri ", "", "ønsker fri", "Ferie"].map(String::from);
        assert_eq!(marker_titles(&typed).unwrap(), ["Ønsker fri", "Ferie"]);
    }
}
