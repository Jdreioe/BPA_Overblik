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
    use super::{is_marker, marker_titles};

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
