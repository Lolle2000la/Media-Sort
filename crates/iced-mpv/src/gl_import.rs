//! `wgpu` HAL handle import logic for zero-copy OpenGL FBO interop.

#[cfg(target_os = "linux")]
use std::os::unix::io::RawFd;

/// Imports a Linux DMA-BUF file descriptor into a [`iced_wgpu::wgpu::Texture`]
/// via Vulkan HAL (`VK_EXT_external_memory_dma_buf`).
///
/// The image is created with external-memory handle type `DMA_BUF_EXT`, a
/// dedicated allocation is imported from `fd` (`vkImportMemoryFdInfoKHR` +
/// `VkMemoryDedicatedAllocateInfo`) and bound to the image. Only
/// `R8G8B8A8_UNORM` images are supported (the producer forces
/// `internal_format = GL_RGBA8`, so the dma-buf bytes are R,G,B,A).
///
/// `stride` must be `>= width * 4` (it is validated, then passed through to
/// the driver implicitly — the dma-buf carries its own pitch via its DRM
/// modifier). The image is destroyed and its memory freed when the returned
/// wgpu texture is dropped.
///
/// # Safety
///
/// `fd` must be a valid, readable DMA-BUF file descriptor backed by linear or
/// optimal tiling GPU memory. Ownership of `fd` transfers to the imported
/// `VkDeviceMemory` on success (closed when the memory is freed); on failure
/// `fd` is closed by this function.
#[cfg(target_os = "linux")]
pub unsafe fn import_dmabuf_to_wgpu(
    wgpu_device: &iced_wgpu::wgpu::Device,
    fd: RawFd,
    width: u32,
    height: u32,
    drm_format: u32,
    stride: u32,
) -> Result<iced_wgpu::wgpu::Texture, String> {
    use ash::vk;

    if drm_format != mpv_utils::gl_context::DRM_FORMAT_ABGR8888 {
        return Err(format!("unsupported drm_format 0x{drm_format:08x}"));
    }
    let Some(expected) = width.checked_mul(4) else {
        return Err("width overflows stride math".to_string());
    };
    if stride < expected {
        return Err(format!(
            "dma-buf stride {stride} < width*4 {expected}; cannot import"
        ));
    }

    unsafe {
        let Some(hal_device) = wgpu_device.as_hal::<wgpu_hal::api::Vulkan>() else {
            return Err("wgpu Device is not using Vulkan backend".to_string());
        };
        let raw_device = hal_device.raw_device();
        let physical_device = hal_device.raw_physical_device();

        // The image must be created with the external-memory handle type so
        // it can later be bound to imported dma-buf memory.
        let mut external_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let img_create_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_info);

        let vk_image = raw_device
            .create_image(&img_create_info, None)
            .map_err(|e| format!("vkCreateImage failed: {e}"))?;

        // Close the fd if anything below fails before the memory owns it.
        macro_rules! fail {
            ($e:expr) => {{
                let e = format!($e);
                let _ = raw_device.destroy_image(vk_image, None);
                // SAFETY: fd ownership only transferred on successful import.
                let _ = libc::close(fd);
                return Err(e);
            }};
        }

        let reqs = raw_device.get_image_memory_requirements(vk_image);

        // Pick a host-visible + host-coherent memory type compatible with the
        // image: linear Mesa dma-bufs are host-accessible, and importing them
        // requires a memory type that supports the handle type (the driver
        // marks the compatible types in `memory_type_bits`).
        let mem_props = {
            let instance = hal_device.shared_instance().raw_instance();
            instance.get_physical_device_memory_properties(physical_device)
        };
        let mem_type_index = mem_props
            .memory_types_as_slice()
            .iter()
            .enumerate()
            .find(|(i, mt)| {
                (reqs.memory_type_bits & (1 << i)) != 0
                    && mt.property_flags.contains(
                        vk::MemoryPropertyFlags::HOST_VISIBLE
                            | vk::MemoryPropertyFlags::HOST_COHERENT,
                    )
            })
            .map(|(i, _)| i as u32);
        let mem_type_index = match mem_type_index {
            Some(v) => v,
            None => fail!("no host-visible memory type for dma-buf import"),
        };

        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(vk_image);
        let mut import_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(fd);
        // The import info chains onto the dedicated allocation.
        import_info.p_next = &mut dedicated as *mut _ as *mut std::ffi::c_void;

        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(reqs.size)
            .memory_type_index(mem_type_index)
            .push_next(&mut import_info);
        let memory = match raw_device.allocate_memory(&alloc_info, None) {
            Ok(m) => m,
            Err(e) => fail!("vkAllocateMemory(import fd) failed: {e}"),
        };
        // On success the VkDeviceMemory owns `fd` (closed on free).

        if let Err(e) = raw_device.bind_image_memory(vk_image, memory, 0) {
            raw_device.free_memory(memory, None);
            fail!("vkBindImageMemory failed: {e}");
        }

        // Free the memory (and destroy the image) when the wgpu texture is
        // dropped. wgpu-hal's `texture_from_raw` neither frees imported
        // memory nor owns the image when a drop callback is provided.
        let device_for_drop = raw_device.clone();
        let drop_callback: wgpu_hal::DropCallback = Box::new(move || {
            device_for_drop.free_memory(memory, None);
            device_for_drop.destroy_image(vk_image, None);
        });

        let hal_texture = hal_device.texture_from_raw(
            vk_image,
            &wgpu_hal::TextureDescriptor {
                label: Some("mpv_dmabuf_texture"),
                size: iced_wgpu::wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: iced_wgpu::wgpu::TextureDimension::D2,
                format: iced_wgpu::wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu_types::TextureUses::RESOURCE,
                memory_flags: wgpu_hal::MemoryFlags::empty(),
                view_formats: Vec::new(),
            },
            Some(drop_callback),
        );

        let wgpu_texture = wgpu_device.create_texture_from_hal::<wgpu_hal::api::Vulkan>(
            hal_texture,
            &iced_wgpu::wgpu::TextureDescriptor {
                label: Some("mpv_dmabuf_wgpu"),
                size: iced_wgpu::wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: iced_wgpu::wgpu::TextureDimension::D2,
                format: iced_wgpu::wgpu::TextureFormat::Rgba8Unorm,
                usage: iced_wgpu::wgpu::TextureUsages::TEXTURE_BINDING
                    | iced_wgpu::wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            },
        );

        Ok(wgpu_texture)
    }
}

/// Imports a Windows Direct3D11 shared handle into a [`iced_wgpu::wgpu::Texture`] via DirectX 12 HAL.
///
/// # Safety
///
/// `shared_handle` must be a valid Direct3D11 shared resource handle created with `D3D11_RESOURCE_MISC_SHARED`.
#[cfg(windows)]
pub unsafe fn import_d3d11_handle_to_wgpu_dx12(
    wgpu_device: &iced_wgpu::wgpu::Device,
    shared_handle: *mut std::ffi::c_void,
    width: u32,
    height: u32,
) -> Result<iced_wgpu::wgpu::Texture, String> {
    unsafe {
        let Some(hal_device) = wgpu_device.as_hal::<wgpu_hal::api::Dx12>() else {
            return Err("wgpu Device is not using DirectX 12 backend".to_string());
        };
        let raw_device = hal_device.raw_device();

        let mut d3d12_resource: Option<windows::Win32::Graphics::Direct3D12::ID3D12Resource> = None;
        let win_handle = windows::Win32::Foundation::HANDLE(shared_handle);
        if let Err(e) = raw_device.OpenSharedHandle(win_handle, &mut d3d12_resource) {
            return Err(format!("OpenSharedHandle failed: {e}"));
        }

        let Some(d3d12_res) = d3d12_resource else {
            return Err("Failed to retrieve ID3D12Resource from OpenSharedHandle".into());
        };

        let hal_texture = wgpu_hal::dx12::Device::texture_from_raw(
            d3d12_res,
            iced_wgpu::wgpu::TextureFormat::Rgba8Unorm,
            iced_wgpu::wgpu::TextureDimension::D2,
            iced_wgpu::wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            1,
            1,
        );

        let wgpu_texture = wgpu_device.create_texture_from_hal::<wgpu_hal::api::Dx12>(
            hal_texture,
            &iced_wgpu::wgpu::TextureDescriptor {
                label: Some("mpv_dx12_wgpu"),
                size: iced_wgpu::wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: iced_wgpu::wgpu::TextureDimension::D2,
                format: iced_wgpu::wgpu::TextureFormat::Rgba8Unorm,
                usage: iced_wgpu::wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        );

        Ok(wgpu_texture)
    }
}
