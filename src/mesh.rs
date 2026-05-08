use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub color: [f32; 3],
}

impl Vertex {
    pub fn binding_description() -> ash::vk::VertexInputBindingDescription {
        ash::vk::VertexInputBindingDescription::default()
            .binding(0)
            .stride(std::mem::size_of::<Vertex>() as u32)
            .input_rate(ash::vk::VertexInputRate::VERTEX)
    }

    pub fn attribute_descriptions() -> [ash::vk::VertexInputAttributeDescription; 2] {
        [
            ash::vk::VertexInputAttributeDescription::default()
                .binding(0)
                .location(0)
                .format(ash::vk::Format::R32G32B32_SFLOAT)
                .offset(0),
            ash::vk::VertexInputAttributeDescription::default()
                .binding(0)
                .location(1)
                .format(ash::vk::Format::R32G32B32_SFLOAT)
                .offset(12),
        ]
    }
}

fn face(
    verts: &mut Vec<Vertex>,
    idx: &mut Vec<u32>,
    p0: [f32; 3],
    p1: [f32; 3],
    p2: [f32; 3],
    p3: [f32; 3],
    color: [f32; 3],
) {
    let b = verts.len() as u32;
    verts.push(Vertex { pos: p0, color });
    verts.push(Vertex { pos: p1, color });
    verts.push(Vertex { pos: p2, color });
    verts.push(Vertex { pos: p3, color });
    idx.extend_from_slice(&[b + 0, b + 1, b + 2, b + 0, b + 2, b + 3]);

    // Sanity check: (p1-p0) x (p2-p0) with standard cross should point away from origin
    let e1 = glam::Vec3::from(p1) - glam::Vec3::from(p0);
    let e2 = glam::Vec3::from(p2) - glam::Vec3::from(p0);
    let normal = e1.cross(e2);
    let center =
        glam::Vec3::from(p0) + glam::Vec3::from(p1) + glam::Vec3::from(p2) + glam::Vec3::from(p3);
    debug_assert!(
        normal.dot(center) > 0.0,
        "face normal does not point outward"
    );
}

pub fn cube(size: f32) -> (Vec<Vertex>, Vec<u32>) {
    let h = size / 2.0;
    let mut verts = Vec::with_capacity(24);
    let mut idx = Vec::with_capacity(36);

    // +X face (right, red)
    face(
        &mut verts,
        &mut idx,
        [h, -h, -h],
        [h, h, -h],
        [h, h, h],
        [h, -h, h],
        [1.0, 0.0, 0.0],
    );
    // -X face (left, dark red)
    face(
        &mut verts,
        &mut idx,
        [-h, -h, h],
        [-h, h, h],
        [-h, h, -h],
        [-h, -h, -h],
        [0.5, 0.0, 0.0],
    );
    // +Y face (top, green)
    face(
        &mut verts,
        &mut idx,
        [-h, h, -h],
        [-h, h, h],
        [h, h, h],
        [h, h, -h],
        [0.0, 1.0, 0.0],
    );
    // -Y face (bottom, dark green)
    face(
        &mut verts,
        &mut idx,
        [-h, -h, h],
        [-h, -h, -h],
        [h, -h, -h],
        [h, -h, h],
        [0.0, 0.5, 0.0],
    );
    // +Z face (far, blue)
    face(
        &mut verts,
        &mut idx,
        [h, -h, h],
        [h, h, h],
        [-h, h, h],
        [-h, -h, h],
        [0.0, 0.0, 1.0],
    );
    // -Z face (near, dark blue)
    face(
        &mut verts,
        &mut idx,
        [-h, -h, -h],
        [-h, h, -h],
        [h, h, -h],
        [h, -h, -h],
        [0.0, 0.0, 0.5],
    );

    (verts, idx)
}

pub fn floor(half: f32, y: f32, color: [f32; 3]) -> (Vec<Vertex>, Vec<u32>) {
    let mut verts = Vec::with_capacity(4);
    let mut idx = Vec::with_capacity(6);

    let b = verts.len() as u32;
    verts.push(Vertex {
        pos: [-half, y, -half],
        color,
    });
    verts.push(Vertex {
        pos: [-half, y, half],
        color,
    });
    verts.push(Vertex {
        pos: [half, y, half],
        color,
    });
    verts.push(Vertex {
        pos: [half, y, -half],
        color,
    });
    idx.extend_from_slice(&[b + 0, b + 1, b + 2, b + 0, b + 2, b + 3]);

    // Sanity check
    let e1 = glam::Vec3::new(0.0, 0.0, 2.0 * half);
    let e2 = glam::Vec3::new(2.0 * half, 0.0, 2.0 * half);
    let normal = e1.cross(e2);
    debug_assert!(normal.y > 0.0, "floor normal does not point up");

    (verts, idx)
}
