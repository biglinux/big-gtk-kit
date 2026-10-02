# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
"""Each rule on the shape that leaked, and on the shapes it must leave alone.

The bad cases are the leaks measured on the BigLinux desktop, reduced to their
shape; the good cases are the false positives the rules produced along the way.
"""

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

    def test_destroy_that_only_cleans_itself_up_is_fine(self):
        self.assertEqual(
            lint("""
            fn wire(button: &gtk::Button, job: Job) {
                button.connect_destroy(move |_| job.cancel());
            }
        """),
            [],
        )

    def test_set_parent_with_nothing_detaching_it_is_an_error(self):
        self.assertIn(
            ("set-parent-without-unparent", "error"),
            lint("""
            fn tile(menu: &gtk::Popover, tile: &gtk::Button) {
                menu.set_parent(tile);
            }
        """),
        )

    def test_set_parent_detached_here_or_by_a_helper_is_fine(self):
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

    def test_icon_with_an_embedded_bitmap_is_an_error(self):
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

    def test_the_same_name_outside_an_icon_is_fine(self):
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

    def test_clone_of_the_handler_argument_is_not_a_capture(self):
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

    def test_names_in_strings_fields_and_if_let_are_not_captures(self):
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

    def test_rebinding_inside_the_body_still_captures_the_outer_name(self):
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

    def test_followers_kept_as_weak_anchor_and_strong_closure_warn(self):
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

    def test_a_child_placed_by_a_builder_and_a_controller_are_inside(self):
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

    def test_a_field_is_not_the_local_of_the_same_name(self):
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
    """Handlers on objects that outlive every window (hosted desktop, 2026-09-26)."""

    RULE = ("process-object-handler", "warning")

    def test_a_discarded_id_on_the_display_clipboard_or_the_app_is_reported(self):
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

    def test_a_kept_id_and_a_program_s_own_main_are_fine(self):
        self.assertEqual(
            lint("""
            fn install(parent: &gtk::Widget) {
                let handler = parent.clipboard().connect_changed(|_| {});
                keep(handler);
            }
            fn main() {
                app.connect_activate(|_| {});
            }
        """),
            [],
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
