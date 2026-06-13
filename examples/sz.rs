use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct T {
    pub a: glam::Mat4,
    pub b: glam::Mat4,
    pub c: glam::Vec4,
    pub d: glam::Vec4,
    pub e: f32,
    pub f: f32,
    pub g: [f32; 2],
}

fn main() {
    println!("size_of<T>={}", std::mem::size_of::<T>());
    println!("align_of<T>={}", std::mem::align_of::<T>());
    println!("size_of<Mat4>={} align={}", std::mem::size_of::<glam::Mat4>(), std::mem::align_of::<glam::Mat4>());
    println!("size_of<(Mat4,u32)>={}", std::mem::size_of::<(glam::Mat4, u32)>());
    println!("size_of<(Mat4,u32,u32)>={}", std::mem::size_of::<(glam::Mat4, u32, u32)>());
    println!("size_of<(Mat4,[u32;3])>={}", std::mem::size_of::<(glam::Mat4, [u32; 3])>());
}
