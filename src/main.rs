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
}

impl ApplicationHandler for AppHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.app.is_none() {
            let attrs = Window::default_attributes()
                .with_title("LearnVulkan - FPS Camera")
                .with_inner_size(winit::dpi::LogicalSize::new(800, 600));
            let window = event_loop.create_window(attrs).unwrap();
            self.app = Some(App::new(window));
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

fn main() {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut handler = AppHandler { app: None };
    event_loop.run_app(&mut handler).unwrap();
}
