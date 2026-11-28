// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Setting a `GtkSwitch` from the state it shows, and turning one in a window
//! that draws no frames.

use gtk::prelude::*;

thread_local! {
    static INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Let a switch change at once in a window that draws no frames. Called by
/// [`crate::install`].
///
/// `GtkSwitch` changes its state on the last frame of its slide (GTK 4.22
/// through 4.25.0, still so on `main`), and a window the compositor suspended
/// (screens off, covered) gets no frames: a key or a screen reader's action
/// on it waited until the window was seen again. The hook runs after the
/// switch's handler started the slide.
pub(crate) fn install() {
    if INSTALLED.replace(true) {
        return;
    }
    crate::on_every_emission("activate", |switch: gtk::Switch| {
        if switch.settings().is_gtk_enable_animations() && draws_no_frames(switch.upcast_ref()) {
            set_active(&switch, !switch.is_active());
        }
    });
}

/// Whether `widget` is off screen or in a window the compositor suspended.
fn draws_no_frames(widget: &gtk::Widget) -> bool {
    !widget.is_mapped()
        || widget
            .root()
            .and_then(|root| root.surface())
            .and_downcast::<gtk::gdk::Toplevel>()
            .is_some_and(|toplevel| {
                toplevel
                    .state()
                    .contains(gtk::gdk::ToplevelState::SUSPENDED)
            })
}

/// Show `active` on `switch`, leaving it alone when it already shows it.
///
/// A click does not flip a `GtkSwitch` at once: it starts a 100 ms slide and
/// flips the switch when the slide ends. `gtk_switch_set_active` stops that
/// slide first, even when the value it is given is the one already shown
/// (GTK 4.22). A refresh that re-applies the current state during those
/// 100 ms (a Bluetooth or network signal, a settings reload) swallows the
/// click, and the person has to click again. `AdwSwitchRow` guards its own
/// setter the same way.
pub fn set_active(switch: &gtk::Switch, active: bool) {
    if switch.is_active() != active {
        #[expect(
            clippy::disallowed_methods,
            reason = "the one caller that checks first"
        )]
        switch.set_active(active);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// A click's slide survives a refresh that shows the same state, and lands.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn same_state_refresh_keeps_click() {
        gtk::init().expect("a display");
        let switch = gtk::Switch::new();
        let window = gtk::Window::builder().child(&switch).build();
        window.present();
        switch.emit_activate();
        set_active(&switch, false);
        let context = gtk::glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !switch.is_active() && Instant::now() < deadline {
            context.iteration(false);
        }
        assert!(switch.is_active());
        window.destroy();
    }

    /// Puts `gtk-enable-animations` back as it was when dropped, so a failed
    /// assert leaves it as the other tests expect.
    struct Animations(gtk::Settings, bool);

    impl Drop for Animations {
        fn drop(&mut self) {
            self.0.set_gtk_enable_animations(self.1);
        }
    }

    /// A switch no frame reaches changes with its action, once. The switch is
    /// in no window, so this covers the unmapped branch of `draws_no_frames`
    /// only, not a mapped switch in a suspended window.
    #[gtk::test]
    #[ignore = "needs a display"]
    fn unseen_switch_turns_at_once() {
        let settings = gtk::Settings::default().expect("a display");
        let _restore = Animations(settings.clone(), settings.is_gtk_enable_animations());
        // Installed twice, the hook still runs once.
        install();
        install();
        let switch = gtk::Switch::new();
        settings.set_gtk_enable_animations(true);
        switch.activate();
        assert!(switch.is_active());
        switch.activate();
        assert!(!switch.is_active());
    }
}
