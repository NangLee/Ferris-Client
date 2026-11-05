//! An offscreen drawing backend for the public Ferris scene format.
//! Geometry is already decided by the server; this module only draws it.

use anyhow::{Context, Result, ensure};
use bytemuck::{Pod, Zeroable};
use ferris_scene::{BlendMode, DrawCommand, Frame, Sprite};
use std::{collections::HashMap, sync::mpsc};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
}

struct Texture {
    _texture: wgpu::Texture,
    binding: wgpu::BindGroup,
}
struct Draw {
    texture: Option<u32>,
    blend: BlendMode,
    first: u32,
    count: u32,
}

pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    alpha: wgpu::RenderPipeline,
    additive: wgpu::RenderPipeline,
    copy: wgpu::RenderPipeline,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    readback: wgpu::Buffer,
    white: Texture,
    textures: HashMap<u32, Texture>,
    width: u32,
    height: u32,
    stride: u32,
    pixels: Vec<u8>,
}

impl GpuRenderer {
    pub async fn new(width: u32, height: u32) -> Result<Self> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .context("No graphics adapter is available")?;
        let adapter_info = adapter.get_info();
        ensure!(
            matches!(
                adapter_info.device_type,
                wgpu::DeviceType::IntegratedGpu | wgpu::DeviceType::DiscreteGpu
            ),
            "GPU mode selected a software or virtual adapter: {} ({:?})",
            adapter_info.name,
            adapter_info.device_type
        );
        let mut limits = wgpu::Limits::default();
        limits.max_texture_dimension_2d = 8192;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Ferris drawing device"),
                required_limits: limits,
                ..Default::default()
            })
            .await
            .context("Could not create the graphics device")?;
        eprintln!(
            "Graphics adapter: {} ({:?}, {:?})",
            adapter_info.name, adapter_info.device_type, adapter_info.backend
        );
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Ferris texture layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
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
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Ferris nearest sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Ferris drawing shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Ferris drawing layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let alpha = pipeline(
            &device,
            &pipeline_layout,
            &shader,
            Some(wgpu::BlendState::ALPHA_BLENDING),
        );
        let additive = pipeline(
            &device,
            &pipeline_layout,
            &shader,
            Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::SrcAlpha,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
            }),
        );
        let copy = pipeline(&device, &pipeline_layout, &shader, None);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Ferris drawing canvas"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let stride = (width * 4).div_ceil(256) * 256;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Ferris frame readback"),
            size: u64::from(stride) * u64::from(height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let white = texture(&device, &queue, &layout, &sampler, 1, 1, &[255; 4]);
        Ok(Self {
            device,
            queue,
            layout,
            sampler,
            alpha,
            additive,
            copy,
            target,
            view,
            readback,
            white,
            textures: HashMap::new(),
            width,
            height,
            stride,
            pixels: vec![0; width as usize * height as usize * 4],
        })
    }

    pub fn texture(&mut self, id: u32, width: u32, height: u32, rgba: &[u8]) {
        let texture = texture(
            &self.device,
            &self.queue,
            &self.layout,
            &self.sampler,
            width,
            height,
            rgba,
        );
        self.textures.insert(id, texture);
    }

    pub fn drop_texture(&mut self, id: u32) {
        self.textures.remove(&id);
    }

    pub fn draw(&mut self, frame: &Frame) -> Result<&[u8]> {
        let mut vertices = Vec::<Vertex>::new();
        let mut draws = Vec::<Draw>::new();
        for command in &frame.commands {
            let first = vertices.len() as u32;
            let (texture, blend) = match command {
                DrawCommand::Sprites {
                    texture,
                    blend,
                    sprites,
                } => {
                    ensure!(
                        self.textures.contains_key(texture),
                        "Frame refers to an unknown texture"
                    );
                    for sprite in sprites {
                        add_sprite(&mut vertices, sprite, self.width, self.height);
                    }
                    (Some(*texture), *blend)
                }
                DrawCommand::MaskOutsideCircle {
                    center,
                    radius,
                    color,
                } => {
                    add_mask(
                        &mut vertices,
                        *center,
                        *radius,
                        *color,
                        self.width,
                        self.height,
                    );
                    (None, BlendMode::Copy)
                }
            };
            let count = vertices.len() as u32 - first;
            if count != 0 {
                draws.push(Draw {
                    texture,
                    blend,
                    first,
                    count,
                });
            }
            ensure!(
                vertices.len() * std::mem::size_of::<Vertex>()
                    <= self.device.limits().max_buffer_size.min(256 * 1024 * 1024) as usize,
                "Frame exceeds graphics buffer limits"
            );
        }
        let buffer = (!vertices.is_empty()).then(|| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Ferris frame geometry"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                })
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Ferris frame commands"),
            });
        {
            let color = frame.clear_color;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Ferris frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: color[0] as f64,
                            g: color[1] as f64,
                            b: color[2] as f64,
                            a: color[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Some(buffer) = &buffer {
                pass.set_vertex_buffer(0, buffer.slice(..));
                for draw in &draws {
                    let texture = match draw.texture {
                        Some(id) => &self.textures[&id],
                        None => &self.white,
                    };
                    pass.set_pipeline(match draw.blend {
                        BlendMode::Alpha => &self.alpha,
                        BlendMode::Additive => &self.additive,
                        BlendMode::Copy => &self.copy,
                    });
                    pass.set_bind_group(0, &texture.binding, &[]);
                    pass.draw(draw.first..draw.first + draw.count, 0..1);
                }
            }
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.stride),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let (send, receive) = mpsc::sync_channel(1);
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = send.send(result);
            });
        self.device
            .poll(wgpu::PollType::Wait)
            .context("Graphics readback failed")?;
        receive
            .recv_timeout(std::time::Duration::from_secs(60))
            .context("Graphics readback timed out")?
            .context("Could not read the graphics frame")?;
        {
            let mapped = self.readback.slice(..).get_mapped_range();
            let row_bytes = self.width as usize * 4;
            for row in 0..self.height as usize {
                self.pixels[row * row_bytes..(row + 1) * row_bytes].copy_from_slice(
                    &mapped[row * self.stride as usize..row * self.stride as usize + row_bytes],
                );
            }
        }
        self.readback.unmap();
        if frame.output_alpha < 1.0 {
            for pixel in self.pixels.chunks_exact_mut(4) {
                pixel[3] = (f32::from(pixel[3]) * frame.output_alpha).round() as u8;
            }
        }
        Ok(&self.pixels)
    }
}

fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("Ferris sprite pipeline"), layout: Some(layout), vertex: wgpu::VertexState { module: shader, entry_point: Some("vertex_main"), compilation_options: Default::default(), buffers: &[wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Vertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4] }] }, fragment: Some(wgpu::FragmentState { module: shader, entry_point: Some("fragment_main"), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: wgpu::TextureFormat::Rgba8Unorm, blend, write_mask: wgpu::ColorWrites::ALL })] }), primitive: wgpu::PrimitiveState::default(), depth_stencil: None, multisample: wgpu::MultisampleState::default(), multiview: None, cache: None,
    })
}

fn texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Ferris scene texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
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
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&Default::default());
    let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Ferris scene texture binding"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    Texture {
        _texture: texture,
        binding,
    }
}

fn add_sprite(vertices: &mut Vec<Vertex>, sprite: &Sprite, width: u32, height: u32) {
    if sprite.size[0] <= 0.0 || sprite.size[1] <= 0.0 {
        return;
    }
    let (sin, cos) = sprite.rotation.sin_cos();
    for point in [
        [0.0, 0.0],
        [1.0, 0.0],
        [0.0, 1.0],
        [1.0, 0.0],
        [1.0, 1.0],
        [0.0, 1.0],
    ] {
        let x = point[0] * sprite.size[0] - sprite.origin[0];
        let y = point[1] * sprite.size[1] - sprite.origin[1];
        let pixel_x = sprite.position[0] + sprite.origin[0] + x * cos - y * sin;
        let pixel_y = sprite.position[1] + sprite.origin[1] + x * sin + y * cos;
        vertices.push(Vertex {
            position: [
                pixel_x / width as f32 * 2.0 - 1.0,
                1.0 - pixel_y / height as f32 * 2.0,
            ],
            uv: [
                sprite.uv_rect[0] + (sprite.uv_rect[2] - sprite.uv_rect[0]) * point[0],
                sprite.uv_rect[1] + (sprite.uv_rect[3] - sprite.uv_rect[1]) * point[1],
            ],
            color: sprite.color,
        });
    }
}

fn add_mask(
    vertices: &mut Vec<Vertex>,
    center: [f32; 2],
    radius: f32,
    color: [f32; 4],
    width: u32,
    height: u32,
) {
    for y in 0..height {
        let dy = y as f32 - center[1];
        let span_squared = radius * radius - dy * dy;
        let (left, right) = if span_squared < 0.0 {
            (width, width)
        } else {
            let span = span_squared.sqrt();
            (
                (center[0] - span).ceil().clamp(0.0, width as f32) as u32,
                ((center[0] + span).floor() + 1.0).clamp(0.0, width as f32) as u32,
            )
        };
        for (x, size) in [(0, left), (right, width - right)] {
            if size == 0 {
                continue;
            }
            add_sprite(
                vertices,
                &Sprite {
                    position: [x as f32, y as f32],
                    size: [size as f32, 1.0],
                    uv_rect: [0.0, 0.0, 1.0, 1.0],
                    color,
                    origin: [0.0, 0.0],
                    rotation: 0.0,
                },
                width,
                height,
            );
        }
    }
}

const SHADER: &str = include_str!("sprite.wgsl");
