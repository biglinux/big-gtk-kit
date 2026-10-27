// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The items, as assistive technology sees them.
//!
//! Each item on screen is an [`ItemProxy`]: a widget that is never drawn and
//! answers for one item as `GtkAccessible` (a role, a name, a place in the set,
//! a selected state, bounds) and as AT-SPI's action, which activates the item
//! the way Enter does. It is a widget because GTK 4.22 offers AT-SPI actions
//! only on widgets, from their own actions. The proxies are the view's first
//! children, ahead of the real ones, and the view offers them as its
//! accessible children.
//!
//! **A window, not the whole model.** The proxies cover the items on screen and a
//! line either side, plus the cursor item: listing every item would make a screen
//! reader realize a context for each of them, and GTK walks the list to find a
//! child's index. "57 of 1129" is still said, from `PosInSet`/`SetSize`.
//!
//! **A ring, so a proxy keeps its identity.** The proxy for position `p` is slot
//! `p % capacity`: scrolling rebinds only the slots whose positions left the
//! window, and the item under the keyboard cursor is the same object for as long
//! as it stays on screen, which is what a screen reader follows. A change of
//! the model moves the proxies with their items ([`Window::shift`]) for the
//! same reason: a file arriving must not take the focused item away.
//!
//! **Bounds are asked, not kept.** A proxy answers its bounds from where the
//! view draws its item at the moment it is asked, so a scroll that leaves the
//! window where it was, or a relayout, cannot leave a screen reader pointing
//! at where the item used to be.

use std::cell::Cell;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;

    pub struct ItemProxy {
        pub(super) position: Cell<u32>,
        pub(super) focused: Cell<bool>,
        pub(super) next: glib::WeakRef<glib::Object>,
    }

    impl Default for ItemProxy {
        fn default() -> Self {
            Self {
                position: Cell::new(gtk::INVALID_LIST_POSITION),
                focused: Cell::new(false),
                next: glib::WeakRef::new(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ItemProxy {
        const NAME: &'static str = "BigItemViewItem";
        // One per module copy of this crate (see `ItemView`).
        const ALLOW_NAME_CONFLICT: bool = true;
        type Type = super::ItemProxy;
        type ParentType = gtk::Widget;
        type Interfaces = (gtk::Accessible,);

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("item");
            klass.set_accessible_role(gtk::AccessibleRole::GridCell);
            // AT-SPI's action on the item: GTK offers a widget's own enabled,
            // parameterless actions.
            klass.install_action("item.activate", None, |proxy, _, _| {
                let position = proxy.position();
                if let Some(view) = proxy.parent().and_downcast::<crate::ItemView>()
                    && position < view.imp().n_items()
                {
                    view.emit_by_name::<()>("activate", &[&position]);
                }
            });
        }
    }

    impl ObjectImpl for ItemProxy {}

    impl WidgetImpl for ItemProxy {}

    impl AccessibleImpl for ItemProxy {
        fn platform_state(&self, state: gtk::AccessiblePlatformState) -> bool {
            match state {
                gtk::AccessiblePlatformState::Focusable => true,
                gtk::AccessiblePlatformState::Focused => self.focused.get(),
                _ => false,
            }
        }

        fn bounds(&self) -> Option<(i32, i32, i32, i32)> {
            let view = self.obj().parent().and_downcast::<crate::ItemView>()?;
            let rect = view.item_rect(self.position.get())?;
            let (x, y, w, h) = (
                rect.x().round() as i32,
                rect.y().round() as i32,
                rect.width().round() as i32,
                rect.height().round() as i32,
            );
            (w > 0 && h > 0).then_some((x, y, w, h))
        }

        fn next_accessible_sibling(&self) -> Option<gtk::Accessible> {
            self.next.upgrade().and_downcast()
        }
    }
}

glib::wrapper! {
    /// One item, for assistive technology.
    pub struct ItemProxy(ObjectSubclass<imp::ItemProxy>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ItemProxy {
    fn new() -> Self {
        let proxy: Self = glib::Object::new();
        proxy.set_can_target(false);
        proxy.set_can_focus(false);
        proxy
    }

    pub(crate) fn position(&self) -> u32 {
        self.imp().position.get()
    }

    fn set_next(&self, next: Option<&glib::Object>) {
        self.imp().next.set(next);
    }

    pub(crate) fn set_focused(&self, focused: bool) {
        if self.imp().focused.replace(focused) != focused {
            self.update_platform_state(gtk::AccessiblePlatformState::Focused);
        }
    }
}

/// The first of `view`'s children that is not a proxy.
pub(crate) fn first_real_child(view: &gtk::Widget) -> Option<gtk::Widget> {
    let mut child = view.first_child();
    while let Some(widget) = child {
        if !widget.is::<ItemProxy>() {
            return Some(widget);
        }
        child = widget.next_sibling();
    }
    None
}

/// What one proxy says.
pub(crate) struct Facts<'a> {
    pub(crate) label: &'a str,
    pub(crate) description: Option<&'a str>,
    pub(crate) selected: bool,
    pub(crate) checked: Option<bool>,
    /// In a tree: the row's depth, and whether it is expanded when it can be.
    pub(crate) tree: Option<(u16, Option<bool>)>,
    pub(crate) set_size: u32,
}

/// Put `facts` on `proxy` for `position`.
pub(crate) fn describe(proxy: &ItemProxy, position: u32, facts: &Facts<'_>) {
    proxy.imp().position.set(position);
    proxy.update_property(&[
        gtk::accessible::Property::Label(facts.label),
        gtk::accessible::Property::Description(facts.description.unwrap_or("")),
    ]);
    proxy.update_state(&[gtk::accessible::State::Selected(Some(facts.selected))]);
    if let Some(checked) = facts.checked {
        proxy.update_state(&[gtk::accessible::State::Checked(if checked {
            gtk::AccessibleTristate::True
        } else {
            gtk::AccessibleTristate::False
        })]);
    }
    if let Some((depth, expanded)) = facts.tree {
        proxy.update_property(&[gtk::accessible::Property::Level(i32::from(depth) + 1)]);
        if let Some(expanded) = expanded {
            proxy.update_state(&[gtk::accessible::State::Expanded(Some(expanded))]);
        }
    }
    let in_set = i32::try_from(position.saturating_add(1)).unwrap_or(i32::MAX);
    let size = i32::try_from(facts.set_size).unwrap_or(i32::MAX);
    // Numbers, not targets: the list `update_relation` leaks exists only for
    // relations that name other accessibles.
    #[expect(
        clippy::disallowed_methods,
        reason = "integer relations carry no target list"
    )]
    proxy.update_relation(&[
        gtk::accessible::Relation::PosInSet(in_set),
        gtk::accessible::Relation::SetSize(size),
    ]);
}

/// What [`Window::cover`] changed. The view parents and unparents the
/// proxies once its window is no longer borrowed, then links them: GTK walks
/// the view's accessible children to each child added or removed, which
/// across a linked ring costs the square of its size.
pub(crate) struct Cover {
    /// The positions whose proxy has to be described again.
    pub(crate) stale: Vec<u32>,
    pub(crate) added: Vec<ItemProxy>,
    pub(crate) gone: Vec<ItemProxy>,
}

/// The proxies of one view.
#[derive(Default)]
pub(crate) struct Window {
    pool: Vec<ItemProxy>,
    /// The positions the pool covers: `start..end`.
    start: u32,
    end: u32,
}

impl Window {
    pub(crate) fn range(&self) -> std::ops::Range<u32> {
        self.start..self.end
    }

    pub(crate) fn first(&self) -> Option<&ItemProxy> {
        self.get(self.start)
    }

    pub(crate) fn get(&self, position: u32) -> Option<&ItemProxy> {
        if position < self.start || position >= self.end || self.pool.is_empty() {
            return None;
        }
        self.pool.get(position as usize % self.pool.len())
    }

    /// Cover `range`, unlinked until [`Self::link`].
    pub(crate) fn cover(&mut self, range: std::ops::Range<u32>) -> Cover {
        let capacity = (range.end - range.start) as usize;
        let mut added = Vec::new();
        let mut gone = Vec::new();
        if capacity != self.pool.len() {
            // A new ring, keeping the proxy of every position both rings
            // cover: a folder smaller than a screen changes size with each
            // file that arrives, and rebuilding threw away the focused item.
            let old_len = self.pool.len();
            let mut old: Vec<Option<ItemProxy>> = std::mem::take(&mut self.pool)
                .into_iter()
                .map(Some)
                .collect();
            let mut pool: Vec<Option<ItemProxy>> = vec![None; capacity];
            for position in range.clone() {
                if old_len > 0 && position >= self.start && position < self.end {
                    pool[position as usize % capacity] = old[position as usize % old_len].take();
                }
            }
            gone.extend(old.into_iter().flatten());
            self.pool = pool
                .into_iter()
                .map(|kept| {
                    let proxy = kept.unwrap_or_else(|| {
                        let proxy = ItemProxy::new();
                        added.push(proxy.clone());
                        proxy
                    });
                    proxy.set_next(None);
                    proxy
                })
                .collect();
        }
        let stale: Vec<u32> = range
            .clone()
            .filter(|position| {
                self.get_slot(*position)
                    .is_none_or(|proxy| proxy.position() != *position)
            })
            .collect();
        self.start = range.start;
        self.end = range.end;
        Cover { stale, added, gone }
    }

    /// Follow a change of the model: at `position`, `removed` items were
    /// replaced by `added`.
    ///
    /// The proxy of every item that stayed moves with it, to the slot of its
    /// new position, and keeps its object; the slots left over get the other
    /// proxies, marked for describing again. The window itself stays where it
    /// was until the view covers its new range.
    pub(crate) fn shift(
        &mut self,
        view: &gtk::Widget,
        position: u32,
        removed: u32,
        added: u32,
        n_items: u32,
    ) {
        let capacity = self.pool.len();
        if capacity == 0 {
            return;
        }
        let mut old: Vec<Option<ItemProxy>> = std::mem::take(&mut self.pool)
            .into_iter()
            .map(Some)
            .collect();
        let mut slots: Vec<Option<ItemProxy>> = vec![None; capacity];
        for before in self.range() {
            let Some(proxy) = old[before as usize % capacity].take() else {
                continue;
            };
            match crate::layout::remap(before, position, removed, added) {
                Some(new) if self.range().contains(&new) => {
                    proxy.imp().position.set(new);
                    slots[new as usize % capacity] = Some(proxy);
                }
                _ => old.push(Some(proxy)),
            }
        }
        // As many proxies as slots: every slot left empty takes one of these.
        let mut spare = old.into_iter().flatten();
        self.pool = slots
            .into_iter()
            .filter_map(|kept| {
                kept.or_else(|| {
                    let proxy = spare.next()?;
                    proxy.imp().position.set(gtk::INVALID_LIST_POSITION);
                    Some(proxy)
                })
            })
            .collect();
        self.end = self.end.min(n_items);
        self.start = self.start.min(self.end);
        self.link(view);
    }

    /// The sibling chain: each proxy to the next position, the last one to the
    /// view's first real child, so those (a rename popover) stay reachable
    /// behind the items.
    pub(crate) fn link(&self, view: &gtk::Widget) {
        for position in self.range() {
            let next = if position + 1 < self.end {
                self.get(position + 1)
                    .map(|proxy| proxy.clone().upcast::<glib::Object>())
            } else {
                first_real_child(view).map(Cast::upcast)
            };
            if let Some(proxy) = self.get(position) {
                proxy.set_next(next.as_ref());
            }
        }
    }

    fn get_slot(&self, position: u32) -> Option<&ItemProxy> {
        if self.pool.is_empty() {
            return None;
        }
        self.pool.get(position as usize % self.pool.len())
    }

    /// Drop every proxy. Returns them, for the view to unparent.
    pub(crate) fn clear(&mut self) -> Vec<ItemProxy> {
        self.start = 0;
        self.end = 0;
        std::mem::take(&mut self.pool)
    }
}
