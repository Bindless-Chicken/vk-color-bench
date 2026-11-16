use crate::capture::Capture;
use crate::gpu;
use crate::input::InputState;
use crate::overlays::OverlayRenderer;
use crate::queues::TransferQueue;
use crate::textures::{Texture, Textures, Upload};
use crate::ui::{UiContext, UiFrame};
use ash::vk;
use std::sync::Arc;

fn is_srgb_format(format: vk::Format) -> bool {
    matches!(
        format,
        vk::Format::R8_SRGB
            | vk::Format::R8G8_SRGB
            | vk::Format::R8G8B8_SRGB
            | vk::Format::B8G8R8_SRGB
            | vk::Format::R8G8B8A8_SRGB
            | vk::Format::B8G8R8A8_SRGB
            | vk::Format::A8B8G8R8_SRGB_PACK32
    )
}

pub(crate) struct SwapchainTarget<'a> {
    pub(crate) format: vk::Format,
    pub(crate) color_space: vk::ColorSpaceKHR,
    pub(crate) extent: vk::Extent2D,
    pub(crate) images: &'a [vk::Image],
    pub(crate) can_capture: bool,
}

pub(crate) struct FrameRenderer {
    targets: RenderTargets,
    frames: Vec<FrameResources>,
    renderer: egui_ash_renderer::Renderer,
    frame_cursor: usize,
    capture: Option<Capture>,
    capture_failed: bool,
    textures: Textures,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    families: [u32; 2],
    transfer: Option<Arc<TransferQueue>>,
    render_queue: Option<vk::Queue>,
    overlay_renderer: Option<OverlayRenderer>,
    hdr_gamut_available: bool,
}
impl FrameRenderer {
    pub(crate) fn new(
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        device: &ash::Device,
        queue_family: u32,
        target: SwapchainTarget<'_>,
        transfer: Option<Arc<TransferQueue>>,
    ) -> gpu::Result<Self> {
        let SwapchainTarget {
            format,
            color_space,
            extent,
            images,
            can_capture,
        } = target;
        if images.is_empty() {
            return Err("cannot create a renderer for an empty swapchain".to_owned());
        }
        if extent.width == 0 || extent.height == 0 {
            return Err("cannot create a renderer for a zero-sized swapchain".to_owned());
        }
        let queue_properties =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        if !queue_properties
            .get(queue_family as usize)
            .is_some_and(|properties| properties.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        {
            return Err("the present queue does not support graphics commands".to_owned());
        }

        let families = [
            queue_family,
            transfer
                .as_ref()
                .map_or(queue_family, |transfer| transfer.family),
        ];
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };
        let capture = if can_capture {
            Capture::new(instance, physical_device, device, format, extent, families)?
        } else {
            None
        };

        let overlay_renderer = if can_capture {
            match OverlayRenderer::new(
                instance,
                physical_device,
                device,
                format,
                vk::Extent3D {
                    width: extent.width,
                    height: extent.height,
                    depth: 1,
                },
                families,
            ) {
                Ok(renderer) => Some(renderer),
                Err(error) => {
                    log::warn!("fullscreen overlay unavailable: {error}");
                    None
                }
            }
        } else {
            None
        };

        let targets = RenderTargets::new(device, format, extent, images)?;
        let renderer = egui_ash_renderer::Renderer::with_default_allocator(
            instance,
            physical_device,
            device.clone(),
            targets.render_pass,
            egui_ash_renderer::Options {
                in_flight_frames: images.len().max(1),
                srgb_framebuffer: is_srgb_format(format),
                ..Default::default()
            },
        )
        .map_err(|error| format!("could not create egui renderer: {error}"))?;

        let frames = images
            .iter()
            .map(|_| FrameResources::new(device, queue_family))
            .collect::<gpu::Result<Vec<_>>>()?;

        Ok(Self {
            targets,
            frames,
            renderer,
            frame_cursor: 0,
            capture,
            capture_failed: false,
            textures: Textures::default(),
            memory_properties,
            families,
            transfer,
            render_queue: None,
            overlay_renderer,
            hdr_gamut_available: color_space == vk::ColorSpaceKHR::HDR10_ST2084_EXT,
        })
    }

    pub(crate) fn queue_family(&self) -> u32 {
        self.families[0]
    }

    pub(crate) fn render(
        &mut self,
        egui: &mut UiContext,
        input: &mut InputState,
        present: PresentFrame<'_>,
    ) -> gpu::Result<Option<vk::Semaphore>> {
        let PresentFrame {
            device,
            queue,
            image_index,
            image,
            extent,
            window,
            wait_semaphores,
        } = present;
        // Upload semaphores are only waited once; later frames rely on submission
        // order, which only holds if we stay on the same queue.
        if self.render_queue.is_some_and(|previous| previous != queue) {
            return Ok(None);
        }
        self.render_queue = Some(queue);
        let transfer = self.transfer.clone();
        let _transfer_guard = match &transfer {
            // Never block the present thread on another swapchain.
            Some(transfer) => match transfer.lock.try_lock() {
                Ok(guard) => Some(guard),
                Err(_) => return Ok(None),
            },
            None => None,
        };
        let transfer_queue = transfer.as_ref().map_or(queue, |transfer| transfer.queue);
        let frame_index = self.frame_cursor;
        let frame = &mut self.frames[frame_index];
        if frame.submitted {
            if !unsafe { device.get_fence_status(frame.fence) }.map_err(gpu::vulkan_error)? {
                return Ok(None);
            }
            frame.submitted = false;
        }
        frame.textures.clear();
        frame.upload = None;
        self.textures.collect(&mut self.renderer);
        frame.upload = self.textures.promote(&mut self.renderer)?;
        if let Some(upload) = &frame.upload {
            egui.commit_analysis(&upload.texture_ids().collect::<Vec<_>>());
        }
        if let Some(capture) = &mut self.capture {
            capture.poll()?;
            if egui.accepts_analysis()
                && let Some(result) = capture.worker.latest()
            {
                egui.set_color_analysis(result);
            }
        }
        let screen = egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(extent.width as f32, extent.height as f32),
        );
        let input = input.collect(window, screen);
        egui.set_hdr_gamut_available(self.hdr_gamut_available && self.overlay_renderer.is_some());
        let UiFrame {
            output,
            mut primitives,
            overlay,
            capture_pointer,
            capture_keyboard,
        } = egui.prepare_frame([extent.width, extent.height], input);
        #[cfg(windows)]
        if let Some(window) = window {
            crate::input::update_window_capture(window, capture_pointer, capture_keyboard);
        }
        #[cfg(not(windows))]
        let _ = (capture_pointer, capture_keyboard);
        self.textures.update(&output.textures_delta)?;
        self.textures.stage(
            device,
            &self.memory_properties,
            self.families,
            transfer_queue,
        )?;
        frame.textures = self.textures.bind(&mut primitives);
        // egui-ash-renderer only advances its buffer ring when it draws something;
        // our frame ring has to stay in step with it.
        let drew_meshes = !primitives.is_empty();
        let capture_index;

        unsafe {
            device
                .reset_command_pool(frame.command_pool, vk::CommandPoolResetFlags::empty())
                .map_err(gpu::vulkan_error)?;
            device
                .begin_command_buffer(
                    frame.command_buffer,
                    &vk::CommandBufferBeginInfo::builder()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(gpu::vulkan_error)?;

            capture_index = if self.capture_failed {
                None
            } else {
                self.capture
                    .as_mut()
                    .map(|capture| capture.record(frame.command_buffer, image, extent))
                    .transpose()?
                    .flatten()
            };

            if let (Some(renderer), Some(overlay_mode)) = (&mut self.overlay_renderer, overlay) {
                renderer.render(
                    device,
                    &frame.command_buffer,
                    image,
                    &self.targets.framebuffers[image_index],
                    extent,
                    overlay_mode,
                );
            }

            device.cmd_begin_render_pass(
                frame.command_buffer,
                &vk::RenderPassBeginInfo::builder()
                    .render_pass(self.targets.render_pass)
                    .framebuffer(self.targets.framebuffers[image_index])
                    .render_area(vk::Rect2D {
                        offset: vk::Offset2D::default(),
                        extent,
                    }),
                vk::SubpassContents::INLINE,
            );
        }
        self.renderer
            .cmd_draw(
                frame.command_buffer,
                extent,
                output.pixels_per_point,
                &primitives,
            )
            .map_err(|error| format!("could not record egui draw commands: {error}"))?;
        unsafe {
            device.cmd_end_render_pass(frame.command_buffer);
            device
                .end_command_buffer(frame.command_buffer)
                .map_err(gpu::vulkan_error)?;
        }

        let signal_semaphore = self.targets.present_semaphores[image_index];
        let mut waits = wait_semaphores.to_vec();
        let mut wait_stages = vec![
            vk::PipelineStageFlags::TRANSFER
                | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT;
            waits.len()
        ];
        if let Some(upload) = &frame.upload {
            waits.push(upload.commands.semaphore);
            wait_stages.push(vk::PipelineStageFlags::FRAGMENT_SHADER);
        }
        let command_buffers = [frame.command_buffer];
        let mut signal_semaphores = vec![signal_semaphore];
        if let Some(index) = capture_index {
            signal_semaphores.push(self.capture.as_ref().unwrap().semaphore(index));
        }
        let submit_info = vk::SubmitInfo::builder()
            .wait_semaphores(&waits)
            .wait_dst_stage_mask(&wait_stages)
            .command_buffers(&command_buffers)
            .signal_semaphores(&signal_semaphores)
            .build();
        unsafe {
            device
                .reset_fences(&[frame.fence])
                .map_err(gpu::vulkan_error)?;
            device
                .queue_submit(queue, &[submit_info], frame.fence)
                .map_err(gpu::vulkan_error)?;
        }
        frame.submitted = true;
        self.textures.free(&output.textures_delta.free);
        if drew_meshes {
            self.frame_cursor = (frame_index + 1) % self.frames.len();
        }
        if let Some(index) = capture_index
            && let Err(error) = self.capture.as_mut().unwrap().submit(index, transfer_queue)
        {
            self.capture_failed = true;
            log::error!("capture disabled after transfer failure: {error}");
        }
        Ok(Some(signal_semaphore))
    }

    pub(crate) fn destroy(self, device: &ash::Device) {
        for frame in &self.frames {
            if frame.submitted {
                unsafe {
                    let _ = device.wait_for_fences(&[frame.fence], true, u64::MAX);
                }
            }
        }
        let Self {
            renderer,
            frames,
            targets,
            capture,
            textures,
            ..
        } = self;
        drop(capture);
        drop(textures);
        drop(renderer);
        drop(frames);
        drop(targets);
    }
}

struct FrameResources {
    device: ash::Device,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    submitted: bool,
    textures: Vec<Arc<Texture>>,
    upload: Option<Upload>,
}

impl FrameResources {
    fn new(device: &ash::Device, queue_family: u32) -> gpu::Result<Self> {
        let mut frame = Self {
            device: device.clone(),
            command_pool: vk::CommandPool::null(),
            command_buffer: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
            submitted: false,
            textures: Vec::new(),
            upload: None,
        };
        unsafe {
            frame.command_pool = device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::builder()
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                        .queue_family_index(queue_family),
                    None,
                )
                .map_err(gpu::vulkan_error)?;
            frame.command_buffer = device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::builder()
                        .command_pool(frame.command_pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .map_err(gpu::vulkan_error)?[0];
            crate::queues::initialize(device, frame.command_buffer)?;
            frame.fence = device
                .create_fence(
                    &vk::FenceCreateInfo::builder().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )
                .map_err(gpu::vulkan_error)?;
        }
        Ok(frame)
    }
}

impl Drop for FrameResources {
    fn drop(&mut self) {
        unsafe {
            if self.submitted {
                let _ = self.device.wait_for_fences(&[self.fence], true, u64::MAX);
            }
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.command_pool, None);
        }
    }
}

pub(crate) struct PresentFrame<'a> {
    pub(crate) device: &'a ash::Device,
    pub(crate) queue: vk::Queue,
    pub(crate) image_index: usize,
    pub(crate) image: vk::Image,
    pub(crate) extent: vk::Extent2D,
    pub(crate) window: Option<isize>,
    pub(crate) wait_semaphores: &'a [vk::Semaphore],
}

struct RenderTargets {
    device: ash::Device,
    render_pass: vk::RenderPass,
    image_views: Vec<vk::ImageView>,
    framebuffers: Vec<vk::Framebuffer>,
    present_semaphores: Vec<vk::Semaphore>,
}
impl RenderTargets {
    fn new(
        device: &ash::Device,
        format: vk::Format,
        extent: vk::Extent2D,
        images: &[vk::Image],
    ) -> gpu::Result<Self> {
        let attachment = vk::AttachmentDescription::builder()
            .format(format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::LOAD)
            .store_op(vk::AttachmentStoreOp::STORE)
            .initial_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .final_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .build();
        let color_attachment = vk::AttachmentReference::builder()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .build();
        let subpass = vk::SubpassDescription::builder()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(std::slice::from_ref(&color_attachment))
            .build();
        let dependency = vk::SubpassDependency::builder()
            .src_subpass(vk::SUBPASS_EXTERNAL)
            .dst_subpass(0)
            .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            .dst_access_mask(
                vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            )
            .build();
        let render_pass_info = vk::RenderPassCreateInfo::builder()
            .attachments(std::slice::from_ref(&attachment))
            .subpasses(std::slice::from_ref(&subpass))
            .dependencies(std::slice::from_ref(&dependency));
        let render_pass = unsafe { device.create_render_pass(&render_pass_info, None) }
            .map_err(gpu::vulkan_error)?;

        let mut targets = Self {
            device: device.clone(),
            render_pass,
            image_views: Vec::with_capacity(images.len()),
            framebuffers: Vec::with_capacity(images.len()),
            present_semaphores: Vec::with_capacity(images.len()),
        };
        for &image in images {
            let view_info = vk::ImageViewCreateInfo::builder()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            targets.image_views.push(
                unsafe { device.create_image_view(&view_info, None) }.map_err(gpu::vulkan_error)?,
            );
        }

        for &view in &targets.image_views {
            let attachments = [view];
            let framebuffer_info = vk::FramebufferCreateInfo::builder()
                .render_pass(render_pass)
                .attachments(&attachments)
                .width(extent.width)
                .height(extent.height)
                .layers(1);
            targets.framebuffers.push(
                unsafe { device.create_framebuffer(&framebuffer_info, None) }
                    .map_err(gpu::vulkan_error)?,
            );
        }
        for _ in images {
            targets.present_semaphores.push(
                unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }
                    .map_err(gpu::vulkan_error)?,
            );
        }
        Ok(targets)
    }
}
impl Drop for RenderTargets {
    fn drop(&mut self) {
        unsafe {
            for &semaphore in &self.present_semaphores {
                self.device.destroy_semaphore(semaphore, None);
            }
            for &framebuffer in &self.framebuffers {
                self.device.destroy_framebuffer(framebuffer, None);
            }
            for &view in &self.image_views {
                self.device.destroy_image_view(view, None);
            }
            self.device.destroy_render_pass(self.render_pass, None);
        }
    }
}
