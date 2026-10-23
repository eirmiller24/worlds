// Instanced textured quads for the map viewport.
//
// One instance = one tile or IntGrid cell. Positions are world pixels; the
// camera maps them to physical pixels inside the viewport:
//   screen = world * zoom - offset     (offset is whole pixels, see Camera)
// Output is premultiplied alpha in gamma space, like egui's own pipeline.

struct Uniforms {
    // xy: snapped camera offset in physical px, z: zoom (px per world px).
    view: vec4<f32>,
    // xy: viewport size in physical px.
    size: vec4<f32>,
    // Opacity of project layer i at opacity[i / 4][i % 4].
    opacity: array<vec4<f32>, 16>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(1) @binding(0) var atlas: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;

struct Inst {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv: vec4<f32>,
    @location(3) color: vec4<f32>,
    // Project layer index (the opacity table); bit 31 = transposed: read
    // the texture's u down the quad and v across it.
    @location(4) layer: u32,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) uv_lo: vec2<f32>,
    @location(3) @interpolate(flat) uv_hi: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Inst) -> VOut {
    // Triangle strip: (0,0) (1,0) (0,1) (1,1).
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let world = inst.pos + corner * inst.size;
    let px = world * u.view.z - u.view.xy;
    var out: VOut;
    out.clip = vec4<f32>(px.x / u.size.x * 2.0 - 1.0, 1.0 - px.y / u.size.y * 2.0, 0.0, 1.0);
    let layer = inst.layer & 0x7fffffffu;
    let transposed = (inst.layer >> 31u) != 0u;
    // Transpose first (swap the corner's axes), then the flips, which the
    // CPU side folded into swapped UV corners (scene.rs, tile_quad).
    out.uv = mix(inst.uv.xy, inst.uv.zw, select(corner, corner.yx, transposed));
    let op = u.opacity[layer / 4u][layer % 4u];
    out.color = vec4<f32>(inst.color.rgb, inst.color.a * op);
    out.uv_lo = min(inst.uv.xy, inst.uv.zw);
    out.uv_hi = max(inst.uv.xy, inst.uv.zw);
    return out;
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(atlas));
    // Keep nearest sampling inside the tile's own texels at any zoom.
    let uv = clamp(in.uv, in.uv_lo + 0.01, in.uv_hi - 0.01);
    let t = textureSampleLevel(atlas, atlas_sampler, uv / dims, 0.0);
    let c = t * in.color;
    return vec4<f32>(c.rgb * c.a, c.a);
}
