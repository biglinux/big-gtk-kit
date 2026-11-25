// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Shared shortcut editor with click-to-rebind capture.
//!
//! Rows are grouped by section in the order the sections first appear:
//! editable rows first, then the fixed rows a widget owns.

use adw::prelude::*;
use big_gtk_kit::tooltip::name_control;
use gtk::{gdk, glib};
use relm4::{Component, ComponentParts, ComponentSender};

use super::shortcut_search::ShortcutSearch;

/// Where a row the editor made keeps its accelerators, for [`search_finds_row`].
const ROW_ACCELS: &str = "big-shortcut-editor-accels";

/// One editable shortcut row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigShortcutEntry {
    /// User-visible title of the section that lists the row.
    pub section: String,
    /// User-visible row title.
    pub title: String,
    /// Stable action id used by persistence.
    pub action: String,
    /// Default GTK accelerator.
    pub default_accel: String,
    /// Current GTK accelerator after custom overrides.
    pub current_accel: String,
    /// Keys accepted by this row's capture dialog.
    pub capture_policy: BigShortcutCapturePolicy,
}

impl BigShortcutEntry {
    /// Create an editable shortcut entry.
    pub fn new(
        section: impl Into<String>,
        title: impl Into<String>,
        action: impl Into<String>,
        default_accel: impl Into<String>,
        current_accel: impl Into<String>,
    ) -> Self {
        Self {
            section: section.into(),
            title: title.into(),
            action: action.into(),
            default_accel: default_accel.into(),
            current_accel: current_accel.into(),
            capture_policy: BigShortcutCapturePolicy::Any,
        }
    }

    /// Restrict which kind of key the capture dialog accepts.
    #[must_use]
    pub const fn with_capture_policy(mut self, capture_policy: BigShortcutCapturePolicy) -> Self {
        self.capture_policy = capture_policy;
        self
    }
}

/// Key shape accepted by one shortcut editor row.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum BigShortcutCapturePolicy {
    /// Accept bare keys and modified shortcuts.
    #[default]
    Any,
    /// Require Ctrl, Alt, or Shift, for application/window accelerators.
    ModifiedKey,
}

/// A row of keys a widget handles itself: listed with its section, never
/// rebound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigShortcutFixedRow {
    /// User-visible title of the section that lists the row.
    pub section: String,
    /// User-visible row title.
    pub title: String,
    /// GTK accelerators; a row without one is not shown.
    pub accels: Vec<String>,
}

impl BigShortcutFixedRow {
    /// Create a fixed shortcut row.
    pub fn new(section: impl Into<String>, title: impl Into<String>, accels: Vec<String>) -> Self {
        Self {
            section: section.into(),
            title: title.into(),
            accels,
        }
    }
}

/// Text supplied by the app for the shortcut editor controls.
#[derive(Debug, Clone)]
pub struct BigShortcutEditorLabels {
    /// Tooltip/name for the rebind button.
    pub change: String,
    /// Tooltip/name for the reset button.
    pub reset: String,
    /// Capture dialog title.
    pub capture_title: String,
    /// Capture dialog hint.
    pub capture_hint: String,
    /// Sentence naming the shortcut that already has the key, with `{owner}`.
    pub conflict: String,
    /// Capture dialog button that takes the key from its owner.
    pub replace: String,
    /// Capture dialog button that leaves the shortcut as it was.
    pub cancel: String,
    /// What a row with no key shows.
    pub disabled: String,
    /// Error shown when a modified-key row receives a bare key.
    pub modified_key_required: String,
}

impl BigShortcutEditorLabels {
    /// Translate the standard shortcut-editor vocabulary at the app boundary.
    #[must_use]
    pub fn translated(gettext: impl Fn(&str) -> String) -> Self {
        Self {
            change: gettext("Change shortcut"),
            reset: gettext("Reset shortcut"),
            capture_title: gettext("Set Shortcut"),
            capture_hint: gettext(
                "Press the new shortcut, Backspace to turn it off, or Esc to cancel.",
            ),
            conflict: gettext("“{owner}” already uses this shortcut."),
            replace: gettext("Replace"),
            cancel: gettext("Cancel"),
            disabled: gettext("Disabled"),
            modified_key_required: gettext("Press a shortcut with Ctrl, Alt, or Shift."),
        }
    }
}

/// A persisted shortcut change requested by the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigShortcutChange {
    /// Action id being changed.
    pub action: String,
    /// New accelerator; empty turns the shortcut off.
    pub accel: String,
    /// Default accelerator for this action.
    pub default_accel: String,
}

/// One shortcut that participates in conflict detection without being editable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigShortcutReservation {
    /// User-visible owner name shown when a conflict is detected.
    pub title: String,
    /// Reserved GTK accelerator.
    pub accel: String,
}

impl BigShortcutReservation {
    /// Create a reserved shortcut.
    pub fn new(title: impl Into<String>, accel: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            accel: accel.into(),
        }
    }
}

/// Initialization data for [`BigShortcutEditor`].
#[derive(Debug, Clone)]
pub struct BigShortcutEditorInit {
    /// Preferences page title.
    pub title: String,
    /// Editable shortcut bindings.
    pub entries: Vec<BigShortcutEntry>,
    /// Keys widgets handle, listed after the editable rows of their section.
    pub fixed_rows: Vec<BigShortcutFixedRow>,
    /// Non-editable bindings used for conflict detection.
    pub reservations: Vec<BigShortcutReservation>,
    /// User-visible labels and accelerator formatter.
    pub labels: BigShortcutEditorLabels,
}

impl BigShortcutEditorInit {
    /// Build editor initialization data without fixed rows or reserved
    /// shortcuts.
    pub fn new(
        title: impl Into<String>,
        entries: Vec<BigShortcutEntry>,
        labels: BigShortcutEditorLabels,
    ) -> Self {
        Self {
            title: title.into(),
            entries,
            fixed_rows: Vec::new(),
            reservations: Vec::new(),
            labels,
        }
    }

    /// List keys widgets handle in their sections.
    #[must_use]
    pub fn with_fixed_rows(mut self, fixed_rows: Vec<BigShortcutFixedRow>) -> Self {
        self.fixed_rows = fixed_rows;
        self
    }

    /// Add non-editable shortcuts to conflict detection.
    #[must_use]
    pub fn with_reservations(mut self, reservations: Vec<BigShortcutReservation>) -> Self {
        self.reservations = reservations;
        self
    }
}

/// Typed messages accepted by [`BigShortcutEditor`].
#[derive(Debug, Clone)]
pub enum BigShortcutEditorInput {
    /// Begin keyboard capture for one stable action ID.
    BeginCapture(String),
    /// Reset one action to its default accelerator.
    Reset(String),
    /// Commit an accelerator captured by the shared dialog; empty turns the
    /// shortcut off.
    CommitCapture(String),
    /// Give the captured accelerator to the active row and turn off the row
    /// that had it.
    ReplaceConflict,
    /// Cancel the active keyboard capture.
    CancelCapture,
    /// The capture dialog closed, by whatever way.
    CaptureClosed,
}

/// Typed events emitted by [`BigShortcutEditor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BigShortcutEditorOutput {
    /// A valid binding change that the application should persist.
    Changed(BigShortcutChange),
    /// A proposed binding was rejected because a shortcut nobody can change
    /// owns it.
    ConflictRejected {
        /// The action whose binding was being changed.
        action: String,
        /// The action that owns the binding.
        owner: String,
    },
    /// Capture ended without changing the active action.
    CaptureCancelled {
        /// The action whose binding was being captured.
        action: String,
    },
}

/// Shared stateful shortcut editor component.
pub struct BigShortcutEditor {
    rows: Vec<BigShortcutRow>,
    reservations: Vec<BigShortcutReservation>,
    labels: BigShortcutEditorLabels,
    active_action: Option<String>,
    /// The accelerator another editable row holds, while the capture dialog
    /// offers to replace it.
    contested_accel: Option<String>,
}

struct BigShortcutRow {
    action: String,
    default_accel: String,
    title: String,
    capture_policy: BigShortcutCapturePolicy,
    row: adw::ActionRow,
    current_label: gtk::Label,
    reset_button: gtk::Button,
    current_accel: String,
}

/// Internal widget collection maintained by the Relm4 component runtime.
pub struct BigShortcutEditorWidgets {
    dialog: adw::Dialog,
    capture_title: gtk::Label,
    conflict_label: gtk::Label,
    replace_button: gtk::Button,
    capture_content: gtk::Box,
}

impl Component for BigShortcutEditor {
    type Init = BigShortcutEditorInit;
    type Input = BigShortcutEditorInput;
    type Output = BigShortcutEditorOutput;
    type CommandOutput = ();
    type Root = adw::PreferencesPage;
    type Widgets = BigShortcutEditorWidgets;

    fn init_root() -> Self::Root {
        adw::PreferencesPage::new()
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        root.set_title(&init.title);
        let mut sections: Vec<&str> = Vec::new();
        let titles = init.entries.iter().map(|entry| entry.section.as_str());
        for section in titles.chain(init.fixed_rows.iter().map(|row| row.section.as_str())) {
            if !sections.contains(&section) {
                sections.push(section);
            }
        }
        let mut rows = Vec::new();
        for section in sections {
            let group = adw::PreferencesGroup::builder().title(section).build();
            let mut has_rows = false;
            for entry in init.entries.iter().filter(|entry| entry.section == section) {
                let editor_row = shortcut_row(entry, &init.labels, &sender);
                group.add(&editor_row.row);
                rows.push(editor_row);
                has_rows = true;
            }
            for fixed in init.fixed_rows.iter().filter(|row| row.section == section) {
                let accels: Vec<String> = fixed
                    .accels
                    .iter()
                    .filter(|accel| !accel.is_empty())
                    .map(|accel| accelerator_label(accel, &init.labels))
                    .collect();
                if accels.is_empty() {
                    continue;
                }
                let row = adw::ActionRow::builder()
                    .title(fixed.title.as_str())
                    .build();
                let label = gtk::Label::new(Some(&accels.join("  ")));
                label.add_css_class("dimmed");
                row.add_suffix(&label);
                keep_accels(&row, fixed.accels.clone());
                group.add(&row);
                has_rows = true;
            }
            if has_rows {
                root.add(&group);
            }
        }
        let widgets = capture_dialog(&init.labels, &sender);
        let model = Self {
            rows,
            reservations: init.reservations,
            labels: init.labels,
            active_action: None,
            contested_accel: None,
        };
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            BigShortcutEditorInput::BeginCapture(action) => {
                self.begin_capture(widgets, root, action);
            }
            BigShortcutEditorInput::Reset(action) => {
                self.commit_default(&sender, &action);
            }
            BigShortcutEditorInput::CommitCapture(accel) => {
                self.commit_capture(widgets, &sender, accel);
            }
            BigShortcutEditorInput::ReplaceConflict => {
                self.replace_conflict(widgets, &sender);
            }
            BigShortcutEditorInput::CancelCapture => {
                if self.end_capture(&sender) {
                    widgets.dialog.close();
                }
            }
            BigShortcutEditorInput::CaptureClosed => {
                self.end_capture(&sender);
            }
        }
    }
}

fn shortcut_row(
    entry: &BigShortcutEntry,
    labels: &BigShortcutEditorLabels,
    sender: &ComponentSender<BigShortcutEditor>,
) -> BigShortcutRow {
    let row = adw::ActionRow::builder()
        .title(entry.title.as_str())
        .build();
    let current_label = gtk::Label::builder()
        .label(accelerator_label(&entry.current_accel, labels))
        .xalign(1.0)
        .build();
    current_label.add_css_class("dimmed");

    let change_button = gtk::Button::builder()
        .icon_name("input-keyboard-symbolic")
        .valign(gtk::Align::Center)
        .build();
    name_control(&change_button, &labels.change);

    let reset_button = gtk::Button::builder()
        .icon_name("edit-undo-symbolic")
        .valign(gtk::Align::Center)
        .build();
    name_control(&reset_button, &labels.reset);
    reset_button.set_visible(!same_accel(&entry.current_accel, &entry.default_accel));

    let action = entry.action.clone();
    let sender_for_change = sender.input_sender().clone();
    change_button.connect_clicked(move |_| {
        let _ = sender_for_change.send(BigShortcutEditorInput::BeginCapture(action.clone()));
    });
    let action = entry.action.clone();
    let sender_for_reset = sender.input_sender().clone();
    reset_button.connect_clicked(move |_| {
        let _ = sender_for_reset.send(BigShortcutEditorInput::Reset(action.clone()));
    });

    keep_accels(&row, vec![entry.current_accel.clone()]);
    row.add_suffix(&current_label);
    row.add_suffix(&change_button);
    row.add_suffix(&reset_button);
    // Activating the row changes the shortcut, as its button does.
    row.set_activatable_widget(Some(&change_button));
    BigShortcutRow {
        action: entry.action.clone(),
        default_accel: entry.default_accel.clone(),
        title: entry.title.clone(),
        capture_policy: entry.capture_policy,
        row,
        current_label,
        reset_button,
        current_accel: entry.current_accel.clone(),
    }
}

fn keep_accels(row: &adw::ActionRow, accels: Vec<String>) {
    // SAFETY: `ROW_ACCELS` only ever holds a `Vec<String>`, here and in
    // `search_finds_row`.
    unsafe { row.set_data(ROW_ACCELS, accels) };
}

/// Whether `search` finds `row` of a [`BigShortcutEditor`] page: by its title,
/// or by its keys when the editor made it, as `ctrl+t` finds Ctrl+T.
#[must_use]
pub fn search_finds_row(search: &ShortcutSearch, row: &adw::PreferencesRow) -> bool {
    // SAFETY: `ROW_ACCELS` only ever holds a `Vec<String>` (`keep_accels`), which
    // the row owns while it lives.
    let accels: Vec<String> = unsafe { row.data::<Vec<String>>(ROW_ACCELS) }
        .map(|accels| {
            // SAFETY: the row holds the list while it lives.
            unsafe { accels.as_ref() }.clone()
        })
        .unwrap_or_default();
    let parsed: Vec<(gdk::Key, gdk::ModifierType)> = accels
        .iter()
        .map(String::as_str)
        .filter_map(gtk::accelerator_parse)
        .collect();
    let names: Vec<(gdk::ModifierType, String)> = parsed
        .iter()
        .filter_map(|(key, modifiers)| Some((*modifiers, key.name()?.to_string())))
        .collect();
    let keys: Vec<(gdk::ModifierType, &str)> = names
        .iter()
        .map(|(modifiers, name)| (*modifiers, name.as_str()))
        .collect();
    // The keys as the row shows them, in the person's language.
    let label = parsed
        .iter()
        .map(|(key, modifiers)| gtk::accelerator_get_label(*key, *modifiers).to_string())
        .collect::<Vec<_>>()
        .join(" ");
    search.finds(&row.title(), &label, &keys)
}

fn capture_dialog(
    labels: &BigShortcutEditorLabels,
    sender: &ComponentSender<BigShortcutEditor>,
) -> BigShortcutEditorWidgets {
    // Kept with the editor and presented again for every capture, so its
    // breakpoints are made once (not a pooled `big_gtk_kit::dialog::Dialog`,
    // which gives its child up when it closes).
    #[expect(
        clippy::disallowed_methods,
        reason = "one dialog for the editor's life"
    )]
    let dialog = adw::Dialog::builder()
        .title(&labels.capture_title)
        .content_width(360)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .focusable(true)
        .spacing(12)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    let capture_title = gtk::Label::builder().wrap(true).build();
    content.append(&capture_title);
    let capture_hint = gtk::Label::builder()
        .label(&labels.capture_hint)
        .wrap(true)
        .build();
    content.append(&capture_hint);
    // Under the capture area: why the shortcut pressed cannot be used.
    let conflict_label = gtk::Label::builder()
        .wrap(true)
        .visible(false)
        .css_classes(["error"])
        .accessible_role(gtk::AccessibleRole::Alert)
        .build();
    content.append(&conflict_label);
    let replace_button = gtk::Button::builder()
        .label(&labels.replace)
        .halign(gtk::Align::Center)
        .visible(false)
        .css_classes(["pill", "destructive-action"])
        .build();
    let input = sender.input_sender().clone();
    replace_button.connect_clicked(move |_| {
        let _ = input.send(BigShortcutEditorInput::ReplaceConflict);
    });
    content.append(&replace_button);

    let cancel_button = gtk::Button::with_label(&labels.cancel);
    let weak_dialog = dialog.downgrade();
    cancel_button.connect_clicked(move |_| {
        if let Some(dialog) = weak_dialog.upgrade() {
            dialog.close();
        }
    });
    let header = crate::layout::window_handle::header_bar(
        adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false),
    );
    header.pack_start(&cancel_button);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    dialog.set_child(Some(&toolbar));
    // However the dialog goes (Cancel, Escape, a click outside), the capture
    // ends with it; a commit has already cleared it by then.
    let input = sender.input_sender().clone();
    dialog.connect_closed(move |_| {
        let _ = input.send(BigShortcutEditorInput::CaptureClosed);
    });

    let key = gtk::EventControllerKey::new();
    let input = sender.input_sender().clone();
    let replace = replace_button.downgrade();
    key.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gdk::Key::Escape {
            let _ = input.send(BigShortcutEditorInput::CancelCapture);
            return glib::Propagation::Stop;
        }
        // A bare Tab is the keyboard's way to Replace while a conflict shows it.
        let replace_shown = replace.upgrade().is_some_and(|button| button.is_visible());
        let held = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK;
        if replace_shown && key == gdk::Key::Tab && !modifiers.intersects(held) {
            return glib::Propagation::Proceed;
        }
        let Some(accel) = key_accel(key, modifiers) else {
            return glib::Propagation::Proceed;
        };
        let _ = input.send(BigShortcutEditorInput::CommitCapture(accel));
        glib::Propagation::Stop
    });
    content.add_controller(key);
    BigShortcutEditorWidgets {
        dialog,
        capture_title,
        conflict_label,
        replace_button,
        capture_content: content,
    }
}

impl BigShortcutEditor {
    fn begin_capture(
        &mut self,
        widgets: &BigShortcutEditorWidgets,
        root: &adw::PreferencesPage,
        action: String,
    ) {
        let Some(row) = self.rows.iter().find(|row| row.action == action) else {
            return;
        };
        let Some(parent) = root.root().and_downcast::<gtk::Window>() else {
            return;
        };
        self.active_action = Some(action);
        self.contested_accel = None;
        widgets.capture_title.set_label(&row.title);
        widgets.conflict_label.set_visible(false);
        widgets.replace_button.set_visible(false);
        widgets.dialog.present(Some(&parent));
        widgets.capture_content.grab_focus();
    }

    fn commit_default(&mut self, sender: &ComponentSender<Self>, action: &str) {
        let Some(index) = self.rows.iter().position(|row| row.action == action) else {
            return;
        };
        let default_accel = self.rows[index].default_accel.clone();
        if let Some(owner) = conflict_title(&self.rows, &self.reservations, action, &default_accel)
        {
            set_conflict_text(&self.rows[index], &self.labels.conflict, &owner);
            let _ = sender.output(BigShortcutEditorOutput::ConflictRejected {
                action: action.to_owned(),
                owner,
            });
            return;
        }
        self.set_accel(sender, index, &default_accel);
    }

    /// Give row `index` the accelerator `accel` and tell the application.
    fn set_accel(&mut self, sender: &ComponentSender<Self>, index: usize, accel: &str) {
        let row = &mut self.rows[index];
        row.current_accel = accel.to_owned();
        keep_accels(&row.row, vec![accel.to_owned()]);
        row.current_label
            .set_label(&accelerator_label(accel, &self.labels));
        row.reset_button
            .set_visible(!same_accel(accel, &row.default_accel));
        clear_conflict_text(row);
        let _ = sender.output(BigShortcutEditorOutput::Changed(BigShortcutChange {
            action: row.action.clone(),
            accel: accel.to_owned(),
            default_accel: row.default_accel.clone(),
        }));
        refresh_conflicts(&self.rows);
    }

    fn commit_capture(
        &mut self,
        widgets: &BigShortcutEditorWidgets,
        sender: &ComponentSender<Self>,
        accel: String,
    ) {
        let Some(action) = self.active_action.clone() else {
            return;
        };
        let Some(index) = self.rows.iter().position(|row| row.action == action) else {
            return;
        };
        self.contested_accel = None;
        // Hiding a focused Replace would leave no widget to capture the next key.
        widgets.replace_button.set_visible(false);
        widgets.capture_content.grab_focus();
        if let Some(requirement) =
            capture_requirement(self.rows[index].capture_policy, &accel, &self.labels)
        {
            show_capture_error(widgets, requirement);
            return;
        }
        if let Some(owner) = conflict_action_for_rows(&self.rows, &action, &accel) {
            let owner = self.rows[owner].title.clone();
            show_capture_error(widgets, &conflict_message(&self.labels.conflict, &owner));
            // The focus stays on the capture box, so Space or Enter is captured
            // like any key; Tab moves to Replace.
            widgets.replace_button.set_visible(true);
            self.contested_accel = Some(accel);
            return;
        }
        if let Some(owner) =
            reservation_conflict_title(&self.reservations, &normalize_accel(&accel))
        {
            show_capture_error(widgets, &conflict_message(&self.labels.conflict, &owner));
            let _ = sender.output(BigShortcutEditorOutput::ConflictRejected { action, owner });
            return;
        }
        self.active_action = None;
        widgets.dialog.close();
        self.set_accel(sender, index, &accel);
    }

    fn replace_conflict(
        &mut self,
        widgets: &BigShortcutEditorWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let (Some(action), Some(accel)) = (self.active_action.clone(), self.contested_accel.take())
        else {
            return;
        };
        let Some(index) = self.rows.iter().position(|row| row.action == action) else {
            return;
        };
        if let Some(owner) = conflict_action_for_rows(&self.rows, &action, &accel) {
            self.set_accel(sender, owner, "");
        }
        self.active_action = None;
        widgets.dialog.close();
        self.set_accel(sender, index, &accel);
    }

    /// End a capture still running, saying so; `false` when none was.
    fn end_capture(&mut self, sender: &ComponentSender<Self>) -> bool {
        let Some(action) = self.active_action.take() else {
            return false;
        };
        self.contested_accel = None;
        let _ = sender.output(BigShortcutEditorOutput::CaptureCancelled { action });
        true
    }
}

fn show_capture_error(widgets: &BigShortcutEditorWidgets, message: &str) {
    widgets.conflict_label.set_label(message);
    widgets.conflict_label.set_visible(true);
    crate::feedback::announce::announce(
        &widgets.conflict_label,
        message,
        gtk::AccessibleAnnouncementPriority::High,
    );
}

fn same_accel(left: &str, right: &str) -> bool {
    normalize_accel(left) == normalize_accel(right)
}

fn capture_requirement<'a>(
    capture_policy: BigShortcutCapturePolicy,
    accel: &str,
    labels: &'a BigShortcutEditorLabels,
) -> Option<&'a str> {
    // Turning a shortcut off fits every policy.
    if accel.is_empty() {
        return None;
    }
    let has_modifiers = accel_has_modifiers(accel);
    match capture_policy {
        BigShortcutCapturePolicy::Any => None,
        BigShortcutCapturePolicy::ModifiedKey if !has_modifiers => {
            Some(&labels.modified_key_required)
        }
        BigShortcutCapturePolicy::ModifiedKey => None,
    }
}

fn accel_has_modifiers(accel: &str) -> bool {
    normalize_accel(accel)
        .split_once(':')
        .is_some_and(|(modifiers, _)| !modifiers.is_empty())
}

fn accelerator_label(accel: &str, labels: &BigShortcutEditorLabels) -> String {
    if accel.is_empty() {
        return labels.disabled.clone();
    }
    gtk::accelerator_parse(accel).map_or_else(
        || accel.to_owned(),
        |(key, modifiers)| gtk::accelerator_get_label(key, modifiers).to_string(),
    )
}

fn clear_conflict_text(row: &BigShortcutRow) {
    row.row.set_subtitle("");
}

fn set_conflict_text(row: &BigShortcutRow, template: &str, owner: &str) {
    row.row.set_subtitle(&conflict_message(template, owner));
}

fn conflict_message(template: &str, owner: &str) -> String {
    template.replace("{owner}", owner)
}

fn refresh_conflicts(rows: &[BigShortcutRow]) {
    for row in rows {
        if conflict_action_for_rows(rows, &row.action, &row.current_accel).is_none() {
            clear_conflict_text(row);
        }
    }
}

/// The index of the other row that holds `accel`.
fn conflict_action_for_rows(rows: &[BigShortcutRow], action: &str, accel: &str) -> Option<usize> {
    let normalized = normalize_accel(accel);
    if normalized.is_empty() {
        return None;
    }
    rows.iter()
        .position(|row| row.action != action && normalize_accel(&row.current_accel) == normalized)
}

fn conflict_title(
    rows: &[BigShortcutRow],
    reservations: &[BigShortcutReservation],
    action: &str,
    accel: &str,
) -> Option<String> {
    let normalized = normalize_accel(accel);
    if normalized.is_empty() {
        return None;
    }
    rows.iter()
        .find(|row| row.action != action && normalize_accel(&row.current_accel) == normalized)
        .map(|row| row.title.clone())
        .or_else(|| reservation_conflict_title(reservations, &normalized))
}

fn normalize_accel(accel: &str) -> String {
    let mut rest = accel.trim();
    let mut modifiers = Vec::new();
    while let Some(without_open) = rest.strip_prefix('<') {
        let Some((modifier, remaining)) = without_open.split_once('>') else {
            break;
        };
        modifiers.push(normalize_modifier(modifier));
        rest = remaining;
    }
    if modifiers.is_empty() && rest.contains('+') {
        let mut parts = rest.split('+').collect::<Vec<_>>();
        if let Some(key) = parts.pop() {
            modifiers.extend(parts.into_iter().map(normalize_modifier));
            rest = key;
        }
    }
    let key = rest.trim().to_ascii_lowercase();
    if key.is_empty() {
        return String::new();
    }
    modifiers.sort_unstable();
    modifiers.dedup();
    format!("{}:{key}", modifiers.join("+"))
}

fn normalize_modifier(modifier: &str) -> String {
    match modifier.trim().to_ascii_lowercase().as_str() {
        "control" | "ctrl" | "primary" => "ctrl".to_string(),
        "mod1" | "alt" => "alt".to_string(),
        other => other.to_string(),
    }
}

fn reservation_conflict_title(
    reservations: &[BigShortcutReservation],
    normalized_accel: &str,
) -> Option<String> {
    if normalized_accel.is_empty() {
        return None;
    }
    reservations
        .iter()
        .find(|reservation| normalize_accel(&reservation.accel) == normalized_accel)
        .map(|reservation| reservation.title.clone())
}

/// The accelerator a key press asks for; empty for a bare Backspace, which
/// turns the shortcut off.
fn key_accel(key: gdk::Key, modifiers: gdk::ModifierType) -> Option<String> {
    if is_modifier_key(key) {
        return None;
    }
    if modifiers.intersects(gdk::ModifierType::SUPER_MASK | gdk::ModifierType::HYPER_MASK) {
        return None;
    }
    let held = gdk::ModifierType::CONTROL_MASK
        | gdk::ModifierType::ALT_MASK
        | gdk::ModifierType::SHIFT_MASK;
    if key == gdk::Key::BackSpace && !modifiers.intersects(held) {
        return Some(String::new());
    }
    let key_name = key.name()?.to_string();
    let mut accel = String::new();
    if modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
        accel.push_str("<Ctrl>");
    }
    if modifiers.contains(gdk::ModifierType::ALT_MASK) {
        accel.push_str("<Alt>");
    }
    if modifiers.contains(gdk::ModifierType::SHIFT_MASK) && !key_name_encodes_shift(&key_name) {
        accel.push_str("<Shift>");
    }
    accel.push_str(&key_name);
    Some(accel)
}

fn is_modifier_key(key: gdk::Key) -> bool {
    matches!(
        key,
        gdk::Key::Alt_L
            | gdk::Key::Alt_R
            | gdk::Key::Caps_Lock
            | gdk::Key::Control_L
            | gdk::Key::Control_R
            | gdk::Key::Hyper_L
            | gdk::Key::Hyper_R
            | gdk::Key::ISO_Level3_Shift
            | gdk::Key::ISO_Level5_Shift
            | gdk::Key::Meta_L
            | gdk::Key::Meta_R
            | gdk::Key::Num_Lock
            | gdk::Key::Shift_L
            | gdk::Key::Shift_Lock
            | gdk::Key::Shift_R
            | gdk::Key::Super_L
            | gdk::Key::Super_R
    )
}

fn key_name_encodes_shift(key_name: &str) -> bool {
    matches!(key_name, "exclam" | "at" | "greater" | "less")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::widget_container::children;

    fn labels() -> BigShortcutEditorLabels {
        BigShortcutEditorLabels {
            change: "Change".to_string(),
            reset: "Reset".to_string(),
            capture_title: "Set Shortcut".to_string(),
            capture_hint: "Press a shortcut".to_string(),
            conflict: "{owner} has it".to_string(),
            replace: "Replace".to_string(),
            cancel: "Cancel".to_string(),
            disabled: "Disabled".to_string(),
            modified_key_required: "Press a modified key".to_string(),
        }
    }

    #[test]
    fn standard_labels_use_the_app_translator() {
        let labels = BigShortcutEditorLabels::translated(|msgid| format!("t:{msgid}"));
        assert_eq!(labels.change, "t:Change shortcut");
        assert_eq!(
            labels.modified_key_required,
            "t:Press a shortcut with Ctrl, Alt, or Shift."
        );
    }

    #[test]
    fn reservations_report_user_visible_titles() {
        let reservations = vec![BigShortcutReservation::new("Play / Pause", "space")];

        assert_eq!(
            reservation_conflict_title(&reservations, &normalize_accel("Space")),
            Some("Play / Pause".to_string())
        );
    }

    #[test]
    fn canonicalization_matches_primary_and_control_syntaxes() {
        assert_eq!(
            normalize_accel("<Primary><Shift>Z"),
            normalize_accel("Shift+Ctrl+z")
        );
    }

    #[test]
    fn modifiers_wait_for_main_key() {
        assert_eq!(
            key_accel(gdk::Key::Control_L, gdk::ModifierType::CONTROL_MASK),
            None
        );
        assert_eq!(
            key_accel(gdk::Key::k, gdk::ModifierType::CONTROL_MASK),
            Some("<Ctrl>k".to_string())
        );
    }

    #[test]
    fn capture_policy_requires_modifiers() {
        let labels = labels();

        assert_eq!(
            capture_requirement(BigShortcutCapturePolicy::ModifiedKey, "f", &labels),
            Some("Press a modified key")
        );
        assert_eq!(
            capture_requirement(BigShortcutCapturePolicy::ModifiedKey, "<Ctrl>f", &labels),
            None
        );
    }

    type Outputs = std::rc::Rc<std::cell::RefCell<Vec<BigShortcutEditorOutput>>>;

    /// An editor with Play on Space and Next on N, shown in a window.
    fn shown_editor() -> (relm4::Controller<BigShortcutEditor>, gtk::Window, Outputs) {
        use relm4::{Component, ComponentController};
        let outputs = Outputs::default();
        let sink = std::rc::Rc::clone(&outputs);
        let editor = BigShortcutEditor::builder()
            .launch(BigShortcutEditorInit::new(
                "Shortcuts",
                vec![
                    BigShortcutEntry::new("Playback", "Play", "play", "space", "space"),
                    BigShortcutEntry::new("Playback", "Next", "next", "n", "n"),
                ],
                BigShortcutEditorLabels::translated(str::to_owned),
            ))
            .connect_receiver(move |_, output| sink.borrow_mut().push(output));
        let window = gtk::Window::builder().child(editor.widget()).build();
        window.present();
        (editor, window, outputs)
    }

    fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
        let mut found = Vec::new();
        let mut pending = vec![root.clone()];
        while let Some(widget) = pending.pop() {
            for next in children(&widget) {
                pending.push(next);
            }
            found.push(widget);
        }
        found
    }

    fn button(root: &impl IsA<gtk::Widget>, name: &str) -> gtk::Button {
        descendants(root.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
            .find(|button| {
                button.label().as_deref() == Some(name)
                    || button.tooltip_text().as_deref() == Some(name)
            })
            .unwrap_or_else(|| panic!("no button {name}"))
    }

    fn changed(action: &str, accel: &str) -> BigShortcutEditorOutput {
        let default_accel = if action == "play" { "space" } else { "n" };
        BigShortcutEditorOutput::Changed(BigShortcutChange {
            action: action.to_owned(),
            accel: accel.to_owned(),
            default_accel: default_accel.to_owned(),
        })
    }

    #[test]
    fn backspace_captures_disable() {
        assert_eq!(
            key_accel(gdk::Key::BackSpace, gdk::ModifierType::empty()),
            Some(String::new())
        );
    }

    #[test]
    fn disabled_row_says_disabled() {
        big_testkit::harness::require_gtk_test(|| {
            use relm4::ComponentController;
            let (editor, window, outputs) = shown_editor();
            editor.emit(BigShortcutEditorInput::BeginCapture("next".to_owned()));
            editor.emit(BigShortcutEditorInput::CommitCapture(String::new()));
            big_testkit::harness::pump();
            assert_eq!(editor.model().rows[1].current_label.label(), "Disabled");
            assert_eq!(*outputs.borrow(), [changed("next", "")]);
            window.destroy();
        });
    }

    #[test]
    fn reset_hidden_at_default() {
        big_testkit::harness::require_gtk_test(|| {
            use relm4::ComponentController;
            let (editor, window, _) = shown_editor();
            big_testkit::harness::pump();
            let row = editor.model().rows[1].row.clone();
            assert!(!button(&row, "Reset shortcut").is_visible());
            editor.emit(BigShortcutEditorInput::BeginCapture("next".to_owned()));
            editor.emit(BigShortcutEditorInput::CommitCapture("m".to_owned()));
            big_testkit::harness::pump();
            assert!(button(&row, "Reset shortcut").is_visible());
            window.destroy();
        });
    }

    #[test]
    fn closing_capture_cancels_it() {
        big_testkit::harness::require_gtk_test(|| {
            use relm4::ComponentController;
            let (editor, window, outputs) = shown_editor();
            editor.emit(BigShortcutEditorInput::BeginCapture("next".to_owned()));
            big_testkit::harness::pump();
            let dialog = editor.widgets().dialog.clone();
            dialog.close();
            big_testkit::harness::pump();
            assert_eq!(
                *outputs.borrow(),
                [BigShortcutEditorOutput::CaptureCancelled {
                    action: "next".to_owned()
                }]
            );
            window.destroy();
        });
    }

    #[test]
    fn replace_disables_the_other() {
        big_testkit::harness::require_gtk_test(|| {
            use relm4::ComponentController;
            let (editor, window, outputs) = shown_editor();
            editor.emit(BigShortcutEditorInput::BeginCapture("next".to_owned()));
            editor.emit(BigShortcutEditorInput::CommitCapture("space".to_owned()));
            big_testkit::harness::pump();
            assert!(outputs.borrow().is_empty());
            let dialog = editor.widgets().dialog.clone();
            button(&dialog, "Replace").emit_clicked();
            big_testkit::harness::pump();
            assert_eq!(
                *outputs.borrow(),
                [changed("play", ""), changed("next", "space")]
            );
            assert_eq!(editor.model().rows[0].current_label.label(), "Disabled");
            window.destroy();
        });
    }

    #[test]
    fn conflict_keeps_capture_focus() {
        big_testkit::harness::require_gtk_test(|| {
            use relm4::ComponentController;
            let (editor, window, _) = shown_editor();
            editor.emit(BigShortcutEditorInput::BeginCapture("next".to_owned()));
            editor.emit(BigShortcutEditorInput::CommitCapture("space".to_owned()));
            big_testkit::harness::pump();
            let widgets = editor.widgets();
            assert!(widgets.replace_button.is_visible());
            let focus = widgets.capture_content.root().and_then(|root| root.focus());
            assert_eq!(focus, Some(widgets.capture_content.clone().upcast()));
            window.destroy();
        });
    }
}
