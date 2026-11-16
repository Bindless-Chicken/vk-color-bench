use crate::color::ImageInfo;
use crate::gpu::{self, Buffer, Commands, Image, Result};
use crate::worker::ColorWorker;
use ash::{Device, Instance, vk};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Slot {
    transfer: Commands,
    snapshot: Image,
    readback: Arc<Buffer>,
    pending: bool,
}

pub(crate) struct Capture {
    pub(crate) worker: ColorWorker,
    slots: Vec<Slot>,
    device: Device,
    pub(crate) extent: vk::Extent2D,
    interval: Duration,
    last_capture: Option<Instant>,
}

pub(crate) fn analysis_extent(source: vk::Extent2D, max_width: u32) -> vk::Extent2D {
    if max_width == 0 || source.width <= max_width {
        return source;
    }
    vk::Extent2D {
        width: max_width,
        height: ((source.height as u64 * max_width as u64 / source.width as u64) as u32).max(1),
    }
}

impl Capture {
    pub(crate) fn new(
        instance: &Instance,
        physical_device: vk::PhysicalDevice,
        device: &Device,
        format: vk::Format,
        source: vk::Extent2D,
        families: [u32; 2],
    ) -> Result<Option<Self>> {
        if !matches!(
            format,
            vk::Format::R8G8B8A8_UNORM
                | vk::Format::B8G8R8A8_UNORM
                | vk::Format::R8G8B8A8_SRGB
                | vk::Format::B8G8R8A8_SRGB
                | vk::Format::A2R10G10B10_UNORM_PACK32
                | vk::Format::A2B10G10R10_UNORM_PACK32
        ) {
            log::warn!("analysis disabled for unsupported format {format:?}");
            return Ok(None);
        }
        let max_width = std::env::var("VK_COLOR_BENCH_MAX_WIDTH")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(source.width / 2);
        let features =
            unsafe { instance.get_physical_device_format_properties(physical_device, format) }
                .optimal_tiling_features;
        let extent = if features
            .contains(vk::FormatFeatureFlags::BLIT_SRC | vk::FormatFeatureFlags::BLIT_DST)
        {
            analysis_extent(source, max_width)
        } else {
            source
        };

        let fps = std::env::var("VK_COLOR_BENCH_ANALYSIS_HZ")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(12)
            .clamp(1, 240);
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };
        let len = (extent.width as usize)
            .checked_mul(extent.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or("capture size overflow")?;
        let mut slots = Vec::new();
        for _ in 0..3 {
            slots.push(Slot {
                transfer: Commands::new(device, families[1])?,
                snapshot: Image::new(
                    device,
                    &memory_properties,
                    extent,
                    format,
                    vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST,
                    families,
                )?,
                readback: Arc::new(Buffer::new(
                    device,
                    &memory_properties,
                    len,
                    vk::BufferUsageFlags::TRANSFER_DST,
                )?),
                pending: false,
            });
        }
        log::info!(
            "analysis samples {}x{} at up to {fps} Hz",
            extent.width,
            extent.height
        );
        Ok(Some(Self {
            worker: ColorWorker::new(ImageInfo {
                width: extent.width,
                height: extent.height,
                pixel_format: format,
            })?,
            slots,
            device: device.clone(),
            extent,
            interval: Duration::from_secs_f64(1.0 / fps as f64),
            last_capture: None,
        }))
    }

    pub(crate) fn poll(&mut self) -> Result<()> {
        for slot in &mut self.slots {
            if slot.pending && slot.transfer.ready()? {
                slot.transfer.submitted = false;
                slot.pending = !self.worker.submit(Arc::clone(&slot.readback));
            }
        }
        Ok(())
    }

    pub(crate) fn record(
        &mut self,
        command_buffer: vk::CommandBuffer,
        source: vk::Image,
        source_extent: vk::Extent2D,
    ) -> Result<Option<usize>> {
        self.poll()?;
        if self
            .last_capture
            .is_some_and(|last| last.elapsed() < self.interval)
        {
            return Ok(None);
        }
        let Some(index) = self
            .slots
            .iter()
            // A slot is free once its transfer is done and the worker dropped the buffer.
            .position(|slot| !slot.pending && Arc::strong_count(&slot.readback) == 1)
        else {
            return Ok(None);
        };
        let slot = &self.slots[index];
        unsafe {
            gpu::barrier(
                &self.device,
                command_buffer,
                source,
                vk::ImageLayout::PRESENT_SRC_KHR,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::AccessFlags::MEMORY_WRITE,
                vk::AccessFlags::TRANSFER_READ,
            );
            gpu::barrier(
                &self.device,
                command_buffer,
                slot.snapshot.image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::AccessFlags::empty(),
                vk::AccessFlags::TRANSFER_WRITE,
            );
            if source_extent == self.extent {
                let region = vk::ImageCopy::builder()
                    .src_subresource(gpu::color_subresource_layers())
                    .dst_subresource(gpu::color_subresource_layers())
                    .extent(vk::Extent3D {
                        width: self.extent.width,
                        height: self.extent.height,
                        depth: 1,
                    });
                self.device.cmd_copy_image(
                    command_buffer,
                    source,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    slot.snapshot.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[*region],
                );
            } else {
                let region = vk::ImageBlit::builder()
                    .src_subresource(gpu::color_subresource_layers())
                    .dst_subresource(gpu::color_subresource_layers())
                    .src_offsets([
                        vk::Offset3D::default(),
                        vk::Offset3D {
                            x: source_extent.width as i32,
                            y: source_extent.height as i32,
                            z: 1,
                        },
                    ])
                    .dst_offsets([
                        vk::Offset3D::default(),
                        vk::Offset3D {
                            x: self.extent.width as i32,
                            y: self.extent.height as i32,
                            z: 1,
                        },
                    ]);
                self.device.cmd_blit_image(
                    command_buffer,
                    source,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    slot.snapshot.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[*region],
                    vk::Filter::NEAREST,
                );
            }
            gpu::barrier(
                &self.device,
                command_buffer,
                slot.snapshot.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::empty(),
            );
            gpu::barrier(
                &self.device,
                command_buffer,
                source,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::ImageLayout::PRESENT_SRC_KHR,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags::TRANSFER_READ,
                vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            );
        }
        Ok(Some(index))
    }
    pub(crate) fn semaphore(&self, index: usize) -> vk::Semaphore {
        self.slots[index].transfer.semaphore
    }

    pub(crate) fn submit(&mut self, index: usize, queue: vk::Queue) -> Result<()> {
        let slot = &mut self.slots[index];
        slot.transfer.begin()?;
        unsafe {
            let region = vk::BufferImageCopy::builder()
                .image_subresource(gpu::color_subresource_layers())
                .image_extent(vk::Extent3D {
                    width: self.extent.width,
                    height: self.extent.height,
                    depth: 1,
                });
            self.device.cmd_copy_image_to_buffer(
                slot.transfer.command_buffer,
                slot.snapshot.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                slot.readback.buffer,
                &[*region],
            );
            let barrier = vk::BufferMemoryBarrier::builder()
                .buffer(slot.readback.buffer)
                .size(vk::WHOLE_SIZE)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::HOST_READ);
            self.device.cmd_pipeline_barrier(
                slot.transfer.command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &[],
                &[*barrier],
                &[],
            );
        }
        slot.transfer
            .submit(queue, &[slot.transfer.semaphore], false)?;
        slot.pending = true;
        self.last_capture = Some(Instant::now());
        Ok(())
    }
}
