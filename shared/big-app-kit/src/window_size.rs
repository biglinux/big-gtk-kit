// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A window's size and whether it is maximized, kept in a file so that a
//! window brought back after a crash or a desktop restart opens as it was.
//!
//! Written a moment after the size changes and at no other time: a window
//! left alone costs nothing. The file is one line, `width height maximized`.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use big_gtk_kit::timer::OnceTimer;
use gtk::prelude::*;

/// How long a resize must settle before it is written.
const SETTLE: Duration = Duration::from_millis(500);

/// A window's size and whether it filled the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BigWindowSize {
    /// Width in logical pixels.
    pub width: i32,
    /// Height in logical pixels.
    pub height: i32,
    /// Whether the window was maximized.
    pub maximized: bool,
}

impl BigWindowSize {
    fn to_line(self) -> String {
        format!(
            "{} {} {}\n",
            self.width,
            self.height,
            u8::from(self.maximized)
        )
    }

    fn from_line(line: &str) -> Option<Self> {
        let mut fields = line.split_whitespace();
        let width = fields.next()?.parse().ok().filter(|width| *width > 0)?;
        let height = fields.next()?.parse().ok().filter(|height| *height > 0)?;
        let maximized = match fields.next()? {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        Some(Self {
            width,
            height,
            maximized,
        })
    }
}

/// The size kept at `path`, if there is one. A small file, read once when
/// the window is built.
#[must_use]
pub fn read_window_size(path: &Path) -> Option<BigWindowSize> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| BigWindowSize::from_line(&text))
}

/// Give `window` the size kept at `path`; `false` when none was kept.
pub fn restore_window_size(window: &impl IsA<gtk::Window>, path: &Path) -> bool {
    let Some(size) = read_window_size(path) else {
        return false;
    };
    window.set_default_size(size.width, size.height);
    window.set_maximized(size.maximized);
    true
}

/// Keep `window`'s size at `path` a moment after it changes, until the window
/// leaves its application (closed by the person, when its owner forgets the
/// file), after which nothing is written.
pub fn keep_window_size(window: &impl IsA<gtk::Window>, path: PathBuf) {
    let pending = OnceTimer::new();
    let path = Rc::new(path);
    let schedule = Rc::new(move |window: &gtk::Window| {
        let weak = window.downgrade();
        let path = Rc::clone(&path);
        pending.arm(SETTLE, move || {
            let Some(window) = weak
                .upgrade()
                .filter(|window| window.application().is_some())
            else {
                return;
            };
            let (width, height) = window.default_size();
            let size = BigWindowSize {
                width,
                height,
                maximized: window.is_maximized(),
            };
            let path = PathBuf::clone(&path);
            gio::spawn_blocking(move || {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = big_os_kit::storage::atomic_write_private(&path, size.to_line().as_bytes());
            });
        });
    });
    let window = window.upcast_ref::<gtk::Window>();
    for property in ["default-width", "default-height", "maximized"] {
        let schedule = Rc::clone(&schedule);
        window.connect_notify_local(Some(property), move |window, _| schedule(window));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_size_reads_back() {
        let size = BigWindowSize {
            width: 900,
            height: 600,
            maximized: true,
        };
        assert_eq!(BigWindowSize::from_line(&size.to_line()), Some(size));
        for nonsense in ["", "0 600 1", "900 600", "900 600 yes", "-5 600 0"] {
            assert_eq!(BigWindowSize::from_line(nonsense), None, "{nonsense:?}");
        }
    }
}
