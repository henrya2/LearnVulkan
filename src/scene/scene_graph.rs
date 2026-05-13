pub struct SceneNode {
    pub local_transform: glam::Mat4,
    pub children: Vec<usize>,
    pub mesh: Option<usize>,
}

pub struct SceneGraph {
    pub nodes: Vec<SceneNode>,
    pub roots: Vec<usize>,
}

impl SceneGraph {
    pub fn compute_world_transforms(&self) -> Vec<glam::Mat4> {
        let mut world = vec![glam::Mat4::IDENTITY; self.nodes.len()];
        for &root in &self.roots {
            self.dfs(root, glam::Mat4::IDENTITY, &mut world);
        }
        world
    }

    fn dfs(&self, node: usize, parent: glam::Mat4, out: &mut [glam::Mat4]) {
        let t = parent * self.nodes[node].local_transform;
        out[node] = t;
        for &child in &self.nodes[node].children {
            self.dfs(child, t, out);
        }
    }
}
