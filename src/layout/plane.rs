//! Plane-local workspace storage.
//!
//! A plane is one depth layer on a monitor. Its workspaces form the existing
//! vertical workspace sequence, and the active workspace index is local to
//! that plane.
//!
//! Monitor now owns a WorkspacePlanes collection. The layout still exposes
//! only the active plane through its compatibility view; depth switching and
//! plane-aware rendering are implemented in later steps.

use std::fmt;
use std::ops::{Deref, DerefMut};

use crate::utils::id::IdCounter;

static PLANE_ID_COUNTER: IdCounter = IdCounter::new();

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

    /// Allocates a process-unique plane ID.
    pub fn next() -> Self {
        Self::new(PLANE_ID_COUNTER.next())
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
    ActiveWorkspaceOutOfBounds { index: usize, len: usize },

    /// The requested workspace index is outside the plane.
    WorkspaceIndexOutOfBounds { index: usize, len: usize },

    /// The last workspace in a plane cannot be removed.
    CannotRemoveLastWorkspace,

    /// A plane with the same identity already exists in the collection.
    DuplicatePlaneId,

    /// The requested plane index is outside the collection.
    PlaneIndexOutOfBounds { index: usize, len: usize },

    /// The last plane cannot be removed.
    CannotRemoveLastPlane,

    /// No plane with the requested stable identity exists.
    UnknownPlaneId,
}

impl fmt::Display for PlaneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPlane => write!(f, "a plane must contain at least one workspace"),
            Self::ActiveWorkspaceOutOfBounds { index, len } => {
                write!(
                    f,
                    "active workspace index {index} is out of bounds for {len} workspaces"
                )
            }
            Self::WorkspaceIndexOutOfBounds { index, len } => {
                write!(
                    f,
                    "workspace index {index} is out of bounds for {len} workspaces"
                )
            }
            Self::CannotRemoveLastWorkspace => {
                write!(f, "cannot remove the last workspace from a plane")
            }
            Self::DuplicatePlaneId => write!(f, "a plane with this ID already exists"),
            Self::PlaneIndexOutOfBounds { index, len } => {
                write!(f, "plane index {index} is out of bounds for {len} planes")
            }
            Self::CannotRemoveLastPlane => {
                write!(f, "cannot remove the last plane from a collection")
            }
            Self::UnknownPlaneId => write!(f, "no plane with this ID exists"),
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
    pub fn workspace_count(&self) -> usize {
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

        if index < self.active_workspace_idx || self.active_workspace_idx == self.workspaces.len() {
            self.active_workspace_idx -= 1;
        }

        Ok(removed)
    }

    /// Consumes the plane and returns its workspaces in vertical order.
    pub fn into_workspaces(self) -> Vec<T> {
        self.workspaces
    }
}

/// An ordered collection of depth planes for one monitor.
///
/// The planes are stored in depth order. `active_plane_idx` identifies the
/// currently selected plane. Each plane independently tracks its active
/// workspace, so switching planes does not lose the vertical position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePlanes<T> {
    planes: Vec<WorkspacePlane<T>>,
    active_plane_idx: usize,
}

impl<T> WorkspacePlanes<T> {
    /// Creates a collection containing one initial plane.
    pub fn new(initial_plane: WorkspacePlane<T>) -> Self {
        Self {
            planes: vec![initial_plane],
            active_plane_idx: 0,
        }
    }

    /// Returns the number of planes in the collection.
    pub fn plane_count(&self) -> usize {
        self.planes.len()
    }

    /// Returns the active plane's index in depth order.
    pub const fn active_plane_idx(&self) -> usize {
        self.active_plane_idx
    }

    /// Returns the active workspace's index within the active plane.
    pub fn active_workspace_idx(&self) -> usize {
        self.active_plane().active_workspace_idx()
    }

    /// Activates a workspace within the active plane.
    pub fn activate_workspace(&mut self, index: usize) -> Result<(), PlaneError> {
        self.active_plane_mut().activate_workspace(index)
    }

    /// Returns the active plane.
    pub fn active_plane(&self) -> &WorkspacePlane<T> {
        &self.planes[self.active_plane_idx]
    }

    /// Returns mutable access to the active plane.
    pub fn active_plane_mut(&mut self) -> &mut WorkspacePlane<T> {
        &mut self.planes[self.active_plane_idx]
    }

    /// Returns the plane at a depth-order index.
    pub fn plane(&self, index: usize) -> Option<&WorkspacePlane<T>> {
        self.planes.get(index)
    }

    /// Returns mutable access to a plane at a depth-order index.
    pub fn plane_mut(&mut self, index: usize) -> Option<&mut WorkspacePlane<T>> {
        self.planes.get_mut(index)
    }

    /// Finds a plane by its stable identity.
    pub fn plane_by_id(&self, id: PlaneId) -> Option<&WorkspacePlane<T>> {
        self.planes.iter().find(|plane| plane.id() == id)
    }

    /// Finds mutable access to a plane by its stable identity.
    pub fn plane_by_id_mut(&mut self, id: PlaneId) -> Option<&mut WorkspacePlane<T>> {
        self.planes.iter_mut().find(|plane| plane.id() == id)
    }

    /// Finds the current depth-order index for a plane identity.
    pub fn index_of(&self, id: PlaneId) -> Option<usize> {
        self.planes.iter().position(|plane| plane.id() == id)
    }

    /// Iterates over planes in depth order.
    pub fn iter_planes(&self) -> impl Iterator<Item = &WorkspacePlane<T>> {
        self.planes.iter()
    }

    /// Iterates mutably over planes in depth order.
    pub fn iter_planes_mut(&mut self) -> impl Iterator<Item = &mut WorkspacePlane<T>> {
        self.planes.iter_mut()
    }

    /// Iterates over every workspace in plane order.
    ///
    /// The workspace index is local to its plane. This is useful for
    /// monitor-wide operations such as workspace lookup and output migration,
    /// which must include workspaces on inactive planes.
    pub fn iter_workspaces(&self) -> impl Iterator<Item = (PlaneId, usize, &T)> {
        self.planes.iter().flat_map(|plane| {
            let plane_id = plane.id();
            plane
                .workspaces()
                .iter()
                .enumerate()
                .map(move |(workspace_idx, workspace)| (plane_id, workspace_idx, workspace))
        })
    }

    /// Iterates mutably over every workspace in plane order.
    ///
    /// The workspace index is local to its plane. This is the mutable
    /// counterpart to Self::iter_workspaces.
    pub fn iter_workspaces_mut(&mut self) -> impl Iterator<Item = (PlaneId, usize, &mut T)> {
        self.planes.iter_mut().flat_map(|plane| {
            let plane_id = plane.id();
            plane
                .workspaces_mut()
                .iter_mut()
                .enumerate()
                .map(move |(workspace_idx, workspace)| (plane_id, workspace_idx, workspace))
        })
    }

    /// Consumes the collection and flattens all planes into workspace order.
    ///
    /// This is used by existing monitor migration paths while they are being
    /// upgraded to preserve plane groups.
    pub fn into_workspaces(self) -> Vec<T> {
        self.planes
            .into_iter()
            .flat_map(WorkspacePlane::into_workspaces)
            .collect()
    }

    /// Activates a plane by its depth-order index.
    pub fn activate_plane(&mut self, index: usize) -> Result<(), PlaneError> {
        if index >= self.planes.len() {
            return Err(PlaneError::PlaneIndexOutOfBounds {
                index,
                len: self.planes.len(),
            });
        }

        self.active_plane_idx = index;
        Ok(())
    }

    /// Activates a plane by its stable identity.
    pub fn activate_plane_by_id(&mut self, id: PlaneId) -> Result<(), PlaneError> {
        let index = self.index_of(id).ok_or(PlaneError::UnknownPlaneId)?;

        self.active_plane_idx = index;
        Ok(())
    }

    /// Inserts a plane at a depth-order index.
    ///
    /// Inserting at or before the active index preserves the previously
    /// active plane by shifting its index.
    pub fn insert_plane(
        &mut self,
        index: usize,
        plane: WorkspacePlane<T>,
    ) -> Result<(), PlaneError> {
        if self
            .planes
            .iter()
            .any(|existing| existing.id() == plane.id())
        {
            return Err(PlaneError::DuplicatePlaneId);
        }

        if index > self.planes.len() {
            return Err(PlaneError::PlaneIndexOutOfBounds {
                index,
                len: self.planes.len(),
            });
        }

        self.planes.insert(index, plane);
        if index <= self.active_plane_idx {
            self.active_plane_idx += 1;
        }

        Ok(())
    }

    /// Appends a plane at the back of the depth order.
    pub fn push_plane(&mut self, plane: WorkspacePlane<T>) -> Result<(), PlaneError> {
        self.insert_plane(self.planes.len(), plane)
    }

    /// Removes a plane while preserving the active plane where possible.
    ///
    /// Removing the active plane selects the plane that takes its place, or
    /// the new last plane when the former active plane was last.
    pub fn remove_plane(&mut self, index: usize) -> Result<WorkspacePlane<T>, PlaneError> {
        if index >= self.planes.len() {
            return Err(PlaneError::PlaneIndexOutOfBounds {
                index,
                len: self.planes.len(),
            });
        }

        if self.planes.len() == 1 {
            return Err(PlaneError::CannotRemoveLastPlane);
        }

        let removed = self.planes.remove(index);

        if index < self.active_plane_idx || self.active_plane_idx == self.planes.len() {
            self.active_plane_idx -= 1;
        }

        Ok(removed)
    }

    /// Consumes the collection and returns its planes in depth order.
    pub fn into_planes(self) -> Vec<WorkspacePlane<T>> {
        self.planes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_plane_with_active_workspace() {
        let plane = WorkspacePlane::new(PlaneId::new(7), vec!["top", "active"], 1).unwrap();

        assert_eq!(plane.id().get(), 7);
        assert_eq!(plane.workspace_count(), 2);
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
        assert_eq!(plane.workspace_count(), 1);
    }

    #[test]
    fn creates_collection_with_initial_plane_active() {
        let plane = WorkspacePlane::new(PlaneId::new(1), vec!["one"], 0).unwrap();
        let planes = WorkspacePlanes::new(plane);

        assert_eq!(planes.plane_count(), 1);
        assert_eq!(planes.active_plane_idx(), 0);
        assert_eq!(planes.active_plane().id(), PlaneId::new(1));
    }

    #[test]
    fn inserts_plane_before_active_and_preserves_active_identity() {
        let first = WorkspacePlane::new(PlaneId::new(1), vec!["one"], 0).unwrap();
        let mut planes = WorkspacePlanes::new(first);
        let second = WorkspacePlane::new(PlaneId::new(2), vec!["two"], 0).unwrap();

        planes.insert_plane(0, second).unwrap();

        assert_eq!(planes.plane_count(), 2);
        assert_eq!(planes.active_plane_idx(), 1);
        assert_eq!(planes.active_plane().id(), PlaneId::new(1));
    }

    #[test]
    fn rejects_duplicate_plane_ids() {
        let first = WorkspacePlane::new(PlaneId::new(1), vec!["one"], 0).unwrap();
        let mut planes = WorkspacePlanes::new(first);
        let duplicate = WorkspacePlane::new(PlaneId::new(1), vec!["duplicate"], 0).unwrap();

        assert!(matches!(
            planes.push_plane(duplicate),
            Err(PlaneError::DuplicatePlaneId)
        ));
        assert_eq!(planes.plane_count(), 1);
    }

    #[test]
    fn iterates_workspaces_in_plane_order_with_plane_local_indices() {
        let first = WorkspacePlane::new(PlaneId::new(10), vec!["a", "b"], 0).unwrap();
        let mut planes = WorkspacePlanes::new(first);
        let second = WorkspacePlane::new(PlaneId::new(20), vec!["c"], 0).unwrap();
        planes.push_plane(second).unwrap();

        let workspaces: Vec<_> = planes
            .iter_workspaces()
            .map(|(plane_id, workspace_idx, workspace)| (plane_id, workspace_idx, *workspace))
            .collect();

        assert_eq!(
            workspaces,
            vec![
                (PlaneId::new(10), 0, "a"),
                (PlaneId::new(10), 1, "b"),
                (PlaneId::new(20), 0, "c"),
            ]
        );
    }

    #[test]
    fn iterates_mutably_over_workspaces_in_all_planes() {
        let first = WorkspacePlane::new(PlaneId::new(1), vec![0, 0], 0).unwrap();
        let mut planes = WorkspacePlanes::new(first);
        let second = WorkspacePlane::new(PlaneId::new(2), vec![0], 0).unwrap();
        planes.push_plane(second).unwrap();

        for (plane_id, workspace_idx, workspace) in planes.iter_workspaces_mut() {
            *workspace += plane_id.get() as usize + workspace_idx;
        }

        assert_eq!(planes.plane(0).unwrap().workspaces(), &[1, 2]);
        assert_eq!(planes.plane(1).unwrap().workspaces(), &[2]);
    }

    #[test]
    fn activates_plane_by_id_and_preserves_its_workspace_selection() {
        let first = WorkspacePlane::new(PlaneId::new(1), vec!["one-a", "one-b"], 1).unwrap();
        let mut planes = WorkspacePlanes::new(first);
        let second = WorkspacePlane::new(PlaneId::new(2), vec!["two-a", "two-b"], 0).unwrap();

        planes.push_plane(second).unwrap();
        planes.activate_plane_by_id(PlaneId::new(2)).unwrap();

        assert_eq!(planes.active_plane_idx(), 1);
        assert_eq!(planes.active_plane().id(), PlaneId::new(2));
        assert_eq!(*planes.active_plane().active_workspace(), "two-a");

        planes.activate_plane_by_id(PlaneId::new(1)).unwrap();

        assert_eq!(planes.active_plane_idx(), 0);
        assert_eq!(*planes.active_plane().active_workspace(), "one-b");
    }

    #[test]
    fn rejects_activation_of_unknown_plane() {
        let first = WorkspacePlane::new(PlaneId::new(1), vec!["one"], 0).unwrap();
        let mut planes = WorkspacePlanes::new(first);

        assert!(matches!(
            planes.activate_plane_by_id(PlaneId::new(99)),
            Err(PlaneError::UnknownPlaneId)
        ));
        assert_eq!(planes.active_plane_idx(), 0);
    }

    #[test]
    fn removing_plane_before_active_preserves_active_identity() {
        let first = WorkspacePlane::new(PlaneId::new(1), vec!["one"], 0).unwrap();
        let mut planes = WorkspacePlanes::new(first);
        let second = WorkspacePlane::new(PlaneId::new(2), vec!["two"], 0).unwrap();
        let third = WorkspacePlane::new(PlaneId::new(3), vec!["three"], 0).unwrap();

        planes.push_plane(second).unwrap();
        planes.push_plane(third).unwrap();
        planes.activate_plane_by_id(PlaneId::new(3)).unwrap();

        assert_eq!(planes.remove_plane(0).unwrap().id(), PlaneId::new(1));

        assert_eq!(planes.active_plane_idx(), 1);
        assert_eq!(planes.active_plane().id(), PlaneId::new(3));
    }

    #[test]
    fn removing_active_last_plane_selects_new_last() {
        let first = WorkspacePlane::new(PlaneId::new(1), vec!["one"], 0).unwrap();
        let mut planes = WorkspacePlanes::new(first);
        let second = WorkspacePlane::new(PlaneId::new(2), vec!["two"], 0).unwrap();
        let third = WorkspacePlane::new(PlaneId::new(3), vec!["three"], 0).unwrap();

        planes.push_plane(second).unwrap();
        planes.push_plane(third).unwrap();
        planes.activate_plane_by_id(PlaneId::new(3)).unwrap();

        assert_eq!(planes.remove_plane(2).unwrap().id(), PlaneId::new(3));

        assert_eq!(planes.active_plane_idx(), 1);
        assert_eq!(planes.active_plane().id(), PlaneId::new(2));
    }

    #[test]
    fn dereferences_to_the_active_plane_workspaces() {
        let first = WorkspacePlane::new(PlaneId::new(1), vec!["a", "b"], 1).unwrap();
        let mut planes = WorkspacePlanes::new(first);
        let second = WorkspacePlane::new(PlaneId::new(2), vec!["c"], 0).unwrap();
        planes.push_plane(second).unwrap();

        assert_eq!(planes.len(), 2);
        assert_eq!(planes.iter().copied().collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(planes[planes.active_workspace_idx()], "b");

        planes.activate_plane(1).unwrap();
        assert_eq!(planes.len(), 1);
        assert_eq!(planes.iter().copied().collect::<Vec<_>>(), vec!["c"]);
    }

    #[test]
    fn consumes_workspaces_from_all_planes_in_plane_order() {
        let first = WorkspacePlane::new(PlaneId::new(1), vec!["a", "b"], 0).unwrap();
        let mut planes = WorkspacePlanes::new(first);
        let second = WorkspacePlane::new(PlaneId::new(2), vec!["c"], 0).unwrap();
        planes.push_plane(second).unwrap();

        assert_eq!(planes.into_workspaces(), vec!["a", "b", "c"]);
    }

    #[test]
    fn cannot_remove_last_plane() {
        let first = WorkspacePlane::new(PlaneId::new(1), vec!["one"], 0).unwrap();
        let mut planes = WorkspacePlanes::new(first);

        assert!(matches!(
            planes.remove_plane(0),
            Err(PlaneError::CannotRemoveLastPlane)
        ));
        assert_eq!(planes.plane_count(), 1);
    }
}

impl<T> Deref for WorkspacePlanes<T> {
    type Target = Vec<T>;

    /// Dereferences to the active plane's workspace list.
    ///
    /// Existing layout operations therefore continue to operate on the
    /// visible vertical workspace sequence. Monitor-wide operations must use
    /// iter_workspaces() to include inactive planes.
    fn deref(&self) -> &Self::Target {
        self.active_plane().workspaces()
    }
}

impl<T> DerefMut for WorkspacePlanes<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.active_plane_mut().workspaces_mut()
    }
}

impl<'a, T> IntoIterator for &'a WorkspacePlanes<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.active_plane().workspaces().iter()
    }
}

impl<'a, T> IntoIterator for &'a mut WorkspacePlanes<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.active_plane_mut().workspaces_mut().iter_mut()
    }
}
