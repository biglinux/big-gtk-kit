// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! A widget drawn alone into a texture.

use gtk::prelude::*;
use gtk::{gdk, graphene};

/// What `widget` paints in its border box (its padding and border included),
/// rendered alone through its window's renderer, so what is behind it does not
/// count. In logical pixels: on a scaled output the texture is smaller than
/// the widget on screen. `None` while the widget is in no realized window, has
/// not drawn a frame yet, or paints nothing.
#[must_use]
pub fn widget_texture(widget: &impl IsA<gtk::Widget>) -> Option<gdk::Texture> {
    let renderer = widget.native()?.renderer()?;
    // The paintable is the widget's border box, wider than `width()` by its
    // padding and border: drawn at any other size it would be scaled.
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let (width, height) = (paintable.intrinsic_width(), paintable.intrinsic_height());
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot.to_node()?;
    // The box as the viewport: without it the texture covers only what was
    // painted, and its origin is not the box's.
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

    /// A button's padding and border are in the texture, at their own size.
    #[test]
    fn texture_covers_the_border_box() {
        big_testkit::harness::require_gtk_test(|| {
            let button = gtk::Button::with_label("Capture");
            let window = gtk::Window::builder().child(&button).build();
            window.present();
            assert!(big_testkit::harness::pump_until(
                || widget_texture(&button).is_some()
            ));
            let texture = widget_texture(&button).expect("a painted button");
            let bounds = button.compute_bounds(&button).expect("bounds");
            assert_eq!(
                (texture.width(), texture.height()),
                (bounds.width() as i32, bounds.height() as i32)
            );
            assert!(texture.width() > button.width(), "the button has padding");
            window.destroy();
        });
    }
}
