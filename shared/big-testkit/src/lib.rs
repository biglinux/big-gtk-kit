// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! Test harness for the suite: widget finalization censuses in the private
//! session (`scripts/census-smoke.sh`), RSS-slope leak verdicts, RAII
//! subscription guards for GTK lifecycle tests, frame-interval percentiles of a
//! window ([`frame_probe`]), and the process as a screen
//! reader reaches it over AT-SPI. [`leak::WidgetFinalizeCensus`] needs a
//! running GTK app (`harness::run_census_app`) and [`atspi`] the real AT-SPI
//! bridge; the rest runs without a display.

pub mod atspi;
pub mod desktop_entry;
pub mod frame_probe;
pub mod harness;
pub mod leak;
pub mod shortcut_reference;
