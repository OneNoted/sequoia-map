//! The terrain tiles on the GPU.
//!
//! A tile's encoded image stays in memory, and the browser decodes it off the main thread into
//! a premultiplied bitmap whenever a texture has to be built: on arrival, and again for a new
//! renderer. The bitmap is uploaded as it is, at most one tile a frame, and closed: no decoded
//! copy outlives its upload, and a tile arriving never stalls a frame with a decode, a
//! read-back through a 2D canvas or a copy through WebAssembly memory.
//!
//! A high-quality tile with a palette (see [`crate::tile_palettes`]) keeps its full resolution
//! as one byte a texel: palette indices, which a GPU pass finds from the decoded colours, plus
//! the RGBA mip levels from 2 down. The tile shader takes level 1 from the indices as the mip
//! chain would make it. That is the same terrain at every zoom in under a third of the memory
//! of a full RGBA chain, which other tiles (the low-quality placeholders) keep. A tile keeps
//! its texture until a better one has been built.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use web_sys::{ImageBitmap, ImageBitmapOptions, PremultiplyAlpha};

use super::{full_mip_level_count, mipmaps, upload_external_image};
use crate::tile_palettes::{self, PALETTE_LEN};
use crate::tiles::{LoadedTile, TileQuality};

/// Decodes in flight at once. Each holds a decoded tile, up to 8 MB, until it is uploaded.
const MAX_DECODES: usize = 4;

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

pub(super) struct Terrain {
    bind_group_layout: wgpu::BindGroupLayout,
    palette_bind_group_layout: wgpu::BindGroupLayout,
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
                entries: &[texture_entry(0), texture_entry(1)],
            });
        let index_pipeline = index_pipeline(device, &index_bind_group_layout);
        Self {
            bind_group_layout,
            palette_bind_group_layout,
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

    pub(super) fn texture(&self, tile_id: usize) -> Option<&TileTexture> {
        self.textures.get(&tile_id)
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
            if self.textures.len() != before {
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
            let have = self.textures.get(&tile.id).map(|texture| texture.quality);
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
            if self
                .textures
                .get(&tile.id)
                .is_none_or(|texture| texture.quality < quality)
            {
                let texture = self.build(device, queue, gl, tile, quality, &bitmap);
                self.textures.insert(tile.id, texture);
                self.revision = self.revision.wrapping_add(1);
            }
            bitmap.close();
        }
        waiting || !self.decodes.is_empty()
    }

    fn build(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        gl: &web_sys::WebGl2RenderingContext,
        tile: &LoadedTile,
        quality: TileQuality,
        bitmap: &ImageBitmap,
    ) -> TileTexture {
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
        let Some(palette) = palette else {
            return self.rgba_tile(device, rect, quality, &full);
        };
        let palette = palette_texture(device, queue, palette);
        let index = self.index_texture(device, queue, &full, &palette);
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
        TileTexture {
            bind_group,
            palette: true,
            rect,
            quality,
        }
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

    /// The palette index of every texel of `full`'s level 0.
    fn index_texture(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        full: &wgpu::Texture,
        palette: &wgpu::Texture,
    ) -> wgpu::Texture {
        let size = full.size();
        let index = create_texture(
            device,
            "tile-index-tex",
            wgpu::TextureFormat::R8Unorm,
            (size.width, size.height),
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let source = full.create_view(&wgpu::TextureViewDescriptor {
            label: Some("tile-index-source"),
            base_mip_level: 0,
            mip_level_count: Some(1),
            ..Default::default()
        });
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
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tile-index-encoder"),
        });
        {
            let target = view(&index);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tile-index-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.index_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));
        index
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

/// Starts the browser decoding `tile` into a premultiplied bitmap.
fn decode(tile: &LoadedTile) -> DecodeSlot {
    let quality = tile.quality;
    let slot = Rc::new(RefCell::new(Decode::Pending));
    let options = ImageBitmapOptions::new();
    options.set_premultiply_alpha(PremultiplyAlpha::Premultiply);
    let promise = web_sys::window().and_then(|window| {
        window
            .create_image_bitmap_with_blob_and_image_bitmap_options(&tile.image, &options)
            .ok()
    });
    let Some(promise) = promise else {
        *slot.borrow_mut() = Decode::Failed(quality);
        return slot;
    };
    let target = Rc::downgrade(&slot);
    wasm_bindgen_futures::spawn_local(async move {
        let decoded = wasm_bindgen_futures::JsFuture::from(promise)
            .await
            .ok()
            .and_then(|bitmap| bitmap.dyn_into::<ImageBitmap>().ok());
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
