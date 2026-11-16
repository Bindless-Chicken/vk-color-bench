use ash::vk;

const REQUIRED_USAGE: vk::ImageUsageFlags = vk::ImageUsageFlags::from_raw(
    vk::ImageUsageFlags::TRANSFER_SRC.as_raw() | vk::ImageUsageFlags::COLOR_ATTACHMENT.as_raw(),
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UsagePlan {
    pub(crate) image_usage: vk::ImageUsageFlags,
    pub(crate) shared_present: bool,
    pub(crate) can_capture: bool,
    pub(crate) can_overlay: bool,
}

impl UsagePlan {
    pub(crate) fn for_swapchain(
        requested: vk::ImageUsageFlags,
        supported: vk::ImageUsageFlags,
        present_mode: vk::PresentModeKHR,
    ) -> Self {
        let shared_present = matches!(
            present_mode,
            vk::PresentModeKHR::SHARED_DEMAND_REFRESH
                | vk::PresentModeKHR::SHARED_CONTINUOUS_REFRESH
        );
        let layer_usage = if shared_present {
            vk::ImageUsageFlags::empty()
        } else {
            supported & REQUIRED_USAGE
        };
        let image_usage = requested | layer_usage;

        Self {
            image_usage,
            shared_present,
            can_capture: image_usage.contains(vk::ImageUsageFlags::TRANSFER_SRC),
            can_overlay: image_usage.contains(vk::ImageUsageFlags::COLOR_ATTACHMENT),
        }
    }
}
