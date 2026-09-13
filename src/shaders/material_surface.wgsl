struct SurfaceUniform {
    tile: vec4<u32>,
    paint: vec4<f32>,
    settings: vec4<f32>,
    surface_origins: array<vec4<i32>, 5>,
    mask_origins: array<vec4<i32>, 5>,
}
@group(0) @binding(0) var<uniform> contact: SurfaceUniform;
@group(0) @binding(1) var old0: texture_2d<f32>;
@group(0) @binding(2) var old1: texture_2d<f32>;
@group(0) @binding(3) var old2: texture_2d<f32>;
@group(0) @binding(4) var old3: texture_2d<f32>;
@group(0) @binding(5) var old4: texture_2d<f32>;
@group(0) @binding(6) var mask0: texture_2d<f32>;
@group(0) @binding(7) var mask1: texture_2d<f32>;
@group(0) @binding(8) var mask2: texture_2d<f32>;
@group(0) @binding(9) var mask3: texture_2d<f32>;
@group(0) @binding(10) var mask4: texture_2d<f32>;
@group(0) @binding(11) var base_color: texture_2d<f32>;
@group(0) @binding(12) var out_color: texture_storage_2d<rgba32float, write>;
@group(0) @binding(13) var out_material: texture_storage_2d<rgba32float, write>;

// Address adjacent logical tiles, not adjacent (unrelated) atlas slots.
fn address(p: vec2<i32>) -> vec3<i32> {
    let size = i32(contact.tile.x);
    if p.x < 0 { return vec3<i32>(p + vec2<i32>(size,0), 1); }
    if p.x >= size { return vec3<i32>(p - vec2<i32>(size,0), 2); }
    if p.y < 0 { return vec3<i32>(p + vec2<i32>(0,size), 3); }
    if p.y >= size { return vec3<i32>(p - vec2<i32>(0,size), 4); }
    return vec3<i32>(p, 0);
}
fn old_surface(p: vec2<i32>) -> vec4<f32> {
    let a = address(p); let origin = contact.surface_origins[a.z];
    if origin.z == 0 { return vec4<f32>(0.0); }
    let q = a.xy + origin.xy;
    switch a.z { case 1: { return textureLoad(old1,q,0); } case 2: { return textureLoad(old2,q,0); } case 3: { return textureLoad(old3,q,0); } case 4: { return textureLoad(old4,q,0); } default: { return textureLoad(old0,q,0); } }
}
fn deposit(p: vec2<i32>) -> vec2<f32> {
    let a = address(p); let origin = contact.mask_origins[a.z];
    if origin.z == 0 { return vec2<f32>(0.0); }
    let q = a.xy + origin.xy;
    switch a.z { case 1: { return textureLoad(mask1,q,0).xy; } case 2: { return textureLoad(mask2,q,0).xy; } case 3: { return textureLoad(mask3,q,0).xy; } case 4: { return textureLoad(mask4,q,0).xy; } default: { return textureLoad(mask0,q,0).xy; } }
}
fn height(p: vec2<i32>) -> f32 {
    return min(64.0, old_surface(p).a * 64.0 + deposit(p).y * contact.settings.x * contact.paint.a);
}
@compute @workgroup_size(8,8)
fn surface_main(@builtin(global_invocation_id) invocation: vec3<u32>) {
    if any(invocation.xy >= vec2<u32>(contact.tile.x)) { return; }
    let p = vec2<i32>(invocation.xy);
    let q = p + vec2<i32>(contact.tile.yz);
    var base = vec4<f32>(0.0);
    if contact.tile.w != 0u { base = textureLoad(base_color,q,0); }
    let old = old_surface(p);
    let d = deposit(p);
    let alpha = clamp(d.x * contact.paint.a, 0.0, 1.0);
    var color = contact.paint.rgb;
    var material = old * (1.0 - alpha);
    var result = vec4<f32>(color * alpha, alpha) + base * (1.0-alpha);
    if contact.settings.y == 1.0 && alpha > 0.0 {
        let dx = (height(p+vec2<i32>(1,0)) - height(p-vec2<i32>(1,0))) * 0.5;
        let dy = (height(p+vec2<i32>(0,1)) - height(p-vec2<i32>(0,1))) * 0.5;
        let normal = normalize(vec3<f32>(-dx * 1.7, -dy * 1.7, 1.0));
        let light = normalize(vec3<f32>(-0.45, 0.65, 1.0));
        let shade = clamp(0.45 + 0.55 * dot(normal, light) / light.z, 0.45, 1.22);
        let half_light = normalize(light + vec3<f32>(0.0, 0.0, 1.0));
        let specular = 0.06 * pow(max(dot(normal, half_light), 0.0), 64.0);
        color = clamp(color * shade + (vec3<f32>(1.0)-color)*specular, vec3<f32>(0.0), vec3<f32>(1.0));
        result = vec4<f32>(color * alpha, alpha) + base * (1.0-alpha);
        let old_pigment = select(contact.paint.rgb, old.rgb / max(old.a, 1e-12), old.a > 0.0);
        let pigment = mix(old_pigment, contact.paint.rgb, alpha);
        let depth = height(p) / 64.0;
        material = vec4<f32>(pigment * depth, depth);
    } else if contact.settings.y == 2.0 { result = base * (1.0-alpha); }
    textureStore(out_color,p,result);
    textureStore(out_material,p,material);
}
