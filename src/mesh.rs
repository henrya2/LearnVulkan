use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
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
                .format(ash::vk::Format::R32G32_SFLOAT)
                .offset(12),
        ]
    }
}

// p0..p3 are the 4 corners of a quad in CCW-from-outside order.
// Corner UVs: p0=(0,1), p1=(0,0), p2=(1,0), p3=(1,1)
// so the texture appears upright (v=0 at top) from outside the face.
fn face(
    verts: &mut Vec<Vertex>,
    idx: &mut Vec<u32>,
    p0: [f32; 3],
    p1: [f32; 3],
    p2: [f32; 3],
    p3: [f32; 3],
) {
    let b = verts.len() as u32;
    verts.push(Vertex {
        pos: p0,
        uv: [0.0, 1.0],
    });
    verts.push(Vertex {
        pos: p1,
        uv: [0.0, 0.0],
    });
    verts.push(Vertex {
        pos: p2,
        uv: [1.0, 0.0],
    });
    verts.push(Vertex {
        pos: p3,
        uv: [1.0, 1.0],
    });
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

    // +X face (right)
    face(
        &mut verts,
        &mut idx,
        [h, -h, -h],
        [h, h, -h],
        [h, h, h],
        [h, -h, h],
    );
    // -X face (left)
    face(
        &mut verts,
        &mut idx,
        [-h, -h, h],
        [-h, h, h],
        [-h, h, -h],
        [-h, -h, -h],
    );
    // +Y face (top)
    face(
        &mut verts,
        &mut idx,
        [-h, h, -h],
        [-h, h, h],
        [h, h, h],
        [h, h, -h],
    );
    // -Y face (bottom)
    face(
        &mut verts,
        &mut idx,
        [-h, -h, h],
        [-h, -h, -h],
        [h, -h, -h],
        [h, -h, h],
    );
    // +Z face (far)
    face(
        &mut verts,
        &mut idx,
        [h, -h, h],
        [h, h, h],
        [-h, h, h],
        [-h, -h, h],
    );
    // -Z face (near)
    face(
        &mut verts,
        &mut idx,
        [-h, -h, -h],
        [-h, h, -h],
        [h, h, -h],
        [h, -h, -h],
    );

    (verts, idx)
}

// `tile` is how many times the texture repeats across the full plane (each axis).
pub fn floor(half: f32, y: f32, tile: f32) -> (Vec<Vertex>, Vec<u32>) {
    let mut verts = Vec::with_capacity(4);
    let mut idx = Vec::with_capacity(6);

    let b = verts.len() as u32;
    verts.push(Vertex {
        pos: [-half, y, -half],
        uv: [0.0, 0.0],
    });
    verts.push(Vertex {
        pos: [-half, y, half],
        uv: [0.0, tile],
    });
    verts.push(Vertex {
        pos: [half, y, half],
        uv: [tile, tile],
    });
    verts.push(Vertex {
        pos: [half, y, -half],
        uv: [tile, 0.0],
    });
    idx.extend_from_slice(&[b + 0, b + 1, b + 2, b + 0, b + 2, b + 3]);

    // Sanity check
    let e1 = glam::Vec3::new(0.0, 0.0, 2.0 * half);
    let e2 = glam::Vec3::new(2.0 * half, 0.0, 2.0 * half);
    let normal = e1.cross(e2);
    debug_assert!(normal.y > 0.0, "floor normal does not point up");

    (verts, idx)
}
