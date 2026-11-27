// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A widget drawn alone into a texture.

use gtk::prelude::*;
use gtk::{gdk, graphene};

/// What `widget` paints in its own box, rendered alone through its window's
/// renderer, so what is behind it does not count. `None` while the widget is in
/// no realized window, or paints nothing.
#[must_use]
pub fn widget_texture(widget: &impl IsA<gtk::Widget>) -> Option<gdk::Texture> {
    let (width, height) = (widget.width(), widget.height());
    let renderer = widget.native()?.renderer()?;
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(widget)).snapshot(
        &snapshot,
        f64::from(width),
        f64::from(height),
    );
    let node = snapshot.to_node()?;
    // The widget's box as the viewport: without it the texture covers only
    // what was painted, and its origin is not the widget's.
    let viewport = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    Some(renderer.render_texture(&node, Some(&viewport)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrealized_widget_has_no_texture() {
        big_testkit::harness::require_gtk_test(|| {
            assert!(widget_texture(&gtk::Label::new(Some("x"))).is_none());
        });
    }

    #[test]
    fn texture_covers_the_widget_box() {
        big_testkit::harness::require_gtk_test(|| {
            let label = gtk::Label::new(Some("Capture"));
            let window = gtk::Window::builder().child(&label).build();
            window.present();
            assert!(big_testkit::harness::pump_until(|| label.width() > 0));
            let texture = widget_texture(&label).expect("a realized label");
            assert_eq!(
                (texture.width(), texture.height()),
                (label.width(), label.height())
            );
            window.destroy();
        });
    }
}
