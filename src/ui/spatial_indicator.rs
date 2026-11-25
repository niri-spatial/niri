use std::cell::RefCell;
use std::collections::HashMap;

use pangocairo::cairo::{self, ImageSurface};
use pangocairo::pango::FontDescription;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::output::{Output, WeakOutput};
use smithay::reexports::gbm::Format as Fourcc;
use smithay::utils::{Logical, Point, Scale, Transform};

use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::utils::{output_size, to_physical_precise_round};

const WIDTH: i32 = 240;
const HEIGHT: i32 = 96;
const MARGIN: i32 = 20;
const PADDING: i32 = 14;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpatialPosition {
    /// One-based active column index. None when no tiling column is focused.
    pub x: Option<usize>,
    /// One-based active workspace index.
    pub y: usize,
    /// One-based active plane index, increasing from back to front.
    pub z: usize,
    /// Workspace occupancy, ordered by plane (Z) and workspace (Y).
    pub workspaces: Vec<Vec<bool>>,
}

struct RenderedIndicator {
    position: SpatialPosition,
    scale: Scale<f64>,
    buffer: Option<TextureBuffer<GlesTexture>>,
}

pub struct SpatialIndicator {
    buffers: RefCell<HashMap<WeakOutput, RenderedIndicator>>,
}

impl Default for SpatialIndicator {
    fn default() -> Self {
        Self {
            buffers: RefCell::new(HashMap::new()),
        }
    }
}

impl SpatialIndicator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn render<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        output: &Output,
        position: SpatialPosition,
    ) -> Option<PrimaryGpuTextureRenderElement> {
        let scale_factor = output.current_scale().fractional_scale();
        let scale = Scale::from(scale_factor);

        let mut buffers = self.buffers.borrow_mut();
        buffers.retain(|output, _| output.is_alive());

        let weak = output.downgrade();
        let needs_render = match buffers.get(&weak) {
            Some(rendered) => rendered.position != position || rendered.scale != scale,
            None => true,
        };

        if needs_render {
            let rendered =
                match render_indicator(renderer.as_gles_renderer(), &position, scale_factor) {
                    Ok(buffer) => Some(buffer),
                    Err(err) => {
                        warn!("failed to render spatial coordinates: {err:#}");
                        None
                    }
                };
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
        let location: Point<f64, Logical> = Point::from((
            (output_size.w - size.w - MARGIN as f64).max(0.),
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
    position: &SpatialPosition,
    scale_factor: f64,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
    let width: i32 = to_physical_precise_round(scale_factor, WIDTH);
    let base_height: i32 = to_physical_precise_round(scale_factor, HEIGHT);
    let padding: i32 = to_physical_precise_round(scale_factor, PADDING);
    let map_margin: i32 = to_physical_precise_round(scale_factor, 10);
    let map_max_height: i32 = to_physical_precise_round(scale_factor, 144);
    let map_gap: i32 = to_physical_precise_round(scale_factor, 3);
    let max_cell_width: i32 = to_physical_precise_round(scale_factor, 48);
    let max_cell_height: i32 = to_physical_precise_round(scale_factor, 14);

    let plane_count = position.workspaces.len().max(1);
    let workspace_row_count = position
        .workspaces
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(1)
        .max(1);
    let plane_count_i32 = plane_count.min(i32::MAX as usize) as i32;
    let row_count_i32 = workspace_row_count.min(i32::MAX as usize) as i32;

    let content_width = width - padding * 2;
    let horizontal_gaps = map_gap * plane_count_i32.saturating_sub(1);
    let available_cell_width = (content_width - horizontal_gaps).max(plane_count_i32);
    let cell_width = (available_cell_width / plane_count_i32).min(max_cell_width).max(1);
    let grid_width = cell_width * plane_count_i32 + horizontal_gaps;
    let grid_start_x = (width - grid_width) / 2;

    let vertical_gaps = map_gap * row_count_i32.saturating_sub(1);
    let available_cell_height =
        (map_max_height - padding * 2 - vertical_gaps).max(row_count_i32);
    let cell_height = (available_cell_height / row_count_i32)
        .min(max_cell_height)
        .max(1);
    let map_height = cell_height * row_count_i32 + vertical_gaps + padding * 2;
    let map_top = base_height + map_margin;
    let height = map_top + map_height;

    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;

    // A compact, translucent coordinates panel.
    cr.set_source_rgba(0.035, 0.045, 0.065, 0.9);
    cr.rectangle(0., 0., width.into(), height.into());
    cr.fill()?;

    cr.set_source_rgba(0.55, 0.66, 0.78, 0.34);
    cr.set_line_width(scale_factor.max(1.));
    cr.rectangle(0.5, 0.5, (width - 1) as f64, (height - 1) as f64);
    cr.stroke()?;

    let mut axis_font = FontDescription::from_string("sans Bold 9px");
    axis_font.set_absolute_size(to_physical_precise_round(scale_factor, axis_font.size()));

    let mut value_font = FontDescription::from_string("monospace Bold 22px");
    value_font.set_absolute_size(to_physical_precise_round(scale_factor, value_font.size()));

    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);

    let content_width = width - padding * 2;
    let cell_width = content_width as f64 / 3.;
    let label_y: i32 = to_physical_precise_round(scale_factor, 12);
    let value_y: i32 = to_physical_precise_round(scale_factor, 28);
    let separator_top: i32 = to_physical_precise_round(scale_factor, 9);
    let separator_bottom: i32 = to_physical_precise_round(scale_factor, 54);

    for separator in [1., 2.] {
        let x = padding as f64 + cell_width * separator;
        cr.set_source_rgba(0.55, 0.66, 0.78, 0.24);
        cr.set_line_width(scale_factor.max(1.));
        cr.move_to(x, separator_top as f64);
        cr.line_to(x, separator_bottom as f64);
        cr.stroke()?;
    }

    let coordinates = [
        ("X", position.x.map_or_else(|| "—".to_owned(), |x| x.to_string())),
        ("Y", position.y.to_string()),
        ("Z", position.z.to_string()),
    ];

    for (index, (axis, value)) in coordinates.iter().enumerate() {
        let cell_left = padding as f64 + cell_width * index as f64;
        let cell_center = cell_left + cell_width / 2.;

        layout.set_font_description(Some(&axis_font));
        layout.set_text(axis);
        let (label_width, _) = layout.pixel_size();
        cr.set_source_rgba(0.45, 0.76, 1.0, 0.92);
        cr.move_to(cell_center - label_width as f64 / 2., label_y.into());
        pangocairo::functions::show_layout(&cr, &layout);

        layout.set_font_description(Some(&value_font));
        layout.set_text(value);
        let (value_width, _) = layout.pixel_size();
        cr.set_source_rgba(0.92, 0.96, 1.0, 0.98);
        cr.move_to(cell_center - value_width as f64 / 2., value_y.into());
        pangocairo::functions::show_layout(&cr, &layout);
    }

    // Separate the coordinates from the depth-position track.
    let divider_y: i32 = to_physical_precise_round(scale_factor, 61);
    cr.set_source_rgba(0.55, 0.66, 0.78, 0.24);
    cr.set_line_width(scale_factor.max(1.));
    cr.move_to(padding as f64, divider_y as f64);
    cr.line_to((width - padding) as f64, divider_y as f64);
    cr.stroke()?;

    let track_inset: i32 = to_physical_precise_round(scale_factor, 2);
    let track_start = padding as f64 + track_inset as f64;
    let track_end = width as f64 - padding as f64 - track_inset as f64;
    let track_y: i32 = to_physical_precise_round(scale_factor, 80);
    let track_y = track_y as f64;

    cr.set_source_rgba(0.55, 0.66, 0.78, 0.34);
    cr.set_line_width(scale_factor.max(1.) * 1.5);
    cr.move_to(track_start, track_y);
    cr.line_to(track_end, track_y);
    cr.stroke()?;

    let plane_count = position.plane_count.max(1);
    let active_plane = position.z.saturating_sub(1).min(plane_count - 1);
    let marker_count = plane_count.min(15);
    let active_marker = if marker_count <= 1 || plane_count <= marker_count {
        active_plane
    } else {
        ((active_plane as f64 / (plane_count - 1) as f64) * (marker_count - 1) as f64).round()
            as usize
    };

    for marker in 0..marker_count {
        let fraction = if marker_count <= 1 {
            0.5
        } else {
            marker as f64 / (marker_count - 1) as f64
        };
        let x = track_start + (track_end - track_start) * fraction;
        let is_active = marker == active_marker;
        let radius_px: i32 =
            to_physical_precise_round(scale_factor, if is_active { 4.5 } else { 2.5 });
        let radius = radius_px as f64;

        if is_active {
            cr.set_source_rgba(0.35, 0.72, 1.0, 0.28);
            cr.arc(x, track_y, radius * 1.65, 0., std::f64::consts::PI * 2.);
            cr.fill()?;
            cr.set_source_rgba(0.52, 0.82, 1.0, 1.0);
        } else {
            cr.set_source_rgba(0.62, 0.70, 0.81, 0.48);
        }

        cr.arc(x, track_y, radius, 0., std::f64::consts::PI * 2.);
        cr.fill()?;
    }

    drop(layout);
    drop(cr);

    let data = surface.take_data()?;
    Ok(TextureBuffer::from_memory(
        renderer,
        &data,
        Fourcc::Argb8888,
        (width, height),
        false,
        scale_factor,
        Transform::Normal,
        Vec::new(),
    )?)
}
