use glam::{Mat4, Vec3};

pub struct Camera {
    pub position: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub fov_y: f32,
    pub move_speed: f32,
    pub mouse_sensitivity: f32,
}

impl Camera {
    pub fn new() -> Self {
        Self {
            position: Vec3::new(0.0, 1.6, -3.0),
            yaw: 0.0,
            pitch: 0.0,
            fov_y: 60.0_f32.to_radians(),
            move_speed: 4.0,
            mouse_sensitivity: 0.0025,
        }
    }

    pub fn forward(&self) -> Vec3 {
        let quat = glam::Quat::from_euler(glam::EulerRot::YXZ, self.yaw, self.pitch, 0.0);
        quat * glam::Vec3::Z
    }

    pub fn right(&self) -> Vec3 {
        Vec3::Y.cross(self.forward()).normalize()
    }

    pub fn up(&self) -> Vec3 {
        self.forward().cross(self.right()).normalize()
    }

    pub fn view_matrix(&self) -> Mat4 {
        Mat4::look_to_lh(self.position, self.forward(), Vec3::Y)
    }

    pub fn projection_matrix(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_lh(self.fov_y, aspect, 0.1, 100.0)
    }

    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        self.projection_matrix(aspect) * self.view_matrix()
    }

    pub fn apply_mouse_delta(&mut self, dx: f64, dy: f64) {
        self.yaw += dx as f32 * self.mouse_sensitivity;
        self.pitch += dy as f32 * self.mouse_sensitivity;
        self.pitch = self
            .pitch
            .clamp(-89.0_f32.to_radians(), 89.0_f32.to_radians());
    }
}
