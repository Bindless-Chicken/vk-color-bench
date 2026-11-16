use ash::vk;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::{Arc, Mutex, Weak};
use vulkan_layer::{InstanceHooks, InstanceInfo, LayerResult, LayerVulkanCommand};

#[derive(Default)]
pub(crate) struct DeviceStates(Mutex<Vec<Weak<Mutex<crate::layer::DeviceState>>>>);

impl DeviceStates {
    pub(crate) fn register(&self, state: &Arc<Mutex<crate::layer::DeviceState>>) {
        let mut states = self.0.lock().unwrap();
        states.retain(|state| state.strong_count() != 0);
        states.push(Arc::downgrade(state));
    }

    pub(crate) fn retire_surface(&self, surface: vk::SurfaceKHR) {
        let mut states = self.0.lock().unwrap();
        states.retain(|state| {
            let Some(state) = state.upgrade() else {
                return false;
            };
            state.lock().unwrap().retire_surface(surface);
            true
        });
    }
}

#[derive(Clone)]
pub(crate) struct InstanceDispatch {
    pub(crate) device_states: Arc<DeviceStates>,
    pub(crate) instance: Arc<ash::Instance>,
    pub(crate) next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pub(crate) surface_windows: Arc<Mutex<HashMap<vk::SurfaceKHR, isize>>>,
}

pub(crate) struct InstanceContext {
    device_states: Arc<DeviceStates>,
    instance: Arc<ash::Instance>,
    next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    physical_device_instances: Arc<Mutex<HashMap<vk::PhysicalDevice, InstanceDispatch>>>,
    instance_handle: vk::Instance,
    win32_surface_fn: vk::KhrWin32SurfaceFn,
    surface_fn: vk::KhrSurfaceFn,
    pub(crate) surface_windows: Arc<Mutex<HashMap<vk::SurfaceKHR, isize>>>,
}

impl InstanceContext {
    pub(crate) fn new(
        instance: Arc<ash::Instance>,
        next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
        physical_device_instances: Arc<Mutex<HashMap<vk::PhysicalDevice, InstanceDispatch>>>,
    ) -> Self {
        let instance_handle = instance.handle();
        let load = |name: &std::ffi::CStr| unsafe {
            next_get_instance_proc_addr(instance_handle, name.as_ptr())
                .map_or(ptr::null(), |function| {
                    function as *const () as *const c_void
                })
        };

        Self {
            device_states: Arc::default(),
            instance,
            next_get_instance_proc_addr,
            physical_device_instances,
            instance_handle,
            win32_surface_fn: vk::KhrWin32SurfaceFn::load(load),
            surface_fn: vk::KhrSurfaceFn::load(load),
            surface_windows: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl InstanceHooks for InstanceContext {
    fn create_device(
        &self,
        physical_device: vk::PhysicalDevice,
        create_info: &vk::DeviceCreateInfo,
        link: &vulkan_layer::VkLayerDeviceLink,
        allocator: Option<&vk::AllocationCallbacks>,
        output: &mut MaybeUninit<vk::Device>,
    ) -> LayerResult<ash::prelude::VkResult<()>> {
        unsafe {
            let get_instance_proc_addr = link.pfnNextGetInstanceProcAddr;
            let Some(next_create_device) =
                get_instance_proc_addr(self.instance_handle, c"vkCreateDevice".as_ptr())
            else {
                return LayerResult::Handled(Err(vk::Result::ERROR_INITIALIZATION_FAILED));
            };
            let next_create_device: vk::PFN_vkCreateDevice =
                std::mem::transmute(next_create_device);
            let Some(get_queue_family_properties) = get_instance_proc_addr(
                self.instance_handle,
                c"vkGetPhysicalDeviceQueueFamilyProperties".as_ptr(),
            ) else {
                return LayerResult::Unhandled;
            };
            let get_queue_family_properties: vk::PFN_vkGetPhysicalDeviceQueueFamilyProperties =
                std::mem::transmute(get_queue_family_properties);
            let mut family_count = 0;
            get_queue_family_properties(physical_device, &mut family_count, ptr::null_mut());
            let mut families = vec![vk::QueueFamilyProperties::default(); family_count as usize];
            get_queue_family_properties(physical_device, &mut family_count, families.as_mut_ptr());
            families.truncate(family_count as usize);
            let requested = std::slice::from_raw_parts(
                create_info.p_queue_create_infos,
                create_info.queue_create_info_count as usize,
            );
            let mut reserved =
                if std::env::var("VK_COLOR_BENCH_TRANSFER_QUEUE").as_deref() == Ok("0") {
                    None
                } else {
                    crate::queues::choose(&families, requested)
                };
            let mut next = create_info.p_next as *const vk::BaseInStructure;
            while let Some(node) = next.as_ref() {
                if node.s_type == vk::StructureType::DEVICE_GROUP_DEVICE_CREATE_INFO {
                    reserved = None;
                }
                next = node.p_next;
            }
            let mut queues = requested.to_vec();
            let mut priorities = Vec::new();
            if let Some((family, _)) = reserved {
                if let Some(queue_info) = queues
                    .iter_mut()
                    .find(|queue_info| queue_info.queue_family_index == family)
                {
                    priorities.extend_from_slice(std::slice::from_raw_parts(
                        queue_info.p_queue_priorities,
                        queue_info.queue_count as usize,
                    ));
                    priorities.push(0.0);
                    queue_info.queue_count += 1;
                    queue_info.p_queue_priorities = priorities.as_ptr();
                } else {
                    priorities.push(0.0);
                    queues.push(
                        *vk::DeviceQueueCreateInfo::builder()
                            .queue_family_index(family)
                            .queue_priorities(&priorities),
                    );
                }
            }
            let mut modified_info = *create_info;
            modified_info.queue_create_info_count = queues.len() as u32;
            modified_info.p_queue_create_infos = queues.as_ptr();
            let links = crate::queues::DeviceLinks::save(create_info);
            let mut result = next_create_device(
                physical_device,
                &modified_info,
                allocator.map_or(ptr::null(), |value| value),
                output.as_mut_ptr(),
            );
            if result != vk::Result::SUCCESS && reserved.is_some() {
                log::warn!(
                    "extra transfer queue request failed ({result:?}); retrying original device request"
                );
                links.restore();
                reserved = None;
                result = next_create_device(
                    physical_device,
                    create_info,
                    allocator.map_or(ptr::null(), |value| value),
                    output.as_mut_ptr(),
                );
            }
            // create_device_info runs right after this hook and looks the instance up
            // by physical device.
            if result == vk::Result::SUCCESS {
                self.physical_device_instances.lock().unwrap().insert(
                    physical_device,
                    InstanceDispatch {
                        device_states: Arc::clone(&self.device_states),
                        instance: Arc::clone(&self.instance),
                        next_get_instance_proc_addr: self.next_get_instance_proc_addr,
                        surface_windows: Arc::clone(&self.surface_windows),
                    },
                );
                crate::queues::register(output.assume_init(), create_info, reserved);
                LayerResult::Handled(Ok(()))
            } else {
                LayerResult::Handled(Err(result))
            }
        }
    }

    fn create_win32_surface_khr(
        &self,
        create_info: &vk::Win32SurfaceCreateInfoKHR,
        allocator: Option<&vk::AllocationCallbacks>,
    ) -> LayerResult<ash::prelude::VkResult<vk::SurfaceKHR>> {
        let mut surface = MaybeUninit::<vk::SurfaceKHR>::uninit();
        let result = unsafe {
            (self.win32_surface_fn.create_win32_surface_khr)(
                self.instance_handle,
                create_info,
                allocator.map_or(ptr::null(), |value| value),
                surface.as_mut_ptr(),
            )
        };
        if result != vk::Result::SUCCESS {
            log::error!("could not create Win32 surface: {result:?}");
            return LayerResult::Handled(Err(result));
        }

        let surface = unsafe { surface.assume_init() };
        self.surface_windows
            .lock()
            .unwrap()
            .insert(surface, create_info.hwnd as isize);
        #[cfg(windows)]
        if !crate::input::install_window_input(create_info.hwnd as isize) {
            log::error!("could not install Win32 input interception");
        }
        LayerResult::Handled(Ok(surface))
    }

    fn destroy_surface_khr(
        &self,
        surface: vk::SurfaceKHR,
        allocator: Option<&vk::AllocationCallbacks>,
    ) -> LayerResult<()> {
        self.device_states.retire_surface(surface);
        let window = self.surface_windows.lock().unwrap().remove(&surface);
        let Some(window) = window else {
            return LayerResult::Unhandled;
        };
        #[cfg(windows)]
        crate::input::uninstall_window_input(window);
        unsafe {
            (self.surface_fn.destroy_surface_khr)(
                self.instance_handle,
                surface,
                allocator.map_or(ptr::null(), |value| value),
            )
        };
        LayerResult::Handled(())
    }
}

impl Drop for InstanceContext {
    fn drop(&mut self) {
        self.physical_device_instances
            .lock()
            .unwrap()
            .retain(|_, dispatch| dispatch.instance.handle() != self.instance_handle);
    }
}

impl InstanceInfo for InstanceContext {
    type HooksType = Self;
    type HooksRefType<'a> = &'a Self;

    fn hooked_commands() -> &'static [LayerVulkanCommand] {
        &[
            LayerVulkanCommand::CreateDevice,
            LayerVulkanCommand::CreateWin32SurfaceKhr,
            LayerVulkanCommand::DestroySurfaceKhr,
        ]
    }

    fn hooks(&self) -> Self::HooksRefType<'_> {
        self
    }
}
