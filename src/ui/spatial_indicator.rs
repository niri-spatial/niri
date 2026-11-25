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
const HEIGHT: i32 = 104;
const MARGIN: i32 = 20;
const PADDING: i32 = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpatialPosition {
    /// One-based active column index. None when no tiling column is focused.
    pub x: Option<usize>,
    /// One-based active workspace index.
    pub y: usize,
    /// One-based active plane index, increasing from back to front.
    pub z: usize,
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
    position: SpatialPosition,
    scale: Scale<f64>,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
    let scale_factor = scale.fractional_scale();
    let width = to_physical_precise_round(scale_factor, WIDTH);
    let height = to_physical_precise_round(scale_factor, HEIGHT);
    let padding = to_physical_precise_round(scale_factor, PADDING);

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

    let mut title_font = FontDescription::from_string("sans Bold 10px");
    title_font.set_absolute_size(to_physical_precise_round(scale_factor, title_font.size()));

    let mut axis_font = FontDescription::from_string("sans Bold 9px");
    axis_font.set_absolute_size(to_physical_precise_round(scale_factor, axis_font.size()));

    let mut value_font = FontDescription::from_string("monospace Bold 22px");
    value_font.set_absolute_size(to_physical_precise_round(scale_factor, value_font.size()));

    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);

    cr.set_source_rgba(0.86, 0.92, 1.0, 0.94);
    layout.set_font_description(Some(&title_font));
    layout.set_text("SPATIAL COORDINATES");
    cr.move_to(padding.into(), padding.into());
    pangocairo::functions::show_layout(&cr, &layout);

    let content_width = width - padding * 2;
    let cell_width = content_width as f64 / 3.;
    let label_y = to_physical_precise_round(scale_factor, 43);
    let value_y = to_physical_precise_round(scale_factor, 51);
    let separator_top = to_physical_precise_round(scale_factor, 34);
    let separator_bottom = height - padding;

    for separator in [1., 2.] {
        let x = padding as f64 + cell_width * separator;
        cr.set_source_rgba(0.55, 0.66, 0.78, 0.24);
        cr.set_line_width(scale_factor.max(1.));
        cr.move_to(x, separator_top as f64);
        cr.line_to(x, separator_bottom as f64);
        cr.stroke()?;
    }

    let coordinates = [
        (
            "X",
            position
                .x
                .map_or_else(|| "—".to_owned(), |x| format!("{x:02}")),
        ),
        ("Y", format!("{:02}", position.y)),
        ("Z", format!("{:02}", position.z)),
    ];

    for (index, (axis, value)) in coordinates.iter().enumerate() {
        let cell_left = padding as f64 + cell_width * index as f64;
        let cell_center = cell_left + cell_width / 2.;

        layout.set_font_description(Some(&axis_font));
        layout.set_text(axis);
        let (label_width, _) = layout.pixel_size();
        cr.set_source_rgba(0.45, 0.76, 1.0, 0.92);
        cr.move_to(
            (cell_center - label_width as f64 / 2.).into(),
            label_y.into(),
        );
        pangocairo::functions::show_layout(&cr, &layout);

        layout.set_font_description(Some(&value_font));
        layout.set_text(value);
        let (value_width, _) = layout.pixel_size();
        cr.set_source_rgba(0.92, 0.96, 1.0, 0.98);
        cr.move_to(
            (cell_center - value_width as f64 / 2.).into(),
            value_y.into(),
        );
        pangocairo::functions::show_layout(&cr, &layout);
    }

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
