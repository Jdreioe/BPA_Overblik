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
