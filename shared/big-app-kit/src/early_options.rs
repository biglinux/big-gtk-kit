// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! What a product answers on the caller's terminal before GTK starts.
//!
//! A product's binary runs its answer as its
//! [`AppShellSpec::early_cli`](crate::AppShellSpec::early_cli), and big-host
//! runs the same function through the module's command-line interface, so a
//! product reads its options the same way under either. The messages come from
//! the product's catalog, and each product extracts this file.

use std::ffi::{OsStr, OsString};

use glib::ExitCode;

/// `version`, then the build's release channel when it has one.
#[must_use]
pub fn release(version: &str) -> String {
    match option_env!("BIG_RELEASE_CHANNEL") {
        Some(channel) => format!("{version} ({channel})"),
        None => version.to_owned(),
    }
}

/// What `name --version` prints.
#[must_use]
pub fn version_line(name: &str, version: &str) -> String {
    format!("{name} {}", release(version))
}

/// A product's command line: `--help`, `--version` and the options it takes.
#[derive(Debug, Clone, Copy)]
pub struct ProductOptions<'a> {
    /// Binary name.
    pub name: &'a str,
    /// The gettext domain of the product's messages.
    pub domain: &'a str,
    /// The product's `CARGO_PKG_VERSION`.
    pub version: &'a str,
    /// Whether the arguments that are not options name files to open. A product
    /// that opens none refuses them.
    pub accepts_files: bool,
    /// The msgid of what the product does, under the usage, in place of the
    /// line `accepts_files` picks.
    pub summary: Option<&'a str>,
    /// Each option as its help shows it, with a word per value it takes
    /// (`--diff LEFT RIGHT`), and the msgid of what it does. A `--` among them
    /// ends the options: what follows is a file.
    pub options: &'a [(&'a str, &'a str)],
}

impl ProductOptions<'_> {
    /// `-h`/`--help` or `-V`/`--version` as the first argument prints its answer
    /// and succeeds; an option the product does not take is refused on stderr
    /// with exit code 2. `None` when `argv` opens a window.
    #[must_use]
    pub fn answer(&self, argv: &[OsString]) -> Option<ExitCode> {
        let arguments = argv.get(1..)?;
        match arguments.first()?.to_str() {
            Some("-h" | "--help") => {
                print!("{}", self.help());
                return Some(ExitCode::SUCCESS);
            }
            Some("-V" | "--version") => {
                println!("{}", version_line(self.name, self.version));
                return Some(ExitCode::SUCCESS);
            }
            _ => {}
        }
        let refused = self.refused(arguments)?;
        let shown = refused.to_string_lossy();
        let t = self.catalog();
        let message = if is_option(refused) {
            t("{name}: unknown option {option}").replace("{option}", &shown)
        } else {
            t("{name}: unexpected argument {argument}").replace("{argument}", &shown)
        };
        eprintln!("{message}\n{}", t("Try \"{name} --help\"."));
        Some(ExitCode::from(2))
    }

    /// What `--help` prints.
    #[must_use]
    pub fn help(&self) -> String {
        let t = self.catalog();
        let usage = match (self.accepts_files, self.options.is_empty()) {
            (true, true) => t("Usage: {name} [FILE...]"),
            (true, false) => t("Usage: {name} [OPTION...] [FILE...]"),
            (false, true) => t("Usage: {name}"),
            (false, false) => t("Usage: {name} [OPTION...]"),
        };
        let summary = match self.summary {
            Some(msgid) => t(msgid),
            None if self.accepts_files => {
                t("Opens {name}, or hands the files to the window already open.")
            }
            None => t("Opens {name}, or shows the window already open."),
        };
        let help = t("Print this help");
        let version = t("Print the version");
        let rows: Vec<(&str, String)> = self
            .options
            .iter()
            .map(|(shown, msgid)| (*shown, t(msgid)))
            .chain([("-h, --help", help), ("-V, --version", version)])
            .collect();
        let width = rows.iter().map(|(shown, _)| shown.len()).max().unwrap_or(0) + 2;
        let mut text = format!("{usage}\n\n{summary}\n\n{}\n", t("Options:"));
        for (shown, what) in rows {
            text.push_str(&format!("  {shown:<width$}{what}\n"));
        }
        text
    }

    /// The first argument the product does not take: an option before a taken
    /// `--`, or anything at all when it opens no files. The values a
    /// taken option shows in its help follow it, whatever their spelling.
    fn refused<'a>(&self, arguments: &'a [OsString]) -> Option<&'a OsStr> {
        let mut arguments = arguments.iter().map(OsString::as_os_str);
        while let Some(argument) = arguments.next() {
            let Some(values) = self.values_of(argument) else {
                if is_option(argument) || !self.accepts_files {
                    return Some(argument);
                }
                continue;
            };
            if argument == "--" {
                return arguments.next().filter(|_| !self.accepts_files);
            }
            for _ in 0..values {
                arguments.next();
            }
        }
        None
    }

    /// How many values `argument` takes, from its help (`--diff LEFT RIGHT`
    /// takes two); `None` when the product does not take it.
    fn values_of(&self, argument: &OsStr) -> Option<usize> {
        self.options.iter().find_map(|(shown, _)| {
            let mut words = shown.split(' ');
            (words.next() == argument.to_str()).then(|| words.count())
        })
    }

    /// Looks a msgid up in the product's catalog and puts its name in place.
    /// Called as `t("...")`, the name `xgettext` extracts.
    fn catalog(&self) -> impl Fn(&str) -> String {
        let (name, domain) = (self.name, self.domain);
        move |msgid| gettextrs::dgettext(domain, msgid).replace("{name}", name)
    }
}

fn is_option(argument: &OsStr) -> bool {
    let bytes = argument.as_encoded_bytes();
    bytes.len() > 1 && bytes[0] == b'-'
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGES: ProductOptions<'static> = ProductOptions {
        name: "viewer",
        domain: "viewer",
        version: "1.0",
        accepts_files: true,
        summary: None,
        options: &[("--resize", "Resize"), ("--", "Files follow")],
    };

    fn refused(options: &ProductOptions<'_>, arguments: &[&str]) -> Option<String> {
        let arguments: Vec<OsString> = arguments.iter().map(OsString::from).collect();
        options
            .refused(&arguments)
            .map(|option| option.to_string_lossy().into_owned())
    }

    #[test]
    fn unknown_option_is_refused() {
        assert_eq!(
            refused(&IMAGES, &["a.png", "--resize", "-x"]),
            Some("-x".into())
        );
        assert_eq!(refused(&IMAGES, &["--resize", "-", "a.png"]), None);
        // After a taken `--`, an option's spelling is a file name.
        assert_eq!(refused(&IMAGES, &["--", "--kittens"]), None);
        let plain = ProductOptions {
            options: &[],
            ..IMAGES
        };
        assert_eq!(refused(&plain, &["--", "a.png"]), Some("--".into()));
        let no_files = ProductOptions {
            accepts_files: false,
            ..IMAGES
        };
        assert_eq!(
            refused(&no_files, &["--resize", "a.png"]),
            Some("a.png".into())
        );
        assert_eq!(refused(&no_files, &["--", "a.png"]), Some("a.png".into()));
    }

    #[test]
    fn option_values_may_start_with_dash() {
        let diff = ProductOptions {
            options: &[("--diff LEFT RIGHT", "Compare")],
            ..IMAGES
        };
        assert_eq!(refused(&diff, &["--diff", "-old.txt", "-new.txt"]), None);
        assert_eq!(
            refused(&diff, &["--diff", "a.txt", "b.txt", "-x"]),
            Some("-x".into())
        );
    }

    #[test]
    fn help_lines_up_every_option() {
        let help = IMAGES.help();
        assert!(
            help.starts_with("Usage: viewer [OPTION...] [FILE...]\n"),
            "{help}"
        );
        assert!(help.contains("\n  --resize       Resize\n"), "{help}");
        assert!(
            help.ends_with("\n  -V, --version  Print the version\n"),
            "{help}"
        );
    }
}
