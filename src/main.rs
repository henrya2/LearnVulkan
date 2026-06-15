use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

mod app;
mod camera;
mod input;
mod mesh;
mod scene;
mod vulkan;

use app::App;

struct AppHandler {
    app: Option<App>,
    enable_validation: bool,
    enable_gpu_assisted: bool,
    width: u32,
    height: u32,
    /// Frame counter, decremented on each `RedrawRequested`. When it hits 0
    /// the app exits cleanly. `None` means "run until window close" (the
    /// normal interactive mode).
    frames_remaining: Option<u32>,
}

impl AppHandler {
    fn try_finish_run(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(remaining) = self.frames_remaining {
            if remaining == 0 {
                println!("RUN_FRAMES_DONE");
                event_loop.exit();
            }
        }
    }
}

impl ApplicationHandler for AppHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.app.is_none() {
            // The title is set twice: once here as a placeholder (visible
            // for the first frame at most, before `App::new` runs) and
            // again by `App::new` from the actual current `TonemapOp`.
            // The final, correct title is the one set in `App::new`.
            let attrs = Window::default_attributes()
                .with_title("LearnVulkan - Tonemap: ACES")
                .with_inner_size(winit::dpi::LogicalSize::new(self.width, self.height));
            let window = event_loop.create_window(attrs).unwrap();
            self.app = Some(App::new(
                window,
                self.enable_validation,
                self.enable_gpu_assisted,
            ));
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(app) = self.app.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => app.on_resize(size),
            WindowEvent::RedrawRequested => {
                app.draw_frame();
                app.window().request_redraw();
                if let Some(ref mut remaining) = self.frames_remaining {
                    if *remaining > 0 {
                        *remaining -= 1;
                    }
                }
                self.try_finish_run(event_loop);
            }
            WindowEvent::KeyboardInput { event, .. } => app.on_keyboard(&event),
            WindowEvent::MouseInput { button, state, .. } => app.on_mouse_button(button, state),
            WindowEvent::Focused(false) => app.on_focus_lost(),
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: DeviceEvent,
    ) {
        let Some(app) = self.app.as_mut() else { return };
        if let DeviceEvent::MouseMotion { delta } = event {
            app.on_device_mouse_motion(delta.0, delta.1);
        }
    }
}

fn parse_resolution() -> (u32, u32) {
    let args: Vec<String> = std::env::args().collect();
    for arg in &args {
        if arg.starts_with("--resolution=") {
            let val = arg.strip_prefix("--resolution=").unwrap();
            if let Some((w, h)) = val.split_once('x') {
                return (
                    w.parse().unwrap_or(800),
                    h.parse().unwrap_or(600),
                );
            }
        }
    }
    (800, 600)
}

/// Parse `--run-frames` and `--frames=N` flags. Returns `Some(N)` if either
/// is present, `None` otherwise. `--frames` overrides the bare `--run-frames`
/// count (which defaults to 120 per the test harness convention).
fn parse_run_frames() -> Option<u32> {
    let args: Vec<String> = std::env::args().collect();
    let mut explicit = None;
    let mut has_flag = false;
    for arg in &args {
        if arg == "--run-frames" {
            has_flag = true;
        } else if let Some(v) = arg.strip_prefix("--frames=") {
            explicit = v.parse::<u32>().ok();
        }
    }
    if explicit.is_some() {
        explicit
    } else if has_flag {
        Some(120)
    } else {
        None
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let enable_validation = args
        .iter()
        .any(|arg| arg == "--validation" || arg == "--validate")
        || cfg!(debug_assertions);

    // GPU-assisted validation is opt-in (default off). It requires the
    // validation layer, so the VulkanContext will silently ignore this flag
    // if validation is not enabled.
    let enable_gpu_assisted = args.iter().any(|arg| {
        arg == "--gpu-assisted" || arg == "--gpu_assisted" || arg == "--vgav"
    });

    let (width, height) = parse_resolution();
    let frames_remaining = parse_run_frames();

    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut handler = AppHandler {
        app: None,
        enable_validation,
        enable_gpu_assisted,
        width,
        height,
        frames_remaining,
    };
    event_loop.run_app(&mut handler).unwrap();
}
