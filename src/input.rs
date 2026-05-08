pub struct InputState {
    pub forward: bool,
    pub back: bool,
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
    pub mouse_delta: (f64, f64),
    pub alt_down: bool,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            forward: false,
            back: false,
            left: false,
            right: false,
            up: false,
            down: false,
            mouse_delta: (0.0, 0.0),
            alt_down: false,
        }
    }

    pub fn drain_mouse_delta(&mut self) -> (f64, f64) {
        let d = self.mouse_delta;
        self.mouse_delta = (0.0, 0.0);
        d
    }
}
