use glam::{Mat4, Vec3};

pub struct Camera {
    pub position: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub fov_y: f32,
    pub quat: glam::Quat,
    pub move_speed: f32,
    pub mouse_sensitivity: f32,
}

impl Camera {
    fn calculate_quat(yaw: f32, pitch: f32) -> glam::Quat {
        glam::Quat::from_euler(glam::EulerRot::YXZ, yaw, pitch, 0.0)
    }

    pub fn new() -> Self {
        let mut result = Self {
            position: Vec3::new(0.0, 1.6, -3.0),
            yaw: 0.0,
            pitch: 0.0,
            quat: glam::Quat::IDENTITY,
            fov_y: 60.0_f32.to_radians(),
            move_speed: 4.0,
            mouse_sensitivity: 0.0025,
        };

        result.quat = Self::calculate_quat(result.yaw, result.pitch);

        result
    }

    pub fn forward(&self) -> Vec3 {
        self.quat * glam::Vec3::Z
    }

    pub fn right(&self) -> Vec3 {
        self.quat * glam::Vec3::X
    }

    pub fn up(&self) -> Vec3 {
        //self.quat * glam::Vec3::Y
        self.forward().cross(self.right()).normalize()
    }

    pub fn view_matrix(&self) -> Mat4 {
        Mat4::look_to_lh(self.position, self.forward(), self.up())
    }

    pub fn projection_matrix(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_lh(self.fov_y, aspect, 0.1, 100.0)
    }

    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        self.projection_matrix(aspect) * self.view_matrix()
    }

    pub fn apply_mouse_delta(&mut self, dx: f64, dy: f64) {
        self.update_rotation(
            self.yaw + dx as f32 * self.mouse_sensitivity,
            self.pitch + dy as f32 * self.mouse_sensitivity,
        );
    }

    fn update_rotation(&mut self, yaw: f32, pitch: f32) {
        self.yaw = yaw;
        self.pitch = pitch;

        self.pitch = self
            .pitch
            .clamp(-89.0_f32.to_radians(), 89.0_f32.to_radians());

        self.quat = Self::calculate_quat(self.yaw, self.pitch);
    }
}
