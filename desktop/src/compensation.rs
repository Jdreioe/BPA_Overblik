//! The Kompensationsydelse tab: the person's expense log, a guiding estimate,
//! the zip export for an application, and how to apply.
//!
//! It needs no shift setup and works during a transfer: it only touches
//! `kompensation.json` and its bilag folder. File work runs on blocking
//! threads through the core `Store`, which applies one change at a time.

use chrono::{Datelike, NaiveDate};
use iced::widget::{button, column, pick_list, row, space, text, text_input, Column};
use iced::{Element, Length, Task};
use std::path::PathBuf;
use teamup_shift_sync_core::compensation::{
    export_rows, format_date, format_kr, latest_rates, parse_amount, parse_date, rates,
    write_export, Bilag, BilagChange, Category, Change, Documentation, Entry, Estimate, Log,
    Period, Store, Summary,
};
use teamup_shift_sync_gui::protocol::{Notice, Tone};

pub const DUKH_GUIDE_URL: &str =
    "https://www.dukh.dk/Guides-og-Praksisnyt/@14/Lovguide---Kompensationsydelse-til-voksne";
pub const BORGER_URL: &str = "https://www.borger.dk/handicap/Hjaelp-i-hverdagen/hjaelp-til-daekning-af-kompensationsberettigende-udgifter-for-voksne";

#[derive(Clone)]
pub enum Message {
    Loaded(Result<Log, String>),
    Saved(Result<Log, String>),
    Date(String),
    Category(Category),
    Amount(String),
    Note(String),
    ChooseBilag,
    BilagChosen(Option<PathBuf>),
    RemoveBilag,
    Save,
    Edit(u64),
    CancelEdit,
    Delete(u64),
    ConfirmDelete,
    CancelDelete,
    Period(PeriodChoice),
    Export,
    /// The expenses whose bilag has gone missing, named for the person.
    ExportChecked(Result<Vec<String>, String>),
    ExportAnyway,
    CancelExport,
    /// Where to save the period's export, chosen in the file dialog.
    ExportTarget(Period, Option<PathBuf>),
    Exported(Result<PathBuf, String>),
    Remind(bool),
    /// A week's transfer completed.
    Transferred(NaiveDate),
    /// Answer a week's reminder by entering its expenses now.
    EnterForWeek(NaiveDate),
    NoExpenses(NaiveDate),
    /// Handled by the app, which owns opening the browser.
    OpenLink(&'static str),
}

/// The overview's period: the last twelve months or one calendar year.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeriodChoice {
    LastTwelveMonths,
    Year(i32),
}

impl PeriodChoice {
    fn period(self, today: NaiveDate) -> Period {
        match self {
            PeriodChoice::LastTwelveMonths => Period::last_twelve_months(today),
            PeriodChoice::Year(year) => Period::year(year),
        }
    }
}

impl std::fmt::Display for PeriodChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeriodChoice::LastTwelveMonths => f.write_str("Seneste 12 måneder"),
            PeriodChoice::Year(year) => write!(f, "{year}"),
        }
    }
}

enum Loading {
    Pending,
    Ready(Log),
    Failed(String),
}

/// The form's bilag: the saved one, a newly chosen file, or none.
enum FormBilag {
    Saved(Bilag),
    Chosen(PathBuf),
    None,
}

struct Form {
    /// The expense being edited, or `None` for a new one.
    editing: Option<u64>,
    date: String,
    category: Option<Category>,
    amount: String,
    note: String,
    bilag: FormBilag,
}

impl Form {
    fn new(date: NaiveDate) -> Self {
        Self {
            editing: None,
            date: format_date(date),
            category: None,
            amount: String::new(),
            note: String::new(),
            bilag: FormBilag::None,
        }
    }
}

pub struct CompensationUi {
    store: Store,
    log: Loading,
    form: Form,
    period: PeriodChoice,
    confirm_delete: Option<u64>,
    /// Named before an export with missing bilag continues.
    export_missing: Option<Vec<String>>,
    /// A form save or export file work is running, so their buttons do
    /// nothing until it finishes. File dialogs do not count: one that never
    /// answers must not lock the tab.
    busy: bool,
    /// The running change is the form's, so a success clears the form. A
    /// failure keeps what the person typed.
    saving_form: bool,
    notice: Option<Notice>,
}

fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

/// Run a store operation on a blocking thread.
fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
    done: impl Fn(Result<T, String>) -> Message + Send + 'static,
) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(work)
                .await
                .unwrap_or_else(|_| Err("Handlingen blev afbrudt. Prøv igen.".into()))
        },
        done,
    )
}

impl CompensationUi {
    pub fn new(data_dir: &std::path::Path) -> Self {
        Self {
            store: Store::new(data_dir),
            log: Loading::Pending,
            form: Form::new(today()),
            period: PeriodChoice::LastTwelveMonths,
            confirm_delete: None,
            export_missing: None,
            busy: false,
            saving_form: false,
            notice: None,
        }
    }

    pub fn load(&self) -> Task<Message> {
        let store = self.store.clone();
        blocking(
            move || store.load().map_err(|e| e.0.to_owned()),
            Message::Loaded,
        )
    }

    /// A notice for the app's notice area, once.
    pub fn take_notice(&mut self) -> Option<Notice> {
        self.notice.take()
    }

    fn log(&self) -> Option<&Log> {
        match &self.log {
            Loading::Ready(log) => Some(log),
            Loading::Pending | Loading::Failed(_) => None,
        }
    }

    pub fn reminds(&self) -> bool {
        self.log().is_some_and(|log| log.remind)
    }

    /// Mondays of transferred weeks still waiting for an answer.
    pub fn reminders(&self) -> Vec<NaiveDate> {
        self.log()
            .map(|log| log.reminders.iter().copied().collect())
            .unwrap_or_default()
    }

    fn change(&self, change: Change) -> Task<Message> {
        let store = self.store.clone();
        blocking(
            move || store.apply(change).map_err(|e| e.0.to_owned()),
            Message::Saved,
        )
    }

    fn error(&mut self, message: &str) {
        self.notice = Some(Notice::from_message(Tone::Error, message));
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Loaded(result) => {
                self.log = match result {
                    Ok(log) => Loading::Ready(log),
                    Err(error) => Loading::Failed(error),
                }
            }
            Message::Saved(result) => {
                let form = std::mem::take(&mut self.saving_form);
                self.busy = false;
                match result {
                    Ok(log) => {
                        self.log = Loading::Ready(log);
                        if form {
                            // Keep the date and category: receipts often come in
                            // batches from the same day or kind.
                            self.form.editing = None;
                            self.form.amount.clear();
                            self.form.note.clear();
                            self.form.bilag = FormBilag::None;
                        }
                    }
                    Err(error) => self.error(&error),
                }
            }
            Message::Date(value) => self.form.date = value,
            Message::Category(category) => self.form.category = Some(category),
            Message::Amount(value) => self.form.amount = value,
            Message::Note(value) => self.form.note = value,
            Message::ChooseBilag => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Vælg bilaget")
                            .add_filter(
                                "Kvitteringer og bilag",
                                &[
                                    "pdf", "jpg", "jpeg", "png", "heic", "webp", "tif", "tiff",
                                    "gif",
                                ],
                            )
                            .pick_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    Message::BilagChosen,
                );
            }
            Message::BilagChosen(path) => {
                // Cancelling the dialog keeps what was there.
                if let Some(path) = path {
                    self.form.bilag = FormBilag::Chosen(path);
                }
            }
            Message::RemoveBilag => self.form.bilag = FormBilag::None,
            Message::Save => {
                if self.busy || self.log().is_none() {
                    return Task::none();
                }
                let entry = match self.entry() {
                    Ok(entry) => entry,
                    Err(error) => {
                        self.error(error);
                        return Task::none();
                    }
                };
                let bilag = match &self.form.bilag {
                    FormBilag::Saved(_) => BilagChange::Keep,
                    FormBilag::Chosen(path) => BilagChange::Replace(path.clone()),
                    FormBilag::None => BilagChange::Remove,
                };
                self.busy = true;
                self.saving_form = true;
                return self.change(Change::Save {
                    id: self.form.editing,
                    entry,
                    bilag,
                });
            }
            Message::Edit(id) => {
                let Some(expense) = self
                    .log()
                    .and_then(|log| log.expenses.iter().find(|e| e.id == id))
                else {
                    return Task::none();
                };
                self.form = Form {
                    editing: Some(id),
                    date: format_date(expense.entry.date),
                    category: Some(expense.entry.category),
                    amount: expense.entry.amount.to_string(),
                    note: expense.entry.note.clone(),
                    bilag: expense
                        .bilag
                        .clone()
                        .map_or(FormBilag::None, FormBilag::Saved),
                };
                self.confirm_delete = None;
            }
            Message::CancelEdit => self.form = Form::new(today()),
            Message::Delete(id) => self.confirm_delete = Some(id),
            Message::CancelDelete => self.confirm_delete = None,
            Message::ConfirmDelete => {
                let Some(id) = self.confirm_delete.take() else {
                    return Task::none();
                };
                if self.form.editing == Some(id) {
                    self.form = Form::new(today());
                }
                return self.change(Change::Delete(id));
            }
            Message::Period(choice) => {
                self.period = choice;
                self.export_missing = None;
            }
            Message::Export => {
                let Some(log) = self.log().cloned() else {
                    return Task::none();
                };
                if self.busy {
                    return Task::none();
                }
                self.busy = true;
                let store = self.store.clone();
                let period = self.period.period(today());
                return blocking(
                    move || {
                        Ok(
                            export_rows(&log.expenses, period, |b| store.bilag_path(b).is_file())
                                .into_iter()
                                .filter(|row| row.documentation == Documentation::Missing)
                                .map(|row| expense_label(&row.expense.entry))
                                .collect(),
                        )
                    },
                    Message::ExportChecked,
                );
            }
            Message::ExportChecked(Ok(missing)) if !missing.is_empty() => {
                self.busy = false;
                self.export_missing = Some(missing);
            }
            Message::ExportChecked(Ok(_)) | Message::ExportAnyway => {
                self.export_missing = None;
                self.busy = false;
                let period = self.period.period(today());
                let name = format!("Kompensationsydelse {}.zip", period.file_label());
                return Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_title("Gem eksporten")
                            .set_file_name(name)
                            .add_filter("Zip-fil", &["zip"])
                            .save_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    move |target| Message::ExportTarget(period, target),
                );
            }
            Message::ExportChecked(Err(error)) => {
                self.busy = false;
                self.error(&error);
            }
            Message::CancelExport => self.export_missing = None,
            Message::ExportTarget(_, None) => {}
            Message::ExportTarget(period, Some(mut target)) => {
                let Some(log) = self.log().cloned() else {
                    return Task::none();
                };
                self.busy = true;
                if !target
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
                {
                    target.as_mut_os_string().push(".zip");
                }
                let store = self.store.clone();
                let today = today();
                return blocking(
                    move || {
                        write_export(&store, &log, period, today, &target)
                            .map(|()| target)
                            .map_err(|e| e.0.to_owned())
                    },
                    Message::Exported,
                );
            }
            Message::Exported(result) => {
                self.busy = false;
                self.notice = Some(match result {
                    Ok(path) => Notice::new(
                        Tone::Success,
                        "Eksporten er gemt",
                        format!(
                            "Den ligger i {}. Den indeholder dine udgifter og bilag, så del den kun med kommunen eller din rådgiver.",
                            path.display()
                        ),
                    ),
                    Err(error) => Notice::from_message(Tone::Error, &error),
                });
            }
            Message::Remind(on) => {
                if self.log().is_some() {
                    return self.change(Change::Remind(on));
                }
            }
            Message::Transferred(monday) => {
                if self.reminds() {
                    return self.change(Change::Transferred(monday));
                }
            }
            Message::EnterForWeek(monday) => {
                if self.form.editing.is_none() {
                    self.form.date = format_date(monday);
                }
                if !self.period.period(today()).contains(monday) {
                    self.period = PeriodChoice::Year(monday.year());
                }
                return self.change(Change::AnswerReminder(monday));
            }
            Message::NoExpenses(monday) => return self.change(Change::AnswerReminder(monday)),
            // The app opens links before handing messages here.
            Message::OpenLink(_) => {}
        }
        Task::none()
    }

    fn entry(&self) -> Result<Entry, &'static str> {
        let date = parse_date(&self.form.date).map_err(|e| e.0)?;
        let category = self
            .form
            .category
            .ok_or("Vælg en kategori fra positivlisten.")?;
        let amount = parse_amount(&self.form.amount).map_err(|e| e.0)?;
        Ok(Entry {
            date,
            category,
            amount,
            note: self.form.note.trim().to_owned(),
        })
    }

    pub fn view(&self) -> Element<'_, Message> {
        let today = today();
        let log = match &self.log {
            Loading::Ready(log) => log,
            Loading::Pending => return text("Indlæser udgifterne …").into(),
            Loading::Failed(error) => {
                return column![
                    crate::widgets::notice_card::<Message>(
                        Notice::from_message(Tone::Error, error),
                        None
                    ),
                    self.guide(today),
                ]
                .spacing(16)
                .into()
            }
        };
        let period = self.period.period(today);
        let mut choices = vec![PeriodChoice::LastTwelveMonths];
        let mut years: Vec<i32> = log
            .expenses
            .iter()
            .map(|e| e.entry.date.year())
            .chain([today.year()])
            .collect();
        years.sort_unstable_by(|a, b| b.cmp(a));
        years.dedup();
        choices.extend(years.into_iter().map(PeriodChoice::Year));

        let header = row![
            text("Periode").size(14),
            pick_list(choices, Some(self.period), Message::Period),
            space::horizontal(),
            primary(
                "Eksportér til ansøgning",
                (!self.busy).then_some(Message::Export)
            ),
        ]
        .spacing(10)
        .align_y(iced::alignment::Vertical::Center);

        // Room for the scrollbar the app puts beside the tab.
        let mut page = column![header]
            .spacing(16)
            .width(Length::Fill)
            .padding(iced::Padding::ZERO.right(16));
        if let Some(missing) = &self.export_missing {
            let mut names = column![].spacing(2);
            for name in missing {
                names = names.push(text(name.clone()).size(13));
            }
            page = page.push(
                column![
                    crate::widgets::notice_card::<Message>(
                        Notice::new(
                            Tone::Warning,
                            "Nogle bilag findes ikke længere",
                            "Ret eller slet udgifterne herunder, eller eksportér alligevel. Så står de som »Bilag mangler« og tæller som sandsynliggjort.",
                        ),
                        None,
                    ),
                    names,
                    row![
                        quiet("Eksportér alligevel", (!self.busy).then_some(Message::ExportAnyway)),
                        quiet("Fortryd", Some(Message::CancelExport)),
                    ]
                    .spacing(8),
                ]
                .spacing(8),
            );
        }
        page = page
            .push(crate::widgets::group(
                text("Overblik").size(16),
                overview(log, period),
            ))
            .push(crate::widgets::group(
                text(if self.form.editing.is_some() {
                    "Ret udgift"
                } else {
                    "Ny udgift"
                })
                .size(16),
                self.form(),
            ))
            .push(crate::widgets::group(
                text(format!("Udgifter · {}", period.label())).size(16),
                self.list(log, period),
            ))
            .push(self.guide(today));
        page.into()
    }

    fn form(&self) -> Column<'_, Message> {
        let labelled =
            |label: &'static str, input: Element<'static, Message>| -> Element<'static, Message> {
                column![text(label).size(13), input].spacing(4).into()
            };
        let fields = row![
            labelled(
                "Dato",
                text_input("dd.mm.åååå", &self.form.date)
                    .on_input(Message::Date)
                    .on_submit(Message::Save)
                    .padding(8)
                    .width(Length::Fixed(130.0))
                    .into(),
            ),
            labelled(
                "Kategori",
                pick_list(Category::ALL, self.form.category, Message::Category)
                    .placeholder("Vælg fra positivlisten")
                    .padding(8)
                    .width(Length::Fixed(240.0))
                    .into(),
            ),
            labelled(
                "Beløb i kr.",
                text_input("Fx 350", &self.form.amount)
                    .on_input(Message::Amount)
                    .on_submit(Message::Save)
                    .padding(8)
                    .width(Length::Fixed(130.0))
                    .into(),
            ),
        ]
        .spacing(12);
        let note = labelled(
            "Note (valgfri)",
            text_input("Fx Taxa til genoptræning", &self.form.note)
                .on_input(Message::Note)
                .on_submit(Message::Save)
                .padding(8)
                .into(),
        );
        let choose = Some(Message::ChooseBilag);
        let bilag = match &self.form.bilag {
            FormBilag::None => row![
                text("Intet bilag. Udgiften tæller som sandsynliggjort.").size(14),
                quiet("Vælg bilag", choose),
            ],
            FormBilag::Saved(bilag) => row![
                text(format!("Bilag: {}", bilag.name)).size(14),
                quiet("Skift bilag", choose),
                quiet("Fjern bilag", Some(Message::RemoveBilag)),
            ],
            FormBilag::Chosen(path) => row![
                text(format!(
                    "Bilag: {}",
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ))
                .size(14),
                quiet("Skift bilag", choose),
                quiet("Fjern bilag", Some(Message::RemoveBilag)),
            ],
        }
        .spacing(10)
        .align_y(iced::alignment::Vertical::Center);
        let save = (!self.busy).then_some(Message::Save);
        let actions = if self.form.editing.is_some() {
            row![
                primary("Gem ændring", save),
                quiet("Fortryd", Some(Message::CancelEdit))
            ]
        } else {
            row![primary("Tilføj udgift", save)]
        }
        .spacing(8);
        column![fields, note, bilag, actions].spacing(12)
    }

    fn list<'a>(&'a self, log: &'a Log, period: Period) -> Column<'a, Message> {
        let mut expenses: Vec<_> = log
            .expenses
            .iter()
            .filter(|e| period.contains(e.entry.date))
            .collect();
        expenses.sort_by_key(|e| std::cmp::Reverse((e.entry.date, e.id)));
        let mut list = column![].spacing(10);
        if expenses.is_empty() {
            return list.push(text("Der er ingen udgifter i perioden endnu."));
        }
        for expense in expenses {
            let entry = &expense.entry;
            let mut details = column![text(expense_label(entry))]
                .spacing(2)
                .width(Length::Fill);
            if !entry.note.is_empty() {
                details = details.push(text(&entry.note).size(13));
            }
            details = details.push(
                text(match &expense.bilag {
                    Some(bilag) => format!("Bilag: {}", bilag.name),
                    None => "Uden bilag (sandsynliggjort)".into(),
                })
                .size(13),
            );
            let actions = if self.confirm_delete == Some(expense.id) {
                row![
                    text("Slet udgiften og dens bilag?").size(14),
                    quiet("Ja, slet", Some(Message::ConfirmDelete)),
                    quiet("Fortryd", Some(Message::CancelDelete)),
                ]
            } else {
                row![
                    quiet("Ret", Some(Message::Edit(expense.id))),
                    quiet("Slet", Some(Message::Delete(expense.id))),
                ]
            }
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center);
            list = list.push(
                row![details, actions]
                    .spacing(12)
                    .align_y(iced::alignment::Vertical::Center),
            );
        }
        list
    }

    /// The rules in short Danish sentences, with the year's amounts.
    fn guide(&self, today: NaiveDate) -> Element<'_, Message> {
        let rates = rates(today.year()).unwrap_or_else(latest_rates);
        let level = format!("({}-niveau)", rates.year);
        let lines = [
            "Kompensationsydelse efter servicelovens § 100 dækker nødvendige merudgifter ved en varigt nedsat funktionsevne. Den gælder fra du er 18 år, til du når folkepensionsalderen.".to_owned(),
            "Får du førtidspension efter reglerne fra før 2003, kan du kun få ydelsen, hvis du også har BPA efter § 95 eller § 96.".to_owned(),
            "Udgiften skal stå på positivlisten: kost og diætpræparater, medicin, befordring, forhøjet husleje, fritidsaktiviteter, handicaprettede kurser, beklædning, el, vand og varme samt øvrige udgifter. Andre slags udgifter kan kun dækkes, hvis hver slags er over 1.250 kr. om måneden (2025-niveau).".to_owned(),
            format!(
                "Gruppe I: Du sandsynliggør udgifter på mindst {}/md. og får et standardbeløb på {}/md. {level}.",
                format_kr(rates.minimum.into()),
                format_kr(rates.group_one.into())
            ),
            format!(
                "Gruppe II: Du dokumenterer udgifter på mindst {}/md. og får dine faktiske udgifter dækket plus {}/md. {level}.",
                format_kr(rates.group_two_limit.into()),
                format_kr(rates.group_two_standard.into())
            ),
            "Det er gennemsnittet over året, der tæller, så udgifterne må gerne svinge fra måned til måned. Ydelsen er skattefri og afhænger ikke af din indkomst.".to_owned(),
            "Du søger hos din kommune via selvbetjeningen på borger.dk. Der er en ansøgning for hver gruppe. Vedhæft eksporten herfra som dokumentation.".to_owned(),
            "Udgifter, som Sygeforsikringen danmark eller en anden forsikring dækker, eller som er egenbetaling efter anden sociallovgivning, kan ikke dækkes.".to_owned(),
            "Fortæl kommunen, hvis dine udgifter ændrer sig, fx hvis de falder under grænsen.".to_owned(),
            "Får du afslag eller nedsat ydelse, kan du klage til kommunen inden 4 uger. Fastholder kommunen afgørelsen, sender den klagen videre til Ankestyrelsen.".to_owned(),
        ];
        let mut guide = column![].spacing(8);
        for line in lines {
            guide = guide.push(text(line));
        }
        guide = guide.push(
            row![
                quiet("Gå til borger.dk", Some(Message::OpenLink(BORGER_URL))),
                quiet(
                    "Læs DUKH's lovguide",
                    Some(Message::OpenLink(DUKH_GUIDE_URL))
                ),
            ]
            .spacing(8),
        );
        crate::widgets::group(text("Sådan søger du").size(16), guide)
    }
}

/// Totals, the monthly average and where it points, for the chosen period.
fn overview<'a>(log: &Log, period: Period) -> Column<'a, Message> {
    let summary = Summary::new(&log.expenses, period, |e| e.bilag.is_some());
    let mut lines = column![text(format!(
        "{}: {} i alt. Gennemsnit {}/md., heraf {} dokumenteret.",
        period.label(),
        format_kr(summary.total),
        format_kr(summary.average()),
        format_kr(summary.documented_average())
    ))]
    .spacing(6);
    for (category, total) in &summary.totals {
        lines = lines.push(text(format!("{}: {}", category.label(), format_kr(*total))).size(13));
    }
    let estimate = match Estimate::new(&summary) {
        Estimate::UnknownRates { year } => format!(
            "Satserne for {year} kendes ikke i denne version af appen. Opdatér appen for at se et overslag."
        ),
        Estimate::BelowMinimum { rates } => format!(
            "Under grænsen på {}/md. for gruppe I ({}-niveau).",
            format_kr(rates.minimum.into()),
            rates.year
        ),
        Estimate::GroupOne { rates } => format!(
            "Peger mod gruppe I: {}/md. ({}-niveau).",
            format_kr(rates.group_one.into()),
            rates.year
        ),
        Estimate::GroupTwo { rates, payment } => format!(
            "Peger mod gruppe II: dine dokumenterede udgifter plus {}, i alt {}/md. ({}-niveau).",
            format_kr(rates.group_two_standard.into()),
            format_kr(payment),
            rates.year
        ),
    };
    lines
        .push(text(estimate).size(16))
        .push(text("Overslaget er vejledende. Kommunen træffer afgørelsen.").size(13))
}

/// »22.09.2026 · Befordring · 350 kr.«
fn expense_label(entry: &Entry) -> String {
    format!(
        "{} · {} · {}",
        format_date(entry.date),
        entry.category.label(),
        format_kr(entry.amount.into())
    )
}

fn quiet<'a>(label: &'a str, message: Option<Message>) -> iced::widget::Button<'a, Message> {
    button(text(label).size(14))
        .style(crate::widgets::outlined)
        .padding([7, 12])
        .on_press_maybe(message)
}

fn primary<'a>(label: &'a str, message: Option<Message>) -> iced::widget::Button<'a, Message> {
    button(text(label))
        .style(iced::widget::button::primary)
        .padding([8, 14])
        .on_press_maybe(message)
}

#[cfg(test)]
impl CompensationUi {
    /// A tab whose log has already loaded.
    pub fn loaded(log: Log) -> Self {
        let mut ui = Self::new(std::path::Path::new("unused-test-directory"));
        ui.log = Loading::Ready(log);
        ui
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invalid_or_failed_save_keeps_what_was_typed() {
        let mut ui = CompensationUi::loaded(Log::default());
        let _ = ui.update(Message::Category(Category::Transport));
        let _ = ui.update(Message::Amount("abc".into()));
        assert_eq!(ui.update(Message::Save).units(), 0);
        assert!(ui.take_notice().is_some());
        assert_eq!(ui.form.amount, "abc");

        let _ = ui.update(Message::Amount("350".into()));
        assert!(ui.update(Message::Save).units() > 0);
        // A second press while saving does nothing.
        assert_eq!(ui.update(Message::Save).units(), 0);
        let _ = ui.update(Message::Saved(Err(
            "Udgiftslisten kunne ikke gemmes.".into()
        )));
        assert_eq!(ui.form.amount, "350");
        assert!(ui.take_notice().is_some());

        let _ = ui.update(Message::Save);
        let _ = ui.update(Message::Saved(Ok(Log::default())));
        assert!(ui.form.amount.is_empty());
        assert_eq!(ui.form.category, Some(Category::Transport));
    }
}
