// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A window's notice strip: one line in the window's layout, saying one thing at
//! a time. A confirmation steps down on its own, a problem stays with Close, and
//! at most one button acts on what was said.
//!
//! Never an `adw::Toast`: a toast
//! floats out of Tab's reach and takes its way back away on a timer.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use super::announce::announce_from_window;
use big_gtk_kit::timer::OnceTimer;

/// How long a confirmation stays: long enough to read a sentence, short enough
/// that nobody mistakes it for part of the window.
pub const NOTICE_LINGER: Duration = Duration::from_secs(6);

/// On every notice strip.
const NOTICE_CSS_CLASS: &str = "big-notice";

/// On a strip saying what was refused or went wrong.
const NOTICE_WAITS_CSS_CLASS: &str = "big-notice-waits";

/// What the strip's button does besides closing it, given the strip.
type Offer = Box<dyn Fn(&adw::Banner)>;

struct State {
    close_label: String,
    /// The step-down of the confirmation on show. Replaced by every message,
    /// so an older confirmation's timer never takes down a newer one.
    step_down: OnceTimer,
    offer: RefCell<Option<Offer>>,
    /// What the warning on show is about, when it was given one: a later
    /// success of the same thing takes it down ([`BigNotice::withdraw`]).
    about: RefCell<Option<String>>,
    /// Where the window's keyboard focus was before it moved into the strip,
    /// followed while the strip is on screen.
    focus_before: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    focus_watch: RefCell<Option<(glib::WeakRef<gtk::Window>, glib::SignalHandlerId)>>,
}

impl State {
    fn follow_focus(&self, window: &gtk::Window, banner: &adw::Banner) {
        if let Some(focus) = GtkWindowExt::focus(window)
            && !focus.is_ancestor(banner)
        {
            self.focus_before.replace(Some(focus.downgrade()));
        }
    }

    /// Put the keyboard back where it was before it moved into the strip, when
    /// the strip is about to slide away with it: left alone, GTK hands it to
    /// the first control it finds near the strip, often the window's first.
    /// Returns where the keyboard went.
    fn return_focus(&self, banner: &adw::Banner) -> Option<gtk::Widget> {
        let window = banner.root().and_downcast::<gtk::Window>()?;
        if !big_gtk_kit::focus::holds_focus(banner) {
            return None;
        }
        let before = self
            .focus_before
            .borrow()
            .as_ref()
            .and_then(glib::WeakRef::upgrade);
        if let Some(before) = before
            && before.is_ancestor(&window)
            && before.is_mapped()
            && before.grab_focus()
        {
            return Some(before);
        }
        window.child_focus(gtk::DirectionType::TabForward);
        GtkWindowExt::focus(&window)
    }
}

/// One window's notice strip. Clones speak through the same strip.
///
/// Place [`Self::widget`] in the window's layout (a workspace's
/// `BigWorkspaceSpec::notice`, or the top of the content), never over it.
///
/// A strip that goes while it holds the keyboard hands it back to the control
/// that had it before, or to the content after the strip once that control is
/// gone. One stepping down while the keyboard is elsewhere leaves it there.
#[derive(Clone)]
pub struct BigNotice {
    banner: adw::Banner,
    state: Rc<State>,
}

/// A [`BigNotice`] that does not keep its window alive.
#[derive(Clone)]
pub struct BigWeakNotice {
    banner: glib::WeakRef<adw::Banner>,
    state: Rc<State>,
}

impl BigWeakNotice {
    /// The strip, while its window is alive.
    #[must_use]
    pub fn upgrade(&self) -> Option<BigNotice> {
        Some(BigNotice {
            banner: self.banner.upgrade()?,
            state: Rc::clone(&self.state),
        })
    }
}

impl BigNotice {
    /// A hidden strip. `close_label` is the app's translation of "Close".
    #[must_use]
    pub fn new(close_label: &str) -> Self {
        install_notice_css();
        let banner = adw::Banner::new("");
        banner.add_css_class(NOTICE_CSS_CLASS);
        // File names and system text are never markup.
        banner.set_use_markup(false);
        banner.set_revealed(false);
        // Hidden once its revealer has slid shut, so a screen reader finds no
        // empty group still holding the last words said.
        banner.set_visible(false);
        if let Some(revealer) = banner.first_child().and_downcast::<gtk::Revealer>() {
            let weak = banner.downgrade();
            revealer.connect_child_revealed_notify(move |revealer| {
                if let Some(banner) = weak.upgrade()
                    && !revealer.is_child_revealed()
                    && !banner.is_revealed()
                {
                    banner.set_visible(false);
                }
            });
        }
        let state = Rc::new(State {
            close_label: close_label.to_owned(),
            step_down: OnceTimer::new(),
            offer: RefCell::default(),
            about: RefCell::default(),
            focus_before: RefCell::default(),
            focus_watch: RefCell::default(),
        });
        let watching = Rc::clone(&state);
        banner.connect_map(move |banner| {
            let Some(window) = banner.root().and_downcast::<gtk::Window>() else {
                return;
            };
            watching.follow_focus(&window, banner);
            let weak = banner.downgrade();
            let state = Rc::clone(&watching);
            let handler = window.connect_focus_widget_notify(move |window| {
                if let Some(banner) = weak.upgrade() {
                    state.follow_focus(window, &banner);
                }
            });
            watching
                .focus_watch
                .replace(Some((window.downgrade(), handler)));
        });
        let watching = Rc::clone(&state);
        banner.connect_unmap(move |_| {
            if let Some((window, handler)) = watching.focus_watch.take()
                && let Some(window) = window.upgrade()
            {
                window.disconnect(handler);
            }
        });
        let answered = Rc::clone(&state);
        banner.connect_button_clicked(move |banner| {
            answered.step_down.cancel();
            let offer = answered.offer.take();
            answered.about.take();
            let returned = answered.return_focus(banner);
            banner.set_revealed(false);
            if let Some(offer) = offer {
                offer(banner);
            }
            // An answer that says something new keeps the keyboard on it,
            // unless the offer took the keyboard elsewhere.
            if banner.is_revealed() && returned.is_some_and(|returned| returned.is_focus()) {
                banner.child_focus(gtk::DirectionType::TabForward);
            }
        });
        Self { banner, state }
    }

    /// The strip, to place in the window's layout.
    #[must_use]
    pub fn widget(&self) -> &adw::Banner {
        &self.banner
    }

    /// A handle for closures the window owns.
    #[must_use]
    pub fn downgrade(&self) -> BigWeakNotice {
        BigWeakNotice {
            banner: self.banner.downgrade(),
            state: Rc::clone(&self.state),
        }
    }

    /// Whether the strip shows a button that does something besides closing.
    #[must_use]
    pub fn is_offering(&self) -> bool {
        self.banner.is_revealed() && self.state.offer.borrow().is_some()
    }

    /// Say what went well; it steps down after [`NOTICE_LINGER`], later while
    /// the pointer or the keyboard is on it.
    pub fn confirm(&self, message: &str) {
        self.show(message, None, Tone::Passing);
    }

    /// [`Self::confirm`], offering `label` to do `offer` while it shows:
    /// "Show in Folder".
    pub fn confirm_offering(
        &self,
        message: &str,
        label: &str,
        offer: impl Fn(&adw::Banner) + 'static,
    ) {
        self.show(message, Some((label, Box::new(offer))), Tone::Passing);
    }

    /// Say what was done, offering `label` to take it back. It stays until it
    /// is used or something newer is said: a way back that vanishes on a timer
    /// is one somebody discovers they have lost.
    pub fn offer_undo(&self, message: &str, label: &str, undo: impl FnOnce() + 'static) {
        let undo = std::cell::Cell::new(Some(undo));
        let offer = move |_: &adw::Banner| {
            if let Some(undo) = undo.take() {
                undo();
            }
        };
        self.show(message, Some((label, Box::new(offer))), Tone::Lasting);
    }

    /// Say how things stand: what the person is asked to do, or why something
    /// is not shown. It stays, with Close, until it is closed or something
    /// newer is said.
    pub fn tell(&self, message: &str) {
        self.show(message, None, Tone::Lasting);
    }

    /// Say what was refused or went wrong. It stays, in the warning color and
    /// with Close, until it is closed or something newer is said.
    pub fn warn(&self, message: &str) {
        self.show(message, None, Tone::Warning);
    }

    /// [`Self::warn`] about `subject`, an action on one item ("rename
    /// /srv/notes.txt"): [`Self::withdraw`] with the same subject takes it down
    /// once that action has gone well.
    pub fn warn_about(&self, subject: &str, message: &str) {
        self.show(message, None, Tone::Warning);
        self.state.about.replace(Some(subject.to_owned()));
    }

    /// Say what waits on the person, about `subject`, offering `label` to deal
    /// with it: "Reopen". It stays until it is used, taken down by
    /// [`Self::withdraw`] or replaced by something newer.
    pub fn offer_about(
        &self,
        subject: &str,
        message: &str,
        label: &str,
        offer: impl Fn(&adw::Banner) + 'static,
    ) {
        self.show(message, Some((label, Box::new(offer))), Tone::Lasting);
        self.state.about.replace(Some(subject.to_owned()));
    }

    /// Whether the strip still says what it was told about `subject`.
    #[must_use]
    pub fn is_about(&self, subject: &str) -> bool {
        self.banner.is_revealed() && self.state.about.borrow().as_deref() == Some(subject)
    }

    /// Take down what the strip says about `subject` when it still says it:
    /// the action a warning refused has since gone well, or what an offer
    /// waited on is gone. Anything else on the strip stays.
    pub fn withdraw(&self, subject: &str) {
        if self.is_about(subject) {
            self.clear();
        }
    }

    /// [`Self::warn`], offering `label` to do `offer` instead of Close:
    /// "Try Again".
    pub fn warn_offering(
        &self,
        message: &str,
        label: &str,
        offer: impl Fn(&adw::Banner) + 'static,
    ) {
        self.show(message, Some((label, Box::new(offer))), Tone::Warning);
    }

    /// Say how far some work has got, offering `label` to stop it. Spoken once,
    /// when it starts: every step aloud would drown the screen reader.
    pub fn working(&self, message: &str, label: &str, offer: impl Fn(&adw::Banner) + 'static) {
        let starting = !self.banner.is_revealed() || self.state.step_down.is_pending();
        self.stop_step_down();
        self.state.about.take();
        self.state.offer.replace(Some(Box::new(offer)));
        self.banner.remove_css_class(NOTICE_WAITS_CSS_CLASS);
        self.banner.set_title(message);
        self.banner.set_button_label(Some(label));
        self.banner.set_visible(true);
        self.banner.set_revealed(true);
        if starting {
            self.speak(message, gtk::AccessibleAnnouncementPriority::Medium);
        }
    }

    /// Take down whatever the strip says.
    pub fn clear(&self) {
        self.stop_step_down();
        self.state.offer.take();
        self.state.about.take();
        self.state.return_focus(&self.banner);
        self.banner.set_revealed(false);
    }

    fn show(&self, message: &str, offer: Option<(&str, Offer)>, tone: Tone) {
        self.stop_step_down();
        self.state.about.take();
        let label = match offer {
            Some((label, offer)) => {
                self.state.offer.replace(Some(offer));
                Some(label.to_owned())
            }
            None => {
                self.state.offer.take();
                (tone != Tone::Passing).then(|| self.state.close_label.clone())
            }
        };
        if tone == Tone::Warning {
            self.banner.add_css_class(NOTICE_WAITS_CSS_CLASS);
        } else {
            self.banner.remove_css_class(NOTICE_WAITS_CSS_CLASS);
        }
        self.banner.set_title(message);
        self.banner.set_button_label(label.as_deref());
        self.banner.set_visible(true);
        self.banner.set_revealed(true);
        // A strip sliding in makes no sound, so a screen reader is told.
        let priority = if tone == Tone::Warning {
            gtk::AccessibleAnnouncementPriority::High
        } else {
            gtk::AccessibleAnnouncementPriority::Medium
        };
        self.speak(message, priority);
        if tone == Tone::Passing {
            self.step_down_later();
        }
    }

    /// Tell a screen reader `message`, from the strip's window: a strip
    /// hidden between messages is a widget no screen reader has looked at.
    fn speak(&self, message: &str, priority: gtk::AccessibleAnnouncementPriority) {
        announce_from_window(&self.banner, message, priority);
    }

    fn stop_step_down(&self) {
        self.state.step_down.cancel();
    }

    fn step_down_later(&self) {
        let weak = self.downgrade();
        self.state.step_down.arm(NOTICE_LINGER, move || {
            let Some(notice) = weak.upgrade() else {
                return;
            };
            let hovered = notice
                .banner
                .state_flags()
                .contains(gtk::StateFlags::PRELIGHT);
            if hovered || big_gtk_kit::focus::holds_focus(&notice.banner) {
                notice.step_down_later();
            } else {
                notice.clear();
            }
        });
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    /// Steps down on its own.
    Passing,
    /// Stays until answered or replaced.
    Lasting,
    /// Stays, in the warning color.
    Warning,
}

/// The warning color through libadwaita's own `--banner-color`, so the strip
/// keeps its shape and its backdrop shade. Once per process and for the whole
/// display: a dialog or an embedded pane can hold a strip too.
fn install_notice_css() {
    // A hairline at its foot keeps the strip from reading as part of the
    // content under it. Inset, because the revealer clips anything outside.
    crate::css::install(
        &format!(
            "banner.{NOTICE_WAITS_CSS_CLASS} {{ --banner-color: var(--warning-bg-color); }}
             banner.{NOTICE_CSS_CLASS} > revealer > widget {{ box-shadow: inset 0 -1px var(--border-color); }}"
        ),
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use big_testkit::harness::{pump, pump_until};

    fn shown_notice() -> (BigNotice, gtk::Window) {
        let notice = BigNotice::new("Close");
        let window = gtk::Window::new();
        window.set_child(Some(notice.widget()));
        window.present();
        (notice, window)
    }

    /// Pump the main loop for `span`.
    fn wait(span: Duration) {
        let until = std::time::Instant::now() + span;
        while std::time::Instant::now() < until {
            pump();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn equal_confirmations_each_linger() {
        big_testkit::harness::require_gtk_test(|| {
            let (notice, window) = shown_notice();
            notice.confirm("Saved");
            wait(NOTICE_LINGER / 2);
            notice.confirm("Saved");
            wait(NOTICE_LINGER * 3 / 4);
            assert!(
                notice.widget().is_revealed(),
                "the second one has its own time"
            );
            wait(NOTICE_LINGER / 2);
            assert!(!notice.widget().is_revealed(), "and then steps down");
            window.destroy();
        });
    }

    /// A confirmation under the pointer or holding the keyboard stays until
    /// it is left.
    #[test]
    fn read_confirmation_waits() {
        big_testkit::harness::require_gtk_test(|| {
            let (notice, window) = shown_notice();
            let banner = notice.widget();
            notice.confirm("Saved");
            banner.set_state_flags(gtk::StateFlags::PRELIGHT, false);
            wait(NOTICE_LINGER + NOTICE_LINGER / 4);
            assert!(banner.is_revealed(), "kept under the pointer");
            banner.unset_state_flags(gtk::StateFlags::PRELIGHT);
            wait(NOTICE_LINGER + NOTICE_LINGER / 4);
            assert!(!banner.is_revealed(), "gone once the pointer left");

            notice.confirm_offering("Saved", "Show", |_| {});
            // Its button takes the focus once it has slid in.
            wait(Duration::from_secs(1));
            assert!(banner.child_focus(gtk::DirectionType::TabForward));
            // Another window taking over leaves the focus where it was, so the
            // strip still waits for the person to come back to it.
            let other = gtk::Window::new();
            other.present();
            assert!(pump_until(|| other.is_active() && !window.is_active()));
            wait(NOTICE_LINGER + NOTICE_LINGER / 4);
            assert!(banner.is_revealed(), "kept while its button has focus");
            other.destroy();
            gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
            wait(NOTICE_LINGER + NOTICE_LINGER / 4);
            assert!(!banner.is_revealed(), "gone once the focus left");
            window.destroy();
        });
    }

    #[test]
    fn warnings_and_undo_stay() {
        big_testkit::harness::require_gtk_test(|| {
            let (notice, window) = shown_notice();
            notice.confirm("Saved");
            notice.warn("Not written");
            assert_eq!(notice.widget().button_label().as_deref(), Some("Close"));
            wait(NOTICE_LINGER + NOTICE_LINGER / 4);
            assert!(notice.widget().is_revealed(), "a problem waits for Close");
            notice.widget().emit_by_name::<()>("button-clicked", &[]);
            assert!(!notice.widget().is_revealed());

            let undone = Rc::new(std::cell::Cell::new(0));
            let counted = Rc::clone(&undone);
            notice.offer_undo("Tab closed", "Undo", move || counted.set(counted.get() + 1));
            wait(NOTICE_LINGER + NOTICE_LINGER / 4);
            assert!(
                notice.widget().is_revealed(),
                "a way back does not time out"
            );
            notice.widget().emit_by_name::<()>("button-clicked", &[]);
            notice.widget().emit_by_name::<()>("button-clicked", &[]);
            assert_eq!(undone.get(), 1);
            window.destroy();
        });
    }

    /// A later success takes down the refusal of that action on that item, and
    /// nothing else the strip says.
    #[test]
    fn success_withdraws_its_refusal() {
        big_testkit::harness::require_gtk_test(|| {
            let (notice, window) = shown_notice();
            notice.warn_about("rename /a", "a changed");
            notice.withdraw("delete /a");
            notice.withdraw("rename /b");
            assert!(notice.widget().is_revealed(), "another action or item");
            notice.withdraw("rename /a");
            assert!(!notice.widget().is_revealed());

            notice.warn_about("rename /a", "a changed");
            notice.warn("b could not be read");
            notice.withdraw("rename /a");
            assert_eq!(
                notice.widget().title(),
                "b could not be read",
                "a newer warning is not the one withdrawn"
            );
            notice.widget().emit_by_name::<()>("button-clicked", &[]);
            notice.warn_about("rename /a", "a changed");
            notice.widget().emit_by_name::<()>("button-clicked", &[]);
            notice.offer_undo("Renamed b", "Undo", || {});
            notice.withdraw("rename /a");
            assert!(notice.widget().is_revealed(), "closed, then replaced");
            window.destroy();
        });
    }

    /// Answering the strip from the keyboard leaves the focus where it was
    /// before it moved in, or on the content after the strip once that is
    /// gone; an answer that brings a new message keeps it on the strip. A
    /// strip stepping down on its own never moves it.
    #[test]
    #[ignore = "display: Tab moves from the focus only in the active window"]
    fn answered_strip_returns_focus() {
        big_testkit::harness::require_gtk_test(|| {
            let notice = BigNotice::new("Close");
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            // Where GTK puts the focus of a strip sliding away with it.
            let top = gtk::Button::with_label("Top");
            let before = gtk::Button::with_label("Before");
            let content = gtk::Button::with_label("Content");
            column.append(&top);
            column.append(&before);
            column.append(notice.widget());
            column.append(&content);
            let window = gtk::Window::new();
            window.set_child(Some(&column));
            window.present();
            assert!(pump_until(|| before.is_mapped() && window.is_active()));
            let banner = notice.widget();
            let revealer = banner
                .first_child()
                .and_downcast::<gtk::Revealer>()
                .expect("the strip's revealer");
            let focus = || GtkWindowExt::focus(&window);
            // Tab from where the focus is into the strip once it has slid in.
            let enter_strip = || {
                assert!(pump_until(|| revealer.is_child_revealed()));
                assert!(window.child_focus(gtk::DirectionType::TabForward));
                let button = focus().expect("the strip's button");
                assert!(button.is_ancestor(banner), "Tab reaches the strip");
                button
            };
            let answers = Rc::new(std::cell::Cell::new(0));
            banner.connect_button_clicked({
                let answers = Rc::clone(&answers);
                move |_| answers.set(answers.get() + 1)
            });
            // Activate `button` as Return does, which clicks it a moment later,
            // and wait until the strip has slid away or says something new.
            let press = |button: gtk::Widget| {
                let answered = answers.get() + 1;
                button.activate();
                assert!(pump_until(|| answers.get() == answered));
                assert!(pump_until(|| !banner.is_visible() || banner.is_revealed()));
            };

            before.grab_focus();
            notice.offer_undo("Moved to the Trash", "Undo", || {});
            press(enter_strip());
            assert_eq!(focus(), Some(before.clone().upcast()));

            notice.warn("Not written");
            press(enter_strip());
            assert_eq!(focus(), Some(before.clone().upcast()), "Close too");

            let weak = notice.downgrade();
            notice.offer_undo("Moved to the Trash", "Undo", move || {
                if let Some(notice) = weak.upgrade() {
                    notice.warn("Could not be put back");
                }
            });
            press(enter_strip());
            let close = focus().expect("the warning's button");
            assert!(close.is_ancestor(banner), "kept on what the answer said");
            press(close);
            assert_eq!(focus(), Some(before.clone().upcast()));

            notice.warn("Not written");
            let close = enter_strip();
            column.remove(&before);
            press(close);
            assert_eq!(focus(), Some(content.upcast()), "that place gone");

            GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
            notice.confirm("Saved");
            wait(NOTICE_LINGER + NOTICE_LINGER / 4);
            assert!(!banner.is_revealed());
            assert_eq!(focus(), None, "a step-down leaves the focus alone");
            window.destroy();
        });
    }

    /// A closed strip keeps no group with its old words in the accessible tree.
    #[test]
    fn closed_strip_leaves_the_tree() {
        big_testkit::harness::require_gtk_test(|| {
            let (notice, window) = shown_notice();
            assert!(!notice.widget().is_visible(), "nothing said yet");
            notice.offer_undo("Moved to the Trash", "Undo", || {});
            assert!(notice.widget().is_visible());
            notice.widget().emit_by_name::<()>("button-clicked", &[]);
            wait(Duration::from_secs(1));
            assert!(!notice.widget().is_visible(), "gone once it slid away");
            window.destroy();
        });
    }
}
