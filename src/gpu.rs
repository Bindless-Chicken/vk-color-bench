use ash::{Device, vk};

pub(crate) type Result<T> = std::result::Result<T, String>;
pub(crate) fn vulkan_error(error: vk::Result) -> String {
    format!("Vulkan error: {error:?}")
}

pub(crate) struct Buffer {
    device: Device,
    pub(crate) buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    pub(crate) mapped: usize,
    pub(crate) len: usize,
}
impl Buffer {
    pub(crate) fn new(
        device: &Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        len: usize,
        usage: vk::BufferUsageFlags,
    ) -> Result<Self> {
        let mut out = Self {
            device: device.clone(),
            buffer: vk::Buffer::null(),
            memory: vk::DeviceMemory::null(),
            mapped: 0,
            len,
        };
        unsafe {
            out.buffer = device
                .create_buffer(
                    &vk::BufferCreateInfo::builder()
                        .size(len as u64)
                        .usage(usage),
                    None,
                )
                .map_err(vulkan_error)?;
            let requirements = device.get_buffer_memory_requirements(out.buffer);
            let required =
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let index = memory_type(
                memory_properties,
                requirements.memory_type_bits,
                required,
                vk::MemoryPropertyFlags::HOST_CACHED,
            )?;
            out.memory = device
                .allocate_memory(
                    &vk::MemoryAllocateInfo::builder()
                        .allocation_size(requirements.size)
                        .memory_type_index(index),
                    None,
                )
                .map_err(vulkan_error)?;
            device
                .bind_buffer_memory(out.buffer, out.memory, 0)
                .map_err(vulkan_error)?;
            out.mapped = device
                .map_memory(out.memory, 0, len as u64, vk::MemoryMapFlags::empty())
                .map_err(vulkan_error)? as usize;
        }
        Ok(out)
    }
    // Safety: the GPU must be done writing to this buffer.
    pub(crate) unsafe fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.mapped as *const u8, self.len) }
    }
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        assert_eq!(bytes.len(), self.len);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.mapped as *mut u8, bytes.len());
        }
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe {
            if self.mapped != 0 {
                self.device.unmap_memory(self.memory);
            }
            self.device.destroy_buffer(self.buffer, None);
            self.device.free_memory(self.memory, None);
        }
    }
}

fn memory_type(
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    bits: u32,
    required: vk::MemoryPropertyFlags,
    preferred: vk::MemoryPropertyFlags,
) -> Result<u32> {
    (0..memory_properties.memory_type_count)
        .filter(|&index| {
            bits & (1 << index) != 0
                && memory_properties.memory_types[index as usize]
                    .property_flags
                    .contains(required)
        })
        .max_by_key(|&index| {
            memory_properties.memory_types[index as usize]
                .property_flags
                .contains(preferred)
        })
        .ok_or_else(|| "no compatible Vulkan memory type".to_owned())
}

pub(crate) struct Image {
    device: Device,
    pub(crate) image: vk::Image,
    memory: vk::DeviceMemory,
    pub(crate) extent: vk::Extent2D,
}
impl Image {
    pub(crate) fn new(
        device: &Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        extent: vk::Extent2D,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
        families: [u32; 2],
    ) -> Result<Self> {
        let mut out = Self {
            device: device.clone(),
            image: vk::Image::null(),
            memory: vk::DeviceMemory::null(),
            extent,
        };
        let mut info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: extent.width,
                height: extent.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage);
        if families[0] != families[1] {
            info = info
                .sharing_mode(vk::SharingMode::CONCURRENT)
                .queue_family_indices(&families);
        }
        unsafe {
            out.image = device.create_image(&info, None).map_err(vulkan_error)?;
            let requirements = device.get_image_memory_requirements(out.image);
            let index = memory_type(
                memory_properties,
                requirements.memory_type_bits,
                vk::MemoryPropertyFlags::empty(),
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )?;
            out.memory = device
                .allocate_memory(
                    &vk::MemoryAllocateInfo::builder()
                        .allocation_size(requirements.size)
                        .memory_type_index(index),
                    None,
                )
                .map_err(vulkan_error)?;
            device
                .bind_image_memory(out.image, out.memory, 0)
                .map_err(vulkan_error)?;
        }
        Ok(out)
    }
}
impl Drop for Image {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_image(self.image, None);
            self.device.free_memory(self.memory, None);
        }
    }
}

pub(crate) struct Commands {
    device: Device,
    pool: vk::CommandPool,
    pub(crate) command_buffer: vk::CommandBuffer,
    pub(crate) fence: vk::Fence,
    pub(crate) semaphore: vk::Semaphore,
    pub(crate) submitted: bool,
}
impl Commands {
    pub(crate) fn new(device: &Device, family: u32) -> Result<Self> {
        let mut out = Self {
            device: device.clone(),
            pool: vk::CommandPool::null(),
            command_buffer: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
            semaphore: vk::Semaphore::null(),
            submitted: false,
        };
        unsafe {
            out.pool = device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::builder().queue_family_index(family),
                    None,
                )
                .map_err(vulkan_error)?;
            out.command_buffer = device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::builder()
                        .command_pool(out.pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .map_err(vulkan_error)?[0];
            crate::queues::initialize(device, out.command_buffer)?;
            out.fence = device
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(vulkan_error)?;
            out.semaphore = device
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                .map_err(vulkan_error)?;
        }
        Ok(out)
    }
    pub(crate) fn ready(&self) -> Result<bool> {
        if !self.submitted {
            Ok(true)
        } else {
            unsafe {
                self.device
                    .get_fence_status(self.fence)
                    .map_err(vulkan_error)
            }
        }
    }
    pub(crate) fn begin(&mut self) -> Result<()> {
        unsafe {
            self.device
                .reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty())
                .map_err(vulkan_error)?;
            self.device
                .reset_fences(&[self.fence])
                .map_err(vulkan_error)?;
            self.submitted = false;
            self.device
                .begin_command_buffer(
                    self.command_buffer,
                    &vk::CommandBufferBeginInfo::builder()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(vulkan_error)
        }
    }
    pub(crate) fn submit(
        &mut self,
        queue: vk::Queue,
        wait: &[vk::Semaphore],
        signal: bool,
    ) -> Result<()> {
        unsafe {
            self.device
                .end_command_buffer(self.command_buffer)
                .map_err(vulkan_error)?;
            let command_buffers = [self.command_buffer];
            let stages = vec![vk::PipelineStageFlags::TRANSFER; wait.len()];
            let signals = if signal { vec![self.semaphore] } else { vec![] };
            let info = vk::SubmitInfo::builder()
                .command_buffers(&command_buffers)
                .wait_semaphores(wait)
                .wait_dst_stage_mask(&stages)
                .signal_semaphores(&signals);
            self.device
                .queue_submit(queue, &[*info], self.fence)
                .map_err(vulkan_error)?;
            self.submitted = true;
            Ok(())
        }
    }
}
impl Drop for Commands {
    fn drop(&mut self) {
        unsafe {
            if self.submitted {
                let _ = self.device.wait_for_fences(&[self.fence], true, u64::MAX);
            }
            self.device.destroy_semaphore(self.semaphore, None);
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.pool, None);
        }
    }
}

pub(crate) fn color_subresource_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}
pub(crate) fn color_subresource_layers() -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        mip_level: 0,
        base_array_layer: 0,
        layer_count: 1,
    }
}
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn barrier(
    device: &Device,
    command_buffer: vk::CommandBuffer,
    image: vk::Image,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags,
    dst_stage: vk::PipelineStageFlags,
    src_access: vk::AccessFlags,
    dst_access: vk::AccessFlags,
) {
    let image_barrier = vk::ImageMemoryBarrier::builder()
        .image(image)
        .subresource_range(color_subresource_range())
        .old_layout(old_layout)
        .new_layout(new_layout)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .src_access_mask(src_access)
        .dst_access_mask(dst_access);
    unsafe {
        device.cmd_pipeline_barrier(
            command_buffer,
            src_stage,
            dst_stage,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[*image_barrier],
        );
    }
}
