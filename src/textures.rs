use crate::gpu::{self, Buffer, Commands, Image, Result};
use ash::{Device, vk};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub(crate) struct Texture {
    device: Device,
    image: Image,
    view: vk::ImageView,
    sampler: vk::Sampler,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
    set: vk::DescriptorSet,
}
impl Texture {
    fn new(
        device: &Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        image: &egui::ColorImage,
        options: egui::TextureOptions,
        families: [u32; 2],
    ) -> Result<Self> {
        let extent = vk::Extent2D {
            width: image.width() as u32,
            height: image.height() as u32,
        };
        let mut out = Self {
            device: device.clone(),
            image: Image::new(
                device,
                memory_properties,
                extent,
                vk::Format::R8G8B8A8_UNORM,
                vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
                families,
            )?,
            view: vk::ImageView::null(),
            sampler: vk::Sampler::null(),
            pool: vk::DescriptorPool::null(),
            layout: vk::DescriptorSetLayout::null(),
            set: vk::DescriptorSet::null(),
        };
        let filter = |texture_filter| {
            if texture_filter == egui::TextureFilter::Nearest {
                vk::Filter::NEAREST
            } else {
                vk::Filter::LINEAR
            }
        };
        let address = match options.wrap_mode {
            egui::TextureWrapMode::ClampToEdge => vk::SamplerAddressMode::CLAMP_TO_EDGE,
            egui::TextureWrapMode::Repeat => vk::SamplerAddressMode::REPEAT,
            egui::TextureWrapMode::MirroredRepeat => vk::SamplerAddressMode::MIRRORED_REPEAT,
        };
        unsafe {
            out.view = device
                .create_image_view(
                    &vk::ImageViewCreateInfo::builder()
                        .image(out.image.image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(vk::Format::R8G8B8A8_UNORM)
                        .subresource_range(gpu::color_subresource_range()),
                    None,
                )
                .map_err(gpu::vulkan_error)?;
            out.sampler = device
                .create_sampler(
                    &vk::SamplerCreateInfo::builder()
                        .min_filter(filter(options.minification))
                        .mag_filter(filter(options.magnification))
                        .address_mode_u(address)
                        .address_mode_v(address)
                        .address_mode_w(address),
                    None,
                )
                .map_err(gpu::vulkan_error)?;
            let sizes = [vk::DescriptorPoolSize {
                ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                descriptor_count: 1,
            }];
            out.pool = device
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::builder()
                        .max_sets(1)
                        .pool_sizes(&sizes),
                    None,
                )
                .map_err(gpu::vulkan_error)?;
            let bindings = [*vk::DescriptorSetLayoutBinding::builder()
                .binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
            out.layout = device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::builder().bindings(&bindings),
                    None,
                )
                .map_err(gpu::vulkan_error)?;
            let allocated = device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::builder()
                    .descriptor_pool(out.pool)
                    .set_layouts(&[out.layout]),
            );
            out.set = allocated.map_err(gpu::vulkan_error)?[0];
            let infos = [*vk::DescriptorImageInfo::builder()
                .sampler(out.sampler)
                .image_view(out.view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            device.update_descriptor_sets(
                &[*vk::WriteDescriptorSet::builder()
                    .dst_set(out.set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&infos)],
                &[],
            );
        }
        Ok(out)
    }
}
impl Drop for Texture {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_descriptor_pool(self.pool, None);
            self.device.destroy_descriptor_set_layout(self.layout, None);
            self.device.destroy_sampler(self.sampler, None);
            self.device.destroy_image_view(self.view, None);
        }
    }
}

struct Binding {
    texture: Arc<Texture>,
    user_id: egui::TextureId,
}
pub(crate) struct Upload {
    pub(crate) commands: Commands,
    _staging: Vec<Buffer>,
    textures: Vec<(egui::TextureId, Arc<Texture>)>,
}

impl Upload {
    pub(crate) fn texture_ids(&self) -> impl Iterator<Item = egui::TextureId> + '_ {
        self.textures.iter().map(|(id, _)| *id)
    }
}

#[derive(Default)]
pub(crate) struct Textures {
    pending: Option<Upload>,
    cpu: HashMap<egui::TextureId, (egui::ColorImage, egui::TextureOptions)>,
    dirty: HashSet<egui::TextureId>,
    active: HashMap<egui::TextureId, Binding>,
    retired: Vec<Binding>,
}
impl Textures {
    pub(crate) fn update(&mut self, delta: &egui::TexturesDelta) -> Result<()> {
        for (id, change) in &delta.set {
            let image = match &change.image {
                egui::ImageData::Color(image) => (**image).clone(),
                egui::ImageData::Font(image) => egui::ColorImage {
                    size: image.size,
                    pixels: image.srgba_pixels(None).collect(),
                },
            };
            if let Some(position) = change.pos {
                let (existing, options) = self
                    .cpu
                    .get_mut(id)
                    .ok_or("partial update of missing texture")?;
                patch(existing, &image, position)?;
                *options = change.options;
            } else {
                self.cpu.insert(*id, (image, change.options));
            }
            self.dirty.insert(*id);
        }
        Ok(())
    }

    pub(crate) fn free(&mut self, ids: &[egui::TextureId]) {
        for id in ids {
            self.cpu.remove(id);
            self.dirty.remove(id);
            if let Some(binding) = self.active.remove(id) {
                self.retired.push(binding);
            }
        }
    }

    pub(crate) fn collect(&mut self, renderer: &mut egui_ash_renderer::Renderer) {
        self.retired.retain(|binding| {
            if Arc::strong_count(&binding.texture) == 1 {
                renderer.remove_user_texture(binding.user_id);
                false
            } else {
                true
            }
        });
    }

    pub(crate) fn promote(
        &mut self,
        renderer: &mut egui_ash_renderer::Renderer,
    ) -> Result<Option<Upload>> {
        if !self
            .pending
            .as_ref()
            .map(|pending| pending.commands.ready())
            .transpose()?
            .unwrap_or(false)
        {
            return Ok(None);
        }
        let mut upload = self.pending.take().unwrap();
        upload.commands.submitted = false;
        for (id, texture) in &upload.textures {
            if !self.cpu.contains_key(id) {
                continue;
            }
            let binding = Binding {
                texture: Arc::clone(texture),
                user_id: renderer.add_user_texture(texture.set),
            };
            if let Some(old) = self.active.insert(*id, binding) {
                self.retired.push(old);
            }
        }
        Ok(Some(upload))
    }

    pub(crate) fn stage(
        &mut self,
        device: &Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        families: [u32; 2],
        queue: vk::Queue,
    ) -> Result<()> {
        if self.pending.is_some() || self.dirty.is_empty() {
            return Ok(());
        }
        let mut upload = Upload {
            commands: Commands::new(device, families[1])?,
            _staging: Vec::new(),
            textures: Vec::new(),
        };
        upload.commands.begin()?;
        for id in &self.dirty {
            let (image, options) = &self.cpu[id];
            let texture = Arc::new(Texture::new(
                device,
                memory_properties,
                image,
                *options,
                families,
            )?);
            let bytes: Vec<u8> = image
                .pixels
                .iter()
                .flat_map(|pixel| pixel.to_array())
                .collect();
            let mut staging = Buffer::new(
                device,
                memory_properties,
                bytes.len(),
                vk::BufferUsageFlags::TRANSFER_SRC,
            )?;
            staging.write(&bytes);
            unsafe {
                gpu::barrier(
                    device,
                    upload.commands.command_buffer,
                    texture.image.image,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::AccessFlags::empty(),
                    vk::AccessFlags::TRANSFER_WRITE,
                );
                let region = vk::BufferImageCopy::builder()
                    .image_subresource(gpu::color_subresource_layers())
                    .image_extent(vk::Extent3D {
                        width: texture.image.extent.width,
                        height: texture.image.extent.height,
                        depth: 1,
                    });
                device.cmd_copy_buffer_to_image(
                    upload.commands.command_buffer,
                    staging.buffer,
                    texture.image.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[*region],
                );
                gpu::barrier(
                    device,
                    upload.commands.command_buffer,
                    texture.image.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::AccessFlags::empty(),
                );
            }
            upload._staging.push(staging);
            upload.textures.push((*id, texture));
        }
        upload.commands.submit(queue, &[], true)?;
        self.dirty.clear();
        self.pending = Some(upload);
        Ok(())
    }

    pub(crate) fn bind(&self, primitives: &mut Vec<egui::ClippedPrimitive>) -> Vec<Arc<Texture>> {
        let mut used = Vec::new();
        primitives.retain_mut(|primitive| {
            let egui::epaint::Primitive::Mesh(mesh) = &mut primitive.primitive else {
                return false;
            };
            let id = mesh.texture_id;
            // Text laid out against a new font atlas would sample garbage from the
            // old one, so skip it until the upload lands.
            if id == egui::TextureId::Managed(0)
                && (self.dirty.contains(&id)
                    || self.pending.as_ref().is_some_and(|pending| {
                        pending
                            .textures
                            .iter()
                            .any(|(pending_id, _)| *pending_id == id)
                    }))
            {
                return false;
            }
            let Some(binding) = self.active.get(&id) else {
                return false;
            };
            mesh.texture_id = binding.user_id;
            if !used
                .iter()
                .any(|texture| Arc::ptr_eq(texture, &binding.texture))
            {
                used.push(Arc::clone(&binding.texture));
            }
            true
        });
        used
    }
}

fn patch(
    target: &mut egui::ColorImage,
    source: &egui::ColorImage,
    [x, y]: [usize; 2],
) -> Result<()> {
    if x + source.width() > target.width() || y + source.height() > target.height() {
        return Err("texture patch out of bounds".to_owned());
    }
    let width = target.width();
    for row in 0..source.height() {
        target.pixels[(y + row) * width + x..(y + row) * width + x + source.width()]
            .copy_from_slice(&source.pixels[row * source.width()..(row + 1) * source.width()]);
    }
    Ok(())
}
