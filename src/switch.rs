// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Setting a `GtkSwitch` from the state it shows.

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

    use gtk::prelude::*;

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
}
