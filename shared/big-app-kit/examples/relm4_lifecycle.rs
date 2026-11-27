// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT
//! Executable contract harness for AppShell's retained Relm4 lifecycle.

use std::collections::BTreeMap;
use std::ffi::OsString;

use big_app_kit::{
    AppShell, AppShellActivation, AppShellActivationError, AppShellActivationRequest,
    AppShellComponent, AppShellContext, AppShellSpec,
};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::{
    Component, ComponentController, ComponentParts, ComponentSender, Controller, RelmWidgetExt,
};

const APP_ID: &str = "br.com.biglinux.AppKitRelm4Lifecycle";

struct Root {
    activation_count: u32,
    background_count: u32,
    next_window_id: u32,
    secondary_windows: BTreeMap<u32, Controller<SecondaryWindow>>,
}

#[derive(Debug)]
enum RootInput {
    Activation(Vec<OsString>),
    SecondaryDestroyed(u32),
}

#[derive(Debug)]
enum RootCommand {
    BackgroundComplete,
}

#[relm4::component]
impl Component for Root {
    type Init = Vec<OsString>;
    type Input = RootInput;
    type Output = ();
    type CommandOutput = RootCommand;

    view! {
        adw::ApplicationWindow {
            set_title: Some("AppShell Relm4 lifecycle harness"),
            set_default_width: 440,
            set_default_height: 180,

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 12,
                set_margin_all: 24,

                gtk::Label {
                    set_label: "AppShell Relm4 lifecycle harness",
                },

                gtk::Label {
                    #[watch]
                    set_label: &format!(
                        "activations={} background={} secondary={}",
                        model.activation_count,
                        model.background_count,
                        model.secondary_windows.len(),
                    ),
                },
            },
        }
    }

    fn init(
        arguments: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        assert_eq!(
            relm4::main_application().application_id().as_deref(),
            Some(APP_ID)
        );
        let mut model = Self {
            activation_count: 0,
            background_count: 0,
            next_window_id: 1,
            secondary_windows: BTreeMap::new(),
        };
        model.handle_activation(arguments, &sender, &root);
        let widgets = view_output!();
        println!("ROOT_CONTROLLER_RETAINED=1");
        ComponentParts { model, widgets }
    }

    fn update(&mut self, input: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match input {
            RootInput::Activation(arguments) => {
                self.handle_activation(arguments, &sender, root);
            }
            RootInput::SecondaryDestroyed(id) => {
                self.secondary_windows.remove(&id);
                println!("SECONDARY_SURVIVORS={}", self.secondary_windows.len());
            }
        }
    }

    fn update_cmd(
        &mut self,
        command: Self::CommandOutput,
        _sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match command {
            RootCommand::BackgroundComplete => {
                self.background_count += 1;
                println!("BACKGROUND_COMPLETE={}", self.background_count);
            }
        }
    }
}

impl Root {
    fn handle_activation(
        &mut self,
        arguments: Vec<OsString>,
        sender: &ComponentSender<Self>,
        root: &adw::ApplicationWindow,
    ) {
        self.activation_count += 1;
        println!("ACTIVATION_COUNT={}", self.activation_count);
        if has_argument(&arguments, "--spawn") {
            sender.oneshot_command(async { RootCommand::BackgroundComplete });
        }
        if has_argument(&arguments, "--new-window") {
            self.open_secondary(sender, root);
        }
        if has_argument(&arguments, "--quit") {
            relm4::main_application().quit();
        }
    }

    fn open_secondary(&mut self, sender: &ComponentSender<Self>, root: &adw::ApplicationWindow) {
        let id = self.next_window_id;
        self.next_window_id += 1;
        let controller = SecondaryWindow::builder().launch(id).detach();
        controller.widget().set_transient_for(Some(root));
        controller.widget().present();
        let sender = sender.input_sender().clone();
        controller.widget().connect_destroy(move |_| {
            let _ = sender.send(RootInput::SecondaryDestroyed(id));
        });
        self.secondary_windows.insert(id, controller);
        println!("SECONDARY_CREATED={id}");
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        println!("ROOT_DROPPED secondary={}", self.secondary_windows.len());
    }
}

struct SecondaryWindow {
    id: u32,
}

#[relm4::component]
impl Component for SecondaryWindow {
    type Init = u32;
    type Input = ();
    type Output = ();
    type CommandOutput = ();

    view! {
        adw::Window {
            set_title: Some(&format!("Secondary {}", model.id)),
            set_default_width: 300,
            set_default_height: 120,

            gtk::Label {
                set_label: &format!("Secondary window {}", model.id),
            }
        }
    }

    fn init(
        id: Self::Init,
        root: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = Self { id };
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }
}

impl Drop for SecondaryWindow {
    fn drop(&mut self) {
        println!("SECONDARY_DROPPED={}", self.id);
    }
}

impl AppShellComponent for Root {
    fn init_for_activation(
        _context: &AppShellContext,
        request: AppShellActivationRequest,
    ) -> Result<Self::Init, AppShellActivationError> {
        let arguments = validated_activation_arguments(request.activation())?;
        request.succeed();
        Ok(arguments)
    }

    fn input_for_activation(
        request: AppShellActivationRequest,
    ) -> Result<Self::Input, AppShellActivationError> {
        let arguments = validated_activation_arguments(request.activation())?;
        request.succeed();
        Ok(RootInput::Activation(arguments))
    }
}

fn validated_activation_arguments(
    activation: &AppShellActivation,
) -> Result<Vec<OsString>, AppShellActivationError> {
    if has_argument(activation.arguments(), "--fail") {
        return Err(AppShellActivationError::new(
            "intentional activation failure",
            23,
        ));
    }
    Ok(activation.arguments().to_vec())
}

fn has_argument(arguments: &[OsString], expected: &str) -> bool {
    arguments.iter().any(|argument| argument == expected)
}

fn early_cli(arguments: &[OsString]) -> Option<gtk::glib::ExitCode> {
    if has_argument(arguments, "--help") {
        println!("Usage: relm4_lifecycle [--version] [--fail] [--spawn] [--new-window] [--quit]");
        return Some(gtk::glib::ExitCode::SUCCESS);
    }
    if has_argument(arguments, "--version") {
        println!("relm4_lifecycle 0.1.0");
        return Some(gtk::glib::ExitCode::SUCCESS);
    }
    None
}

fn main() -> gtk::glib::ExitCode {
    let exit_code = AppShell::run::<Root>(AppShellSpec::new(APP_ID).early_cli(early_cli));
    println!("APP_SHELL_RETURNED=1");
    exit_code
}
