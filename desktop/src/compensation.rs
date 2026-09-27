//! The Kompensationsydelse tab, a port of kompensationsydelsesapp: expenses
//! grouped by month, Kørsel to copy from, a form to add or edit one, and the
//! report. Its settings live in Indstillinger → Kompensation.
//!
//! It needs no shift setup and works during a transfer: it only touches
//! `kompensation.json` and its bilag folder, through the core `Store` on
//! blocking threads.

use chrono::NaiveDate;
use iced::widget::{
    button, column, container, row, rule, space, text, text_input, toggler, Column,
};
use iced::{Element, Length, Task};
use std::path::PathBuf;
use teamup_shift_sync_core::compensation::{
    address_suggestions, driving_price, export_rows, format_date, format_km, format_kr,
    month_label, newest_first, parse_amount, parse_decimal, remembered_km, write_export, Bilag,
    BilagChange, Change, Documentation, Entry, Expense, ExpenseType, FrequentAddress, Imported,
    Log, MonthlyEstimate, Route, Store,
};
use teamup_shift_sync_gui::protocol::{Notice, Tone};

/// The app's sections, except its settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum View {
    Expenses,
    Driving,
    Add,
    Report,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressField {
    Fra,
    Til,
}

#[derive(Clone)]
pub enum Message {
    Loaded(Result<Log, String>),
    /// Any change's result that leaves the form alone.
    Saved(Result<Log, String>),
    /// The form's own save. Only it clears the form.
    FormSaved(Result<Log, String>),
    /// Adding a frequent address. Only success clears what was typed.
    AddressSaved(Result<Log, String>),
    Show(View),
    Kind(ExpenseType),
    ToggleCalendar,
    CalendarMonth(NaiveDate),
    PickDate(NaiveDate),
    Beskrivelse(String),
    Address(AddressField, String),
    Suggested(AddressField, String),
    Swap,
    Km(String),
    Pris(String),
    Andet(String),
    ChooseBilag,
    BilagChosen(Option<PathBuf>),
    RemoveBilag,
    Save,
    Edit(u64),
    /// Start a new expense from an earlier Kørsel.
    Copy(u64),
    CancelEdit,
    Delete(u64),
    ConfirmDelete,
    CancelDelete,
    Export,
    /// The expenses whose bilag has gone missing.
    ExportChecked(Result<Vec<String>, String>),
    ExportAnyway,
    CancelExport,
    ExportTarget(Option<PathBuf>),
    /// Open Indstillinger → Kompensation. The app handles it.
    PriceSettings,
    Exported(Result<PathBuf, String>),
    Remind(bool),
    /// A week's transfer completed.
    Transferred(NaiveDate),
    /// Answer a week's reminder by entering its expenses now.
    EnterForWeek(NaiveDate),
    NoExpenses(NaiveDate),
    PriceDraft(String),
    SavePrice,
    NicknameDraft(String),
    AddressDraft(String),
    AddAddress,
    RemoveAddress(String),
    Import,
    ImportChosen(Option<PathBuf>),
    Imported(Result<(Log, Imported), String>),
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
    kind: Option<ExpenseType>,
    date: NaiveDate,
    /// The month the date picker shows while it is open.
    calendar: Option<NaiveDate>,
    beskrivelse: String,
    fra: String,
    til: String,
    km: String,
    /// The person typed the km, so a changed address keeps them.
    km_typed: bool,
    pris: String,
    andet: String,
    bilag: FormBilag,
    /// The address field last typed in, which shows suggestions.
    suggest: Option<AddressField>,
}

impl Form {
    fn new(date: NaiveDate) -> Self {
        Self {
            editing: None,
            kind: None,
            date,
            calendar: None,
            beskrivelse: String::new(),
            fra: String::new(),
            til: String::new(),
            km: String::new(),
            km_typed: false,
            pris: String::new(),
            andet: String::new(),
            bilag: FormBilag::None,
            suggest: None,
        }
    }

    fn from_expense(expense: &Expense, editing: bool) -> Self {
        let entry = &expense.entry;
        let route = entry.route.as_ref();
        Self {
            editing: editing.then_some(expense.id),
            kind: Some(entry.kind),
            date: entry.date,
            calendar: None,
            beskrivelse: entry.beskrivelse.clone(),
            fra: route.map(|r| r.fra.clone()).unwrap_or_default(),
            til: route.map(|r| r.til.clone()).unwrap_or_default(),
            km: route.and_then(|r| r.km).map(format_km).unwrap_or_default(),
            km_typed: false,
            pris: entry.pris.to_string(),
            andet: entry.andet.clone(),
            bilag: match (&expense.bilag, editing) {
                (Some(bilag), true) => FormBilag::Saved(bilag.clone()),
                _ => FormBilag::None,
            },
            suggest: None,
        }
    }
}

pub struct CompensationUi {
    store: Store,
    log: Loading,
    view: View,
    form: Form,
    confirm_delete: Option<u64>,
    /// Named before an export with missing bilag continues.
    export_missing: Option<Vec<String>>,
    /// Work whose button does nothing until it finishes. File dialogs do not
    /// count: one that never answers must not lock the tab.
    saving: bool,
    exporting: bool,
    importing: bool,
    price_draft: String,
    nickname_draft: String,
    address_draft: String,
    /// What the last import did. It stays, because bilag it could not link
    /// are named here to attach by hand.
    import_report: Option<String>,
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
            view: View::Expenses,
            form: Form::new(today()),
            confirm_delete: None,
            export_missing: None,
            saving: false,
            exporting: false,
            importing: false,
            price_draft: String::new(),
            nickname_draft: String::new(),
            address_draft: String::new(),
            import_report: None,
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
        self.change_then(change, Message::Saved)
    }

    fn change_then(
        &self,
        change: Change,
        done: fn(Result<Log, String>) -> Message,
    ) -> Task<Message> {
        let store = self.store.clone();
        blocking(
            move || store.apply(change).map_err(|e| e.0.to_owned()),
            done,
        )
    }

    /// Show `log` unless a newer one is already shown. Results of changes
    /// can arrive out of order.
    fn accept(&mut self, log: Log) {
        if self
            .log()
            .is_none_or(|shown| shown.revision() <= log.revision())
        {
            self.log = Loading::Ready(log);
        }
    }

    fn error(&mut self, message: &str) {
        self.notice = Some(Notice::from_message(Tone::Error, message));
    }

    /// Fill in the km of a route driven before, and its price. For a route
    /// not driven before, km that belonged to the previous route are cleared
    /// with their price, unless the person typed them.
    fn fill_km(&mut self) {
        match self
            .log()
            .and_then(|log| remembered_km(&log.expenses, &self.form.fra, &self.form.til))
        {
            Some(km) => {
                self.form.km = format_km(km);
                self.form.km_typed = false;
                self.fill_price();
            }
            None if !self.form.km_typed && !self.form.km.is_empty() => {
                self.form.km.clear();
                self.form.pris.clear();
            }
            None => {}
        }
    }

    /// Kørsel costs km × pris/km. The amount stays editable.
    fn fill_price(&mut self) {
        let price = self.log().and_then(|log| log.price_per_km);
        if let (Some(ExpenseType::Driving), Some(km), Some(price)) =
            (self.form.kind, parse_decimal(&self.form.km), price)
        {
            self.form.pris = driving_price(km, price).to_string();
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Loaded(result) => match result {
                Ok(log) => {
                    self.price_draft = log
                        .price_per_km
                        .map(|price| price.to_string().replace('.', ","))
                        .unwrap_or_default();
                    self.log = Loading::Ready(log);
                }
                Err(error) => self.log = Loading::Failed(error),
            },
            Message::Saved(result) => match result {
                Ok(log) => {
                    self.accept(log);
                    // A price per km saved after the km were typed prices them.
                    if self.form.pris.is_empty() {
                        self.fill_price();
                    }
                }
                Err(error) => self.error(&error),
            },
            // A failure keeps what the person typed.
            Message::FormSaved(result) => {
                self.saving = false;
                match result {
                    Ok(log) => {
                        self.accept(log);
                        self.form = Form::new(today());
                        self.view = View::Expenses;
                    }
                    Err(error) => self.error(&error),
                }
            }
            Message::AddressSaved(result) => match result {
                Ok(log) => {
                    self.accept(log);
                    self.nickname_draft.clear();
                    self.address_draft.clear();
                }
                Err(error) => self.error(&error),
            },
            Message::Show(view) => {
                self.view = view;
                self.confirm_delete = None;
            }
            Message::Kind(kind) => {
                self.form.kind = Some(kind);
                self.fill_price();
            }
            Message::ToggleCalendar => {
                self.form.calendar = match self.form.calendar {
                    Some(_) => None,
                    None => Some(self.form.date),
                }
            }
            Message::CalendarMonth(month) => self.form.calendar = Some(month),
            Message::PickDate(date) => {
                self.form.date = date;
                self.form.calendar = None;
            }
            Message::Beskrivelse(value) => self.form.beskrivelse = value,
            Message::Address(field, value) => {
                match field {
                    AddressField::Fra => self.form.fra = value,
                    AddressField::Til => self.form.til = value,
                }
                self.form.suggest = Some(field);
                self.fill_km();
            }
            Message::Suggested(field, value) => {
                match field {
                    AddressField::Fra => self.form.fra = value,
                    AddressField::Til => self.form.til = value,
                }
                self.form.suggest = None;
                self.fill_km();
            }
            Message::Swap => {
                std::mem::swap(&mut self.form.fra, &mut self.form.til);
                self.form.suggest = None;
            }
            Message::Km(value) => {
                self.form.km = value;
                self.form.km_typed = true;
                self.fill_price();
            }
            Message::Pris(value) => self.form.pris = value,
            Message::Andet(value) => self.form.andet = value,
            Message::ChooseBilag => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Vælg bilag")
                            .add_filter(
                                "PDF og billeder",
                                &["pdf", "jpg", "jpeg", "png", "heic", "webp"],
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
                if self.saving || self.log().is_none() {
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
                self.saving = true;
                return self.change_then(
                    Change::Save {
                        id: self.form.editing,
                        entry,
                        bilag,
                    },
                    Message::FormSaved,
                );
            }
            Message::Edit(id) | Message::Copy(id) => {
                let editing = matches!(message, Message::Edit(_));
                if let Some(expense) = self
                    .log()
                    .and_then(|log| log.expenses.iter().find(|e| e.id == id))
                {
                    self.form = Form::from_expense(expense, editing);
                    self.view = View::Add;
                    self.confirm_delete = None;
                }
            }
            Message::CancelEdit => {
                self.form = Form::new(today());
                self.view = View::Expenses;
            }
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
            Message::Export => {
                let Some(log) = self.log().cloned() else {
                    return Task::none();
                };
                if self.exporting {
                    return Task::none();
                }
                self.exporting = true;
                let store = self.store.clone();
                return blocking(
                    move || {
                        Ok(
                            export_rows(&log.expenses, |b| store.bilag_path(b).is_file())
                                .into_iter()
                                .filter(|row| row.documentation == Documentation::Missing)
                                .map(|row| {
                                    let entry = &row.expense.entry;
                                    format!(
                                        "{} · {} · {}",
                                        format_date(entry.date),
                                        entry.kind.label(),
                                        entry.beskrivelse
                                    )
                                })
                                .collect(),
                        )
                    },
                    Message::ExportChecked,
                );
            }
            Message::ExportChecked(Ok(missing)) if !missing.is_empty() => {
                self.exporting = false;
                self.export_missing = Some(missing);
            }
            Message::ExportChecked(Ok(_)) | Message::ExportAnyway => {
                self.exporting = false;
                self.export_missing = None;
                let name = format!("Kompensationsydelse {}.zip", today());
                return Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_title("Gem rapport")
                            .set_file_name(name)
                            .add_filter("Zip-fil", &["zip"])
                            .save_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    Message::ExportTarget,
                );
            }
            Message::ExportChecked(Err(error)) => {
                self.exporting = false;
                self.error(&error);
            }
            Message::CancelExport => self.export_missing = None,
            // The app opens Indstillinger; the form stays as typed.
            Message::PriceSettings => {}
            Message::ExportTarget(None) => {}
            Message::ExportTarget(Some(mut target)) => {
                let Some(log) = self.log().cloned() else {
                    return Task::none();
                };
                self.exporting = true;
                if !target
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
                {
                    target.as_mut_os_string().push(".zip");
                }
                let store = self.store.clone();
                return blocking(
                    move || {
                        write_export(&store, &log, today(), &target)
                            .map(|()| target)
                            .map_err(|e| e.0.to_owned())
                    },
                    Message::Exported,
                );
            }
            Message::Exported(result) => {
                self.exporting = false;
                self.notice = Some(match result {
                    Ok(path) => Notice::new(
                        Tone::Success,
                        "Rapporten er gemt",
                        path.display().to_string(),
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
                    self.form.date = monday;
                }
                self.view = View::Add;
                return self.change(Change::AnswerReminder(monday));
            }
            Message::NoExpenses(monday) => return self.change(Change::AnswerReminder(monday)),
            Message::PriceDraft(value) => self.price_draft = value,
            Message::SavePrice => match parse_decimal(&self.price_draft) {
                Some(price) if self.log().is_some() => {
                    return self.change(Change::PricePerKm(price))
                }
                Some(_) => {}
                None => self.error("Skriv pris pr. km, fx 3,79."),
            },
            Message::NicknameDraft(value) => self.nickname_draft = value,
            Message::AddressDraft(value) => self.address_draft = value,
            Message::AddAddress => {
                let (nickname, address) = (
                    self.nickname_draft.trim().to_owned(),
                    self.address_draft.trim().to_owned(),
                );
                if nickname.is_empty() || address.is_empty() {
                    self.error("Skriv både kaldenavn og adresse.");
                } else if self.log().is_some() {
                    return self.change_then(
                        Change::AddAddress(FrequentAddress { nickname, address }),
                        Message::AddressSaved,
                    );
                }
            }
            Message::RemoveAddress(nickname) => {
                return self.change(Change::RemoveAddress(nickname));
            }
            Message::Import => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Vælg rapporten fra kompensationsydelsesapp")
                            .add_filter("Zip-fil", &["zip"])
                            .pick_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    Message::ImportChosen,
                );
            }
            Message::ImportChosen(None) => {}
            Message::ImportChosen(Some(path)) => {
                if self.importing || self.log().is_none() {
                    return Task::none();
                }
                self.importing = true;
                let store = self.store.clone();
                return blocking(
                    move || store.import_rapport(&path).map_err(|e| e.0.to_owned()),
                    Message::Imported,
                );
            }
            Message::Imported(result) => {
                self.importing = false;
                match result {
                    Ok((log, imported)) => {
                        self.accept(log);
                        self.import_report = Some(import_report(&imported));
                    }
                    Err(error) => self.error(&error),
                }
            }
        }
        Task::none()
    }

    fn entry(&self) -> Result<Entry, &'static str> {
        let kind = self.form.kind.ok_or("Vælg en type.")?;
        let beskrivelse = self.form.beskrivelse.trim().to_owned();
        if beskrivelse.is_empty() {
            return Err("Skriv en beskrivelse.");
        }
        let route = if kind == ExpenseType::Driving {
            let (fra, til) = (self.form.fra.trim(), self.form.til.trim());
            if fra.is_empty() || til.is_empty() {
                return Err("Skriv både Fra og Til.");
            }
            Some(Route {
                fra: fra.to_owned(),
                til: til.to_owned(),
                km: Some(parse_decimal(&self.form.km).ok_or("Skriv antal km.")?),
            })
        } else {
            None
        };
        Ok(Entry {
            date: self.form.date,
            kind,
            beskrivelse,
            route,
            pris: parse_amount(&self.form.pris).map_err(|e| e.0)?,
            andet: self.form.andet.trim().to_owned(),
        })
    }

    pub fn view(&self) -> Element<'_, Message> {
        let log = match &self.log {
            Loading::Ready(log) => log,
            Loading::Pending => return text("Indlæser …").into(),
            Loading::Failed(error) => {
                return crate::widgets::notice_card::<Message>(
                    Notice::from_message(Tone::Error, error),
                    None,
                )
            }
        };
        let mut sections = row![].spacing(8);
        for (view, label) in [
            (View::Expenses, "Udgifter"),
            (View::Driving, "Kørsel"),
            (View::Add, "Tilføj udgift"),
            (View::Report, "Rapport"),
        ] {
            let selected = self.view == view;
            sections = sections.push(
                button(text(label).size(14))
                    .style(move |theme, status| {
                        if selected {
                            iced::widget::button::secondary(theme, status)
                        } else {
                            crate::widgets::outlined(theme, status)
                        }
                    })
                    .padding([7, 12])
                    .on_press(Message::Show(view)),
            );
        }
        let content = match self.view {
            View::Expenses => self.expenses(log),
            View::Driving => self.driving(log),
            View::Add => self.form(log),
            View::Report => self.report(log),
        };
        // Room for the scrollbar the app puts beside the tab.
        column![sections, content]
            .spacing(16)
            .width(Length::Fill)
            .padding(iced::Padding::ZERO.right(16))
            .into()
    }

    fn expenses<'a>(&'a self, log: &'a Log) -> Column<'a, Message> {
        if log.expenses.is_empty() {
            return column![text("Ingen udgifter endnu.")];
        }
        by_month(newest_first(&log.expenses), |expense| {
            let entry = &expense.entry;
            let mut lines = column![text(format!(
                "{} · {}",
                entry.kind.label(),
                format_kr(entry.pris.into())
            ))]
            .spacing(2)
            .width(Length::Fill);
            lines = lines.push(
                text(format!(
                    "{} · {}",
                    format_date(entry.date),
                    entry.beskrivelse
                ))
                .size(13),
            );
            if let Some(route) = &entry.route {
                lines = lines.push(text(route_line(route)).size(13));
            }
            if !entry.andet.is_empty() {
                lines = lines.push(text(&entry.andet).size(13));
            }
            if let Some(bilag) = &expense.bilag {
                lines = lines.push(text(format!("Bilag: {}", bilag.name)).size(13));
            }
            let actions = if self.confirm_delete == Some(expense.id) {
                row![
                    quiet("Ja, slet", Some(Message::ConfirmDelete)),
                    quiet("Fortryd", Some(Message::CancelDelete)),
                ]
            } else {
                row![
                    quiet("Ret", Some(Message::Edit(expense.id))),
                    quiet("Slet", Some(Message::Delete(expense.id))),
                ]
            };
            row![lines, actions.spacing(8)]
                .spacing(12)
                .align_y(iced::alignment::Vertical::Center)
                .into()
        })
    }

    fn driving<'a>(&'a self, log: &'a Log) -> Column<'a, Message> {
        let trips: Vec<&Expense> = newest_first(&log.expenses)
            .into_iter()
            .filter(|e| e.entry.route.is_some())
            .collect();
        if trips.is_empty() {
            return column![text("Ingen kørsel endnu.")];
        }
        by_month(trips, |expense| {
            let entry = &expense.entry;
            let mut lines = column![text(format!(
                "{} · {}",
                format_date(entry.date),
                format_kr(entry.pris.into())
            ))]
            .spacing(2)
            .width(Length::Fill);
            if let Some(route) = &entry.route {
                lines = lines.push(text(route_line(route)).size(13));
            }
            lines = lines.push(text(&entry.beskrivelse).size(13));
            row![lines, quiet("Kopiér", Some(Message::Copy(expense.id)))]
                .spacing(12)
                .align_y(iced::alignment::Vertical::Center)
                .into()
        })
    }

    /// Type tiles on the left; the chosen type's fields on the right.
    fn form<'a>(&'a self, log: &'a Log) -> Column<'a, Message> {
        let mut tiles = column![text("1 · Hvad har du betalt for?").size(16)].spacing(10);
        for kinds in ExpenseType::ALL.chunks(3) {
            let mut line = row![].spacing(10);
            for &kind in kinds {
                let tile = column![
                    text(icon(kind)).size(24),
                    text(kind.label()).size(13).center()
                ]
                .spacing(6)
                .align_x(iced::alignment::Horizontal::Center);
                line = line.push(
                    button(container(tile).center(Length::Fill))
                        .style(chosen(self.form.kind == Some(kind), 8.0))
                        .padding([8, 4])
                        .width(Length::Fill)
                        .height(Length::Fixed(88.0))
                        .on_press(Message::Kind(kind)),
                );
            }
            tiles = tiles.push(line);
        }
        let tiles = tiles
            .push(text("Hver type viser kun de felter, den skal bruge.").size(13))
            .width(Length::Fixed(420.0));
        let fields = match self.form.kind {
            None => column![
                text("2 · Udfyld").size(16),
                text("Vælg først, hvad du har betalt for."),
            ]
            .spacing(12),
            Some(kind) => self.fields(log, kind),
        };
        column![row![tiles, fields.width(Length::Fill)].spacing(28)]
    }

    fn fields<'a>(&'a self, log: &'a Log, kind: ExpenseType) -> Column<'a, Message> {
        let form = &self.form;
        let driving = kind == ExpenseType::Driving;
        let labelled = |label: &'static str, input: Element<'a, Message>| -> Element<'a, Message> {
            column![text(label).size(13), input].spacing(4).into()
        };
        let input = |value: &'a str, on: fn(String) -> Message| {
            text_input("", value)
                .on_input(on)
                .on_submit(Message::Save)
                .padding(8)
        };
        let mut fields = column![
            text(format!("2 · {}", kind.label())).size(16),
            row![
                labelled(
                    "Dato",
                    crate::widgets::date_picker(
                        form.date,
                        form.calendar,
                        Message::ToggleCalendar,
                        Message::CalendarMonth,
                        Message::PickDate,
                    ),
                ),
                labelled(
                    if driving { "Formål" } else { "Beskrivelse" },
                    input(&form.beskrivelse, Message::Beskrivelse).into(),
                ),
            ]
            .spacing(12),
        ]
        .spacing(12);
        if driving {
            for (field, label, value) in [
                (AddressField::Fra, "Fra", &form.fra),
                (AddressField::Til, "Til", &form.til),
            ] {
                let mut address = column![text_input("", value)
                    .on_input(move |value| Message::Address(field, value))
                    .padding(8)]
                .spacing(6);
                if form.suggest == Some(field) {
                    for suggestion in address_suggestions(log, value) {
                        address = address.push(
                            button(text(suggestion.label).size(13))
                                .style(iced::widget::button::text)
                                .on_press(Message::Suggested(field, suggestion.value)),
                        );
                    }
                }
                // Frequent addresses are one press away, by nickname.
                if !log.addresses.is_empty() {
                    let mut chips = row![].spacing(8);
                    for frequent in &log.addresses {
                        chips = chips.push(
                            button(text(&frequent.nickname).size(14))
                                .style(chosen(*value == frequent.address, 16.0))
                                .padding([6, 12])
                                .on_press(Message::Suggested(field, frequent.address.clone())),
                        );
                    }
                    address = address.push(chips.wrap().vertical_spacing(8));
                }
                fields = fields.push(labelled(label, address.into()));
                if field == AddressField::Fra {
                    fields = fields.push(quiet("⇅ Byt Fra og Til", Some(Message::Swap)));
                }
            }
            if log.price_per_km.is_none() {
                fields = fields.push(price_missing());
            }
        }
        let amount = labelled("Beløb (kr)", input(&form.pris, Message::Pris).into());
        fields = fields.push(if driving {
            row![labelled("Km", input(&form.km, Message::Km).into()), amount].spacing(12)
        } else {
            row![amount, space::horizontal()].spacing(12)
        });
        if let (true, Some(km), Some(price)) = (driving, parse_decimal(&form.km), log.price_per_km)
        {
            fields = fields.push(
                text(format!(
                    "{} km × {} kr/km",
                    format_km(km),
                    price.to_string().replace('.', ",")
                ))
                .size(13),
            );
        }
        fields = fields.push(labelled(
            "Note (valgfri)",
            input(&form.andet, Message::Andet).into(),
        ));
        let bilag_name = match &form.bilag {
            FormBilag::None => None,
            FormBilag::Saved(bilag) => Some(bilag.name.clone()),
            FormBilag::Chosen(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
        };
        fields = fields.push(match bilag_name {
            None => row![quiet("Tilføj bilag", Some(Message::ChooseBilag))],
            Some(name) => row![
                text(format!("Bilag: {name}")).size(14),
                quiet("Skift", Some(Message::ChooseBilag)),
                quiet("Fjern", Some(Message::RemoveBilag)),
            ]
            .spacing(10)
            .align_y(iced::alignment::Vertical::Center),
        });
        let save = (!self.saving).then_some(Message::Save);
        fields
            .push(rule::horizontal(1))
            .push(if form.editing.is_some() {
                row![
                    primary("Gem ændring", save),
                    quiet("Fortryd", Some(Message::CancelEdit))
                ]
                .spacing(8)
            } else {
                row![primary("Gem udgift", save)]
            })
    }

    fn report<'a>(&'a self, log: &'a Log) -> Column<'a, Message> {
        let mut report = column![row![
            primary(
                "Eksportér rapport",
                (!self.exporting).then_some(Message::Export)
            ),
            text(format!("{} udgifter", log.expenses.len())),
        ]
        .spacing(12)
        .align_y(iced::alignment::Vertical::Center)]
        .spacing(10);
        if let Some(missing) = &self.export_missing {
            let mut names = column![text("Disse bilag findes ikke længere:")].spacing(2);
            for name in missing {
                names = names.push(text(name.clone()).size(13));
            }
            report = report.push(names).push(
                row![
                    quiet(
                        "Eksportér alligevel",
                        (!self.exporting).then_some(Message::ExportAnyway)
                    ),
                    quiet("Fortryd", Some(Message::CancelExport)),
                ]
                .spacing(8),
            );
        }
        if let Some(estimate) = MonthlyEstimate::new(&log.expenses) {
            report = report.push(
                text(format!(
                    "Estimeret pr. måned: {}",
                    format_kr(estimate.total)
                ))
                .size(18),
            );
            for (kind, amount) in &estimate.by_type {
                report =
                    report.push(text(format!("{}: {}", kind.label(), format_kr(*amount))).size(13));
            }
            report = report.push(
                text(format!(
                    "Ud fra {} dages registrering.",
                    estimate.covered_days
                ))
                .size(13),
            );
        }
        report
    }

    /// Indstillinger → Kompensation: the app's settings, the reminder and the
    /// one-time import.
    pub fn settings_view(&self) -> Element<'_, Message> {
        let ready = self.log().is_some();
        let mut settings = column![toggler(self.reminds())
            .label("Påmind om udgifter, når en uge er overført")
            .on_toggle_maybe(ready.then_some(Message::Remind))]
        .spacing(12);
        if let Loading::Failed(error) = &self.log {
            settings = settings.push(text(error.clone()).size(13));
        }
        settings = settings.push(
            row![
                text("Pris pr. km"),
                text_input("Fx 3,79", &self.price_draft)
                    .on_input(Message::PriceDraft)
                    .on_submit(Message::SavePrice)
                    .padding(8)
                    .width(Length::Fixed(150.0)),
                quiet("Gem pris pr. km", ready.then_some(Message::SavePrice)),
            ]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center),
        );
        let mut addresses = column![
            text("Hyppige adresser").size(16),
            row![
                text_input("Kaldenavn, fx Hjem", &self.nickname_draft)
                    .on_input(Message::NicknameDraft)
                    .padding(8)
                    .width(Length::Fixed(180.0)),
                text_input("Adresse", &self.address_draft)
                    .on_input(Message::AddressDraft)
                    .on_submit(Message::AddAddress)
                    .padding(8),
                quiet("Tilføj", ready.then_some(Message::AddAddress)),
            ]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center),
        ]
        .spacing(8);
        for address in self
            .log()
            .map(|log| log.addresses.as_slice())
            .unwrap_or_default()
        {
            addresses = addresses.push(
                row![
                    text(format!("{}: {}", address.nickname, address.address)),
                    space::horizontal(),
                    quiet(
                        "Fjern",
                        Some(Message::RemoveAddress(address.nickname.clone()))
                    ),
                ]
                .align_y(iced::alignment::Vertical::Center),
            );
        }
        settings = settings.push(addresses).push(quiet(
            "Importér fra kompensationsydelsesapp",
            (ready && !self.importing).then_some(Message::Import),
        ));
        if let Some(report) = &self.import_report {
            settings = settings.push(text(report.clone()).size(13));
        }
        settings.into()
    }
}

/// Cards under a heading per month, like the app's list.
fn by_month<'a>(
    expenses: Vec<&'a Expense>,
    card: impl Fn(&'a Expense) -> Element<'a, Message>,
) -> Column<'a, Message> {
    let mut list = column![].spacing(8);
    let mut month = None;
    for expense in expenses {
        let label = month_label(expense.entry.date);
        if month.as_ref() != Some(&label) {
            list = list.push(text(label.clone()).size(16));
            month = Some(label);
        }
        list = list.push(crate::widgets::card(card(expense)));
    }
    list
}

/// »Hjemvej 1 → Hallen 2 · 12,4 km«
fn route_line(route: &Route) -> String {
    match route.km {
        Some(km) => format!("{} → {} · {} km", route.fra, route.til, format_km(km)),
        None => format!("{} → {}", route.fra, route.til),
    }
}

fn import_report(imported: &Imported) -> String {
    let mut parts = vec![format!(
        "Importerede {} udgifter og {} bilag.",
        imported.expenses, imported.bilag
    )];
    if imported.skipped > 0 {
        parts.push(format!("{} fandtes allerede.", imported.skipped));
    }
    if imported.unreadable > 0 {
        parts.push(format!("{} rækker kunne ikke læses.", imported.unreadable));
    }
    if !imported.unmatched.is_empty() {
        parts.push(format!("Tilføj selv: {}.", imported.unmatched.join(", ")));
    }
    parts.join(" ")
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

/// The icon on an expense type's tile.
fn icon(kind: ExpenseType) -> &'static str {
    match kind {
        ExpenseType::Driving => "🚗",
        ExpenseType::Medicine => "💊",
        ExpenseType::Diet => "🍎",
        ExpenseType::Rent => "🏠",
        ExpenseType::Leisure => "🎟",
        ExpenseType::Courses => "🎓",
        ExpenseType::Clothing => "👕",
        ExpenseType::Utilities => "💡",
        ExpenseType::Other => "…",
    }
}

/// An outlined choice. The selected one gets a thicker accent border and a
/// tint, so the choice does not depend on colour alone.
fn chosen(
    selected: bool,
    radius: f32,
) -> impl Fn(&iced::Theme, iced::widget::button::Status) -> iced::widget::button::Style {
    move |theme, status| {
        let mut style = crate::widgets::outlined(theme, status);
        style.border.radius = radius.into();
        if selected {
            let accent = theme.extended_palette().primary.base.color;
            style.border.color = accent;
            style.border.width = 2.0;
            style.background = Some(accent.scale_alpha(0.2).into());
        }
        style
    }
}

/// Kørsel is priced from pris pr. km, so its absence is said where the
/// amount is entered, with the way to fix it.
fn price_missing<'a>() -> Element<'a, Message> {
    container(
        row![
            text("! Pris pr. km mangler, så beløbet kan ikke regnes ud.")
                .size(14)
                .width(Length::Fill),
            quiet("Sæt pris pr. km", Some(Message::PriceSettings)),
        ]
        .spacing(10)
        .align_y(iced::alignment::Vertical::Center),
    )
    .padding([8, 12])
    .width(Length::Fill)
    .style(|theme: &iced::Theme| {
        let warning = theme.extended_palette().warning.base.color;
        iced::widget::container::Style {
            background: Some(warning.scale_alpha(0.12).into()),
            border: iced::Border {
                color: warning.scale_alpha(0.6),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..iced::widget::container::Style::default()
        }
    })
    .into()
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
        let _ = ui.update(Message::Kind(ExpenseType::Medicine));
        let _ = ui.update(Message::Beskrivelse("Piller".into()));
        let _ = ui.update(Message::Pris("abc".into()));
        assert_eq!(ui.update(Message::Save).units(), 0);
        assert!(ui.take_notice().is_some());
        assert_eq!(ui.form.pris, "abc");

        let _ = ui.update(Message::Pris("350".into()));
        assert!(ui.update(Message::Save).units() > 0);
        // Another change finishing meanwhile leaves the form save running.
        let _ = ui.update(Message::Saved(Ok(Log::default())));
        assert_eq!(ui.update(Message::Save).units(), 0);
        let _ = ui.update(Message::FormSaved(Err("Kunne ikke gemmes.".into())));
        assert_eq!(ui.form.pris, "350");
        assert!(ui.take_notice().is_some());

        let _ = ui.update(Message::Save);
        let _ = ui.update(Message::FormSaved(Ok(Log::default())));
        assert!(ui.form.pris.is_empty());
        assert_eq!(ui.view, View::Expenses);
    }

    #[test]
    fn a_price_per_km_saved_later_prices_typed_km_but_keeps_a_typed_amount() {
        let mut ui = CompensationUi::loaded(Log::default());
        let _ = ui.update(Message::Kind(ExpenseType::Driving));
        let _ = ui.update(Message::Km("10".into()));
        assert!(ui.form.pris.is_empty());
        let mut priced = Log::default();
        priced.price_per_km = Some(3.79);
        let _ = ui.update(Message::Saved(Ok(priced.clone())));
        assert_eq!(ui.form.pris, "38");

        let _ = ui.update(Message::Pris("40".into()));
        let _ = ui.update(Message::Saved(Ok(priced)));
        assert_eq!(ui.form.pris, "40");
    }

    #[test]
    fn a_route_driven_before_fills_in_km_and_price() {
        let mut log = Log::default();
        log.price_per_km = Some(3.79);
        log.expenses.push(Expense {
            id: 1,
            entry: Entry {
                date: "2026-09-01".parse().unwrap(),
                kind: ExpenseType::Driving,
                beskrivelse: "Træning".into(),
                route: Some(Route {
                    fra: "Hjemvej 1".into(),
                    til: "Hallen 2".into(),
                    km: Some(12.4),
                }),
                pris: 47,
                andet: String::new(),
            },
            bilag: None,
        });
        let mut ui = CompensationUi::loaded(log);
        let _ = ui.update(Message::Kind(ExpenseType::Driving));
        let _ = ui.update(Message::Address(AddressField::Fra, "Hallen 2".into()));
        let _ = ui.update(Message::Suggested(AddressField::Til, "Hjemvej 1".into()));
        assert_eq!(ui.form.km, "12,4");
        assert_eq!(ui.form.pris, "47");
        // Typing on towards another address leaves no stale km or price.
        let _ = ui.update(Message::Address(AddressField::Til, "Hjemvej 12".into()));
        assert_eq!((ui.form.km.as_str(), ui.form.pris.as_str()), ("", ""));
        let _ = ui.update(Message::Km("20".into()));
        assert_eq!(ui.form.pris, "76");
        let _ = ui.update(Message::Address(AddressField::Til, "Hjemvej 13".into()));
        assert_eq!(ui.form.km, "20");
        let _ = ui.view();
    }
}
