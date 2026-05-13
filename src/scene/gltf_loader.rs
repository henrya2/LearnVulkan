use ash::vk;
use glam::{Mat4, Vec4};
use std::collections::{HashMap, HashSet};

use crate::mesh::PbrVertex;
use crate::scene::model::GpuMesh;
use crate::scene::{GpuMaterial, PbrMaterial, SceneGraph, SceneNode};
use crate::vulkan::buffer::{GpuBuffer, create_device_local_buffer};
use crate::vulkan::context::VulkanContext;
use crate::vulkan::texture::Texture;

pub struct Scene {
    pub meshes: Vec<GpuMesh>,
    pub materials: Vec<PbrMaterial>,
    pub textures: Vec<Texture>,
    pub material_buffer: GpuBuffer,
    pub fallback_textures: FallbackTextures,
}

pub struct FallbackTextures {
    pub white_srgb: Texture,
    pub white_linear: Texture,
    pub black_srgb: Texture,
    pub normal_linear: Texture,
    pub metallic_roughness_linear: Texture,
}

struct DecodedImage {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

pub fn load_gltf(ctx: &VulkanContext, command_pool: vk::CommandPool, path: &str) -> Scene {
    let (document, buffers, images) =
        gltf::import(path).unwrap_or_else(|e| panic!("Failed to load glTF {}: {}", path, e));

    let decoded_images: Vec<_> = images.iter().map(decode_image).collect();
    let mut textures = Vec::new();
    let mut texture_variants = HashMap::new();

    let fallback_textures = create_fallback_textures(ctx, command_pool);

    // Load materials. glTF color textures are sRGB; data textures are linear UNORM.
    let mut materials = Vec::new();
    for mat in document.materials() {
        let pbr = mat.pbr_metallic_roughness();
        materials.push(PbrMaterial {
            base_color_factor: pbr.base_color_factor(),
            metallic_factor: pbr.metallic_factor(),
            roughness_factor: pbr.roughness_factor(),
            emissive_factor: mat.emissive_factor(),
            base_color_texture: pbr.base_color_texture().map(|t| {
                get_or_create_texture_variant(
                    ctx,
                    command_pool,
                    &decoded_images,
                    &mut textures,
                    &mut texture_variants,
                    t.texture().source().index(),
                    vk::Format::R8G8B8A8_SRGB,
                )
            }),
            metallic_roughness_texture: pbr.metallic_roughness_texture().map(|t| {
                get_or_create_texture_variant(
                    ctx,
                    command_pool,
                    &decoded_images,
                    &mut textures,
                    &mut texture_variants,
                    t.texture().source().index(),
                    vk::Format::R8G8B8A8_UNORM,
                )
            }),
            normal_texture: mat.normal_texture().map(|t| {
                get_or_create_texture_variant(
                    ctx,
                    command_pool,
                    &decoded_images,
                    &mut textures,
                    &mut texture_variants,
                    t.texture().source().index(),
                    vk::Format::R8G8B8A8_UNORM,
                )
            }),
            occlusion_texture: mat.occlusion_texture().map(|t| {
                get_or_create_texture_variant(
                    ctx,
                    command_pool,
                    &decoded_images,
                    &mut textures,
                    &mut texture_variants,
                    t.texture().source().index(),
                    vk::Format::R8G8B8A8_UNORM,
                )
            }),
            emissive_texture: mat.emissive_texture().map(|t| {
                get_or_create_texture_variant(
                    ctx,
                    command_pool,
                    &decoded_images,
                    &mut textures,
                    &mut texture_variants,
                    t.texture().source().index(),
                    vk::Format::R8G8B8A8_SRGB,
                )
            }),
            normal_scale: mat.normal_texture().map(|t| t.scale()).unwrap_or(1.0),
            occlusion_strength: mat.occlusion_texture().map(|t| t.strength()).unwrap_or(1.0),
        });
    }

    let mut default_material_index = None;

    // Build scene graph and compute world transforms for the active scene only.
    let mut nodes = Vec::new();
    let mut roots = Vec::new();
    let mut gltf_to_local: HashMap<usize, usize> = HashMap::new();
    let mut reachable = HashSet::new();

    for node in document.nodes() {
        let local = convert_transform(node.transform());
        let local_idx = nodes.len();
        gltf_to_local.insert(node.index(), local_idx);
        nodes.push(SceneNode {
            local_transform: local,
            children: Vec::new(),
            mesh: None,
        });
    }

    let active_scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .expect("glTF document has no scenes");

    for node in active_scene.nodes() {
        roots.push(gltf_to_local[&node.index()]);
        add_reachable_children(&mut nodes, &node, &gltf_to_local, &mut reachable);
    }

    let scene_graph = SceneGraph { nodes, roots };
    let world_transforms = scene_graph.compute_world_transforms();

    // Extract meshes from reachable active-scene nodes.
    let mut meshes = Vec::new();
    for node in document
        .nodes()
        .filter(|node| reachable.contains(&node.index()))
    {
        if let Some(mesh) = node.mesh() {
            let world = world_transforms[gltf_to_local[&node.index()]];
            for primitive in mesh.primitives() {
                let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));

                let positions: Vec<[f32; 3]> = reader
                    .read_positions()
                    .expect("Missing positions")
                    .collect();

                let indices: Vec<u32> = if let Some(idx) = reader.read_indices() {
                    idx.into_u32().collect()
                } else {
                    (0..positions.len() as u32).collect()
                };

                let normals: Vec<[f32; 3]> = if let Some(n) = reader.read_normals() {
                    n.collect()
                } else {
                    compute_normals(&positions, &indices)
                };

                let tangents: Vec<[f32; 4]> = if let Some(t) = reader.read_tangents() {
                    t.collect()
                } else {
                    compute_tangents(&positions, &normals)
                };

                let texcoords: Vec<[f32; 2]> = if let Some(tc) = reader.read_tex_coords(0) {
                    tc.into_f32().collect()
                } else {
                    vec![[0.0, 0.0]; positions.len()]
                };

                let mut vertices = Vec::with_capacity(positions.len());
                for i in 0..positions.len() {
                    let mut pos = positions[i];
                    pos[2] = -pos[2];

                    let mut normal = normals[i];
                    normal[2] = -normal[2];

                    let mut tangent = tangents[i];
                    tangent[2] = -tangent[2];
                    tangent[3] = -tangent[3];

                    vertices.push(PbrVertex {
                        pos,
                        normal,
                        tangent,
                        uv0: texcoords[i],
                    });
                }

                let vb = create_device_local_buffer(
                    ctx,
                    command_pool,
                    &vertices,
                    vk::BufferUsageFlags::VERTEX_BUFFER,
                );
                let ib = create_device_local_buffer(
                    ctx,
                    command_pool,
                    &indices,
                    vk::BufferUsageFlags::INDEX_BUFFER,
                );

                let material_index = match primitive.material().index() {
                    Some(index) => index,
                    None => *default_material_index.get_or_insert_with(|| {
                        let index = materials.len();
                        materials.push(PbrMaterial::default_gltf());
                        index
                    }),
                };
                assert!(
                    material_index < materials.len(),
                    "Mesh references material {}, but only {} materials are loaded",
                    material_index,
                    materials.len()
                );

                meshes.push(GpuMesh {
                    vertex_buffer: vb,
                    index_buffer: ib,
                    index_count: indices.len() as u32,
                    material_index,
                    world_matrix: world,
                });
            }
        }
    }

    assert!(
        materials.len() <= 64,
        "PBR shader supports at most 64 materials, but glTF loaded {}",
        materials.len()
    );

    // Upload material buffer.
    let gpu_materials: Vec<GpuMaterial> = materials.iter().map(|m| GpuMaterial::from(m)).collect();
    let material_buffer = create_device_local_buffer(
        ctx,
        command_pool,
        &gpu_materials,
        vk::BufferUsageFlags::UNIFORM_BUFFER,
    );

    Scene {
        meshes,
        materials,
        textures,
        material_buffer,
        fallback_textures,
    }
}

fn decode_image(image: &gltf::image::Data) -> DecodedImage {
    let pixels = match image.format {
        gltf::image::Format::R8 => image.pixels.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        gltf::image::Format::R8G8 => image
            .pixels
            .chunks_exact(2)
            .flat_map(|chunk| [chunk[0], chunk[0], chunk[0], chunk[1]])
            .collect(),
        gltf::image::Format::R8G8B8 => image
            .pixels
            .chunks_exact(3)
            .flat_map(|chunk| [chunk[0], chunk[1], chunk[2], 255])
            .collect(),
        gltf::image::Format::R8G8B8A8 => image.pixels.clone(),
        _ => panic!("Unsupported image format: {:?}", image.format),
    };

    DecodedImage {
        pixels,
        width: image.width,
        height: image.height,
    }
}

fn get_or_create_texture_variant(
    ctx: &VulkanContext,
    command_pool: vk::CommandPool,
    decoded_images: &[DecodedImage],
    textures: &mut Vec<Texture>,
    texture_variants: &mut HashMap<(usize, vk::Format), usize>,
    image_index: usize,
    format: vk::Format,
) -> usize {
    if let Some(&texture_index) = texture_variants.get(&(image_index, format)) {
        return texture_index;
    }

    let image = &decoded_images[image_index];
    let texture = Texture::from_rgba8_with_format(
        ctx,
        command_pool,
        &image.pixels,
        image.width,
        image.height,
        format,
    );
    let texture_index = textures.len();
    textures.push(texture);
    texture_variants.insert((image_index, format), texture_index);
    texture_index
}

fn convert_transform(t: gltf::scene::Transform) -> Mat4 {
    let rh_to_lh = Mat4::from_diagonal(Vec4::new(1.0, 1.0, -1.0, 1.0));
    let mat = match t {
        gltf::scene::Transform::Matrix { matrix } => Mat4::from_cols_array_2d(&matrix),
        gltf::scene::Transform::Decomposed {
            translation,
            rotation,
            scale,
        } => Mat4::from_scale_rotation_translation(
            glam::Vec3::from(scale),
            glam::Quat::from_array(rotation),
            glam::Vec3::from(translation),
        ),
    };
    rh_to_lh * mat * rh_to_lh
}

fn add_reachable_children(
    nodes: &mut [SceneNode],
    parent: &gltf::Node,
    mapping: &HashMap<usize, usize>,
    reachable: &mut HashSet<usize>,
) {
    reachable.insert(parent.index());

    let parent_idx = mapping[&parent.index()];
    for child in parent.children() {
        let child_idx = mapping[&child.index()];
        nodes[parent_idx].children.push(child_idx);
        add_reachable_children(nodes, &child, mapping, reachable);
    }
}

fn create_fallback_textures(
    ctx: &VulkanContext,
    command_pool: vk::CommandPool,
) -> FallbackTextures {
    let white_srgb = Texture::from_rgba8_with_format(
        ctx,
        command_pool,
        &[255, 255, 255, 255],
        1,
        1,
        vk::Format::R8G8B8A8_SRGB,
    );
    let white_linear = Texture::from_rgba8_with_format(
        ctx,
        command_pool,
        &[255, 255, 255, 255],
        1,
        1,
        vk::Format::R8G8B8A8_UNORM,
    );
    let black_srgb = Texture::from_rgba8_with_format(
        ctx,
        command_pool,
        &[0, 0, 0, 255],
        1,
        1,
        vk::Format::R8G8B8A8_SRGB,
    );
    let normal_linear = Texture::from_rgba8_with_format(
        ctx,
        command_pool,
        &[128, 128, 255, 255],
        1,
        1,
        vk::Format::R8G8B8A8_UNORM,
    );
    let metallic_roughness_linear = Texture::from_rgba8_with_format(
        ctx,
        command_pool,
        &[255, 255, 255, 255],
        1,
        1,
        vk::Format::R8G8B8A8_UNORM,
    );

    FallbackTextures {
        white_srgb,
        white_linear,
        black_srgb,
        normal_linear,
        metallic_roughness_linear,
    }
}

fn compute_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut normals = vec![[0.0f32; 3]; positions.len()];
    for tri in indices.chunks_exact(3) {
        let p0 = glam::Vec3::from(positions[tri[0] as usize]);
        let p1 = glam::Vec3::from(positions[tri[1] as usize]);
        let p2 = glam::Vec3::from(positions[tri[2] as usize]);
        let n = (p1 - p0).cross(p2 - p0).normalize_or_zero();
        for &idx in tri {
            let i = idx as usize;
            normals[i][0] += n.x;
            normals[i][1] += n.y;
            normals[i][2] += n.z;
        }
    }
    for n in &mut normals {
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len > 0.0 {
            n[0] /= len;
            n[1] /= len;
            n[2] /= len;
        } else {
            *n = [0.0, 1.0, 0.0];
        }
    }
    normals
}

fn compute_tangents(positions: &[[f32; 3]], normals: &[[f32; 3]]) -> Vec<[f32; 4]> {
    positions
        .iter()
        .zip(normals.iter())
        .map(|(_pos, normal)| {
            let n = glam::Vec3::from(*normal).normalize_or_zero();
            let arbitrary = if n.y.abs() < 0.999 {
                glam::Vec3::Y
            } else {
                glam::Vec3::X
            };
            let t = arbitrary.cross(n).normalize_or_zero();
            [t.x, t.y, t.z, 1.0]
        })
        .collect()
}

impl Scene {
    pub unsafe fn destroy(&mut self, device: &ash::Device) {
        for mesh in &self.meshes {
            unsafe {
                mesh.destroy(device);
            }
        }
        unsafe {
            self.material_buffer.destroy(device);
        }
        for tex in &self.textures {
            unsafe {
                tex.destroy(device);
            }
        }
        unsafe {
            self.fallback_textures.white_srgb.destroy(device);
            self.fallback_textures.white_linear.destroy(device);
            self.fallback_textures.black_srgb.destroy(device);
            self.fallback_textures.normal_linear.destroy(device);
            self.fallback_textures
                .metallic_roughness_linear
                .destroy(device);
        }
    }
}
