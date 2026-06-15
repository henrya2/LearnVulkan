use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::mem::ManuallyDrop;
use std::sync::Arc;
use std::time::Instant;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window};

use crate::camera::Camera;
use crate::input::InputState;
use crate::vulkan::context::VulkanContext;
use crate::vulkan::postprocess::TonemapOp;
use crate::vulkan::renderer::Renderer;

pub struct App {
    pub window: Arc<Window>,
    pub ctx: ManuallyDrop<VulkanContext>,
    pub renderer: ManuallyDrop<Renderer>,
    pub camera: Camera,
    pub input: InputState,
    pub mouse_locked: bool,
    pub last_frame: Instant,
    /// Current tonemap selection. Kept in sync with the renderer's UBO via
    /// `cycle_tonemap`; on startup it is initialized to match the renderer's
    /// default (ACES, per `PostProcessSettings::default`).
    pub current_tonemap: TonemapOp,
}

impl Drop for App {
    fn drop(&mut self) {
        unsafe {
            // 1. Explicitly destroy all renderer resources (requires allocator).
            //    This calls every `destroy(device, allocator)` on the renderer's
            //    owned resources, freeing all `Allocation`s through the allocator
            //    and destroying all Vulkan objects.
            //
            //    Use raw pointers to bypass the borrow checker: the lifetimes
            //    are clear (`renderer` is used before `ctx` is dropped), and
            //    `destroy` does not retain the borrows past its return.
            let renderer_ptr: *mut Renderer = &mut *self.renderer as *mut Renderer;
            let device_ptr: *const ash::Device = &self.ctx.device as *const ash::Device;
            let allocator_ptr: *mut crate::vulkan::memory::MemoryAllocator =
                &mut *self.ctx.allocator as *mut _;
            (*renderer_ptr).destroy(&*device_ptr, &mut *allocator_ptr);
            // 2. Drop the Renderer struct. Renderer's Drop is empty (debug-only
            //    warn if destroy wasn't called).
            ManuallyDrop::drop(&mut self.renderer);
            // 3. Drop VulkanContext. Its Drop manually drops the allocator
            //    before destroying the device.
            ManuallyDrop::drop(&mut self.ctx);
        }
    }
}

impl App {
    pub fn new(
        window: Window,
        enable_validation: bool,
        enable_gpu_assisted: bool,
    ) -> Self {
        let window = Arc::new(window);

        let display = window.display_handle().unwrap();
        let win_handle = window.window_handle().unwrap();

        let mut ctx = VulkanContext::new(display, win_handle, enable_validation, enable_gpu_assisted);

        let size = window.inner_size();
        // Renderer::new needs `&mut ctx` because it allocates buffers/images
        // via the allocator during construction.
        let renderer = Renderer::new(&mut ctx, size.width, size.height);

        let current_tonemap = TonemapOp::Aces;
        // Set the initial window title from the tonemap we just initialised
        // the renderer with. The constructor in `main.rs` only knows a
        // static string; the actual tonemap defaults live in
        // `PostProcessSettings::default`, so the title must be applied
        // here (after both are constructed) to stay in sync.
        let title = format_title(current_tonemap);
        window.set_title(&title);

        Self {
            window,
            ctx: ManuallyDrop::new(ctx),
            renderer: ManuallyDrop::new(renderer),
            camera: Camera::new(),
            input: InputState::new(),
            mouse_locked: false,
            last_frame: Instant::now(),
            current_tonemap,
        }
    }

    pub fn on_resize(&mut self, size: PhysicalSize<u32>) {
        if size.width > 0 && size.height > 0 {
            self.renderer.framebuffer_resized = true;
        }
    }

    pub fn draw_frame(&mut self) {
        let (view, proj, camera_pos) = self.update();
        // draw_frame needs `&mut ctx` because it can trigger a swapchain
        // recreate, which uses the allocator.
        self.renderer.draw_frame(&mut self.ctx, view, proj, camera_pos);
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    pub fn on_keyboard(&mut self, event: &KeyEvent) {
        let pressed = event.state == ElementState::Pressed;
        match event.physical_key {
            PhysicalKey::Code(KeyCode::KeyW) => self.input.forward = pressed,
            PhysicalKey::Code(KeyCode::KeyS) => self.input.back = pressed,
            PhysicalKey::Code(KeyCode::KeyA) => self.input.left = pressed,
            PhysicalKey::Code(KeyCode::KeyD) => self.input.right = pressed,
            PhysicalKey::Code(KeyCode::Space) => self.input.up = pressed,
            PhysicalKey::Code(KeyCode::ShiftLeft) => self.input.down = pressed,
            PhysicalKey::Code(KeyCode::AltLeft) => self.input.alt_down = pressed,
            PhysicalKey::Code(KeyCode::KeyZ) => {
                if pressed && self.input.alt_down && self.mouse_locked {
                    self.set_mouse_lock(false);
                }
            }
            PhysicalKey::Code(KeyCode::KeyT) => {
                if pressed {
                    self.cycle_tonemap();
                }
            }
            _ => {}
        }
    }

    /// Advance the tonemap selection Linear -> Reinhard -> ACES -> Linear and
    /// push the new value to the renderer. The UBO is rewritten every frame,
    /// so the change takes effect on the very next `draw_frame`. The window
    /// title is updated to match so the user can see which operator is
    /// active from the taskbar / window list.
    pub fn cycle_tonemap(&mut self) {
        let next = self.current_tonemap.next();
        self.current_tonemap = next;
        self.renderer.set_tonemap(next);
        let title = format_title(next);
        self.window.set_title(&title);
        eprintln!("[tonemap] -> {}", next);
    }

    pub fn on_mouse_button(&mut self, button: MouseButton, state: ElementState) {
        if button == MouseButton::Left && state == ElementState::Pressed && !self.mouse_locked {
            self.set_mouse_lock(true);
        }
    }

    pub fn on_device_mouse_motion(&mut self, dx: f64, dy: f64) {
        if self.mouse_locked {
            self.input.mouse_delta.0 += dx;
            self.input.mouse_delta.1 += dy;
        }
    }

    pub fn on_focus_lost(&mut self) {
        if self.mouse_locked {
            self.set_mouse_lock(false);
        }
    }

    fn set_mouse_lock(&mut self, locked: bool) {
        self.mouse_locked = locked;
        if locked {
            let mode = self
                .window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| self.window.set_cursor_grab(CursorGrabMode::Confined));
            if mode.is_err() {
                let _ = self.window.set_cursor_grab(CursorGrabMode::Confined);
            }
            self.window.set_cursor_visible(false);
        } else {
            let _ = self.window.set_cursor_grab(CursorGrabMode::None);
            self.window.set_cursor_visible(true);
        }
    }

    fn update(&mut self) -> (glam::Mat4, glam::Mat4, glam::Vec3) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;

        let (dx, dy) = self.input.drain_mouse_delta();
        self.camera.apply_mouse_delta(dx, dy);

        let forward = self.camera.forward();
        let right = self.camera.right();
        let mut move_dir = glam::Vec3::ZERO;
        if self.input.forward {
            move_dir += forward;
        }
        if self.input.back {
            move_dir -= forward;
        }
        if self.input.right {
            move_dir += right;
        }
        if self.input.left {
            move_dir -= right;
        }
        if self.input.up {
            move_dir += glam::Vec3::Y;
        }
        if self.input.down {
            move_dir -= glam::Vec3::Y;
        }
        if move_dir != glam::Vec3::ZERO {
            self.camera.position += move_dir.normalize() * self.camera.move_speed * dt;
        }

        let size = self.window.inner_size();
        let aspect = if size.height > 0 {
            size.width as f32 / size.height as f32
        } else {
            1.0
        };
        (
            self.camera.view_matrix(),
            self.camera.projection_matrix(aspect),
            self.camera.position,
        )
    }
}

/// Build the user-facing window title. Format: "LearnVulkan - Tonemap: <OP>".
/// Used both at startup and on every tonemap switch (T key). Kept as a free
/// function so the format is defined in exactly one place.
fn format_title(op: TonemapOp) -> String {
    format!("LearnVulkan - Tonemap: {}", op)
}
