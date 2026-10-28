//! Host overlays: Map Intel site names, war plates, then teammates (faces or dots, and
//! names), drawn after the map and minimap with the main projection (`overlay.wgsl`).
//!
//! Each layer owns one atlas of sprites: text with its shadow, rasterized by Canvas2D in the
//! overlay font at the surface's device pixel ratio, and teammate faces. Atlases are baked
//! only when their content, the ratio or the font changes. Instances carry world anchors and
//! device-pixel offsets, so the camera only moves the viewport uniform; per-instance zoom
//! thresholds are checked in the shader against the exact scale. Instances outside the view
//! are left to the rasterizer: these layers hold tens to a few hundred quads, and choosing
//! visible ones on the CPU would cost an upload on every camera change.

use std::sync::Arc;

use sequoia_shared::war_board::{Ending, StageKind};
use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, HtmlImageElement};

use crate::overlay::{PlateLabel, Prepared, SiteLabels, Teammates, WarPlates, atlas_extent};

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
/// Room around a shadowless plate glyph, device pixels.
const GLYPH_MARGIN: f64 = 1.0;
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
/// Radius of the ring co-located teammates are fanned onto, as a fraction of the head size.
const FAN_RADIUS_FACTOR: f64 = 0.65;
/// Face images are drawn into the atlas at this size, as requested from the skin renderer.
const FACE_PX: u32 = 64;
const ATLAS_WIDTH: u32 = 2048;
/// War plates use the map's pixel face, so they read as part of the territory labelling.
const PLATE_FONT_CSS: &str = "16px Minecraft";
const PLATE_FONT_PX: f64 = 16.0;
const PLATE_FONT_FAMILY: &str = "Minecraft, monospace";
/// Plate box, CSS pixels: padding around the content and the icon-to-text gap.
const PLATE_PAD_X: f64 = 3.0;
const PLATE_PAD_Y: f64 = 1.0;
const PLATE_GAP: f64 = 2.0;
/// Backing around an ending's glyph, CSS pixels, and the glyph's magnification: its 7x7
/// pixel art drawn two pixels to a cell.
const PIN_PAD: f64 = 2.0;
const PIN_ZOOM: f32 = 2.0;
const PLATE_BAR_HEIGHT: f64 = 2.0;
/// Plates show their text from this camera scale; further out they are icon-only pins.
const PLATE_TEXT_MIN_SCALE: f32 = 0.32;
const PLATE_FILL: [u8; 4] = [12, 14, 23, 232];
const PLATE_TRACK: [u8; 3] = [0x3a, 0x3f, 0x5c];
/// Dimming applied to every plate while the feed is reconnecting.
const PLATE_STALE_ALPHA: f32 = 0.5;
/// 7x7 icons, one CSS pixel per cell: the countdown's hourglass and the war's crossed swords.
const HOURGLASS: [&str; 7] = [
    "#######", ".#...#.", "..#.#..", "...#...", "..#.#..", ".#...#.", "#######",
];
const SWORDS: [&str; 7] = [
    "#.....#", ".#...#.", "..#.#..", "...#...", "..#.#..", "##...##", "#.....#",
];
/// An ending's pin: captured, cancelled, or ended some other way.
const CHECK: [&str; 7] = [
    "......#", ".....##", "#...##.", "##.##..", ".###...", "..#....", ".......",
];
const CROSS: [&str; 7] = [
    ".......", ".##.##.", "..###..", "...#...", "..###..", ".##.##.", ".......",
];
const STOP: [&str; 7] = [
    ".......", ".#####.", ".#####.", ".#####.", ".#####.", ".#####.", ".......",
];

/// `kind` in overlay.wgsl.
#[derive(Clone, Copy)]
#[repr(u32)]
enum OverlayKind {
    Sprite = 0,
    /// A white sprite drawn in the instance colour.
    Tinted = 1,
    Dot = 2,
    /// A solid rectangle in the instance colour.
    Rect = 3,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct OverlayInstance {
    world: [f32; 2],
    offset: [f32; 2],
    size: [f32; 2],
    uv_rect: [f32; 4],
    rgba: [u8; 4],
    /// Drawn while `scales[0] <= camera scale < scales[1]`.
    scales: [f32; 2],
    kind: u32,
}

const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
    1 => Float32x2, 2 => Float32x2, 3 => Float32x2, 4 => Float32x4, 5 => Unorm8x4,
    6 => Float32x2, 7 => Uint32
];

/// What one sprite is drawn from.
enum SpriteSource<'a> {
    Text {
        text: &'a str,
        rgb: [u8; 3],
    },
    Face(&'a HtmlImageElement),
    /// One plate glyph, white, without the shadow.
    Glyph(char),
    /// Pixel art, `#` for a white cell, one CSS pixel per cell.
    Pixels(&'a [&'a str]),
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
    plates: Layer,
    teammates: Layer,
    /// What the layers were prepared from.
    sites_from: Prepared<SiteLabels>,
    plates_from: Prepared<WarPlates>,
    teammates_from: Prepared<Teammates>,
    teammate_atlas: Option<TeammateAtlasKey>,
    /// The glyphs in `plates`' atlas. Baked once per ratio and font: plate text changes every
    /// second, and only the instances follow it.
    plate_glyphs: Option<PlateGlyphs>,
    /// The ratio the atlases were baked with.
    dpr: f32,
    /// Fonts an atlas was baked in before they loaded.
    pending_fonts: Vec<&'static str>,
}

/// Where each plate glyph and icon sits in the plate atlas, in device pixels.
struct PlateGlyphs {
    /// Sorted by character.
    glyphs: Vec<(char, Sprite)>,
    hourglass: Sprite,
    swords: Sprite,
    /// Captured, cancelled and ended pins.
    check: Sprite,
    cross: Sprite,
    stop: Sprite,
    /// Baseline to the top of the tallest glyph, and to the bottom of the lowest.
    ascent: f32,
    descent: f32,
}

impl PlateGlyphs {
    fn glyph(&self, ch: char) -> Option<&Sprite> {
        self.glyphs
            .binary_search_by_key(&ch, |(glyph, _)| *glyph)
            .ok()
            .map(|index| &self.glyphs[index].1)
    }
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
            plates: Layer::new(device),
            teammates: Layer::new(device),
            sites_from: Prepared::new(),
            plates_from: Prepared::new(),
            teammates_from: Prepared::new(),
            teammate_atlas: None,
            plate_glyphs: None,
            dpr: 0.0,
            pending_fonts: Vec::new(),
        }
    }

    /// Brings every layer up to date with the host's overlays and the surface ratio; returns
    /// the bytes uploaded. Does nothing when nothing changed.
    pub(super) fn sync(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sites: Option<&Arc<SiteLabels>>,
        plates: Option<&Arc<WarPlates>>,
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
        if let Some(plates) = self.plates_from.update(plates) {
            bytes += self.sync_plates(device, queue, plates);
        }
        if let Some(teammates) = self.teammates_from.update(teammates) {
            bytes += self.sync_teammates(device, queue, teammates);
        }
        bytes
    }

    fn invalidate_atlases(&mut self) {
        self.sites_from.invalidate();
        self.plates_from.invalidate();
        self.teammates_from.invalidate();
        self.teammate_atlas = None;
        self.plate_glyphs = None;
    }

    fn sync_plates(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plates: Option<Arc<WarPlates>>,
    ) -> u64 {
        let Some(plates) = plates.filter(|plates| !plates.plates.is_empty()) else {
            // The glyph atlas stays: it does not depend on which plates are shown.
            self.plates.count = 0;
            return 0;
        };
        let mut bytes = 0;
        if self.plate_glyphs.is_none() {
            bytes += self.bake_plate_glyphs(device, queue);
        }
        let Some(glyphs) = self.plate_glyphs.as_ref() else {
            self.plates.count = 0;
            return bytes;
        };
        let instances = plate_instances(&plates, glyphs, f64::from(self.dpr));
        bytes + self.plates.upload(device, queue, &instances)
    }

    fn bake_plate_glyphs(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> u64 {
        let mut chars: Vec<char> = PlateLabel::WORDS
            .concat()
            .chars()
            .chain("0123456789:".chars())
            .collect();
        chars.sort_unstable();
        chars.dedup();
        let mut sources: Vec<SpriteSource> =
            chars.iter().map(|&ch| SpriteSource::Glyph(ch)).collect();
        for art in [&HOURGLASS, &SWORDS, &CHECK, &CROSS, &STOP] {
            sources.push(SpriteSource::Pixels(art));
        }
        let bytes = self.bake(device, queue, &sources, Which::Plates);
        let sprites = &self.plates.sprites;
        let [glyph_sprites @ .., hourglass, swords, check, cross, stop] = sprites.as_slice() else {
            return bytes;
        };
        if glyph_sprites.len() != chars.len() {
            return bytes;
        }
        let margin = GLYPH_MARGIN as f32;
        let (ascent, descent) = glyph_sprites
            .iter()
            .fold((0.0f32, 0.0f32), |(a, d), sprite| {
                (
                    a.max(sprite.origin[1] - margin),
                    d.max(sprite.size[1] - sprite.origin[1] - margin),
                )
            });
        self.plate_glyphs = Some(PlateGlyphs {
            glyphs: chars
                .into_iter()
                .zip(glyph_sprites.iter().copied())
                .collect(),
            hourglass: *hourglass,
            swords: *swords,
            check: *check,
            cross: *cross,
            stop: *stop,
            ascent,
            descent,
        });
        bytes
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
                scales: [SITE_LABEL_MIN_SCALE.next_up(), f32::MAX],
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
        let mut instances = Vec::with_capacity(team.players.len() * 2);
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
                    scales: [0.0, f32::MAX],
                    kind: kind as u32,
                }
            };
            // The dot stands in for a face that is loading or failed, and for the faces-off
            // configuration, as on the canvas overlay.
            let below = if key.faces[index].is_some()
                && let Some(face) = faces.next()
            {
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
                    scales: [TEAMMATE_NAME_MIN_SCALE, f32::MAX],
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
        let (font_css, font_px, font_family) = match which {
            Which::Plates => (PLATE_FONT_CSS, PLATE_FONT_PX, PLATE_FONT_FAMILY),
            Which::Sites | Which::Teammates => (FONT_CSS, FONT_PX, FONT_FAMILY),
        };
        if !font_loaded(font_css) && !self.pending_fonts.contains(&font_css) {
            self.pending_fonts.push(font_css);
        }
        let max_side = device.limits().max_texture_dimension_2d;
        let baked = bake_atlas(
            sources,
            (font_px, font_family),
            f64::from(self.dpr),
            max_side,
        );
        let layer = match which {
            Which::Sites => &mut self.sites,
            Which::Plates => &mut self.plates,
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

    /// A font an atlas was baked in before it loaded.
    pub(super) fn pending_font(&self) -> Option<&'static str> {
        self.pending_fonts.first().copied()
    }

    /// Re-bakes the atlases now that `font` has loaded.
    pub(super) fn font_loaded(&mut self, font: &str) {
        let pending = self.pending_fonts.len();
        self.pending_fonts.retain(|waiting| *waiting != font);
        if self.pending_fonts.len() != pending {
            self.invalidate_atlases();
        }
    }

    /// Whether any layer has anything to draw this frame.
    pub(super) fn has_instances(&self) -> bool {
        self.sites.count > 0 || self.plates.count > 0 || self.teammates.count > 0
    }

    /// Draws site names, war plates, then teammates; returns the draw calls made.
    pub(super) fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        viewport_bind_group: &wgpu::BindGroup,
        quad_vertices: &wgpu::Buffer,
        quad_indices: &wgpu::Buffer,
    ) -> u32 {
        let mut draws = 0;
        for layer in [&self.sites, &self.plates, &self.teammates] {
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
    Plates,
    Teammates,
}

/// Plate colours as (border and icon, text): queued lavender, entered amber, war red and a
/// capture green. Endings that do not say who won are grey.
fn plate_tones(label: PlateLabel) -> ([u8; 3], [u8; 3]) {
    match label {
        PlateLabel::Front {
            stage: StageKind::Queued,
            ..
        } => ([0xc7, 0xa3, 0xf0], [0xe6, 0xd8, 0xf8]),
        PlateLabel::Front {
            stage: StageKind::Entered,
            ..
        } => ([0xff, 0xa1, 0x3d], [0xff, 0xd6, 0xa3]),
        PlateLabel::Front {
            stage: StageKind::Fighting,
            ..
        } => ([0xff, 0x45, 0x45], [0xff, 0xb4, 0xb4]),
        PlateLabel::Ending(Ending::Captured) => ([0x50, 0xc8, 0x78], [0xb8, 0xf0, 0xca]),
        PlateLabel::Ending(Ending::Cancelled | Ending::Ended) => {
            ([0x6f, 0x74, 0x8f], [0xc4, 0xc2, 0xbc])
        }
    }
}

/// Lays each plate out around its anchor in whole device pixels, so glyphs land texel for
/// pixel: a bordered box holding the stage icon and the text, and for a war the tower's
/// remaining health along the bottom. Below [`PLATE_TEXT_MIN_SCALE`] the same plate shrinks
/// to a pin with only its icon, so neighbouring war territories do not bury each other. An
/// ending is only ever a small glyph on a dark backing.
fn plate_instances(plates: &WarPlates, glyphs: &PlateGlyphs, dpr: f64) -> Vec<OverlayInstance> {
    let px = |css: f64| (css * dpr).round().max(1.0);
    let metrics = PlateMetrics {
        pad_x: px(PLATE_PAD_X),
        pad_y: px(PLATE_PAD_Y),
        gap: px(PLATE_GAP),
        border: px(1.0),
        bar: px(PLATE_BAR_HEIGHT),
        line: f64::from(glyphs.ascent + glyphs.descent).ceil(),
        fade: if plates.stale { PLATE_STALE_ALPHA } else { 1.0 },
    };
    let mut instances = Vec::with_capacity(plates.plates.len() * 14);
    for plate in &plates.plates {
        if let PlateLabel::Ending(ending) = plate.label {
            let glyph = match ending {
                Ending::Captured => glyphs.check,
                Ending::Cancelled => glyphs.cross,
                Ending::Ended => glyphs.stop,
            };
            // A capture keeps its green; the others need the light ink to read on the backing.
            let (accent, ink) = plate_tones(plate.label);
            let tone = if ending == Ending::Captured {
                accent
            } else {
                ink
            };
            metrics.push_pin(&mut instances, plate.world, glyph, tone, px(PIN_PAD));
            continue;
        }
        let (icon, health) = match plate.label {
            PlateLabel::Front {
                stage: StageKind::Fighting,
                health,
                ..
            } => (Some(glyphs.swords), health),
            PlateLabel::Front { .. } => (Some(glyphs.hourglass), None),
            PlateLabel::Ending(_) => (None, None),
        };
        let (accent, ink) = plate_tones(plate.label);
        let text = plate.label.text().unwrap_or_default();
        let full: Vec<&Sprite> = text.chars().filter_map(|ch| glyphs.glyph(ch)).collect();
        let mut push = |glyph_run: &[&Sprite], health, scales| {
            metrics.push(
                &mut instances,
                PlateContent {
                    world: plate.world,
                    icon,
                    glyphs: glyph_run,
                    health,
                    accent,
                    ink,
                    ascent: glyphs.ascent,
                },
                scales,
            );
        };
        push(&full, health, [PLATE_TEXT_MIN_SCALE, f32::MAX]);
        push(&[], None, [0.0, PLATE_TEXT_MIN_SCALE]);
    }
    instances
}

/// Device-pixel plate geometry, fixed for one atlas.
struct PlateMetrics {
    pad_x: f64,
    pad_y: f64,
    gap: f64,
    border: f64,
    bar: f64,
    line: f64,
    fade: f32,
}

/// What one plate box shows.
struct PlateContent<'a> {
    world: [f32; 2],
    icon: Option<Sprite>,
    glyphs: &'a [&'a Sprite],
    health: Option<f32>,
    accent: [u8; 3],
    ink: [u8; 3],
    ascent: f32,
}

impl PlateMetrics {
    fn color(&self, [r, g, b]: [u8; 3], alpha: u8) -> [u8; 4] {
        [r, g, b, (f32::from(alpha) * self.fade).round() as u8]
    }

    /// An ending's glyph on a backing just larger than it, centred on the anchor, at every
    /// zoom.
    fn push_pin(
        &self,
        instances: &mut Vec<OverlayInstance>,
        world: [f32; 2],
        glyph: Sprite,
        accent: [u8; 3],
        pad: f64,
    ) {
        let size = [glyph.size[0] * PIN_ZOOM, glyph.size[1] * PIN_ZOOM];
        let [w, h] = [f64::from(size[0]), f64::from(size[1])];
        let (left, top) = (-((w / 2.0).round() + pad), -((h / 2.0).round() + pad));
        let [fr, fg, fb, fa] = PLATE_FILL;
        let place =
            |x: f64, y: f64, size: [f32; 2], uv_rect, rgba, kind: OverlayKind| OverlayInstance {
                world,
                offset: [x as f32, y as f32],
                size,
                uv_rect,
                rgba,
                scales: [0.0, f32::MAX],
                kind: kind as u32,
            };
        instances.push(place(
            left,
            top,
            [(w + pad * 2.0) as f32, (h + pad * 2.0) as f32],
            [0.0; 4],
            self.color([fr, fg, fb], fa),
            OverlayKind::Rect,
        ));
        instances.push(place(
            left + pad,
            top + pad,
            size,
            glyph.uv_rect,
            self.color(accent, 255),
            OverlayKind::Tinted,
        ));
    }

    /// One plate box, centred on its anchor and straddling the territory's top edge, shown
    /// while the camera scale is within `scales`.
    fn push(&self, instances: &mut Vec<OverlayInstance>, plate: PlateContent, scales: [f32; 2]) {
        let place = |x: f64, y: f64, size: [f32; 2], uv_rect: [f32; 4], rgba, kind: OverlayKind| {
            OverlayInstance {
                world: plate.world,
                offset: [x as f32, y as f32],
                size,
                uv_rect,
                rgba,
                scales,
                kind: kind as u32,
            }
        };
        let rect = |x: f64, y: f64, w: f64, h: f64, rgba| {
            place(
                x,
                y,
                [w as f32, h as f32],
                [0.0; 4],
                rgba,
                OverlayKind::Rect,
            )
        };
        // A plate without an icon still needs something to show as a pin.
        let marker = plate.icon.map_or([self.line * 0.6; 2], |icon| {
            [f64::from(icon.size[0]), f64::from(icon.size[1])]
        });
        let text_w: f64 = plate
            .glyphs
            .iter()
            .map(|glyph| f64::from(glyph.advance).round())
            .sum();
        let has_text = !plate.glyphs.is_empty();
        let show_marker = plate.icon.is_some() || !has_text;
        let content_w = if show_marker { marker[0] } else { 0.0 }
            + if show_marker && has_text {
                self.gap
            } else {
                0.0
            }
            + text_w;
        let content_h = if has_text {
            self.line.max(marker[1])
        } else {
            marker[1]
        };
        let bar_h = if plate.health.is_some() {
            self.bar + self.border
        } else {
            0.0
        };
        let inner_w = self.pad_x * 2.0 + content_w;
        let inner_h = self.pad_y * 2.0 + content_h + bar_h;
        let left = -((inner_w / 2.0).round() + self.border);
        let top = -((inner_h / 2.0).round() + self.border);
        let [fr, fg, fb, fa] = PLATE_FILL;
        instances.push(rect(
            left,
            top,
            inner_w + self.border * 2.0,
            inner_h + self.border * 2.0,
            self.color(plate.accent, 255),
        ));
        instances.push(rect(
            left + self.border,
            top + self.border,
            inner_w,
            inner_h,
            self.color([fr, fg, fb], fa),
        ));

        let content_top = top + self.border + self.pad_y;
        let mut x = left + self.border + self.pad_x;
        if show_marker {
            let y = content_top + ((content_h - marker[1]) / 2.0).round();
            let accent = self.color(plate.accent, 255);
            instances.push(match plate.icon {
                Some(icon) => place(x, y, icon.size, icon.uv_rect, accent, OverlayKind::Tinted),
                None => rect(x, y, marker[0], marker[1], accent),
            });
            x += marker[0] + self.gap;
        }
        let baseline =
            content_top + ((content_h - self.line) / 2.0).round() + f64::from(plate.ascent).round();
        for glyph in plate.glyphs {
            instances.push(place(
                x - f64::from(glyph.origin[0]),
                baseline - f64::from(glyph.origin[1]),
                glyph.size,
                glyph.uv_rect,
                self.color(plate.ink, 255),
                OverlayKind::Tinted,
            ));
            x += f64::from(glyph.advance).round();
        }
        if let Some(health) = plate.health {
            let bar_x = left + self.border + self.pad_x;
            let bar_y = top + self.border + inner_h - self.pad_y - self.bar;
            let bar_w = inner_w - self.pad_x * 2.0;
            instances.push(rect(
                bar_x,
                bar_y,
                bar_w,
                self.bar,
                self.color(PLATE_TRACK, 255),
            ));
            let filled = (bar_w * f64::from(health.clamp(0.0, 1.0))).round();
            if filled > 0.0 {
                instances.push(rect(
                    bar_x,
                    bar_y,
                    filled,
                    self.bar,
                    self.color(plate.accent, 255),
                ));
            }
        }
    }
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
    (font_px, font_family): (f64, &str),
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
    let font = format!("{}px {font_family}", font_px * dpr);
    ctx.set_font(&font);
    // Device pixels per pixel-art cell: whole, so the art stays crisp.
    let cell = dpr.round().max(1.0);
    let measure = |text: &str, margin: f64| {
        let metrics = ctx.measure_text(text).ok()?;
        let left = metrics.actual_bounding_box_left().max(0.0).ceil();
        let right = metrics
            .actual_bounding_box_right()
            .max(metrics.width())
            .ceil();
        let ascent = metrics.actual_bounding_box_ascent().max(0.0).ceil();
        let descent = metrics.actual_bounding_box_descent().max(0.0).ceil();
        let origin = (margin + left, margin + ascent);
        Some((
            origin.0 + right + margin,
            origin.1 + descent + margin,
            origin,
            metrics.width(),
        ))
    };

    // Measure, then place: (x, y, sprite) in device pixels.
    let mut placed = Vec::with_capacity(sources.len());
    let (mut x, mut y, mut row_height, mut width) = (0.0f64, 0.0f64, 0.0f64, 1.0f64);
    let atlas_width = f64::from(ATLAS_WIDTH.min(max_side));
    for source in sources {
        let (w, h, origin, advance) = match source {
            SpriteSource::Text { text, .. } => measure(text, SPRITE_MARGIN)?,
            SpriteSource::Glyph(ch) => measure(ch.encode_utf8(&mut [0; 4]), GLYPH_MARGIN)?,
            SpriteSource::Face(_) => (f64::from(FACE_PX), f64::from(FACE_PX), (0.0, 0.0), 0.0),
            SpriteSource::Pixels(rows) => {
                let columns = rows.iter().map(|row| row.len()).max().unwrap_or(0);
                (
                    columns as f64 * cell,
                    rows.len() as f64 * cell,
                    (0.0, 0.0),
                    0.0,
                )
            }
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
            SpriteSource::Glyph(ch) => {
                ctx.set_shadow_color("rgba(0,0,0,0)");
                ctx.set_shadow_blur(0.0);
                ctx.set_fill_style_str("#fff");
                ctx.fill_text(ch.encode_utf8(&mut [0; 4]), sx + origin.0, sy + origin.1)
                    .ok()?;
            }
            SpriteSource::Pixels(rows) => {
                ctx.set_fill_style_str("#fff");
                for (row_index, row) in rows.iter().enumerate() {
                    for (column, _) in row.bytes().enumerate().filter(|(_, pixel)| *pixel == b'#') {
                        ctx.fill_rect(
                            sx + column as f64 * cell,
                            sy + row_index as f64 * cell,
                            cell,
                            cell,
                        );
                    }
                }
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
