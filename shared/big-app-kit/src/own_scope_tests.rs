// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

use super::*;

/// A token given for the press that asked reaches the program started, in
/// both of the variables programs read it from.
#[test]
fn given_token_reaches_program() {
    let dir = glib::mkdtemp(std::env::temp_dir().join("big-app-kit-token-XXXXXX"))
        .expect("temporary directory");
    let script = dir.join("record");
    let seen = dir.join("seen");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s %s' \"$XDG_ACTIVATION_TOKEN\" \"$DESKTOP_STARTUP_ID\" >\"$1.part\"\nmv \"$1.part\" \"$1\"\n",
    )
    .expect("script");
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("executable");
    let command = format!(
        "{} {}",
        glib::shell_quote(&script).to_string_lossy(),
        glib::shell_quote(&seen).to_string_lossy()
    );
    let program = gio::AppInfo::create_from_commandline(
        &command,
        Some("Record"),
        gio::AppInfoCreateFlags::NONE,
    )
    .expect("an entry");
    launch(&program, &[], Some("press-token"), None, None).expect("started");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !seen.exists() && std::time::Instant::now() < until {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::read_to_string(&seen).expect("the program ran"),
        "press-token press-token"
    );
    std::fs::remove_dir_all(&dir).expect("temporary directory removed");
}

fn gpu(name: &str, default: bool, environment: &[&str]) -> glib::VariantDict {
    let gpu = glib::VariantDict::new(None);
    gpu.insert_value("Name", &name.to_variant());
    gpu.insert_value("Default", &default.to_variant());
    gpu.insert_value("Environment", &environment.to_variant());
    gpu
}

#[test]
fn each_gpu_environment_found() {
    let gpus = glib::Variant::array_from_iter_with_type(
        glib::VariantTy::VARDICT,
        [
            gpu(
                "Intel HD Graphics 630",
                true,
                &[
                    "DRI_PRIME",
                    "pci-0000_00_02_0",
                    "VK_LOADER_DRIVERS_SELECT",
                    "*intel*",
                ],
            )
            .end(),
            gpu(
                "NVIDIA GeForce GTX 1050 Ti",
                false,
                &[
                    "__NV_PRIME_RENDER_OFFLOAD",
                    "1",
                    "__GLX_VENDOR_LIBRARY_NAME",
                    "nvidia",
                ],
            )
            .end(),
        ],
    );
    assert_eq!(
        environment_of(&gpus, false),
        Some(vec![
            ("__NV_PRIME_RENDER_OFFLOAD".to_owned(), "1".to_owned()),
            ("__GLX_VENDOR_LIBRARY_NAME".to_owned(), "nvidia".to_owned()),
        ])
    );
    assert_eq!(
        environment_of(&gpus, true),
        Some(vec![
            ("DRI_PRIME".to_owned(), "pci-0000_00_02_0".to_owned()),
            ("VK_LOADER_DRIVERS_SELECT".to_owned(), "*intel*".to_owned()),
        ])
    );
    let one = glib::Variant::array_from_iter_with_type(
        glib::VariantTy::VARDICT,
        [gpu("Intel HD Graphics 630", true, &[]).end()],
    );
    assert_eq!(environment_of(&one, false), None);
}

/// The arguments and environment of a program started with `renderer`'s
/// environment, and the `gpu` one switcheroo-control would give when that is
/// `Some`, from an entry whose `Exec` passes `--flag` and the files.
fn launched_with(
    renderer: Renderer,
    gpu: Option<&[(String, String)]>,
) -> (Vec<String>, Vec<String>) {
    launched_from(renderer, gpu, false)
}

/// [`launched_with`] from a desktop file, `DBusActivatable` or not, named
/// as a bus name so GIO gives it an application id.
fn launched_from(
    renderer: Renderer,
    gpu: Option<&[(String, String)]>,
    dbus_activatable: bool,
) -> (Vec<String>, Vec<String>) {
    let dir = glib::mkdtemp(std::env::temp_dir().join("big-app-kit-launch-XXXXXX"))
        .expect("temporary directory");
    let script = dir.join("report");
    std::fs::write(
        &script,
        "{ printf '%s\\n' \"$@\"; echo --; env; } > \"$0.part\" && mv \"$0.part\" \"$0.out\"\n",
    )
    .expect("script written");
    let entry = dir.join("org.example.Report.desktop");
    std::fs::write(
        &entry,
        format!(
            "[Desktop Entry]\nType=Application\nName=Report\n\
             Exec=sh \"{}\" --flag %f\nDBusActivatable={dbus_activatable}\n",
            script.display()
        ),
    )
    .expect("desktop file written");
    let application =
        big_testkit::desktop_entry::app_info_from_file(&entry).expect("valid desktop file");
    let context = gio::AppLaunchContext::new();
    set_environment(&context, renderer);
    if let Some(environment) = gpu {
        set_gpu_environment(&context, renderer == Renderer::NonDefaultGpu, environment);
    }
    start(
        &application,
        &[gio::File::for_path("/srv/a b.txt")],
        Some(&context),
    )
    .expect("program started");
    let out = dir.join("report.out");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !out.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let report = std::fs::read_to_string(&out).expect("program reported");
    std::fs::remove_dir_all(&dir).expect("temporary directory removed");
    let mut lines = report.lines().map(str::to_owned);
    let arguments = lines.by_ref().take_while(|line| line != "--").collect();
    (arguments, lines.collect())
}

#[test]
fn renderer_environment_reaches_program() {
    use big_os_kit::subprocess::DESKTOP_GPU;
    const CHILD: &str = "BIG_APP_KIT_RENDERER_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // The desktop's variables have to be in this process's environment:
        // run this test again in a child that has them and, past what it needs
        // to run, nothing else, so a caller's renderer variables (the headless
        // gate's software ones) cannot reach what it checks.
        let mut child = big_os_kit::subprocess::BigSubprocessSpec::builder()
            .program(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "own_scope::tests::renderer_environment_reaches_program",
                "--quiet",
            ])
            .env_inherit(false);
        for variable in ["PATH", "TMPDIR"] {
            if let Some(value) = std::env::var_os(variable) {
                child = child.env(variable, value);
            }
        }
        let out = child
            .env(
                "__EGL_VENDOR_LIBRARY_FILENAMES",
                "/usr/share/glvnd/egl_vendor.d/50_mesa.json",
            )
            .env("VK_LOADER_DRIVERS_DISABLE", "*nvidia*")
            // A session that offloads everything to the NVIDIA GPU.
            .env("__NV_PRIME_RENDER_OFFLOAD", "1")
            .env("__GLX_VENDOR_LIBRARY_NAME", "nvidia")
            .env("__VK_LAYER_NV_optimus", "NVIDIA_only")
            .env(DESKTOP_GPU, "mesa")
            .env(CHILD, "1")
            .env("NOTIFY_SOCKET", "@desktop")
            // A person's own driver list, which leaves lavapipe out.
            .env("VK_DRIVER_FILES", "/usr/share/vulkan/icd.d/nvidia_icd.json")
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .run()
            .expect("child test runs");
        assert!(
            out.status.success() && out.stdout_lossy().contains("1 passed"),
            "{}",
            out.stdout_lossy()
        );
        return;
    }
    let set = |environment: &[String], variable: &str| {
        environment
            .iter()
            .any(|line| line.starts_with(&format!("{variable}=")))
    };
    let desktop_only = ["VK_LOADER_DRIVERS_DISABLE", DESKTOP_GPU, "NOTIFY_SOCKET"];

    let nvidia_offload = [
        "__NV_PRIME_RENDER_OFFLOAD",
        "__GLX_VENDOR_LIBRARY_NAME",
        "__VK_LAYER_NV_optimus",
    ];

    let (arguments, environment) = launched_with(Renderer::Software, None);
    assert_eq!(arguments, ["--flag", "/srv/a b.txt"]);
    for (variable, value) in SOFTWARE_RENDERING {
        assert!(
            environment.contains(&format!("{variable}={value}")),
            "{variable} missing: {environment:?}"
        );
    }
    for variable in desktop_only.into_iter().chain(["VK_DRIVER_FILES"]) {
        assert!(!set(&environment, variable), "{variable} kept");
    }

    // An entry without a preference keeps the session's choices.
    let (arguments, environment) = launched_with(Renderer::DefaultGpu, None);
    assert_eq!(arguments, ["--flag", "/srv/a b.txt"]);
    for variable in nvidia_offload.into_iter().chain(["VK_DRIVER_FILES"]) {
        assert!(set(&environment, variable), "{variable} dropped");
    }
    for variable in desktop_only
        .into_iter()
        .chain(["__EGL_VENDOR_LIBRARY_FILENAMES", "LIBGL_ALWAYS_SOFTWARE"])
    {
        assert!(!set(&environment, variable), "{variable} kept");
    }

    // The integrated GPU, asked for.
    let integrated = [
        ("DRI_PRIME".to_owned(), "pci-0000_00_02_0".to_owned()),
        ("VK_LOADER_DRIVERS_SELECT".to_owned(), "*intel*".to_owned()),
    ];
    let (_, environment) = launched_with(Renderer::DefaultGpu, Some(&integrated));
    for (variable, value) in &integrated {
        assert!(environment.contains(&format!("{variable}={value}")));
    }
    for variable in nvidia_offload.into_iter().chain(["VK_DRIVER_FILES"]) {
        assert!(!set(&environment, variable), "{variable} kept");
    }

    // The dedicated one keeps them: its own offload variables come on top.
    let (_, environment) = launched_with(Renderer::NonDefaultGpu, Some(&[]));
    for variable in nvidia_offload.into_iter().chain(["VK_DRIVER_FILES"]) {
        assert!(set(&environment, variable), "{variable} dropped");
    }
}

/// In the child of [`person_numeric_locale_reaches_program`]: after this
/// process pins its own numbers to `C`, a launch carries `expected` as its
/// `LC_NUMERIC`, none when it is empty, and never the variable that kept it.
fn launch_has_numeric(expected: &str) {
    // SAFETY: the child runs this one test, on the only thread that touches
    // the environment.
    unsafe { big_os_kit::init_c_numeric_locale() };
    let (_, environment) = launched_with(Renderer::DefaultGpu, None);
    let numeric: Vec<_> = environment
        .iter()
        .filter(|line| line.starts_with("LC_NUMERIC="))
        .collect();
    if expected.is_empty() {
        assert!(numeric.is_empty(), "{numeric:?}");
    } else {
        assert_eq!(numeric, [&format!("LC_NUMERIC={expected}")]);
    }
    assert!(
        !environment
            .iter()
            .any(|line| line.starts_with("BIG_PERSON_LC_NUMERIC=")),
        "{environment:?}"
    );
}

/// A program started for the person reads numbers in the person's locale, not
/// in the `C` this process pinned for itself: `LC_ALL` over `LC_NUMERIC`, and
/// none when the person named none.
#[test]
fn person_numeric_locale_reaches_program() {
    const EXPECTED: &str = "BIG_APP_KIT_EXPECTED_NUMERIC";
    if let Some(expected) = std::env::var_os(EXPECTED) {
        launch_has_numeric(&expected.to_string_lossy());
        return;
    }
    let cases: [(&[(&str, &str)], &str); 4] = [
        (&[("LC_NUMERIC", "pt_BR.UTF-8")], "pt_BR.UTF-8"),
        (
            &[("LC_ALL", "de_DE.UTF-8"), ("LC_NUMERIC", "pt_BR.UTF-8")],
            "de_DE.UTF-8",
        ),
        (&[("LANG", "pt_BR.UTF-8")], ""),
        (&[("LC_ALL", ""), ("LANG", "pt_BR.UTF-8")], ""),
    ];
    for (locale, expected) in cases {
        let mut child = big_os_kit::subprocess::BigSubprocessSpec::builder()
            .program(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "own_scope::tests::person_numeric_locale_reaches_program",
                "--quiet",
            ])
            .env_inherit(false)
            .env(EXPECTED, expected);
        for variable in ["PATH", "TMPDIR"] {
            if let Some(value) = std::env::var_os(variable) {
                child = child.env(variable, value);
            }
        }
        for (variable, value) in locale {
            child = child.env(variable, value);
        }
        let out = child
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .run()
            .expect("child test runs");
        assert!(
            out.status.success() && out.stdout_lossy().contains("1 passed"),
            "{locale:?}: {}",
            out.stdout_lossy()
        );
    }
}

/// GIO asks the session bus to start a `DBusActivatable` entry, and the
/// bus's service would run without the chosen environment.
#[test]
fn bus_activated_entry_gets_environment() {
    let (arguments, environment) = launched_from(Renderer::Software, None, true);
    assert_eq!(arguments, ["--flag", "/srv/a b.txt"]);
    assert!(environment.contains(&"LIBGL_ALWAYS_SOFTWARE=1".to_owned()));
}

#[test]
fn chromium_starts_with_page_tree() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("big-page-tree-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("chromium");
    std::fs::write(
        &program,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.part\" && mv \"$0.part\" \"$0.args\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let keys = glib::KeyFile::new();
    keys.load_from_data(
        &format!(
            "[Desktop Entry]\nType=Application\nName=Web\nExec=\"{}\" --incognito %U\n",
            program.display()
        ),
        glib::KeyFileFlags::NONE,
    )
    .unwrap();

    forcing_page_accessibility(&keys)
        .expect("an entry")
        .launch_uris(&["https://example.org/"], gio::AppLaunchContext::NONE)
        .unwrap();
    let args = dir.join("chromium.args");
    let start = std::time::Instant::now();
    while !args.exists() && start.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let args = std::fs::read_to_string(args);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(
        args.unwrap(),
        "--force-renderer-accessibility\n--incognito\nhttps://example.org/\n"
    );
}

/// Run `test` again in a child with `variables`, for what this process reads
/// once: its environment, GIO's index of desktop files.
fn run_in_child(test: &str, variables: &[(&str, &std::ffi::OsStr)]) {
    let mut child = big_os_kit::subprocess::BigSubprocessSpec::builder()
        .program(std::env::current_exe().expect("test binary"))
        .args(["--exact", test, "--quiet"]);
    for (name, value) in variables {
        child = child.env(*name, *value);
    }
    let out = child
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .run()
        .expect("child test runs");
    assert!(
        out.status.success() && out.stdout_lossy().contains("1 passed"),
        "{}",
        out.stdout_lossy()
    );
}

/// A token given for the press reaches the default handler of a URI, also
/// one whose entry the session bus would start, which passes no environment.
#[test]
fn given_token_reaches_bus_activatable_handler() {
    const CHILD: &str = "BIG_APP_KIT_OPEN_URI_DIR";
    let Some(dir) = std::env::var_os(CHILD).map(std::path::PathBuf::from) else {
        let dir = glib::mkdtemp(std::env::temp_dir().join("big-app-kit-open-XXXXXX"))
            .expect("temporary directory");
        let applications = dir.join("data/applications");
        std::fs::create_dir_all(&applications).expect("applications folder");
        std::fs::create_dir_all(dir.join("config")).expect("config folder");
        std::fs::write(
            dir.join("record"),
            "printf '%s %s' \"$XDG_ACTIVATION_TOKEN\" \"$1\" > \"$0.part\" && mv \"$0.part\" \"$0.out\"\n",
        )
        .expect("script");
        std::fs::write(
            applications.join("org.example.BigOpen.desktop"),
            format!(
                "[Desktop Entry]\nType=Application\nName=Open\n\
                 Exec=sh \"{}\" %u\nMimeType=x-scheme-handler/x-big-open;\n\
                 DBusActivatable=true\nStartupNotify=true\n",
                dir.join("record").display()
            ),
        )
        .expect("desktop file");
        std::fs::write(
            dir.join("config/mimeapps.list"),
            "[Default Applications]\nx-scheme-handler/x-big-open=org.example.BigOpen.desktop\n",
        )
        .expect("default handler");
        let child = std::panic::catch_unwind(|| {
            run_in_child(
                "own_scope::tests::given_token_reaches_bus_activatable_handler",
                &[
                    (CHILD, dir.as_os_str()),
                    ("XDG_DATA_HOME", dir.join("data").as_os_str()),
                    ("XDG_CONFIG_HOME", dir.join("config").as_os_str()),
                ],
            );
        });
        std::fs::remove_dir_all(&dir).expect("temporary directory removed");
        if let Err(panic) = child {
            std::panic::resume_unwind(panic);
        }
        return;
    };
    glib::MainContext::default()
        .block_on(open_uri("x-big-open:page", Some("press-token")))
        .expect("handler started");
    let seen = dir.join("record.out");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !seen.exists() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::read_to_string(&seen).expect("the handler ran"),
        "press-token x-big-open:page"
    );
}

/// A Chromium entry whose `Exec` quotes the program still gets the page tree.
#[test]
fn quoted_chromium_gets_page_tree() {
    use std::os::unix::fs::PermissionsExt as _;
    if std::env::var_os("GNOME_ACCESSIBILITY").is_none_or(|value| value != "1") {
        run_in_child(
            "own_scope::tests::quoted_chromium_gets_page_tree",
            &[("GNOME_ACCESSIBILITY", std::ffi::OsStr::new("1"))],
        );
        return;
    }
    let dir = glib::mkdtemp(std::env::temp_dir().join("big-app-kit-chromium-XXXXXX"))
        .expect("temporary directory");
    let program = dir.join("chromium");
    std::fs::write(&program, "#!/bin/sh\n").expect("program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("executable");
    let entry = dir.join("org.example.Web.desktop");
    std::fs::write(
        &entry,
        format!(
            "[Desktop Entry]\nType=Application\nName=Web\nExec=\"{}\" %U\n",
            program.display()
        ),
    )
    .expect("desktop file");
    let application =
        big_testkit::desktop_entry::app_info_from_file(&entry).expect("valid desktop file");
    let accessible = with_page_accessibility(&application).and_then(|copy| copy.commandline());
    std::fs::remove_dir_all(&dir).expect("temporary directory removed");
    let commandline = accessible.expect("a Chromium-family entry");
    assert!(
        commandline
            .to_string_lossy()
            .contains("--force-renderer-accessibility"),
        "{commandline:?}"
    );
}

#[test]
fn entry_without_exec_is_refused() {
    if std::env::var_os("GNOME_ACCESSIBILITY").is_none_or(|value| value != "1") {
        // Only read while programs are asked for their accessibility tree:
        // run this test again in a child that asks.
        run_in_child(
            "own_scope::tests::entry_without_exec_is_refused",
            &[("GNOME_ACCESSIBILITY", std::ffi::OsStr::new("1"))],
        );
        return;
    }
    let keys = glib::KeyFile::new();
    keys.load_from_data(
        "[Desktop Entry]\nType=Application\nName=Broken\n",
        glib::KeyFileFlags::NONE,
    )
    .unwrap();
    let application = big_relm4_components::desktop_entry::app_info_from_keyfile(&keys)
        .expect("GIO keeps an entry without Exec");
    assert!(launch(&application, &[], None, None, None).is_err());
}

/// A launch on a GPU answers before its program starts, so a program that
/// cannot start is told afterwards, not only logged.
#[test]
fn gpu_launch_reports_a_failed_start() {
    use std::os::unix::fs::PermissionsExt as _;
    // GIO takes an entry only while its program exists: it goes before the
    // launch, which then cannot start it.
    let dir = glib::mkdtemp(std::env::temp_dir().join("big-app-kit-failed-XXXXXX"))
        .expect("temporary directory");
    let program = dir.join("gone");
    std::fs::write(&program, "#!/bin/sh\n").expect("program written");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
        .expect("program made runnable");
    let keys = glib::KeyFile::new();
    keys.load_from_data(
        &format!(
            "[Desktop Entry]\nType=Application\nName=Gone\nExec={}\n",
            program.display()
        ),
        glib::KeyFileFlags::NONE,
    )
    .unwrap();
    let application =
        big_relm4_components::desktop_entry::app_info_from_keyfile(&keys).expect("an entry");
    std::fs::remove_dir_all(&dir).expect("temporary directory removed");
    let failure = std::rc::Rc::new(std::cell::Cell::new(false));
    let told = std::rc::Rc::clone(&failure);
    let answered = big_testkit::harness::on_test_context(|context| {
        let answered = launch_on(
            &application,
            &[],
            Renderer::DefaultGpu,
            None,
            None,
            Some(Box::new(move |_| told.set(true))),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !failure.get() && std::time::Instant::now() < deadline {
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        answered
    });
    assert!(answered.is_ok());
    assert!(failure.get(), "the failed start was not reported");
}

#[test]
fn scope_ends_with_session() {
    let request = scope_request("app-bigshell-x-12.scope", 12, "x");
    assert_eq!(request.type_().as_str(), "(ssa(sv)a(sa(sv)))");
    let part_of = request
        .child_value(2)
        .iter()
        .find(|entry| entry.child_value(0).str() == Some("PartOf"))
        .and_then(|entry| entry.child_value(1).as_variant())
        .and_then(|value| value.get::<Vec<String>>());
    assert_eq!(part_of, Some(vec!["graphical-session.target".to_owned()]));
}

/// systemd reads PID 0 as the caller: a scope for it, or for this process,
/// would move the whole desktop process into the program's scope.
#[test]
fn scope_refuses_this_process() {
    let mut child = big_os_kit::subprocess::BigSubprocessSpec::builder()
        .program("true")
        .build()
        .spawn()
        .expect("a child process");
    let other = child.id();
    child.wait().expect("the child's exit");
    assert!(scope_for(0, "x").is_none());
    assert!(scope_for(std::process::id(), "x").is_none());
    let (unit, _) = scope_for(other, "x").expect("a scope for another program");
    assert_eq!(unit, format!("app-bigshell-x-{other}.scope"));
}

/// A launch from a process with neither a token nor a display still gets the
/// renderer's environment.
#[test]
fn launch_without_display_keeps_renderer() {
    let environment: Vec<_> = launch_context(Renderer::Software, None, None)
        .environment()
        .into_iter()
        .map(|line| line.to_string_lossy().into_owned())
        .collect();
    assert!(
        environment
            .iter()
            .any(|line| line == "LIBGL_ALWAYS_SOFTWARE=1"),
        "{environment:?}"
    );
}
