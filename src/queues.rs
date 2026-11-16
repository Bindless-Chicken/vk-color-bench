use ash::vk::{self, Handle};
use once_cell::sync::Lazy;
use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{Arc, Mutex},
};

type LoaderData = unsafe extern "C" fn(vk::Device, *mut c_void) -> vk::Result;
#[repr(C)]
union LoaderPayload {
    link: *mut vulkan_layer::VkLayerDeviceLink,
    callback: Option<LoaderData>,
}
#[repr(C)]
// Mirrors VkLayerDeviceCreateInfo from vk_layer.h, which vulkan-layer doesn't expose.
struct LoaderInfo {
    s_type: vk::StructureType,
    next: *const vk::BaseInStructure,
    function: u32,
    payload: LoaderPayload,
}
#[derive(Clone, Copy)]
struct Registration {
    reserved: Option<(u32, u32)>,
    loader: Option<LoaderData>,
}
static DEVICES: Lazy<Mutex<HashMap<vk::Device, Registration>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub(crate) unsafe fn register(
    device: vk::Device,
    info: &vk::DeviceCreateInfo,
    reserved: Option<(u32, u32)>,
) {
    let mut next = info.p_next as *const vk::BaseInStructure;
    let mut loader = None;
    unsafe {
        while let Some(node) = next.as_ref() {
            if node.s_type == vk::StructureType::LOADER_DEVICE_CREATE_INFO {
                let node = &*(next as *const LoaderInfo);
                if node.function == 1 {
                    loader = node.payload.callback;
                }
            }
            next = node.p_next;
        }
    }
    DEVICES
        .lock()
        .unwrap()
        .insert(device, Registration { reserved, loader });
}

// Layers below us advance the chain links during vkCreateDevice, so they
// have to be put back before calling it a second time.
pub(crate) struct DeviceLinks(Vec<(*mut LoaderInfo, *mut vulkan_layer::VkLayerDeviceLink)>);
impl DeviceLinks {
    pub(crate) unsafe fn save(info: &vk::DeviceCreateInfo) -> Self {
        let mut links = Vec::new();
        let mut next = info.p_next as *const vk::BaseInStructure;
        unsafe {
            while let Some(node) = next.as_ref() {
                if node.s_type == vk::StructureType::LOADER_DEVICE_CREATE_INFO {
                    let loader = next as *mut LoaderInfo;
                    if (*loader).function == 0 {
                        links.push((loader, (*loader).payload.link));
                    }
                }
                next = node.p_next;
            }
        }
        Self(links)
    }
    pub(crate) unsafe fn restore(&self) {
        for &(node, link) in &self.0 {
            unsafe {
                (*node).payload.link = link;
            }
        }
    }
}
pub(crate) fn unregister(device: vk::Device) {
    DEVICES.lock().unwrap().remove(&device);
}

// Dispatchable handles we create ourselves need the loader's dispatch data.
pub(crate) fn initialize<T: Handle>(device: &ash::Device, object: T) -> crate::gpu::Result<()> {
    let loader = DEVICES
        .lock()
        .unwrap()
        .get(&device.handle())
        .and_then(|registration| registration.loader);
    if let Some(loader) = loader {
        let result = unsafe { loader(device.handle(), object.as_raw() as usize as *mut c_void) };
        if result != vk::Result::SUCCESS {
            return Err(crate::gpu::vulkan_error(result));
        }
    }
    Ok(())
}

pub(crate) struct TransferQueue {
    pub(crate) queue: vk::Queue,
    pub(crate) family: u32,
    pub(crate) lock: Mutex<()>,
}
pub(crate) fn get(device: &ash::Device) -> Option<Arc<TransferQueue>> {
    let (family, index) = DEVICES.lock().unwrap().get(&device.handle())?.reserved?;
    let queue = unsafe { device.get_device_queue(family, index) };
    if let Err(error) = initialize(device, queue) {
        log::warn!("transfer queue unavailable: {error}");
        return None;
    }
    log::info!("reserved asynchronous transfer queue family={family} index={index}");
    Some(Arc::new(TransferQueue {
        queue,
        family,
        lock: Mutex::new(()),
    }))
}

pub(crate) fn choose(
    properties: &[vk::QueueFamilyProperties],
    requested: &[vk::DeviceQueueCreateInfo],
) -> Option<(u32, u32)> {
    properties
        .iter()
        .enumerate()
        .filter_map(|(family, family_properties)| {
            if !family_properties.queue_flags.intersects(
                vk::QueueFlags::TRANSFER | vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE,
            ) {
                return None;
            }
            let requests: Vec<_> = requested
                .iter()
                .filter(|request| request.queue_family_index == family as u32)
                .collect();
            if requests.len() > 1
                || requests
                    .first()
                    .is_some_and(|request| !request.flags.is_empty())
            {
                return None;
            }
            let count = requests.first().map_or(0, |request| request.queue_count);
            if count >= family_properties.queue_count {
                return None;
            }
            let score = if !family_properties
                .queue_flags
                .intersects(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE)
            {
                0
            } else if !family_properties
                .queue_flags
                .contains(vk::QueueFlags::GRAPHICS)
            {
                1
            } else {
                2
            };
            Some((score, family as u32, count))
        })
        .min_by_key(|candidate| candidate.0)
        .map(|(_, family, index)| (family, index))
}
