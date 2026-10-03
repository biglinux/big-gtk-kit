// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The items, as assistive technology sees them.
//!
//! Each item on screen is an [`ItemProxy`]: a plain GObject that implements
//! `GtkAccessible` — a role, a name, a place in the set, a selected state, bounds
//! — and no widget behind it. The view offers them as its accessible children,
//! the way `GtkStack` offers its pages.
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

use std::cell::{Cell, RefCell};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::ItemProxy)]
    pub struct ItemProxy {
        #[property(name = "accessible-role", override_interface = gtk::Accessible, get, set, builder(gtk::AccessibleRole::GridCell))]
        pub(super) role: Cell<gtk::AccessibleRole>,
        pub(super) view: glib::WeakRef<gtk::Widget>,
        pub(super) position: Cell<u32>,
        pub(super) focused: Cell<bool>,
        pub(super) context: RefCell<Option<gtk::ATContext>>,
        pub(super) next: glib::WeakRef<glib::Object>,
    }

    impl Default for ItemProxy {
        fn default() -> Self {
            Self {
                role: Cell::new(gtk::AccessibleRole::GridCell),
                view: glib::WeakRef::new(),
                position: Cell::new(gtk::INVALID_LIST_POSITION),
                focused: Cell::new(false),
                context: RefCell::new(None),
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
        type ParentType = glib::Object;
        type Interfaces = (gtk::Accessible,);
    }

    #[glib::derived_properties]
    impl ObjectImpl for ItemProxy {}

    impl AccessibleImpl for ItemProxy {
        fn platform_state(&self, state: gtk::AccessiblePlatformState) -> bool {
            match state {
                gtk::AccessiblePlatformState::Focusable => true,
                gtk::AccessiblePlatformState::Focused => self.focused.get(),
                _ => false,
            }
        }

        fn bounds(&self) -> Option<(i32, i32, i32, i32)> {
            let view = self.view.upgrade().and_downcast::<crate::ItemView>()?;
            let rect = view.item_rect(self.position.get())?;
            let (x, y, w, h) = (
                rect.x().round() as i32,
                rect.y().round() as i32,
                rect.width().round() as i32,
                rect.height().round() as i32,
            );
            (w > 0 && h > 0).then_some((x, y, w, h))
        }

        fn at_context(&self) -> Option<gtk::ATContext> {
            if let Some(context) = self.context.borrow().as_ref() {
                return Some(context.clone());
            }
            let view = self.view.upgrade()?;
            let context = gtk::ATContext::create(self.role.get(), &*self.obj(), &view.display())?;
            self.context.replace(Some(context.clone()));
            Some(context)
        }

        fn accessible_parent(&self) -> Option<gtk::Accessible> {
            self.view.upgrade().map(|view| view.upcast())
        }

        fn first_accessible_child(&self) -> Option<gtk::Accessible> {
            None
        }

        fn next_accessible_sibling(&self) -> Option<gtk::Accessible> {
            self.next.upgrade().and_downcast()
        }
    }
}

glib::wrapper! {
    /// One item, for assistive technology.
    pub struct ItemProxy(ObjectSubclass<imp::ItemProxy>)
        @implements gtk::Accessible;
}

impl ItemProxy {
    fn new(view: &gtk::Widget, role: gtk::AccessibleRole) -> Self {
        let proxy: Self = glib::Object::builder()
            .property("accessible-role", role)
            .build();
        proxy.imp().view.set(Some(view));
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
    #[allow(
        clippy::disallowed_methods,
        reason = "integer relations carry no target list"
    )]
    proxy.update_relation(&[
        gtk::accessible::Relation::PosInSet(in_set),
        gtk::accessible::Relation::SetSize(size),
    ]);
}

/// The proxies of one view.
pub(crate) struct Window {
    pool: Vec<ItemProxy>,
    /// The positions the pool covers: `start..end`.
    start: u32,
    end: u32,
    role: gtk::AccessibleRole,
}

impl Window {
    pub(crate) fn new(role: gtk::AccessibleRole) -> Self {
        Self {
            pool: Vec::new(),
            start: 0,
            end: 0,
            role,
        }
    }

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

    /// Cover `range`. Returns the positions whose proxy has to be described again.
    pub(crate) fn cover(&mut self, view: &gtk::Widget, range: std::ops::Range<u32>) -> Vec<u32> {
        let capacity = (range.end - range.start) as usize;
        if capacity != self.pool.len() {
            // A new ring, keeping the proxy of every position both rings
            // cover: a folder smaller than a screen changes size with each
            // file that arrives, and rebuilding threw away the focused item.
            let mut pool: Vec<Option<ItemProxy>> = vec![None; capacity];
            for position in range.clone() {
                pool[position as usize % capacity] = self.get(position).cloned();
            }
            self.pool = pool
                .into_iter()
                .map(|kept| kept.unwrap_or_else(|| ItemProxy::new(view, self.role)))
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
        self.link(view);
        stale
    }

    /// Follow a change of the model: at `position`, `removed` items were
    /// replaced by `added`.
    ///
    /// The proxy of every item that stayed moves with it, to the slot of its
    /// new position, and keeps its object; the slots left over get the proxies
    /// of the items that went, marked for describing again. The window itself
    /// stays where it was until the view covers its new range.
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
        let mut slots: Vec<Option<ItemProxy>> = vec![None; capacity];
        let mut spare = Vec::new();
        for old in self.range() {
            let Some(proxy) = self.get(old).cloned() else {
                continue;
            };
            match crate::layout::remap(old, position, removed, added) {
                Some(new) if self.range().contains(&new) => {
                    proxy.imp().position.set(new);
                    slots[new as usize % capacity] = Some(proxy);
                }
                _ => spare.push(proxy),
            }
        }
        self.pool = slots
            .into_iter()
            .map(|kept| {
                kept.unwrap_or_else(|| {
                    let proxy = spare
                        .pop()
                        .unwrap_or_else(|| ItemProxy::new(view, self.role));
                    proxy.imp().position.set(gtk::INVALID_LIST_POSITION);
                    proxy
                })
            })
            .collect();
        self.end = self.end.min(n_items);
        self.start = self.start.min(self.end);
        self.link(view);
    }

    /// The sibling chain: each proxy to the next position, the last one to the
    /// view's own first child, so real children (a rename popover) stay
    /// reachable behind the items.
    fn link(&self, view: &gtk::Widget) {
        for position in self.range() {
            let next = if position + 1 < self.end {
                self.get(position + 1)
                    .map(|proxy| proxy.clone().upcast::<glib::Object>())
            } else {
                view.first_child().map(Cast::upcast)
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

    pub(crate) fn clear(&mut self) {
        self.pool.clear();
        self.start = 0;
        self.end = 0;
    }
}
