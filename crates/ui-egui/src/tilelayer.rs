//! The GPU map's live raster tile layer (#635).
//!
//! A raster tile layer used to be part of each GPU scene: every scene placed its tiles for the
//! camera it was drawn for, so panning and zooming kept drawing new scenes, and each scene made
//! its own buffers and bind groups (which the browser's WebGPU frees only when its garbage
//! collector gets round to it). Here, as MapLibre draws its tiles, the scene only says which
//! layer goes where ([`DrawCmd::LiveTiles`]), and every frame the map shows places the layer's
//! tiles again for that frame's camera (cheap: tile cache lookups) and draws them from textures
//! uploaded once:
//!
//! - **Fixed pools.** Tile pictures are layers of a few texture arrays (one set per tile size),
//!   added a chunk at a time up to a budget and reused least recently used first: nothing is
//!   created or freed while the map moves, and what is freed is freed at once (`destroy`).
//! - **Paced uploads.** At most [`UPLOADS`] new pictures a frame, and for at most
//!   [`UPLOAD_TIME`]: a tile not uploaded yet is
//!   left out for a frame or two (its parent stands in for it once the tile cache has it).
//! - **One draw per chunk.** A layer's tiles are instances: per frame only the instance list
//!   and one uniform are written.
//!
//! [`DrawCmd::LiveTiles`]: rustgis_engine::render::gpu_scene::DrawCmd::LiveTiles

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Weak};

use egui_wgpu::wgpu;
use rustgis_engine::map::Camera;
use rustgis_engine::render::gpu_scene::TileQuad;
use rustgis_engine::render::rustgis_tiles::cache::Payload;

/// Bytes of tile pictures kept on the GPU, over all sizes.
const BUDGET: u64 = if cfg!(target_arch = "wasm32") { 128 << 20 } else { 384 << 20 };
/// Bytes in one chunk of a pool (one texture array): 64 pictures of 256 pixels, 16 of 512.
const CHUNK_BYTES: u64 = 16 << 20;
/// New pictures uploaded in one frame, at most.
const UPLOADS: usize = if cfg!(target_arch = "wasm32") { 8 } else { 32 };
/// And for at most this long (the first is always uploaded): a slow machine uploads fewer a
/// frame rather than missing frames.
const UPLOAD_TIME: web_time::Duration = web_time::Duration::from_millis(if cfg!(target_arch = "wasm32") { 4 } else { 6 });
/// The largest tile picture placed (a side, in pixels).
const MAX_SIDE: u32 = 2048;

/// One tile drawn (WGSL `LiveIn`).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    rect: [f32; 4],
    uv: [f32; 4],
    layer: u32,
    opacity: f32,
}

/// What a live layer draws in one frame: per chunk and sampling (nearest or not), its instances.
#[derive(Default, Clone)]
struct Runs(Vec<(usize, bool, Range<u32>)>);

/// One texture array of a pool.
struct Chunk {
    texture: wgpu::Texture,
    bind: wgpu::BindGroup,
    /// The same, sampled nearest: a layer set to nearest-neighbour resampling (#2804).
    nearest_bind: wgpu::BindGroup,
    #[cfg_attr(not(test), expect(dead_code, reason = "read by the tests only"))]
    layers: u32,
    /// Its pictures' side, and its bytes.
    side: u32,
    bytes: u64,
}

/// Where a picture is: its chunk (index into all chunks) and layer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct At {
    chunk: usize,
    layer: u32,
}

struct Slot {
    /// Held weakly: the tile cache keeps the picture alive; once it lets go the slot is free
    /// (and the address can't be reused for another picture until then).
    picture: Weak<Payload>,
    at: At,
    used: u64,
}

/// The pictures of one size.
#[derive(Default)]
struct Pool {
    by_picture: HashMap<usize, Slot>,
    free: Vec<At>,
    chunks: Vec<usize>,
}

/// The live tile layers' textures, pipeline and per-frame instances, kept by the GPU map renderer.
/// One map view places live layers per frame: the instances and uniform are written in its
/// paint callback's `prepare` and read by its scene pass, so a second view placing live layers in
/// the same frame would need its own (the split view's scenes have none).
pub(crate) struct LiveTiles {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    nearest_sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    /// Every chunk, by the index an [`At`] names; `None` once given back to the budget.
    chunks: Vec<Option<Chunk>>,
    pools: HashMap<u32, Pool>,
    instances: Option<(wgpu::Buffer, u64)>,
    /// What each live layer of the frame draws, by its place among the scene's live layers.
    runs: Vec<Runs>,
    frame: u64,
    bytes: u64,
    budget: u64,
    max_layers: u32,
    max_side: u32,
}

/// The uploads of one frame so far.
struct Uploads {
    n: usize,
    started: web_time::Instant,
}

/// What [`LiveTiles::update`] left for later frames.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Backlog {
    /// Pictures waiting to be uploaded: draw again next frame.
    pub uploads: bool,
}

impl LiveTiles {
    /// The pipeline, drawing into the scene's MSAA target (`samples`, `format`) with its depth
    /// and stencil attachment left alone.
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat, samples: u32, depth: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("live_tiles"),
            source: wgpu::ShaderSource::Wgsl(include_str!("tilelayer.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("live_tiles"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("live_tiles"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let attributes = [
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 16, shader_location: 1 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32, offset: 32, shader_location: 2 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32, offset: 36, shader_location: 3 },
        ];
        let buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Instance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &attributes,
        })];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("live_tiles"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_live"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &buffers,
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_live"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(crate::mapgpu::PREMULTIPLIED_BLEND),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState { count: samples, mask: !0, alpha_to_coverage_enabled: false },
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("live_tiles"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let nearest_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("live_tiles_nearest"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("live_tiles_uniforms"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let limits = device.limits();
        LiveTiles {
            pipeline,
            layout,
            sampler,
            nearest_sampler,
            uniforms,
            chunks: Vec::new(),
            pools: HashMap::new(),
            instances: None,
            runs: Vec::new(),
            frame: 0,
            bytes: 0,
            budget: BUDGET,
            max_layers: limits.max_texture_array_layers,
            max_side: limits.max_texture_dimension_2d.min(MAX_SIDE),
        }
    }

    /// Places the frame's live layers (`layers`, in the scene's order of them, each bottom first,
    /// from `camera`'s centre) for a `size` (logical) view: uploads pictures not on the GPU yet
    /// (at most [`UPLOADS`]), writes the instances and the camera's uniform.
    pub(crate) fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layers: &[Vec<TileQuad>],
        camera: &Camera,
        size: [f64; 2],
    ) -> Backlog {
        self.frame += 1;
        let mut backlog = Backlog::default();
        let mut uploads = Uploads { n: 0, started: web_time::Instant::now() };
        let mut instances: Vec<Instance> = Vec::new();
        self.runs.clear();
        // Every picture of the frame already here is marked drawn first, so making room for a
        // new one never takes one the frame draws further on.
        let frame = self.frame;
        for q in layers.iter().flatten() {
            let Some(pm) = q.pixmap() else { continue };
            if let Some(s) = self.pools.get_mut(&pm.width()).and_then(|p| p.by_picture.get_mut(&(Arc::as_ptr(&q.picture) as usize)))
                && s.picture.upgrade().is_some_and(|p| Arc::ptr_eq(&p, &q.picture))
            {
                s.used = frame;
            }
        }
        for quads in layers {
            let mut by_chunk: Vec<((usize, bool), Vec<Instance>)> = Vec::new();
            for q in quads {
                let Some(at) = self.slot(device, queue, q, &mut uploads, &mut backlog) else { continue };
                let i = Instance { rect: q.rect, uv: q.uv, layer: at.layer, opacity: q.opacity };
                let run = (at.chunk, q.nearest);
                match by_chunk.iter_mut().find(|(c, _)| *c == run) {
                    Some((_, v)) => v.push(i),
                    None => by_chunk.push((run, vec![i])),
                }
            }
            let mut runs = Runs::default();
            for ((chunk, nearest), v) in by_chunk {
                let first = instances.len() as u32;
                instances.extend(v);
                runs.0.push((chunk, nearest, first..instances.len() as u32));
            }
            self.runs.push(runs);
        }
        let res = camera.res.max(1e-12);
        let scale = [(2.0 / (size[0].max(1.0) * res)) as f32, (2.0 / (size[1].max(1.0) * res)) as f32, 0.0, 0.0];
        queue.write_buffer(&self.uniforms, 0, bytemuck::cast_slice(&scale));
        if !instances.is_empty() {
            let bytes = std::mem::size_of_val(instances.as_slice()) as u64;
            if self.instances.as_ref().is_none_or(|(_, cap)| *cap < bytes) {
                let cap = bytes.next_power_of_two().max(4096);
                if let Some((old, _)) = self.instances.take() {
                    old.destroy();
                }
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("live_tiles_instances"),
                    size: cap,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.instances = Some((buffer, cap));
            }
            if let Some((buffer, _)) = &self.instances {
                queue.write_buffer(buffer, 0, bytemuck::cast_slice(&instances));
            }
        }
        backlog
    }

    /// Where `q`'s picture is on the GPU, uploading it when there is room this frame.
    fn slot(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        q: &TileQuad,
        uploads: &mut Uploads,
        backlog: &mut Backlog,
    ) -> Option<At> {
        let pm = q.pixmap()?;
        let side = pm.width();
        if side != pm.height() || side == 0 || side > self.max_side || pm.data().len() as u64 != u64::from(side) * u64::from(side) * 4 {
            return None;
        }
        let key = Arc::as_ptr(&q.picture) as usize;
        let frame = self.frame;
        let pool = self.pools.entry(side).or_default();
        if let Some(s) = pool.by_picture.get_mut(&key)
            && s.picture.upgrade().is_some_and(|p| Arc::ptr_eq(&p, &q.picture))
        {
            s.used = frame;
            return Some(s.at);
        }
        if uploads.n >= UPLOADS || (uploads.n > 0 && uploads.started.elapsed() >= UPLOAD_TIME) {
            backlog.uploads = true;
            return None;
        }
        let at = self.take(device, side)?;
        let size = wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 1 };
        let texture = &self.chunks.get(at.chunk)?.as_ref()?.texture;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: at.layer },
                aspect: wgpu::TextureAspect::All,
            },
            pm.data(),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(side * 4), rows_per_image: Some(side) },
            size,
        );
        uploads.n += 1;
        let pool = self.pools.entry(side).or_default();
        pool.by_picture.insert(key, Slot { picture: Arc::downgrade(&q.picture), at, used: frame });
        Some(at)
    }

    /// A free layer for a picture `side` pixels square: one let go by the tile cache, a new
    /// chunk's while the budget allows, else the least recently used one of this size not drawn
    /// this frame, else one of a new chunk made from the budget of another size's chunk that this
    /// frame doesn't draw from (a layer of another tile size, added after the budget was spent).
    fn take(&mut self, device: &wgpu::Device, side: u32) -> Option<At> {
        let frame = self.frame;
        let pool = self.pools.entry(side).or_default();
        if pool.free.is_empty() {
            let gone: Vec<usize> = pool.by_picture.iter().filter(|(_, s)| s.picture.strong_count() == 0).map(|(k, _)| *k).collect();
            for k in gone {
                if let Some(s) = pool.by_picture.remove(&k) {
                    pool.free.push(s.at);
                }
            }
        }
        if let Some(at) = pool.free.pop() {
            return Some(at);
        }
        if let Some(at) = self.grow(device, side) {
            return Some(at);
        }
        let pool = self.pools.entry(side).or_default();
        let oldest = pool.by_picture.iter().filter(|(_, s)| s.used != frame).min_by_key(|(_, s)| s.used).map(|(k, _)| *k);
        if let Some(at) = oldest.and_then(|k| pool.by_picture.remove(&k)).map(|s| s.at) {
            return Some(at);
        }
        if self.reclaim(side) { self.grow(device, side) } else { None }
    }

    /// A new chunk for pictures `side` pixels square while the budget allows: its first layer.
    fn grow(&mut self, device: &wgpu::Device, side: u32) -> Option<At> {
        let picture = u64::from(side) * u64::from(side) * 4;
        let layers = (CHUNK_BYTES / picture).clamp(1, u64::from(self.max_layers.max(1))) as u32;
        let chunk_bytes = picture * u64::from(layers);
        if self.bytes + chunk_bytes > self.budget {
            return None;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("live_tiles_chunk"),
            size: wgpu::Extent3d { width: side, height: side, depth_or_array_layers: layers },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture
            .create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
        let bind_with = |sampler: &wgpu::Sampler| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("live_tiles_chunk"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.uniforms.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler) },
                ],
            })
        };
        let (bind, nearest_bind) = (bind_with(&self.sampler), bind_with(&self.nearest_sampler));
        let c = Chunk { texture, bind, nearest_bind, layers, side, bytes: chunk_bytes };
        // A slot given back earlier is reused, so the list doesn't grow with every exchange.
        let chunk = match self.chunks.iter().position(Option::is_none) {
            Some(i) => {
                if let Some(slot) = self.chunks.get_mut(i) {
                    *slot = Some(c);
                }
                i
            }
            None => {
                self.chunks.push(Some(c));
                self.chunks.len() - 1
            }
        };
        self.bytes += chunk_bytes;
        let pool = self.pools.entry(side).or_default();
        pool.chunks.push(chunk);
        pool.free.extend((1..layers).rev().map(|layer| At { chunk, layer }));
        Some(At { chunk, layer: 0 })
    }

    /// Gives the budget of another size's chunk back, the one whose pictures were drawn least
    /// recently, so long as this frame draws none of them: whether there was one.
    fn reclaim(&mut self, side: u32) -> bool {
        let frame = self.frame;
        let mut best: Option<(u64, usize)> = None;
        for (s, pool) in &self.pools {
            if *s == side {
                continue;
            }
            for &c in &pool.chunks {
                let used = pool.by_picture.values().filter(|p| p.at.chunk == c).map(|p| p.used).max().unwrap_or(0);
                if used != frame && best.is_none_or(|(u, _)| used < u) {
                    best = Some((used, c));
                }
            }
        }
        let Some((_, c)) = best else { return false };
        let Some(chunk) = self.chunks.get_mut(c).and_then(Option::take) else { return false };
        if let Some(pool) = self.pools.get_mut(&chunk.side) {
            pool.by_picture.retain(|_, p| p.at.chunk != c);
            pool.free.retain(|a| a.chunk != c);
            pool.chunks.retain(|&x| x != c);
        }
        // Not drawn this frame; work already submitted that reads it still completes (WebGPU and
        // wgpu free it only after).
        chunk.texture.destroy();
        self.bytes = self.bytes.saturating_sub(chunk.bytes);
        true
    }

    /// Draws the `n`th live layer of the frame placed last ([`update`](Self::update)).
    pub(crate) fn paint(&self, n: usize, rp: &mut wgpu::RenderPass<'_>) {
        let (Some(runs), Some((instances, _))) = (self.runs.get(n), &self.instances) else { return };
        if runs.0.is_empty() {
            return;
        }
        rp.set_pipeline(&self.pipeline);
        rp.set_vertex_buffer(0, instances.slice(..));
        for (chunk, nearest, range) in &runs.0 {
            if let Some(Some(c)) = self.chunks.get(*chunk) {
                rp.set_bind_group(0, if *nearest { &c.nearest_bind } else { &c.bind }, &[]);
                rp.draw(0..6, range.clone());
            }
        }
    }

    /// Keeps at most `bytes` of pictures (a test's small budget).
    #[cfg(test)]
    pub(crate) fn with_budget(mut self, bytes: u64) -> Self {
        self.budget = bytes;
        self
    }

    /// Bytes of tile pictures on the GPU.
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// How many pictures are on the GPU.
    #[cfg(test)]
    pub(crate) fn pictures(&self) -> usize {
        self.pools.values().map(|p| p.by_picture.len()).sum()
    }

    /// Layers in all chunks.
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> u32 {
        self.chunks.iter().flatten().map(|c| c.layers).sum()
    }
}

impl Drop for LiveTiles {
    fn drop(&mut self) {
        // The browser's WebGPU frees a texture only once its handle is collected: free them now.
        for c in self.chunks.iter().flatten() {
            c.texture.destroy();
        }
        if let Some((b, _)) = &self.instances {
            b.destroy();
        }
        self.uniforms.destroy();
    }
}
