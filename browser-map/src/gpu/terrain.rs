//! The terrain tiles on the GPU.
//!
//! The browser decodes a tile's image off the main thread into a premultiplied bitmap whenever
//! a texture has to be built: on arrival, from the download, and for a later renderer (after a
//! lost context) from the browser's cache. The bitmap is uploaded as it is, at most one tile a
//! frame, and closed: neither the download nor a decoded copy outlives its upload, and a tile
//! arriving never stalls a frame with a decode, a read-back through a 2D canvas or a copy
//! through WebAssembly memory.
//!
//! A high-quality tile with a palette (see [`crate::tile_palettes`]) keeps its full resolution
//! as one byte a texel: palette indices, which a GPU pass finds from the decoded colours, plus
//! the RGBA mip levels from 2 down. The tile shader takes level 1 from the indices as the mip
//! chain would make it. That is the same terrain at every zoom in under a third of the memory
//! of a full RGBA chain, which other tiles (the low-quality placeholders) keep. The main
//! tiles keep theirs together, in an atlas laid out like the world (see
//! [`crate::terrain_layout`]), so they are drawn in one call rather than one each. A tile
//! keeps its texture until a better one has been built.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use web_sys::{ImageBitmap, ImageBitmapOptions, PremultiplyAlpha};

use sequoia_shared::MAP_TILES;

use super::{full_mip_level_count, mipmaps, upload_external_image};
use crate::terrain_layout::{AtlasLayout, CELL, CELL_LEVEL, atlas_layout};
use crate::tile_palettes::{self, PALETTE_LEN};
use crate::tiles::{LoadedTile, TileQuality, fetch_blob};

/// Width of the atlas's cell map: one upload stride of R8 texels, more than any atlas needs.
const CELL_ROW: u32 = 256;

/// Decodes in flight at once. Each holds a decoded tile, up to 8 MB, until it is uploaded,
/// and uploads go one a frame: two keep that going without holding more.
const MAX_DECODES: usize = 2;

/// A tile's texture, ready to draw.
pub(super) struct TileTexture {
    pub(super) bind_group: wgpu::BindGroup,
    /// Drawn with the palette pipeline rather than the RGBA one.
    pub(super) palette: bool,
    /// World x, z, width and height.
    pub(super) rect: [f32; 4],
    quality: TileQuality,
}

/// Where a tile's decode stands.
enum Decode {
    Pending,
    Ready(TileQuality, ImageBitmap),
    /// The browser could not decode it; not retried at this quality.
    Failed(TileQuality),
}

type DecodeSlot = Rc<RefCell<Decode>>;

/// The main tiles' palette indices in one world-sized texture, with their RGBA levels from 2
/// down, their palettes and which of them have loaded: drawn as one quad.
struct WorldAtlas {
    layout: AtlasLayout,
    index: wgpu::Texture,
    coarse: wgpu::Texture,
    palettes: wgpu::Texture,
    cells: wgpu::Texture,
    /// Per cell, its tile's number plus one once that tile has loaded, else 0.
    cell_values: Vec<u8>,
    /// Which members have their full-resolution image in the atlas.
    loaded: Vec<bool>,
    bind_group: wgpu::BindGroup,
}

pub(super) struct Terrain {
    bind_group_layout: wgpu::BindGroupLayout,
    palette_bind_group_layout: wgpu::BindGroupLayout,
    atlas_bind_group_layout: wgpu::BindGroupLayout,
    atlas: Option<WorldAtlas>,
    index_bind_group_layout: wgpu::BindGroupLayout,
    index_pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    mips: mipmaps::MipChain,
    textures: HashMap<usize, TileTexture>,
    decodes: HashMap<usize, DecodeSlot>,
    failed: HashMap<usize, TileQuality>,
    /// The tile set last synced, by id and quality.
    signature: Option<u64>,
    revision: u64,
}

impl Terrain {
    pub(super) fn new(
        device: &wgpu::Device,
        bind_group_layout: wgpu::BindGroupLayout,
        sampler: wgpu::Sampler,
        mips: mipmaps::MipChain,
    ) -> Self {
        let palette_bind_group_layout = palette_bind_group_layout(device);
        let index_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("tile-index-bgl"),
                entries: &[
                    texture_entry(0),
                    texture_entry(1),
                    uniform_entry(2, wgpu::ShaderStages::FRAGMENT),
                ],
            });
        let index_pipeline = index_pipeline(device, &index_bind_group_layout);
        let atlas_bind_group_layout = atlas_bind_group_layout(device);
        let atlas = atlas_layout(MAP_TILES)
            .map(|layout| WorldAtlas::new(device, &atlas_bind_group_layout, &sampler, layout));
        Self {
            bind_group_layout,
            palette_bind_group_layout,
            atlas_bind_group_layout,
            atlas,
            index_bind_group_layout,
            index_pipeline,
            sampler,
            mips,
            textures: HashMap::new(),
            decodes: HashMap::new(),
            failed: HashMap::new(),
            signature: None,
            revision: 0,
        }
    }

    /// The layout of an RGBA tile's bind group, for the pipeline that draws them.
    pub(super) fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.bind_group_layout
    }

    /// The layout of a palette tile's bind group, for the pipeline that draws them.
    pub(super) fn palette_bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.palette_bind_group_layout
    }

    /// The layout of the atlas's bind group, for the pipeline that draws it.
    pub(super) fn atlas_bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.atlas_bind_group_layout
    }

    pub(super) fn texture(&self, tile_id: usize) -> Option<&TileTexture> {
        self.textures.get(&tile_id)
    }

    /// Whether `tile_id` is drawn as part of the atlas rather than on its own.
    pub(super) fn in_atlas(&self, tile_id: usize) -> bool {
        self.atlas
            .as_ref()
            .is_some_and(|atlas| atlas.loaded.get(tile_id).copied().unwrap_or(false))
    }

    /// The atlas's bind group, for one draw that covers every tile in it.
    pub(super) fn atlas_bind_group(&self) -> Option<&wgpu::BindGroup> {
        self.atlas.as_ref().map(|atlas| &atlas.bind_group)
    }

    /// The best image of `tile_id` the GPU has.
    fn quality(&self, tile_id: usize) -> Option<TileQuality> {
        if self.in_atlas(tile_id) {
            return Some(TileQuality::High);
        }
        self.textures.get(&tile_id).map(|texture| texture.quality)
    }

    pub(super) fn sampler(&self) -> &wgpu::Sampler {
        &self.sampler
    }

    /// Changes whenever the uploaded tile set does.
    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    /// Brings the textures up to `tiles`: drops tiles that are gone, starts decoding new or
    /// better ones and builds one decoded tile. Returns whether decodes are still to land.
    pub(super) fn sync(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        gl: &web_sys::WebGl2RenderingContext,
        tiles: &[LoadedTile],
    ) -> bool {
        let signature = signature(tiles);
        if self.signature != Some(signature) {
            self.signature = Some(signature);
            let present = |id: &usize| tiles.iter().any(|tile| tile.id == *id);
            let before = self.textures.len();
            self.textures.retain(|id, _| present(id));
            self.decodes.retain(|id, slot| present(id) || release(slot));
            self.failed.retain(|id, _| present(id));
            let mut changed = self.textures.len() != before;
            if let Some(atlas) = self.atlas.as_mut() {
                for id in 0..atlas.loaded.len() {
                    if atlas.loaded[id] && !present(&id) {
                        atlas.unload(queue, id);
                        changed = true;
                    }
                }
            }
            if changed {
                self.revision = self.revision.wrapping_add(1);
            }
        }

        // Settle decodes, then start new ones up to the limit, in load order (nearest the
        // first view first).
        let mut decoding = 0;
        let failed = &mut self.failed;
        self.decodes.retain(|id, slot| match &*slot.borrow() {
            Decode::Pending => {
                decoding += 1;
                true
            }
            Decode::Ready(..) => true,
            Decode::Failed(quality) => {
                failed.insert(*id, *quality);
                false
            }
        });
        let mut waiting = false;
        for tile in tiles {
            let have = self.quality(tile.id);
            if have.is_some_and(|have| have >= tile.quality)
                || self.decodes.contains_key(&tile.id)
                || self.failed.get(&tile.id) == Some(&tile.quality)
            {
                continue;
            }
            if decoding >= MAX_DECODES {
                waiting = true;
                break;
            }
            self.decodes.insert(tile.id, decode(tile));
            decoding += 1;
        }

        let ready = tiles.iter().find(|tile| {
            self.decodes
                .get(&tile.id)
                .is_some_and(|slot| matches!(*slot.borrow(), Decode::Ready(..)))
        });
        if let Some(tile) = ready
            && let Some(slot) = self.decodes.remove(&tile.id)
            && let Decode::Ready(quality, bitmap) = slot.replace(Decode::Pending)
        {
            if self.quality(tile.id).is_none_or(|have| have < quality) {
                match self.build(device, queue, gl, tile, quality, &bitmap) {
                    Some(texture) => {
                        self.textures.insert(tile.id, texture);
                    }
                    // In the atlas now; its placeholder goes.
                    None => {
                        self.textures.remove(&tile.id);
                    }
                }
                self.revision = self.revision.wrapping_add(1);
            }
            bitmap.close();
        }
        waiting || !self.decodes.is_empty()
    }

    /// Builds `tile`'s texture from its decoded image, or writes it into the atlas (`None`).
    fn build(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        gl: &web_sys::WebGl2RenderingContext,
        tile: &LoadedTile,
        quality: TileQuality,
        bitmap: &ImageBitmap,
    ) -> Option<TileTexture> {
        let (width, height) = (bitmap.width().max(1), bitmap.height().max(1));
        let levels = full_mip_level_count(width, height);
        // Drawing the mip levels needs a render-attachment destination, and so do external
        // image copies.
        let full = create_texture(
            device,
            "tile-tex",
            wgpu::TextureFormat::Rgba8Unorm,
            (width, height),
            levels,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        // The bitmap was premultiplied when decoded, so the mip levels average colour and
        // coverage together. WebGL uploads a bitmap as it was decoded, and wgpu accepts a
        // bitmap only described as not premultiplied on upload.
        upload_external_image(
            queue,
            &full,
            wgpu::ExternalImageSource::ImageBitmap(bitmap.clone()),
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            gl,
            false,
        );
        self.mips.generate(device, queue, &full);

        let rect = tile_rect(tile);
        let palette = match quality {
            TileQuality::High if levels > 2 => tile_palettes::palette(tile.id),
            _ => None,
        };
        let Some(colours) = palette else {
            return Some(self.rgba_tile(device, rect, quality, &full));
        };
        let palette = palette_texture(device, queue, colours);
        let member = self
            .atlas
            .as_ref()
            .and_then(|atlas| atlas.layout.members.get(tile.id).copied().flatten())
            .filter(|member| member[2] == width && member[3] == height);
        if let Some(member) = member {
            let index_target = self.atlas.as_ref().map(|atlas| atlas.index.clone())?;
            self.index_pass(device, queue, &full, &palette, &index_target, member);
            if let Some(atlas) = self.atlas.as_mut() {
                atlas.load(device, queue, tile.id, member, &full, colours);
            }
            return None;
        }
        let index = create_texture(
            device,
            "tile-index-tex",
            wgpu::TextureFormat::R8Unorm,
            (width, height),
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        self.index_pass(
            device,
            queue,
            &full,
            &palette,
            &index,
            [0, 0, width, height],
        );
        let coarse = copy_from_level(device, queue, &full, 2);
        // `full` goes here: only the indices, the palette and levels 2 and up stay.
        let rect_buffer = rect_buffer(device, rect);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tile-palette-bg"),
            layout: &self.palette_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: rect_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view(&index)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&view(&palette)),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&view(&coarse)),
                },
            ],
        });
        Some(TileTexture {
            bind_group,
            palette: true,
            rect,
            quality,
        })
    }

    /// A tile drawn from its whole RGBA mip chain.
    fn rgba_tile(
        &self,
        device: &wgpu::Device,
        rect: [f32; 4],
        quality: TileQuality,
        full: &wgpu::Texture,
    ) -> TileTexture {
        let rect_buffer = rect_buffer(device, rect);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tile-bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: rect_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view(full)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        TileTexture {
            bind_group,
            palette: false,
            rect,
            quality,
        }
    }

    /// Writes the palette index of every texel of `full`'s level 0 into `target`, at the
    /// texel rectangle `rect` (x, y, width, height), keeping the rest of `target`.
    fn index_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        full: &wgpu::Texture,
        palette: &wgpu::Texture,
        target: &wgpu::Texture,
        rect: [u32; 4],
    ) {
        let source = full.create_view(&wgpu::TextureViewDescriptor {
            label: Some("tile-index-source"),
            base_mip_level: 0,
            mip_level_count: Some(1),
            ..Default::default()
        });
        let origin = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("tile-index-origin"),
                contents: bytemuck::cast_slice(&[rect[0] as i32, rect[1] as i32, 0, 0]),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tile-index-bg"),
            layout: &self.index_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view(palette)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: origin.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tile-index-encoder"),
        });
        {
            let target = view(target);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tile-index-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_viewport(
                rect[0] as f32,
                rect[1] as f32,
                rect[2] as f32,
                rect[3] as f32,
                0.0,
                1.0,
            );
            pass.set_pipeline(&self.index_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}

impl WorldAtlas {
    fn new(
        device: &wgpu::Device,
        bind_group_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        layout: AtlasLayout,
    ) -> Self {
        let (width, height) = layout.size;
        let (cells_w, cells_h) = layout.cells();
        // Every atlas texture can be drawn into, so wgpu clears it on the GPU before its first
        // partial write instead of uploading zeros, megabytes of them for the large ones.
        let index = create_texture(
            device,
            "terrain-atlas-index",
            wgpu::TextureFormat::R8Unorm,
            (width, height),
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let coarse = create_texture(
            device,
            "terrain-atlas-coarse",
            wgpu::TextureFormat::Rgba8Unorm,
            (width >> 2, height >> 2),
            CELL_LEVEL - 1,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let palettes = create_texture(
            device,
            "terrain-atlas-palettes",
            wgpu::TextureFormat::Rgba8Unorm,
            (PALETTE_LEN as u32, layout.members.len().max(1) as u32),
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        // Rows of whole upload strides, so writes need no padding.
        let cells = create_texture(
            device,
            "terrain-atlas-cells",
            wgpu::TextureFormat::R8Unorm,
            (CELL_ROW, cells_h),
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let uniform = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("terrain-atlas-ubo"),
                contents: bytemuck::cast_slice(&[
                    layout.origin.0 as f32,
                    layout.origin.1 as f32,
                    width as f32,
                    height as f32,
                ]),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terrain-atlas-bg"),
            layout: bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view(&index)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&view(&coarse)),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&view(&palettes)),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&view(&cells)),
                },
            ],
        });
        let tiles = layout.members.len();
        debug_assert!(cells_w <= CELL_ROW);
        Self {
            cell_values: vec![0; (CELL_ROW * cells_h) as usize],
            loaded: vec![false; tiles],
            layout,
            index,
            coarse,
            palettes,
            cells,
            bind_group,
        }
    }

    /// Adds tile `id`, whose palette indices are already at `rect`: its RGBA levels 2 to
    /// [`CELL_LEVEL`] from `full`, its palette, and its cells.
    fn load(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: usize,
        rect: [u32; 4],
        full: &wgpu::Texture,
        colours: &[u8],
    ) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("terrain-atlas-levels"),
        });
        for level in 2..=CELL_LEVEL.min(full.mip_level_count().saturating_sub(1)) {
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: full,
                    mip_level: level,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &self.coarse,
                    mip_level: level - 2,
                    origin: wgpu::Origin3d {
                        x: rect[0] >> level,
                        y: rect[1] >> level,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: rect[2] >> level,
                    height: rect[3] >> level,
                    depth_or_array_layers: 1,
                },
            );
        }
        queue.submit(std::iter::once(encoder.finish()));
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.palettes,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: id as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            colours,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(PALETTE_LEN as u32 * 4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: PALETTE_LEN as u32,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        self.set_cells(queue, rect, id as u8 + 1);
        self.loaded[id] = true;
    }

    /// Stops drawing tile `id`.
    fn unload(&mut self, queue: &wgpu::Queue, id: usize) {
        if let Some(rect) = self.layout.members.get(id).copied().flatten() {
            self.set_cells(queue, rect, 0);
        }
        self.loaded[id] = false;
    }

    fn set_cells(&mut self, queue: &wgpu::Queue, rect: [u32; 4], value: u8) {
        let (_, cells_h) = self.layout.cells();
        for y in rect[1] / CELL..(rect[1] + rect[3]) / CELL {
            for x in rect[0] / CELL..(rect[0] + rect[2]) / CELL {
                self.cell_values[(y * CELL_ROW + x) as usize] = value;
            }
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.cells,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.cell_values,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(CELL_ROW),
                rows_per_image: Some(cells_h),
            },
            wgpu::Extent3d {
                width: CELL_ROW,
                height: cells_h,
                depth_or_array_layers: 1,
            },
        );
    }
}

fn view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

fn rect_buffer(device: &wgpu::Device, rect: [f32; 4]) -> wgpu::Buffer {
    wgpu::util::DeviceExt::create_buffer_init(
        device,
        &wgpu::util::BufferInitDescriptor {
            label: Some("tile-rect-ubo"),
            contents: bytemuck::cast_slice(&rect),
            usage: wgpu::BufferUsages::UNIFORM,
        },
    )
}

fn create_texture(
    device: &wgpu::Device,
    label: &'static str,
    format: wgpu::TextureFormat,
    (width, height): (u32, u32),
    levels: u32,
    extra_usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels.max(1),
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | extra_usage,
        view_formats: &[],
    })
}

/// A texture holding `source`'s levels from `skip` down, copied level by level.
fn copy_from_level(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    source: &wgpu::Texture,
    skip: u32,
) -> wgpu::Texture {
    let size = source.size();
    let levels = source.mip_level_count() - skip;
    let width = (size.width >> skip).max(1);
    let height = (size.height >> skip).max(1);
    let target = create_texture(
        device,
        "tile-coarse-tex",
        source.format(),
        (width, height),
        levels,
        wgpu::TextureUsages::empty(),
    );
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("tile-level-copy"),
    });
    for level in 0..levels {
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: source,
                mip_level: skip + level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: (width >> level).max(1),
                height: (height >> level).max(1),
                depth_or_array_layers: 1,
            },
        );
    }
    queue.submit(std::iter::once(encoder.finish()));
    target
}

fn palette_texture(device: &wgpu::Device, queue: &wgpu::Queue, colours: &[u8]) -> wgpu::Texture {
    let texture = create_texture(
        device,
        "tile-palette-tex",
        wgpu::TextureFormat::Rgba8Unorm,
        (PALETTE_LEN as u32, 1),
        1,
        wgpu::TextureUsages::empty(),
    );
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        colours,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(PALETTE_LEN as u32 * 4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: PALETTE_LEN as u32,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    texture
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn atlas_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("terrain-atlas-bgl"),
        entries: &[
            uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
            texture_entry(1),
            texture_entry(2),
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            texture_entry(4),
            texture_entry(5),
        ],
    })
}

fn palette_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("tile-palette-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            texture_entry(1),
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            texture_entry(3),
            texture_entry(4),
        ],
    })
}

fn index_pipeline(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("tile-index-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("tile_index.wgsl").into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("tile-index-pl"),
        bind_group_layouts: &[layout],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("tile-index-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::R8Unorm,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    })
}

/// Frees a decoded bitmap that will not be uploaded; false, for `retain`.
fn release(slot: &DecodeSlot) -> bool {
    if let Decode::Ready(_, bitmap) = &*slot.borrow() {
        bitmap.close();
    }
    false
}

/// World x, z, width and height; tile bounds are inclusive.
fn tile_rect(tile: &LoadedTile) -> [f32; 4] {
    let x1 = tile.x1.min(tile.x2) as f32;
    let z1 = tile.z1.min(tile.z2) as f32;
    let width = (tile.x1.max(tile.x2) - tile.x1.min(tile.x2) + 1) as f32;
    let height = (tile.z1.max(tile.z2) - tile.z1.min(tile.z2) + 1) as f32;
    [x1, z1, width, height]
}

/// Starts the browser decoding `tile` into a premultiplied bitmap: the image as downloaded,
/// the first time, and afterwards the browser's cached copy.
fn decode(tile: &LoadedTile) -> DecodeSlot {
    let quality = tile.quality;
    let slot = Rc::new(RefCell::new(Decode::Pending));
    let downloaded = tile.image.lock().ok().and_then(|mut image| image.take());
    let src = tile.src.clone();
    let target = Rc::downgrade(&slot);
    wasm_bindgen_futures::spawn_local(async move {
        let image = match downloaded {
            Some(image) => Some(image),
            None => fetch_blob(&src).await,
        };
        let options = ImageBitmapOptions::new();
        options.set_premultiply_alpha(PremultiplyAlpha::Premultiply);
        let promise = image.and_then(|image| {
            web_sys::window()?
                .create_image_bitmap_with_blob_and_image_bitmap_options(&image, &options)
                .ok()
        });
        let decoded = match promise {
            Some(promise) => wasm_bindgen_futures::JsFuture::from(promise)
                .await
                .ok()
                .and_then(|bitmap| bitmap.dyn_into::<ImageBitmap>().ok()),
            None => None,
        };
        match (target.upgrade(), decoded) {
            (Some(slot), Some(bitmap)) => *slot.borrow_mut() = Decode::Ready(quality, bitmap),
            (Some(slot), None) => *slot.borrow_mut() = Decode::Failed(quality),
            // The tile went, or the renderer did, while it decoded.
            (None, Some(bitmap)) => bitmap.close(),
            (None, None) => {}
        }
    });
    slot
}

/// Identifies a tile set by id and quality, in load order.
fn signature(tiles: &[LoadedTile]) -> u64 {
    tiles.iter().fold(0u64, |acc, tile| {
        let quality_bits = match tile.quality {
            TileQuality::Low => 1u64,
            TileQuality::High => 2u64,
        };
        acc.wrapping_mul(1_099_511_628_211)
            .wrapping_add(((tile.id as u64) << 2) ^ quality_bits)
    })
}
