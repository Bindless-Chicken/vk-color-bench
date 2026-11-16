use crate::input::InputState;
use crate::instance::{InstanceContext, InstanceDispatch};
use crate::queues::TransferQueue;
use crate::renderer::{FrameRenderer, PresentFrame, SwapchainTarget};
use crate::swapchain::UsagePlan;
use crate::ui::UiContext;
use ash::prelude::VkResult;
use ash::vk::{self, Handle};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::{Arc, Mutex, MutexGuard};
use vulkan_layer::{
    DeviceHooks, DeviceInfo, Global, InstanceInfo, Layer, LayerManifest, LayerResult,
    LayerVulkanCommand, StubGlobalHooks, auto_deviceinfo_impl, declare_introspection_queries,
};

#[cfg(windows)]
use crate::input::update_window_capture;

static LOGGER: Lazy<()> = Lazy::new(|| {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .try_init();
});

mod metadata {
    include!(concat!(env!("OUT_DIR"), "/layer_metadata.rs"));
}

struct SwapchainState {
    surface: vk::SurfaceKHR,
    format: vk::Format,
    color_space: vk::ColorSpaceKHR,
    extent: vk::Extent2D,
    images: Vec<vk::Image>,
    can_capture: bool,
    can_overlay: bool,
    window: Option<isize>,
    renderer: Option<FrameRenderer>,
}

#[derive(Default)]
struct ApplicationState {
    egui: UiContext,
    input: InputState,
    active_swapchain: Option<vk::SwapchainKHR>,
}

impl ApplicationState {
    fn activate(&mut self, swapchain: vk::SwapchainKHR) {
        if self.active_swapchain != Some(swapchain) {
            self.egui.reset_analysis();
            self.active_swapchain = Some(swapchain);
        }
    }

    fn deactivate(&mut self, swapchain: vk::SwapchainKHR) -> bool {
        if self.active_swapchain != Some(swapchain) {
            return false;
        }
        self.active_swapchain = None;
        self.egui.reset_analysis();
        true
    }
}

#[derive(Default)]
pub(crate) struct DeviceState {
    swapchains: HashMap<vk::SwapchainKHR, Arc<Mutex<SwapchainState>>>,
    applications: HashMap<vk::SurfaceKHR, Arc<Mutex<ApplicationState>>>,
    queue_families: HashMap<vk::Queue, u32>,
    warned_multi_swapchain: bool,
}

impl DeviceState {
    pub(crate) fn retire_surface(&mut self, surface: vk::SurfaceKHR) {
        self.applications.remove(&surface);
    }
}

struct DeviceContext {
    vk_device: Arc<ash::Device>,
    device_handle: vk::Device,
    physical_device: vk::PhysicalDevice,
    instance: Option<Arc<ash::Instance>>,
    swapchain_fn: vk::KhrSwapchainFn,
    surface_fn: Option<vk::KhrSurfaceFn>,
    surface_windows: Option<Arc<Mutex<HashMap<vk::SurfaceKHR, isize>>>>,
    state: Arc<Mutex<DeviceState>>,
    transfer: Option<Arc<TransferQueue>>,
}

impl DeviceContext {
    fn new(
        physical_device: vk::PhysicalDevice,
        instance_dispatch: Option<&InstanceDispatch>,
        vk_device: Arc<ash::Device>,
        next_get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr,
    ) -> Self {
        let device_handle = vk_device.handle();
        let swapchain_fn = vk::KhrSwapchainFn::load(|name| unsafe {
            next_get_device_proc_addr(device_handle, name.as_ptr())
                .map_or(ptr::null(), |function| {
                    function as *const () as *const c_void
                })
        });
        let surface_fn = instance_dispatch.map(|dispatch| {
            let instance_handle = dispatch.instance.handle();
            vk::KhrSurfaceFn::load(|name| unsafe {
                (dispatch.next_get_instance_proc_addr)(instance_handle, name.as_ptr())
                    .map_or(ptr::null(), |function| {
                        function as *const () as *const c_void
                    })
            })
        });

        let transfer = crate::queues::get(&vk_device);
        if transfer.is_none() {
            log::info!("no private transfer queue; using nonblocking shared-queue fallback");
        }
        let state = Arc::new(Mutex::new(DeviceState::default()));
        if let Some(dispatch) = instance_dispatch {
            dispatch.device_states.register(&state);
        }
        Self {
            transfer,
            physical_device,
            instance: instance_dispatch.map(|dispatch| Arc::clone(&dispatch.instance)),
            vk_device,
            device_handle,
            swapchain_fn,
            surface_fn,
            surface_windows: instance_dispatch
                .map(|dispatch| Arc::clone(&dispatch.surface_windows)),
            state,
        }
    }

    fn state(&self) -> MutexGuard<'_, DeviceState> {
        self.state.lock().unwrap()
    }

    fn supported_usage_flags(&self, surface: vk::SurfaceKHR) -> VkResult<vk::ImageUsageFlags> {
        let Some(surface_fn) = &self.surface_fn else {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        };
        let mut capabilities = MaybeUninit::<vk::SurfaceCapabilitiesKHR>::uninit();
        let result = unsafe {
            (surface_fn.get_physical_device_surface_capabilities_khr)(
                self.physical_device,
                surface,
                capabilities.as_mut_ptr(),
            )
        };

        if result == vk::Result::SUCCESS {
            Ok(unsafe { capabilities.assume_init() }.supported_usage_flags)
        } else {
            Err(result)
        }
    }

    fn create_swapchain(
        &self,
        create_info: &vk::SwapchainCreateInfoKHR,
        allocator: Option<&vk::AllocationCallbacks>,
    ) -> VkResult<vk::SwapchainKHR> {
        let mut swapchain = MaybeUninit::<vk::SwapchainKHR>::uninit();
        let result = unsafe {
            (self.swapchain_fn.create_swapchain_khr)(
                self.device_handle,
                create_info,
                allocator.map_or(ptr::null(), |value| value),
                swapchain.as_mut_ptr(),
            )
        };

        if result == vk::Result::SUCCESS {
            Ok(unsafe { swapchain.assume_init() })
        } else {
            Err(result)
        }
    }

    fn get_swapchain_images(&self, swapchain: vk::SwapchainKHR) -> VkResult<Vec<vk::Image>> {
        loop {
            let mut image_count = 0;
            let result = unsafe {
                (self.swapchain_fn.get_swapchain_images_khr)(
                    self.device_handle,
                    swapchain,
                    &mut image_count,
                    ptr::null_mut(),
                )
            };
            if result != vk::Result::SUCCESS {
                return Err(result);
            }
            if image_count == 0 {
                return Ok(Vec::new());
            }

            let mut images = vec![vk::Image::null(); image_count as usize];
            let result = unsafe {
                (self.swapchain_fn.get_swapchain_images_khr)(
                    self.device_handle,
                    swapchain,
                    &mut image_count,
                    images.as_mut_ptr(),
                )
            };
            if result == vk::Result::SUCCESS {
                images.truncate(image_count as usize);
                return Ok(images);
            }
            if result != vk::Result::INCOMPLETE {
                return Err(result);
            }
        }
    }

    fn destroy_swapchain(
        &self,
        swapchain: vk::SwapchainKHR,
        allocator: Option<&vk::AllocationCallbacks>,
    ) {
        unsafe {
            (self.swapchain_fn.destroy_swapchain_khr)(
                self.device_handle,
                swapchain,
                allocator.map_or(ptr::null(), |value| value),
            )
        }
    }
}

#[auto_deviceinfo_impl]
impl DeviceHooks for DeviceContext {
    fn get_device_queue(
        &self,
        queue_family_index: u32,
        queue_index: u32,
    ) -> LayerResult<vk::Queue> {
        let queue = unsafe {
            self.vk_device
                .get_device_queue(queue_family_index, queue_index)
        };
        self.state()
            .queue_families
            .insert(queue, queue_family_index);
        LayerResult::Handled(queue)
    }

    fn get_device_queue2(&self, queue_info: &vk::DeviceQueueInfo2) -> LayerResult<vk::Queue> {
        let queue = unsafe { self.vk_device.get_device_queue2(queue_info) };
        self.state()
            .queue_families
            .insert(queue, queue_info.queue_family_index);
        LayerResult::Handled(queue)
    }

    fn create_swapchain_khr(
        &self,
        input: &vk::SwapchainCreateInfoKHR,
        allocator: Option<&vk::AllocationCallbacks>,
    ) -> LayerResult<VkResult<vk::SwapchainKHR>> {
        log::info!(
            "intercepting {}x{} {:?} {:?} swapchain",
            input.image_extent.width,
            input.image_extent.height,
            input.image_format,
            input.image_color_space
        );
        if self.surface_fn.is_none() {
            log::error!("no owning instance found for device; swapchain interception disabled");
            return LayerResult::Unhandled;
        };

        let mut create_info = *input;
        let supported_usage = match self.supported_usage_flags(create_info.surface) {
            Ok(flags) => flags,
            Err(error) => {
                log::error!("could not query swapchain surface capabilities: {error:?}");
                return LayerResult::Handled(Err(error));
            }
        };

        let usage = UsagePlan::for_swapchain(
            create_info.image_usage,
            supported_usage,
            create_info.present_mode,
        );
        create_info.image_usage = usage.image_usage;

        if usage.shared_present {
            log::warn!("shared-present swapchain detected; layer usage flags were not added");
        }

        if !usage.can_capture {
            log::warn!("swapchain does not support TRANSFER_SRC; image analysis is disabled");
        }
        if !usage.can_overlay {
            log::warn!("swapchain does not support COLOR_ATTACHMENT; overlay is disabled");
        }

        let swapchain = match self.create_swapchain(&create_info, allocator) {
            Ok(swapchain) => swapchain,
            Err(error) => {
                log::error!("could not create swapchain: {error:?}");
                return LayerResult::Handled(Err(error));
            }
        };

        let images = match self.get_swapchain_images(swapchain) {
            Ok(images) => images,
            Err(error) => {
                log::error!("could not retrieve swapchain images: {error:?}");
                self.destroy_swapchain(swapchain, allocator);
                return LayerResult::Handled(Err(error));
            }
        };

        self.state().swapchains.insert(
            swapchain,
            Arc::new(Mutex::new(SwapchainState {
                surface: create_info.surface,
                format: create_info.image_format,
                color_space: create_info.image_color_space,
                extent: create_info.image_extent,
                images,
                can_capture: usage.can_capture,
                can_overlay: usage.can_overlay
                    && !usage.shared_present
                    && !create_info
                        .flags
                        .contains(vk::SwapchainCreateFlagsKHR::PROTECTED),
                window: self
                    .surface_windows
                    .as_ref()
                    .and_then(|windows| windows.lock().unwrap().get(&create_info.surface).copied()),
                renderer: None,
            })),
        );

        LayerResult::Handled(Ok(swapchain))
    }

    fn destroy_swapchain_khr(
        &self,
        swapchain: vk::SwapchainKHR,
        allocator: Option<&vk::AllocationCallbacks>,
    ) -> LayerResult<()> {
        let Some(swapchain_state) = self.state().swapchains.remove(&swapchain) else {
            return LayerResult::Unhandled;
        };

        let mut swapchain_state = swapchain_state.lock().unwrap();
        if let Some(renderer) = swapchain_state.renderer.take() {
            renderer.destroy(&self.vk_device);
        }
        let application = self
            .state()
            .applications
            .get(&swapchain_state.surface)
            .cloned();
        let is_active = application.as_ref().is_none_or(|application| {
            let mut application = application.lock().unwrap();
            application.deactivate(swapchain)
        });
        #[cfg(windows)]
        if is_active && let Some(window) = swapchain_state.window {
            update_window_capture(window, false, false);
        }
        #[cfg(not(windows))]
        let _ = is_active;
        self.destroy_swapchain(swapchain, allocator);

        LayerResult::Handled(())
    }

    fn queue_present_khr(
        &self,
        queue: vk::Queue,
        present_info: &vk::PresentInfoKHR,
    ) -> LayerResult<VkResult<()>> {
        if present_info.swapchain_count == 0 {
            return LayerResult::Unhandled;
        }

        // We consume the app's wait semaphores in our own submit, which can't be
        // done once per swapchain.
        if present_info.swapchain_count != 1 {
            let mut state = self.state();
            if !state.warned_multi_swapchain {
                log::warn!(
                    "multi-swapchain presentation is not supported; presenting without overlay"
                );
                state.warned_multi_swapchain = true;
            }
            return LayerResult::Unhandled;
        }

        let swapchains = unsafe {
            std::slice::from_raw_parts(
                present_info.p_swapchains,
                present_info.swapchain_count as usize,
            )
        };
        let image_indices = unsafe {
            std::slice::from_raw_parts(
                present_info.p_image_indices,
                present_info.swapchain_count as usize,
            )
        };

        let swapchain = swapchains[0];
        let image_index = image_indices[0] as usize;
        let wait_semaphores = if present_info.wait_semaphore_count == 0 {
            &[]
        } else {
            unsafe {
                std::slice::from_raw_parts(
                    present_info.p_wait_semaphores,
                    present_info.wait_semaphore_count as usize,
                )
            }
        };

        let signal_semaphore = {
            let (queue_family, swapchain_state) = {
                let Ok(state) = self.state.try_lock() else {
                    return LayerResult::Unhandled;
                };
                let Some(&family) = state.queue_families.get(&queue) else {
                    return LayerResult::Unhandled;
                };
                let Some(swapchain) = state.swapchains.get(&swapchain).cloned() else {
                    return LayerResult::Unhandled;
                };
                (family, swapchain)
            };
            let Ok(mut guard) = swapchain_state.try_lock() else {
                return LayerResult::Unhandled;
            };
            let swapchain_state = &mut *guard;
            if !swapchain_state.can_overlay {
                return LayerResult::Unhandled;
            }
            if image_index >= swapchain_state.images.len() {
                log::error!(
                    "invalid image index {image_index} for swapchain {:?}",
                    swapchain.as_raw()
                );
                return LayerResult::Unhandled;
            }

            if swapchain_state.renderer.is_none() {
                let Some(instance) = &self.instance else {
                    log::error!("cannot initialize renderer without an owning Vulkan instance");
                    return LayerResult::Unhandled;
                };
                match FrameRenderer::new(
                    instance,
                    self.physical_device,
                    &self.vk_device,
                    queue_family,
                    SwapchainTarget {
                        format: swapchain_state.format,
                        color_space: swapchain_state.color_space,
                        extent: swapchain_state.extent,
                        images: &swapchain_state.images,
                        can_capture: swapchain_state.can_capture,
                    },
                    self.transfer.clone(),
                ) {
                    Ok(renderer) => {
                        log::info!(
                            "renderer initialized for {}x{} {:?} swapchain",
                            swapchain_state.extent.width,
                            swapchain_state.extent.height,
                            swapchain_state.format
                        );
                        swapchain_state.renderer = Some(renderer);
                    }
                    Err(error) => {
                        log::error!("could not initialize renderer: {error}");
                        swapchain_state.can_overlay = false;
                        #[cfg(windows)]
                        if let Some(window) = swapchain_state.window {
                            update_window_capture(window, false, false);
                        }
                        return LayerResult::Unhandled;
                    }
                }
            }

            if swapchain_state.renderer.as_ref().unwrap().queue_family() != queue_family {
                return LayerResult::Unhandled;
            }
            let application = {
                let Ok(mut state) = self.state.try_lock() else {
                    return LayerResult::Unhandled;
                };
                Arc::clone(
                    state
                        .applications
                        .entry(swapchain_state.surface)
                        .or_default(),
                )
            };
            let Ok(mut application) = application.try_lock() else {
                return LayerResult::Unhandled;
            };
            application.activate(swapchain);
            let ApplicationState { egui, input, .. } = &mut *application;
            match swapchain_state.renderer.as_mut().unwrap().render(
                egui,
                input,
                PresentFrame {
                    device: &self.vk_device,
                    queue,
                    image_index,
                    image: swapchain_state.images[image_index],
                    extent: swapchain_state.extent,
                    window: swapchain_state.window,
                    wait_semaphores,
                },
            ) {
                Ok(Some(semaphore)) => semaphore,
                Ok(None) => return LayerResult::Unhandled,
                Err(error) => {
                    log::error!("overlay disabled after render error: {error}");
                    swapchain_state.can_overlay = false;
                    #[cfg(windows)]
                    if let Some(window) = swapchain_state.window {
                        update_window_capture(window, false, false);
                    }
                    return LayerResult::Unhandled;
                }
            }
        };

        let layer_wait_semaphores = [signal_semaphore];
        let mut layer_present_info = *present_info;
        layer_present_info.wait_semaphore_count = 1;
        layer_present_info.p_wait_semaphores = layer_wait_semaphores.as_ptr();
        let result = unsafe { (self.swapchain_fn.queue_present_khr)(queue, &layer_present_info) };
        if result != vk::Result::SUCCESS && result != vk::Result::SUBOPTIMAL_KHR {
            log::error!("presentation failed: {result:?}");
        }
        LayerResult::Handled(if result == vk::Result::SUCCESS {
            Ok(())
        } else {
            Err(result)
        })
    }
}

#[derive(Default)]
struct ColorBenchLayer {
    stub_global_hooks: StubGlobalHooks,
    physical_device_instances: Arc<Mutex<HashMap<vk::PhysicalDevice, InstanceDispatch>>>,
}

impl Layer for ColorBenchLayer {
    type GlobalHooksInfo = StubGlobalHooks;
    type InstanceInfo = InstanceContext;
    type DeviceInfo = DeviceContext;
    type InstanceInfoContainer = InstanceContext;
    type DeviceInfoContainer = DeviceContext;

    fn manifest() -> LayerManifest {
        let mut manifest = LayerManifest::default();
        manifest.name = metadata::NAME;
        manifest.spec_version = metadata::API_VERSION;
        manifest.implementation_version = metadata::IMPLEMENTATION_VERSION;
        manifest.description = metadata::DESCRIPTION;
        manifest
    }

    fn global_instance() -> impl std::ops::Deref<Target = Global<Self>> + 'static {
        Lazy::force(&LOGGER);
        static GLOBAL: Lazy<Global<ColorBenchLayer>> = Lazy::new(Default::default);
        &*GLOBAL
    }

    fn global_hooks_info(&self) -> &Self::GlobalHooksInfo {
        &self.stub_global_hooks
    }

    fn create_instance_info(
        &self,
        _: &vk::InstanceCreateInfo,
        _: Option<&vk::AllocationCallbacks>,
        instance: Arc<ash::Instance>,
        next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    ) -> Self::InstanceInfoContainer {
        log::debug!("initializing Color Bench instance state");
        InstanceContext::new(
            instance,
            next_get_instance_proc_addr,
            Arc::clone(&self.physical_device_instances),
        )
    }

    fn create_device_info(
        &self,
        physical_device: vk::PhysicalDevice,
        _: &vk::DeviceCreateInfo,
        _: Option<&vk::AllocationCallbacks>,
        vk_device: Arc<ash::Device>,
        next_get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr,
    ) -> Self::DeviceInfoContainer {
        let instance_dispatch = self
            .physical_device_instances
            .lock()
            .unwrap()
            .get(&physical_device)
            .cloned();

        if instance_dispatch.is_none() {
            log::error!("physical device was not registered during device creation");
        }

        DeviceContext::new(
            physical_device,
            instance_dispatch.as_ref(),
            vk_device,
            next_get_device_proc_addr,
        )
    }

    fn hooked_device_commands(
        &self,
        _instance_info: &Self::InstanceInfo,
        _device_info: Option<&Self::DeviceInfo>,
    ) -> Box<dyn Iterator<Item = LayerVulkanCommand>> {
        Box::new(Self::DeviceInfo::hooked_commands().iter().cloned())
    }

    fn hooked_instance_commands(
        &self,
        _instance_info: &Self::InstanceInfo,
    ) -> Box<dyn Iterator<Item = LayerVulkanCommand>> {
        Box::new(Self::InstanceInfo::hooked_commands().iter().cloned())
    }
}

declare_introspection_queries!(Global::<ColorBenchLayer>);

impl Drop for DeviceContext {
    fn drop(&mut self) {
        crate::queues::unregister(self.device_handle);
    }
}
