//! Plane-local workspace storage.
//!
//! A plane is one depth layer on a monitor. Its workspaces form the existing
//! vertical workspace sequence, and the active workspace index is local to
//! that plane.
//!
//! A monitor owns a WorkspacePlanes collection. Existing Y-axis operations
//! use the active plane's compatibility view, while monitor-wide traversal
//! includes every plane. Plane switching and plane-aware rendering are
//! implemented in later steps.

use std::fmt;
use std::ops::{Deref, DerefMut, Index, IndexMut};

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
    /// Creates an empty collection.
    ///
    /// This is used while no outputs are connected. Monitors themselves must
    /// always contain at least one plane.
    pub fn empty() -> Self {
        Self {
            planes: vec![],
            active_plane_idx: 0,
        }
    }

    /// Creates a collection containing one initial plane.
    pub fn new(initial_plane: WorkspacePlane<T>) -> Self {
        Self {
            planes: vec![initial_plane],
            active_plane_idx: 0,
        }
    }

    /// Creates a collection from planes in depth order.
    pub fn from_planes(
        planes: Vec<WorkspacePlane<T>>,
        active_plane_idx: usize,
    ) -> Result<Self, PlaneError> {
        if planes.is_empty() {
            if active_plane_idx != 0 {
                return Err(PlaneError::PlaneIndexOutOfBounds {
                    index: active_plane_idx,
                    len: 0,
                });
            }

            return Ok(Self::empty());
        }

        if active_plane_idx >= planes.len() {
            return Err(PlaneError::PlaneIndexOutOfBounds {
                index: active_plane_idx,
                len: planes.len(),
            });
        }

        for (index, plane) in planes.iter().enumerate() {
            if planes[..index]
                .iter()
                .any(|previous| previous.id() == plane.id())
            {
                return Err(PlaneError::DuplicatePlaneId);
            }
        }

        Ok(Self {
            planes,
            active_plane_idx,
        })
    }

    /// Returns whether the collection contains no planes.
    pub fn is_empty(&self) -> bool {
        self.planes.is_empty()
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
    /// Prefer `Self::into_planes` when transferring workspaces between monitors
    /// so their depth grouping and active selection remain intact.
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

        let was_empty = self.planes.is_empty();
        self.planes.insert(index, plane);
        if was_empty {
            self.active_plane_idx = 0;
        } else if index <= self.active_plane_idx {
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

        self.remove_plane_allow_empty(index)
    }

    /// Removes a plane even when it is the final plane.
    ///
    /// This is reserved for the no-output compatibility collection, which
    /// historically allowed its workspace list to become empty.
    fn remove_plane_allow_empty(&mut self, index: usize) -> Result<WorkspacePlane<T>, PlaneError> {
        if index >= self.planes.len() {
            return Err(PlaneError::PlaneIndexOutOfBounds {
                index,
                len: self.planes.len(),
            });
        }

        let removed = self.planes.remove(index);

        if self.planes.is_empty() {
            self.active_plane_idx = 0;
        } else if index < self.active_plane_idx || self.active_plane_idx == self.planes.len() {
            self.active_plane_idx -= 1;
        }

        Ok(removed)
    }

    /// Extracts workspaces matching a predicate, preserving their plane groups.
    ///
    /// A plane split across the two collections keeps its stable identity in
    /// the collection that retains the original workspaces; the extracted
    /// fragment receives a fresh identity.
    pub fn extract_workspaces_where(&mut self, mut predicate: impl FnMut(&T) -> bool) -> Self {
        let original_active_plane_idx = self.active_plane_idx;
        let mut retained = Vec::new();
        let mut extracted = Vec::new();
        let mut retained_active_plane_idx = None;
        let mut extracted_active_plane_idx = None;

        for (plane_idx, plane) in self.planes.drain(..).enumerate() {
            let WorkspacePlane {
                id,
                workspaces,
                active_workspace_idx,
            } = plane;
            let mut matching = Vec::new();
            let mut remaining = Vec::new();
            let mut matching_active_idx = None;
            let mut matching_fallback_active_idx = None;
            let mut remaining_active_idx = None;

            for (workspace_idx, workspace) in workspaces.into_iter().enumerate() {
                if predicate(&workspace) {
                    if workspace_idx == active_workspace_idx {
                        matching_active_idx = Some(matching.len());
                        // If the active workspace is extracted, the remaining
                        // plane falls back to the previous workspace, or the
                        // first one when nothing remains above it.
                        remaining_active_idx = Some(remaining.len().saturating_sub(1));
                    }
                    matching.push(workspace);
                } else {
                    if workspace_idx == active_workspace_idx {
                        remaining_active_idx = Some(remaining.len());
                        // If the active workspace stays behind, the extracted
                        // fragment selects its nearest preceding workspace.
                        matching_fallback_active_idx = Some(matching.len().saturating_sub(1));
                    }
                    remaining.push(workspace);
                }
            }

            if !matching.is_empty() {
                let extracted_id = if remaining.is_empty() {
                    id
                } else {
                    PlaneId::next()
                };
                let active_idx = matching_active_idx
                    .or(matching_fallback_active_idx)
                    .unwrap_or_else(|| active_workspace_idx.min(matching.len() - 1));

                if plane_idx == original_active_plane_idx {
                    extracted_active_plane_idx = Some(extracted.len());
                }

                extracted.push(WorkspacePlane {
                    id: extracted_id,
                    workspaces: matching,
                    active_workspace_idx: active_idx,
                });
            }

            if !remaining.is_empty() {
                let active_idx = remaining_active_idx
                    .unwrap_or_else(|| active_workspace_idx.min(remaining.len() - 1));

                if plane_idx == original_active_plane_idx {
                    retained_active_plane_idx = Some(retained.len());
                }

                retained.push(WorkspacePlane {
                    id,
                    workspaces: remaining,
                    active_workspace_idx: active_idx,
                });
            }
        }

        self.planes = retained;
        self.active_plane_idx = retained_active_plane_idx
            .unwrap_or_else(|| original_active_plane_idx.min(self.planes.len().saturating_sub(1)));

        let extracted_active_plane_idx = extracted_active_plane_idx
            .unwrap_or_else(|| original_active_plane_idx.min(extracted.len().saturating_sub(1)));

        Self {
            planes: extracted,
            active_plane_idx: extracted_active_plane_idx,
        }
    }

    /// Consumes the collection and returns its planes in depth order.
    pub fn into_planes(self) -> Vec<WorkspacePlane<T>> {
        self.planes
    }
}

/// Flat compatibility view for workspaces while no outputs are connected.
///
/// Unlike a monitor, the no-output layout historically treated all workspaces
/// as one flat list. This wrapper preserves that API while retaining the
/// original plane grouping and active plane selection for output reconnection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoOutputWorkspacePlanes<T> {
    planes: WorkspacePlanes<T>,
}

impl<T> NoOutputWorkspacePlanes<T> {
    /// Creates an empty no-output workspace collection.
    pub fn empty() -> Self {
        Self {
            planes: WorkspacePlanes::empty(),
        }
    }

    /// Creates the no-output view from a plane collection.
    pub fn from_planes(planes: WorkspacePlanes<T>) -> Self {
        Self { planes }
    }

    /// Creates one initial plane from a legacy flat workspace list.
    pub fn from_workspaces(workspaces: Vec<T>) -> Result<Self, PlaneError> {
        if workspaces.is_empty() {
            return Ok(Self::empty());
        }

        let plane = WorkspacePlane::new(PlaneId::next(), workspaces, 0)?;
        Ok(Self::from_planes(WorkspacePlanes::new(plane)))
    }

    /// Returns the number of depth planes.
    pub fn plane_count(&self) -> usize {
        self.planes.plane_count()
    }

    /// Returns the active plane's depth-order index, or zero when empty.
    pub fn active_plane_idx(&self) -> usize {
        self.planes.active_plane_idx()
    }

    /// Returns the stable identity of a plane at the given depth index.
    pub fn plane_id(&self, index: usize) -> Option<PlaneId> {
        self.planes.plane(index).map(WorkspacePlane::id)
    }

    /// Returns the active plane's stable identity, if the collection is non-empty.
    pub fn active_plane_id(&self) -> Option<PlaneId> {
        if self.planes.is_empty() {
            None
        } else {
            Some(self.planes.active_plane().id())
        }
    }

    /// Returns the active workspace's index within the active plane.
    pub fn active_workspace_idx(&self) -> Option<usize> {
        (!self.planes.is_empty()).then(|| self.planes.active_workspace_idx())
    }

    /// Returns the number of workspaces across all planes.
    pub fn len(&self) -> usize {
        self.planes.iter_workspaces().count()
    }

    /// Returns whether there are no workspaces.
    pub fn is_empty(&self) -> bool {
        self.planes.is_empty()
    }

    /// Iterates over all workspaces in depth and vertical order.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.planes
            .iter_workspaces()
            .map(|(_, _, workspace)| workspace)
    }

    /// Iterates mutably over all workspaces in depth and vertical order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.planes
            .iter_workspaces_mut()
            .map(|(_, _, workspace)| workspace)
    }

    /// Appends a workspace to the end of the flat view, creating a plane if needed.
    pub fn push(&mut self, workspace: T) {
        if self.planes.is_empty() {
            let plane = WorkspacePlane::new(PlaneId::next(), vec![workspace], 0)
                .expect("a plane created with one workspace must be valid");
            self.planes
                .push_plane(plane)
                .expect("a newly generated plane ID must be unique");
        } else {
            let last_plane_idx = self.planes.plane_count() - 1;
            self.planes
                .plane_mut(last_plane_idx)
                .unwrap()
                .push_workspace(workspace);
        }
    }

    /// Inserts a workspace at its flattened index.
    pub fn insert(&mut self, index: usize, workspace: T) {
        let len = self.len();
        assert!(index <= len, "workspace index out of bounds");

        if self.planes.is_empty() {
            self.push(workspace);
            return;
        }

        let mut offset = 0;
        for plane in self.planes.iter_planes_mut() {
            let plane_len = plane.workspace_count();
            if index <= offset + plane_len {
                plane
                    .insert_workspace(index - offset, workspace)
                    .expect("the flattened workspace index must be valid");
                return;
            }
            offset += plane_len;
        }

        unreachable!("a valid flattened workspace index must belong to a plane");
    }

    /// Removes a workspace at its flattened index.
    ///
    /// If it was the only workspace in its plane, the entire plane is removed.
    pub fn remove(&mut self, index: usize) -> T {
        let (plane_idx, workspace_idx) = self
            .planes
            .iter_planes()
            .enumerate()
            .scan(0usize, |offset, (plane_idx, plane)| {
                let start = *offset;
                *offset += plane.workspace_count();
                Some((plane_idx, plane, start))
            })
            .find_map(|(plane_idx, plane, start)| {
                (index < start + plane.workspace_count()).then_some((plane_idx, index - start))
            })
            .expect("workspace index out of bounds");

        if self.planes.plane(plane_idx).unwrap().workspace_count() == 1 {
            self.planes
                .remove_plane_allow_empty(plane_idx)
                .expect("the selected plane index must be valid")
                .into_workspaces()
                .into_iter()
                .next()
                .expect("a plane must contain a workspace")
        } else {
            self.planes
                .plane_mut(plane_idx)
                .unwrap()
                .remove_workspace(workspace_idx)
                .expect("the selected workspace index must be valid")
        }
    }

    /// Returns a workspace at its flattened index.
    pub fn get(&self, index: usize) -> Option<&T> {
        self.iter().nth(index)
    }

    /// Returns mutable access to a workspace at its flattened index.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        self.iter_mut().nth(index)
    }

    /// Consumes the flat view and returns its structured plane collection.
    pub fn into_planes(self) -> WorkspacePlanes<T> {
        self.planes
    }
}

impl<T> Default for NoOutputWorkspacePlanes<T> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<T> Index<usize> for NoOutputWorkspacePlanes<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).expect("workspace index out of bounds")
    }
}

impl<T> IndexMut<usize> for NoOutputWorkspacePlanes<T> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        self.get_mut(index).expect("workspace index out of bounds")
    }
}

impl<'a, T> IntoIterator for &'a NoOutputWorkspacePlanes<T> {
    type Item = &'a T;
    type IntoIter = std::vec::IntoIter<&'a T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter().collect::<Vec<_>>().into_iter()
    }
}

impl<'a, T> IntoIterator for &'a mut NoOutputWorkspacePlanes<T> {
    type Item = &'a mut T;
    type IntoIter = std::vec::IntoIter<&'a mut T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut().collect::<Vec<_>>().into_iter()
    }
}

/// Compatibility view: dereference to the active plane's workspace list.
impl<T> Deref for WorkspacePlanes<T> {
    type Target = Vec<T>;

    /// Dereferences to the active plane's workspace list.
    ///
    /// Existing layout operations therefore continue to operate on the
    /// visible vertical workspace sequence. Monitor-wide operations must use
    /// iter_workspaces() to include inactive planes.
    fn deref(&self) -> &Self::Target {
        &self.active_plane().workspaces
    }
}

impl<T> DerefMut for WorkspacePlanes<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.active_plane_mut().workspaces
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
    fn extracting_workspaces_preserves_plane_groups_and_selection() {
        let first_id = PlaneId::new(100);
        let second_id = PlaneId::new(101);
        let first = WorkspacePlane::new(first_id, vec![10, 11], 1).unwrap();
        let second = WorkspacePlane::new(second_id, vec![20, 21], 1).unwrap();
        let mut planes = WorkspacePlanes::new(first);
        planes.push_plane(second).unwrap();
        planes.activate_plane(1).unwrap();

        let extracted = planes.extract_workspaces_where(|workspace| *workspace >= 20);

        assert_eq!(planes.plane_count(), 1);
        assert_eq!(planes.active_plane_idx(), 0);
        assert_eq!(planes.active_plane().id(), first_id);
        assert_eq!(planes.active_workspace_idx(), 1);
        assert_eq!(planes.active_plane().workspaces(), &[10, 11]);

        assert_eq!(extracted.plane_count(), 1);
        assert_eq!(extracted.active_plane_idx(), 0);
        assert_eq!(extracted.active_plane().id(), second_id);
        assert_eq!(extracted.active_workspace_idx(), 1);
        assert_eq!(extracted.active_plane().workspaces(), &[20, 21]);
    }

    #[test]
    fn extracting_active_workspace_falls_back_to_the_previous_retained_workspace() {
        let plane_id = PlaneId::new(u64::MAX - 2);
        let plane = WorkspacePlane::new(plane_id, vec![0, 1, 2, 3], 2).unwrap();
        let mut planes = WorkspacePlanes::new(plane);

        let extracted = planes.extract_workspaces_where(|workspace| *workspace == 2);

        assert_eq!(planes.active_plane().workspaces(), &[0, 1, 3]);
        assert_eq!(planes.active_workspace_idx(), 1);
        assert_eq!(planes.active_plane().active_workspace(), &1);
        assert_eq!(extracted.active_plane().workspaces(), &[2]);
    }

    #[test]
    fn extracting_inactive_fragment_selects_nearest_preceding_workspace() {
        let plane_id = PlaneId::new(u64::MAX - 3);
        let plane = WorkspacePlane::new(plane_id, vec![0, 1, 2, 3], 1).unwrap();
        let mut planes = WorkspacePlanes::new(plane);

        let extracted = planes
            .extract_workspaces_where(|workspace| *workspace == 0 || *workspace == 2);

        assert_eq!(planes.active_plane().active_workspace(), &1);
        assert_eq!(extracted.active_plane().workspaces(), &[0, 2]);
        assert_eq!(extracted.active_workspace_idx(), 0);
        assert_eq!(extracted.active_plane().active_workspace(), &0);
    }

    #[test]
    fn splitting_a_plane_keeps_the_original_id_with_the_remaining_workspaces() {
        let original_id = PlaneId::new(u64::MAX - 1);
        let plane = WorkspacePlane::new(original_id, vec![0, 1, 2, 3], 2).unwrap();
        let mut planes = WorkspacePlanes::new(plane);

        let extracted =
            planes.extract_workspaces_where(|workspace| *workspace == 1 || *workspace == 2);

        assert_eq!(planes.plane_count(), 1);
        assert_eq!(planes.active_plane().id(), original_id);
        assert_eq!(planes.active_plane().workspaces(), &[0, 3]);
        assert_eq!(planes.active_workspace_idx(), 0);

        assert_eq!(extracted.plane_count(), 1);
        assert_ne!(extracted.active_plane().id(), original_id);
        assert_eq!(extracted.active_plane().workspaces(), &[1, 2]);
        assert_eq!(extracted.active_workspace_idx(), 1);
    }

    #[test]
    fn no_output_flat_view_keeps_plane_structure() {
        let first_id = PlaneId::new(200);
        let second_id = PlaneId::new(201);
        let first = WorkspacePlane::new(first_id, vec![1, 2], 1).unwrap();
        let second = WorkspacePlane::new(second_id, vec![3, 4], 0).unwrap();
        let mut planes = WorkspacePlanes::new(first);
        planes.push_plane(second).unwrap();
        planes.activate_plane(1).unwrap();
        let mut flat = NoOutputWorkspacePlanes::from_planes(planes);

        assert_eq!(flat.iter().copied().collect::<Vec<_>>(), vec![1, 2, 3, 4]);
        assert_eq!(flat.plane_count(), 2);
        assert_eq!(flat.active_plane_idx(), 1);
        assert_eq!(flat.active_workspace_idx(), Some(0));
        assert_eq!(flat.active_plane_id(), Some(second_id));

        assert_eq!(flat.remove(0), 1);
        assert_eq!(flat.remove(1), 3);
        assert_eq!(flat.remove(0), 2);
        assert_eq!(flat.plane_count(), 1);
        assert_eq!(flat.active_plane_id(), Some(second_id));
        assert_eq!(flat.iter().copied().collect::<Vec<_>>(), vec![4]);

        assert_eq!(flat.remove(0), 4);
        assert!(flat.is_empty());

        flat.push(5);
        assert_eq!(flat.plane_count(), 1);
        assert_eq!(flat.active_plane_idx(), 0);
        assert!(flat.active_plane_id().is_some());
        assert_eq!(flat.iter().copied().collect::<Vec<_>>(), vec![5]);
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
