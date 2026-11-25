// SPDX-License-Identifier: MIT

//! User-visible diagnostics bundle assembly.
//!
//! This module is GTK-free and performs no disk or network I/O. App layers
//! provide GTK/libadwaita, renderer, GPU, and module facts; this foundation
//! module assembles a redacted plain-text report the app can show before the
//! user copies it.

use std::env;
use std::fmt;

pub(crate) mod redaction;

pub use redaction::{
    BigRedactionRules, Redacted, redact_diagnostics_path, redact_diagnostics_text, redact_path,
};

/// Caller-supplied application identity and diagnostics facts.
///
/// # Examples
///
/// ```
/// use big_os_kit::diagnostics::{BigAppIdentity, BigDiagnosticsModule};
///
/// let identity = BigAppIdentity::new("big", "1.0.0", "br.com.biglinux.Demo", "1.0.0")
///     .with_module(BigDiagnosticsModule::new("files", "Files").with_abi("6"));
/// assert_eq!(identity.app_id(), "br.com.biglinux.Demo");
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BigAppIdentity {
    suite_id: String,
    suite_version: String,
    app_id: String,
    app_version: String,
    build_id: Option<String>,
    graphics: BigGraphicsRuntime,
    modules: Vec<BigDiagnosticsModule>,
}

impl BigAppIdentity {
    /// Create the required identity fields for a diagnostics report.
    #[must_use]
    pub fn new(
        suite_id: impl Into<String>,
        suite_version: impl Into<String>,
        app_id: impl Into<String>,
        app_version: impl Into<String>,
    ) -> Self {
        Self {
            suite_id: suite_id.into(),
            suite_version: suite_version.into(),
            app_id: app_id.into(),
            app_version: app_version.into(),
            build_id: None,
            graphics: BigGraphicsRuntime::from_environment(),
            modules: Vec::new(),
        }
    }

    /// Add one installed module entry.
    #[must_use]
    pub fn with_module(mut self, module: BigDiagnosticsModule) -> Self {
        self.modules.push(module);
        self
    }

    /// App identifier.
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }
}

/// Caller-supplied installed module diagnostics entry.
///
/// # Examples
///
/// ```
/// use big_os_kit::diagnostics::BigDiagnosticsModule;
///
/// let module = BigDiagnosticsModule::new("files", "Files").with_abi("6");
/// assert_eq!(module.id(), "files");
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BigDiagnosticsModule {
    id: String,
    display_name: String,
    abi: Option<String>,
}

impl BigDiagnosticsModule {
    /// Create a module entry without an ABI value.
    #[must_use]
    pub fn new(id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            abi: None,
        }
    }

    /// Add the caller's ABI string for this module.
    #[must_use]
    pub fn with_abi(mut self, abi: impl Into<String>) -> Self {
        self.abi = Some(abi.into());
        self
    }

    /// Stable module identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// GTK/libadwaita and display diagnostics supplied by the app layer.
///
/// # Examples
///
/// ```
/// use big_os_kit::diagnostics::BigGraphicsRuntime;
///
/// let graphics = BigGraphicsRuntime::from_environment()
///     .with_gtk_versions("4.22.0", "4.22.1")
///     .with_adwaita_versions("1.9.0", "1.9.0")
///     .with_renderer("gl");
/// assert!(graphics.renderer().contains("gl"));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BigGraphicsRuntime {
    gtk_compile_version: String,
    gtk_runtime_version: String,
    adwaita_compile_version: String,
    adwaita_runtime_version: String,
    renderer: String,
    gpu: String,
    compositor: String,
    display_probe_note: String,
}

impl BigGraphicsRuntime {
    /// Build display hints from a small allowlist of environment variables.
    ///
    /// GTK/adwaita versions, exact renderer, and GPU strings require the caller
    /// to query GTK/GDK after display initialization.
    #[must_use]
    pub fn from_environment() -> Self {
        Self {
            gtk_compile_version: "not supplied by GTK-free collector".to_owned(),
            gtk_runtime_version: "not supplied by GTK-free collector".to_owned(),
            adwaita_compile_version: "not supplied by GTK-free collector".to_owned(),
            adwaita_runtime_version: "not supplied by GTK-free collector".to_owned(),
            renderer: ["GSK_RENDERER", "GDK_RENDERER"]
                .into_iter()
                .find_map(|name| env::var(name).ok().map(|value| format!("{name}={value}")))
                .unwrap_or_else(|| "not detected from environment".to_owned()),
            gpu: "not supplied; requires GTK/GDK display probe".to_owned(),
            compositor: compositor_hint_from_environment(),
            display_probe_note:
                "GPU and exact renderer require a live GTK/GDK display; this collector is graceful without one."
                    .to_owned(),
        }
    }

    /// Set compile-time and runtime GTK version strings.
    #[must_use]
    pub fn with_gtk_versions(
        mut self,
        compile_version: impl Into<String>,
        runtime_version: impl Into<String>,
    ) -> Self {
        self.gtk_compile_version = compile_version.into();
        self.gtk_runtime_version = runtime_version.into();
        self
    }

    /// Set compile-time and runtime libadwaita version strings.
    #[must_use]
    pub fn with_adwaita_versions(
        mut self,
        compile_version: impl Into<String>,
        runtime_version: impl Into<String>,
    ) -> Self {
        self.adwaita_compile_version = compile_version.into();
        self.adwaita_runtime_version = runtime_version.into();
        self
    }

    /// Set the renderer string supplied by the app layer.
    #[must_use]
    pub fn with_renderer(mut self, renderer: impl Into<String>) -> Self {
        self.renderer = renderer.into();
        self
    }

    /// Renderer or renderer hint.
    #[must_use]
    pub fn renderer(&self) -> &str {
        &self.renderer
    }
}

/// Plain-text diagnostics report.
///
/// # Examples
///
/// ```
/// use big_os_kit::diagnostics::{BigAppIdentity, BigDiagnosticsReport};
///
/// let identity = BigAppIdentity::new("big", "1.0.0", "demo.app", "1.0.0");
/// let report = BigDiagnosticsReport::collect(&identity);
/// assert!(report.as_str().contains("BigLinux diagnostics"));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BigDiagnosticsReport {
    text: String,
}

impl BigDiagnosticsReport {
    /// Collect a report using environment-derived redaction rules.
    #[must_use]
    pub fn collect(app_identity: &BigAppIdentity) -> Self {
        Self::collect_with_redaction(app_identity, &BigRedactionRules::from_environment())
    }

    /// Collect a report using caller-supplied redaction rules.
    #[must_use]
    pub(crate) fn collect_with_redaction(
        app_identity: &BigAppIdentity,
        redaction: &BigRedactionRules,
    ) -> Self {
        let mut text = String::new();
        // Writing into a String cannot fail.
        let _ = write_report(&mut text, app_identity);
        Self {
            text: redact_diagnostics_text(&text, redaction),
        }
    }

    /// Borrow the report text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Consume the report into its text.
    #[must_use]
    pub fn into_string(self) -> String {
        self.text
    }
}

impl fmt::Display for BigDiagnosticsReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

fn write_report(text: &mut String, app_identity: &BigAppIdentity) -> fmt::Result {
    use fmt::Write as _;

    writeln!(text, "BigLinux diagnostics")?;
    writeln!(
        text,
        "Privacy: user-initiated, user-visible, no telemetry, no disk writes."
    )?;
    writeln!(text)?;

    writeln!(text, "Application")?;
    writeln!(
        text,
        "  Suite: {} {}",
        app_identity.suite_id, app_identity.suite_version
    )?;
    writeln!(
        text,
        "  App: {} {}",
        app_identity.app_id, app_identity.app_version
    )?;
    writeln!(
        text,
        "  Build: {}",
        app_identity.build_id.as_deref().unwrap_or("not supplied")
    )?;
    writeln!(text)?;

    writeln!(text, "Graphics")?;
    let graphics = &app_identity.graphics;
    writeln!(text, "  GTK compile: {}", graphics.gtk_compile_version)?;
    writeln!(text, "  GTK runtime: {}", graphics.gtk_runtime_version)?;
    writeln!(
        text,
        "  libadwaita compile: {}",
        graphics.adwaita_compile_version
    )?;
    writeln!(
        text,
        "  libadwaita runtime: {}",
        graphics.adwaita_runtime_version
    )?;
    writeln!(text, "  Renderer: {}", graphics.renderer)?;
    writeln!(text, "  GPU: {}", graphics.gpu)?;
    writeln!(text, "  Compositor: {}", graphics.compositor)?;
    writeln!(text, "  Display note: {}", graphics.display_probe_note)?;
    writeln!(text)?;

    writeln!(text, "Modules")?;
    if app_identity.modules.is_empty() {
        writeln!(text, "  (none supplied by caller)")?;
    } else {
        for module in &app_identity.modules {
            writeln!(
                text,
                "  - {} ({}) ABI {}",
                module.id,
                module.display_name,
                module.abi.as_deref().unwrap_or("not supplied")
            )?;
        }
    }
    writeln!(text)
}

fn compositor_hint_from_environment() -> String {
    let display = if let Ok(wayland_display) = env::var("WAYLAND_DISPLAY") {
        format!("wayland display {wayland_display}")
    } else if let Ok(x11_display) = env::var("DISPLAY") {
        format!("x11 display {x11_display}")
    } else {
        "no display variable detected".to_owned()
    };
    match env::var("XDG_CURRENT_DESKTOP") {
        Ok(desktop) if !desktop.trim().is_empty() => format!("{display}; desktop {desktop}"),
        _ => display,
    }
}
