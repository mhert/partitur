//! The GPU side of [`frame_view`](super::frame_view): one texture per view, drawn pixel for
//! pixel.
//!
//! [`FramePipeline`] keeps a texture for each view drawn in the last frame, the size of its
//! picture, and a small buffer with where the picture's top-left pixel lands on the screen.
//! When a [`FramePrimitive`] is prepared, its view's texture is made anew only when there is
//! none yet or the picture's size changed ([`Slots::prepare`]). What is written into it
//! ([`uploads`]) is nothing when it holds the picture already, only the picture's damage when
//! it holds the picture just before, and the whole picture otherwise. The pixels are copied
//! from the view's slot, in BGRA with premultiplied alpha as the engine painted them, into a
//! texture of that format, a region at a time and under the slot's lock for no longer than the
//! copy. The slot may hold newer pixels than the frame announced by then; their damage comes
//! with the next frame, which is already on its way. The renderer submits the writes before it
//! draws the frame, so the frame shows the new picture. After each frame the textures of views
//! that were not drawn in it go ([`Slots::trim`]).
//!
//! The shader takes no sampler: each screen pixel the renderer lets it draw, inside the bounds
//! of the widget and its clip, reads the picture's pixel at its own place less the picture's
//! origin, and draws nothing where the picture has no pixel. The origin is the top-left corner
//! of the bounds in physical pixels, each coordinate rounded to the nearest pixel, as the
//! renderer rounds the bounds for its scissor rectangle ([`origin`]). The texture holds the
//! page's colours as they are. On an sRGB screen the shader takes each pixel's straight colour
//! out of its alpha, turns it into linear light and multiplies the alpha in again, so the
//! picture blends over what lies below exactly as the renderer's own images, whose sRGB
//! textures do the same in hardware; on a screen that is not sRGB it passes the pixels on as
//! they are. Either way the pixels leave the shader premultiplied, and they are blended so.

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::Entry;

use iced::wgpu;
use iced::widget::shader::{self, Viewport};
use iced::{Rectangle, mouse};
use partitur_core::widget_host::{Damage, Region, ViewId};

use super::Frame;

/// The shader that draws a picture pixel for pixel.
const SHADER: &str = r"
struct Place {
    // The picture's origin, and whether the screen is sRGB (1) or not (0).
    origin: vec4<i32>,
}

@group(0) @binding(0) var picture: texture_2d<f32>;
@group(0) @binding(1) var<uniform> place: Place;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // One triangle over the whole viewport, which the renderer sets to the widget's bounds.
    let x = f32(i32(index & 1u) * 4 - 1);
    let y = f32(i32(index >> 1u) * 4 - 1);
    return vec4<f32>(x, y, 0.0, 1.0);
}

fn to_linear(colour: vec3<f32>) -> vec3<f32> {
    let low = colour / 12.92;
    let high = pow((colour + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, colour <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(at.xy)) - place.origin.xy;
    let size = vec2<i32>(textureDimensions(picture));
    if any(pixel < vec2<i32>(0)) || any(pixel >= size) {
        discard;
    }
    // Premultiplied, in the page's sRGB colours.
    let texel = textureLoad(picture, pixel, 0);
    if place.origin.z == 0 || texel.a <= 0.0 {
        return texel;
    }
    let straight = min(texel.rgb / texel.a, vec3<f32>(1.0));
    return vec4<f32>(to_linear(straight) * texel.a, texel.a);
}
";

/// The size of the buffer that holds a picture's origin: four 32-bit integers.
const PLACE_SIZE: u64 = 16;

/// What a view's picture asks of its texture when it is prepared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Upload {
    /// The texture holds the picture already.
    Keep,
    /// The texture holds the picture just before, and the picture's damage goes into it.
    Damage,
    /// The texture fits the picture but holds an older one, and the whole picture goes into it.
    Write,
    /// A texture of the picture's size is made, and the whole picture goes into it.
    Create,
}

/// The regions of a picture `size` large that `upload` writes into its texture, its `damage`
/// being the regions painted since the picture before.
pub(super) fn uploads(upload: Upload, (width, height): (u32, u32), damage: &Damage) -> Vec<Region> {
    match upload {
        Upload::Keep => Vec::new(),
        Upload::Damage => damage
            .regions()
            .iter()
            .filter_map(|region| region.within(width, height))
            .collect(),
        Upload::Write | Upload::Create => vec![Region::whole(width, height)],
    }
}

/// Where `region` starts in the pixels of a picture `width` pixels wide, in bytes, and how many
/// bytes a row of that picture has.
pub(super) fn copy_layout(width: u32, region: Region) -> (u64, u32) {
    let offset = (u64::from(region.y) * u64::from(width) + u64::from(region.x)) * 4;
    (offset, width.saturating_mul(4))
}

/// The textures `T` of the views drawn in the last frame, with what each holds.
#[derive(Debug)]
pub(super) struct Slots<T> {
    /// Each view's texture, by view.
    slots: HashMap<ViewId, Slot<T>>,
}

impl<T> Default for Slots<T> {
    fn default() -> Self {
        Self {
            slots: HashMap::new(),
        }
    }
}

/// One view's texture `T` and what it holds.
#[derive(Debug)]
struct Slot<T> {
    /// The width and height of the texture.
    size: (u32, u32),
    /// The number of the picture the texture holds.
    generation: u64,
    /// Whether the view was drawn since the last trim.
    drawn: bool,
    /// The texture.
    texture: T,
}

impl<T> Slots<T> {
    /// Readies the texture of `view` for its picture numbered `generation`, `size` large, and
    /// marks the view drawn: makes it with `create` when there is none or it has another size,
    /// and returns it with what the picture asks of it.
    pub(super) fn prepare(
        &mut self,
        view: ViewId,
        size: (u32, u32),
        generation: u64,
        create: impl FnOnce() -> T,
    ) -> (Upload, &mut T) {
        let upload = match self.slots.get(&view) {
            Some(slot) if slot.size != size => Upload::Create,
            Some(slot) if slot.generation == generation => Upload::Keep,
            Some(slot) if slot.generation.checked_add(1) == Some(generation) => Upload::Damage,
            Some(_) => Upload::Write,
            None => Upload::Create,
        };
        let fresh = |texture| Slot {
            size,
            generation,
            drawn: false,
            texture,
        };
        let slot = match self.slots.entry(view) {
            Entry::Occupied(mut occupied) => {
                if upload == Upload::Create {
                    occupied.insert(fresh(create()));
                }
                occupied.into_mut()
            }
            Entry::Vacant(vacant) => vacant.insert(fresh(create())),
        };
        slot.generation = generation;
        slot.drawn = true;
        (upload, &mut slot.texture)
    }

    /// The texture of `view`, if it holds the picture numbered `generation`.
    pub(super) fn holding(&self, view: ViewId, generation: u64) -> Option<&T> {
        self.slots
            .get(&view)
            .filter(|slot| slot.generation == generation)
            .map(|slot| &slot.texture)
    }

    /// Drops the textures of the views not drawn since the last trim, and starts counting
    /// anew.
    pub(super) fn trim(&mut self) {
        self.slots.retain(|_, slot| slot.drawn);
        for slot in self.slots.values_mut() {
            slot.drawn = false;
        }
    }

    /// How many textures are kept.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.slots.len()
    }
}

/// Where the top-left pixel of a picture drawn in `bounds` lands in a window of the scale
/// factor `scale`, in physical pixels: each coordinate of the corner, scaled, rounded to the
/// nearest pixel.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a window's coordinates are far within i32 once rounded"
)]
pub(super) fn origin(bounds: &Rectangle, scale: f32) -> (i32, i32) {
    (
        (bounds.x * scale).round() as i32,
        (bounds.y * scale).round() as i32,
    )
}

/// The bytes of the buffer that tells the shader the picture's `origin` and whether the screen
/// is `srgb`.
fn place((x, y): (i32, i32), srgb: bool) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (to, from) in bytes
        .chunks_exact_mut(4)
        .zip([x, y, i32::from(srgb), 0].map(i32::to_le_bytes))
    {
        to.copy_from_slice(&from);
    }
    bytes
}

/// Whether a picture `size` large fits a texture whose sides may be at most `max` long.
pub(super) fn fits((width, height): (u32, u32), max: u32) -> bool {
    width > 0 && height > 0 && width <= max && height <= max
}

/// The shader program that draws a [`Frame`] as a [`FramePrimitive`].
pub(super) struct FrameProgram {
    /// The picture.
    frame: Frame,
}

impl FrameProgram {
    /// The program that draws `frame`.
    pub(super) fn new(frame: &Frame) -> Self {
        Self {
            frame: frame.clone(),
        }
    }
}

impl<Message> shader::Program<Message> for FrameProgram {
    type State = ();
    type Primitive = FramePrimitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> FramePrimitive {
        FramePrimitive {
            frame: self.frame.clone(),
        }
    }
}

/// One picture drawn by [`FramePipeline`].
#[derive(Debug)]
pub(super) struct FramePrimitive {
    /// The picture.
    frame: Frame,
}

/// A view's texture on the GPU, with what the shader reads it through.
#[derive(Debug)]
pub(super) struct Texture {
    /// The picture.
    picture: wgpu::Texture,
    /// The picture's origin, for the shader.
    place: wgpu::Buffer,
    /// The texture and the origin, bound for the shader.
    bind_group: wgpu::BindGroup,
}

/// The render pipeline of [`FramePrimitive`]s and the textures of the views.
#[derive(Debug)]
pub(super) struct FramePipeline {
    /// The pipeline.
    pipeline: wgpu::RenderPipeline,
    /// The layout of a view's bind group.
    layout: wgpu::BindGroupLayout,
    /// Whether the screen is sRGB.
    srgb: bool,
    /// The views' textures.
    textures: Slots<Texture>,
}

/// A texture for a picture `size` large in BGRA, with its origin buffer and its bind group of
/// `layout`.
fn texture(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    (width, height): (u32, u32),
) -> Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("partitur frame view texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let place = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("partitur frame view origin"),
        size: PLACE_SIZE,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("partitur frame view bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: place.as_entire_binding(),
            },
        ],
    });
    Texture {
        picture: texture,
        place,
        bind_group,
    }
}

impl shader::Pipeline for FramePipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("partitur frame view layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(PLACE_SIZE),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("partitur frame view pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("partitur frame view shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHADER)),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("partitur frame view pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        Self {
            pipeline,
            layout,
            srgb: format.is_srgb(),
            textures: Slots::default(),
        }
    }

    fn trim(&mut self) {
        self.textures.trim();
    }
}

impl shader::Primitive for FramePrimitive {
    type Pipeline = FramePipeline;

    fn prepare(
        &self,
        pipeline: &mut FramePipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        let frame = &self.frame;
        // The slot's size now, which a resize since the frame was taken may have changed.
        let size = frame.latest().size();
        if !fits(size, device.limits().max_texture_dimension_2d) {
            return;
        }
        let layout = &pipeline.layout;
        let (upload, texture) =
            pipeline
                .textures
                .prepare(frame.view, size, frame.generation, || {
                    texture(device, layout, size)
                });
        let regions = uploads(upload, size, frame.damage());
        if !regions.is_empty() {
            frame.latest().read(|picture| {
                // Resized again in between: the frame that brings the new size writes it whole.
                if (picture.width, picture.height) != size {
                    return;
                }
                for region in regions {
                    let (offset, bytes_per_row) = copy_layout(picture.width, region);
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &texture.picture,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: region.x,
                                y: region.y,
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        &picture.bgra,
                        wgpu::TexelCopyBufferLayout {
                            offset,
                            bytes_per_row: Some(bytes_per_row),
                            rows_per_image: None,
                        },
                        wgpu::Extent3d {
                            width: region.width,
                            height: region.height,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            });
        }
        let at = origin(bounds, viewport.scale_factor());
        queue.write_buffer(&texture.place, 0, &place(at, pipeline.srgb));
    }

    fn draw(&self, pipeline: &FramePipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        if let Some(texture) = pipeline
            .textures
            .holding(self.frame.view, self.frame.generation)
        {
            render_pass.set_pipeline(&pipeline.pipeline);
            render_pass.set_bind_group(0, &texture.bind_group, &[]);
            render_pass.draw(0..3, 0..1);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn a_view_gets_a_texture_once_and_a_new_one_only_for_another_size() {
        let mut slots = Slots::default();
        let mut made = 0;
        let mut prepare = |slots: &mut Slots<u32>, size, generation| {
            let (upload, texture) = slots.prepare(ViewId(1), size, generation, || {
                made += 1;
                made
            });
            (upload, *texture)
        };

        assert_eq!(prepare(&mut slots, (4, 3), 1), (Upload::Create, 1));
        // The same picture drawn again is not written again.
        assert_eq!(prepare(&mut slots, (4, 3), 1), (Upload::Keep, 1));
        // The next picture of the same size writes its damage into the same texture.
        assert_eq!(prepare(&mut slots, (4, 3), 2), (Upload::Damage, 1));
        // One that missed a picture between is written whole.
        assert_eq!(prepare(&mut slots, (4, 3), 4), (Upload::Write, 1));
        // A picture of another size gets a texture of its own.
        assert_eq!(prepare(&mut slots, (5, 3), 5), (Upload::Create, 2));
        assert_eq!(prepare(&mut slots, (5, 4), 5), (Upload::Create, 3));
        assert_eq!(slots.len(), 1);
        assert_eq!(slots.holding(ViewId(1), 5), Some(&3));
        assert_eq!(slots.holding(ViewId(1), 4), None);
        assert_eq!(slots.holding(ViewId(2), 5), None);
    }

    #[test]
    fn views_keep_their_own_textures() {
        let mut slots = Slots::default();
        let (upload, _) = slots.prepare(ViewId(1), (2, 2), 1, || 'a');
        assert_eq!(upload, Upload::Create);
        let (upload, _) = slots.prepare(ViewId(2), (2, 2), 1, || 'b');
        assert_eq!(upload, Upload::Create);
        assert_eq!(slots.holding(ViewId(1), 1), Some(&'a'));
        assert_eq!(slots.holding(ViewId(2), 1), Some(&'b'));
    }

    #[test]
    fn a_trim_drops_the_textures_of_views_not_drawn_since_the_last() {
        let mut slots = Slots::default();
        let _ = slots.prepare(ViewId(1), (2, 2), 1, || 1);
        let _ = slots.prepare(ViewId(2), (2, 2), 1, || 2);
        slots.trim();
        assert_eq!(slots.len(), 2, "both were drawn in the frame");

        let _ = slots.prepare(ViewId(2), (2, 2), 1, || 3);
        slots.trim();
        assert_eq!(slots.holding(ViewId(1), 1), None);
        assert_eq!(slots.holding(ViewId(2), 1), Some(&2));

        slots.trim();
        assert_eq!(slots.len(), 0, "a frame without the view drops it");
        // Drawn again, it is made and written anew.
        let (upload, _) = slots.prepare(ViewId(2), (2, 2), 1, || 4);
        assert_eq!(upload, Upload::Create);
    }

    #[test]
    fn the_origin_is_the_corner_rounded_to_the_nearest_physical_pixel() {
        let bounds = |x, y| Rectangle {
            x,
            y,
            width: 10.0,
            height: 10.0,
        };
        assert_eq!(origin(&bounds(10.0, 20.0), 1.0), (10, 20));
        assert_eq!(origin(&bounds(10.0, 20.0), 1.5), (15, 30));
        // 13.5 and 20.1 logical pixels are 20.25 and 30.15 physical ones.
        assert_eq!(origin(&bounds(13.5, 20.1), 1.5), (20, 30));
        // 13.7 logical pixels are 20.55 physical ones.
        assert_eq!(origin(&bounds(13.7, 0.3), 1.5), (21, 0));
        // A frame scrolled above the window starts above it.
        assert_eq!(origin(&bounds(0.0, -10.5), 1.5), (0, -16));
        // As iced's scissor rectangle rounds its corner.
        let snapped = (bounds(13.7, 20.1) * iced::Transformation::scale(1.5))
            .snap()
            .unwrap();
        assert_eq!(
            origin(&bounds(13.7, 20.1), 1.5),
            (
                i32::try_from(snapped.x).unwrap(),
                i32::try_from(snapped.y).unwrap()
            )
        );
    }

    #[test]
    fn the_origin_goes_to_the_shader_as_four_little_endian_integers() {
        let bytes = place((3, -2), false);
        assert_eq!(bytes[..4], 3_i32.to_le_bytes());
        assert_eq!(bytes[4..8], (-2_i32).to_le_bytes());
        assert_eq!(bytes[8..], [0; 8]);
        assert_eq!(u64::try_from(bytes.len()).unwrap(), PLACE_SIZE);
        let bytes = place((3, -2), true);
        assert_eq!(bytes[8..12], 1_i32.to_le_bytes(), "an sRGB screen");
        assert_eq!(bytes[12..], [0; 4]);
    }

    fn region(x: u32, y: u32, width: u32, height: u32) -> Region {
        Region {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn a_picture_writes_nothing_its_damage_or_all_of_it() {
        let mut damage = Damage::default();
        damage.add(region(1, 0, 2, 1));
        damage.add(region(3, 2, 9, 9));
        damage.add(region(0, 1, 1, 1));
        assert_eq!(uploads(Upload::Keep, (4, 3), &damage), []);
        assert_eq!(
            uploads(Upload::Damage, (4, 3), &damage),
            [region(1, 0, 2, 1), region(3, 2, 1, 1), region(0, 1, 1, 1)],
            "cut off at the picture's edges"
        );
        assert_eq!(
            uploads(Upload::Damage, (2, 2), &damage),
            [region(1, 0, 1, 1), region(0, 1, 1, 1)],
            "nothing outside it"
        );
        assert_eq!(
            uploads(Upload::Write, (4, 3), &damage),
            [region(0, 0, 4, 3)]
        );
        assert_eq!(
            uploads(Upload::Create, (4, 3), &Damage::default()),
            [region(0, 0, 4, 3)]
        );
    }

    #[test]
    fn a_region_is_copied_from_its_first_pixel_a_whole_row_at_a_time() {
        assert_eq!(copy_layout(10, region(0, 0, 10, 4)), (0, 40));
        assert_eq!(copy_layout(10, region(3, 0, 2, 2)), (12, 40));
        assert_eq!(copy_layout(10, region(3, 2, 2, 2)), (92, 40));
        assert_eq!(
            copy_layout(8192, region(8191, 8191, 1, 1)),
            ((8191 * 8192 + 8191) * 4, 32768)
        );
    }

    #[test]
    fn a_picture_fits_a_texture_when_neither_side_is_empty_or_too_long() {
        assert!(fits((2460, 1256), 8192));
        assert!(fits((8192, 1), 8192));
        assert!(!fits((8193, 1), 8192));
        assert!(!fits((1, 8193), 8192));
        assert!(!fits((0, 10), 8192));
        assert!(!fits((10, 0), 8192));
    }
}
