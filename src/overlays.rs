use crate::gpu;
use crate::gpu::Image;
use ash::vk;

use crate::ui::OverlayMode;

pub(crate) struct OverlayRenderer {
    device: ash::Device,
    render_pass: vk::RenderPass,
    target_sampling_copy: Image,
    image_view: vk::ImageView,
    sampler: vk::Sampler,
    descriptor_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set: vk::DescriptorSet,
    pipeline_layout: vk::PipelineLayout,
    clipping_pipeline: vk::Pipeline,
    channels_pipeline: vk::Pipeline,
    gamut_pipeline: vk::Pipeline,
    luminance_pipeline: vk::Pipeline,
}

impl OverlayRenderer {
    pub(crate) fn new(
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        device: &ash::Device,
        target_format: vk::Format,
        target_extent: vk::Extent3D,
        families: [u32; 2],
    ) -> gpu::Result<Self> {
        let properties = unsafe {
            instance.get_physical_device_format_properties(physical_device, target_format)
        };
        if !properties
            .optimal_tiling_features
            .contains(vk::FormatFeatureFlags::SAMPLED_IMAGE | vk::FormatFeatureFlags::TRANSFER_DST)
        {
            return Err(format!(
                "format {target_format:?} cannot be copied and sampled"
            ));
        }
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };
        let target_sampling_copy = Image::new(
            device,
            &memory_properties,
            vk::Extent2D {
                width: target_extent.width,
                height: target_extent.height,
            },
            target_format,
            vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
            families,
        )?;
        let render_pass = {
            let attachment = vk::AttachmentDescription::builder()
                .format(target_format)
                .samples(vk::SampleCountFlags::TYPE_1)
                .load_op(vk::AttachmentLoadOp::DONT_CARE)
                .store_op(vk::AttachmentStoreOp::STORE)
                .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .final_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .build();

            let color_attachment_ref = vk::AttachmentReference::builder()
                .attachment(0)
                .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .build();

            let subpass = vk::SubpassDescription::builder()
                .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
                .color_attachments(std::slice::from_ref(&color_attachment_ref))
                .build();

            let dependency = vk::SubpassDependency::builder()
                .src_subpass(vk::SUBPASS_EXTERNAL)
                .dst_subpass(0)
                .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .dst_access_mask(
                    vk::AccessFlags::COLOR_ATTACHMENT_READ
                        | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                )
                .build();

            let render_pass_info = vk::RenderPassCreateInfo::builder()
                .attachments(std::slice::from_ref(&attachment))
                .subpasses(std::slice::from_ref(&subpass))
                .dependencies(std::slice::from_ref(&dependency));

            unsafe { device.create_render_pass(&render_pass_info, None) }
                .map_err(gpu::vulkan_error)?
        };

        let mut renderer = Self {
            device: device.clone(),
            render_pass,
            target_sampling_copy,
            image_view: vk::ImageView::null(),
            sampler: vk::Sampler::null(),
            descriptor_layout: vk::DescriptorSetLayout::null(),
            descriptor_pool: vk::DescriptorPool::null(),
            descriptor_set: vk::DescriptorSet::null(),
            pipeline_layout: vk::PipelineLayout::null(),
            clipping_pipeline: vk::Pipeline::null(),
            channels_pipeline: vk::Pipeline::null(),
            gamut_pipeline: vk::Pipeline::null(),
            luminance_pipeline: vk::Pipeline::null(),
        };
        renderer.create_pipeline(target_format)?;
        Ok(renderer)
    }

    fn create_pipeline(&mut self, format: vk::Format) -> gpu::Result<()> {
        let device = &self.device;
        unsafe {
            self.image_view = device
                .create_image_view(
                    &vk::ImageViewCreateInfo::builder()
                        .image(self.target_sampling_copy.image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(format)
                        .subresource_range(gpu::color_subresource_range()),
                    None,
                )
                .map_err(gpu::vulkan_error)?;
            self.sampler = device
                .create_sampler(
                    &vk::SamplerCreateInfo::builder()
                        .mag_filter(vk::Filter::NEAREST)
                        .min_filter(vk::Filter::NEAREST)
                        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
                    None,
                )
                .map_err(gpu::vulkan_error)?;
            let bindings = [vk::DescriptorSetLayoutBinding::builder()
                .binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)
                .build()];
            self.descriptor_layout = device
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::builder().bindings(&bindings),
                    None,
                )
                .map_err(gpu::vulkan_error)?;
            let sizes = [vk::DescriptorPoolSize {
                ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                descriptor_count: 1,
            }];
            self.descriptor_pool = device
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::builder()
                        .max_sets(1)
                        .pool_sizes(&sizes),
                    None,
                )
                .map_err(gpu::vulkan_error)?;
            let layouts = [self.descriptor_layout];
            self.descriptor_set = device
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::builder()
                        .descriptor_pool(self.descriptor_pool)
                        .set_layouts(&layouts),
                )
                .map_err(gpu::vulkan_error)?[0];
            let images = [vk::DescriptorImageInfo {
                sampler: self.sampler,
                image_view: self.image_view,
                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            }];
            device.update_descriptor_sets(
                &[vk::WriteDescriptorSet::builder()
                    .dst_set(self.descriptor_set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&images)
                    .build()],
                &[],
            );
            self.pipeline_layout = device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::builder()
                        .set_layouts(&layouts)
                        .push_constant_ranges(&[vk::PushConstantRange {
                            stage_flags: vk::ShaderStageFlags::FRAGMENT,
                            offset: 0,
                            size: std::mem::size_of::<u32>() as u32,
                        }]),
                    None,
                )
                .map_err(gpu::vulkan_error)?;
        }

        self.clipping_pipeline = self.create_overlay_pipeline(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/clipping.frag.spv"
        )))?;
        self.channels_pipeline = self.create_overlay_pipeline(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/channels.frag.spv"
        )))?;
        self.gamut_pipeline = self
            .create_overlay_pipeline(include_bytes!(concat!(env!("OUT_DIR"), "/gamut.frag.spv")))?;
        self.luminance_pipeline = self.create_overlay_pipeline(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/luminance.frag.spv"
        )))?;
        Ok(())
    }

    fn create_overlay_pipeline(&self, fragment_code: &[u8]) -> gpu::Result<vk::Pipeline> {
        let device = &self.device;
        let vertex = load_shader(
            device,
            include_bytes!(concat!(env!("OUT_DIR"), "/fullscreen.vert.spv")),
        )?;
        let fragment = match load_shader(device, fragment_code) {
            Ok(module) => module,
            Err(error) => {
                unsafe { device.destroy_shader_module(vertex, None) };
                return Err(error);
            }
        };
        let stages = [
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vertex)
                .name(c"main")
                .build(),
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fragment)
                .name(c"main")
                .build(),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::builder()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = vk::PipelineViewportStateCreateInfo::builder()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::builder()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::builder()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let attachments = [vk::PipelineColorBlendAttachmentState::builder()
            .color_write_mask(vk::ColorComponentFlags::RGBA)
            .build()];
        let blend = vk::PipelineColorBlendStateCreateInfo::builder().attachments(&attachments);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::builder().dynamic_states(&dynamic_states);
        let info = vk::GraphicsPipelineCreateInfo::builder()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(self.pipeline_layout)
            .render_pass(self.render_pass)
            .subpass(0);
        unsafe {
            let result =
                device.create_graphics_pipelines(vk::PipelineCache::null(), &[*info], None);
            device.destroy_shader_module(vertex, None);
            device.destroy_shader_module(fragment, None);
            match result {
                Ok(pipelines) => Ok(pipelines[0]),
                Err((pipelines, error)) => {
                    for pipeline in pipelines {
                        device.destroy_pipeline(pipeline, None);
                    }
                    Err(gpu::vulkan_error(error))
                }
            }
        }
    }

    pub(crate) fn render(
        &mut self,
        device: &ash::Device,
        command_buffer: &vk::CommandBuffer,
        frame_buffer_target: vk::Image,
        frame_buffer: &vk::Framebuffer,
        extent: vk::Extent2D,
        overlay_mode: OverlayMode,
    ) {
        unsafe {
            gpu::barrier(
                device,
                *command_buffer,
                frame_buffer_target,
                vk::ImageLayout::PRESENT_SRC_KHR,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::AccessFlags::MEMORY_WRITE,
                vk::AccessFlags::TRANSFER_READ,
            );
            gpu::barrier(
                device,
                *command_buffer,
                self.target_sampling_copy.image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::PipelineStageFlags::TRANSFER,
                vk::AccessFlags::empty(),
                vk::AccessFlags::TRANSFER_WRITE,
            );

            device.cmd_copy_image(
                *command_buffer,
                frame_buffer_target,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.target_sampling_copy.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::ImageCopy::builder()
                    .src_subresource(gpu::color_subresource_layers())
                    .dst_subresource(gpu::color_subresource_layers())
                    .extent(vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    })
                    .build()],
            );

            gpu::barrier(
                device,
                *command_buffer,
                frame_buffer_target,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags::TRANSFER_READ,
                vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            );
            gpu::barrier(
                device,
                *command_buffer,
                self.target_sampling_copy.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::SHADER_READ,
            );

            device.cmd_begin_render_pass(
                *command_buffer,
                &vk::RenderPassBeginInfo::builder()
                    .render_pass(self.render_pass)
                    .framebuffer(*frame_buffer)
                    .render_area(vk::Rect2D {
                        offset: vk::Offset2D::default(),
                        extent,
                    })
                    .build(),
                vk::SubpassContents::INLINE,
            );

            let pipeline = match overlay_mode {
                OverlayMode::Clipping => self.clipping_pipeline,
                OverlayMode::Gamut => self.gamut_pipeline,
                OverlayMode::Luminance => self.luminance_pipeline,
                OverlayMode::Channels(channel) => {
                    device.cmd_push_constants(
                        *command_buffer,
                        self.pipeline_layout,
                        vk::ShaderStageFlags::FRAGMENT,
                        0,
                        &(channel as u32).to_ne_bytes(),
                    );
                    self.channels_pipeline
                }
            };
            device.cmd_bind_pipeline(*command_buffer, vk::PipelineBindPoint::GRAPHICS, pipeline);
            device.cmd_bind_descriptor_sets(
                *command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline_layout,
                0,
                &[self.descriptor_set],
                &[],
            );
            device.cmd_set_viewport(
                *command_buffer,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: extent.width as f32,
                    height: extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            device.cmd_set_scissor(
                *command_buffer,
                0,
                &[vk::Rect2D {
                    offset: vk::Offset2D::default(),
                    extent,
                }],
            );
            device.cmd_draw(*command_buffer, 3, 1, 0, 0);
            device.cmd_end_render_pass(*command_buffer);
        }
    }
}

fn load_shader(device: &ash::Device, bytes: &[u8]) -> gpu::Result<vk::ShaderModule> {
    let code =
        ash::util::read_spv(&mut std::io::Cursor::new(bytes)).map_err(|error| error.to_string())?;
    unsafe { device.create_shader_module(&vk::ShaderModuleCreateInfo::builder().code(&code), None) }
        .map_err(gpu::vulkan_error)
}

impl Drop for OverlayRenderer {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_pipeline(self.clipping_pipeline, None);
            self.device.destroy_pipeline(self.channels_pipeline, None);
            self.device.destroy_pipeline(self.gamut_pipeline, None);
            self.device.destroy_pipeline(self.luminance_pipeline, None);
            self.device
                .destroy_pipeline_layout(self.pipeline_layout, None);
            self.device
                .destroy_descriptor_pool(self.descriptor_pool, None);
            self.device
                .destroy_descriptor_set_layout(self.descriptor_layout, None);
            self.device.destroy_sampler(self.sampler, None);
            self.device.destroy_image_view(self.image_view, None);
            self.device.destroy_render_pass(self.render_pass, None);
        }
    }
}
