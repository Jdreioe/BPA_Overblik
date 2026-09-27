//! Kompensationsydelse (servicelovens § 100): the person's own log of
//! disability-related expenses, the yearly rates and a guiding estimate of
//! which group the expenses point to.
//!
//! The municipality decides. Everything here only helps the person gather and
//! document their expenses. Expenses such as medicine and diet are health
//! information, so nothing in this module logs or reports them.

mod export;
mod pdf;
mod store;

pub use export::{export_rows, write_export, BilagNumber, Documentation, ExportRow};
pub use store::{BilagChange, Change, Log, Store};

use chrono::{Datelike, Months, NaiveDate};
use serde::{Deserialize, Serialize};

/// A Danish message for the person, never containing a path or expense data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompensationError(pub &'static str);

impl std::fmt::Display for CompensationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Positivlisten: the exhaustive list of expense kinds the ydelse covers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Diet,
    Medicine,
    Transport,
    Rent,
    Leisure,
    Courses,
    Clothing,
    Utilities,
    Other,
}

impl Category {
    pub const ALL: [Category; 9] = [
        Category::Diet,
        Category::Medicine,
        Category::Transport,
        Category::Rent,
        Category::Leisure,
        Category::Courses,
        Category::Clothing,
        Category::Utilities,
        Category::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Diet => "Kost og diætpræparater",
            Category::Medicine => "Medicin",
            Category::Transport => "Befordring",
            Category::Rent => "Forhøjet husleje",
            Category::Leisure => "Fritidsaktiviteter",
            Category::Courses => "Handicaprettede kurser",
            Category::Clothing => "Beklædning",
            Category::Utilities => "El, vand og varme",
            Category::Other => "Øvrige udgifter",
        }
    }
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// What the person typed for one expense.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub date: NaiveDate,
    pub category: Category,
    /// Whole kroner. Øre are rounded when the amount is entered.
    pub amount: u32,
    #[serde(default)]
    pub note: String,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Expense {
    pub id: u64,
    #[serde(flatten)]
    pub entry: Entry,
    #[serde(default)]
    pub bilag: Option<Bilag>,
}

/// The amounts for one year (Social- og Boligstyrelsen's satser), per month.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rates {
    pub year: i32,
    /// Group I needs at least this, sandsynliggjort.
    pub minimum: u32,
    /// Group I pays this.
    pub group_one: u32,
    /// Group II needs at least this, dokumenteret.
    pub group_two_limit: u32,
    /// Group II pays the documented expenses plus this.
    pub group_two_standard: u32,
}

/// The rates for `year`, when this version of the app knows them. They change
/// every year, so a new year's rates must be added here.
pub fn rates(year: i32) -> Option<Rates> {
    let (minimum, group_one, group_two_limit, group_two_standard) = match year {
        2025 => (555, 1_105, 2_000, 500),
        2026 => (580, 1_155, 2_090, 523),
        _ => return None,
    };
    Some(Rates {
        year,
        minimum,
        group_one,
        group_two_limit,
        group_two_standard,
    })
}

/// The newest rates this version knows, for describing the rules.
pub fn latest_rates() -> Rates {
    rates(2026).expect("2026 rates are known")
}

/// Whole calendar months, from the first day of `first` for `months` months.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Period {
    first: NaiveDate,
    months: u32,
}

impl Period {
    /// The twelve months ending with the month `today` is in.
    pub fn last_twelve_months(today: NaiveDate) -> Self {
        let this_month = today.with_day(1).expect("first of month");
        Self {
            first: this_month - Months::new(11),
            months: 12,
        }
    }
    pub fn year(year: i32) -> Self {
        Self {
            first: NaiveDate::from_ymd_opt(year, 1, 1).expect("valid year"),
            months: 12,
        }
    }
    pub fn months(self) -> u32 {
        self.months
    }
    /// The first day of the period's last month.
    pub fn last_month(self) -> NaiveDate {
        self.first + Months::new(self.months - 1)
    }
    pub fn contains(self, date: NaiveDate) -> bool {
        date >= self.first && date < self.first + Months::new(self.months)
    }
    /// For example »oktober 2025 – september 2026«.
    pub fn label(self) -> String {
        let last = self.last_month();
        format!(
            "{} {} – {} {}",
            month_da(self.first.month()),
            self.first.year(),
            month_da(last.month()),
            last.year()
        )
    }
    /// For file names, for example `2025-10 til 2026-09`.
    pub fn file_label(self) -> String {
        let last = self.last_month();
        format!(
            "{}-{:02} til {}-{:02}",
            self.first.year(),
            self.first.month(),
            last.year(),
            last.month()
        )
    }
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

/// Sums for one period. Only documented expenses can reach group II.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Summary {
    pub period: Period,
    /// Categories with expenses, in positivlisten's order.
    pub totals: Vec<(Category, u64)>,
    pub total: u64,
    pub documented: u64,
}

impl Summary {
    /// `documented` says whether an expense's bilag can be shown. An export
    /// counts a bilag whose file has gone missing as sandsynliggjort.
    pub fn new(
        expenses: &[Expense],
        period: Period,
        documented: impl Fn(&Expense) -> bool,
    ) -> Self {
        let mut totals = std::collections::BTreeMap::new();
        let (mut total, mut documented_total) = (0, 0);
        for expense in expenses.iter().filter(|e| period.contains(e.entry.date)) {
            let amount = u64::from(expense.entry.amount);
            *totals.entry(expense.entry.category).or_insert(0) += amount;
            total += amount;
            if documented(expense) {
                documented_total += amount;
            }
        }
        Self {
            period,
            totals: totals.into_iter().collect(),
            total,
            documented: documented_total,
        }
    }
    /// The monthly average, rounded to whole kroner.
    pub fn average(&self) -> u64 {
        per_month(self.total, self.period.months)
    }
    pub fn documented_average(&self) -> u64 {
        per_month(self.documented, self.period.months)
    }
}

fn per_month(total: u64, months: u32) -> u64 {
    let months = u64::from(months.max(1));
    (total + months / 2) / months
}

/// Which group a period's expenses point to, at the rates of the year its
/// last month is in. Only a guide: the municipality decides.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Estimate {
    /// This version of the app does not know that year's rates.
    UnknownRates {
        year: i32,
    },
    BelowMinimum {
        rates: Rates,
    },
    GroupOne {
        rates: Rates,
    },
    /// Pays the documented monthly average plus the group II standard amount.
    GroupTwo {
        rates: Rates,
        payment: u64,
    },
}

impl Estimate {
    pub fn new(summary: &Summary) -> Self {
        let year = summary.period.last_month().year();
        let Some(rates) = rates(year) else {
            return Estimate::UnknownRates { year };
        };
        // Compare totals, not rounded averages, so 6.954 kr. over a year
        // stays below a 580 kr. monthly minimum.
        let months = u64::from(summary.period.months);
        if summary.documented >= u64::from(rates.group_two_limit) * months {
            Estimate::GroupTwo {
                rates,
                payment: summary.documented_average() + u64::from(rates.group_two_standard),
            }
        } else if summary.total >= u64::from(rates.minimum) * months {
            Estimate::GroupOne { rates }
        } else {
            Estimate::BelowMinimum { rates }
        }
    }
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

/// Read a date written as `22.09.2026`, `22-9-2026`, `22/09/2026` or
/// `2026-09-22`.
pub fn parse_date(input: &str) -> Result<NaiveDate, CompensationError> {
    const INVALID: CompensationError =
        CompensationError("Skriv datoen som dag.måned.år, fx 22.09.2026.");
    let parts: Vec<&str> = input.trim().split(['.', '-', '/']).collect();
    let [a, b, c] = parts[..] else {
        return Err(INVALID);
    };
    let number = |part: &str| part.parse::<u32>().map_err(|_| INVALID);
    let (year, month, day) = if a.len() == 4 {
        (number(a)?, number(b)?, number(c)?)
    } else if c.len() == 4 {
        (number(c)?, number(b)?, number(a)?)
    } else {
        return Err(INVALID);
    };
    NaiveDate::from_ymd_opt(year as i32, month, day).ok_or(INVALID)
}

/// `22.09.2026`, the form `parse_date` reads back.
pub fn format_date(date: NaiveDate) -> String {
    date.format("%d.%m.%Y").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expense(id: u64, date: &str, amount: u32, documented: bool) -> Expense {
        Expense {
            id,
            entry: Entry {
                date: date.parse().unwrap(),
                category: Category::Transport,
                amount,
                note: String::new(),
            },
            bilag: documented.then(|| Bilag {
                file: format!("{id}.jpg"),
                name: "kvittering.jpg".into(),
            }),
        }
    }

    fn estimate(expenses: &[Expense]) -> Estimate {
        let summary = Summary::new(expenses, Period::year(2026), |e| e.bilag.is_some());
        Estimate::new(&summary)
    }

    #[test]
    fn the_group_i_minimum_is_the_monthly_rate_over_the_whole_period() {
        let rates = rates(2026).unwrap();
        assert_eq!(
            estimate(&[expense(1, "2026-03-01", 579 * 12, false)]),
            Estimate::BelowMinimum { rates }
        );
        assert_eq!(
            estimate(&[expense(1, "2026-03-01", 580 * 12, false)]),
            Estimate::GroupOne { rates }
        );
        // Rounds to a 580 kr. average, but the yearly total is still short.
        assert_eq!(
            estimate(&[expense(1, "2026-03-01", 6_954, false)]),
            Estimate::BelowMinimum { rates }
        );
    }

    #[test]
    fn only_documented_expenses_reach_group_ii() {
        let rates = rates(2026).unwrap();
        assert_eq!(
            estimate(&[expense(1, "2026-05-10", 2_090 * 12, true)]),
            Estimate::GroupTwo {
                rates,
                payment: 2_090 + 523
            }
        );
        assert_eq!(
            estimate(&[expense(1, "2026-05-10", 2_090 * 12, false)]),
            Estimate::GroupOne { rates }
        );
    }

    #[test]
    fn a_year_without_known_rates_is_not_guessed() {
        let summary = Summary::new(&[], Period::year(2027), |_| true);
        assert_eq!(
            Estimate::new(&summary),
            Estimate::UnknownRates { year: 2027 }
        );
    }

    #[test]
    fn the_last_twelve_months_end_with_this_month() {
        let period = Period::last_twelve_months("2026-09-27".parse().unwrap());
        assert!(period.contains("2025-10-01".parse().unwrap()));
        assert!(period.contains("2026-09-30".parse().unwrap()));
        assert!(!period.contains("2025-09-30".parse().unwrap()));
        assert!(!period.contains("2026-10-01".parse().unwrap()));
        assert_eq!(period.label(), "oktober 2025 – september 2026");
        assert_eq!(period.file_label(), "2025-10 til 2026-09");
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
        assert_eq!(format_kr(1_250), "1.250 kr.");
        assert_eq!(format_kr(1_234_567), "1.234.567 kr.");
    }

    #[test]
    fn dates_read_in_danish_and_iso_order() {
        let date: NaiveDate = "2026-09-22".parse().unwrap();
        for input in ["22.09.2026", "22-9-2026", "22/09/2026", "2026-09-22"] {
            assert_eq!(parse_date(input), Ok(date), "{input}");
        }
        assert!(parse_date("31.02.2026").is_err());
        assert!(parse_date("22.09.26").is_err());
        assert_eq!(format_date(date), "22.09.2026");
    }
}
