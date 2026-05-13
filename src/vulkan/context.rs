use crate::vulkan::debug_marker::DebugMarker;
use ash::vk;
use raw_window_handle::{DisplayHandle, WindowHandle};
use std::ffi::{CStr, CString, c_char};

pub struct VulkanContext {
    #[allow(dead_code)]
    pub entry: ash::Entry,
    pub instance: ash::Instance,
    pub surface: vk::SurfaceKHR,
    pub surface_loader: ash::khr::surface::Instance,
    pub physical_device: vk::PhysicalDevice,
    pub device: ash::Device,
    pub graphics_queue: vk::Queue,
    pub present_queue: vk::Queue,
    pub graphics_family: u32,
    #[allow(dead_code)]
    pub present_family: u32,
    pub debug_utils: Option<DebugUtils>,
    pub debug_marker: Option<DebugMarker>,
}

pub struct DebugUtils {
    pub loader: ash::ext::debug_utils::Instance,
    pub messenger: vk::DebugUtilsMessengerEXT,
}

impl VulkanContext {
    pub fn new(display: DisplayHandle, window: WindowHandle, enable_validation: bool) -> Self {
        let entry = unsafe { ash::Entry::load() }.unwrap();
        let instance = create_instance(&entry, display, enable_validation);

        let debug_utils = if enable_validation {
            Some(create_debug_utils(&entry, &instance))
        } else {
            None
        };

        let surface = unsafe {
            ash_window::create_surface(&entry, &instance, display.as_raw(), window.as_raw(), None)
                .unwrap()
        };
        let surface_loader = ash::khr::surface::Instance::new(&entry, &instance);

        let (physical_device, graphics_family, present_family) =
            pick_physical_device(&instance, &surface_loader, surface);

        let (device, graphics_queue, present_queue) =
            create_logical_device(&instance, physical_device, graphics_family, present_family);

        let debug_marker = Some(DebugMarker::new(&instance, &device));

        Self {
            entry,
            instance,
            surface,
            surface_loader,
            physical_device,
            device,
            graphics_queue,
            present_queue,
            graphics_family,
            present_family,
            debug_utils,
            debug_marker,
        }
    }
}

impl Drop for VulkanContext {
    fn drop(&mut self) {
        unsafe {
            if let Some(ref du) = self.debug_utils {
                du.loader.destroy_debug_utils_messenger(du.messenger, None);
            }
            self.surface_loader.destroy_surface(self.surface, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

fn create_instance(
    entry: &ash::Entry,
    display: DisplayHandle,
    enable_validation: bool,
) -> ash::Instance {
    let app_name = CString::new("LearnVulkan").unwrap();
    let engine_name = CString::new("NoEngine").unwrap();

    let app_info = vk::ApplicationInfo::default()
        .application_name(&app_name)
        .application_version(vk::make_api_version(0, 1, 0, 0))
        .engine_name(&engine_name)
        .engine_version(vk::make_api_version(0, 1, 0, 0))
        .api_version(vk::API_VERSION_1_3);

    let mut extensions = ash_window::enumerate_required_extensions(display.as_raw())
        .unwrap()
        .to_vec();

    extensions.push(ash::ext::debug_utils::NAME.as_ptr());

    let mut layer_names = Vec::new();
    if enable_validation {
        layer_names.push(c"VK_LAYER_KHRONOS_validation".as_ptr() as *const c_char);
    }

    let create_info = vk::InstanceCreateInfo::default()
        .application_info(&app_info)
        .enabled_extension_names(&extensions)
        .enabled_layer_names(&layer_names);

    unsafe { entry.create_instance(&create_info, None).unwrap() }
}

fn create_debug_utils(entry: &ash::Entry, instance: &ash::Instance) -> DebugUtils {
    let loader = ash::ext::debug_utils::Instance::new(entry, instance);

    let create_info = vk::DebugUtilsMessengerCreateInfoEXT::default()
        .message_severity(
            vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                | vk::DebugUtilsMessageSeverityFlagsEXT::INFO,
        )
        .message_type(
            vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
        )
        .pfn_user_callback(Some(vulkan_debug_callback));

    let messenger = unsafe {
        loader
            .create_debug_utils_messenger(&create_info, None)
            .unwrap()
    };

    DebugUtils { loader, messenger }
}

unsafe extern "system" fn vulkan_debug_callback(
    message_severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    message_type: vk::DebugUtilsMessageTypeFlagsEXT,
    p_callback_data: *const vk::DebugUtilsMessengerCallbackDataEXT,
    _user_data: *mut std::ffi::c_void,
) -> vk::Bool32 {
    let callback_data = unsafe { *p_callback_data };
    let message = unsafe { CStr::from_ptr(callback_data.p_message) }.to_string_lossy();
    println!("[{:?}][{:?}] {}", message_severity, message_type, message);
    vk::FALSE
}

fn pick_physical_device(
    instance: &ash::Instance,
    surface_loader: &ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,
) -> (vk::PhysicalDevice, u32, u32) {
    let devices = unsafe { instance.enumerate_physical_devices().unwrap() };

    for &device in &devices {
        let props = unsafe { instance.get_physical_device_properties(device) };
        let queue_families =
            unsafe { instance.get_physical_device_queue_family_properties(device) };

        let mut graphics_family = None;
        let mut present_family = None;

        for (i, family) in queue_families.iter().enumerate() {
            let i = i as u32;
            if family.queue_flags.contains(vk::QueueFlags::GRAPHICS) {
                graphics_family = Some(i);
            }
            let present_support = unsafe {
                surface_loader
                    .get_physical_device_surface_support(device, i, surface)
                    .unwrap()
            };
            if present_support {
                present_family = Some(i);
            }
        }

        let extensions = unsafe {
            instance
                .enumerate_device_extension_properties(device)
                .unwrap()
        };
        let has_swapchain = extensions.iter().any(|ext| {
            let name = unsafe { CStr::from_ptr(ext.extension_name.as_ptr()) };
            name == ash::khr::swapchain::NAME
        });

        if let (Some(g), Some(p)) = (graphics_family, present_family) {
            if has_swapchain {
                println!(
                    "Selected device: {}",
                    unsafe { CStr::from_ptr(props.device_name.as_ptr()) }.to_string_lossy()
                );
                return (device, g, p);
            }
        }
    }

    panic!("No suitable physical device found");
}

fn create_logical_device(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    graphics_family: u32,
    present_family: u32,
) -> (ash::Device, vk::Queue, vk::Queue) {
    let unique_families: std::collections::HashSet<u32> =
        [graphics_family, present_family].iter().copied().collect();

    let queue_priorities = [1.0f32];
    let queue_create_infos: Vec<_> = unique_families
        .iter()
        .map(|&family| {
            vk::DeviceQueueCreateInfo::default()
                .queue_family_index(family)
                .queue_priorities(&queue_priorities)
        })
        .collect();

    let device_extensions = [ash::khr::swapchain::NAME.as_ptr()];

    let create_info = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queue_create_infos)
        .enabled_extension_names(&device_extensions);

    let device = unsafe {
        instance
            .create_device(physical_device, &create_info, None)
            .unwrap()
    };

    let graphics_queue = unsafe { device.get_device_queue(graphics_family, 0) };
    let present_queue = unsafe { device.get_device_queue(present_family, 0) };

    (device, graphics_queue, present_queue)
}
