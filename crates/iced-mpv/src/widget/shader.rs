use crate::Rotation;
use iced::advanced::graphics::Viewport;
use iced::advanced::mouse;
use iced::{Element, Length, Rectangle};
use iced_wgpu::primitive::{Pipeline, Primitive};
use iced_wgpu::wgpu;
use iced_wgpu::wgpu::util::DeviceExt;

const SHADER_WGSL: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) tex_coords: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) tex_coords: vec2<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(input.position, 0.0, 1.0);
    out.tex_coords = input.tex_coords;
    return out;
}

@group(0) @binding(0)
var r_texture: texture_2d<f32>;
@group(0) @binding(1)
var r_sampler: sampler;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(r_texture, r_sampler, in.tex_coords);
}
"#;

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 2],
    tex_coords: [f32; 2],
}

const VERTICES: &[Vertex] = &[
    Vertex {
        position: [-1.0, -1.0],
        tex_coords: [0.0, 1.0],
    },
    Vertex {
        position: [1.0, -1.0],
        tex_coords: [1.0, 1.0],
    },
    Vertex {
        position: [-1.0, 1.0],
        tex_coords: [0.0, 0.0],
    },
    Vertex {
        position: [1.0, 1.0],
        tex_coords: [1.0, 0.0],
    },
];

/// The wgpu pipeline used to blit a video frame texture to the screen.
pub struct VideoPipeline {
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    bind_group_layout: wgpu::BindGroupLayout,
    texture: Option<wgpu::Texture>,
    bind_group: Option<wgpu::BindGroup>,
    width: u32,
    height: u32,
    /// The imported zero-copy texture + the worker slot it came from; cached
    /// across frames so each slot is imported once (until it changes).
    zero_copy_texture: Option<std::sync::Arc<wgpu::Texture>>,
    zero_copy_slot: Option<u32>,
}

impl Pipeline for VideoPipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("video_shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(SHADER_WGSL)),
        });

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("video_quad_vertices"),
            contents: bytemuck::cast_slice(VERTICES),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video_bind_group_layout"),
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

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video_render_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2,
                        1 => Float32x2
                    ],
                }],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            cache: None,
            multiview: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            pipeline,
            vertex_buffer,
            sampler,
            bind_group_layout,
            texture: None,
            bind_group: None,
            width: 0,
            height: 0,
            zero_copy_texture: None,
            zero_copy_slot: None,
        }
    }
}

/// A custom shader primitive for directly rendering an mpv zero-copy [`wgpu::BindGroup`].
#[derive(Debug)]
pub struct MpvShaderPipeline {
    pub bind_group: wgpu::BindGroup,
}

impl Primitive for MpvShaderPipeline {
    type Pipeline = VideoPipeline;

    fn prepare(
        &self,
        _pipeline: &mut Self::Pipeline,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
    }

    fn draw(&self, pipeline: &Self::Pipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        render_pass.set_pipeline(&pipeline.pipeline);
        render_pass.set_vertex_buffer(0, pipeline.vertex_buffer.slice(..));
        render_pass.set_bind_group(0, &self.bind_group, &[]);
        render_pass.draw(0..4, 0..1);
        true
    }
}

/// A single video frame to render: raw RGBA pixels or zero-copy GPU texture plus display geometry.
///
/// The frame is uploaded or bound to the GPU with aspect-ratio fitting and rotation
/// applied via texture coordinates.
#[derive(Debug, Clone)]
pub struct VideoPrimitive {
    pub width: u32,
    pub height: u32,
    pub rotation: Rotation,
    pub rgba: Option<std::sync::Arc<Vec<u8>>>,
    pub zero_copy_texture: Option<std::sync::Arc<wgpu::Texture>>,
    /// A zero-copy GPU frame referenced by a DMA-BUF handle; imported into a
    /// wgpu texture during `prepare` (which has the device). Mutually
    /// exclusive with `rgba`.
    pub zero_copy: Option<crate::state::ZeroCopyFrame>,
}

impl Primitive for VideoPrimitive {
    type Pipeline = VideoPipeline;

    fn prepare(
        &self,
        pipeline: &mut Self::Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        _viewport: &Viewport,
    ) {
        if self.width == 0 || self.height == 0 {
            return;
        }

        let (eff_w, eff_h) = if self.rotation.is_swapped() {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        };

        // Calculate aspect ratios and fit-shrink scales
        let mut sx = 1.0f32;
        let mut sy = 1.0f32;
        if bounds.width > 0.0 && bounds.height > 0.0 {
            let r_video = eff_w as f32 / eff_h as f32;
            let r_bounds = bounds.width / bounds.height;
            if r_video > r_bounds {
                sx = 1.0;
                sy = r_bounds / r_video;
            } else {
                sx = r_video / r_bounds;
                sy = 1.0;
            }
        }

        let (mut t_bl, mut t_br, mut t_tl, mut t_tr) = match self.rotation {
            Rotation::R90 => ([1.0, 1.0], [1.0, 0.0], [0.0, 1.0], [0.0, 0.0]),
            Rotation::R180 => ([1.0, 0.0], [0.0, 0.0], [1.0, 1.0], [0.0, 1.0]),
            Rotation::R270 => ([0.0, 0.0], [0.0, 1.0], [1.0, 0.0], [1.0, 1.0]),
            Rotation::R0 => ([0.0, 1.0], [1.0, 1.0], [0.0, 0.0], [1.0, 0.0]),
        };
        // GL FBOs are bottom-up (row 0 is the image's bottom), while the
        // software path uploads top-down data. Flip V so the zero-copy texture
        // samples right-side up. Composing the flip with the rotation above.
        if self.zero_copy.is_some() || self.zero_copy_texture.is_some() {
            for tc in [&mut t_bl, &mut t_br, &mut t_tl, &mut t_tr] {
                tc[1] = 1.0 - tc[1];
            }
        }

        let vertices = [
            Vertex {
                position: [-sx, -sy],
                tex_coords: t_bl,
            },
            Vertex {
                position: [sx, -sy],
                tex_coords: t_br,
            },
            Vertex {
                position: [-sx, sy],
                tex_coords: t_tl,
            },
            Vertex {
                position: [sx, sy],
                tex_coords: t_tr,
            },
        ];

        queue.write_buffer(&pipeline.vertex_buffer, 0, bytemuck::cast_slice(&vertices));

        // Zero-copy GPU path: import (once per slot) and bind the DMA-BUF
        // texture. The import needs the device, so it happens here in prepare.
        if let Some(zc) = &self.zero_copy {
            if pipeline.zero_copy_slot != Some(zc.slot) {
                // New slot: release the previous imported texture and import
                // the new DMA-BUF. The fd ownership transfers to the import.
                pipeline.zero_copy_texture = None;
                #[cfg(target_os = "linux")]
                {
                    let fd = match zc.handle.clone().into_fd() {
                        Some(fd) => fd,
                        None => return,
                    };
                    let (stride, drm_format) = match &zc.handle {
                        mpv_utils::gl_interop::SharedGpuFrameHandle::DmaBuf(h) => {
                            (h.stride(), mpv_utils::gl_context::DRM_FORMAT_ABGR8888)
                        }
                        _ => return,
                    };
                    // SAFETY: fd is a valid dma-buf produced by the worker.
                    let imported = unsafe {
                        crate::gl_import::import_dmabuf_to_wgpu(
                            device,
                            fd,
                            self.width,
                            self.height,
                            drm_format,
                            stride,
                        )
                    };
                    match imported {
                        Ok(texture) => {
                            pipeline.zero_copy_texture = Some(std::sync::Arc::new(texture));
                            pipeline.zero_copy_slot = Some(zc.slot);
                        }
                        Err(e) => {
                            tracing::error!("zero-copy dma-buf import failed: {e}");
                            return;
                        }
                    }
                }
                #[cfg(not(target_os = "linux"))]
                return;
            }

            if let Some(texture) = &pipeline.zero_copy_texture {
                let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("mpv_zero_copy_bind_group"),
                    layout: &pipeline.bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&texture_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                        },
                    ],
                });
                pipeline.bind_group = Some(bind_group);
                pipeline.width = self.width;
                pipeline.height = self.height;
            }
            return;
        }

        if let Some(zero_copy_tex) = &self.zero_copy_texture {
            let texture_view = zero_copy_tex.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mpv_zero_copy_bind_group"),
                layout: &pipeline.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&texture_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                    },
                ],
            });
            pipeline.bind_group = Some(bind_group);
            pipeline.width = self.width;
            pipeline.height = self.height;
            return;
        }

        let size_changed = pipeline.width != self.width
            || pipeline.height != self.height
            || pipeline.texture.is_none();

        if size_changed {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("video_frame_texture"),
                size: wgpu::Extent3d {
                    width: self.width,
                    height: self.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());

            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("video_bind_group"),
                layout: &pipeline.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&texture_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                    },
                ],
            });

            pipeline.texture = Some(texture);
            pipeline.bind_group = Some(bind_group);
            pipeline.width = self.width;
            pipeline.height = self.height;
        }

        if let Some(texture) = &pipeline.texture
            && let Some(rgba) = &self.rgba
            && !rgba.is_empty()
        {
            // Defense in depth: the state's FrameReady handler validates the
            // buffer, but never let a mismatched buffer reach the GPU upload
            // (wgpu panics on undersized sources, and width*4 is u32 math).
            let Some(bytes_per_row) = self.width.checked_mul(4) else {
                return;
            };
            let Some(required) = bytes_per_row
                .checked_mul(self.height)
                .and_then(|n| usize::try_from(n).ok())
            else {
                return;
            };
            if rgba.len() < required {
                tracing::error!(
                    "frame buffer too small: {} bytes for {}x{}; skipping upload",
                    rgba.len(),
                    self.width,
                    self.height
                );
                return;
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(self.height),
                },
                wgpu::Extent3d {
                    width: self.width,
                    height: self.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    fn draw(&self, pipeline: &Self::Pipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        if let Some(bind_group) = &pipeline.bind_group {
            render_pass.set_pipeline(&pipeline.pipeline);
            render_pass.set_vertex_buffer(0, pipeline.vertex_buffer.slice(..));
            render_pass.set_bind_group(0, bind_group, &[]);
            render_pass.draw(0..4, 0..1);
            true
        } else {
            false
        }
    }
}

/// The iced shader program bridging [`VideoPrimitive`] into the
/// `iced::widget::shader::Program` widget API. Prefer [`video_shader_view`].
#[derive(Debug, Clone)]
pub struct VideoProgram {
    primitive: VideoPrimitive,
}

impl VideoProgram {
    /// Creates a program that renders the given frame.
    pub fn new(primitive: VideoPrimitive) -> Self {
        Self { primitive }
    }
}

impl<Message> iced::widget::shader::Program<Message> for VideoProgram {
    type State = ();
    type Primitive = VideoPrimitive;

    fn draw(
        &self,
        _state: &Self::State,
        _cursor: mouse::Cursor,
        _bounds: Rectangle,
    ) -> Self::Primitive {
        self.primitive.clone()
    }
}

/// A raw video frame element without any control UI or overlay buttons.
///
/// Renders `rgba` (which must be `width × height × 4` bytes, unrotated) with
/// the given [`Rotation`] applied at draw time. Returns an empty element when
/// `rgba` is `None`.
pub fn video_shader_view<'a, Message: 'a, Theme: 'a, Renderer>(
    width: u32,
    height: u32,
    rotation: Rotation,
    rgba: Option<std::sync::Arc<Vec<u8>>>,
) -> Element<'a, Message, Theme, Renderer>
where
    Renderer: iced_wgpu::primitive::Renderer + 'a,
{
    iced::widget::Shader::new(VideoProgram::new(VideoPrimitive {
        width,
        height,
        rotation,
        rgba,
        zero_copy_texture: None,
        zero_copy: None,
    }))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// A zero-copy video frame element using imported GPU texture memory directly.
///
/// `frame` carries the DMA-BUF handle plus the worker slot it came from; the
/// texture is imported during `prepare` (which has the wgpu device). Prefer
/// [`video_player_view`](crate::video_player_view) which picks the path based
/// on the state.
pub fn video_zero_copy_shader_view<'a, Message: 'a, Theme: 'a, Renderer>(
    width: u32,
    height: u32,
    rotation: Rotation,
    frame: Option<crate::state::ZeroCopyFrame>,
) -> Element<'a, Message, Theme, Renderer>
where
    Renderer: iced_wgpu::primitive::Renderer + 'a,
{
    iced::widget::Shader::new(VideoProgram::new(VideoPrimitive {
        width,
        height,
        rotation,
        rgba: None,
        zero_copy_texture: None,
        zero_copy: frame,
    }))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
