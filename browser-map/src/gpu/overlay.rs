//! Host overlays: Map Intel site names, then teammates (faces on plates or dots, and names),
//! drawn after the map and minimap with the main projection (`overlay.wgsl`).
//!
//! Each layer owns one atlas of sprites: text with its shadow, rasterized by Canvas2D in the
//! overlay font at the surface's device pixel ratio, and teammate faces. Atlases are baked
//! only when their content, the ratio or the font changes. Instances carry world anchors and
//! device-pixel offsets, so the camera only moves the viewport uniform; per-instance zoom
//! thresholds are checked in the shader against the exact scale. Instances outside the view
//! are left to the rasterizer: these layers hold tens to a few hundred quads, and choosing
//! visible ones on the CPU would cost an upload on every camera change.

use std::sync::Arc;

use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, HtmlImageElement};

use crate::overlay::{Prepared, SiteLabels, Teammates, atlas_extent};

/// The canvas overlays' font, which is kept: 10 CSS pixels of the monospace UI face.
const FONT_CSS: &str = "10px 'JetBrains Mono', monospace";
const FONT_PX: f64 = 10.0;
const FONT_FAMILY: &str = "'JetBrains Mono', monospace";
const SHADOW_COLOR: &str = "rgba(0,0,0,0.85)";
/// Canvas `shadowBlur` ignores the transform, so the canvas overlays blurred by four device
/// pixels at any ratio; the sprites do the same.
const SHADOW_BLUR: f64 = 4.0;
/// Room around the text for its shadow, device pixels.
const SPRITE_MARGIN: f64 = 8.0;
/// From a site to its name's baseline-left origin, CSS pixels.
const SITE_LABEL_OFFSET: (f64, f64) = (9.0, -7.0);
/// Site names show above this scale.
const SITE_LABEL_MIN_SCALE: f32 = 0.58;
/// Teammate names show from this scale.
const TEAMMATE_NAME_MIN_SCALE: f32 = 0.35;
/// From a teammate marker's lower edge to the name's baseline, CSS pixels.
const TEAMMATE_NAME_GAP: f64 = 11.0;
const TEAMMATE_RGB: [u8; 3] = [0xf5, 0xc5, 0x42];
/// Teammate dot radius, CSS pixels (also `DOT_RADIUS` in overlay.wgsl, which adds the
/// outline and its antialiasing within `DOT_HALF_EXTENT`).
const DOT_RADIUS: f64 = 3.0;
const DOT_HALF_EXTENT: f64 = DOT_RADIUS + 1.5;
/// The plate behind a face: skins have light pixels, and so do the map tiles.
const PLATE_RGBA: [u8; 4] = [12, 14, 23, 217];
/// Radius of the ring co-located teammates are fanned onto, as a fraction of the head size.
const FAN_RADIUS_FACTOR: f64 = 0.65;
/// Face images are drawn into the atlas at this size, as requested from the skin renderer.
const FACE_PX: u32 = 64;
const ATLAS_WIDTH: u32 = 2048;

/// `kind` in overlay.wgsl.
#[derive(Clone, Copy)]
#[repr(u32)]
enum OverlayKind {
    Sprite = 0,
    Fill = 1,
    Dot = 2,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct OverlayInstance {
    world: [f32; 2],
    offset: [f32; 2],
    size: [f32; 2],
    uv_rect: [f32; 4],
    rgba: [u8; 4],
    min_scale: f32,
    kind: u32,
}

const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
    1 => Float32x2, 2 => Float32x2, 3 => Float32x2, 4 => Float32x4, 5 => Unorm8x4,
    6 => Float32, 7 => Uint32
];

/// What one sprite is drawn from.
enum SpriteSource<'a> {
    Text { text: &'a str, rgb: [u8; 3] },
    Face(&'a HtmlImageElement),
}

/// A sprite's place in its atlas, in device pixels.
#[derive(Clone, Copy, Default)]
struct Sprite {
    uv_rect: [f32; 4],
    size: [f32; 2],
    /// Text: the baseline-left origin inside the sprite. Faces: zero.
    origin: [f32; 2],
    /// Text: advance width.
    advance: f32,
}

/// One drawn layer: its atlas and instances.
struct Layer {
    bind_group: Option<wgpu::BindGroup>,
    /// Kept alive for the bind group.
    _atlas: Option<wgpu::Texture>,
    sprites: Vec<Sprite>,
    buffer: wgpu::Buffer,
    capacity: u32,
    count: u32,
}

impl Layer {
    fn new(device: &wgpu::Device) -> Self {
        Self {
            bind_group: None,
            _atlas: None,
            sprites: Vec::new(),
            buffer: allocate(device, 64),
            capacity: 64,
            count: 0,
        }
    }

    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[OverlayInstance],
    ) -> u64 {
        self.count = instances.len() as u32;
        if instances.is_empty() {
            return 0;
        }
        if self.count > self.capacity {
            self.capacity = self.count.next_power_of_two();
            self.buffer = allocate(device, self.capacity);
        }
        let bytes: &[u8] = bytemuck::cast_slice(instances);
        queue.write_buffer(&self.buffer, 0, bytes);
        bytes.len() as u64
    }

    fn clear(&mut self) {
        self.bind_group = None;
        self._atlas = None;
        self.sprites.clear();
        self.count = 0;
    }
}

/// What a teammates atlas was baked from; positions and the head size do not change it.
#[derive(Clone, PartialEq)]
struct TeammateAtlasKey {
    faces: Vec<Option<HtmlImageElement>>,
    names: Vec<String>,
}

pub(super) struct GpuOverlayRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    sites: Layer,
    teammates: Layer,
    /// What the layers were prepared from.
    sites_from: Prepared<SiteLabels>,
    teammates_from: Prepared<Teammates>,
    teammate_atlas: Option<TeammateAtlasKey>,
    /// The ratio and font the atlases were baked with.
    dpr: f32,
    font_ready: bool,
}

impl GpuOverlayRenderer {
    pub(super) fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        viewport_bind_group_layout: &wgpu::BindGroupLayout,
        vertex_layout: &wgpu::VertexBufferLayout<'static>,
    ) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay-bgl"),
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
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        // Sprites are placed texel for pixel and faces are pixel art: no filtering, as the
        // canvas overlays drew faces with image smoothing off.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("overlay-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("overlay-params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("overlay-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("overlay.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("overlay-pl"),
            bind_group_layouts: &[viewport_bind_group_layout, &bind_group_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("overlay-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[
                    vertex_layout.clone(),
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<OverlayInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &INSTANCE_ATTRIBUTES,
                    },
                ],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        Self {
            pipeline,
            bind_group_layout,
            sampler,
            params,
            sites: Layer::new(device),
            teammates: Layer::new(device),
            sites_from: Prepared::new(),
            teammates_from: Prepared::new(),
            teammate_atlas: None,
            dpr: 0.0,
            font_ready: true,
        }
    }

    /// Brings both layers up to date with the host's overlays and the surface ratio; returns
    /// the bytes uploaded. Does nothing when nothing changed.
    pub(super) fn sync(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sites: Option<&Arc<SiteLabels>>,
        teammates: Option<&Arc<Teammates>>,
        dpr: f32,
    ) -> u64 {
        let mut bytes = 0;
        if self.dpr != dpr {
            self.dpr = dpr;
            queue.write_buffer(&self.params, 0, bytemuck::cast_slice(&[dpr, 0.0, 0.0, 0.0]));
            bytes += 16;
            // Sprites are baked for one ratio.
            self.invalidate_atlases();
        }
        if let Some(sites) = self.sites_from.update(sites) {
            bytes += self.sync_sites(device, queue, sites);
        }
        if let Some(teammates) = self.teammates_from.update(teammates) {
            bytes += self.sync_teammates(device, queue, teammates);
        }
        bytes
    }

    fn invalidate_atlases(&mut self) {
        self.sites_from.invalidate();
        self.teammates_from.invalidate();
        self.teammate_atlas = None;
    }

    fn sync_sites(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sites: Option<Arc<SiteLabels>>,
    ) -> u64 {
        let Some(sites) = sites else {
            self.sites.clear();
            return 0;
        };
        let sources: Vec<SpriteSource> = sites
            .labels
            .iter()
            .map(|label| SpriteSource::Text {
                text: &label.text,
                rgb: label.rgb,
            })
            .collect();
        let mut bytes = self.bake(device, queue, &sources, Which::Sites);
        let dpr = f64::from(self.dpr);
        let offset = (
            (SITE_LABEL_OFFSET.0 * dpr).round() as f32,
            (SITE_LABEL_OFFSET.1 * dpr).round() as f32,
        );
        let instances: Vec<OverlayInstance> = sites
            .labels
            .iter()
            .zip(&self.sites.sprites)
            .map(|(label, sprite)| OverlayInstance {
                world: label.world,
                offset: [offset.0 - sprite.origin[0], offset.1 - sprite.origin[1]],
                size: sprite.size,
                uv_rect: sprite.uv_rect,
                rgba: [255; 4],
                min_scale: SITE_LABEL_MIN_SCALE.next_up(),
                kind: OverlayKind::Sprite as u32,
            })
            .collect();
        bytes += self.sites.upload(device, queue, &instances);
        bytes
    }

    fn sync_teammates(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        team: Option<Arc<Teammates>>,
    ) -> u64 {
        let Some(team) = team else {
            self.teammates.clear();
            self.teammate_atlas = None;
            return 0;
        };
        let face_of = |index: usize| {
            team.players[index]
                .face
                .as_ref()
                .filter(|_| team.show_faces)
        };
        let key = TeammateAtlasKey {
            faces: (0..team.players.len())
                .map(|i| face_of(i).cloned())
                .collect(),
            names: if team.show_names {
                team.players.iter().map(|p| p.username.clone()).collect()
            } else {
                Vec::new()
            },
        };
        let mut bytes = 0;
        if self.teammate_atlas.as_ref() != Some(&key) {
            // Faces first, then names, one sprite per present entry.
            let mut sources: Vec<SpriteSource> =
                key.faces.iter().flatten().map(SpriteSource::Face).collect();
            sources.extend(key.names.iter().map(|name| SpriteSource::Text {
                text: name,
                rgb: TEAMMATE_RGB,
            }));
            bytes += self.bake(device, queue, &sources, Which::Teammates);
            self.teammate_atlas = Some(key.clone());
        }

        let dpr = f64::from(self.dpr);
        let size = f64::from(team.head_size);
        let fan_radius = size * FAN_RADIUS_FACTOR * dpr;
        let face_count = key.faces.iter().flatten().count();
        let (face_sprites, name_sprites) = self
            .teammates
            .sprites
            .split_at(face_count.min(self.teammates.sprites.len()));
        let mut faces = face_sprites.iter();
        let mut names = name_sprites.iter();
        let mut instances = Vec::with_capacity(team.players.len() * 3);
        for (index, player) in team.players.iter().enumerate() {
            let fan = (
                f64::from(player.fan[0]) * fan_radius,
                f64::from(player.fan[1]) * fan_radius,
            );
            let quad = |offset: (f64, f64), extent: f64, kind: OverlayKind, rgba: [u8; 4]| {
                OverlayInstance {
                    world: player.world,
                    offset: [(fan.0 + offset.0) as f32, (fan.1 + offset.1) as f32],
                    size: [extent as f32, extent as f32],
                    uv_rect: [0.0; 4],
                    rgba,
                    min_scale: 0.0,
                    kind: kind as u32,
                }
            };
            // The dot stands in for a face that is loading or failed, and for the faces-off
            // configuration, as on the canvas overlay.
            let below = if key.faces[index].is_some()
                && let Some(face) = faces.next()
            {
                let plate = (size + 2.0) * dpr;
                instances.push(quad(
                    (-plate / 2.0, -plate / 2.0),
                    plate,
                    OverlayKind::Fill,
                    PLATE_RGBA,
                ));
                let edge = size * dpr;
                instances.push(OverlayInstance {
                    uv_rect: face.uv_rect,
                    ..quad(
                        (-edge / 2.0, -edge / 2.0),
                        edge,
                        OverlayKind::Sprite,
                        [255; 4],
                    )
                });
                size / 2.0
            } else {
                let extent = DOT_HALF_EXTENT * 2.0 * dpr;
                let [r, g, b] = TEAMMATE_RGB;
                instances.push(quad(
                    (-extent / 2.0, -extent / 2.0),
                    extent,
                    OverlayKind::Dot,
                    [r, g, b, 255],
                ));
                DOT_RADIUS
            };
            if team.show_names
                && let Some(name) = names.next()
            {
                let x = fan.0.round()
                    - (f64::from(name.advance) / 2.0).round()
                    - f64::from(name.origin[0]);
                let y =
                    (fan.1 + (below + TEAMMATE_NAME_GAP) * dpr).round() - f64::from(name.origin[1]);
                instances.push(OverlayInstance {
                    world: player.world,
                    offset: [x as f32, y as f32],
                    size: name.size,
                    uv_rect: name.uv_rect,
                    rgba: [255; 4],
                    min_scale: TEAMMATE_NAME_MIN_SCALE,
                    kind: OverlayKind::Sprite as u32,
                });
            }
        }
        bytes += self.teammates.upload(device, queue, &instances);
        bytes
    }

    /// Rasterizes `sources` into a new atlas for `which` and records their sprites. Atlas
    /// preparation runs on content changes only, never per frame.
    fn bake(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sources: &[SpriteSource],
        which: Which,
    ) -> u64 {
        let font_ready = font_loaded(FONT_CSS);
        if !font_ready {
            self.font_ready = false;
        }
        let max_side = device.limits().max_texture_dimension_2d;
        let baked = bake_atlas(sources, f64::from(self.dpr), max_side);
        let layer = match which {
            Which::Sites => &mut self.sites,
            Which::Teammates => &mut self.teammates,
        };
        let Some((image, sprites, (width, height))) = baked.and_then(|(image, sprites)| {
            let extent = atlas_extent(
                f64::from(image.width()),
                f64::from(image.height()),
                max_side,
            )?;
            Some((image, sprites, extent))
        }) else {
            web_sys::console::warn_1(&"map overlay: could not prepare its atlas".into());
            layer.clear();
            return 0;
        };
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("overlay-atlas"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            // External image copies require a render-attachment destination.
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        queue.copy_external_image_to_texture(
            &wgpu::CopyExternalImageSourceInfo {
                source: wgpu::ExternalImageSource::ImageData(image),
                origin: wgpu::Origin2d::ZERO,
                flip_y: false,
            },
            wgpu::CopyExternalImageDestInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
                color_space: wgpu::PredefinedColorSpace::Srgb,
                premultiplied_alpha: false,
            },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        layer.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("overlay-bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.params.as_entire_binding(),
                },
            ],
        }));
        layer._atlas = Some(texture);
        layer.sprites = sprites;
        4 * u64::from(size.width) * u64::from(size.height)
    }

    /// The overlay font, if an atlas was baked before it loaded.
    pub(super) fn pending_font(&self) -> Option<&'static str> {
        (!self.font_ready).then_some(FONT_CSS)
    }

    /// Re-bakes both atlases in `font` now that it has loaded.
    pub(super) fn font_loaded(&mut self, font: &str) {
        if font == FONT_CSS && !self.font_ready {
            self.font_ready = true;
            self.invalidate_atlases();
        }
    }

    /// Whether either layer has anything to draw this frame.
    pub(super) fn has_instances(&self) -> bool {
        self.sites.count > 0 || self.teammates.count > 0
    }

    /// Draws site names, then teammates; returns the draw calls made.
    pub(super) fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        viewport_bind_group: &wgpu::BindGroup,
        quad_vertices: &wgpu::Buffer,
        quad_indices: &wgpu::Buffer,
    ) -> u32 {
        let mut draws = 0;
        for layer in [&self.sites, &self.teammates] {
            let Some(bind_group) = layer.bind_group.as_ref().filter(|_| layer.count > 0) else {
                continue;
            };
            if draws == 0 {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, viewport_bind_group, &[]);
                pass.set_vertex_buffer(0, quad_vertices.slice(..));
                pass.set_index_buffer(quad_indices.slice(..), wgpu::IndexFormat::Uint16);
            }
            pass.set_bind_group(1, bind_group, &[]);
            pass.set_vertex_buffer(1, layer.buffer.slice(..));
            pass.draw_indexed(0..6, 0, 0..layer.count);
            draws += 1;
        }
        draws
    }
}

#[derive(Clone, Copy)]
enum Which {
    Sites,
    Teammates,
}

fn allocate(device: &wgpu::Device, capacity: u32) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("overlay-instances"),
        size: u64::from(capacity) * std::mem::size_of::<OverlayInstance>() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn font_loaded(font: &str) -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.fonts().check(font).ok())
        .unwrap_or(true)
}

/// Lays `sources` out in rows and draws them on a scratch canvas: text with the canvas
/// overlays' fill and shadow, faces at their own size without smoothing. Returns the pixels
/// and each source's sprite, or `None` when the document cannot provide a 2D canvas or a
/// sprite or the atlas would not fit in a `max_side` texture (or measured nonsense).
fn bake_atlas(
    sources: &[SpriteSource],
    dpr: f64,
    max_side: u32,
) -> Option<(web_sys::ImageData, Vec<Sprite>)> {
    if !(dpr.is_finite() && dpr > 0.0) {
        return None;
    }
    let canvas = web_sys::window()?
        .document()?
        .create_element("canvas")
        .ok()?
        .dyn_into::<HtmlCanvasElement>()
        .ok()?;
    let ctx = super::get_2d_context(&canvas, true)?;
    let font = format!("{}px {FONT_FAMILY}", FONT_PX * dpr);
    ctx.set_font(&font);

    // Measure, then place: (x, y, sprite) in device pixels.
    let mut placed = Vec::with_capacity(sources.len());
    let (mut x, mut y, mut row_height, mut width) = (0.0f64, 0.0f64, 0.0f64, 1.0f64);
    let atlas_width = f64::from(ATLAS_WIDTH.min(max_side));
    for source in sources {
        let (w, h, origin, advance) = match source {
            SpriteSource::Text { text, .. } => {
                let metrics = ctx.measure_text(text).ok()?;
                let left = metrics.actual_bounding_box_left().max(0.0).ceil();
                let right = metrics
                    .actual_bounding_box_right()
                    .max(metrics.width())
                    .ceil();
                let ascent = metrics.actual_bounding_box_ascent().max(0.0).ceil();
                let descent = metrics.actual_bounding_box_descent().max(0.0).ceil();
                let origin = (SPRITE_MARGIN + left, SPRITE_MARGIN + ascent);
                (
                    origin.0 + right + SPRITE_MARGIN,
                    origin.1 + descent + SPRITE_MARGIN,
                    origin,
                    metrics.width(),
                )
            }
            SpriteSource::Face(_) => (f64::from(FACE_PX), f64::from(FACE_PX), (0.0, 0.0), 0.0),
        };
        // Every sprite has to fit on its own; this also keeps NaN out of the sums below.
        atlas_extent(w, h, max_side)?;
        if x > 0.0 && x + w > atlas_width {
            x = 0.0;
            y += row_height;
            row_height = 0.0;
        }
        placed.push((x, y, w, h, origin, advance));
        x += w;
        width = width.max(x);
        row_height = row_height.max(h);
    }
    let (width_px, height_px) = atlas_extent(width, (y + row_height).max(1.0), max_side)?;
    let (width, height) = (f64::from(width_px), f64::from(height_px));
    // Resizing resets the context's state.
    canvas.set_width(width_px);
    canvas.set_height(height_px);
    ctx.set_font(&font);
    ctx.set_text_align("left");
    ctx.set_text_baseline("alphabetic");
    ctx.set_image_smoothing_enabled(false);

    let mut sprites = Vec::with_capacity(sources.len());
    for (source, &(sx, sy, w, h, origin, advance)) in sources.iter().zip(&placed) {
        match source {
            SpriteSource::Text { text, rgb } => {
                ctx.set_shadow_color(SHADOW_COLOR);
                ctx.set_shadow_blur(SHADOW_BLUR);
                ctx.set_fill_style_str(&format!("rgb({},{},{})", rgb[0], rgb[1], rgb[2]));
                ctx.fill_text(text, sx + origin.0, sy + origin.1).ok()?;
            }
            SpriteSource::Face(image) => {
                ctx.set_shadow_color("rgba(0,0,0,0)");
                ctx.set_shadow_blur(0.0);
                ctx.draw_image_with_html_image_element_and_dw_and_dh(image, sx, sy, w, h)
                    .ok()?;
            }
        }
        sprites.push(Sprite {
            uv_rect: [
                (sx / width) as f32,
                (sy / height) as f32,
                ((sx + w) / width) as f32,
                ((sy + h) / height) as f32,
            ],
            size: [w as f32, h as f32],
            origin: [origin.0 as f32, origin.1 as f32],
            advance: advance as f32,
        });
    }
    let image = ctx.get_image_data(0.0, 0.0, width, height).ok()?;
    // Release the scratch canvas' backing store now rather than at collection.
    canvas.set_width(0);
    canvas.set_height(0);
    Some((image, sprites))
}
