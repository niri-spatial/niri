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

const WIDTH: i32 = 216;
const HEIGHT: i32 = 88;
const MARGIN: i32 = 20;
const PADDING: i32 = 12;
const MAX_VISIBLE_PLANES: usize = 7;
const TRACK_MARGIN: i32 = 22;
const TRACK_Y: i32 = 54;
const TRACK_RADIUS: f64 = 3.0;
const ACTIVE_RADIUS: f64 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpatialPosition {
    /// Zero-based plane index. Plane indices increase toward the front.
    pub plane_idx: usize,
    pub plane_count: usize,
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
        let scale = output.current_scale().fractional_scale();
        let scale = Scale::from(scale);

        let mut buffers = self.buffers.borrow_mut();
        buffers.retain(|output, _| output.is_alive());

        let weak = output.downgrade();
        let needs_render = match buffers.get(&weak) {
            Some(rendered) => rendered.position != position || rendered.scale != scale,
            None => true,
        };

        if needs_render {
            let rendered = match render_indicator(renderer.as_gles_renderer(), position, scale) {
                Ok(buffer) => Some(buffer),
                Err(err) => {
                    warn!("failed to render spatial indicator: {err:#}");
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
    position: SpatialPosition,
    scale: Scale<f64>,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
    let scale_factor = scale.fractional_scale();
    let width = to_physical_precise_round(scale_factor, WIDTH);
    let height = to_physical_precise_round(scale_factor, HEIGHT);
    let padding = to_physical_precise_round(scale_factor, PADDING);
    let track_margin = to_physical_precise_round(scale_factor, TRACK_MARGIN);
    let track_y = to_physical_precise_round(scale_factor, TRACK_Y);
    let track_radius = TRACK_RADIUS * scale_factor;
    let active_radius = ACTIVE_RADIUS * scale_factor;

    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;

    // Compact translucent panel shown only while the overview is open.
    cr.set_source_rgba(0.035, 0.045, 0.065, 0.88);
    cr.rectangle(0., 0., width.into(), height.into());
    cr.fill()?;

    cr.set_source_rgba(0.55, 0.66, 0.78, 0.32);
    cr.set_line_width(scale_factor.max(1.));
    cr.rectangle(0.5, 0.5, (width - 1) as f64, (height - 1) as f64);
    cr.stroke()?;

    let mut title_font = FontDescription::from_string("sans Bold 11px");
    title_font.set_absolute_size(to_physical_precise_round(scale_factor, title_font.size()));
    let mut value_font = FontDescription::from_string("monospace 10px");
    value_font.set_absolute_size(to_physical_precise_round(scale_factor, value_font.size()));

    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);

    cr.set_source_rgba(0.88, 0.93, 1.0, 0.96);
    layout.set_font_description(Some(&title_font));
    layout.set_text("DEPTH POSITION");
    cr.move_to(padding.into(), padding.into());
    pangocairo::functions::show_layout(&cr, &layout);

    layout.set_font_description(Some(&value_font));
    cr.set_source_rgba(0.72, 0.79, 0.88, 0.94);
    layout.set_text(&format!(
        "PLANE  {:02} / {:02}",
        position.plane_idx + 1,
        position.plane_count
    ));
    cr.move_to(
        (width - padding - layout.pixel_size().0).into(),
        padding.into(),
    );
    pangocairo::functions.show_layout(&cr, &layout);

    // A single front-to-back track. Higher plane indices are closer to the front.
    let visible = visible_range(position.plane_idx, position.plane_count);
    let visible_count = visible.end - visible.start;
    let track_left = track_margin as f64;
    let track_right = (width - track_margin) as f64;
    let track_width = track_right - track_left;
    let spacing = if visible_count > 1 {
        track_width / (visible_count - 1) as f64
    } else {
        0.
    };

    cr.set_source_rgba(0.55, 0.66, 0.78, 0.35);
    cr.set_line_width((scale_factor * 1.2).max(1.));
    cr.move_to(track_left, track_y as f64);
    cr.line_to(track_right, track_y as f64);
    cr.stroke()?;

    for plane_idx in visible.clone() {
        let x = track_left + (plane_idx - visible.start) as f64 * spacing;
        let distance_from_front = position
            .plane_count
            .saturating_sub(1)
            .saturating_sub(plane_idx);
        let depth = MAX_VISIBLE_PLANES.saturating_sub(distance_from_front.min(MAX_VISIBLE_PLANES));
        let alpha = (0.42 + 0.08 * depth as f64).min(0.9);

        if plane_idx == position.plane_idx {
            cr.set_source_rgba(0.34, 0.83, 1.0, 0.98);
            cr.arc(x, track_y as f64, active_radius, 0., std::f64::consts::TAU);
            cr.fill()?;

            cr.set_source_rgba(0.34, 0.83, 1.0, 0.55);
            cr.set_line_width(scale_factor.max(1.));
            cr.arc(
                x,
                track_y as f64,
                active_radius + to_physical_precise_round(scale_factor, 3) as f64,
                0.,
                std::f64::consts::TAU,
            );
            cr.stroke()?;
        } else {
            cr.set_source_rgba(0.64, 0.72, 0.83, alpha);
            cr.arc(x, track_y as f64, track_radius, 0., std::f64::consts::TAU);
            cr.fill()?;
        }
    }

    let mut hint_font = FontDescription::from_string("sans 8px");
    hint_font.set_absolute_size(to_physical_precise_round(scale_factor, hint_font.size()));
    layout.set_font_description(Some(&hint_font));
    cr.set_source_rgba(0.62, 0.7, 0.8, 0.78);

    layout.set_text("BACK");
    cr.move_to(
        padding.into(),
        to_physical_precise_round(scale_factor, 75).into(),
    );
    pangocairo::functions::show_layout(&cr, &layout);

    layout.set_text("FRONT");
    let (hint_width, _) = layout.pixel_size();
    cr.move_to(
        (width - padding - hint_width).into(),
        to_physical_precise_round(scale_factor, 75).into(),
    );
    pangocairo::functions::show_layout(&cr, &layout);

    drop(layout);
    drop(cr);

    let data = surface.take_data()?;
    TextureBuffer::from_memory(
        renderer,
        &data,
        Fourcc::Argb8888,
        (width, height),
        false,
        scale_factor,
        Transform::Normal,
        Vec::new(),
    )
}

fn visible_range(current: usize, total: usize) -> std::ops::Range<usize> {
    if total <= MAX_VISIBLE_PLANES {
        return 0..total;
    }

    let start = current
        .saturating_sub(MAX_VISIBLE_PLANES / 2)
        .min(total - MAX_VISIBLE_PLANES);
    start..start + MAX_VISIBLE_PLANES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_range_keeps_small_depths_whole() {
        assert_eq!(visible_range(0, 1), 0..1);
        assert_eq!(visible_range(2, 4), 0..4);
    }

    #[test]
    fn visible_range_centers_the_active_plane_when_possible() {
        assert_eq!(visible_range(5, 10), 2..9);
    }

    #[test]
    fn visible_range_stays_inside_depth_bounds() {
        assert_eq!(visible_range(0, 10), 0..7);
        assert_eq!(visible_range(9, 10), 3..10);
    }
}
