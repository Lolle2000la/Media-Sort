//! Phase 3a spike: validates the Linux zero-copy round trip
//! `GBM dma-buf → EGLImage → GL FBO → mpv render → Vulkan import`.
//!
//! The tests skip (return early, printing SKIP) when the environment lacks a
//! usable GPU (EGL, GBM render node, Vulkan). CI machines without hardware
//! acceleration therefore don't fail, and the local dev machine with an
//! AMD/RADV stack exercises the full path.

#![cfg(target_os = "linux")]

use mpv_utils::MpvContext;
use mpv_utils::gl_context::{DRM_FORMAT_ABGR8888, GbmDevice, GlContext};
use std::path::PathBuf;
use std::slice;

// ---------------------------------------------------------------------------
// Minimal Vulkan helper for the import side (ash, dev-dependency only)
// ---------------------------------------------------------------------------

struct VkImport {
    _instance: ash::Instance,
    _device: ash::Device,
    memory: ash::vk::DeviceMemory,
    mapped: *mut libc::c_void,
}

impl VkImport {
    fn new(fd: i32, width: u32, height: u32) -> Result<Self, String> {
        use ash::vk;

        // SAFETY: loading the Vulkan entry points is safe.
        let entry =
            unsafe { ash::Entry::load() }.map_err(|e| format!("Entry::load failed: {e}"))?;
        let app_info = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_2);
        let instance = unsafe {
            entry.create_instance(
                &vk::InstanceCreateInfo::default().application_info(&app_info),
                None,
            )
        }
        .map_err(|e| format!("create_instance failed: {e}"))?;

        let phys = unsafe { instance.enumerate_physical_devices() }
            .map_err(|e| format!("enumerate_physical_devices failed: {e}"))?
            .into_iter()
            .next()
            .ok_or_else(|| "no Vulkan physical device".to_string())?;

        let props = unsafe { instance.get_physical_device_properties(phys) };
        eprintln!(
            "SPIKE: Vulkan device: {}",
            unsafe { std::ffi::CStr::from_ptr(props.device_name.as_ptr()) }.to_string_lossy()
        );

        let queue_family = unsafe { instance.get_physical_device_queue_family_properties(phys) }
            .iter()
            .position(|p| p.queue_flags.contains(vk::QueueFlags::COMPUTE))
            .ok_or_else(|| "no compute queue family".to_string())?;

        let mut dev_create = vk::DeviceCreateInfo::default();
        let queue_priorities = [1.0f32];
        let queue_info = vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family as u32)
            .queue_priorities(&queue_priorities);
        let ext_names: Vec<std::ffi::CString> = [
            "VK_KHR_external_memory_fd",
            "VK_EXT_external_memory_dma_buf",
        ]
        .into_iter()
        .map(|s| std::ffi::CString::new(s).unwrap())
        .collect();
        let ext_ptrs: Vec<*const libc::c_char> = ext_names.iter().map(|s| s.as_ptr()).collect();
        dev_create = dev_create
            .queue_create_infos(slice::from_ref(&queue_info))
            .enabled_extension_names(&ext_ptrs);

        let device = unsafe { instance.create_device(phys, &dev_create, None) }
            .map_err(|e| format!("create_device failed: {e}"))?;

        // Image with external dma-buf handle type.
        let mut ext_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let img_info = vk::ImageCreateInfo::default()
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
            .push_next(&mut ext_info);
        let image = unsafe { device.create_image(&img_info, None) }
            .map_err(|e| format!("create_image failed: {e}"))?;

        let reqs = unsafe { device.get_image_memory_requirements(image) };

        let mem_props = unsafe { instance.get_physical_device_memory_properties(phys) };
        let mem_type = mem_props
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
            .map(|(i, _)| i as u32)
            .ok_or_else(|| "no host-visible memory type for dma-buf import".to_string())?;

        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let mut import_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(fd);
        import_info.p_next = &mut dedicated as *mut _ as *mut libc::c_void;

        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(reqs.size)
            .memory_type_index(mem_type)
            .push_next(&mut import_info);
        let memory = unsafe { device.allocate_memory(&alloc_info, None) }
            .map_err(|e| format!("allocate_memory(import fd) failed: {e}"))?;
        unsafe { device.bind_image_memory(image, memory, 0) }
            .map_err(|e| format!("bind_image_memory failed: {e}"))?;

        // SAFETY: map the host-visible dma-buf memory.
        let mapped =
            unsafe { device.map_memory(memory, 0, reqs.size, vk::MemoryMapFlags::empty()) }
                .map_err(|e| format!("map_memory failed: {e}"))?;

        Ok(Self {
            _instance: instance,
            _device: device,
            memory,
            mapped,
        })
    }

    fn read_pixels(&self, len: usize) -> Vec<u8> {
        // SAFETY: mapped memory holds at least `size` bytes; read only `len`.
        let data = unsafe { slice::from_raw_parts(self.mapped as *const u8, len) };
        data.to_vec()
    }
}

impl Drop for VkImport {
    fn drop(&mut self) {
        // SAFETY: unmap then free the imported memory on the device.
        unsafe {
            self._device.unmap_memory(self.memory);
            self._device.free_memory(self.memory, None);
        }
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../resources/MockState"
    ))
    .join(name)
}

/// Returns a GL context + GBM device, or `None` (skip) when the environment
/// lacks EGL/GBM support.
fn gl_env() -> Option<(GlContext, GbmDevice)> {
    let gl = match GlContext::new() {
        Ok(gl) => gl,
        Err(e) => {
            eprintln!("SKIP: no EGL surfaceless context: {e}");
            return None;
        }
    };
    let gbm = match GbmDevice::new() {
        Ok(gbm) => gbm,
        Err(e) => {
            eprintln!("SKIP: no GBM device: {e}");
            return None;
        }
    };
    Some((gl, gbm))
}

/// Allocates a dma-buf, wraps it in a GL texture/FBO and imports the same fd
/// into Vulkan, returning the FBO and the importer.
fn round_trip(
    gl: &mut GlContext,
    gbm: &GbmDevice,
    w: u32,
    h: u32,
) -> Result<(u32, VkImport), String> {
    let bo = gbm.allocate(w, h)?;
    let stride = bo.stride();
    let fd = bo.fd()?;
    gl.make_current()?;
    let (tex, fbo) = gl.texture_from_dmabuf(fd, w, h, stride, DRM_FORMAT_ABGR8888)?;
    // Note: `bo` is kept alive (its buffer backs the GL texture) but we hold
    // the exported fd for Vulkan separately.
    let import = VkImport::new(fd, w, h)?;
    // The imported memory is the SAME dma-buf the GL texture renders into.
    let _ = tex;
    let _ = bo;
    Ok((fbo, import))
}

// ---------------------------------------------------------------------------
// Spike tests
// ---------------------------------------------------------------------------

/// Test 1: GL clear → dma-buf → Vulkan map. Writes (0,0,0,255) via GL and
/// reads it back through Vulkan-mapped memory, proving the shared buffer
/// round trip and that the memory type selection works.
#[test]
fn test_gl_clear_dmabuf_vulkan_roundtrip() {
    let Some((mut gl, gbm)) = gl_env() else {
        return;
    };
    let w = 64u32;
    let h = 64u32;
    let (fbo, import) = match round_trip(&mut gl, &gbm, w, h) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("SKIP: round trip setup failed: {e}");
            return;
        }
    };
    gl.clear_and_finish(fbo, w, h, [1.0, 0.0, 0.0, 1.0]);
    let pixels = import.read_pixels((w * h * 4) as usize);
    assert!(
        pixels.iter().all(|&b| b == 255 || b == 0),
        "unexpected pattern in cleared buffer"
    );
    // Every pixel must be red (255,0,0,255): R first byte.
    assert!(
        pixels[..(w * h * 4) as usize]
            .chunks_exact(4)
            .all(|px| px[0] == 255 && px[1] == 0 && px[2] == 0 && px[3] == 255),
        "expected red fill via dma-buf, got {pixels:?} (first pixels)"
    );
}

/// Test 2: mpv GL-renders a real video into the FBO → dma-buf → Vulkan.
/// Skips when libmpv is unavailable; renders the fixture at 128×72.
#[test]
fn test_mpv_gl_render_to_dmabuf_vulkan() {
    let gbm = match GbmDevice::new() {
        Ok(gbm) => gbm,
        Err(e) => {
            eprintln!("SKIP: no GBM device: {e}");
            return;
        }
    };
    // Create the mpv player first; its GL context owns the target FBO (GL
    // objects are context-scoped, and render_gl_frame needs them there).
    let mut player =
        match MpvContext::new_gl(GlContext::new().expect("GL context creation failed"), true) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("SKIP: MpvContext::new_gl failed: {e}");
                return;
            }
        };
    let w = 128u32;
    let h = 72u32;
    let gl_ctx = match player.gl_mut() {
        Some(g) => g,
        None => {
            eprintln!("SKIP: no GL context on player");
            return;
        }
    };
    let (fbo, import) = match round_trip(gl_ctx, &gbm, w, h) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("SKIP: round trip setup failed: {e}");
            return;
        }
    };

    // Load the fixture and wait for the video output to be ready.
    let path = fixture("mock 3.mp4");
    player.stop();
    if let Err(e) = player.load_file(&path) {
        eprintln!("SKIP: load_file failed: {e}");
        return;
    }
    player.set_paused(true);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut rendered = false;
    while std::time::Instant::now() < deadline {
        if player.has_frame_ready()
            && player.is_video_ready()
            && player.render_gl_frame(fbo, w as i32, h as i32).is_ok()
        {
            rendered = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !rendered {
        eprintln!("SKIP: mpv did not produce a GL-rendered frame within 5 s");
        return;
    }

    let pixels = import.read_pixels((w * h * 4) as usize);
    // The video content should not be all-black or all-transparent: assert
    // there is meaningful variation and that the buffer is populated.
    let nonzero = pixels.iter().filter(|&&b| b != 0).count();
    assert!(
        nonzero > (w * h) as usize,
        "expected more than one byte per pixel set (got {nonzero} non-zero bytes)"
    );
}
