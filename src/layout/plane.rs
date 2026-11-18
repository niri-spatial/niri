//! Plane-local workspace storage.
//!
//! A plane is one depth layer on a monitor. Its workspaces form the existing
//! vertical workspace sequence, and the active workspace index is local to
//! that plane.
//!
//! This module deliberately does not change Monitor or rendering yet. It
//! establishes and tests the plane-level invariant before the layout is
//! migrated to use it.

use std::fmt;

/// Stable identity for a depth plane.
///
/// The numeric value is an identity, not a Z coordinate. Plane ordering and
/// the currently selected plane are managed by the owning monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlaneId(u64);

impl PlaneId {
    /// Creates a plane ID.
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// Returns the underlying ID.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Errors returned when a plane's workspace invariant would be violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneError {
    /// A plane must always contain at least one workspace.
    EmptyPlane,

    /// The requested active workspace index is outside the plane.
    ActiveWorkspaceOutOfBounds {
        index: usize,
        len: usize,
    },

    /// The requested workspace index is outside the plane.
    WorkspaceIndexOutOfBounds {
        index: usize,
        len: usize,
    },

    /// The last workspace in a plane cannot be removed.
    CannotRemoveLastWorkspace,
}

impl fmt::Display for PlaneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPlane => write!(f, "a plane must contain at least one workspace"),
            Self::ActiveWorkspaceOutOfBounds { index, len } => {
                write!(f, "active workspace index {index} is out of bounds for {len} workspaces")
            }
            Self::WorkspaceIndexOutOfBounds { index, len } => {
                write!(f, "workspace index {index} is out of bounds for {len} workspaces")
            }
            Self::CannotRemoveLastWorkspace => {
                write!(f, "cannot remove the last workspace from a plane")
            }
        }
    }
}

impl std::error::Error for PlaneError {}

/// A depth plane containing a vertical sequence of workspaces.
///
/// The active index is local to this plane. The plane always contains at
/// least one workspace, matching Niri's existing per-monitor invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePlane<T> {
    id: PlaneId,
    workspaces: Vec<T>,
    active_workspace_idx: usize,
}

impl<T> WorkspacePlane<T> {
    /// Creates a plane with an existing sequence of workspaces.
    pub fn new(
        id: PlaneId,
        workspaces: Vec<T>,
        active_workspace_idx: usize,
    ) -> Result<Self, PlaneError> {
        if workspaces.is_empty() {
            return Err(PlaneError::EmptyPlane);
        }

        if active_workspace_idx >= workspaces.len() {
            return Err(PlaneError::ActiveWorkspaceOutOfBounds {
                index: active_workspace_idx,
                len: workspaces.len(),
            });
        }

        Ok(Self {
            id,
            workspaces,
            active_workspace_idx,
        })
    }

    /// Returns this plane's stable identity.
    pub const fn id(&self) -> PlaneId {
        self.id
    }

    /// Returns the workspaces in vertical order.
    pub fn workspaces(&self) -> &[T] {
        &self.workspaces
    }

    /// Returns mutable access to the workspaces in vertical order.
    pub fn workspaces_mut(&mut self) -> &mut [T] {
        &mut self.workspaces
    }

    /// Returns the number of workspaces in this plane.
    pub fn len(&self) -> usize {
        self.workspaces.len()
    }

    /// Returns the active workspace's plane-local index.
    pub const fn active_workspace_idx(&self) -> usize {
        self.active_workspace_idx
    }

    /// Returns the active workspace.
    pub fn active_workspace(&self) -> &T {
        &self.workspaces[self.active_workspace_idx]
    }

    /// Returns mutable access to the active workspace.
    pub fn active_workspace_mut(&mut self) -> &mut T {
        &mut self.workspaces[self.active_workspace_idx]
    }

    /// Changes the active workspace within this plane.
    pub fn activate_workspace(&mut self, index: usize) -> Result<(), PlaneError> {
        if index >= self.workspaces.len() {
            return Err(PlaneError::WorkspaceIndexOutOfBounds {
                index,
                len: self.workspaces.len(),
            });
        }

        self.active_workspace_idx = index;
        Ok(())
    }

    /// Inserts a workspace at a vertical index.
    ///
    /// Inserting at or before the active index keeps the same workspace
    /// active by shifting its index.
    pub fn insert_workspace(&mut self, index: usize, workspace: T) -> Result<(), PlaneError> {
        if index > self.workspaces.len() {
            return Err(PlaneError::WorkspaceIndexOutOfBounds {
                index,
                len: self.workspaces.len(),
            });
        }

        self.workspaces.insert(index, workspace);
        if index <= self.active_workspace_idx {
            self.active_workspace_idx += 1;
        }

        Ok(())
    }

    /// Appends a workspace to the bottom of this plane.
    pub fn push_workspace(&mut self, workspace: T) {
        self.workspaces.push(workspace);
    }

    /// Removes a workspace while preserving the active workspace where possible.
    ///
    /// Removing the active workspace selects the workspace that takes its
    /// place, or the new last workspace when the former active one was last.
    pub fn remove_workspace(&mut self, index: usize) -> Result<T, PlaneError> {
        if index >= self.workspaces.len() {
            return Err(PlaneError::WorkspaceIndexOutOfBounds {
                index,
                len: self.workspaces.len(),
            });
        }

        if self.workspaces.len() == 1 {
            return Err(PlaneError::CannotRemoveLastWorkspace);
        }

        let removed = self.workspaces.remove(index);

        if index < self.active_workspace_idx {
            self.active_workspace_idx -= 1;
        } else if index == self.active_workspace_idx
            && self.active_workspace_idx == self.workspaces.len()
        {
            self.active_workspace_idx -= 1;
        }

        Ok(removed)
    }

    /// Consumes the plane and returns its workspaces in vertical order.
    pub fn into_workspaces(self) -> Vec<T> {
        self.workspaces
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_plane_with_active_workspace() {
        let plane = WorkspacePlane::new(PlaneId::new(7), vec!["top", "active"], 1).unwrap();

        assert_eq!(plane.id().get(), 7);
        assert_eq!(plane.len(), 2);
        assert_eq!(plane.active_workspace_idx(), 1);
        assert_eq!(*plane.active_workspace(), "active");
    }

    #[test]
    fn rejects_empty_plane() {
        let result = WorkspacePlane::<u8>::new(PlaneId::new(0), vec![], 0);

        assert!(matches!(result, Err(PlaneError::EmptyPlane)));
    }

    #[test]
    fn rejects_invalid_initial_active_index() {
        let result = WorkspacePlane::new(PlaneId::new(0), vec!["only"], 1);

        assert!(matches!(
            result,
            Err(PlaneError::ActiveWorkspaceOutOfBounds { index: 1, len: 1 })
        ));
    }

    #[test]
    fn activates_workspace_within_plane() {
        let mut plane = WorkspacePlane::new(PlaneId::new(0), vec!["top", "bottom"], 0).unwrap();

        plane.activate_workspace(1).unwrap();

        assert_eq!(plane.active_workspace_idx(), 1);
        assert_eq!(*plane.active_workspace(), "bottom");
    }

    #[test]
    fn rejects_out_of_bounds_activation() {
        let mut plane = WorkspacePlane::new(PlaneId::new(0), vec!["only"], 0).unwrap();

        let result = plane.activate_workspace(1);

        assert!(matches!(
            result,
            Err(PlaneError::WorkspaceIndexOutOfBounds { index: 1, len: 1 })
        ));
        assert_eq!(plane.active_workspace_idx(), 0);
    }

    #[test]
    fn insertion_before_active_workspace_preserves_active_item() {
        let mut plane =
            WorkspacePlane::new(PlaneId::new(0), vec!["top", "active", "bottom"], 1).unwrap();

        plane.insert_workspace(0, "new top").unwrap();

        assert_eq!(plane.active_workspace_idx(), 2);
        assert_eq!(*plane.active_workspace(), "active");
    }

    #[test]
    fn removing_workspace_before_active_preserves_active_item() {
        let mut plane =
            WorkspacePlane::new(PlaneId::new(0), vec!["top", "active", "bottom"], 1).unwrap();

        assert_eq!(plane.remove_workspace(0).unwrap(), "top");

        assert_eq!(plane.active_workspace_idx(), 0);
        assert_eq!(*plane.active_workspace(), "active");
    }

    #[test]
    fn removing_active_last_workspace_selects_new_last() {
        let mut plane =
            WorkspacePlane::new(PlaneId::new(0), vec!["top", "middle", "bottom"], 2).unwrap();

        assert_eq!(plane.remove_workspace(2).unwrap(), "bottom");

        assert_eq!(plane.active_workspace_idx(), 1);
        assert_eq!(*plane.active_workspace(), "middle");
    }

    #[test]
    fn cannot_remove_last_workspace() {
        let mut plane = WorkspacePlane::new(PlaneId::new(0), vec!["only"], 0).unwrap();

        assert!(matches!(
            plane.remove_workspace(0),
            Err(PlaneError::CannotRemoveLastWorkspace)
        ));
        assert_eq!(plane.len(), 1);
    }
}
