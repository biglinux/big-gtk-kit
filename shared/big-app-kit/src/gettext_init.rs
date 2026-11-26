// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
use crate::app_shell::AppShellSpecError;

/// One-shot gettext initialization hook run before the first window.
///
/// `big-app-kit` owns only process-level gettext binding. Framework crates
/// keep their label structs translation-agnostic: apps translate msgids in
/// their binaries and pass the resulting strings into framework APIs.
///
/// # Example
///
/// ```no_run
/// use big_app_kit::GettextInit;
///
/// fn bind_textdomain(init: &GettextInit) -> Result<(), String> {
///     assert_eq!(init.domain(), "bigdemo");
///     Ok(())
/// }
///
/// let init = GettextInit::new("bigdemo", bind_textdomain)
///     .with_locale_dir("/usr/share/locale");
/// assert_eq!(init.locale_dir(), Some("/usr/share/locale"));
///
/// let standard = GettextInit::standard("bigdemo", "/usr/share/locale");
/// assert_eq!(standard.domain(), "bigdemo");
/// ```
#[derive(Debug, Clone, Copy)]
pub struct GettextInit {
    domain: &'static str,
    locale_dir: Option<&'static str>,
    bind: fn(&GettextInit) -> Result<(), String>,
}

impl GettextInit {
    /// Create a gettext hook for `domain`.
    ///
    /// The `bind` function is called during AppShell boot on the main thread,
    /// before GTK starts and before any other thread does.
    #[must_use]
    pub const fn new(domain: &'static str, bind: fn(&GettextInit) -> Result<(), String>) -> Self {
        Self {
            domain,
            locale_dir: None,
            bind,
        }
    }

    /// Create the standard gettext binding for `domain` and `locale_dir`.
    ///
    /// During AppShell boot this calls gettext-rs `setlocale(LC_ALL, "")`,
    /// `bindtextdomain(domain, locale_dir)`,
    /// `bind_textdomain_codeset(domain, "UTF-8")`, and `textdomain(domain)`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use big_app_kit::GettextInit;
    ///
    /// let init = GettextInit::standard("bigdemo", "/usr/share/locale");
    /// assert_eq!(init.domain(), "bigdemo");
    /// assert_eq!(init.locale_dir(), Some("/usr/share/locale"));
    /// ```
    #[must_use]
    pub const fn standard(domain: &'static str, locale_dir: &'static str) -> Self {
        Self::new(domain, bind_standard_gettext).with_locale_dir(locale_dir)
    }

    /// Attach the locale directory passed to the app-owned gettext binder.
    #[must_use]
    pub const fn with_locale_dir(mut self, locale_dir: &'static str) -> Self {
        self.locale_dir = Some(locale_dir);
        self
    }

    /// The gettext domain requested by the app.
    #[must_use]
    pub const fn domain(&self) -> &'static str {
        self.domain
    }

    /// The optional locale directory requested by the app.
    #[must_use]
    pub const fn locale_dir(&self) -> Option<&'static str> {
        self.locale_dir
    }

    pub(crate) fn validate(&self) -> Result<(), AppShellSpecError> {
        if self.domain.trim().is_empty() {
            return Err(AppShellSpecError::EmptyGettextDomain);
        }
        if self
            .locale_dir
            .is_some_and(|locale_dir| locale_dir.trim().is_empty())
        {
            return Err(AppShellSpecError::EmptyGettextLocaleDir);
        }
        Ok(())
    }

    pub(crate) fn bind(&self) -> Result<(), String> {
        (self.bind)(self)
    }

    /// Bind this domain's catalog without claiming the process-global domain.
    ///
    /// `setlocale` and `textdomain` are process-global, so in a multicall host
    /// they belong to whoever booted the process: several products share it and
    /// the last one to call `textdomain` would silently answer for all of them.
    /// A hosted product binds only its own catalog here and resolves its
    /// messages through [`dgettext`](gettextrs::dgettext).
    ///
    /// # Errors
    ///
    /// Returns the failing gettext call when the domain has no locale directory
    /// or the catalog cannot be bound.
    pub fn bind_catalog(&self) -> Result<(), String> {
        let domain = self.domain;
        let locale_dir = self
            .locale_dir
            .ok_or_else(|| format!("gettext domain {domain} is missing a locale dir"))?;
        gettextrs::bindtextdomain(domain, locale_dir)
            .map_err(|error| format!("bindtextdomain({domain}, {locale_dir}): {error}"))?;
        gettextrs::bind_textdomain_codeset(domain, "UTF-8")
            .map_err(|error| format!("bind_textdomain_codeset({domain}, UTF-8): {error}"))?;
        Ok(())
    }
}

fn bind_standard_gettext(init: &GettextInit) -> Result<(), String> {
    let domain = init.domain();
    let locale_dir = init
        .locale_dir()
        .ok_or_else(|| format!("gettext domain {domain} is missing a locale dir"))?;
    gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "");
    gettextrs::bindtextdomain(domain, locale_dir)
        .map_err(|error| format!("bindtextdomain({domain}, {locale_dir}): {error}"))?;
    gettextrs::bind_textdomain_codeset(domain, "UTF-8")
        .map_err(|error| format!("bind_textdomain_codeset({domain}, UTF-8): {error}"))?;
    gettextrs::textdomain(domain).map_err(|error| format!("textdomain({domain}): {error}"))?;
    Ok(())
}
