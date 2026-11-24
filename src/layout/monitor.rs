use std::cmp::min;
use std::iter::zip;
use std::rc::Rc;
use std::time::Duration;

use niri_config::{CornerRadius, LayoutPart};
use smithay::backend::renderer::element::utils::{
    CropRenderElement, Relocate, RelocateRenderElement, RescaleRenderElement,
};
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Size};

use super::insert_hint_element::{InsertHintElement, InsertHintRenderElement};
use super::plane::{NoOutputWorkspacePlanes, PlaneError, PlaneId, WorkspacePlane, WorkspacePlanes};
use super::scrolling::{Column, ColumnWidth};
use super::tile::Tile;
use super::workspace::{
    compute_working_area, OutputId, Workspace, WorkspaceAddWindowTarget, WorkspaceId,
    WorkspaceRenderElement,
};
use super::{compute_overview_zoom, ActivateWindow, HitType, LayoutElement, Options};
use crate::animation::{Animation, Clock};
use crate::input::swipe_tracker::SwipeTracker;
use crate::niri_render_elements;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::shadow::ShadowRenderElement;
use crate::render_helpers::solid_color::SolidColorRenderElement;
use crate::render_helpers::xray::XrayPos;
use crate::render_helpers::RenderCtx;
use crate::rubber_band::RubberBand;
use crate::utils::transaction::Transaction;
use crate::utils::{
    output_size, round_logical_in_physical, round_logical_in_physical_max1, ResizeEdge,
};

/// Amount of touchpad movement to scroll the height of one workspace.
const WORKSPACE_GESTURE_MOVEMENT: f64 = 300.;

const WORKSPACE_GESTURE_RUBBER_BAND: RubberBand = RubberBand {
    stiffness: 0.5,
    limit: 0.05,
};

/// Amount of DnD edge scrolling to scroll the height of one workspace.
///
/// This constant is tied to the default dnd-edge-workspace-switch max-speed setting.
const WORKSPACE_DND_EDGE_SCROLL_MOVEMENT: f64 = 1500.;

#[derive(Debug)]
pub struct Monitor<W: LayoutElement> {
    /// Output for this monitor.
    pub(super) output: Output,
    /// Cached name of the output.
    output_name: String,
    /// Latest known scale for this output.
    scale: smithay::output::Scale,
    /// Latest known size for this output.
    view_size: Size<f64, Logical>,
    /// Latest known working area for this output.
    ///
    /// Not rounded to physical pixels.
    // FIXME: since this is used for things like DnD scrolling edges in the overview, ideally this
    // should only consider overlay and top layer-shell surfaces. However, Smithay doesn't easily
    // let you do this at the moment.
    working_area: Rectangle<f64, Logical>,
    /// Depth planes for this monitor. Each plane always contains at least one
    /// workspace. Existing Y-axis operations dereference to the active plane.
    pub(super) workspaces: WorkspacePlanes<Workspace<W>>,
    /// ID of the previously active workspace.
    pub(super) previous_workspace_id: Option<WorkspaceId>,
    /// In-progress switch between workspaces.
    pub(super) workspace_switch: Option<WorkspaceSwitch>,
    /// Indication where an interactively-moved window is about to be placed.
    pub(super) insert_hint: Option<InsertHint>,
    /// Insert hint element for rendering.
    insert_hint_element: InsertHintElement,
    /// Location to render the insert hint element.
    insert_hint_render_loc: Option<InsertHintRenderLoc>,
    /// Whether the overview is open.
    pub(super) overview_open: bool,
    /// Progress of the overview zoom animation, 1 is fully in overview.
    overview_progress: Option<OverviewProgress>,
    /// Clock for driving animations.
    pub(super) clock: Clock,
    /// Configurable properties of the layout as received from the parent layout.
    pub(super) base_options: Rc<Options>,
    /// Configurable properties of the layout.
    pub(super) options: Rc<Options>,
    /// Layout config overrides for this monitor.
    layout_config: Option<niri_config::LayoutPart>,
}

#[derive(Debug)]
pub enum WorkspaceSwitch {
    Animation(Animation),
    Gesture(WorkspaceSwitchGesture),
}

#[derive(Debug)]
pub struct WorkspaceSwitchGesture {
    /// Index of the workspace where the gesture was started.
    center_idx: usize,
    /// Fractional workspace index where the gesture was started.
    ///
    /// Can differ from center_idx when starting a gesture in the middle between workspaces, for
    /// example by "catching" an animation.
    start_idx: f64,
    /// Current, fractional workspace index.
    pub(super) current_idx: f64,
    /// Animation for the extra offset to the current position.
    ///
    /// For example, if there's a workspace switch during a DnD scroll.
    animation: Option<Animation>,
    tracker: SwipeTracker,
    /// Whether the gesture is controlled by the touchpad.
    is_touchpad: bool,
    /// Whether the gesture is clamped to +-1 workspace around the center.
    is_clamped: bool,

    // If this gesture is for drag-and-drop scrolling, this is the last event's unadjusted
    // timestamp.
    dnd_last_event_time: Option<Duration>,
    // Time when the drag-and-drop scroll delta became non-zero, used for debouncing.
    //
    // If `None` then the scroll delta is currently zero.
    dnd_nonzero_start_time: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InsertPosition {
    NewColumn(usize),
    InColumn(usize, usize),
    Floating,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InsertWorkspace {
    Existing(WorkspaceId),
    NewAt(usize),
}

#[derive(Debug)]
pub(super) struct InsertHint {
    pub workspace: InsertWorkspace,
    pub position: InsertPosition,
    pub corner_radius: CornerRadius,
}

#[derive(Debug, Clone, Copy)]
struct InsertHintRenderLoc {
    workspace: InsertWorkspace,
    location: Point<f64, Logical>,
}

#[derive(Debug)]
pub(super) enum OverviewProgress {
    Animation(Animation),
    Value(f64),
}

/// Where to put a newly added window.
#[derive(Debug, Default, PartialEq, Eq)]
pub enum MonitorAddWindowTarget<'a, W: LayoutElement> {
    /// No particular preference.
    #[default]
    Auto,
    /// On this workspace.
    Workspace {
        /// Id of the target workspace.
        id: WorkspaceId,
        /// Override where the window will open as a new column.
        column_idx: Option<usize>,
    },
    /// Next to this existing window.
    NextTo(&'a W::Id),
}

impl<'a, W: LayoutElement> Copy for MonitorAddWindowTarget<'a, W> {}

impl<'a, W: LayoutElement> Clone for MonitorAddWindowTarget<'a, W> {
    fn clone(&self) -> Self {
        *self
    }
}

niri_render_elements! {
    MonitorInnerRenderElement<R> => {
        Workspace = CropRenderElement<WorkspaceRenderElement<R>>,
        InsertHint = CropRenderElement<InsertHintRenderElement>,
        UncroppedInsertHint = InsertHintRenderElement,
        Shadow = ShadowRenderElement,
        SolidColor = SolidColorRenderElement,
    }
}

pub type MonitorRenderElement<R> =
    RelocateRenderElement<RescaleRenderElement<MonitorInnerRenderElement<R>>>;

impl WorkspaceSwitch {
    pub fn current_idx(&self) -> f64 {
        match self {
            WorkspaceSwitch::Animation(anim) => anim.value(),
            WorkspaceSwitch::Gesture(gesture) => {
                gesture.current_idx + gesture.animation.as_ref().map_or(0., |anim| anim.value())
            }
        }
    }

    pub fn target_idx(&self) -> f64 {
        match self {
            WorkspaceSwitch::Animation(anim) => anim.to(),
            WorkspaceSwitch::Gesture(gesture) => gesture.current_idx,
        }
    }

    pub fn offset(&mut self, delta: isize) {
        match self {
            WorkspaceSwitch::Animation(anim) => anim.offset(delta as f64),
            WorkspaceSwitch::Gesture(gesture) => {
                if delta >= 0 {
                    gesture.center_idx += delta as usize;
                } else {
                    gesture.center_idx -= (-delta) as usize;
                }
                gesture.start_idx += delta as f64;
                gesture.current_idx += delta as f64;
            }
        }
    }

    fn is_animation_ongoing(&self) -> bool {
        match self {
            WorkspaceSwitch::Animation(_) => true,
            WorkspaceSwitch::Gesture(gesture) => gesture.animation.is_some(),
        }
    }
}

impl WorkspaceSwitchGesture {
    fn min_max(&self, workspace_count: usize) -> (f64, f64) {
        if self.is_clamped {
            let min = self.center_idx.saturating_sub(1) as f64;
            let max = (self.center_idx + 1).min(workspace_count - 1) as f64;
            (min, max)
        } else {
            (0., (workspace_count - 1) as f64)
        }
    }

    fn animate_from(&mut self, from: f64, clock: Clock, config: niri_config::Animation) {
        let current = self.animation.as_ref().map_or(0., Animation::value);
        self.animation = Some(Animation::new(clock, from + current, 0., 0., config));
    }
}

impl InsertWorkspace {
    fn existing_id(self) -> Option<WorkspaceId> {
        match self {
            InsertWorkspace::Existing(id) => Some(id),
            InsertWorkspace::NewAt(_) => None,
        }
    }
}

impl OverviewProgress {
    pub fn value(&self) -> f64 {
        match self {
            OverviewProgress::Animation(anim) => anim.value(),
            OverviewProgress::Value(v) => *v,
        }
    }

    pub fn clamped_value(&self) -> f64 {
        match self {
            OverviewProgress::Animation(anim) => anim.clamped_value(),
            OverviewProgress::Value(v) => *v,
        }
    }
}

impl From<&super::OverviewProgress> for OverviewProgress {
    fn from(value: &super::OverviewProgress) -> Self {
        match value {
            super::OverviewProgress::Animation(anim) => Self::Animation(anim.clone()),
            super::OverviewProgress::Gesture(gesture) => Self::Value(gesture.value),
            super::OverviewProgress::Open => Self::Value(1.),
        }
    }
}

impl<W: LayoutElement> Monitor<W> {
    pub fn new(
        output: Output,
        workspaces: Vec<Workspace<W>>,
        ws_id_to_activate: Option<WorkspaceId>,
        clock: Clock,
        base_options: Rc<Options>,
        layout_config: Option<LayoutPart>,
    ) -> Self {
        let workspaces = if workspaces.is_empty() {
            WorkspacePlanes::empty()
        } else {
            let plane = WorkspacePlane::new(PlaneId::next(), workspaces, 0)
                .expect("the supplied plane must contain workspaces");
            WorkspacePlanes::new(plane)
        };

        Self::new_with_planes(
            output,
            workspaces,
            ws_id_to_activate,
            clock,
            base_options,
            layout_config,
        )
    }

    /// Creates a monitor while preserving its existing ordered depth planes.
    pub fn new_with_planes(
        output: Output,
        mut workspaces: WorkspacePlanes<Workspace<W>>,
        ws_id_to_activate: Option<WorkspaceId>,
        clock: Clock,
        base_options: Rc<Options>,
        layout_config: Option<LayoutPart>,
    ) -> Self {
        let options =
            Rc::new(Options::clone(&base_options).with_merged_layout(layout_config.as_ref()));

        let scale = output.current_scale();
        let view_size = output_size(&output);
        let working_area = compute_working_area(&output);

        let needs_initial_workspace = workspaces.is_empty();
        if needs_initial_workspace {
            let workspace = Workspace::new(output.clone(), clock.clone(), options.clone());
            let plane = WorkspacePlane::new(PlaneId::next(), vec![workspace], 0)
                .expect("a new monitor plane must contain one workspace");
            workspaces
                .push_plane(plane)
                .expect("a newly generated plane ID must be unique");
        }

        let mut plane_to_activate = None;
        for (plane_idx, plane) in workspaces.iter_planes_mut().enumerate() {
            let mut active_workspace_idx = plane.active_workspace_idx();

            for (idx, ws) in plane.workspaces_mut().iter_mut().enumerate() {
                // Empty workspaces are valid Y-coordinate placeholders. Keep
                // them when restoring a plane so every plane can share the
                // same active vertical position without compressing its rows.
                if ws.original_output.is_empty() {
                    ws.original_output = OutputId::new(&output);
                }
                ws.set_output(Some(output.clone()));
                ws.update_config(options.clone());

                if ws_id_to_activate.is_some_and(|id| ws.id() == id) {
                    active_workspace_idx = idx;
                    plane_to_activate = Some(plane_idx);
                }
            }

            plane
                .activate_workspace(active_workspace_idx)
                .expect("the plane's active workspace index must be valid");

            // New boundary workspaces belong to the monitor's output, not
            // necessarily to the first workspace in this plane. A no-output
            // collection can contain workspaces configured for several outputs.
            let plane_original_output = OutputId::new(&output);

            if options.layout.empty_workspace_above_first
                && !(needs_initial_workspace && plane_idx == 0)
                && plane
                    .workspaces()
                    .first()
                    .is_some_and(|workspace| workspace.has_windows_or_name())
            {
                let mut ws = Workspace::new(output.clone(), clock.clone(), options.clone());
                ws.original_output = plane_original_output.clone();
                plane
                    .insert_workspace(0, ws)
                    .expect("inserting the first workspace must succeed");
            }

            if !needs_initial_workspace
                && plane
                    .workspaces()
                    .last()
                    .is_some_and(|workspace| workspace.has_windows_or_name())
            {
                let mut ws = Workspace::new(output.clone(), clock.clone(), options.clone());
                ws.original_output = plane_original_output;
                plane.push_workspace(ws);
            }
        }

        if let Some(plane_idx) = plane_to_activate {
            workspaces
                .activate_plane(plane_idx)
                .expect("the matching workspace plane index must be valid");
        }

        // Reconnected and restored planes may have been saved with different
        // local selections by an older session. Normalize them to one monitor-
        // wide vertical position before exposing the monitor.
        let shared_active_workspace_idx = workspaces.active_workspace_idx();
        for plane in workspaces.iter_planes_mut() {
            while plane.workspace_count() <= shared_active_workspace_idx {
                let mut workspace =
                    Workspace::new(output.clone(), clock.clone(), options.clone());
                workspace.original_output = OutputId::new(&output);
                plane.push_workspace(workspace);
            }
            plane
                .activate_workspace(shared_active_workspace_idx)
                .expect("every plane must contain the shared active workspace");
        }

        Self {
            output_name: output.name(),
            output,
            scale,
            view_size,
            working_area,
            workspaces,
            previous_workspace_id: None,
            insert_hint: None,
            insert_hint_element: InsertHintElement::new(options.layout.insert_hint),
            insert_hint_render_loc: None,
            overview_open: false,
            overview_progress: None,
            workspace_switch: None,
            clock,
            base_options,
            options,
            layout_config,
        }
    }

    /// Transfers this monitor's non-empty planes to the no-output state.
    pub fn into_planes(self) -> NoOutputWorkspacePlanes<Workspace<W>> {
        let mut planes = self.workspaces;

        // Preserve empty workspaces up to each plane's active Y coordinate.
        // They may be alignment placeholders for the monitor-wide shared
        // workspace position, so removing them would shift later workspaces
        // when the output is disconnected and restored. Keep content beyond
        // the active row as well, plus at most one empty trailing boundary.
        for plane in planes.iter_planes_mut() {
            let last_non_empty_workspace_idx = plane
                .workspaces()
                .iter()
                .rposition(|workspace| workspace.has_windows_or_name());
            let keep_workspace_count = (plane.active_workspace_idx() + 1).max(
                last_non_empty_workspace_idx.map_or(1, |idx| idx.saturating_add(2)),
            );

            while plane.workspace_count() > keep_workspace_count {
                let last_idx = plane.workspace_count() - 1;
                plane
                    .remove_workspace(last_idx)
                    .expect("trailing workspace must exist");
            }
        }

        for (_, _, workspace) in planes.iter_workspaces_mut() {
            workspace.set_output(None);
        }

        NoOutputWorkspacePlanes::from_planes(planes)
    }

    pub fn output(&self) -> &Output {
        &self.output
    }

    pub fn output_name(&self) -> &String {
        &self.output_name
    }

    /// Restores the monitor invariant after extracting all of its planes.
    ///
    /// A connected monitor must always contain at least one plane and one
    /// workspace, even when all workspaces were routed to another output.
    pub(super) fn ensure_workspace_plane(&mut self) {
        if self.workspaces.is_empty() {
            let mut workspace = Workspace::new(
                self.output.clone(),
                self.clock.clone(),
                self.options.clone(),
            );
            workspace.original_output = OutputId::new(&self.output);
            let plane = WorkspacePlane::new(PlaneId::next(), vec![workspace], 0)
                .expect("a monitor fallback plane must contain one workspace");
            self.workspaces
                .push_plane(plane)
                .expect("a newly generated plane ID must be unique");
        }
    }

    /// Restores the required empty boundary workspaces on every plane.
    ///
    /// Workspace routing can extract a plane's previous boundary workspace
    /// when it belonged to another output. The remaining plane still needs
    /// its own empty end workspace (and, when configured, an empty first one).
    pub(super) fn ensure_workspace_boundaries(&mut self) {
        let output = self.output.clone();
        let output_id = OutputId::new(&output);
        let clock = self.clock.clone();
        let options = self.options.clone();
        let empty_workspace_above_first = options.layout.empty_workspace_above_first;

        for plane in self.workspaces.iter_planes_mut() {
            if empty_workspace_above_first
                && plane
                    .workspaces()
                    .first()
                    .is_some_and(|workspace| workspace.has_windows_or_name())
            {
                let mut workspace = Workspace::new(output.clone(), clock.clone(), options.clone());
                workspace.original_output = output_id.clone();
                plane
                    .insert_workspace(0, workspace)
                    .expect("inserting the first workspace must succeed");
            }

            if plane
                .workspaces()
                .last()
                .is_some_and(|workspace| workspace.has_windows_or_name())
            {
                let mut workspace = Workspace::new(output.clone(), clock.clone(), options.clone());
                workspace.original_output = output_id.clone();
                plane.push_workspace(workspace);
            }
        }

        let active_workspace_idx = self.active_workspace_idx();
        self.set_active_workspace_idx(active_workspace_idx);
    }

    /// Returns the number of depth planes on this monitor.
    pub fn plane_count(&self) -> usize {
        self.workspaces.plane_count()
    }

    /// Returns the active plane's index in depth order.
    pub fn active_plane_idx(&self) -> usize {
        self.workspaces.active_plane_idx()
    }

    /// Returns the active plane's stable identity.
    pub fn active_plane_id(&self) -> PlaneId {
        self.workspaces.active_plane().id()
    }

    /// Returns whether the active plane contains no windows and no named workspaces.
    fn active_plane_is_empty(&self) -> bool {
        self.workspaces
            .active_plane()
            .workspaces()
            .iter()
            .all(|workspace| !workspace.has_windows_or_name())
    }

    /// Inserts an existing plane into this monitor.
    ///
    /// Workspaces are updated to use this monitor's output and current options.
    pub fn insert_plane(
        &mut self,
        index: usize,
        mut plane: WorkspacePlane<Workspace<W>>,
    ) -> Result<(), PlaneError> {
        for workspace in plane.workspaces_mut() {
            workspace.set_output(Some(self.output.clone()));
            workspace.update_config(self.options.clone());
        }

        self.workspaces.insert_plane(index, plane)?;
        let active_workspace_idx = self.active_workspace_idx();
        self.set_active_workspace_idx(active_workspace_idx);
        Ok(())
    }

    /// Creates a new empty plane immediately after the active plane and activates it.
    pub fn add_plane(&mut self) -> PlaneId {
        self.add_plane_at(self.active_plane_idx() + 1)
    }

    /// Creates and activates an empty plane at the requested depth-order index.
    ///
    /// Its workspace sequence and current vertical selection match the monitor's
    /// current workspace position, so a new plane starts at the same Y coordinate.
    fn add_plane_at(&mut self, index: usize) -> PlaneId {
        let active_workspace_idx = self.active_workspace_idx();
        // Only materialize the rows needed to reach the shared Y position,
        // plus one trailing empty workspace. Rows before the selected index
        // are alignment padding for a newly-created Plane.
        let workspace_count = active_workspace_idx + 2;

        let mut workspaces = Vec::with_capacity(workspace_count);
        for _ in 0..workspace_count {
            let mut workspace = Workspace::new(
                self.output.clone(),
                self.clock.clone(),
                self.options.clone(),
            );
            workspace.original_output = OutputId::new(&self.output);
            workspaces.push(workspace);
        }

        let plane = WorkspacePlane::new(PlaneId::next(), workspaces, active_workspace_idx)
            .expect("a new plane must contain the active workspace");
        let plane_id = plane.id();

        self.insert_plane(index, plane)
            .expect("a newly generated plane ID must be unique");
        self.activate_plane(index)
            .expect("the inserted plane index must be valid");

        plane_id
    }

    /// Activates a plane by its depth-order index.
    ///
    /// All planes on this monitor share one active vertical workspace index.
    pub fn activate_plane(&mut self, index: usize) -> Result<(), PlaneError> {
        if index != self.active_plane_idx() {
            let active_workspace_idx = self.active_workspace_idx();
            self.workspaces.activate_plane(index)?;
            self.set_active_workspace_idx(active_workspace_idx);
            self.workspace_switch = None;
            self.previous_workspace_id = None;
        }

        Ok(())
    }

    /// Focuses the adjacent plane farther back in depth order.
    ///
    /// Plane indices increase toward the front. Moving past the back edge
    /// creates a new plane when the current boundary plane has been used;
    /// an unused empty boundary plane is kept instead of accumulating blanks.
    pub fn focus_plane_backward(&mut self) {
        let index = self.active_plane_idx();
        if index == 0 {
            // Keep a single empty boundary plane. It becomes eligible for
            // another plane only after it is used.
            if !self.active_plane_is_empty() {
                self.add_plane_at(0);
            }
        } else {
            self.activate_plane(index - 1)
                .expect("the previous plane index must be valid");
        }
    }

    /// Focuses the adjacent plane closer to the front in depth order.
    ///
    /// Moving past the front edge creates a new plane when the current boundary
    /// plane has been used; an unused empty boundary plane is kept in place.
    pub fn focus_plane_forward(&mut self) {
        let index = self.active_plane_idx() + 1;
        if index < self.plane_count() {
            self.activate_plane(index)
                .expect("the next plane index must be valid");
        } else if !self.active_plane_is_empty() {
            // Keep a single empty boundary plane until it receives content.
            self.add_plane();
        }
    }

    /// Moves the active plane one step farther back in depth order.
    ///
    /// The active plane and its selected workspace remain active. Returns
    /// whether the plane changed position.
    pub fn move_active_plane_backward(&mut self) -> bool {
        let index = self.active_plane_idx();
        if index == 0 {
            return false;
        }

        self.workspaces.move_plane(index, index - 1).is_ok()
    }

    /// Moves the active plane one step closer to the front in depth order.
    ///
    /// The active plane and its selected workspace remain active. Returns
    /// whether the plane changed position.
    pub fn move_active_plane_forward(&mut self) -> bool {
        let index = self.active_plane_idx();
        let next_index = index + 1;
        if next_index >= self.plane_count() {
            return false;
        }

        self.workspaces.move_plane(index, next_index).is_ok()
    }

    /// Removes a plane and returns it with all of its workspaces.
    ///
    /// The final plane cannot be removed. If the active plane is removed, the
    /// neighboring plane becomes active and its previously active workspace is
    /// retained.
    pub fn remove_plane(
        &mut self,
        index: usize,
    ) -> Result<WorkspacePlane<Workspace<W>>, PlaneError> {
        let active_plane_id = self.active_plane_id();
        let active_workspace_idx = self.active_workspace_idx();
        let removed = self.workspaces.remove_plane(index)?;

        if self.active_plane_id() != active_plane_id {
            self.workspace_switch = None;
            self.previous_workspace_id = None;
        }

        self.set_active_workspace_idx(active_workspace_idx);
        Ok(removed)
    }

    pub fn active_workspace_idx(&self) -> usize {
        self.workspaces.active_workspace_idx()
    }

    pub(super) fn set_active_workspace_idx(&mut self, index: usize) {
        let output = self.output.clone();
        let clock = self.clock.clone();
        let options = self.options.clone();

        // Keep the active Y coordinate shared across all depth planes. A plane
        // that has not reached this workspace yet receives empty workspaces up
        // to the requested index.
        for plane_idx in 0..self.workspaces.plane_count() {
            loop {
                let workspace_count = self
                    .workspaces
                    .plane(plane_idx)
                    .expect("the plane index must be valid")
                    .workspace_count();
                if workspace_count > index {
                    break;
                }

                let mut workspace =
                    Workspace::new(output.clone(), clock.clone(), options.clone());
                workspace.original_output = OutputId::new(&output);
                self.workspaces
                    .plane_mut(plane_idx)
                    .expect("the plane index must be valid")
                    .push_workspace(workspace);
            }
        }

        for plane in self.workspaces.iter_planes_mut() {
            plane
                .activate_workspace(index)
                .expect("the shared active workspace index must exist on every plane");
        }
    }

    pub fn active_workspace_ref(&self) -> &Workspace<W> {
        &self.workspaces[self.active_workspace_idx()]
    }

    pub fn find_named_workspace(&self, workspace_name: &str) -> Option<&Workspace<W>> {
        self.workspaces.iter_workspaces().find_map(|(_, _, ws)| {
            ws.name
                .as_ref()
                .is_some_and(|name| name.eq_ignore_ascii_case(workspace_name))
                .then_some(ws)
        })
    }

    pub fn find_named_workspace_index(&self, workspace_name: &str) -> Option<(PlaneId, usize)> {
        self.workspaces
            .iter_workspaces()
            .find_map(|(plane_id, workspace_idx, ws)| {
                ws.name
                    .as_ref()
                    .is_some_and(|name| name.eq_ignore_ascii_case(workspace_name))
                    .then_some((plane_id, workspace_idx))
            })
    }

    pub fn active_workspace(&mut self) -> &mut Workspace<W> {
        let index = self.active_workspace_idx();
        &mut self.workspaces[index]
    }

    pub fn windows(&self) -> impl Iterator<Item = &W> {
        self.workspaces
            .iter_workspaces()
            .flat_map(|(_, _, ws)| ws.windows())
    }

    pub fn has_window(&self, window: &W::Id) -> bool {
        self.windows().any(|win| win.id() == window)
    }

    pub fn add_workspace_at(&mut self, idx: usize) {
        let workspace = Workspace::new(
            self.output.clone(),
            self.clock.clone(),
            self.options.clone(),
        );

        self.workspaces.insert(idx, workspace);
        if idx <= self.active_workspace_idx() {
            self.set_active_workspace_idx(self.active_workspace_idx() + 1);
        }

        if let Some(switch) = &mut self.workspace_switch {
            if idx as f64 <= switch.target_idx() {
                switch.offset(1);
            }
        }
    }

    pub fn add_workspace_top(&mut self) {
        self.add_workspace_at(0);
    }

    pub fn add_workspace_bottom(&mut self) {
        self.add_workspace_at(self.workspaces.len());
    }

    pub(super) fn add_workspace_at_in_plane(&mut self, plane_idx: usize, idx: usize) {
        if plane_idx == self.workspaces.active_plane_idx() {
            self.add_workspace_at(idx);
            return;
        }

        let workspace = Workspace::new(
            self.output.clone(),
            self.clock.clone(),
            self.options.clone(),
        );
        let shared_active_workspace_idx = self.active_workspace_idx();
        let plane = self
            .workspaces
            .plane_mut(plane_idx)
            .expect("the plane index must be valid");
        plane
            .insert_workspace(idx, workspace)
            .expect("the workspace index must be valid");
        plane
            .activate_workspace(shared_active_workspace_idx)
            .expect("every plane must retain the shared active workspace index");
    }

    fn add_workspace_bottom_in_plane(&mut self, plane_idx: usize) {
        let idx = self
            .workspaces
            .plane(plane_idx)
            .expect("the plane index must be valid")
            .workspace_count();
        self.add_workspace_at_in_plane(plane_idx, idx);
    }

    pub fn activate_workspace(&mut self, idx: usize) {
        self.activate_workspace_with_anim_config(idx, None);
    }

    pub fn activate_workspace_with_anim_config(
        &mut self,
        idx: usize,
        config: Option<niri_config::Animation>,
    ) {
        // FIXME: also compute and use current velocity.
        let current_idx = self.workspace_render_idx();

        if self.active_workspace_idx() != idx {
            self.previous_workspace_id = Some(self.workspaces[self.active_workspace_idx()].id());
        }

        let prev_active_idx = self.active_workspace_idx();
        self.set_active_workspace_idx(idx);

        let config = config.unwrap_or(self.options.animations.workspace_switch.0);

        match &mut self.workspace_switch {
            // During a DnD scroll, we want to visually animate even if idx matches the active idx.
            Some(WorkspaceSwitch::Gesture(gesture)) if gesture.dnd_last_event_time.is_some() => {
                gesture.center_idx = idx;

                // Adjust start_idx to make current_idx point at idx.
                let current_pos = gesture.current_idx - gesture.start_idx;
                gesture.start_idx = idx as f64 - current_pos;
                let prev_current_idx = gesture.current_idx;
                gesture.current_idx = idx as f64;

                let current_idx_delta = gesture.current_idx - prev_current_idx;
                gesture.animate_from(-current_idx_delta, self.clock.clone(), config);
            }
            _ => {
                // Don't animate if nothing changed.
                if prev_active_idx == idx {
                    return;
                }

                self.workspace_switch = Some(WorkspaceSwitch::Animation(Animation::new(
                    self.clock.clone(),
                    current_idx,
                    idx as f64,
                    0.,
                    config,
                )));
            }
        }
    }

    pub(super) fn resolve_add_window_target<'a>(
        &self,
        target: MonitorAddWindowTarget<'a, W>,
    ) -> (usize, usize, WorkspaceAddWindowTarget<'a, W>) {
        match target {
            MonitorAddWindowTarget::Auto => (
                self.workspaces.active_plane_idx(),
                self.active_workspace_idx(),
                WorkspaceAddWindowTarget::Auto,
            ),
            MonitorAddWindowTarget::Workspace { id, column_idx } => {
                let (plane_id, workspace_idx, _) = self
                    .workspaces
                    .iter_workspaces()
                    .find_map(|(plane_id, workspace_idx, ws)| {
                        (ws.id() == id).then_some((plane_id, workspace_idx, ws))
                    })
                    .expect("the target workspace must exist");
                let plane_idx = self
                    .workspaces
                    .index_of(plane_id)
                    .expect("the target workspace's plane must exist");
                let target = if let Some(column_idx) = column_idx {
                    WorkspaceAddWindowTarget::NewColumnAt(column_idx)
                } else {
                    WorkspaceAddWindowTarget::Auto
                };
                (plane_idx, workspace_idx, target)
            }
            MonitorAddWindowTarget::NextTo(win_id) => {
                let (plane_id, workspace_idx, _) = self
                    .workspaces
                    .iter_workspaces()
                    .find_map(|(plane_id, workspace_idx, ws)| {
                        ws.has_window(win_id)
                            .then_some((plane_id, workspace_idx, ws))
                    })
                    .expect("the target window must exist");
                let plane_idx = self
                    .workspaces
                    .index_of(plane_id)
                    .expect("the target window's plane must exist");
                (
                    plane_idx,
                    workspace_idx,
                    WorkspaceAddWindowTarget::NextTo(win_id),
                )
            }
        }
    }

    pub fn add_window(
        &mut self,
        window: W,
        target: MonitorAddWindowTarget<W>,
        activate: ActivateWindow,
        width: ColumnWidth,
        is_full_width: bool,
        is_floating: bool,
    ) {
        // Currently, everything a workspace sets on a Tile is the same across all workspaces of a
        // monitor. So we can use any workspace, not necessarily the exact target workspace.
        let tile = self.workspaces[0].make_tile(window);

        self.add_tile(
            tile,
            target,
            activate,
            true,
            width,
            is_full_width,
            is_floating,
        );
    }

    pub fn add_column(&mut self, mut workspace_idx: usize, column: Column<W>, activate: bool) {
        let workspace = &mut self.workspaces[workspace_idx];

        workspace.add_column(column, activate);

        // After adding a new window, workspace becomes this output's own.
        if workspace.name().is_none() {
            workspace.original_output = OutputId::new(&self.output);
        }

        if workspace_idx == self.workspaces.len() - 1 {
            self.add_workspace_bottom();
        }
        if self.options.layout.empty_workspace_above_first && workspace_idx == 0 {
            self.add_workspace_top();
            workspace_idx += 1;
        }

        if activate {
            self.activate_workspace(workspace_idx);
        }
    }

    fn add_column_to_plane(
        &mut self,
        plane_idx: usize,
        mut workspace_idx: usize,
        column: Column<W>,
        activate: bool,
    ) {
        let plane_workspace_count = self
            .workspaces
            .plane(plane_idx)
            .expect("the target plane must exist")
            .workspace_count();

        {
            let workspace = self
                .workspaces
                .plane_mut(plane_idx)
                .expect("the target plane must exist")
                .workspaces_mut()
                .get_mut(workspace_idx)
                .expect("the target workspace must exist");

            workspace.add_column(column, activate);

            // After adding a new column, workspace becomes this output's own.
            if workspace.name().is_none() {
                workspace.original_output = OutputId::new(&self.output);
            }
        }

        if workspace_idx == plane_workspace_count - 1 {
            self.add_workspace_bottom_in_plane(plane_idx);
        }
        if self.options.layout.empty_workspace_above_first && workspace_idx == 0 {
            self.add_workspace_at_in_plane(plane_idx, 0);
            workspace_idx += 1;
        }

        if activate {
            self.activate_plane(plane_idx)
                .expect("the target plane index must be valid");
            self.activate_workspace(workspace_idx);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_tile(
        &mut self,
        tile: Tile<W>,
        target: MonitorAddWindowTarget<W>,
        activate: ActivateWindow,
        // FIXME: Refactor ActivateWindow enum to make this better.
        allow_to_activate_workspace: bool,
        width: ColumnWidth,
        is_full_width: bool,
        is_floating: bool,
    ) {
        let (plane_idx, mut workspace_idx, target) = self.resolve_add_window_target(target);
        let plane_workspace_count = self
            .workspaces
            .plane(plane_idx)
            .expect("the target plane must exist")
            .workspace_count();

        {
            let workspace = self
                .workspaces
                .plane_mut(plane_idx)
                .expect("the target plane must exist")
                .workspaces_mut()
                .get_mut(workspace_idx)
                .expect("the target workspace must exist");

            workspace.add_tile(tile, target, activate, width, is_full_width, is_floating);

            // After adding a new window, workspace becomes this output's own.
            if workspace.name().is_none() {
                workspace.original_output = OutputId::new(&self.output);
            }
        }

        if workspace_idx == plane_workspace_count - 1 {
            // Insert a new empty workspace at the end of the target plane.
            self.add_workspace_bottom_in_plane(plane_idx);
        }

        if self.options.layout.empty_workspace_above_first && workspace_idx == 0 {
            self.add_workspace_at_in_plane(plane_idx, 0);
            workspace_idx += 1;
        }

        if allow_to_activate_workspace && activate.map_smart(|| false) {
            self.activate_plane(plane_idx)
                .expect("the target plane index must be valid");
            self.activate_workspace(workspace_idx);
        }
    }

    pub fn add_tile_to_column(
        &mut self,
        workspace_idx: usize,
        column_idx: usize,
        tile_idx: Option<usize>,
        tile: Tile<W>,
        activate: bool,
        // FIXME: Refactor ActivateWindow enum to make this better.
        allow_to_activate_workspace: bool,
    ) {
        let workspace = &mut self.workspaces[workspace_idx];

        workspace.add_tile_to_column(column_idx, tile_idx, tile, activate);

        // After adding a new window, workspace becomes this output's own.
        if workspace.name().is_none() {
            workspace.original_output = OutputId::new(&self.output);
        }

        // Since we're adding window to an existing column, the workspace isn't empty, and
        // therefore cannot be the last one, so we never need to insert a new empty workspace.

        if allow_to_activate_workspace && activate {
            self.activate_workspace(workspace_idx);
        }
    }

    pub fn clean_up_workspaces(&mut self) {
        assert!(self.workspace_switch.is_none());
        self.clean_up_plane_workspaces(self.workspaces.active_plane_idx());
    }

    pub(super) fn clean_up_plane_workspaces(&mut self, plane_idx: usize) {
        let empty_workspace_above_first = self.options.layout.empty_workspace_above_first;
        let range_start = if empty_workspace_above_first { 1 } else { 0 };
        let shared_active_workspace_idx = self.active_workspace_idx();
        {
            let plane = self
                .workspaces
                .plane_mut(plane_idx)
                .expect("the plane index must be valid");

            for idx in (range_start..plane.workspace_count() - 1).rev() {
                // Empty rows before the shared Y coordinate are alignment
                // padding. They must remain so every Plane maps the same
                // vertical position to the same workspace index.
                if idx <= shared_active_workspace_idx {
                    continue;
                }

                if !plane.workspaces()[idx].has_windows_or_name() {
                    plane
                        .remove_workspace(idx)
                        .expect("the workspace index must be valid");
                }
            }

            // Special case handling when empty_workspace_above_first is set and all workspaces
            // are empty.
            if empty_workspace_above_first && plane.workspace_count() == 2 {
                assert!(!plane.workspaces()[0].has_windows_or_name());
                assert!(!plane.workspaces()[1].has_windows_or_name());
                plane
                    .remove_workspace(1)
                    .expect("the trailing workspace must exist");
            }
        }

        // Cleanup may shift an inactive plane's local index. Reapply the
        // monitor-wide Y coordinate so switching planes never changes location.
        let active_workspace_idx = self.active_workspace_idx();
        self.set_active_workspace_idx(active_workspace_idx);
    }

    pub fn unname_workspace(&mut self, id: WorkspaceId) -> bool {
        let Some(plane_idx) = self
            .workspaces
            .iter_planes()
            .position(|plane| plane.workspaces().iter().any(|ws| ws.id() == id))
        else {
            return false;
        };

        let workspace = self
            .workspaces
            .plane_mut(plane_idx)
            .expect("the workspace's plane must exist")
            .workspaces_mut()
            .iter_mut()
            .find(|ws| ws.id() == id)
            .expect("the workspace must exist");
        workspace.unname();

        if self.workspace_switch.is_none() {
            self.clean_up_plane_workspaces(plane_idx);
        }

        true
    }

    pub fn remove_workspace_by_idx(&mut self, mut idx: usize) -> Workspace<W> {
        if idx == self.workspaces.len() - 1 {
            self.add_workspace_bottom();
        }
        if self.options.layout.empty_workspace_above_first && idx == 0 {
            self.add_workspace_top();
            idx += 1;
        }

        let mut ws = self.workspaces.remove(idx);
        ws.set_output(None);

        // For monitor current workspace removal, we focus previous rather than next (<= rather
        // than <). This is different from columns and tiles, but it lets move-workspace-to-monitor
        // back and forth to preserve position.
        if idx <= self.active_workspace_idx() && self.active_workspace_idx() > 0 {
            self.set_active_workspace_idx(self.active_workspace_idx() - 1);
        }

        self.workspace_switch = None;
        self.clean_up_workspaces();

        ws
    }

    pub fn insert_workspace(&mut self, mut ws: Workspace<W>, mut idx: usize, activate: bool) {
        ws.set_output(Some(self.output.clone()));
        ws.update_config(self.options.clone());

        // Don't insert past the last empty workspace.
        if idx == self.workspaces.len() {
            idx -= 1;
        }
        if idx == 0 && self.options.layout.empty_workspace_above_first {
            // Insert a new empty workspace on top to prepare for insertion of new workspace.
            self.add_workspace_top();
            idx += 1;
        }

        self.workspaces.insert(idx, ws);

        if idx <= self.active_workspace_idx() {
            self.set_active_workspace_idx(self.active_workspace_idx() + 1);
        }

        if activate {
            self.workspace_switch = None;
            self.activate_workspace(idx);
        }

        self.workspace_switch = None;
        self.clean_up_workspaces();
    }

    /// Appends planes transferred from a disconnected monitor.
    ///
    /// Each imported plane remains a separate depth layer, and its active
    /// vertical workspace is retained.
    pub fn append_planes(
        &mut self,
        planes: NoOutputWorkspacePlanes<Workspace<W>>,
        original_output: OutputId,
    ) {
        if planes.is_empty() {
            return;
        }

        let active_workspace_idx = self.active_workspace_idx();
        let empty_was_focused = active_workspace_idx == self.workspaces.len() - 1;
        let mut last_added_plane_idx = None;

        for mut plane in planes.into_planes().into_planes() {
            for workspace in plane.workspaces_mut() {
                workspace.set_output(Some(self.output.clone()));
                workspace.update_config(self.options.clone());
            }

            if self.options.layout.empty_workspace_above_first
                && plane.workspaces()[0].has_windows_or_name()
            {
                let mut empty = Workspace::new(
                    self.output.clone(),
                    self.clock.clone(),
                    self.options.clone(),
                );
                empty.original_output = original_output.clone();
                plane
                    .insert_workspace(0, empty)
                    .expect("inserting the first workspace must succeed");
            }

            if plane
                .workspaces()
                .last()
                .is_some_and(|workspace| workspace.has_windows_or_name())
            {
                let mut empty = Workspace::new(
                    self.output.clone(),
                    self.clock.clone(),
                    self.options.clone(),
                );
                // Keep generated boundary workspaces with the transferred plane so
                // reconnecting its original output restores the complete plane.
                empty.original_output = original_output.clone();
                plane.push_workspace(empty);
            }

            let plane_idx = self.workspaces.plane_count();
            self.workspaces
                .push_plane(plane)
                .expect("transferred plane IDs must be unique");
            last_added_plane_idx = Some(plane_idx);
        }

        // Imported planes join this monitor's shared Y coordinate while they
        // are attached here. Their saved active workspace IDs allow their
        // original vertical selection to be restored when their output returns.
        self.set_active_workspace_idx(active_workspace_idx);

        // Before planes existed, disconnected workspaces were inserted before
        // the primary monitor's trailing empty workspace. When that empty
        // workspace was focused, focus moved to the imported workspaces. Keep
        // that behavior by activating the last imported plane, but retain the
        // monitor's shared Y coordinate: activate_plane() already aligns the
        // imported plane to the current row.
        if empty_was_focused {
            if let Some(plane_idx) = last_added_plane_idx {
                self.activate_plane(plane_idx)
                    .expect("the last imported plane index must be valid");
            }
        }

        self.workspace_switch = None;
        self.clean_up_workspaces();
    }

    pub fn move_down_or_to_workspace_down(&mut self) {
        if !self.active_workspace().move_down() {
            self.move_to_workspace_down(true);
        }
    }

    pub fn move_up_or_to_workspace_up(&mut self) {
        if !self.active_workspace().move_up() {
            self.move_to_workspace_up(true);
        }
    }

    pub fn focus_window_or_workspace_down(&mut self) {
        if !self.active_workspace().focus_down() {
            self.switch_workspace_down();
        }
    }

    pub fn focus_window_or_workspace_up(&mut self) {
        if !self.active_workspace().focus_up() {
            self.switch_workspace_up();
        }
    }

    pub fn move_to_workspace_up(&mut self, focus: bool) {
        let source_workspace_idx = self.active_workspace_idx();

        let new_idx = source_workspace_idx.saturating_sub(1);
        if new_idx == source_workspace_idx {
            return;
        }
        let new_id = self.workspaces[new_idx].id();

        let workspace = &mut self.workspaces[source_workspace_idx];
        let Some(removed) = workspace.remove_active_tile(Transaction::new()) else {
            return;
        };

        let activate = if focus {
            ActivateWindow::Yes
        } else {
            ActivateWindow::Smart
        };

        self.add_tile(
            removed.tile,
            MonitorAddWindowTarget::Workspace {
                id: new_id,
                column_idx: None,
            },
            activate,
            true,
            removed.width,
            removed.is_full_width,
            removed.is_floating,
        );
    }

    pub fn move_to_workspace_down(&mut self, focus: bool) {
        let source_workspace_idx = self.active_workspace_idx();

        let new_idx = min(source_workspace_idx + 1, self.workspaces.len() - 1);
        if new_idx == source_workspace_idx {
            return;
        }
        let new_id = self.workspaces[new_idx].id();

        let workspace = &mut self.workspaces[source_workspace_idx];
        let Some(removed) = workspace.remove_active_tile(Transaction::new()) else {
            return;
        };

        let activate = if focus {
            ActivateWindow::Yes
        } else {
            ActivateWindow::Smart
        };

        self.add_tile(
            removed.tile,
            MonitorAddWindowTarget::Workspace {
                id: new_id,
                column_idx: None,
            },
            activate,
            true,
            removed.width,
            removed.is_full_width,
            removed.is_floating,
        );
    }

    pub fn move_to_workspace(
        &mut self,
        window: Option<&W::Id>,
        idx: usize,
        activate: ActivateWindow,
    ) {
        let (source_plane_id, source_workspace_idx) = if let Some(window) = window {
            self.workspaces
                .iter_workspaces()
                .find_map(|(plane_id, workspace_idx, workspace)| {
                    workspace
                        .has_window(window)
                        .then_some((plane_id, workspace_idx))
                })
                .unwrap()
        } else {
            (
                self.workspaces.active_plane().id(),
                self.active_workspace_idx(),
            )
        };
        let source_plane_idx = self
            .workspaces
            .index_of(source_plane_id)
            .expect("the source plane must still exist");

        let new_idx = min(idx, self.workspaces.len() - 1);
        let target_plane_id = self.workspaces.active_plane().id();
        if source_plane_id == target_plane_id && new_idx == source_workspace_idx {
            return;
        }
        let new_id = self.workspaces[new_idx].id();

        let activate = activate.map_smart(|| {
            window.is_none_or(|win| self.active_window().map(|win| win.id()) == Some(win))
        });

        let workspace = self
            .workspaces
            .plane_mut(source_plane_idx)
            .expect("the source plane index must be valid")
            .workspaces_mut()
            .get_mut(source_workspace_idx)
            .expect("the source workspace index must be valid");
        let transaction = Transaction::new();
        let removed = if let Some(window) = window {
            workspace.remove_tile(window, transaction)
        } else if let Some(removed) = workspace.remove_active_tile(transaction) {
            removed
        } else {
            return;
        };

        self.add_tile(
            removed.tile,
            MonitorAddWindowTarget::Workspace {
                id: new_id,
                column_idx: None,
            },
            if activate {
                ActivateWindow::Yes
            } else {
                ActivateWindow::No
            },
            true,
            removed.width,
            removed.is_full_width,
            removed.is_floating,
        );

        if self.workspace_switch.is_none() {
            self.clean_up_plane_workspaces(source_plane_idx);
        }
    }

    pub fn move_column_to_workspace_up(&mut self, activate: bool) {
        let source_workspace_idx = self.active_workspace_idx();

        let new_idx = source_workspace_idx.saturating_sub(1);
        if new_idx == source_workspace_idx {
            return;
        }

        let workspace = &mut self.workspaces[source_workspace_idx];
        if workspace.floating_is_active() {
            self.move_to_workspace_up(activate);
            return;
        }

        let Some(column) = workspace.remove_active_column() else {
            return;
        };

        self.add_column(new_idx, column, activate);
    }

    pub fn move_column_to_workspace_down(&mut self, activate: bool) {
        let source_workspace_idx = self.active_workspace_idx();

        let new_idx = min(source_workspace_idx + 1, self.workspaces.len() - 1);
        if new_idx == source_workspace_idx {
            return;
        }

        let workspace = &mut self.workspaces[source_workspace_idx];
        if workspace.floating_is_active() {
            self.move_to_workspace_down(activate);
            return;
        }

        let Some(column) = workspace.remove_active_column() else {
            return;
        };

        self.add_column(new_idx, column, activate);
    }

    pub fn move_column_to_workspace(&mut self, idx: usize, activate: bool) {
        let source_workspace_idx = self.active_workspace_idx();

        let new_idx = min(idx, self.workspaces.len() - 1);
        if new_idx == source_workspace_idx {
            return;
        }

        let workspace = &mut self.workspaces[source_workspace_idx];
        if workspace.floating_is_active() {
            let activate = if activate {
                ActivateWindow::Smart
            } else {
                ActivateWindow::No
            };
            self.move_to_workspace(None, idx, activate);
            return;
        }

        let Some(column) = workspace.remove_active_column() else {
            return;
        };

        self.add_column(new_idx, column, activate);
    }

    /// Moves the active column to a workspace on the specified plane.
    ///
    /// The source is always the currently active plane and workspace. The
    /// destination is resolved by stable plane identity, without activating
    /// that plane before the source column has been removed.
    pub fn move_column_to_workspace_on_plane(
        &mut self,
        target_plane_id: Option<PlaneId>,
        idx: usize,
        activate: bool,
    ) {
        let source_plane_idx = self.active_plane_idx();
        let source_workspace_idx = self.active_workspace_idx();
        let target_plane_idx = match target_plane_id {
            Some(id) => {
                let Some(index) = self.workspaces.index_of(id) else {
                    return;
                };
                index
            }
            None => source_plane_idx,
        };

        let target_workspace_count = self
            .workspaces
            .plane(target_plane_idx)
            .expect("the target plane must exist")
            .workspace_count();
        let new_idx = min(idx, target_workspace_count - 1);

        if target_plane_idx == source_plane_idx && new_idx == source_workspace_idx {
            return;
        }

        let source_workspace = self
            .workspaces
            .plane(source_plane_idx)
            .expect("the source plane must exist")
            .workspaces()
            .get(source_workspace_idx)
            .expect("the source workspace must exist");

        if source_workspace.floating_is_active() {
            let Some(window) = source_workspace
                .active_window()
                .map(|window| window.id().clone())
            else {
                return;
            };

            self.activate_plane(target_plane_idx)
                .expect("the target plane index must be valid");
            self.move_to_workspace(
                Some(&window),
                new_idx,
                if activate {
                    ActivateWindow::Yes
                } else {
                    ActivateWindow::No
                },
            );

            if !activate {
                self.activate_plane(source_plane_idx)
                    .expect("the source plane index must be valid");
            }
            return;
        }

        let Some(column) = self
            .workspaces
            .plane_mut(source_plane_idx)
            .expect("the source plane must exist")
            .workspaces_mut()
            .get_mut(source_workspace_idx)
            .expect("the source workspace must exist")
            .remove_active_column()
        else {
            return;
        };

        self.add_column_to_plane(target_plane_idx, new_idx, column, activate);
    }

    /// Moves the active column to the active workspace on an adjacent plane.
    ///
    /// If the active window is floating, the existing workspace-move behavior
    /// moves that window instead of a tiled column.
    pub fn move_active_column_to_plane(&mut self, target_plane_id: PlaneId, focus: bool) {
        let source_plane_idx = self.active_plane_idx();
        let Some(target_plane_idx) = self.workspaces.index_of(target_plane_id) else {
            return;
        };
        if source_plane_idx == target_plane_idx {
            return;
        }

        let target_workspace_idx = self
            .workspaces
            .plane(target_plane_idx)
            .expect("the target plane must exist")
            .active_workspace_idx();

        self.move_column_to_workspace_on_plane(Some(target_plane_id), target_workspace_idx, focus);
    }

    /// Moves the active column to the adjacent plane farther back.
    pub fn move_active_column_to_plane_backward(&mut self, focus: bool) {
        let Some(target_plane_idx) = self.active_plane_idx().checked_sub(1) else {
            return;
        };
        let Some(target_plane_id) = self
            .workspaces
            .plane(target_plane_idx)
            .map(|plane| plane.id())
        else {
            return;
        };

        self.move_active_column_to_plane(target_plane_id, focus);
    }

    /// Moves the active column to the adjacent plane closer to the front.
    pub fn move_active_column_to_plane_forward(&mut self, focus: bool) {
        let target_plane_idx = self.active_plane_idx() + 1;
        let Some(target_plane_id) = self
            .workspaces
            .plane(target_plane_idx)
            .map(|plane| plane.id())
        else {
            return;
        };

        self.move_active_column_to_plane(target_plane_id, focus);
    }

    /// Moves the active window to the active workspace on another plane.
    ///
    /// When focus is false, the window is moved without changing the active
    /// plane. Otherwise, focus follows the window to the destination plane.
    pub fn move_active_window_to_plane(&mut self, target_plane_id: PlaneId, focus: bool) {
        let Some(window_id) = self.active_window().map(|window| window.id().clone()) else {
            return;
        };

        let source_plane_idx = self.active_plane_idx();
        let Some(target_plane_idx) = self.workspaces.index_of(target_plane_id) else {
            return;
        };
        if source_plane_idx == target_plane_idx {
            return;
        }

        let target_workspace_idx = self
            .workspaces
            .plane(target_plane_idx)
            .expect("the target plane must exist")
            .active_workspace_idx();

        self.activate_plane(target_plane_idx)
            .expect("the target plane index must be valid");
        self.move_to_workspace(
            Some(&window_id),
            target_workspace_idx,
            if focus {
                ActivateWindow::Yes
            } else {
                ActivateWindow::No
            },
        );

        if !focus {
            self.activate_plane(source_plane_idx)
                .expect("the source plane index must be valid");
        }
    }

    /// Moves the active window to the adjacent plane farther back.
    pub fn move_active_window_to_plane_backward(&mut self, focus: bool) {
        let Some(target_plane_idx) = self.active_plane_idx().checked_sub(1) else {
            return;
        };
        let Some(target_plane_id) = self
            .workspaces
            .plane(target_plane_idx)
            .map(|plane| plane.id())
        else {
            return;
        };

        self.move_active_window_to_plane(target_plane_id, focus);
    }

    /// Moves the active window to the adjacent plane closer to the front.
    pub fn move_active_window_to_plane_forward(&mut self, focus: bool) {
        let target_plane_idx = self.active_plane_idx() + 1;
        let Some(target_plane_id) = self
            .workspaces
            .plane(target_plane_idx)
            .map(|plane| plane.id())
        else {
            return;
        };

        self.move_active_window_to_plane(target_plane_id, focus);
    }

    pub fn switch_workspace_up(&mut self) {
        let new_idx = match &self.workspace_switch {
            // During a DnD scroll, select the prev apparent workspace.
            Some(WorkspaceSwitch::Gesture(gesture)) if gesture.dnd_last_event_time.is_some() => {
                let current = gesture.current_idx;
                let new = current.ceil() - 1.;
                new.clamp(0., (self.workspaces.len() - 1) as f64) as usize
            }
            _ => self.active_workspace_idx().saturating_sub(1),
        };

        self.activate_workspace(new_idx);
    }

    pub fn switch_workspace_down(&mut self) {
        let new_idx = match &self.workspace_switch {
            // During a DnD scroll, select the next apparent workspace.
            Some(WorkspaceSwitch::Gesture(gesture)) if gesture.dnd_last_event_time.is_some() => {
                let current = gesture.current_idx;
                let new = current.floor() + 1.;
                new.clamp(0., (self.workspaces.len() - 1) as f64) as usize
            }
            _ => min(self.active_workspace_idx() + 1, self.workspaces.len() - 1),
        };

        self.activate_workspace(new_idx);
    }

    fn previous_workspace_idx(&self) -> Option<usize> {
        let id = self.previous_workspace_id?;
        self.workspaces.iter().position(|w| w.id() == id)
    }

    pub fn switch_workspace(&mut self, idx: usize) {
        self.activate_workspace(min(idx, self.workspaces.len() - 1));
    }

    pub fn switch_workspace_auto_back_and_forth(&mut self, idx: usize) {
        let idx = min(idx, self.workspaces.len() - 1);

        if idx == self.active_workspace_idx() {
            if let Some(prev_idx) = self.previous_workspace_idx() {
                self.switch_workspace(prev_idx);
            }
        } else {
            self.switch_workspace(idx);
        }
    }

    pub fn switch_workspace_previous(&mut self) {
        if let Some(idx) = self.previous_workspace_idx() {
            self.switch_workspace(idx);
        }
    }

    pub fn active_window(&self) -> Option<&W> {
        self.active_workspace_ref().active_window()
    }

    pub fn advance_animations(&mut self) {
        match &mut self.workspace_switch {
            Some(WorkspaceSwitch::Animation(anim)) => {
                if anim.is_done() {
                    self.workspace_switch = None;
                    self.clean_up_workspaces();
                }
            }
            Some(WorkspaceSwitch::Gesture(gesture)) => {
                // Make sure the last event time doesn't go too much out of date (for
                // monitors not under cursor), causing sudden jumps.
                //
                // This happens after any dnd_scroll_gesture_scroll() calls (in
                // Layout::advance_animations()), so it doesn't mess up the time delta there.
                if let Some(last_time) = &mut gesture.dnd_last_event_time {
                    let now = self.clock.now_unadjusted();
                    if *last_time != now {
                        *last_time = now;

                        // If last_time was already == now, then dnd_scroll_gesture_scroll() must've
                        // updated the gesture already. Therefore, when this code runs, the pointer
                        // must be outside the DnD scrolling zone.
                        gesture.dnd_nonzero_start_time = None;
                    }
                }

                if let Some(anim) = &mut gesture.animation {
                    if anim.is_done() {
                        gesture.animation = None;
                    }
                }
            }
            None => (),
        }

        for (_, _, ws) in self.workspaces.iter_workspaces_mut() {
            ws.advance_animations();
        }
    }

    pub(super) fn are_animations_ongoing(&self) -> bool {
        self.workspace_switch
            .as_ref()
            .is_some_and(|s| s.is_animation_ongoing())
            || self
                .workspaces
                .iter_workspaces()
                .any(|(_, _, ws)| ws.are_animations_ongoing())
    }

    pub fn are_transitions_ongoing(&self) -> bool {
        self.workspace_switch.is_some()
            || self
                .workspaces
                .iter_workspaces()
                .any(|(_, _, ws)| ws.are_transitions_ongoing())
    }

    pub fn update_render_elements(&mut self, is_active: bool) {
        let mut insert_hint_ws_geo = None;
        let insert_hint_ws_id = self
            .insert_hint
            .as_ref()
            .and_then(|hint| hint.workspace.existing_id());

        for (ws, geo) in self.workspaces_with_render_geo_mut(true) {
            ws.update_render_elements(is_active);

            if Some(ws.id()) == insert_hint_ws_id {
                insert_hint_ws_geo = Some(geo);
            }
        }

        self.insert_hint_render_loc = None;
        if let Some(hint) = &self.insert_hint {
            match hint.workspace {
                InsertWorkspace::Existing(ws_id) => {
                    if let Some((_, _, ws)) = self
                        .workspaces
                        .iter_workspaces()
                        .find(|(_, _, ws)| ws.id() == ws_id)
                    {
                        if let Some(mut area) = ws.insert_hint_area(hint.position) {
                            let scale = ws.scale().fractional_scale();
                            let view_size = ws.view_size();

                            // Make sure the hint is at least partially visible.
                            if matches!(hint.position, InsertPosition::NewColumn(_)) {
                                let zoom = self.overview_zoom();
                                let geo = insert_hint_ws_geo.unwrap();
                                let geo = geo.downscale(zoom);

                                area.loc.x = area.loc.x.max(-geo.loc.x - area.size.w / 2.);
                                area.loc.x =
                                    area.loc.x.min(geo.loc.x + geo.size.w - area.size.w / 2.);
                            }

                            // Round to physical pixels.
                            area = area.to_physical_precise_round(scale).to_logical(scale);

                            let view_rect = Rectangle::new(area.loc.upscale(-1.), view_size);
                            self.insert_hint_element.update_render_elements(
                                area.size,
                                view_rect,
                                hint.corner_radius,
                                scale,
                            );
                            self.insert_hint_render_loc = Some(InsertHintRenderLoc {
                                workspace: hint.workspace,
                                location: area.loc,
                            });
                        }
                    } else {
                        error!("insert hint workspace missing from monitor");
                    }
                }
                InsertWorkspace::NewAt(ws_idx) => {
                    let scale = self.scale.fractional_scale();
                    let zoom = self.overview_zoom();
                    let gap = self.workspace_gap(zoom);

                    let hint_gap = round_logical_in_physical(scale, gap * 0.1);
                    let hint_height = gap - hint_gap * 2.;

                    let next_ws_geo = self.workspaces_render_geo().nth(ws_idx).unwrap();
                    let hint_width = round_logical_in_physical(scale, next_ws_geo.size.w * 0.75);
                    let hint_x =
                        round_logical_in_physical(scale, (next_ws_geo.size.w - hint_width) / 2.);

                    let hint_loc_diff = Point::from((-hint_x, hint_height + hint_gap));
                    let hint_loc = next_ws_geo.loc - hint_loc_diff;
                    let hint_size = Size::from((hint_width, hint_height));

                    // Sometimes the hint ends up 1 px wider than necessary and/or 1 px
                    // narrower than necessary. The values here seem correct. Might have to do with
                    // how zooming out currently doesn't round to output scale properly.

                    // Compute view rect as if we're above the next workspace (rather than below
                    // the previous one).
                    let view_rect = Rectangle::new(hint_loc_diff, next_ws_geo.size);

                    self.insert_hint_element.update_render_elements(
                        hint_size,
                        view_rect,
                        CornerRadius::default(),
                        scale,
                    );
                    self.insert_hint_render_loc = Some(InsertHintRenderLoc {
                        workspace: hint.workspace,
                        location: hint_loc,
                    });
                }
            }
        }
    }

    pub fn update_config(&mut self, base_options: Rc<Options>) {
        let options =
            Rc::new(Options::clone(&base_options).with_merged_layout(self.layout_config.as_ref()));

        if self.options.layout.empty_workspace_above_first
            != options.layout.empty_workspace_above_first
            && self.workspaces.len() > 1
        {
            if options.layout.empty_workspace_above_first {
                self.add_workspace_top();
            } else if self.workspace_switch.is_none() && self.active_workspace_idx() != 0 {
                self.workspaces.remove(0);
                self.set_active_workspace_idx(self.active_workspace_idx().saturating_sub(1));
            }
        }

        for (_, _, ws) in self.workspaces.iter_workspaces_mut() {
            ws.update_config(options.clone());
        }

        self.insert_hint_element
            .update_config(options.layout.insert_hint);

        self.base_options = base_options;
        self.options = options;
    }

    pub fn update_layout_config(&mut self, layout_config: Option<niri_config::LayoutPart>) -> bool {
        if self.layout_config == layout_config {
            return false;
        }

        self.layout_config = layout_config;
        self.update_config(self.base_options.clone());

        true
    }

    pub fn update_shaders(&mut self) {
        for (_, _, ws) in self.workspaces.iter_workspaces_mut() {
            ws.update_shaders();
        }

        self.insert_hint_element.update_shaders();
    }

    pub fn update_output_size(&mut self) {
        self.scale = self.output.current_scale();
        self.view_size = output_size(&self.output);
        self.working_area = compute_working_area(&self.output);

        for (_, _, ws) in self.workspaces.iter_workspaces_mut() {
            ws.update_output_size();
        }
    }

    pub fn move_workspace_down(&mut self) {
        let mut new_idx = min(self.active_workspace_idx() + 1, self.workspaces.len() - 1);
        if new_idx == self.active_workspace_idx() {
            return;
        }

        let active_workspace_idx = self.active_workspace_idx();
        self.workspaces.swap(active_workspace_idx, new_idx);

        if new_idx == self.workspaces.len() - 1 {
            // Insert a new empty workspace.
            self.add_workspace_bottom();
        }

        if self.options.layout.empty_workspace_above_first && self.active_workspace_idx() == 0 {
            self.add_workspace_top();
            new_idx += 1;
        }

        let previous_workspace_id = self.previous_workspace_id;
        self.activate_workspace(new_idx);
        self.workspace_switch = None;
        self.previous_workspace_id = previous_workspace_id;

        self.clean_up_workspaces();
    }

    pub fn move_workspace_up(&mut self) {
        let mut new_idx = self.active_workspace_idx().saturating_sub(1);
        if new_idx == self.active_workspace_idx() {
            return;
        }

        let active_workspace_idx = self.active_workspace_idx();
        self.workspaces.swap(active_workspace_idx, new_idx);

        if self.active_workspace_idx() == self.workspaces.len() - 1 {
            // Insert a new empty workspace.
            self.add_workspace_bottom();
        }

        if self.options.layout.empty_workspace_above_first && new_idx == 0 {
            self.add_workspace_top();
            new_idx += 1;
        }

        let previous_workspace_id = self.previous_workspace_id;
        self.activate_workspace(new_idx);
        self.workspace_switch = None;
        self.previous_workspace_id = previous_workspace_id;

        self.clean_up_workspaces();
    }

    pub fn move_workspace_to_idx(&mut self, old_idx: usize, new_idx: usize) {
        if self.workspaces.len() <= old_idx {
            return;
        }

        let mut new_idx = new_idx.clamp(0, self.workspaces.len() - 1);
        if old_idx == new_idx {
            return;
        }

        let ws = self.workspaces.remove(old_idx);
        self.workspaces.insert(new_idx, ws);

        if new_idx > old_idx {
            if new_idx == self.workspaces.len() - 1 {
                // Insert a new empty workspace.
                self.add_workspace_bottom();
            }

            if self.options.layout.empty_workspace_above_first && old_idx == 0 {
                self.add_workspace_top();
                new_idx += 1;
            }
        } else {
            if old_idx == self.workspaces.len() - 1 {
                // Insert a new empty workspace.
                self.add_workspace_bottom();
            }

            if self.options.layout.empty_workspace_above_first && new_idx == 0 {
                self.add_workspace_top();
                new_idx += 1;
            }
        }

        // Only refocus the workspace if it was already focused
        if self.active_workspace_idx() == old_idx {
            self.set_active_workspace_idx(new_idx);
        // If the workspace order was switched so that the current workspace moved down the
        // workspace stack, focus correctly
        } else if new_idx <= self.active_workspace_idx() && old_idx > self.active_workspace_idx() {
            self.set_active_workspace_idx(self.active_workspace_idx() + 1);
        } else if new_idx >= self.active_workspace_idx() && old_idx < self.active_workspace_idx() {
            self.set_active_workspace_idx(self.active_workspace_idx().saturating_sub(1));
        }

        self.workspace_switch = None;

        self.clean_up_workspaces();
    }

    /// Returns the geometry of the active tile relative to and clamped to the output.
    ///
    /// During animations, assumes the final view position.
    pub fn active_tile_visual_rectangle(&self) -> Option<Rectangle<f64, Logical>> {
        if self.overview_open {
            return None;
        }

        self.active_workspace_ref().active_tile_visual_rectangle()
    }

    fn workspace_size(&self, zoom: f64) -> Size<f64, Logical> {
        let ws_size = self.view_size.upscale(zoom);
        let scale = self.scale.fractional_scale();
        ws_size.to_physical_precise_ceil(scale).to_logical(scale)
    }

    fn workspace_gap(&self, zoom: f64) -> f64 {
        let scale = self.scale.fractional_scale();
        let gap = self.view_size.h * 0.1 * zoom;
        round_logical_in_physical_max1(scale, gap)
    }

    fn workspace_size_with_gap(&self, zoom: f64) -> Size<f64, Logical> {
        let gap = self.workspace_gap(zoom);
        self.workspace_size(zoom) + Size::from((0., gap))
    }

    pub fn overview_zoom(&self) -> f64 {
        let progress = self.overview_progress.as_ref().map(|p| p.value());
        compute_overview_zoom(&self.options, progress)
    }

    pub(super) fn set_overview_progress(&mut self, progress: Option<&super::OverviewProgress>) {
        let prev_render_idx = self.workspace_render_idx();
        self.overview_progress = progress.map(OverviewProgress::from);
        let new_render_idx = self.workspace_render_idx();

        // If the view jumped (can happen when going from corrected to uncorrected render_idx, for
        // example when toggling the overview in the middle of an overview animation), then restart
        // the workspace switch to avoid jumps.
        if prev_render_idx != new_render_idx {
            if let Some(WorkspaceSwitch::Animation(anim)) = &mut self.workspace_switch {
                // FIXME: maintain velocity.
                *anim = anim.restarted(prev_render_idx, anim.to(), 0.);
            }
        }
    }

    #[cfg(test)]
    pub(super) fn overview_progress_value(&self) -> Option<f64> {
        self.overview_progress.as_ref().map(|p| p.value())
    }

    pub fn workspace_render_idx(&self) -> f64 {
        // If workspace switch and overview progress are matching animations, then compute a
        // correction term to make the movement appear monotonic.
        if let (
            Some(WorkspaceSwitch::Animation(switch_anim)),
            Some(OverviewProgress::Animation(progress_anim)),
        ) = (&self.workspace_switch, &self.overview_progress)
        {
            if switch_anim.start_time() == progress_anim.start_time()
                && (switch_anim.duration().as_secs_f64() - progress_anim.duration().as_secs_f64())
                    .abs()
                    <= 0.001
            {
                #[rustfmt::skip]
                // How this was derived:
                //
                // - Assume we're animating a zoom + switch. Consider switch "from" and "to".
                //   These are render_idx values, so first workspace to second would have switch
                //   from = 0. and to = 1. regardless of the zoom level.
                //
                // - At the start, the point at "from" is at Y = 0. We're moving the point at "to"
                //   to Y = 0. We want this to be a monotonic motion in apparent coordinates (after
                //   zoom).
                //
                // - Height at the start:
                //   from_height = (size.h + gap) * from_zoom.
                //
                // - Current height:
                //   current_height = (size.h + gap) * zoom.
                //
                // - We're moving the "to" point to Y = 0:
                //   to_y = 0.
                //
                // - The initial position of the point we're moving:
                //   from_y = (to - from) * from_height.
                //
                // - We want this point to travel monotonically in apparent coordinates:
                //   current_y = from_y + (to_y - from_y) * progress,
                //   where progress is from 0 to 1, equals to the animation progress (switch and
                //   zoom are the same since they are synchronized).
                //
                // - Derive the Y of the first workspace from this:
                //   first_y = current_y - to * current_height.
                //
                // Now, let's substitute and rearrange the terms.
                //
                // - current_y = from_y + (0 - (to - from) * from_height) * progress
                // - progress = (switch_anim.value() - from) / (to - from)
                // - current_y = from_y - (to - from) * from_height * (switch_anim.value() - from) / (to - from)
                // - current_y = from_y - from_height * (switch_anim.value() - from)
                // - first_y = from_y - from_height * (switch_anim.value() - from) - to * current_height
                // - first_y = (to - from) * from_height - from_height * (switch_anim.value() - from) - to * current_height
                // - first_y = to * from_height - switch_anim.value() * from_height - to * current_height
                // - first_y = -switch_anim.value() * from_height + to * (from_height - current_height)
                let from = progress_anim.from();
                let from_zoom = compute_overview_zoom(&self.options, Some(from));
                let from_ws_height_with_gap = self.workspace_size_with_gap(from_zoom).h;

                let zoom = self.overview_zoom();
                let ws_height_with_gap = self.workspace_size_with_gap(zoom).h;

                let first_ws_y = -switch_anim.value() * from_ws_height_with_gap
                    + switch_anim.to() * (from_ws_height_with_gap - ws_height_with_gap);

                return -first_ws_y / ws_height_with_gap;
            }
        };

        if let Some(switch) = &self.workspace_switch {
            switch.current_idx()
        } else {
            self.active_workspace_idx() as f64
        }
    }

    pub fn workspaces_render_geo(&self) -> impl Iterator<Item = Rectangle<f64, Logical>> {
        let scale = self.scale.fractional_scale();
        let zoom = self.overview_zoom();

        let ws_size = self.workspace_size(zoom);
        let gap = self.workspace_gap(zoom);
        let ws_height_with_gap = ws_size.h + gap;

        let static_offset = (self.view_size.to_point() - ws_size.to_point()).downscale(2.);
        let static_offset = static_offset
            .to_physical_precise_round(scale)
            .to_logical(scale);

        let first_ws_y = -self.workspace_render_idx() * ws_height_with_gap;
        let first_ws_y = round_logical_in_physical(scale, first_ws_y);

        // Return position for one-past-last workspace too.
        (0..=self.workspaces.len()).map(move |idx| {
            let y = first_ws_y + idx as f64 * ws_height_with_gap;
            let loc = Point::from((0., y)) + static_offset;

            // Even though all components that go into loc are rounded to physical pixels, the
            // floating point addition may lose precision. This can result for example in the
            // current workspace having y = 0.0000000000002 and thus missing pointer hits at the
            // monitor edge with y = 0. So, post-round the location too.
            let loc = loc.to_physical_precise_round(scale).to_logical(scale);

            Rectangle::new(loc, ws_size)
        })
    }

    pub fn workspaces_with_render_geo(
        &self,
    ) -> impl Iterator<Item = (&Workspace<W>, Rectangle<f64, Logical>)> {
        let output_geo = Rectangle::from_size(self.view_size);

        let geo = self.workspaces_render_geo();
        zip(self.workspaces.iter(), geo)
            // Cull out workspaces outside the output.
            .filter(move |(_ws, geo)| geo.intersection(output_geo).is_some())
    }

    pub fn workspaces_with_render_geo_idx(
        &self,
    ) -> impl Iterator<Item = ((usize, &Workspace<W>), Rectangle<f64, Logical>)> {
        let output_geo = Rectangle::from_size(self.view_size);

        let geo = self.workspaces_render_geo();
        zip(self.workspaces.iter().enumerate(), geo)
            // Cull out workspaces outside the output.
            .filter(move |(_ws, geo)| geo.intersection(output_geo).is_some())
    }

    pub fn workspaces_with_render_geo_mut(
        &mut self,
        cull: bool,
    ) -> impl Iterator<Item = (&mut Workspace<W>, Rectangle<f64, Logical>)> {
        let output_geo = Rectangle::from_size(self.view_size);

        let geo = self.workspaces_render_geo();
        zip(self.workspaces.iter_mut(), geo)
            // Cull out workspaces outside the output.
            .filter(move |(_ws, geo)| !cull || geo.intersection(output_geo).is_some())
    }

    pub fn workspace_under(
        &self,
        pos_within_output: Point<f64, Logical>,
    ) -> Option<(&Workspace<W>, Rectangle<f64, Logical>)> {
        let (ws, geo) = self.workspaces_with_render_geo().find_map(|(ws, geo)| {
            // Extend width to entire output.
            let loc = Point::from((0., geo.loc.y));
            let size = Size::from((self.view_size.w, geo.size.h));
            let bounds = Rectangle::new(loc, size);

            bounds.contains(pos_within_output).then_some((ws, geo))
        })?;
        Some((ws, geo))
    }

    pub fn workspace_under_narrow(
        &self,
        pos_within_output: Point<f64, Logical>,
    ) -> Option<&Workspace<W>> {
        self.workspaces_with_render_geo()
            .find_map(|(ws, geo)| geo.contains(pos_within_output).then_some(ws))
    }

    pub fn window_under(&self, pos_within_output: Point<f64, Logical>) -> Option<(&W, HitType)> {
        let (ws, geo) = self.workspace_under(pos_within_output)?;

        if self.overview_progress.is_some() {
            let zoom = self.overview_zoom();
            let pos_within_workspace = (pos_within_output - geo.loc).downscale(zoom);
            let (win, hit) = ws.window_under(pos_within_workspace)?;
            // During the overview animation, we cannot do input hits because we cannot really
            // represent scaled windows properly.
            Some((win, hit.to_activate()))
        } else {
            let (win, hit) = ws.window_under(pos_within_output - geo.loc)?;
            Some((win, hit.offset_win_pos(geo.loc)))
        }
    }

    pub fn resize_edges_under(&self, pos_within_output: Point<f64, Logical>) -> Option<ResizeEdge> {
        if self.overview_progress.is_some() {
            return None;
        }

        let (ws, geo) = self.workspace_under(pos_within_output)?;
        ws.resize_edges_under(pos_within_output - geo.loc)
    }

    pub(super) fn insert_position(
        &self,
        pos_within_output: Point<f64, Logical>,
    ) -> (InsertWorkspace, Rectangle<f64, Logical>) {
        let mut iter = self.workspaces_with_render_geo_idx();

        let dummy = Rectangle::default();

        // Monitors always have at least one workspace.
        let ((idx, ws), geo) = iter.next().unwrap();

        // Check if above first.
        if pos_within_output.y < geo.loc.y {
            return (InsertWorkspace::NewAt(idx), dummy);
        }

        let contains = move |geo: Rectangle<f64, Logical>| {
            geo.loc.y <= pos_within_output.y && pos_within_output.y < geo.loc.y + geo.size.h
        };

        // Check first.
        if contains(geo) {
            return (InsertWorkspace::Existing(ws.id()), geo);
        }

        let mut last_geo = geo;
        let mut last_idx = idx;
        for ((idx, ws), geo) in iter {
            // Check gap above.
            let gap_loc = Point::from((last_geo.loc.x, last_geo.loc.y + last_geo.size.h));
            let gap_size = Size::from((geo.size.w, geo.loc.y - gap_loc.y));
            let gap_geo = Rectangle::new(gap_loc, gap_size);
            if contains(gap_geo) {
                return (InsertWorkspace::NewAt(idx), dummy);
            }

            // Check workspace itself.
            if contains(geo) {
                return (InsertWorkspace::Existing(ws.id()), geo);
            }

            last_geo = geo;
            last_idx = idx;
        }

        // Anything below.
        (InsertWorkspace::NewAt(last_idx + 1), dummy)
    }

    pub fn render_above_top_layer(&self) -> bool {
        // Render above the top layer only if the view is stationary.
        if self.workspace_switch.is_some() || self.overview_progress.is_some() {
            return false;
        }

        let ws = &self.workspaces[self.active_workspace_idx()];
        ws.render_above_top_layer()
    }

    pub fn render_insert_hint_between_workspaces<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        push: &mut dyn FnMut(MonitorRenderElement<R>),
    ) {
        if self.options.layout.insert_hint.off {
            return;
        }
        let Some(render_loc) = self.insert_hint_render_loc else {
            return;
        };
        let InsertWorkspace::NewAt(_) = render_loc.workspace else {
            return;
        };

        self.insert_hint_element
            .render(renderer, render_loc.location, &mut |elem| {
                let elem = MonitorInnerRenderElement::UncroppedInsertHint(elem);
                let elem = RescaleRenderElement::from_element(elem, Point::default(), 1.);
                let elem =
                    RelocateRenderElement::from_element(elem, Point::default(), Relocate::Relative);
                push(elem);
            });
    }

    pub fn render_workspaces<R: NiriRenderer>(
        &self,
        mut ctx: RenderCtx<R>,
        focus_ring: bool,
        push: &mut dyn FnMut(MonitorRenderElement<R>),
    ) {
        let _span = tracy_client::span!("Monitor::render_workspaces");

        let scale = self.scale.fractional_scale();
        // Ceil the height in physical pixels.
        let height = (self.view_size.h * scale).ceil() as i32;

        // Crop the elements to prevent them overflowing, currently visible during a workspace
        // switch.
        //
        // HACK: crop to infinite bounds at least horizontally where we
        // know there's no workspace joining or monitor bounds, otherwise
        // it will cut pixel shaders and mess up the coordinate space.
        // There's also a damage tracking bug which causes glitched
        // rendering for maximized GTK windows.
        //
        // FIXME: use proper bounds after fixing the Crop element.
        let crop_bounds = if self.workspace_switch.is_some() || self.overview_progress.is_some() {
            Rectangle::new(
                Point::from((-i32::MAX / 2, 0)),
                Size::from((i32::MAX, height)),
            )
        } else {
            Rectangle::new(
                Point::from((-i32::MAX / 2, -i32::MAX / 2)),
                Size::from((i32::MAX, i32::MAX)),
            )
        };

        let zoom = self.overview_zoom();

        let insert_hint_render_loc = self
            .insert_hint_render_loc
            .filter(|_| !self.options.layout.insert_hint.off);

        let scale_relocate = move |geo: Rectangle<f64, Logical>, elem| {
            let elem = RescaleRenderElement::from_element(elem, Point::from((0, 0)), zoom);
            RelocateRenderElement::from_element(
                elem,
                // The offset we get from workspaces_with_render_geo() is already
                // rounded to physical pixels, but it's in the logical coordinate
                // space, so we need to convert it to physical.
                geo.loc.to_physical_precise_round(scale),
                Relocate::Relative,
            )
        };

        for (ws, geo) in self.workspaces_with_render_geo() {
            // Macro instead of closure because ws and insert hint have different elem types.
            macro_rules! push {
                () => {{
                    &mut |elem| {
                        let elem = CropRenderElement::from_element(elem, scale, crop_bounds);
                        if let Some(elem) = elem {
                            let elem = MonitorInnerRenderElement::from(elem);
                            push(scale_relocate(geo, elem));
                        }
                    }
                }};
            }

            let xray_pos = XrayPos::new(geo.loc, zoom);

            ws.render_floating(ctx.r(), xray_pos, focus_ring, push!());

            if let Some(loc) = insert_hint_render_loc {
                if loc.workspace == InsertWorkspace::Existing(ws.id()) {
                    self.insert_hint_element
                        .render(ctx.renderer, loc.location, push!());
                }
            }

            ws.render_scrolling(ctx.r(), xray_pos, focus_ring, push!());
        }
    }

    pub fn render_workspace_shadows<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        push: &mut dyn FnMut(MonitorRenderElement<R>),
    ) {
        let Some(progress) = self.overview_progress.as_ref().map(|p| p.clamped_value()) else {
            return;
        };
        let alpha = progress.clamp(0., 1.) as f32;

        let _span = tracy_client::span!("Monitor::render_workspace_shadows");

        let scale = self.scale.fractional_scale();
        let zoom = self.overview_zoom();

        for (ws, geo) in self.workspaces_with_render_geo() {
            ws.render_shadow(renderer, &mut |elem| {
                let elem = elem.with_alpha(alpha);
                let elem = MonitorInnerRenderElement::Shadow(elem);
                let elem = RescaleRenderElement::from_element(elem, Point::from((0, 0)), zoom);
                let elem = RelocateRenderElement::from_element(
                    elem,
                    geo.loc.to_physical_precise_round(scale),
                    Relocate::Relative,
                );
                push(elem);
            });
        }
    }

    pub fn workspace_switch_gesture_begin(&mut self, is_touchpad: bool) {
        let center_idx = self.active_workspace_idx();
        let current_idx = self.workspace_render_idx();

        let gesture = WorkspaceSwitchGesture {
            center_idx,
            start_idx: current_idx,
            current_idx,
            animation: None,
            tracker: SwipeTracker::new(),
            is_touchpad,
            is_clamped: !self.overview_open,
            dnd_last_event_time: None,
            dnd_nonzero_start_time: None,
        };
        self.workspace_switch = Some(WorkspaceSwitch::Gesture(gesture));
    }

    pub fn dnd_scroll_gesture_begin(&mut self) {
        if let Some(WorkspaceSwitch::Gesture(WorkspaceSwitchGesture {
            dnd_last_event_time: Some(_),
            ..
        })) = &self.workspace_switch
        {
            // Already active.
            return;
        }

        if !self.overview_open {
            // This gesture is only for the overview.
            return;
        }

        let center_idx = self.active_workspace_idx();
        let current_idx = self.workspace_render_idx();

        let gesture = WorkspaceSwitchGesture {
            center_idx,
            start_idx: current_idx,
            current_idx,
            animation: None,
            tracker: SwipeTracker::new(),
            is_touchpad: false,
            is_clamped: false,
            dnd_last_event_time: Some(self.clock.now_unadjusted()),
            dnd_nonzero_start_time: None,
        };
        self.workspace_switch = Some(WorkspaceSwitch::Gesture(gesture));
    }

    pub fn workspace_switch_gesture_update(
        &mut self,
        delta_y: f64,
        timestamp: Duration,
        is_touchpad: bool,
    ) -> Option<bool> {
        let Some(WorkspaceSwitch::Gesture(gesture)) = &self.workspace_switch else {
            return None;
        };

        if gesture.is_touchpad != is_touchpad || gesture.dnd_last_event_time.is_some() {
            return None;
        }

        let zoom = self.overview_zoom();
        let total_height = if gesture.is_touchpad {
            WORKSPACE_GESTURE_MOVEMENT
        } else {
            self.workspace_size_with_gap(1.).h
        };

        let Some(WorkspaceSwitch::Gesture(gesture)) = &mut self.workspace_switch else {
            return None;
        };

        // Reduce the effect of zoom on the touchpad somewhat.
        let delta_scale = if gesture.is_touchpad {
            (zoom - 1.) / 2.5 + 1.
        } else {
            zoom
        };

        let delta_y = delta_y / delta_scale;
        let mut rubber_band = WORKSPACE_GESTURE_RUBBER_BAND;
        rubber_band.limit /= zoom;

        gesture.tracker.push(delta_y, timestamp);

        let pos = gesture.tracker.pos() / total_height;

        let (min, max) = gesture.min_max(self.workspaces.len());
        let new_idx = gesture.start_idx + pos;
        let new_idx = rubber_band.clamp(min, max, new_idx);

        if gesture.current_idx == new_idx {
            return Some(false);
        }

        gesture.current_idx = new_idx;
        Some(true)
    }

    pub fn dnd_scroll_gesture_scroll(&mut self, pos: Point<f64, Logical>, speed: f64) -> bool {
        let zoom = self.overview_zoom();

        let Some(WorkspaceSwitch::Gesture(gesture)) = &mut self.workspace_switch else {
            return false;
        };

        let Some(last_time) = gesture.dnd_last_event_time else {
            // Not a DnD scroll.
            return false;
        };

        let config = &self.options.gestures.dnd_edge_workspace_switch;
        let trigger_height = config.trigger_height;

        // Restrict the scrolling horizontally to the strip of workspaces to avoid unwanted trigger
        // after using the hot corner or during horizontal scroll.
        let width = self.view_size.w * zoom;
        let x = pos.x - (self.view_size.w - width) / 2.;

        // Consider the working area so layer-shell docks and such don't prevent scrolling.
        let y = pos.y - self.working_area.loc.y;
        let height = self.working_area.size.h;

        let y = y.clamp(0., height);
        let trigger_height = trigger_height.clamp(0., height / 2.);

        let delta = if x < 0. || width <= x {
            // Outside the bounds horizontally.
            0.
        } else if y < trigger_height {
            -(trigger_height - y)
        } else if height - y < trigger_height {
            trigger_height - (height - y)
        } else {
            0.
        };

        let delta = if trigger_height < 0.01 {
            // Sanity check for trigger-height 0 or small window sizes.
            0.
        } else {
            // Normalize to [0, 1].
            delta / trigger_height
        };
        let delta = delta * speed;

        let now = self.clock.now_unadjusted();
        gesture.dnd_last_event_time = Some(now);

        if delta == 0. {
            // We're outside the scrolling zone.
            gesture.dnd_nonzero_start_time = None;
            return false;
        }

        let nonzero_start = *gesture.dnd_nonzero_start_time.get_or_insert(now);

        // Delay starting the gesture a bit to avoid unwanted movement when dragging across
        // monitors.
        let delay = Duration::from_millis(u64::from(config.delay_ms));
        if now.saturating_sub(nonzero_start) < delay {
            return true;
        }

        let time_delta = now.saturating_sub(last_time).as_secs_f64();

        let delta = delta * time_delta * config.max_speed;

        gesture.tracker.push(delta, now);

        let total_height = WORKSPACE_DND_EDGE_SCROLL_MOVEMENT;
        let pos = gesture.tracker.pos() / total_height;
        let unclamped = gesture.start_idx + pos;

        let (min, max) = gesture.min_max(self.workspaces.len());
        let clamped = unclamped.clamp(min, max);

        // Make sure that DnD scrolling too much outside the min/max does not "build up".
        gesture.start_idx += clamped - unclamped;
        gesture.current_idx = clamped;

        true
    }

    pub fn workspace_switch_gesture_end(&mut self, is_touchpad: Option<bool>) -> bool {
        let Some(WorkspaceSwitch::Gesture(gesture)) = &self.workspace_switch else {
            return false;
        };

        if is_touchpad.is_some_and(|x| gesture.is_touchpad != x) {
            return false;
        }

        let zoom = self.overview_zoom();
        let total_height = if gesture.dnd_last_event_time.is_some() {
            WORKSPACE_DND_EDGE_SCROLL_MOVEMENT
        } else if gesture.is_touchpad {
            WORKSPACE_GESTURE_MOVEMENT
        } else {
            self.workspace_size_with_gap(1.).h
        };

        let (gesture_current_idx, velocity, new_idx) = {
            let Some(WorkspaceSwitch::Gesture(gesture)) = &mut self.workspace_switch else {
                return false;
            };

            // Take into account any idle time between the last event and now.
            let now = self.clock.now_unadjusted();
            gesture.tracker.push(0., now);

            let mut rubber_band = WORKSPACE_GESTURE_RUBBER_BAND;
            rubber_band.limit /= zoom;

            let mut velocity = gesture.tracker.velocity() / total_height;
            let current_pos = gesture.tracker.pos() / total_height;
            let pos = gesture.tracker.projected_end_pos() / total_height;

            let (min, max) = gesture.min_max(self.workspaces.len());
            let new_idx = gesture.start_idx + pos;

            let new_idx = new_idx.clamp(min, max);
            let new_idx = new_idx.round() as usize;

            velocity *= rubber_band.clamp_derivative(min, max, gesture.start_idx + current_pos);

            (gesture.current_idx, velocity, new_idx)
        };

        let active_workspace_idx = self.active_workspace_idx();
        if active_workspace_idx != new_idx {
            self.previous_workspace_id = Some(self.workspaces[active_workspace_idx].id());
        }

        self.workspaces
            .activate_workspace(new_idx)
            .expect("gesture target workspace index must be valid");
        self.workspace_switch = Some(WorkspaceSwitch::Animation(Animation::new(
            self.clock.clone(),
            gesture_current_idx,
            new_idx as f64,
            velocity,
            self.options.animations.workspace_switch.0,
        )));

        true
    }

    pub fn dnd_scroll_gesture_end(&mut self) {
        if !matches!(
            self.workspace_switch,
            Some(WorkspaceSwitch::Gesture(WorkspaceSwitchGesture {
                dnd_last_event_time: Some(_),
                ..
            }))
        ) {
            // Not a DnD scroll.
            return;
        };

        self.workspace_switch_gesture_end(None);
    }

    pub fn scale(&self) -> smithay::output::Scale {
        self.scale
    }

    pub fn view_size(&self) -> Size<f64, Logical> {
        self.view_size
    }

    pub fn working_area(&self) -> Rectangle<f64, Logical> {
        self.working_area
    }

    pub fn layout_config(&self) -> Option<&niri_config::LayoutPart> {
        self.layout_config.as_ref()
    }

    #[cfg(test)]
    pub(super) fn verify_invariants(&self) {
        use approx::assert_abs_diff_eq;

        let options =
            Options::clone(&self.base_options).with_merged_layout(self.layout_config.as_ref());
        assert_eq!(&*self.options, &options);

        assert!(
            !self.workspaces.is_empty(),
            "monitor must have at least one workspace"
        );
        let shared_active_workspace_idx = self.active_workspace_idx();
        assert!(shared_active_workspace_idx < self.workspaces.len());
        for plane in self.workspaces.iter_planes() {
            assert!(
                shared_active_workspace_idx < plane.workspace_count(),
                "every plane must contain the shared active workspace"
            );
            assert_eq!(
                plane.active_workspace_idx(),
                shared_active_workspace_idx,
                "all planes on a monitor must share the active workspace index"
            );
        }

        if let Some(WorkspaceSwitch::Animation(anim)) = &self.workspace_switch {
            let before_idx = anim.from() as usize;
            let after_idx = anim.to() as usize;

            assert!(before_idx < self.workspaces.len());
            assert!(after_idx < self.workspaces.len());
        }

        assert!(
            !self.workspaces.last().unwrap().has_windows(),
            "monitor must have an empty workspace in the end"
        );
        if self.options.layout.empty_workspace_above_first {
            assert!(
                !self.workspaces.first().unwrap().has_windows(),
                "first workspace must be empty when empty_workspace_above_first is set"
            )
        }

        assert!(
            self.workspaces.last().unwrap().name.is_none(),
            "monitor must have an unnamed workspace in the end"
        );
        if self.options.layout.empty_workspace_above_first {
            assert!(
                self.workspaces.first().unwrap().name.is_none(),
                "first workspace must be unnamed when empty_workspace_above_first is set"
            )
        }

        if self.options.layout.empty_workspace_above_first {
            assert!(
                self.workspaces.len() != 2,
                "if empty_workspace_above_first is set there must be just 1 or 3+ workspaces"
            )
        }

        // If there's no workspace switch in progress, workspaces below the active row
        // cannot be empty and unnamed except for the last boundary workspace. Empty rows
        // before the active row are allowed as Y-alignment padding in a new Plane.
        let pre_skip = if self.options.layout.empty_workspace_above_first {
            1
        } else {
            0
        };
        if self.workspace_switch.is_none() {
            for (idx, ws) in self
                .workspaces
                .iter()
                .enumerate()
                .skip(pre_skip)
                .rev()
                // skip last
                .skip(1)
            {
                // Empty workspaces before the shared Y coordinate are
                // intentional alignment padding in a newly-created Plane.
                if idx > shared_active_workspace_idx {
                    assert!(
                        ws.has_windows_or_name(),
                        "workspace below the active row can't be empty and unnamed except the last one"
                    );
                }
            }
        }

        for (_, _, workspace) in self.workspaces.iter_workspaces() {
            assert_eq!(self.clock, workspace.clock);

            assert_eq!(
                self.scale().integer_scale(),
                workspace.scale().integer_scale()
            );
            assert_eq!(
                self.scale().fractional_scale(),
                workspace.scale().fractional_scale()
            );
            assert_eq!(self.view_size, workspace.view_size());
            assert_eq!(self.working_area, workspace.working_area());

            assert_eq!(
                workspace.base_options, self.options,
                "workspace options must be synchronized with monitor"
            );
        }

        let scale = self.scale().fractional_scale();
        let iter = self.workspaces_with_render_geo();
        for (_ws, ws_geo) in iter {
            let pos = ws_geo.loc;
            let rounded_pos = pos.to_physical_precise_round(scale).to_logical(scale);

            // Workspace positions must be rounded to physical pixels.
            assert_abs_diff_eq!(pos.x, rounded_pos.x, epsilon = 1e-5);
            assert_abs_diff_eq!(pos.y, rounded_pos.y, epsilon = 1e-5);
        }
    }
}
