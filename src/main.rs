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
    width: u32,
    height: u32,
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
            self.app = Some(App::new(window, self.enable_validation));
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let enable_validation = args
        .iter()
        .any(|arg| arg == "--validation" || arg == "--validate")
        || cfg!(debug_assertions);

    let (width, height) = parse_resolution();

    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut handler = AppHandler {
        app: None,
        enable_validation,
        width,
        height,
    };
    event_loop.run_app(&mut handler).unwrap();
}
