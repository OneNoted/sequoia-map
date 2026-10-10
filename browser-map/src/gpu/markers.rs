//! Map Intel markers: the host's [`MapMarkers`] as one instance buffer, drawn over the map
//! at a constant screen size (`marker.wgsl`).

use std::ops::Range;
use std::sync::Arc;

use sequoia_map_engine::map_markers::{MapMarker, MapMarkers, marker_style, shows_node_summaries};
use sequoia_map_engine::viewport::Viewport;

/// Screen margin, CSS pixels, within which a node off screen may still show part of itself.
const NODE_CULL_MARGIN_PX: f64 = 8.0;

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct MarkerInstance {
    world: [f32; 2],
    rgba: [u8; 4],
    shape: u32,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct MarkerStyleUniform {
    node: [f32; 4],
    sites: [f32; 4],
    stroke: [f32; 4],
}

pub(super) struct GpuMarkerRenderer {
    pipeline: wgpu::RenderPipeline,
    style_buffer: wgpu::Buffer,
    style_bind_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    capacity: u32,
    /// The marker set in `instance_buffer`.
    uploaded: Option<Arc<MapMarkers>>,
    summaries: Range<u32>,
    sites: Range<u32>,
    /// Instance ranges this frame draws, from `prepare`.
    draw_ranges: Vec<Range<u32>>,
}

impl GpuMarkerRenderer {
    pub(super) fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        viewport_bind_group_layout: &wgpu::BindGroupLayout,
        vertex_layout: &wgpu::VertexBufferLayout<'static>,
    ) -> Self {
        let style_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("marker-style"),
            size: std::mem::size_of::<MarkerStyleUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let style_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("marker-style-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let style_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("marker-style-bg"),
            layout: &style_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: style_buffer.as_entire_binding(),
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("marker-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("marker.wgsl").into()),
        });
        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MarkerInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: 8,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Unorm8x4,
                },
                wgpu::VertexAttribute {
                    offset: 12,
                    shader_location: 3,
                    format: wgpu::VertexFormat::Uint32,
                },
            ],
        };
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("marker-pl"),
            bind_group_layouts: &[viewport_bind_group_layout, &style_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("marker-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[vertex_layout.clone(), instance_layout],
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
        let capacity = 1024;
        Self {
            pipeline,
            style_buffer,
            style_bind_group,
            instance_buffer: Self::allocate(device, capacity),
            capacity,
            uploaded: None,
            summaries: 0..0,
            sites: 0..0,
            draw_ranges: Vec::new(),
        }
    }

    fn allocate(device: &wgpu::Device, capacity: u32) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("marker-instances"),
            size: u64::from(capacity) * std::mem::size_of::<MarkerInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Uploads a marker set the buffer does not hold yet; returns the bytes uploaded.
    pub(super) fn sync(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        markers: Option<&Arc<MapMarkers>>,
    ) -> u64 {
        let unchanged = match (markers, self.uploaded.as_ref()) {
            (Some(next), Some(current)) => Arc::ptr_eq(next, current),
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return 0;
        }
        self.uploaded = markers.cloned();
        let Some(markers) = markers else {
            self.summaries = 0..0;
            self.sites = 0..0;
            return 0;
        };

        // Nodes first, so `MapMarkers::node_ranges_in` indexes the buffer directly.
        let instances: Vec<MarkerInstance> = markers
            .nodes()
            .iter()
            .chain(markers.summaries())
            .chain(markers.sites())
            .map(instance)
            .collect();
        let nodes_end = markers.nodes().len() as u32;
        let summaries_end = nodes_end + markers.summaries().len() as u32;
        self.summaries = nodes_end..summaries_end;
        self.sites = summaries_end..instances.len() as u32;
        if instances.len() as u32 > self.capacity {
            self.capacity = (instances.len() as u32).next_power_of_two();
            self.instance_buffer = Self::allocate(device, self.capacity);
        }
        if instances.is_empty() {
            return 0;
        }
        let bytes: &[u8] = bytemuck::cast_slice(&instances);
        queue.write_buffer(&self.instance_buffer, 0, bytes);
        bytes.len() as u64
    }

    /// Picks this frame's markers (summaries when zoomed out, else the nodes of the cells in
    /// view, then the sites over them) and writes their sizes, before the render pass.
    /// Returns false when nothing shows.
    pub(super) fn prepare(
        &mut self,
        queue: &wgpu::Queue,
        vp: &Viewport,
        css_size: (f64, f64),
        dpr: f32,
    ) -> bool {
        self.draw_ranges.clear();
        let Some(markers) = self.uploaded.as_ref() else {
            return false;
        };
        if shows_node_summaries(vp.scale) {
            self.draw_ranges.push(self.summaries.clone());
        } else {
            let margin = NODE_CULL_MARGIN_PX;
            let (min_x, min_z) = vp.screen_to_world(-margin, -margin);
            let (max_x, max_z) = vp.screen_to_world(css_size.0 + margin, css_size.1 + margin);
            self.draw_ranges.extend(
                markers.node_ranges_in([min_x as f32, min_z as f32], [max_x as f32, max_z as f32]),
            );
        }
        self.draw_ranges.push(self.sites.clone());
        self.draw_ranges.retain(|range| !range.is_empty());
        if self.draw_ranges.is_empty() {
            return false;
        }
        let style = marker_style(vp.scale);
        let uniform = MarkerStyleUniform {
            node: [
                style.node_radius,
                style.simple_node_size,
                style.summary_size,
                1.0 / dpr.max(1.0),
            ],
            sites: [
                style.event_radius,
                style.raid_radius,
                style.camp_radius,
                style.site_stroke_width,
            ],
            stroke: style.site_stroke_rgba,
        };
        queue.write_buffer(&self.style_buffer, 0, bytemuck::bytes_of(&uniform));
        true
    }

    /// Draws the markers; returns the draw calls made.
    pub(super) fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        viewport_bind_group: &wgpu::BindGroup,
        quad_vertices: &wgpu::Buffer,
        quad_indices: &wgpu::Buffer,
    ) -> u32 {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, viewport_bind_group, &[]);
        pass.set_bind_group(1, &self.style_bind_group, &[]);
        pass.set_vertex_buffer(0, quad_vertices.slice(..));
        pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
        pass.set_index_buffer(quad_indices.slice(..), wgpu::IndexFormat::Uint16);
        for range in &self.draw_ranges {
            pass.draw_indexed(0..6, 0, range.clone());
        }
        self.draw_ranges.len() as u32
    }
}

fn instance(marker: &MapMarker) -> MarkerInstance {
    let [r, g, b] = marker.rgb;
    MarkerInstance {
        world: marker.world,
        rgba: [r, g, b, 255],
        shape: marker.shape.code(),
    }
}
