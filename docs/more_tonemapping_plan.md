# Tonemapping Implementation & Postprocessing Framework Refactor

## Overview

1. **Improve tonemapping shader implementations** (especially Reinhard: make it luminance-based)
2. **Add `T` key to cycle between Linear/Reinhard/ACES**
3. **Refactor `PostProcessSettings`** for cleaner per-operator parameter tuning in code

---

## 1. UBO Changes (`src/vulkan/postprocess/ubo.rs`)

Replace padding with named, tunable tonemapping parameters. Total size stays 64 bytes.

**Before:**
```rust
pub tonemap_op: u32,   // offset 48
pub _pad: [u32; 3],    // offset 52
```

**After:**
```rust
pub tonemap_op: u32,        // offset 48 (0=Linear, 1=Reinhard, 2=ACES)
pub reinhard_white: f32,    // offset 52 (white point for Reinhard, default 1.0)
pub _reserved0: f32,        // offset 56 (future use)
pub _reserved1: f32,        // offset 60 (future use)
```

Update `Default` accordingly: `reinhard_white: 1.0`.

---

## 2. Shader Changes (`shaders/postprocess/composite.frag`)

### 2a. Update UBO declaration
```glsl
layout(set = 1, binding = 0) uniform PostProcessUBO {
    float exposure;
    float bloom_threshold;
    float bloom_knee;
    float bloom_intensity;
    float bloom_weights[8];
    uint  tonemap_op;
    float reinhard_white;
    float _reserved0;
    float _reserved1;
} pp;
```

### 2b. Luminance-based Reinhard (more correct for PBR)
**Before (per-channel, creates hue shifts):**
```glsl
vec3 reinhard(vec3 c) {
    return c / (c + vec3(1.0));
}
```

**After (luminance-based with white point):**
```glsl
vec3 reinhard(vec3 c) {
    // Photographic Reinhard with white point (luminance-preserving hue)
    float lum = dot(c, vec3(0.2126, 0.7152, 0.0722)); // BT.709 luminance
    float white = pp.reinhard_white;
    float tone = lum * (1.0 + lum / (white * white)) / (1.0 + lum);
    return c * (tone / max(lum, 1e-5));
}
```

### 2c. ACES and Linear remain unchanged
- ACES Narkowicz approximation is correct and standard
- Linear (clamp) is correct for debug

---

## 3. Shader Changes (`shaders/postprocess/bright.frag` and `shaders/postprocess/blur.frag`)

Update UBO declarations to match the new layout (replace `uint _pad[3]` with `float reinhard_white; float _reserved0; float _reserved1;`). These shaders don't use the new fields, but the block declaration must match GLSL std140 layout.

---

## 4. PostProcessSettings Refactor (`src/vulkan/postprocess/resources.rs`)

**Before:**
```rust
pub struct PostProcessSettings {
    pub ubo: PostProcessUBO,
    pub bloom_enabled: bool,
}
```

**After:**
```rust
/// Per-operator tonemap tunables (in-code only, no runtime UI).
pub struct TonemapSettings {
    pub operator: TonemapOp,
    pub exposure: f32,          // stops, applied as pow(2, exposure)
    pub reinhard_white: f32,    // white point for Reinhard luminance
}

impl Default for TonemapSettings {
    fn default() -> Self {
        Self { operator: TonemapOp::Aces, exposure: 0.0, reinhard_white: 1.0 }
    }
}

/// Bloom parameter tunables (in-code only, no runtime UI).
pub struct BloomSettings {
    pub enabled: bool,
    pub threshold: f32,
    pub knee: f32,
    pub intensity: f32,
    pub weights: [f32; 8],
}

impl Default for BloomSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 1.0,
            knee: 0.5,
            intensity: 0.04,
            weights: [0.4, 0.3, 0.25, 0.2, 0.15, 0.1, 0.05, 0.025],
        }
    }
}

/// Runtime-tweakable postprocess settings (CPU-side, canonical source).
pub struct PostProcessSettings {
    pub tonemap: TonemapSettings,
    pub bloom: BloomSettings,
}

impl PostProcessSettings {
    /// Build the GPU-side UBO from the decomposed CPU settings.
    pub fn to_ubo(&self) -> PostProcessUBO {
        PostProcessUBO {
            exposure: self.tonemap.exposure,
            bloom_threshold: self.bloom.threshold,
            bloom_knee: self.bloom.knee,
            bloom_intensity: if self.bloom.enabled { self.bloom.intensity } else { 0.0 },
            bloom_weights: self.bloom.weights,
            tonemap_op: self.tonemap.operator as u32,
            reinhard_white: self.tonemap.reinhard_white,
            _reserved0: 0.0,
            _reserved1: 0.0,
        }
    }
}
```

Update `update_ubo()` to call `self.settings.to_ubo()` instead of manually zeroing bloom.

---

## 5. Renderer Changes (`src/vulkan/renderer.rs`)

Add method:
```rust
pub fn cycle_tonemap_op(&mut self) {
    if let Some(ref mut pp) = self.postprocess {
        pp.settings.tonemap.operator = match pp.settings.tonemap.operator {
            TonemapOp::Linear => TonemapOp::Reinhard,
            TonemapOp::Reinhard => TonemapOp::Aces,
            TonemapOp::Aces => TonemapOp::Linear,
        };
    }
}
```

---

## 6. Input Handling (`src/app.rs`)

Add to `on_keyboard`:
```rust
PhysicalKey::Code(KeyCode::KeyT) => {
    if pressed {
        self.renderer.cycle_tonemap_op();
    }
}
```

---

## Files to Modify (5 files)

| File | Changes |
|---|---|
| `shaders/postprocess/composite.frag` | UBO layout, luminance-based Reinhard |
| `shaders/postprocess/bright.frag` | UBO layout padding update |
| `shaders/postprocess/blur.frag` | UBO layout padding update |
| `src/vulkan/postprocess/ubo.rs` | `_pad` → `reinhard_white` + reserved |
| `src/vulkan/postprocess/resources.rs` | Refactor `PostProcessSettings` into `TonemapSettings` + `BloomSettings` |
| `src/vulkan/renderer.rs` | Add `cycle_tonemap_op()` |
| `src/app.rs` | Handle `KeyT` press |

## Post-Implementation Steps

- Recompile shaders: `cd shaders && ./compile.bat`
- Build: `cargo build`
- Run and verify: press `T` to cycle Linear → Reinhard → ACES → Linear
- Visual verification: Linear should look washed out (no tonemapping), ACES should have filmic contrast, Reinhard should be softer desaturation

## No UBO size change

UBO stays at exactly 64 bytes. No descriptor pool or buffer allocation changes needed. No shader recompile needed for anything outside the three postprocess .frag files. No pipeline or descriptor layout changes needed.
