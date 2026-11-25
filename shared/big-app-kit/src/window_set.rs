// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The primary window and every secondary workspace window of an app, and
//! routing a message to the window that owns a workspace.
//!
//! Secondary window state sits behind [`ComponentController::model`], which
//! returns a [`Ref`] guard, so it is reached through closure-scoped accessors
//! ([`BigWindowSet::with_window`], [`BigWindowSet::map_windows`],
//! [`BigWindowSet::with_active_window`]).

use std::collections::BTreeMap;

use relm4::{Component, ComponentController, Controller};

/// A workspace-scoped window state that knows its own workspace identity.
///
/// The identity is the window's own type rather than a fixed one, so this kit
/// does not depend on the workspace crate: one type used in one map dragged
/// 27k lines of workspace UI into every consumer, including the ones that
/// never open a workspace window.
pub trait BigWorkspaceWindow {
    /// Process-local identity of the workspace this window presents.
    type Id: Copy + Ord;

    /// The workspace this window presents.
    fn workspace_id(&self) -> Self::Id;
}

/// A secondary-window Relm4 component that wraps one workspace window state.
///
/// Implemented by each app's secondary component so [`BigWindowSet`] can reach
/// the same window-state type the app roots directly as its primary window.
pub trait BigSecondaryWindow: Component {
    /// The workspace window state this secondary component owns.
    type Window: BigWorkspaceWindow;

    /// Borrow the wrapped window state.
    fn window(&self) -> &Self::Window;
}

/// App-scoped ownership of the primary window state and every secondary
/// window's retained controller, plus the active-window cursor.
pub struct BigWindowSet<P, S>
where
    P: BigWorkspaceWindow,
    S: BigSecondaryWindow<Window = P>,
{
    primary: P,
    secondary: BTreeMap<P::Id, Controller<S>>,
    active: P::Id,
    /// The primary window state outlives its window: an app hosted in a
    /// longer-lived process, or held open by a job, keeps running after it
    /// closes, and a path opened then must not go to a window nobody can see.
    primary_open: bool,
}

impl<P, S> BigWindowSet<P, S>
where
    P: BigWorkspaceWindow,
    S: BigSecondaryWindow<Window = P>,
{
    /// Create a window set owning `primary`, with it as the active window.
    #[must_use]
    pub fn new(primary: P) -> Self {
        let active = primary.workspace_id();
        Self {
            primary,
            secondary: BTreeMap::new(),
            active,
            primary_open: true,
        }
    }

    /// Borrow the primary window state.
    #[must_use]
    pub fn primary(&self) -> &P {
        &self.primary
    }

    /// The primary window's workspace identity.
    #[must_use]
    pub fn primary_id(&self) -> P::Id {
        self.primary.workspace_id()
    }

    /// The workspace whose window is currently active (focused).
    #[must_use]
    pub fn active(&self) -> P::Id {
        self.active
    }

    /// The active window if one is still open; `None` once every window of
    /// the app has closed, when a request has to open a window of its own.
    #[must_use]
    pub fn live_active(&self) -> Option<P::Id> {
        (self.active != self.primary.workspace_id() || self.primary_open).then_some(self.active)
    }

    /// Record that the primary window closed, and move the active cursor to a
    /// secondary window that is still open.
    pub fn close_primary(&mut self) {
        self.primary_open = false;
        if self.active == self.primary.workspace_id()
            && let Some(workspace) = self.secondary.keys().next()
        {
            self.active = *workspace;
        }
    }

    /// Whether the primary window is still open.
    #[must_use]
    pub fn primary_open(&self) -> bool {
        self.primary_open
    }

    /// Whether `workspace` names the primary window or a live secondary window.
    #[must_use]
    pub fn is_known(&self, workspace: P::Id) -> bool {
        workspace == self.primary.workspace_id() || self.secondary.contains_key(&workspace)
    }

    /// Register a launched secondary controller under its own workspace id.
    ///
    /// Returns the workspace identity.
    ///
    /// # Panics
    ///
    /// When the workspace is already registered: app workspace identities are
    /// unique.
    pub fn insert_secondary(&mut self, controller: Controller<S>) -> P::Id {
        let workspace = controller.model().window().workspace_id();
        assert!(
            !self.secondary.contains_key(&workspace),
            "workspace identities are app-scoped"
        );
        self.secondary.insert(workspace, controller);
        workspace
    }

    /// Borrow the retained controller for a live secondary window.
    #[must_use]
    pub fn secondary(&self, workspace: P::Id) -> Option<&Controller<S>> {
        self.secondary.get(&workspace)
    }

    /// Drop a closed secondary window; moves the active cursor to a window
    /// that is still open when the closed window was active.
    pub fn remove_secondary(&mut self, workspace: P::Id) {
        self.secondary.remove(&workspace);
        if self.active == workspace {
            self.active = match self.secondary.keys().next() {
                Some(open) if !self.primary_open => *open,
                _ => self.primary.workspace_id(),
            };
        }
    }

    /// Point the active cursor at `workspace` when it is a known window.
    ///
    /// Returns whether the cursor moved to a known window.
    pub fn set_active(&mut self, workspace: P::Id) -> bool {
        let closed_primary = workspace == self.primary.workspace_id() && !self.primary_open;
        if self.is_known(workspace) && !closed_primary {
            self.active = workspace;
            true
        } else {
            false
        }
    }

    /// The workspace identities of every live secondary window, in id order.
    pub fn secondary_ids(&self) -> impl Iterator<Item = P::Id> + '_ {
        self.secondary.keys().copied()
    }

    /// The retained controller of every live secondary window, in id order.
    ///
    /// Use this to fan an app message out to the secondary windows via
    /// [`ComponentController::emit`].
    pub fn secondary_controllers(&self) -> impl Iterator<Item = &Controller<S>> {
        self.secondary.values()
    }

    /// Run `f` on the window state that owns `workspace`, if it is live.
    ///
    /// Returns `None` when no window owns `workspace`.
    pub fn with_window<R>(&self, workspace: P::Id, f: impl FnOnce(&P) -> R) -> Option<R> {
        if workspace == self.primary.workspace_id() {
            Some(f(&self.primary))
        } else {
            self.secondary.get(&workspace).map(|controller| {
                let guard = controller.model();
                f(guard.window())
            })
        }
    }

    /// Run `f` on the active window state, falling back to the primary window.
    pub fn with_active_window<R>(&self, f: impl FnOnce(&P) -> R) -> R {
        if self.active == self.primary.workspace_id() {
            return f(&self.primary);
        }
        match self.secondary.get(&self.active) {
            Some(controller) => {
                let guard = controller.model();
                f(guard.window())
            }
            None => f(&self.primary),
        }
    }

    /// Collect `f` applied to every window state, primary first then each
    /// secondary in id order.
    pub fn map_windows<T>(&self, mut f: impl FnMut(&P) -> T) -> Vec<T> {
        let mut out = Vec::with_capacity(self.secondary.len() + 1);
        out.push(f(&self.primary));
        for controller in self.secondary.values() {
            let guard = controller.model();
            out.push(f(guard.window()));
        }
        out
    }

    /// Route a per-window effect to the window that owns `workspace`.
    ///
    /// The primary branch calls `on_primary` with the primary window state; a
    /// secondary branch calls `on_secondary` with the retained controller (so
    /// it can read [`BigSecondaryWindow::window`] or
    /// [`ComponentController::emit`]). `payload` moves into whichever branch
    /// runs, so a single owned value can be consumed by either side. Nothing
    /// runs when `workspace` names no live window.
    pub fn dispatch<T>(
        &self,
        workspace: P::Id,
        payload: T,
        on_primary: impl FnOnce(&P, T),
        on_secondary: impl FnOnce(&Controller<S>, T),
    ) {
        if workspace == self.primary.workspace_id() {
            on_primary(&self.primary, payload);
        } else if let Some(controller) = self.secondary.get(&workspace) {
            on_secondary(controller, payload);
        }
    }
}
