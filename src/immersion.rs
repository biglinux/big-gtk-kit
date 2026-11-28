// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Bars over a picture or a video that fade out while the person only watches.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use gtk::prelude::*;

use crate::timer::OnceTimer;

/// On registered bars while they are idle: transparent, and out of the
/// pointer's way, but still in the focus chain and the accessibility tree.
pub const BIG_MEDIA_CHROME_IDLE_CSS_CLASS: &str = "big-media-chrome-idle";

const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(2);
/// A shorter delay hides the bars before the eye finds them.
const MIN_IDLE_TIMEOUT: Duration = Duration::from_millis(250);
/// A redrawn picture repeats `motion` where the pointer rests, within this.
const MOTION_EPSILON_PX: f64 = 2.0;

/// Fully transparent, an idle bar makes no render node, so a video under it
/// can still go to a hardware plane.
const CSS: &str = "
revealer.big-media-chrome { transition: opacity 200ms ease-out; }
revealer.big-media-chrome.big-media-chrome-idle { opacity: 0; }
";

/// Shows the bars on activity over a media surface and hides them once the
/// person has been idle for a while, unless a guard or an open popover pins
/// them. Neither the pointer resting on a bar nor the keyboard focus in one
/// keeps it: both stay usable, and the next key or motion shows it again.
pub struct BigMediaImmersionController {
    /// Each bar, with whether it takes the pointer while shown.
    bars: RefCell<Vec<(gtk::glib::WeakRef<gtk::Revealer>, bool)>>,
    popover_guards: RefCell<Vec<gtk::glib::WeakRef<gtk::Popover>>>,
    hide_guards: RefCell<Vec<Box<dyn Fn() -> bool>>>,
    cursor_host: gtk::glib::WeakRef<gtk::Widget>,
    idle_timeout: Cell<Duration>,
    pinned: Cell<bool>,
    hides_cursor: Cell<bool>,
    last_pointer: Cell<(f64, f64)>,
    idle_timer: OnceTimer,
    this: Weak<Self>,
}

impl BigMediaImmersionController {
    /// A controller whose idle cursor hides over `cursor_host`.
    #[must_use]
    pub fn new(cursor_host: &impl IsA<gtk::Widget>) -> Rc<Self> {
        Rc::new_cyclic(|this| Self {
            bars: RefCell::default(),
            popover_guards: RefCell::default(),
            hide_guards: RefCell::default(),
            cursor_host: cursor_host.upcast_ref::<gtk::Widget>().downgrade(),
            idle_timeout: Cell::new(DEFAULT_IDLE_TIMEOUT),
            pinned: Cell::new(false),
            hides_cursor: Cell::new(false),
            last_pointer: Cell::new((f64::NAN, f64::NAN)),
            idle_timer: OnceTimer::new(),
            this: this.clone(),
        })
    }

    /// Wait `idle_timeout`, at least a quarter of a second, before hiding.
    pub fn set_idle_timeout(&self, idle_timeout: Duration) {
        self.idle_timeout.set(idle_timeout.max(MIN_IDLE_TIMEOUT));
    }

    /// Hide the cursor over the host while the bars are hidden.
    pub fn set_hide_cursor_when_idle(&self, hides: bool) {
        self.hides_cursor.set(hides);
    }

    /// Show the bars on pointer motion, the pointer entering or leaving, a
    /// key press or the focus moving over `root`.
    pub fn attach_activity_detection(&self, root: &impl IsA<gtk::Widget>) {
        let motion = gtk::EventControllerMotion::new();
        let this = self.this.clone();
        motion.connect_enter(move |_, x, y| {
            if let Some(this) = this.upgrade() {
                this.last_pointer.set((x, y));
                this.show_and_schedule_hide();
            }
        });
        let this = self.this.clone();
        motion.connect_motion(move |_, x, y| {
            let Some(this) = this.upgrade() else {
                return;
            };
            // Only a pointer that moved: GTK repeats the last position when
            // what lies under it changes, as when the bars hide.
            if moved_beyond_epsilon(this.last_pointer.get(), (x, y)) {
                this.last_pointer.set((x, y));
                this.show_and_schedule_hide();
            }
        });
        // Leaving starts the delay again instead of hiding at once, so the
        // pointer can reach a popover, which reads as a leave.
        let this = self.this.clone();
        motion.connect_leave(move |_| {
            if let Some(this) = this.upgrade() {
                this.show_and_schedule_hide();
            }
        });
        root.add_controller(motion);

        let key = gtk::EventControllerKey::new();
        let this = self.this.clone();
        key.connect_key_pressed(move |_, _, _, _| {
            if let Some(this) = this.upgrade() {
                this.show_and_schedule_hide();
            }
            gtk::glib::Propagation::Proceed
        });
        root.add_controller(key);

        // The focus moving is activity too, from a shortcut or a screen
        // reader as much as from a key over the root.
        let this = self.this.clone();
        root.connect_realize(move |root| {
            let Some(window) = root.root().and_downcast::<gtk::Window>() else {
                return;
            };
            let this = this.clone();
            window.connect_focus_widget_notify(move |_| {
                if let Some(this) = this.upgrade() {
                    this.show_and_schedule_hide();
                }
            });
        });
        self.show_and_schedule_hide();
    }

    /// Fade `revealer` out while idle, by [`BIG_MEDIA_CHROME_IDLE_CSS_CLASS`].
    pub fn register_revealer(&self, revealer: &gtk::Revealer) {
        crate::css::install_for_display(
            &revealer.display(),
            CSS,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION - 1,
        );
        revealer.add_css_class("big-media-chrome");
        self.bars
            .borrow_mut()
            .push((revealer.downgrade(), revealer.can_target()));
    }

    /// Keep the bars shown while `popover` is open; its closing starts the
    /// delay again, as the click that picked an item came while it pinned them.
    pub fn register_popover_hide_guard(&self, popover: &gtk::Popover) {
        let this = self.this.clone();
        popover.connect_show(move |_| {
            if let Some(this) = this.upgrade() {
                this.show_and_schedule_hide();
            }
        });
        let this = self.this.clone();
        popover.connect_closed(move |_| {
            if let Some(this) = this.upgrade() {
                this.show_and_schedule_hide();
            }
        });
        self.popover_guards.borrow_mut().push(popover.downgrade());
    }

    /// Keep the bars shown while `keeps_shown` says so. Its owner calls
    /// [`Self::show_and_schedule_hide`] when the answer turns to `false`.
    pub fn register_hide_guard(&self, keeps_shown: impl Fn() -> bool + 'static) {
        self.hide_guards.borrow_mut().push(Box::new(keeps_shown));
    }

    /// Keep the bars and the cursor shown while `pinned`, as for a view with
    /// nothing to look at instead; unpinned, they hide again when idle.
    pub fn set_pinned(&self, pinned: bool) {
        if self.pinned.replace(pinned) != pinned {
            self.show_and_schedule_hide();
        }
    }

    /// Show the bars, and hide them after the delay unless something pins them.
    pub fn show_and_schedule_hide(&self) {
        self.idle_timer.cancel();
        self.show_bars();
        if self.keeps_shown() {
            return;
        }
        let this = self.this.clone();
        self.idle_timer.arm(self.idle_timeout.get(), move || {
            if let Some(this) = this.upgrade()
                && !this.keeps_shown()
            {
                this.hide_bars();
            }
        });
    }

    /// Hide the bars now, whatever pins them, as for a window the person left.
    pub fn hide_now(&self) {
        self.idle_timer.cancel();
        self.hide_bars();
    }

    fn keeps_shown(&self) -> bool {
        if self.pinned.get() {
            return true;
        }
        let popover_open = {
            let mut popovers = self.popover_guards.borrow_mut();
            popovers.retain(|popover| popover.upgrade().is_some());
            popovers
                .iter()
                .filter_map(gtk::glib::WeakRef::upgrade)
                .any(|popover| popover.is_visible())
        };
        popover_open
            || self
                .hide_guards
                .borrow()
                .iter()
                .any(|keeps_shown| keeps_shown())
    }

    fn show_bars(&self) {
        for (revealer, targets) in self.bars.borrow().iter() {
            if let Some(revealer) = revealer.upgrade() {
                revealer.set_reveal_child(true);
                revealer.remove_css_class(BIG_MEDIA_CHROME_IDLE_CSS_CLASS);
                revealer.set_can_target(*targets);
            }
        }
        self.set_cursor("default");
    }

    fn hide_bars(&self) {
        // Kept revealed: a hidden revealer's controls leave the focus chain
        // and the accessibility tree, and the keyboard could not reach them.
        for (revealer, _) in self.bars.borrow().iter() {
            if let Some(revealer) = revealer.upgrade() {
                revealer.add_css_class(BIG_MEDIA_CHROME_IDLE_CSS_CLASS);
                revealer.set_can_target(false);
            }
        }
        self.set_cursor("none");
    }

    fn set_cursor(&self, name: &str) {
        if self.hides_cursor.get()
            && let Some(host) = self.cursor_host.upgrade()
        {
            host.set_cursor_from_name(Some(name));
        }
    }
}

impl Drop for BigMediaImmersionController {
    fn drop(&mut self) {
        self.idle_timer.cancel();
    }
}

fn moved_beyond_epsilon(last: (f64, f64), now: (f64, f64)) -> bool {
    !((now.0 - last.0).abs() < MOTION_EPSILON_PX && (now.1 - last.1).abs() < MOTION_EPSILON_PX)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use big_testkit::harness::{pump, require_gtk_test};

    use super::*;

    /// One bar holding `button`, in a shown window, followed by a controller
    /// that hides at the shortest delay.
    fn bar_with(
        button: &gtk::Button,
    ) -> (gtk::Window, gtk::Revealer, Rc<BigMediaImmersionController>) {
        let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let revealer = gtk::Revealer::builder()
            .reveal_child(true)
            .child(button)
            .build();
        let picture = gtk::Button::with_label("Picture");
        host.append(&picture);
        host.append(&revealer);
        let window = gtk::Window::builder().child(&host).build();
        window.present();
        let controller = BigMediaImmersionController::new(&host);
        controller.set_idle_timeout(Duration::ZERO);
        controller.register_revealer(&revealer);
        assert!(picture.grab_focus());
        (window, revealer, controller)
    }

    /// Long enough for the shortest delay to pass.
    fn idle() {
        let until = Instant::now() + 2 * MIN_IDLE_TIMEOUT;
        while Instant::now() < until {
            pump();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn hidden(revealer: &gtk::Revealer) -> bool {
        revealer.has_css_class(BIG_MEDIA_CHROME_IDLE_CSS_CLASS)
    }

    #[test]
    fn pinned_chrome_stays_shown() {
        require_gtk_test(|| {
            let (window, revealer, controller) = bar_with(&gtk::Button::with_label("Next"));
            controller.set_pinned(true);
            idle();
            assert!(!hidden(&revealer), "pinned bars do not hide");
            controller.set_pinned(false);
            idle();
            assert!(hidden(&revealer), "unpinned bars hide when idle");
            window.destroy();
        });
    }

    #[test]
    fn idle_chrome_stays_reachable() {
        require_gtk_test(|| {
            let next = gtk::Button::with_label("Next");
            let (window, revealer, controller) = bar_with(&next);
            controller.hide_now();
            assert!(next.is_mapped(), "a screen reader still finds the control");
            assert!(!revealer.can_target(), "an unseen control takes no click");
            assert!(next.grab_focus());
            controller.show_and_schedule_hide();
            assert!(revealer.can_target() && !hidden(&revealer));
            window.destroy();
        });
    }

    /// Keyboard focus in a bar, its ring showing, does not keep the bars.
    #[test]
    fn focus_pins_nothing() {
        require_gtk_test(|| {
            let next = gtk::Button::with_label("Next");
            let (window, revealer, controller) = bar_with(&next);
            assert!(next.grab_focus());
            window.set_focus_visible(true);
            controller.show_and_schedule_hide();
            idle();
            assert!(hidden(&revealer));
            assert!(crate::focus::holds_focus(&next), "the control keeps the focus");
            window.destroy();
        });
    }

    #[test]
    fn open_popover_pins() {
        require_gtk_test(|| {
            let next = gtk::Button::with_label("Next");
            let (window, revealer, controller) = bar_with(&next);
            let popover = gtk::Popover::builder()
                .child(&gtk::Label::new(Some("Menu")))
                .build();
            popover.set_parent(&next);
            controller.register_popover_hide_guard(&popover);
            popover.popup();
            idle();
            assert!(!hidden(&revealer), "an open popover pins the bars");
            popover.popdown();
            idle();
            assert!(hidden(&revealer), "closed, it lets them go");
            popover.unparent();
            window.destroy();
        });
    }

    #[test]
    fn hide_guard_pins() {
        require_gtk_test(|| {
            let (window, revealer, controller) = bar_with(&gtk::Button::with_label("Next"));
            let held = Rc::new(Cell::new(true));
            controller.register_hide_guard({
                let held = held.clone();
                move || held.get()
            });
            controller.show_and_schedule_hide();
            idle();
            assert!(!hidden(&revealer));
            held.set(false);
            controller.show_and_schedule_hide();
            idle();
            assert!(hidden(&revealer));
            window.destroy();
        });
    }

    /// The pointer entering shows hidden bars without moving: a picture that
    /// is not redrawn repeats no motion to show them otherwise.
    #[test]
    fn pointer_entering_shows_bars() {
        require_gtk_test(|| {
            let (window, revealer, controller) = bar_with(&gtk::Button::with_label("Next"));
            let root = window.child().expect("the window holds the host");
            controller.attach_activity_detection(&root);
            controller.hide_now();
            let motion = root
                .observe_controllers()
                .into_iter()
                .filter_map(Result::ok)
                .find_map(|controller| controller.downcast::<gtk::EventControllerMotion>().ok())
                .expect("the pointer is followed");
            motion.emit_by_name::<()>("enter", &[&10.0_f64, &10.0_f64]);
            assert!(!hidden(&revealer));
            window.destroy();
        });
    }

    /// A redrawn picture repeats `motion` where the pointer rests.
    #[test]
    fn small_motion_is_rest() {
        let last = (100.0, 100.0);
        assert!(!moved_beyond_epsilon(last, (100.0, 100.0)));
        assert!(!moved_beyond_epsilon(last, (101.5, 98.5)));
        assert!(moved_beyond_epsilon(last, (102.0, 100.0)));
        assert!(moved_beyond_epsilon(last, (100.0, 97.0)));
        assert!(moved_beyond_epsilon((f64::NAN, f64::NAN), last));
    }

    #[test]
    fn idle_timeout_never_flickers() {
        require_gtk_test(|| {
            let controller = BigMediaImmersionController::new(&gtk::Box::default());
            controller.set_idle_timeout(Duration::ZERO);
            assert_eq!(controller.idle_timeout.get(), MIN_IDLE_TIMEOUT);
            controller.set_idle_timeout(Duration::from_millis(1_500));
            assert_eq!(controller.idle_timeout.get(), Duration::from_millis(1_500));
        });
    }

    #[test]
    fn sheet_parses_cleanly() {
        require_gtk_test(|| big_testkit::harness::assert_css_parses(CSS));
    }
}
