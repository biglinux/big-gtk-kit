// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The `AdwApplication` every AppShell program runs.

use std::ops::ControlFlow;

use adw::subclass::prelude::*;

glib::wrapper! {
    /// An `AdwApplication` that hands a command line holding `--` to
    /// `command-line` as the program was given it.
    pub struct ShellApplication(ObjectSubclass<imp::ShellApplication>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl ShellApplication {
    pub fn new(application_id: &str, flags: gio::ApplicationFlags) -> Self {
        glib::Object::builder()
            .property("application-id", application_id)
            .property("flags", flags)
            .build()
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ShellApplication;

    #[glib::object_subclass]
    impl ObjectSubclass for ShellApplication {
        const NAME: &'static str = "BigAppShellApplication";
        type Type = super::ShellApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for ShellApplication {}

    impl ApplicationImpl for ShellApplication {
        /// GApplication reads its own options (`--gapplication-service`) with a
        /// `GOptionContext`, which also takes a lone `--` out of the arguments,
        /// so `prog -- cmd` would reach the program as `prog cmd`. Declined
        /// here, `g_application_run` registers and hands the arguments as they
        /// came to the local `command-line`, or to the running instance; its
        /// options mean nothing in a command line holding `--`.
        fn local_command_line(
            &self,
            arguments: &mut gio::subclass::ArgumentList,
        ) -> ControlFlow<glib::ExitCode> {
            if arguments.iter().any(|argument| argument == "--") {
                return ControlFlow::Continue(());
            }
            self.parent_local_command_line(arguments)
        }
    }

    impl GtkApplicationImpl for ShellApplication {}

    impl AdwApplicationImpl for ShellApplication {}
}
