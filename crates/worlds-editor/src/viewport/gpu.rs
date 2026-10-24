//! GPU side of the map renderer.
//!
//! [`ViewportGpu`] lives in egui-wgpu's `callback_resources`, so the paint
//! callback can reach it during egui's render pass. Everything that writes
//! (texture uploads, chunk buffer uploads, the uniform buffer, the draw
//! list) happens on the UI thread before the frame is rendered, through
//! [`Gpu::renderer`]. The paint callback ([`ViewportCallback`]) only binds
//! and draws, so it allocates nothing.
//!
//! The chunk buffers and textures are camera-independent and shared by
//! every map tab. What is per tab (its camera uniforms, its draw list and
//! where it sits on screen) is a [`Slot`]: the n-th map drawn in a frame
//! fills slot n, and its paint callback draws slot n.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use egui::mutex::RwLock;
use egui_wgpu::wgpu;

use super::scene::{Chunk, Instance, TEX_MISSING, TEX_WHITE};

/// Device, queue and the egui renderer, shared by the window and the
/// headless screenshot path.
#[derive(Clone)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub renderer: Arc<RwLock<egui_wgpu::Renderer>>,
    pub format: wgpu::TextureFormat,
}

impl Gpu {
    pub fn from_render_state(rs: &egui_wgpu::RenderState) -> Self {
        Gpu {
            device: rs.device.clone(),
            queue: rs.queue.clone(),
            renderer: rs.renderer.clone(),
            format: rs.target_format,
        }
    }

    /// Put a fresh [`ViewportGpu`] into the renderer's callback resources
    /// (replacing any old one).
    pub fn install_viewport(&self) {
        let vg = ViewportGpu::new(&self.device, &self.queue, self.format);
        self.renderer.write().callback_resources.insert(vg);
    }
}

/// Number of project layers the opacity table holds.
pub const MAX_LAYERS: usize = 64;

/// Texture slots of reference images start here (tilesets take the slots
/// from 2 up; see `scene::tileset_tex`).
pub const REF_TEX_BASE: u32 = 4096;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Uniforms {
    /// xy: snapped camera offset (physical px), z: zoom.
    pub view: [f32; 4],
    /// xy: viewport size in physical px.
    pub size: [f32; 4],
    pub opacity: [[f32; 4]; MAX_LAYERS / 4],
}

impl Uniforms {
    pub fn set_opacity(&mut self, layer: usize, op: f32) {
        if layer < MAX_LAYERS {
            self.opacity[layer / 4][layer % 4] = op;
        }
    }
}

/// One instanced draw: `start..end` of chunk buffer `chunk` with texture `tex`.
#[derive(Debug, Clone, Copy)]
pub struct DrawCmd {
    pub chunk: u32,
    pub tex: u32,
    pub start: u32,
    pub end: u32,
}

struct GpuTex {
    bind_group: wgpu::BindGroup,
    size: [u32; 2],
}

struct GpuChunk {
    buffer: wgpu::Buffer,
    capacity: u32,
    version: u64,
}

/// One map tab's part of a frame: its uniforms, draws and screen rect.
pub struct Slot {
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    /// This frame's draws, rebuilt in place every frame.
    pub draws: Vec<DrawCmd>,
    /// Viewport in physical px: left, top, width, height.
    pub viewport_px: [f32; 4],
    /// Reference images' quads for this tab's world and view, drawn before
    /// every chunk (under the layers): one buffer rewritten each frame
    /// ([`ViewportGpu::set_refs`]).
    refs: Option<GpuChunk>,
    /// This frame's reference image draws (`chunk` unused).
    pub ref_draws: Vec<DrawCmd>,
}

pub struct ViewportGpu {
    pipeline: wgpu::RenderPipeline,
    uniform_layout: wgpu::BindGroupLayout,
    /// Per map tab drawn this frame, in drawing order (see the module docs).
    slots: Vec<Slot>,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// By texture slot (see `scene::tileset_tex`); `None` draws the missing texture.
    textures: Vec<Option<GpuTex>>,
    /// By chunk id.
    chunks: Vec<Option<GpuChunk>>,
    /// Instances uploaded since the last [`Self::take_upload_count`].
    uploaded: usize,
    /// Reference image textures by image path ([`super::references`]).
    pub ref_images: super::references::RefImages,
}

impl ViewportGpu {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("worlds tiles"),
            source: wgpu::ShaderSource::Wgsl(include_str!("tiles.wgsl").into()),
        });
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("worlds viewport uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("worlds tileset"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("worlds tiles"),
            bind_group_layouts: &[Some(&uniform_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("worlds tiles"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2,
                        1 => Float32x2,
                        2 => Float32x4,
                        3 => Unorm8x4,
                        4 => Uint32,
                    ],
                })],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("worlds nearest"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let mut vg = ViewportGpu {
            pipeline,
            uniform_layout,
            slots: Vec::new(),
            texture_layout,
            sampler,
            textures: Vec::new(),
            chunks: Vec::new(),
            uploaded: 0,
            ref_images: Default::default(),
        };
        vg.slot(device, 0);
        vg.set_texture(device, queue, TEX_WHITE, 1, 1, &[255, 255, 255, 255]);
        #[rustfmt::skip]
        let missing = [
            255, 0, 255, 255,   0, 0, 0, 255,
            0, 0, 0, 255,       255, 0, 255, 255,
        ];
        vg.set_texture(device, queue, TEX_MISSING, 2, 2, &missing);
        vg
    }

    /// Upload (or replace) the RGBA8 texture in slot `slot`.
    pub fn set_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slot: u32,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("worlds tileset"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Not sRGB: like egui, we blend in gamma space.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("worlds tileset"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let slot = slot as usize;
        if self.textures.len() <= slot {
            self.textures.resize_with(slot + 1, || None);
        }
        self.textures[slot] = Some(GpuTex {
            bind_group,
            size: [width, height],
        });
    }

    /// Forget a texture slot (it draws as missing).
    pub fn clear_texture(&mut self, slot: u32) {
        if let Some(t) = self.textures.get_mut(slot as usize) {
            *t = None;
        }
    }

    pub fn texture_size(&self, slot: u32) -> Option<[u32; 2]> {
        self.textures
            .get(slot as usize)
            .and_then(|t| t.as_ref())
            .map(|t| t.size)
    }

    /// Slot `i` (made on first use), for one map tab's frame.
    pub fn slot(&mut self, device: &wgpu::Device, i: usize) -> &mut Slot {
        while self.slots.len() <= i {
            let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("worlds viewport uniforms"),
                size: std::mem::size_of::<Uniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("worlds viewport uniforms"),
                layout: &self.uniform_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                }],
            });
            self.slots.push(Slot {
                uniform_buffer,
                uniform_bind_group,
                draws: Vec::with_capacity(1024),
                viewport_px: [0.0; 4],
                refs: None,
                ref_draws: Vec::new(),
            });
        }
        &mut self.slots[i]
    }

    /// Drop every chunk buffer (the scene was rebuilt with new ids).
    pub fn clear_chunks(&mut self) {
        self.chunks.clear();
        for s in &mut self.slots {
            s.draws.clear();
        }
    }

    /// Drop one chunk's buffer (its id was retired).
    pub fn free_chunk(&mut self, id: u32) {
        if let Some(c) = self.chunks.get_mut(id as usize) {
            *c = None;
        }
    }

    /// Make sure chunk `c`'s buffer holds its current instances.
    pub fn sync_chunk(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, c: &Chunk) {
        let id = c.id as usize;
        if self.chunks.len() <= id {
            self.chunks.resize_with(id + 1, || None);
        }
        if self.chunks[id]
            .as_ref()
            .is_some_and(|g| g.version == c.version)
        {
            return;
        }
        let n = c.instances.len() as u32;
        let bytes: &[u8] = bytemuck::cast_slice(&c.instances);
        let fits = self.chunks[id].as_ref().is_some_and(|g| g.capacity >= n);
        if !fits {
            let capacity = n.next_power_of_two().max(16);
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("worlds chunk"),
                size: capacity as u64 * std::mem::size_of::<Instance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.chunks[id] = Some(GpuChunk {
                buffer,
                capacity,
                version: 0,
            });
        }
        let g = self.chunks[id].as_mut().expect("just made");
        if !bytes.is_empty() {
            queue.write_buffer(&g.buffer, 0, bytes);
        }
        g.version = c.version;
        self.uploaded += c.instances.len();
    }

    /// Upload this frame's reference image quads for slot `slot` (drawn
    /// with its [`Slot::ref_draws`]).
    pub fn set_refs(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slot: usize,
        inst: &[Instance],
    ) {
        let n = inst.len() as u32;
        if n == 0 {
            return;
        }
        let s = self.slot(device, slot);
        if s.refs.as_ref().is_none_or(|g| g.capacity < n) {
            let capacity = n.next_power_of_two().max(16);
            s.refs = Some(GpuChunk {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("worlds references"),
                    size: capacity as u64 * std::mem::size_of::<Instance>() as u64,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                capacity,
                version: 0,
            });
        }
        let g = s.refs.as_ref().expect("just made");
        queue.write_buffer(&g.buffer, 0, bytemuck::cast_slice(inst));
    }

    pub fn take_upload_count(&mut self) -> usize {
        std::mem::take(&mut self.uploaded)
    }

    /// Write slot `slot`'s uniforms (the slot exists: see [`Self::slot`]).
    pub fn write_uniforms(&self, queue: &wgpu::Queue, slot: usize, u: &Uniforms) {
        if let Some(s) = self.slots.get(slot) {
            queue.write_buffer(&s.uniform_buffer, 0, bytemuck::bytes_of(u));
        }
    }

    fn paint(&self, slot: usize, pass: &mut wgpu::RenderPass<'static>) {
        let Some(s) = self.slots.get(slot) else {
            return;
        };
        if s.draws.is_empty() && s.ref_draws.is_empty() {
            return;
        }
        let [x, y, w, h] = s.viewport_px;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        pass.set_viewport(x, y, w, h, 0.0, 1.0);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &s.uniform_bind_group, &[]);
        let missing = &self.textures[TEX_MISSING as usize]
            .as_ref()
            .expect("missing texture")
            .bind_group;
        let mut bound_tex = u32::MAX;
        let mut bound_chunk = u32::MAX;
        if let Some(refs) = s.refs.as_ref().filter(|_| !s.ref_draws.is_empty()) {
            pass.set_vertex_buffer(0, refs.buffer.slice(..));
            for d in &s.ref_draws {
                if d.tex != bound_tex {
                    let bg = self
                        .textures
                        .get(d.tex as usize)
                        .and_then(|t| t.as_ref())
                        .map_or(missing, |t| &t.bind_group);
                    pass.set_bind_group(1, bg, &[]);
                    bound_tex = d.tex;
                }
                pass.draw(0..4, d.start..d.end);
            }
        }
        for d in &s.draws {
            let Some(Some(chunk)) = self.chunks.get(d.chunk as usize) else {
                continue;
            };
            if d.tex != bound_tex {
                let bg = self
                    .textures
                    .get(d.tex as usize)
                    .and_then(|t| t.as_ref())
                    .map_or(missing, |t| &t.bind_group);
                pass.set_bind_group(1, bg, &[]);
                bound_tex = d.tex;
            }
            if d.chunk != bound_chunk {
                pass.set_vertex_buffer(0, chunk.buffer.slice(..));
                bound_chunk = d.chunk;
            }
            pass.draw(0..4, d.start..d.end);
        }
    }
}

/// The paint callback egui runs inside its render pass. Carries only its
/// [`Slot`]: the draw list and uniforms were prepared on the UI thread.
pub struct ViewportCallback(pub usize);

impl egui_wgpu::CallbackTrait for ViewportCallback {
    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(vg) = resources.get::<ViewportGpu>() {
            vg.paint(self.0, pass);
        }
    }
}
