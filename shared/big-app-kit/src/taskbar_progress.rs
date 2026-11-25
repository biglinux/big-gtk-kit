// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Progress on the application's taskbar button.
//!
//! Each [`TaskbarProgress`] is one running task of one product; its button
//! shows the mean of that product's tasks that know their fraction, through
//! the `com.canonical.Unity.LauncherEntry` `Update` signal the shell listens
//! to. The product names itself, since a shared host process runs several
//! under its own application id. A signal goes out only when the shown percent
//! changes, at most four times a second, and never while nothing changes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::{Duration, Instant};

use big_relm4_components::timer::OnceTimer;
use gio::prelude::*;
use glib::variant::ToVariant;

const OBJECT_PATH: &str = "/com/canonical/unity/launcherentry/1";
const INTERFACE: &str = "com.canonical.Unity.LauncherEntry";
const INTERVAL: Duration = Duration::from_millis(250);

#[derive(Default)]
struct Launchers {
    next_id: u64,
    by_app: HashMap<&'static str, Launcher>,
}

thread_local! {
    static LAUNCHERS: RefCell<Launchers> = RefCell::default();
}

/// One task's share of a product's taskbar button. Created and used on the
/// GTK thread; dropping it takes the task off the button.
pub struct TaskbarProgress {
    app_id: &'static str,
    id: u64,
    _gtk_thread: PhantomData<Rc<()>>,
}

impl TaskbarProgress {
    /// A task of the product whose `.desktop` file is named `app_id`, with no
    /// progress shown yet.
    #[must_use]
    pub fn new(app_id: &'static str) -> Self {
        let id = LAUNCHERS.with_borrow_mut(|launchers| {
            launchers.next_id += 1;
            launchers.next_id
        });
        Self {
            app_id,
            id,
            _gtk_thread: PhantomData,
        }
    }

    /// Show `fraction` (0 to 1) for this task, or take it off the button with
    /// `None` while it runs without a known fraction or has ended.
    pub fn set(&self, fraction: Option<f64>) {
        LAUNCHERS.with_borrow_mut(|launchers| self.apply(launchers, fraction));
    }

    fn apply(&self, launchers: &mut Launchers, fraction: Option<f64>) {
        let launcher = launchers.by_app.entry(self.app_id).or_default();
        launcher.set(self.id, fraction);
        update(self.app_id, launcher);
    }
}

impl Drop for TaskbarProgress {
    fn drop(&mut self) {
        // Gone with the thread's other locals at exit, when there is no one
        // left to tell.
        let _ = LAUNCHERS.try_with(|launchers| self.apply(&mut launchers.borrow_mut(), None));
    }
}

#[derive(Default)]
struct Launcher {
    fractions: HashMap<u64, f64>,
    /// The percent last sent, `None` while hidden.
    sent: Option<u8>,
    sent_at: Option<Instant>,
    flush: OnceTimer,
}

#[derive(Debug, PartialEq)]
enum Due {
    Nothing,
    Send(Option<u8>),
    Wait(Duration),
}

impl Launcher {
    fn set(&mut self, id: u64, fraction: Option<f64>) {
        match fraction {
            Some(fraction) => self.fractions.insert(id, fraction.clamp(0.0, 1.0)),
            None => self.fractions.remove(&id),
        };
    }

    fn wanted(&self) -> Option<u8> {
        if self.fractions.is_empty() {
            return None;
        }
        let mean = self.fractions.values().sum::<f64>() / self.fractions.len() as f64;
        // Floored, so the button never reads full before the work is.
        Some((mean * 100.0).floor() as u8)
    }

    fn due(&self, now: Instant) -> Due {
        let wanted = self.wanted();
        if wanted == self.sent {
            return Due::Nothing;
        }
        // A hide cannot wait: the process may be quitting, and the shell keeps
        // the last bar it was sent.
        if wanted.is_none() {
            return Due::Send(None);
        }
        match self.sent_at.map(|at| now.saturating_duration_since(at)) {
            Some(since) if since < INTERVAL => Due::Wait(INTERVAL.saturating_sub(since)),
            _ => Due::Send(wanted),
        }
    }
}

fn update(app_id: &'static str, launcher: &mut Launcher) {
    let now = Instant::now();
    match launcher.due(now) {
        Due::Nothing => {}
        Due::Send(percent) => {
            launcher.flush.cancel();
            launcher.sent = percent;
            launcher.sent_at = Some(now);
            send(app_id, percent);
        }
        // An armed flush reads the latest state when it fires.
        Due::Wait(_) if launcher.flush.is_pending() => {}
        Due::Wait(delay) => {
            launcher.flush.arm(delay, move || {
                LAUNCHERS.with_borrow_mut(|launchers| {
                    if let Some(launcher) = launchers.by_app.get_mut(app_id) {
                        update(app_id, launcher);
                    }
                });
            });
        }
    }
}

fn send(app_id: &str, percent: Option<u8>) {
    let Some(connection) = gio::Application::default().and_then(|app| app.dbus_connection()) else {
        return;
    };
    let body = update_body(&format!("application://{app_id}.desktop"), percent);
    if let Err(error) = connection.emit_signal(None, OBJECT_PATH, INTERFACE, "Update", Some(&body))
    {
        log::debug!("taskbar progress not sent: {error}");
    }
}

fn update_body(uri: &str, percent: Option<u8>) -> glib::Variant {
    let properties = glib::VariantDict::new(None);
    properties.insert("progress", f64::from(percent.unwrap_or(0)) / 100.0);
    properties.insert("progress-visible", percent.is_some());
    glib::Variant::tuple_from_iter([uri.to_variant(), properties.end()])
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Due, INTERVAL, Launcher, update, update_body};

    #[test]
    fn update_body_carries_progress() {
        let body = update_body("application://br.com.biglinux.bigfiles.desktop", Some(40));

        assert_eq!(body.type_().as_str(), "(sa{sv})");
        assert_eq!(
            body.child_value(0).get::<String>().as_deref(),
            Some("application://br.com.biglinux.bigfiles.desktop")
        );
        let properties = glib::VariantDict::new(Some(&body.child_value(1)));
        assert_eq!(
            properties.lookup::<f64>("progress").ok().flatten(),
            Some(0.4)
        );
        assert_eq!(
            properties.lookup::<bool>("progress-visible").ok().flatten(),
            Some(true)
        );
        let hidden = update_body("application://x.desktop", None);
        let properties = glib::VariantDict::new(Some(&hidden.child_value(1)));
        assert_eq!(
            properties.lookup::<bool>("progress-visible").ok().flatten(),
            Some(false)
        );
    }

    #[test]
    fn two_tasks_show_their_mean() {
        let mut launcher = Launcher::default();
        launcher.set(1, Some(0.2));
        launcher.set(2, Some(0.6));

        assert_eq!(launcher.due(Instant::now()), Due::Send(Some(40)));
    }

    #[test]
    fn last_task_ending_hides_at_once() {
        let start = Instant::now();
        let mut launcher = Launcher::default();
        launcher.set(1, Some(0.3));
        launcher.set(2, Some(0.5));
        launcher.sent = Some(40);
        launcher.sent_at = Some(start);

        launcher.set(1, None);
        launcher.set(2, None);

        assert_eq!(
            launcher.due(start + Duration::from_millis(10)),
            Due::Send(None)
        );
    }

    #[test]
    fn changes_wait_for_percent_and_interval() {
        let start = Instant::now();
        let mut launcher = Launcher::default();
        launcher.set(1, Some(0.405));
        launcher.sent = Some(40);
        launcher.sent_at = Some(start);

        assert_eq!(launcher.due(start + INTERVAL), Due::Nothing);
        launcher.set(1, Some(0.6));
        assert_eq!(
            launcher.due(start + Duration::from_millis(100)),
            Due::Wait(Duration::from_millis(150))
        );
        assert_eq!(launcher.due(start + INTERVAL), Due::Send(Some(60)));
    }

    #[test]
    fn hide_drops_waiting_flush() {
        let app = "br.com.biglinux.example";
        let mut launcher = Launcher::default();
        launcher.set(1, Some(0.6));
        launcher.sent = Some(30);
        // Sent "later" than now, so the change waits however slow the run is.
        launcher.sent_at = Some(Instant::now() + Duration::from_secs(60));

        update(app, &mut launcher);
        assert!(
            launcher.flush.is_pending(),
            "a change inside the interval waits"
        );

        launcher.set(1, None);
        update(app, &mut launcher);
        assert!(!launcher.flush.is_pending(), "a hide is sent at once");
        assert_eq!(launcher.sent, None);
    }
}
