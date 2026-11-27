# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
"""Each rule on the shape that leaked, and on the shapes it must leave alone.

The bad cases are the leaks measured on the BigLinux desktop, reduced to their
shape; the good cases are look-alike shapes that do not leak.
"""

import contextlib
import io
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import leak_lint


def lint(code: str, icons=frozenset()):
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "case.rs"
        path.write_text(textwrap.dedent(code))
        return [(f.rule, f.severity) for f in leak_lint.check([path], set(icons))]


class ErrorRules(unittest.TestCase):
    def test_unparent_inside_closed_is_an_error(self):
        self.assertIn(
            ("closed-unparent", "error"),
            lint("""
            fn show(p: &gtk::Popover) {
                p.connect_closed(|p| p.unparent());
            }
        """),
        )
        self.assertIn(
            ("closed-unparent", "error"),
            lint("""
            fn show(p: &gtk::Popover) {
                p.connect_closed(gtk::Popover::unparent);
            }
        """),
        )

    def test_unparent_from_an_idle_is_fine(self):
        self.assertEqual(
            lint("""
            fn show(p: &gtk::Popover) {
                p.connect_closed(|p| {
                    let p = p.clone();
                    glib::idle_add_local_once(move || p.unparent());
                });
            }
        """),
            [],
        )

    def test_window_close_by_destroy_is_an_error(self):
        self.assertIn(
            ("window-close-by-destroy", "error"),
            lint("""
            fn wire(runtime: &State, owner: &Sender) {
                let owner = owner.clone();
                runtime.workspace_window().connect_destroy(move |_| {
                    let _ = owner.send(Input::SecondaryClosed);
                });
            }
        """),
        )

    def test_self_cleaning_destroy_is_fine(self):
        self.assertEqual(
            lint("""
            fn wire(button: &gtk::Button, job: Job) {
                button.connect_destroy(move |_| job.cancel());
            }
        """),
            [],
        )

    def test_undetached_set_parent_is_an_error(self):
        self.assertIn(
            ("set-parent-without-unparent", "error"),
            lint("""
            fn tile(menu: &gtk::Popover, tile: &gtk::Button) {
                menu.set_parent(tile);
            }
        """),
        )

    def test_detached_set_parent_is_fine(self):
        self.assertEqual(
            lint("""
            fn tile(menu: &gtk::Popover, tile: &gtk::Button) {
                menu.set_parent(tile);
                let weak = menu.downgrade();
                tile.connect_destroy(move |_| {
                    if let Some(menu) = weak.upgrade() { menu.unparent(); }
                });
            }
        """),
            [],
        )
        self.assertEqual(
            lint("""
            fn tile(menu: &gtk::Popover, tile: &gtk::Button) {
                menu.set_parent(tile);
                autodestroy_on_close(menu);
            }
        """),
            [],
        )

    def test_teardown_reading_the_child_is_an_error(self):
        self.assertIn(
            ("teardown-reads-child", "error"),
            lint("""
            fn factory(f: &gtk::SignalListItemFactory, parts: Parts) {
                f.connect_teardown(move |_, object| {
                    let Some(row) = object.downcast_ref::<gtk::ListItem>().and_then(gtk::ListItem::child) else { return; };
                    parts.borrow_mut().remove(&row);
                });
            }
        """),
        )

    def test_embedded_bitmap_icon_is_an_error(self):
        self.assertIn(
            ("embedded-image-icon", "error"),
            lint(
                """
            fn page() -> Page {
                Page { icon: "preferences-desktop-appearance-symbolic" }
            }
        """,
                icons={"preferences-desktop-appearance-symbolic"},
            ),
        )

    def test_name_outside_icon_is_fine(self):
        self.assertEqual(
            lint(
                """
            fn kind(name: &str) -> Kind {
                match name { "audio-headset" => Kind::Headset, _ => Kind::Other }
            }
        """,
                icons={"audio-headset"},
            ),
            [],
        )


class WarningRules(unittest.TestCase):
    def test_handler_holding_its_row_warns(self):
        self.assertIn(
            ("handler-holds-owner", "warning"),
            lint("""
            fn relief(runtime: &Runtime) -> adw::ActionRow {
                let relief = adw::ActionRow::new();
                let scale = gtk::Scale::new();
                {
                    let row = relief.clone();
                    scale.connect_value_changed(move |scale| {
                        row.set_subtitle(&format!("{}", scale.value()));
                    });
                }
                relief.add_suffix(&scale);
                relief
            }
        """),
        )

    def test_handler_holding_itself_warns(self):
        self.assertIn(
            ("handler-holds-owner", "warning"),
            lint("""
            fn picture(row: &adw::ActionRow) {
                let chosen = adw::ActionRow::new();
                {
                    let row = chosen.clone();
                    chosen.connect_activated(move |_| row.set_subtitle("x"));
                }
            }
        """),
        )

    def test_weak_holds_are_fine(self):
        self.assertEqual(
            lint("""
            fn relief() -> adw::ActionRow {
                let relief = adw::ActionRow::new();
                let scale = gtk::Scale::new();
                let row = relief.downgrade();
                scale.connect_value_changed(move |scale| {
                    if let Some(row) = row.upgrade() { row.set_subtitle("x"); }
                });
                relief.add_suffix(&scale);
                relief
            }
        """),
            [],
        )

    def test_handler_argument_clone_captures_nothing(self):
        self.assertEqual(
            lint("""
            fn show(popover: &gtk::Popover) {
                popover.connect_closed(|popover| {
                    let popover = popover.clone();
                    glib::idle_add_local_once(move || popover.unparent());
                });
            }
        """),
            [],
        )

    def test_strings_and_fields_capture_nothing(self):
        self.assertEqual(
            lint("""
            fn dialog(entry: &gtk::Entry) {
                let dialog = adw::AlertDialog::new(None, None);
                dialog.set_extra_child(Some(entry));
                let weak = dialog.downgrade();
                entry.connect_changed(move |entry| {
                    if let Some(dialog) = weak.upgrade() {
                        dialog.set_response_enabled("dialog entry", true);
                    }
                    let _ = Msg { entry: 1 };
                });
            }
        """),
            [],
        )

    def test_glib_clone_weak_is_fine(self):
        self.assertEqual(
            lint("""
            fn calc() {
                let day = gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 1.0, 0.0);
                let year = gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 1.0, 0.0);
                day.connect_value_changed(glib::clone!(#[weak] day, #[weak] year, move |_| refresh(&day, &year)));
                year.connect_value_changed(glib::clone!(#[weak] day, #[weak] year, move |_| refresh(&day, &year)));
            }
        """),
            [],
        )

    def test_siblings_holding_each_other_warn(self):
        self.assertIn(
            ("mutual-siblings", "warning"),
            lint("""
            fn picker() -> gtk::Box {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                let choose = gtk::Button::new();
                let clear = gtk::Button::new();
                {
                    let clear = clear.clone();
                    choose.connect_clicked(move |_| clear.set_visible(true));
                }
                {
                    let choose = choose.clone();
                    clear.connect_clicked(move |_| choose.set_label("x"));
                }
                row.append(&choose);
                row.append(&clear);
                row
            }
        """),
        )

    def test_rebinding_captures_outer_name(self):
        self.assertIn(
            ("mutual-siblings", "warning"),
            lint("""
            fn picker() {
                let choose = gtk::Button::new();
                let clear = gtk::Button::new();
                {
                    let clear = clear.clone();
                    choose.connect_clicked(move |choose| {
                        let (clear, choose) = (clear.clone(), choose.clone());
                        later(move || { clear.show(); choose.show(); });
                    });
                }
                {
                    let choose = choose.clone();
                    clear.connect_clicked(move |_| choose.set_label("x"));
                }
            }
        """),
        )

    def test_field_target_names_no_local(self):
        # `picker.open` is the button, `open` the closure its handler calls.
        self.assertEqual(
            lint("""
            fn connect(picker: &Rc<Picker>) {
                let open = {
                    let picker = Rc::downgrade(picker);
                    move |name: Option<String>| {
                        let Some(picker) = picker.upgrade() else {
                            return;
                        };
                        picker.show(name);
                    }
                };
                let open = Rc::new(open);
                let button_open = Rc::clone(&open);
                picker.open.connect_clicked(move |_| button_open(None));
            }
        """),
            [],
        )

    def test_flag_set_pipes_bind_nothing(self):
        # The `|` of a pattern and of a flag set paired with the inner
        # closure's `|popover|`.
        self.assertEqual(
            lint("""
            fn let_the_keyboard_in(popover: &gtk::Popover) {
                let space = gtk::EventControllerKey::new();
                let weak = popover.downgrade();
                space.connect_key_pressed(move |_, key, _, state| {
                    if !matches!(key, Key::space | Key::KP_Space)
                        || state.intersects(
                            ModifierType::CONTROL_MASK
                                | ModifierType::ALT_MASK
                                | ModifierType::SUPER_MASK,
                        )
                    {
                        return Proceed;
                    }
                    let focus = weak.upgrade().and_then(|popover| {
                        popover
                            .root()
                            .and_then(|root| root.focus())
                            .filter(|focus| focus.is_ancestor(&popover))
                    });
                    match focus {
                        Some(focus) if focus.activate() => Stop,
                        _ => Proceed,
                    }
                });
                popover.add_controller(space);
            }
        """),
            [],
        )

    def test_match_arm_binding_shadows_outer(self):
        # The arm's `widget` is `controller.widget()`, not the function's `widget`.
        self.assertEqual(
            lint("""
            fn attach(widget: &gtk::Widget, open: impl Fn(&gtk::Widget) -> bool + 'static) {
                let widget = widget.as_ref();
                let keys = gtk::EventControllerKey::new();
                keys.connect_key_pressed(
                    move |controller, key, _, modifiers| match controller.widget() {
                        Some(widget) if is_menu_key(key, modifiers) && open(&widget, None) => {
                            gtk::glib::Propagation::Stop
                        }
                        _ => gtk::glib::Propagation::Proceed,
                    },
                );
                widget.add_controller(keys);
            }
        """),
            [],
        )
        # A guard reads the outer name: it is captured.
        self.assertIn(
            ("handler-holds-owner", "warning"),
            lint("""
            fn attach(widget: &gtk::Widget) {
                let widget = widget.clone();
                let keys = gtk::EventControllerKey::new();
                keys.connect_key_pressed(move |controller, key, _, _| match key {
                    Key::Menu if widget.is_visible() => Stop,
                    _ => Proceed,
                });
                widget.add_controller(keys);
            }
        """),
        )

    def test_arms_found_past_braced_scrutinee(self):
        # The first `{` after `match` may be a block scrutinee or a closure's body.
        case = """
            fn attach(widget: &gtk::Widget) {
                let widget = widget.clone();
                let keys = gtk::EventControllerKey::new();
                keys.connect_key_pressed(move |controller, _, _, _| match SCRUTINEE {
                    Some(widget) => {
                        widget.grab_focus();
                        Stop
                    }
                    None => Proceed,
                });
                widget.add_controller(keys);
            }
        """
        for scrutinee in (
            "{ controller.widget() }",
            "controller.widget().filter(|w| { w.is_visible() })",
        ):
            with self.subTest(scrutinee=scrutinee):
                self.assertEqual(lint(case.replace("SCRUTINEE", scrutinee)), [])

    def test_match_without_arms_read(self):
        for scrutinee in ("never {}", "unsafe { read() } { _ => () }"):
            with self.subTest(scrutinee=scrutinee):
                self.assertEqual(
                    lint(f"""
                    fn attach(widget: &gtk::Widget) {{
                        let button = gtk::Button::new();
                        button.connect_clicked(move |_| match {scrutinee});
                    }}
                    """),
                    [],
                )

    def test_arm_binding_ends_with_its_arm(self):
        # The second arm reads the function's `widget`: it is captured.
        self.assertIn(
            ("handler-holds-owner", "warning"),
            lint("""
            fn attach(widget: &gtk::Widget) {
                let widget = widget.clone();
                let keys = gtk::EventControllerKey::new();
                keys.connect_key_pressed(move |controller, _, _, _| match controller.widget() {
                    Some(widget) => {
                        widget.grab_focus();
                        Stop
                    }
                    None => {
                        widget.grab_focus();
                        Proceed
                    }
                });
                widget.add_controller(keys);
            }
        """),
        )

    def test_field_label_binds_nothing(self):
        # In `Event { widget: w }` the arm binds `w`; its `widget` is the function's.
        case = """
            fn attach(widget: &gtk::Widget) {
                let widget = widget.clone();
                let keys = gtk::EventControllerKey::new();
                keys.connect_key_pressed(move |controller, _, _, _| match controller.event() {
                    Some(PATTERN) => {
                        widget.grab_focus();
                        Stop
                    }
                    _ => Proceed,
                });
                widget.add_controller(keys);
            }
        """
        for pattern, expected in (
            ("Event { widget: w }", [("handler-holds-owner", "warning")]),
            ("Event { widget }", []),
            ("Event { source: widget }", []),
            ("Event { source: Some(widget), .. }", []),
        ):
            with self.subTest(pattern=pattern):
                self.assertEqual(lint(case.replace("PATTERN", pattern)), expected)

    def test_inner_closure_binding_ends_with_it(self):
        # After the inner closure, `widget` is the function's again: it is captured.
        case = """
            fn attach(widget: &gtk::Widget) {
                let widget = widget.clone();
                let keys = gtk::EventControllerKey::new();
                keys.connect_key_pressed(move |controller, _, _, _| {
                    controller.widget().map(INNER);
                    widget.grab_focus();
                    Stop
                });
                widget.add_controller(keys);
            }
        """
        for inner in (
            "|widget| widget.grab_focus()",
            "|widget| { widget.grab_focus() }",
        ):
            with self.subTest(inner=inner):
                self.assertIn(
                    ("handler-holds-owner", "warning"),
                    lint(case.replace("INNER", inner)),
                )

    def test_weak_anchor_strong_closure_warns(self):
        self.assertIn(
            ("lazy-followers", "warning"),
            lint("""
            type DeviceView = (glib::WeakRef<gtk::Widget>, Rc<dyn Fn(bool)>);
        """),
        )


class HeldThroughShapes(unittest.TestCase):
    """Cycles that hide behind an `Rc`, a builder or a controller (editor quick open)."""

    HOLDS = ("handler-holds-owner", "warning")

    def test_a_shared_closure_holds_what_it_captured(self):
        self.assertIn(
            self.HOLDS,
            lint("""
            fn present() {
                let entry = gtk::SearchEntry::new();
                let refresh = {
                    let entry = entry.clone();
                    move || entry.set_text("")
                };
                let refresh = std::rc::Rc::new(refresh);
                entry.connect_search_changed({
                    let refresh = std::rc::Rc::clone(&refresh);
                    move |_| refresh()
                });
            }
        """),
        )

    def test_builder_child_and_controller_inside(self):
        self.assertIn(
            self.HOLDS,
            lint("""
            fn present() {
                let entry = gtk::SearchEntry::new();
                let dialog = adw::Dialog::builder().child(&entry).build();
                let keys = gtk::EventControllerKey::new();
                keys.connect_key_pressed({
                    let dialog = dialog.clone();
                    move |_, _, _, _| { dialog.close(); gtk::glib::Propagation::Stop }
                });
                entry.add_controller(keys);
            }
        """),
        )

    def test_weak_captures_and_their_copies_are_fine(self):
        self.assertEqual(
            lint("""
            fn present() {
                let entry = gtk::SearchEntry::new();
                let dialog = adw::Dialog::builder().child(&entry).build();
                let weak = dialog.downgrade();
                let keys = gtk::EventControllerKey::new();
                keys.connect_key_pressed({
                    let dialog = weak.clone();
                    move |_, _, _, _| {
                        if let Some(dialog) = dialog.upgrade() && dialog.is_visible() {
                            dialog.close();
                        }
                        gtk::glib::Propagation::Stop
                    }
                });
                entry.add_controller(keys);
            }
        """),
            [],
        )

    def test_only_a_whole_argument_is_placed(self):
        # `list.append(&row(&dialog))` puts the row in the list, not the dialog.
        self.assertEqual(
            lint("""
            fn present(dialog: &adw::Dialog) {
                let search = gtk::SearchEntry::new();
                let list = gtk::ListBox::new();
                list.append(&conversation_row(&dialog));
                dialog.set_child(Some(&search));
                search.connect_search_changed({
                    let list = list.clone();
                    move |_| list.remove_all()
                });
            }
        """),
            [],
        )

    def test_field_differs_from_same_local(self):
        self.assertEqual(
            lint("""
            fn present(properties: &Properties) {
                let group = adw::PreferencesGroup::new();
                let apply = gtk::Button::new();
                group.add(&apply);
                let initial_group = properties.group.clone();
                apply.connect_clicked(move |_| println!("{initial_group}"));
            }
        """),
            [],
        )


class ProcessObjectHandlers(unittest.TestCase):
    """Handlers on objects that outlive every window."""

    RULE = ("process-object-handler", "warning")

    def test_discarded_process_handler_is_reported(self):
        self.assertIn(
            self.RULE,
            lint("""
            fn install(parent: &gtk::Widget) {
                parent.clipboard().connect_changed(|_| {});
            }
        """),
        )
        self.assertIn(
            self.RULE,
            lint("""
            fn init(init: Init) {
                init.app.connect_shutdown(|_| {});
            }
        """),
        )
        self.assertIn(
            self.RULE,
            lint("""
            fn count_once_online() {
                let _ = gio::NetworkMonitor::default().connect_network_changed(|_, _| {});
            }
        """),
        )

    def test_kept_id_and_main_are_fine(self):
        self.assertEqual(
            lint("""
            fn install(parent: &gtk::Widget) {
                let handler = parent.clipboard().connect_changed(|_| {});
                keep(handler);
            }
            fn main() {
                app.connect_activate(|_| {});
            }
            fn count_once_online(self: &Rc<Self>) {
                let handler =
                    gio::NetworkMonitor::default().connect_network_changed(move |_, _| {});
                self.once_online.replace(Some(handler));
            }
            fn init(sender: Sender) -> Model {
                Model {
                    dark_mode_handler: Some({
                        let input = sender.clone();
                        adw::StyleManager::default().connect_dark_notify(move |_| input.send(()))
                    }),
                }
            }
        """),
            [],
        )


class RepeatingSources(unittest.TestCase):
    """A repeating GLib source whose id is dropped runs until its closure breaks."""

    RULE = ("repeating-source-without-weak-exit", "warning")

    def test_unbounded_dropped_source_warns(self):
        self.assertIn(
            self.RULE,
            lint("""
            fn watch(label: &gtk::Label) {
                let label = label.clone();
                glib::timeout_add_seconds_local(600, move || {
                    label.set_text("checked");
                    glib::ControlFlow::Continue
                });
            }
        """),
        )
        self.assertIn(
            self.RULE,
            lint("""
            fn watch(label: gtk::Label) {
                let mut tick = move || {
                    label.set_text("checked");
                    glib::ControlFlow::Continue
                };
                let _ = gtk::glib::idle_add_local(tick);
            }
        """),
        )

    def test_typed_local_callback_warns(self):
        self.assertIn(
            self.RULE,
            lint("""
            fn watch(label: gtk::Label) {
                let tick: Box<dyn FnMut() -> glib::ControlFlow> = Box::new(move || {
                    label.set_text("checked");
                    glib::ControlFlow::Continue
                });
                glib::idle_add_local(tick);
            }
        """),
        )

    def test_break_on_upgrade_success_warns(self):
        # A failed upgrade continues: the source outlives its target.
        case = """
            fn watch(label: &gtk::Label) {
                let weak = label.downgrade();
                glib::timeout_add_local(SECOND, move || match weak.upgrade() {
                    Some(_) => glib::ControlFlow::Break,
                    None => ARM
                });
            }
        """
        for arm in (
            "glib::ControlFlow::Continue,",
            "{ glib::ControlFlow::Continue }",
            '{ log("gone"); glib::ControlFlow::Continue }',
            "{ if retry() { return glib::ControlFlow::Continue; } glib::ControlFlow::Break }",
        ):
            with self.subTest(arm=arm):
                self.assertIn(self.RULE, lint(case.replace("ARM", arm)))

    def test_bounded_sources_are_fine(self):
        self.assertEqual(
            lint("""
            fn keep_age_current(time: &gtk::Label) {
                let clock = time.downgrade();
                glib::timeout_add_seconds_local(60, move || {
                    clock.upgrade().map_or(glib::ControlFlow::Break, |time| {
                        time.set_text("now");
                        glib::ControlFlow::Continue
                    })
                });
            }
            fn kept(label: gtk::Label) -> glib::SourceId {
                let id =
                    glib::timeout_add_local(SECOND, move || {
                    label.set_text("x");
                    glib::ControlFlow::Continue
                });
                id
            }
            fn returned(label: gtk::Label) -> glib::SourceId {
                glib::timeout_add_local(SECOND, move || {
                    label.set_text("x");
                    glib::ControlFlow::Continue
                })
            }
            fn worded(button: gtk::Button) {
                glib::idle_add_local(move || {
                    button.set_label("Continue");
                    glib::ControlFlow::Break
                });
            }
            fn weak_exits(label: &gtk::Label) {
                let weak = label.downgrade();
                glib::timeout_add_local(SECOND, move || match weak.upgrade() {
                    Some(label) => {
                        label.set_text("x");
                        glib::ControlFlow::Continue
                    }
                    None => glib::ControlFlow::Break,
                });
                let weak = label.downgrade();
                glib::timeout_add_local(SECOND, move || match weak.upgrade() {
                    Some(label) => {
                        label.set_text("x");
                        glib::ControlFlow::Continue
                    }
                    None => { glib::ControlFlow::Break }
                });
                let weak = label.downgrade();
                glib::timeout_add_local(SECOND, move || {
                    let Some(label) = weak.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    label.set_text("x");
                    glib::ControlFlow::Continue
                });
                glib::timeout_add_local(
                    SECOND,
                    glib::clone!(
                        #[weak]
                        label,
                        #[upgrade_or]
                        glib::ControlFlow::Break,
                        move || {
                            label.set_text("x");
                            glib::ControlFlow::Continue
                        }
                    ),
                );
            }
            fn once(label: gtk::Label) {
                glib::timeout_add_local_once(SECOND, move || label.set_text("x"));
                glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
                    label.grab_focus();
                    glib::ControlFlow::Break
                });
                glib::idle_add_local(tick);
            }
        """),
            [],
        )


class Strict(unittest.TestCase):
    def run_main(self, code: str, *flags: str) -> int:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "case.rs"
            path.write_text(textwrap.dedent(code))
            with (
                contextlib.redirect_stdout(io.StringIO()),
                contextlib.redirect_stderr(io.StringIO()),
            ):
                return leak_lint.main([*flags, "--no-icons", str(path)])

    def test_strict_fails_on_warnings(self):
        holds_itself = """
            fn row() {
                let row = gtk::Button::new();
                let held = row.clone();
                row.connect_clicked(move |_| held.set_label("x"));
            }
        """
        self.assertEqual(self.run_main(holds_itself), 0)
        self.assertEqual(self.run_main(holds_itself, "--strict"), 1)
        waived = holds_itself.replace(
            "row.connect_clicked",
            "// leak-guard: allow(handler-holds-owner) lives as long as the app\n"
            "                row.connect_clicked",
        )
        self.assertEqual(self.run_main(waived, "--strict"), 0)

    def test_unusable_path_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "case.rs"
            source.write_text("fn main() {}\n")
            notes = Path(tmp) / "README.md"
            notes.write_text("notes\n")
            for extra in (Path(tmp) / "missing", notes):
                with (
                    self.subTest(extra=extra.name),
                    contextlib.redirect_stdout(io.StringIO()),
                    contextlib.redirect_stderr(io.StringIO()),
                ):
                    self.assertEqual(
                        leak_lint.main(
                            ["--strict", "--no-icons", str(source), str(extra)]
                        ),
                        2,
                    )


class Suppression(unittest.TestCase):
    def test_allow_with_a_reason_suppresses(self):
        self.assertEqual(
            lint("""
            fn tile(menu: &gtk::Popover, anchor: &gtk::Overlay) {
                // leak-guard: allow(set-parent-without-unparent) the parent is a gtk::Overlay
                menu.set_parent(anchor);
            }
        """),
            [],
        )

    def test_allow_without_a_reason_is_reported(self):
        self.assertIn(
            ("allow-without-reason", "error"),
            lint("""
            fn tile(menu: &gtk::Popover, anchor: &gtk::Overlay) {
                // leak-guard: allow(set-parent-without-unparent)
                menu.set_parent(anchor);
            }
        """),
        )

    def test_test_code_is_skipped(self):
        self.assertEqual(
            lint("""
            #[cfg(test)]
            mod tests {
                fn probe(p: &gtk::Popover) { p.connect_closed(|p| p.unparent()); }
            }
        """),
            [],
        )


if __name__ == "__main__":
    unittest.main()
