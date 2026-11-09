//! The terrain tiles on the GPU. The browser decodes each tile off the main thread into a
//! premultiplied bitmap, which is uploaded as it is, at most one tile a frame: a tile arriving
//! never stalls a frame with a decode, a read-back through a 2D canvas or an 8 MB copy. A tile
//! keeps its texture until a better one has been uploaded, and the bitmap is released once it
//! has been.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use web_sys::{ImageBitmap, ImageBitmapOptions, PremultiplyAlpha};

use super::{mipmaps, upload_external_image};
use crate::tiles::{LoadedTile, TileQuality};

/// A tile's texture, ready to draw.
pub(super) struct TileTexture {
    pub(super) bind_group: wgpu::BindGroup,
    /// World x, z, width and height.
    pub(super) rect: [f32; 4],
    quality: TileQuality,
}

/// Where a tile's decode stands.
enum Decode {
    Pending(TileQuality),
    Ready(TileQuality, ImageBitmap),
    /// The browser could not decode it; not retried at this quality.
    Failed(TileQuality),
}

impl Decode {
    fn quality(&self) -> TileQuality {
        match *self {
            Decode::Pending(quality) | Decode::Ready(quality, _) | Decode::Failed(quality) => {
                quality
            }
        }
    }
}

type DecodeSlot = Rc<RefCell<Decode>>;

pub(super) struct Terrain {
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    mips: mipmaps::MipChain,
    textures: HashMap<usize, TileTexture>,
    decodes: HashMap<usize, DecodeSlot>,
    /// The tile set last synced, by id and quality.
    signature: Option<u64>,
    revision: u64,
}

impl Terrain {
    pub(super) fn new(
        bind_group_layout: wgpu::BindGroupLayout,
        sampler: wgpu::Sampler,
        mips: mipmaps::MipChain,
    ) -> Self {
        Self {
            bind_group_layout,
            sampler,
            mips,
            textures: HashMap::new(),
            decodes: HashMap::new(),
            signature: None,
            revision: 0,
        }
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
    /// better ones and uploads one decoded tile. Returns whether decodes are still to land.
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
            if self.textures.len() != before {
                self.revision = self.revision.wrapping_add(1);
            }
            for tile in tiles {
                let uploaded = self.textures.get(&tile.id).map(|texture| texture.quality);
                let decoding = self
                    .decodes
                    .get(&tile.id)
                    .map(|decode| decode.borrow().quality());
                if uploaded
                    .max(decoding)
                    .is_none_or(|have| have < tile.quality)
                    && let Some(replaced) = self.decodes.insert(tile.id, decode(tile))
                {
                    release(&replaced);
                }
            }
        }

        let ready = self
            .decodes
            .iter()
            .find_map(|(id, slot)| matches!(*slot.borrow(), Decode::Ready(..)).then_some(*id));
        if let Some(id) = ready
            && let Some(slot) = self.decodes.remove(&id)
            && let Decode::Ready(quality, bitmap) = slot.replace(Decode::Pending(TileQuality::Low))
        {
            if let Some(tile) = tiles.iter().find(|tile| tile.id == id) {
                self.upload(device, queue, gl, tile, quality, &bitmap);
                self.revision = self.revision.wrapping_add(1);
            }
            bitmap.close();
        }
        self.decodes
            .values()
            .any(|slot| !matches!(*slot.borrow(), Decode::Failed(_)))
    }

    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        gl: &web_sys::WebGl2RenderingContext,
        tile: &LoadedTile,
        quality: TileQuality,
        bitmap: &ImageBitmap,
    ) {
        let size = wgpu::Extent3d {
            width: bitmap.width().max(1),
            height: bitmap.height().max(1),
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tile-tex"),
            size,
            mip_level_count: super::full_mip_level_count(size.width, size.height),
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            // External image copies, and drawing the mip levels, need a render-attachment
            // destination.
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        // The bitmap was premultiplied when decoded, so the mip levels average colour and
        // coverage together. WebGL uploads a bitmap as it was decoded, and wgpu accepts a
        // bitmap only described as not premultiplied on upload.
        upload_external_image(
            queue,
            &texture,
            wgpu::ExternalImageSource::ImageBitmap(bitmap.clone()),
            size,
            gl,
            false,
        );
        self.mips.generate(device, queue, &texture);

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // Tile bounds are inclusive.
        let x1 = tile.x1.min(tile.x2) as f32;
        let z1 = tile.z1.min(tile.z2) as f32;
        let width = (tile.x1.max(tile.x2) - tile.x1.min(tile.x2) + 1) as f32;
        let height = (tile.z1.max(tile.z2) - tile.z1.min(tile.z2) + 1) as f32;
        let rect = [x1, z1, width, height];
        let rect_buffer = wgpu::util::DeviceExt::create_buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("tile-rect-ubo"),
                contents: bytemuck::cast_slice(&rect),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
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
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.textures.insert(
            tile.id,
            TileTexture {
                bind_group,
                rect,
                quality,
            },
        );
    }
}

/// Frees a decoded bitmap that will not be uploaded; false, for `retain`.
fn release(slot: &DecodeSlot) -> bool {
    if let Decode::Ready(_, bitmap) = &*slot.borrow() {
        bitmap.close();
    }
    false
}

/// Starts the browser decoding `tile` into a premultiplied bitmap.
fn decode(tile: &LoadedTile) -> DecodeSlot {
    let quality = tile.quality;
    let slot = Rc::new(RefCell::new(Decode::Pending(quality)));
    let options = ImageBitmapOptions::new();
    options.set_premultiply_alpha(PremultiplyAlpha::Premultiply);
    let promise = web_sys::window().and_then(|window| {
        window
            .create_image_bitmap_with_html_image_element_and_image_bitmap_options(
                &tile.image,
                &options,
            )
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
