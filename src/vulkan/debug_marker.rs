use ash::vk;
use std::ffi::CString;

pub struct DebugMarker {
    loader: ash::ext::debug_utils::Device,
}

impl DebugMarker {
    pub fn new(instance: &ash::Instance, device: &ash::Device) -> Self {
        let loader = ash::ext::debug_utils::Device::new(instance, device);
        Self { loader }
    }

    pub unsafe fn begin_label(
        &self,
        command_buffer: vk::CommandBuffer,
        name: &str,
        color: [f32; 4],
    ) {
        let name = CString::new(name).unwrap();
        let label = vk::DebugUtilsLabelEXT::default()
            .label_name(&name)
            .color(color);
        unsafe {
            self.loader
                .cmd_begin_debug_utils_label(command_buffer, &label);
        }
    }

    pub unsafe fn end_label(&self, command_buffer: vk::CommandBuffer) {
        unsafe {
            self.loader.cmd_end_debug_utils_label(command_buffer);
        }
    }

    pub unsafe fn insert_label(
        &self,
        command_buffer: vk::CommandBuffer,
        name: &str,
        color: [f32; 4],
    ) {
        let name = CString::new(name).unwrap();
        let label = vk::DebugUtilsLabelEXT::default()
            .label_name(&name)
            .color(color);
        unsafe {
            self.loader
                .cmd_insert_debug_utils_label(command_buffer, &label);
        }
    }

    pub unsafe fn set_object_name<T: vk::Handle>(&self, object: T, name: &str) {
        let name = CString::new(name).unwrap();
        let name_info = vk::DebugUtilsObjectNameInfoEXT::default()
            .object_handle(object)
            .object_name(&name);
        unsafe {
            self.loader.set_debug_utils_object_name(&name_info).unwrap();
        }
    }
}
