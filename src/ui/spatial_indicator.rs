use std::cell::RefCell;
use std::collections::HashMap;

use pangocairo::cairo::{self, ImageSurface};
use pangocairo::pango::FontDescription;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::output::{Output, WeakOutput};
use smithay::reexports::gbm::Format as Fourcc;
use smithay::utils::{Point, Scale, Transform};

use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::utils::{output_size, to_physical_precise_round};

const WIDTH: i32 = 184;
const HEIGHT: i32 = 140;
const MARGIN: i32 = 20;
const PADDING: i32 = 12;
const MAX_VISIBLE_ITEMS: usize = 5;
const GRID_SPACING: i32 = 12;
const GRID_TOP: i32 = 70;
const GRID_RADIUS: f64 = 2.2;
const ACTIVE_RADIUS: f64 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpatialPosition {
    pub plane_idx: usize,
    pub plane_count: usize,
    pub workspace_idx: usize,
    pub workspace_count: usize,
}

struct RenderedIndicator {
    position: SpatialPosition,
    scale: Scale<f64>,
    buffer: Option<TextureBuffer<GlesTexture>>,
}

pub struct SpatialIndicator {
    buffers: RefCell<HashMap<WeakOutput, RenderedIndicator>>,
}

impl SpatialIndicator {
    pub fn new() -> Self {
        Self {
            buffers: RefCell::new(HashMap::new()),
        }
    }

    pub fn render<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        output: &Output,
        position: SpatialPosition,
    ) -> Option<PrimaryGpuTextureRenderElement> {
        let scale = output.current_scale().fractional_scale();
        let scale = Scale::from(scale);

        let mut buffers = self.buffers.borrow_mut();
        buffers.retain(|output, _| output.is_alive());

        let weak = output.downgrade();
        let needs_render = buffers.get(&weak).is_none_or(|rendered| {
            rendered.position != position || rendered.scale != scale
        });

        if needs_render {
            let rendered = render_indicator(renderer.as_gles_renderer(), position, scale)
                .unwrap_or_else(|err| {
                    warn!("failed to render spatial indicator: {err:#}");
                    None
                });
            buffers.insert(
                weak.clone(),
                RenderedIndicator {
                    position,
                    scale,
                    buffer: rendered,
                },
            );
        }

        let buffer = buffers.get(&weak)?.buffer.as_ref()?;

        let size = buffer.logical_size();
        let output_size = output_size(output).to_f64();
        let location = Point::from((
            output_size.w - size.w - MARGIN as f64,
            MARGIN as f64,
        ));
        let location = location.to_physical_precise_round(scale).to_logical(scale);

        let elem = TextureRenderElement::from_texture_buffer(
            buffer.clone(),
            location,
            0.94,
            None,
            None,
            Kind::Unspecified,
        );

        Some(PrimaryGpuTextureRenderElement(elem))
    }
}

fn render_indicator(
    renderer: &mut GlesRenderer,
    position: SpatialPosition,
    scale: Scale<f64>,
) -> anyhow::Result<Option<TextureBuffer<GlesTexture>>> {
    let scale_factor = scale.fractional_scale();
    let width = to_physical_precise_round(scale_factor, WIDTH);
    let height = to_physical_precise_round(scale_factor, HEIGHT);
    let padding = to_physical_precise_round(scale_factor, PADDING);
    let grid_spacing = to_physical_precise_round(scale_factor, GRID_SPACING);
    let grid_top = to_physical_precise_round(scale_factor, GRID_TOP);
    let grid_radius = GRID_RADIUS * scale_factor;
    let active_radius = ACTIVE_RADIUS * scale_factor;

    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;

    // Translucent panel and a quiet outline.
    cr.set_source_rgba(0.035, 0.045, 0.065, 0.88);
    cr.rectangle(0., 0., width.into(), height.into());
    cr.fill()?;

    cr.set_source_rgba(0.55, 0.66, 0.78, 0.32);
    cr.set_line_width((scale_factor).max(1.));
    cr.rectangle(0.5, 0.5, (width - 1) as f64, (height - 1) as f64);
    cr.stroke()?;

    let mut title_font = FontDescription::from_string("sans Bold 11px");
    title_font.set_absolute_size(to_physical_precise_round(
        scale_factor,
        title_font.size(),
    ));
    let mut body_font = FontDescription::from_string("monospace 10px");
    body_font.set_absolute_size(to_physical_precise_round(
        scale_factor,
        body_font.size(),
    ));

    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);

    cr.set_source_rgba(0.88, 0.93, 1.0, 0.96);
    layout.set_font_description(Some(&title_font));
    layout.set_text("SPATIAL POSITION");
    cr.move_to(padding.into(), (padding - to_physical_precise_round(scale_factor, 1)).into());
    pangocairo::functions::show_layout(&cr, &layout);

    layout.set_font_description(Some(&body_font));
    cr.set_source_rgba(0.72, 0.79, 0.88, 0.92);
    layout.set_text(&format!(
        "Z  PLANE      {:02} / {:02}",
        position.plane_idx + 1,
        position.plane_count
    ));
    cr.move_to(
        padding.into(),
        to_physical_precise_round(scale_factor, 34).into(),
    );
    pangocairo::functions::show_layout(&cr, &layout);

    layout.set_text(&format!(
        "Y  WORKSPACE  {:02} / {:02}",
        position.workspace_idx + 1,
        position.workspace_count
    ));
    cr.move_to(
        padding.into(),
        to_physical_precise_round(scale_factor, 51).into(),
    );
    pangocairo::functions::show_layout(&cr, &layout);

    // The map is a top-down projection: depth planes run horizontally (Z),
    // and vertical workspaces run from top to bottom (Y).
    let plane_range = visible_range(position.plane_idx, position.plane_count);
    let workspace_range = visible_range(position.workspace_idx, position.workspace_count);
    let plane_visible_count = plane_range.end - plane_range.start;
    let workspace_visible_count = workspace_range.end - workspace_range.start;

    let map_width = plane_visible_count.saturating_sub(1) as i32 * grid_spacing;
    let map_height = workspace_visible_count.saturating_sub(1) as i32 * grid_spacing;
    let map_left = (width - map_width) / 2;
    let map_top = grid_top + (to_physical_precise_round(scale_factor, 40) - map_height) / 2;

    for plane_idx in plane_range.clone() {
        for workspace_idx in workspace_range.clone() {
            let x = map_left + (plane_idx - plane_range.start) as i32 * grid_spacing;
            let y = map_top + (workspace_idx - workspace_range.start) as i32 * grid_spacing;
            let is_active =
                plane_idx == position.plane_idx && workspace_idx == position.workspace_idx;

            if is_active {
                cr.set_source_rgba(0.34, 0.83, 1.0, 0.98);
                cr.arc(
                    x.into(),
                    y.into(),
                    active_radius,
                    0.,
                    std::f64::consts::TAU,
                );
                cr.fill()?;

                cr.set_source_rgba(0.34, 0.83, 1.0, 0.52);
                cr.set_line_width((scale_factor).max(1.));
                cr.arc(
                    x.into(),
                    y.into(),
                    active_radius + to_physical_precise_round(scale_factor, 3) as f64,
                    0.,
                    std::f64::consts::TAU,
                );
                cr.stroke()?;
            } else {
                cr.set_source_rgba(0.64, 0.72, 0.83, 0.58);
                cr.arc(
                    x.into(),
                    y.into(),
                    grid_radius,
                    0.,
                    std::f64::consts::TAU,
                );
                cr.fill()?;
            }
        }
    }

    // Axis hints make the map readable without taking much space.
    let mut hint_font = FontDescription::from_string("sans 8px");
    hint_font.set_absolute_size(to_physical_precise_round(
        scale_factor,
        hint_font.size(),
    ));
    layout.set_font_description(Some(&hint_font));
    cr.set_source_rgba(0.62, 0.7, 0.8, 0.78);
    layout.set_text("BACK");
    cr.move_to(
        padding.into(),
        to_physical_precise_round(scale_factor, 126).into(),
    );
    pangocairo::functions::show_layout(&cr, &layout);

    layout.set_text("FRONT  ·  Y ↓");
    let (hint_width, _) = layout.pixel_size();
    cr.move_to(
        (width - padding - hint_width).into(),
        to_physical_precise_round(scale_factor, 113).into(),
    );
    pangocairo::functions::show_layout(&cr, &layout);

    drop(layout);
    drop(cr);

    let data = surface.take_data()?;
    let buffer = TextureBuffer::from_memory(
        renderer,
        &data,
        Fourcc::Argb8888,
        (width, height),
        false,
        scale_factor,
        Transform::Normal,
        Vec::new(),
    )?;

    Ok(Some(buffer))
}

fn visible_range(current: usize, total: usize) -> std::ops::Range<usize> {
    if total <= MAX_VISIBLE_ITEMS {
        return 0..total;
    }

    let start = current
        .saturating_sub(MAX_VISIBLE_ITEMS / 2)
        .min(total - MAX_VISIBLE_ITEMS);
    start..start + MAX_VISIBLE_ITEMS
}
