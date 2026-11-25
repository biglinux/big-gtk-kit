// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Idle reveal and focus guard policy for immersive media chrome.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use relm4::gtk;

use big_gtk_kit::timer::OnceTimer;

const DEFAULT_IMMERSION_IDLE_TIMEOUT_MS: u64 = 2_000;

/// On registered chrome while it is idle. The chrome stays where it is, in the
/// focus chain and the accessibility tree, and stops taking the pointer; the
/// surface's stylesheet makes it transparent.
pub const BIG_MEDIA_CHROME_IDLE_CSS_CLASS: &str = "big-media-chrome-idle";
const MIN_IMMERSION_IDLE_TIMEOUT_MS: u64 = 250;

/// Shared idle-reveal controller for media chrome.
///
/// The controller observes pointer and keyboard activity on a media root, keeps
/// registered revealers visible while the user is active,
/// and hides them after a short idle timeout unless typed playback policy,
/// focused chrome, an open popover or a pointer resting on the chrome requires
/// the controls to remain visible.
pub struct BigMediaImmersionController {
    /// Each revealer, with whether it takes the pointer while shown.
    reveal_targets: RefCell<Vec<(gtk::glib::WeakRef<gtk::Revealer>, bool)>>,
    focus_hide_guards: RefCell<Vec<gtk::glib::WeakRef<gtk::Widget>>>,
    popover_hide_guards: RefCell<Vec<gtk::glib::WeakRef<gtk::Popover>>>,
    hover_hide_guards: RefCell<Vec<gtk::glib::WeakRef<gtk::EventControllerMotion>>>,
    cursor_host: gtk::glib::WeakRef<gtk::Widget>,
    idle_timeout_ms: Cell<u64>,
    is_policy_pinned: Cell<bool>,
    should_hide_cursor_when_idle: Cell<bool>,
    idle_timer: OnceTimer,
    self_weak: RefCell<Weak<Self>>,
}

impl BigMediaImmersionController {
    /// Create a controller whose cursor is restored on `cursor_host` activity.
    #[must_use]
    pub fn new(cursor_host: &impl IsA<gtk::Widget>) -> Rc<Self> {
        let controller = Rc::new(Self {
            reveal_targets: RefCell::new(Vec::new()),
            focus_hide_guards: RefCell::new(Vec::new()),
            popover_hide_guards: RefCell::new(Vec::new()),
            hover_hide_guards: RefCell::new(Vec::new()),
            cursor_host: cursor_host.upcast_ref::<gtk::Widget>().downgrade(),
            idle_timeout_ms: Cell::new(DEFAULT_IMMERSION_IDLE_TIMEOUT_MS),
            is_policy_pinned: Cell::new(false),
            should_hide_cursor_when_idle: Cell::new(false),
            idle_timer: OnceTimer::new(),
            self_weak: RefCell::new(Weak::new()),
        });
        *controller.self_weak.borrow_mut() = Rc::downgrade(&controller);
        controller
    }

    /// Override the default idle timeout used before hiding registered chrome.
    pub fn set_idle_timeout(&self, idle_timeout: std::time::Duration) {
        self.idle_timeout_ms
            .set(bounded_immersion_idle_timeout_ms(idle_timeout));
    }

    /// Hide the cursor on the controller host when media chrome becomes idle.
    pub fn set_hide_cursor_when_idle(&self, should_hide: bool) {
        self.should_hide_cursor_when_idle.set(should_hide);
    }

    /// Reset the idle timer on pointer or keyboard activity over `root`.
    pub fn attach_activity_detection(self: &Rc<Self>, root: &impl IsA<gtk::Widget>) {
        let motion = gtk::EventControllerMotion::new();
        let controller = self.clone();
        // Only a pointer that moved: GTK repeats the last position whenever
        // what lies under the pointer changes, and the chrome hiding is such a
        // change, so every hide would bring itself straight back.
        let last = std::cell::Cell::new(None::<(f64, f64)>);
        motion.connect_motion(move |_, x, y| {
            if last.replace(Some((x, y))) != Some((x, y)) {
                controller.show_and_schedule_hide();
            }
        });
        root.add_controller(motion);

        let key = gtk::EventControllerKey::new();
        let controller = self.clone();
        key.connect_key_pressed(move |_, _, _, _| {
            controller.show_and_schedule_hide();
            gtk::glib::Propagation::Proceed
        });
        root.add_controller(key);
        // Focus moving is activity too, and it is the one that frees chrome
        // held open by focus: a window that opens with the keyboard on a
        // header button and then hands it to the picture would otherwise keep
        // its chrome until the pointer happened to move again.
        let controller = Rc::downgrade(self);
        root.connect_realize(move |root| {
            let Some(window) = root.root().and_downcast::<gtk::Window>() else {
                return;
            };
            let controller = controller.clone();
            window.connect_focus_widget_notify(move |_| {
                if let Some(controller) = controller.upgrade() {
                    controller.show_and_schedule_hide();
                }
            });
        });
        self.show_and_schedule_hide();
    }

    /// Hide `revealer` when the media surface becomes idle, by
    /// [`BIG_MEDIA_CHROME_IDLE_CSS_CLASS`].
    pub fn register_revealer(&self, revealer: &gtk::Revealer) {
        self.reveal_targets
            .borrow_mut()
            .push((revealer.downgrade(), revealer.can_target()));
        self.register_focus_hide_guard(revealer);
        // A pointer resting on the chrome is about to use it: hidden under it,
        // the button it was aimed at would take the click no more.
        let hover = gtk::EventControllerMotion::new();
        let controller = self.self_weak.borrow().clone();
        hover.connect_leave(move |_| {
            // GTK clears `contains-pointer` only after `leave` returns.
            let controller = controller.clone();
            gtk::glib::idle_add_local_once(move || {
                if let Some(controller) = controller.upgrade() {
                    controller.show_and_schedule_hide();
                }
            });
        });
        revealer.add_controller(hover.clone());
        self.hover_hide_guards.borrow_mut().push(hover.downgrade());
    }

    /// Keep chrome visible while `widget` or one of its descendants has focus.
    pub(crate) fn register_focus_hide_guard(&self, widget: &impl IsA<gtk::Widget>) {
        self.focus_hide_guards
            .borrow_mut()
            .push(widget.upcast_ref::<gtk::Widget>().downgrade());
    }

    /// Keep chrome visible while `popover` is open.
    pub fn register_popover_hide_guard(&self, popover: &gtk::Popover) {
        self.popover_hide_guards
            .borrow_mut()
            .push(popover.downgrade());
    }

    /// Keep the chrome and the cursor shown while `pinned`, as for a view with
    /// nothing to look at instead; unpinned, they hide again when idle.
    pub fn set_pinned(&self, pinned: bool) {
        if self.is_policy_pinned.replace(pinned) == pinned {
            return;
        }
        self.show_and_schedule_hide();
    }

    /// Reveal registered chrome and arm the idle hide timer when guards allow it.
    pub fn show_and_schedule_hide(&self) {
        self.idle_timer.cancel();
        self.show_all();
        if self.should_keep_chrome_visible() {
            return;
        }
        let controller = self.self_weak.borrow().clone();
        let idle_timeout = std::time::Duration::from_millis(self.idle_timeout_ms.get());
        self.idle_timer.arm(idle_timeout, move || {
            if let Some(controller) = controller.upgrade() {
                controller.hide_all();
            }
        });
    }

    fn guard_snapshot(&self) -> BigMediaImmersionGuardSnapshot {
        let active_focus_guard_count = {
            let mut guards = self.focus_hide_guards.borrow_mut();
            guards.retain(|guard| guard.upgrade().is_some());
            guards
                .iter()
                .filter_map(gtk::glib::WeakRef::upgrade)
                .filter(media_chrome_subtree_has_focus)
                .count()
        };
        let active_popover_guard_count = {
            let mut guards = self.popover_hide_guards.borrow_mut();
            guards.retain(|guard| guard.upgrade().is_some());
            guards
                .iter()
                .filter_map(gtk::glib::WeakRef::upgrade)
                .filter(|popover| popover.is_visible())
                .count()
        };
        let is_pointer_on_chrome = {
            let mut guards = self.hover_hide_guards.borrow_mut();
            guards.retain(|guard| guard.upgrade().is_some());
            guards
                .iter()
                .filter_map(gtk::glib::WeakRef::upgrade)
                .any(|hover| hover.contains_pointer())
        };
        BigMediaImmersionGuardSnapshot {
            is_policy_pinned: self.is_policy_pinned.get(),
            active_focus_guard_count,
            active_popover_guard_count,
            is_pointer_on_chrome,
        }
    }

    fn should_keep_chrome_visible(&self) -> bool {
        self.guard_snapshot().should_keep_chrome_visible()
    }

    fn show_all(&self) {
        for (revealer, targets) in self.reveal_targets.borrow().iter() {
            if let Some(revealer) = revealer.upgrade() {
                revealer.set_reveal_child(true);
                revealer.remove_css_class(BIG_MEDIA_CHROME_IDLE_CSS_CLASS);
                revealer.set_can_target(*targets);
            }
        }
        if self.should_hide_cursor_when_idle.get()
            && let Some(host) = self.cursor_host.upgrade()
        {
            host.set_cursor_from_name(Some("default"));
        }
    }

    fn hide_all(&self) {
        if self.should_keep_chrome_visible() {
            return;
        }
        // Kept revealed: a hidden revealer's controls leave the focus chain
        // and the accessibility tree, and the keyboard could not reach them.
        for (revealer, _) in self.reveal_targets.borrow().iter() {
            if let Some(revealer) = revealer.upgrade() {
                revealer.add_css_class(BIG_MEDIA_CHROME_IDLE_CSS_CLASS);
                revealer.set_can_target(false);
            }
        }
        if self.should_hide_cursor_when_idle.get()
            && let Some(host) = self.cursor_host.upgrade()
        {
            host.set_cursor_from_name(Some("none"));
        }
    }
}

impl Drop for BigMediaImmersionController {
    fn drop(&mut self) {
        self.idle_timer.cancel();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BigMediaImmersionGuardSnapshot {
    is_policy_pinned: bool,
    active_focus_guard_count: usize,
    active_popover_guard_count: usize,
    is_pointer_on_chrome: bool,
}

impl BigMediaImmersionGuardSnapshot {
    const fn should_keep_chrome_visible(self) -> bool {
        self.is_policy_pinned
            || self.active_focus_guard_count > 0
            || self.active_popover_guard_count > 0
            || self.is_pointer_on_chrome
    }
}

fn bounded_immersion_idle_timeout_ms(idle_timeout: std::time::Duration) -> u64 {
    let timeout_ms = idle_timeout.as_millis().try_into().unwrap_or(u64::MAX);
    timeout_ms.max(MIN_IMMERSION_IDLE_TIMEOUT_MS)
}

/// Return whether `chrome_widget` or one of its descendants has keyboard focus.
fn media_chrome_subtree_has_focus(chrome_widget: &impl IsA<gtk::Widget>) -> bool {
    let chrome_widget = chrome_widget.upcast_ref::<gtk::Widget>();
    if chrome_widget.has_focus() {
        return true;
    }

    let Some(window) = chrome_widget.root().and_downcast::<gtk::Window>() else {
        return false;
    };
    let Some(focused_widget) = gtk::prelude::GtkWindowExt::focus(&window) else {
        return false;
    };

    widget_contains_descendant(chrome_widget, &focused_widget)
}

fn widget_contains_descendant(widget: &gtk::Widget, descendant: &gtk::Widget) -> bool {
    if descendant == widget {
        return true;
    }

    let mut parent = descendant.parent();
    while let Some(current_parent) = parent {
        if &current_parent == widget {
            return true;
        }
        parent = current_parent.parent();
    }

    false
}

#[cfg(test)]
mod tests {
    use super::{
        BigMediaImmersionGuardSnapshot, MIN_IMMERSION_IDLE_TIMEOUT_MS,
        bounded_immersion_idle_timeout_ms,
    };

    #[test]
    fn guard_snapshot_pins_chrome() {
        assert!(
            BigMediaImmersionGuardSnapshot {
                is_policy_pinned: true,
                active_focus_guard_count: 0,
                active_popover_guard_count: 0,
                is_pointer_on_chrome: false,
            }
            .should_keep_chrome_visible()
        );
        assert!(
            BigMediaImmersionGuardSnapshot {
                is_policy_pinned: false,
                active_focus_guard_count: 1,
                active_popover_guard_count: 0,
                is_pointer_on_chrome: false,
            }
            .should_keep_chrome_visible()
        );
        assert!(
            BigMediaImmersionGuardSnapshot {
                is_policy_pinned: false,
                active_focus_guard_count: 0,
                active_popover_guard_count: 1,
                is_pointer_on_chrome: false,
            }
            .should_keep_chrome_visible()
        );
        assert!(
            BigMediaImmersionGuardSnapshot {
                is_policy_pinned: false,
                active_focus_guard_count: 0,
                active_popover_guard_count: 0,
                is_pointer_on_chrome: true,
            }
            .should_keep_chrome_visible()
        );
        assert!(
            !BigMediaImmersionGuardSnapshot {
                is_policy_pinned: false,
                active_focus_guard_count: 0,
                active_popover_guard_count: 0,
                is_pointer_on_chrome: false,
            }
            .should_keep_chrome_visible()
        );
    }

    #[test]
    fn pinned_chrome_stays_shown() {
        use relm4::gtk;
        use relm4::gtk::prelude::*;
        big_testkit::harness::require_gtk_test(|| {
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let revealer = gtk::Revealer::new();
            host.append(&revealer);
            let controller = super::BigMediaImmersionController::new(&host);
            controller.set_idle_timeout(std::time::Duration::ZERO);
            controller.register_revealer(&revealer);
            let idle = || {
                let until = std::time::Instant::now()
                    + std::time::Duration::from_millis(2 * MIN_IMMERSION_IDLE_TIMEOUT_MS);
                while std::time::Instant::now() < until {
                    big_testkit::harness::pump();
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            };

            let idle_class = super::BIG_MEDIA_CHROME_IDLE_CSS_CLASS;
            controller.set_pinned(true);
            idle();
            assert!(
                !revealer.has_css_class(idle_class),
                "pinned chrome does not hide"
            );
            controller.set_pinned(false);
            idle();
            assert!(
                revealer.has_css_class(idle_class),
                "unpinned chrome hides when idle"
            );
        });
    }

    #[test]
    fn idle_chrome_stays_reachable() {
        use relm4::gtk;
        use relm4::gtk::prelude::*;
        big_testkit::harness::require_gtk_test(|| {
            let window = gtk::Window::new();
            let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let revealer = gtk::Revealer::builder().reveal_child(true).build();
            let next = gtk::Button::with_label("Next");
            revealer.set_child(Some(&next));
            let picture = gtk::Button::with_label("Picture");
            host.append(&picture);
            host.append(&revealer);
            window.set_child(Some(&host));
            window.present();
            let controller = super::BigMediaImmersionController::new(&host);
            controller.register_revealer(&revealer);

            assert!(picture.grab_focus());
            controller.hide_all();
            assert!(next.is_mapped(), "a screen reader still finds the control");
            assert!(!revealer.can_target(), "an unseen control takes no click");
            assert!(next.grab_focus());
            controller.show_and_schedule_hide();
            assert!(
                revealer.can_target()
                    && !revealer.has_css_class(super::BIG_MEDIA_CHROME_IDLE_CSS_CLASS)
            );
            window.destroy();
        });
    }

    #[test]
    fn immersion_idle_timeout_never_flickers_immediately() {
        assert_eq!(
            bounded_immersion_idle_timeout_ms(std::time::Duration::ZERO),
            MIN_IMMERSION_IDLE_TIMEOUT_MS
        );
        assert_eq!(
            bounded_immersion_idle_timeout_ms(std::time::Duration::from_millis(1)),
            MIN_IMMERSION_IDLE_TIMEOUT_MS
        );
        assert_eq!(
            bounded_immersion_idle_timeout_ms(std::time::Duration::from_millis(1_500)),
            1_500
        );
    }
}
