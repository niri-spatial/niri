use std::cell::RefCell;
use std::collections::HashMap;

use pangocairo::cairo::{self, ImageSurface};
use pangocairo::pango::FontDescription;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::output::{Output, WeakOutput};
use smithay::utils::{Logical, Point, Scale, Size, Transform};

use crate::layout::workspace::Workspace;
use crate::layout::LayoutElement;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::utils::{output_size, to_physical_precise_round};

const WIDTH: i32 = 240;
const HEIGHT: i32 = 96;
const MARGIN: i32 = 20;
const PADDING: i32 = 14;
const MAP_MARGIN: i32 = 10;
const MAP_WIDTH: i32 = WIDTH - PADDING * 2;
const MAP_HEIGHT: i32 = 112;
const PANEL_HEIGHT: i32 = HEIGHT + MAP_MARGIN + MAP_HEIGHT + PADDING;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpatialPosition {
    /// One-based active column index. None when no tiling column is focused.
    pub x: Option<usize>,
    /// One-based active workspace index.
    pub y: usize,
    /// One-based active plane index, increasing from back to front.
    pub z: usize,
    /// Total number of depth planes on this monitor.
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

    pub fn render<R: NiriRenderer, W: LayoutElement>(
        &self,
        renderer: &mut R,
        output: &Output,
        position: SpatialPosition,
        workspace: &Workspace<W>,
    ) -> Option<Vec<PrimaryGpuTextureRenderElement>> {
        let scale_factor = output.current_scale().fractional_scale();
        let scale = Scale::from(scale_factor);

        let weak = output.downgrade();
        let panel_buffer = {
            let mut buffers = self.buffers.borrow_mut();
            buffers.retain(|output, _| output.is_alive());

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

            buffers.get(&weak)?.buffer.as_ref()?.clone()
        };

        let panel_size = panel_buffer.logical_size();
        let output_size = output_size(output).to_f64();
        let panel_location: Point<f64, Logical> = Point::from((
            (output_size.w - panel_size.w - MARGIN as f64).max(0.),
            MARGIN as f64,
        ));
        let panel_location = panel_location
            .to_physical_precise_round(scale)
            .to_logical(scale);

        let panel_element = TextureRenderElement::from_texture_buffer(
            panel_buffer,
            panel_location,
            0.94,
            None,
            None,
            Kind::Unspecified,
        );
        let panel_element = PrimaryGpuTextureRenderElement(panel_element);

        let (content_size, viewport) = workspace.minimap_geometry();
        if content_size.w <= 0. || content_size.h <= 0. {
            return Some(vec![panel_element]);
        }

        let map_zoom = (MAP_WIDTH as f64 / content_size.w)
            .min(MAP_HEIGHT as f64 / content_size.h);
        if !map_zoom.is_finite() || map_zoom <= 0. {
            return Some(vec![panel_element]);
        }

        let map_origin = Point::from((
            (MAP_WIDTH as f64 - content_size.w * map_zoom) / 2.,
            (MAP_HEIGHT as f64 - content_size.h * map_zoom) / 2.,
        ));
        let map_width_px: i32 = to_physical_precise_round(scale_factor, MAP_WIDTH);
        let map_height_px: i32 = to_physical_precise_round(scale_factor, MAP_HEIGHT);
        let map_size = Size::from((map_width_px, map_height_px));

        let map_buffer = match workspace.render_minimap_texture(
            renderer.as_gles_renderer(),
            scale,
            map_size,
            map_zoom,
            map_origin,
        ) {
            Ok(buffer) => buffer,
            Err(err) => {
                warn!("failed to render workspace minimap: {err:#}");
                return Some(vec![panel_element]);
            }
        };

        let map_location = panel_location
            + Point::from((PADDING as f64, (HEIGHT + MAP_MARGIN) as f64));
        let map_element = TextureRenderElement::from_texture_buffer(
            map_buffer,
            map_location,
            1.0,
            None,
            None,
            Kind::Unspecified,
        );
        let map_element = PrimaryGpuTextureRenderElement(map_element);

        let mut elements = Vec::with_capacity(3);
        match render_viewport_overlay(
            renderer.as_gles_renderer(),
            scale_factor,
            viewport,
            content_size,
            map_zoom,
            map_origin,
        ) {
            Ok(buffer) => {
                let overlay = TextureRenderElement::from_texture_buffer(
                    buffer,
                    map_location,
                    1.0,
                    None,
                    None,
                    Kind::Unspecified,
                );
                // Output elements are stored top-to-bottom, so the viewport
                // outline must precede the thumbnail and its background.
                elements.push(PrimaryGpuTextureRenderElement(overlay));
            }
            Err(err) => warn!("failed to render workspace minimap viewport: {err:#}"),
        }
        elements.push(map_element);
        elements.push(panel_element);

        Some(elements)
    }
}

fn render_indicator(
    renderer: &mut GlesRenderer,
    position: &SpatialPosition,
    scale_factor: f64,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
    let width: i32 = to_physical_precise_round(scale_factor, WIDTH);
    let height: i32 = to_physical_precise_round(scale_factor, PANEL_HEIGHT);
    let padding: i32 = to_physical_precise_round(scale_factor, PADDING);

    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;

    // A compact, translucent panel containing coordinates and a live workspace preview.
    cr.set_source_rgba(0.035, 0.045, 0.065, 0.92);
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
    let coordinate_cell_width = content_width as f64 / 3.;
    let label_y: i32 = to_physical_precise_round(scale_factor, 12);
    let value_y: i32 = to_physical_precise_round(scale_factor, 28);
    let separator_top: i32 = to_physical_precise_round(scale_factor, 9);
    let separator_bottom: i32 = to_physical_precise_round(scale_factor, 54);

    for separator in [1., 2.] {
        let x = padding as f64 + coordinate_cell_width * separator;
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
        let cell_left = padding as f64 + coordinate_cell_width * index as f64;
        let cell_center = cell_left + coordinate_cell_width / 2.;

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

    // The depth track uses the same back-to-front ordering as the Z coordinate.
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

    // Backplate behind the live workspace thumbnail.
    let map_left = padding as f64;
    let map_top = to_physical_precise_round(scale_factor, HEIGHT + MAP_MARGIN) as f64;
    let map_width = (width - padding * 2) as f64;
    let map_height = to_physical_precise_round(scale_factor, MAP_HEIGHT) as f64;
    cr.set_source_rgba(0.012, 0.018, 0.03, 0.98);
    cr.rectangle(map_left, map_top, map_width, map_height);
    cr.fill()?;
    cr.set_source_rgba(0.55, 0.66, 0.78, 0.35);
    cr.set_line_width(scale_factor.max(1.));
    cr.rectangle(
        map_left + 0.5,
        map_top + 0.5,
        map_width - 1.,
        map_height - 1.,
    );
    cr.stroke()?;

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

fn render_viewport_overlay(
    renderer: &mut GlesRenderer,
    scale_factor: f64,
    viewport: smithay::utils::Rectangle<f64, Logical>,
    content_size: Size<f64, Logical>,
    map_zoom: f64,
    map_origin: Point<f64, Logical>,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
    let width: i32 = to_physical_precise_round(scale_factor, MAP_WIDTH);
    let height: i32 = to_physical_precise_round(scale_factor, MAP_HEIGHT);
    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;
    cr.set_operator(cairo::Operator::Source);
    cr.set_source_rgba(0., 0., 0., 0.);
    cr.paint()?;
    cr.set_operator(cairo::Operator::Over);

    let viewport_left = viewport.loc.x.clamp(0., content_size.w);
    let viewport_right = (viewport.loc.x + viewport.size.w).clamp(viewport_left, content_size.w);
    let visible_width = viewport_right - viewport_left;
    let visible_height = viewport.size.h.min(content_size.h);
    if visible_width > 0. && visible_height > 0. {
        let x = (map_origin.x + viewport_left * map_zoom) * scale_factor;
        let y = map_origin.y * scale_factor;
        let w = visible_width * map_zoom * scale_factor;
        let h = visible_height * map_zoom * scale_factor;

        cr.set_source_rgba(0.25, 0.64, 0.98, 0.10);
        cr.rectangle(x, y, w, h);
        cr.fill()?;

        cr.set_source_rgba(0.62, 0.88, 1.0, 0.98);
        let line_width_px: i32 = to_physical_precise_round(scale_factor, 1.5);
        cr.set_line_width(line_width_px as f64);
        cr.rectangle(x + 0.5, y + 0.5, (w - 1.).max(0.), (h - 1.).max(0.));
        cr.stroke()?;
    }

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
