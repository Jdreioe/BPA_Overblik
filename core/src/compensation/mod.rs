//! Kompensationsydelse: a port of kompensationsydelsesapp. The person logs
//! expenses, with the route for Kørsel, attaches bilag and exports a report.
//!
//! Expenses such as medicine and diet are health information, and routes are
//! places the person goes, so nothing here logs, reports or sends them.

mod export;
mod import;
mod pdf;
mod store;

pub use export::{export_rows, write_export, BilagNumber, Documentation, ExportRow};
pub use import::Imported;
pub use store::{BilagChange, Change, FrequentAddress, Log, Store};

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};

/// A Danish message for the person, never containing a path or expense data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompensationError(pub &'static str);

impl std::fmt::Display for CompensationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// The expense types of kompensationsydelsesapp.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpenseType {
    Driving,
    Medicine,
    Diet,
    Rent,
    Leisure,
    Courses,
    Clothing,
    Utilities,
    Other,
}

impl ExpenseType {
    pub const ALL: [ExpenseType; 9] = [
        ExpenseType::Driving,
        ExpenseType::Medicine,
        ExpenseType::Diet,
        ExpenseType::Rent,
        ExpenseType::Leisure,
        ExpenseType::Courses,
        ExpenseType::Clothing,
        ExpenseType::Utilities,
        ExpenseType::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ExpenseType::Driving => "Kørsel",
            ExpenseType::Medicine => "Medicin",
            ExpenseType::Diet => "Kost",
            ExpenseType::Rent => "Forhøjet husleje",
            ExpenseType::Leisure => "Fritidsaktiviteter",
            ExpenseType::Courses => "Handicaprelaterede kurser",
            ExpenseType::Clothing => "Beklædning",
            ExpenseType::Utilities => "El/vand/varme",
            ExpenseType::Other => "Andet",
        }
    }

    /// The type a label from kompensationsydelsesapp names. That app spelled
    /// Kost as `Kosrt`; anything unknown becomes Andet.
    pub fn from_label(label: &str) -> Self {
        let label = label.trim().to_lowercase();
        if label == "kosrt" {
            return ExpenseType::Diet;
        }
        Self::ALL
            .into_iter()
            .find(|kind| kind.label().to_lowercase() == label)
            .unwrap_or(ExpenseType::Other)
    }
}

impl std::fmt::Display for ExpenseType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Where a Kørsel went.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Route {
    pub fra: String,
    pub til: String,
    /// Imported trips may lack it.
    pub km: Option<f64>,
}

/// What the person typed for one expense.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub date: NaiveDate,
    pub kind: ExpenseType,
    pub beskrivelse: String,
    /// Set exactly for Kørsel.
    #[serde(default)]
    pub route: Option<Route>,
    /// Whole kroner.
    pub pris: u32,
    #[serde(default)]
    pub andet: String,
}

/// A receipt copied into the log's own folder, so it is still there when the
/// person exports months later.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Bilag {
    /// The copy's file name inside the store's `bilag` folder.
    pub file: String,
    /// The chosen file's own name, shown to the person. Never a path.
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Expense {
    pub id: u64,
    #[serde(flatten)]
    pub entry: Entry,
    #[serde(default)]
    pub bilag: Option<Bilag>,
}

/// Newest first, as the app lists them.
pub fn newest_first(expenses: &[Expense]) -> Vec<&Expense> {
    let mut sorted: Vec<&Expense> = expenses.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse((e.entry.date, e.id)));
    sorted
}

/// The app's monthly estimate: everything registered, spread over the days
/// from the first to the last expense, scaled to an average month.
#[derive(Clone, Debug, PartialEq)]
pub struct MonthlyEstimate {
    pub covered_days: i64,
    pub total: u64,
    /// Largest first.
    pub by_type: Vec<(ExpenseType, u64)>,
}

impl MonthlyEstimate {
    pub fn new(expenses: &[Expense]) -> Option<Self> {
        let first = expenses.iter().map(|e| e.entry.date).min()?;
        let last = expenses.iter().map(|e| e.entry.date).max()?;
        let covered_days = (last - first).num_days() + 1;
        let per_month = |sum: u64| (sum as f64 * 30.4375 / covered_days as f64).round() as u64;
        let mut sums = std::collections::BTreeMap::new();
        for expense in expenses {
            *sums.entry(expense.entry.kind).or_insert(0u64) += u64::from(expense.entry.pris);
        }
        let total = per_month(sums.values().sum());
        let mut by_type: Vec<(ExpenseType, u64)> = sums
            .into_iter()
            .map(|(kind, sum)| (kind, per_month(sum)))
            .collect();
        by_type.sort_by_key(|(kind, amount)| (std::cmp::Reverse(*amount), *kind));
        Some(Self {
            covered_days,
            total,
            by_type,
        })
    }
}

/// The km of the latest Kørsel between the same two addresses, either way.
/// This replaces the app's online route lookup: no address leaves the device.
pub fn remembered_km(expenses: &[Expense], fra: &str, til: &str) -> Option<f64> {
    let (fra, til) = (same_place(fra), same_place(til));
    if fra.is_empty() || til.is_empty() {
        return None;
    }
    newest_first(expenses).into_iter().find_map(|expense| {
        let route = expense.entry.route.as_ref()?;
        let (a, b) = (same_place(&route.fra), same_place(&route.til));
        ((a == fra && b == til) || (a == til && b == fra))
            .then_some(route.km)
            .flatten()
    })
}

fn same_place(address: &str) -> String {
    address.trim().to_lowercase()
}

/// A suggestion for a Fra or Til field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Suggestion {
    /// `Hjem: Hjemvej 1` for a frequent address, else the address itself.
    pub label: String,
    pub value: String,
}

/// Frequent addresses matching `query` (all of them for an empty query),
/// then addresses used before, at most eight and never twice.
pub fn address_suggestions(log: &Log, query: &str) -> Vec<Suggestion> {
    let query = same_place(query);
    let frequent = log
        .addresses
        .iter()
        .filter(|a| {
            query.is_empty()
                || a.nickname.to_lowercase().contains(&query)
                || a.address.to_lowercase().contains(&query)
        })
        .map(|a| Suggestion {
            label: format!("{}: {}", a.nickname, a.address),
            value: a.address.clone(),
        });
    let used = log
        .expenses
        .iter()
        .filter_map(|e| e.entry.route.as_ref())
        .flat_map(|route| [&route.fra, &route.til])
        .filter(|address| query.chars().count() >= 2 && address.to_lowercase().contains(&query))
        .map(|address| Suggestion {
            label: address.clone(),
            value: address.clone(),
        });
    let mut seen = std::collections::BTreeSet::new();
    frequent
        .chain(used)
        .filter(|s| seen.insert(same_place(&s.value)))
        .take(8)
        .collect()
}

/// km × pris/km, rounded to whole kroner.
pub fn driving_price(km: f64, price_per_km: f64) -> u32 {
    (km * price_per_km).round().clamp(0.0, u32::MAX as f64) as u32
}

/// Danish kroner with thousands separators, for example `1.250 kr.`.
pub fn format_kr(amount: u64) -> String {
    let digits = amount.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push('.');
        }
        grouped.push(digit);
    }
    format!("{grouped} kr.")
}

/// `12,4`, with at most one decimal.
pub fn format_km(km: f64) -> String {
    let rounded = (km * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.1}").replace('.', ",")
    }
}

/// A positive decimal number written with a comma or a dot, like km or pris/km.
pub fn parse_decimal(input: &str) -> Option<f64> {
    input
        .trim()
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0)
}

/// Read an amount the way people write it in Danish (`350`, `1.250`,
/// `1.250,50`, `89,95 kr.`) and round it to whole kroner.
pub fn parse_amount(input: &str) -> Result<u32, CompensationError> {
    const INVALID: CompensationError =
        CompensationError("Skriv beløbet i kroner, fx 350 eller 1.250,50.");
    let cleaned: String = input
        .trim()
        .trim_end_matches('.')
        .trim_end_matches("kr")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let (whole, fraction) = match cleaned.rsplit_once(',') {
        Some((whole, fraction)) => (whole, fraction),
        // Without a comma, a dot followed by exactly three digits groups
        // thousands (`1.250`); any other dot is a decimal point (`12.50`).
        None => match cleaned.rsplit_once('.') {
            Some((whole, fraction)) if fraction.len() != 3 => (whole, fraction),
            _ => (cleaned.as_str(), ""),
        },
    };
    let whole = ungroup(whole).ok_or(INVALID)?;
    if fraction.len() > 2 || !fraction.chars().all(|c| c.is_ascii_digit()) {
        return Err(INVALID);
    }
    let kroner: u64 = whole.parse().map_err(|_| INVALID)?;
    let rounds_up = fraction.parse::<u32>().is_ok_and(|ore| {
        let ore = if fraction.len() == 1 { ore * 10 } else { ore };
        ore >= 50
    });
    let amount = kroner + u64::from(rounds_up);
    if amount == 0 {
        return Err(CompensationError("Beløbet skal være mindst 1 kr."));
    }
    u32::try_from(amount)
        .ok()
        .filter(|amount| *amount <= 10_000_000)
        .ok_or(CompensationError("Beløbet er for stort."))
}

/// Digits with optional dots between thousands (`1.250.000`), without the dots.
fn ungroup(whole: &str) -> Option<String> {
    let mut groups = whole.split('.');
    let first = groups.next()?;
    let digits = |group: &str| !group.is_empty() && group.chars().all(|c| c.is_ascii_digit());
    if !digits(first) || (whole.contains('.') && first.len() > 3) {
        return None;
    }
    let mut plain = first.to_owned();
    for group in groups {
        if group.len() != 3 || !digits(group) {
            return None;
        }
        plain.push_str(group);
    }
    Some(plain)
}

/// `22.09.2026`.
pub fn format_date(date: NaiveDate) -> String {
    date.format("%d.%m.%Y").to_string()
}

/// `September 2026`, the heading the app groups expenses under.
pub fn month_label(date: NaiveDate) -> String {
    let name = month_da(date.month());
    let mut chars = name.chars();
    let capitalized: String = chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    format!("{capitalized} {}", date.year())
}

pub fn month_da(month: u32) -> &'static str {
    [
        "januar",
        "februar",
        "marts",
        "april",
        "maj",
        "juni",
        "juli",
        "august",
        "september",
        "oktober",
        "november",
        "december",
    ][(month as usize).saturating_sub(1).min(11)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expense(id: u64, date: &str, kind: ExpenseType, pris: u32) -> Expense {
        Expense {
            id,
            entry: Entry {
                date: date.parse().unwrap(),
                kind,
                beskrivelse: "Test".into(),
                route: None,
                pris,
                andet: String::new(),
            },
            bilag: None,
        }
    }

    fn trip(id: u64, date: &str, fra: &str, til: &str, km: f64) -> Expense {
        let mut trip = expense(id, date, ExpenseType::Driving, 100);
        trip.entry.route = Some(Route {
            fra: fra.into(),
            til: til.into(),
            km: Some(km),
        });
        trip
    }

    #[test]
    fn the_monthly_estimate_spreads_everything_over_the_covered_days() {
        assert_eq!(MonthlyEstimate::new(&[]), None);
        // 61 days from 1 August to 30 September.
        let estimate = MonthlyEstimate::new(&[
            expense(1, "2026-08-01", ExpenseType::Medicine, 600),
            expense(2, "2026-09-30", ExpenseType::Diet, 1_200),
            expense(3, "2026-09-10", ExpenseType::Medicine, 600),
        ])
        .unwrap();
        assert_eq!(estimate.covered_days, 61);
        assert_eq!(estimate.total, 1_198); // 2.400 × 30,4375 / 61
        assert_eq!(
            estimate.by_type,
            [(ExpenseType::Medicine, 599), (ExpenseType::Diet, 599)]
        );
    }

    #[test]
    fn a_route_driven_before_fills_in_its_km_either_way() {
        let expenses = [
            trip(1, "2026-09-01", "Hjemvej 1", "Træning 2", 12.0),
            trip(2, "2026-09-08", "Hjemvej 1", "Træning 2", 12.4),
        ];
        assert_eq!(
            remembered_km(&expenses, " hjemvej 1", "Træning 2"),
            Some(12.4)
        );
        assert_eq!(
            remembered_km(&expenses, "Træning 2", "Hjemvej 1"),
            Some(12.4)
        );
        assert_eq!(remembered_km(&expenses, "Hjemvej 1", "Andetsteds"), None);
    }

    #[test]
    fn suggestions_put_frequent_addresses_first_and_never_twice() {
        let mut log = Log::default();
        log.addresses.push(FrequentAddress {
            nickname: "Hjem".into(),
            address: "Hjemvej 1".into(),
        });
        log.expenses
            .push(trip(1, "2026-09-01", "Hjemvej 1", "Hjemmeplejen 3", 4.0));
        let labels: Vec<String> = address_suggestions(&log, "hjem")
            .into_iter()
            .map(|s| s.label)
            .collect();
        assert_eq!(labels, ["Hjem: Hjemvej 1", "Hjemmeplejen 3"]);
        assert_eq!(address_suggestions(&log, "").len(), 1);
    }

    #[test]
    fn app_labels_map_to_types() {
        assert_eq!(ExpenseType::from_label("Kosrt"), ExpenseType::Diet);
        assert_eq!(
            ExpenseType::from_label("forhøjet husleje"),
            ExpenseType::Rent
        );
        assert_eq!(ExpenseType::from_label("Kørsel"), ExpenseType::Driving);
        assert_eq!(ExpenseType::from_label("Noget nyt"), ExpenseType::Other);
    }

    #[test]
    fn danish_amounts_round_to_whole_kroner() {
        for (input, expected) in [
            ("350", 350),
            ("1.250", 1_250),
            ("1.250,50", 1_251),
            ("89,49 kr.", 89),
            ("12.5", 13),
            ("1 250 kr", 1_250),
        ] {
            assert_eq!(parse_amount(input), Ok(expected), "{input}");
        }
        for input in ["", "abc", "0", "0,20", "1.2.3,4", "-5"] {
            assert!(parse_amount(input).is_err(), "{input}");
        }
        assert_eq!(format_kr(1_234_567), "1.234.567 kr.");
        assert_eq!(parse_decimal("3,79"), Some(3.79));
        assert_eq!(parse_decimal("0"), None);
        assert_eq!(format_km(12.44), "12,4");
        assert_eq!(format_km(12.0), "12");
        assert_eq!(driving_price(12.4, 3.79), 47);
    }
}
