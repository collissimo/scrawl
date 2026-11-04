#include <metal_stdlib>
using namespace metal;

// ─── Pass 1: Render elements into dedicated tiles in offscreen_a (960 × 256 pt) ────────────

struct TileUniforms {
    float2 atlas_size;   // in pixels
    float2 canvas_size;  // in points (960, 256)
    float scale;         // Retina backing scale (2.0)
    float _pad0;
    float2 _pad1;
};

// kind == 0: analytic rounded rect / circle (`rect` in canvas pt, `param.x` = corner radius)
// kind == 1: textured quad from `atlas` (`rect` in canvas pt, `uv_rect` = [u0, v0, u1, v1] in 0..1)
struct TilePrim {
    float4 rect;      // x, y, w, h in canvas points
    float4 uv_rect;   // u0, v0, u1, v1 (for kind == 1) or (radius, 0, 0, 0) (for kind == 0)
    float4 color;     // premultiplied or straight RGBA (r, g, b, a)
    uint kind;
    uint _pad0;
    uint _pad1;
    uint _pad2;
};

struct TileVertexOut {
    float4 position [[position]];
    float2 pt;        // position in canvas points
    float2 uv;
    float4 rect;
    float4 color;
    float radius;
    uint kind;
};

vertex TileVertexOut pill_tile_vertex(
    uint vid [[vertex_id]],
    uint iid [[instance_id]],
    constant TileUniforms &u [[buffer(0)]],
    const device TilePrim *prims [[buffer(1)]]
) {
    const float2 corners[6] = {
        float2(0.0, 0.0), float2(1.0, 0.0), float2(0.0, 1.0),
        float2(0.0, 1.0), float2(1.0, 0.0), float2(1.0, 1.0)
    };
    TilePrim prim = prims[iid];
    float2 c = corners[vid];
    // For analytic shapes, expand quad by 1.5 pt so anti-aliased edges never clip.
    float pad = (prim.kind == 0u) ? 1.5 : 0.0;
    float2 p = prim.rect.xy - float2(pad) + c * (prim.rect.zw + float2(2.0 * pad));
    float2 ndc = float2(
        (p.x / u.canvas_size.x) * 2.0 - 1.0,
        1.0 - (p.y / u.canvas_size.y) * 2.0
    );

    TileVertexOut out;
    out.position = float4(ndc, 0.0, 1.0);
    out.pt = p;
    out.uv = mix(prim.uv_rect.xy, prim.uv_rect.zw, c);
    out.rect = prim.rect;
    out.color = prim.color;
    out.radius = prim.uv_rect.x;
    out.kind = prim.kind;
    return out;
}

fragment float4 pill_tile_fragment(
    TileVertexOut in [[stage_in]],
    constant TileUniforms &u [[buffer(0)]],
    texture2d<float> atlas [[texture(0)]]
) {
    if (in.kind == 0u) {
        float2 center = in.rect.xy + in.rect.zw * 0.5;
        float2 half_sz = in.rect.zw * 0.5;
        float r = min(in.radius, min(half_sz.x, half_sz.y));
        float2 q = abs(in.pt - center) - (half_sz - float2(r));
        float sdf = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
        float cov = clamp(0.5 - sdf * u.scale, 0.0, 1.0);
        float a = in.color.a * cov;
        return float4(in.color.rgb * a, a);
    } else {
        constexpr sampler s(mag_filter::linear, min_filter::linear, address::clamp_to_edge);
        float4 tex = atlas.sample(s, in.uv);
        // Atlas stores white mask with alpha in .a (premultiplied).
        float cov = tex.a;
        float a = in.color.a * cov;
        return float4(in.color.rgb * a, a);
    }
}

// ─── Pass 2: 1D Horizontal Gaussian Blur (offscreen_a -> offscreen_b) ──────────────────────

struct BlurBox {
    float4 tile_rect; // x, y, w, h in canvas points (960 × 256)
    float sigma_pt;   // blur sigma in points
    float _pad0;
    float2 _pad1;
};

struct BlurVertexOut {
    float4 position [[position]];
    float2 pt;
    float4 tile_rect;
    float sigma_pt;
};

vertex BlurVertexOut pill_blur_h_vertex(
    uint vid [[vertex_id]],
    uint iid [[instance_id]],
    constant TileUniforms &u [[buffer(0)]],
    const device BlurBox *boxes [[buffer(1)]]
) {
    const float2 corners[6] = {
        float2(0.0, 0.0), float2(1.0, 0.0), float2(0.0, 1.0),
        float2(0.0, 1.0), float2(1.0, 0.0), float2(1.0, 1.0)
    };
    BlurBox b = boxes[iid];
    float2 c = corners[vid];
    float2 p = b.tile_rect.xy + c * b.tile_rect.zw;
    float2 ndc = float2(
        (p.x / u.canvas_size.x) * 2.0 - 1.0,
        1.0 - (p.y / u.canvas_size.y) * 2.0
    );
    BlurVertexOut out;
    out.position = float4(ndc, 0.0, 1.0);
    out.pt = p;
    out.tile_rect = b.tile_rect;
    out.sigma_pt = b.sigma_pt;
    return out;
}

fragment float4 pill_blur_h_fragment(
    BlurVertexOut in [[stage_in]],
    constant TileUniforms &u [[buffer(0)]],
    texture2d<float> src [[texture(0)]]
) {
    constexpr sampler s(mag_filter::linear, min_filter::linear, address::clamp_to_edge);
    float2 inv_canvas = 1.0 / u.canvas_size;
    float sigma = in.sigma_pt;
    if (sigma < 0.05) {
        return src.sample(s, in.pt * inv_canvas);
    }
    float min_x = in.tile_rect.x + 0.5;
    float max_x = in.tile_rect.x + in.tile_rect.z - 0.5;
    int radius = min(24, int(ceil(sigma * 3.0 * u.scale)));
    float step_pt = 1.0 / u.scale;
    float inv_2s2 = 0.5 / max(sigma * sigma, 1e-4);

    float4 acc = float4(0.0);
    float w_sum = 0.0;
    for (int k = -radius; k <= radius; ++k) {
        float dx = float(k) * step_pt;
        float w = exp(-(dx * dx) * inv_2s2);
        float sx = clamp(in.pt.x + dx, min_x, max_x);
        acc += src.sample(s, float2(sx, in.pt.y) * inv_canvas) * w;
        w_sum += w;
    }
    return acc / max(w_sum, 1e-5);
}

// ─── Pass 3: Analytic SDF Capsule + 1D Vertical Blurred Element Composite + Hairline Border ─

struct CompositeElem {
    float4 screen_rect; // x, y, w, h in panel points (scaled)
    float4 tile_rect;   // tile_x, tile_y, tile_w, tile_h in offscreen_b points
    float2 base_size;   // unscaled (w, h) in points
    float scale;        // element scale (0.82 .. 1.0)
    float alpha;        // element opacity (0 .. 1)
    float sigma_pt;     // vertical blur sigma in points
    float pad_pt;       // tile padding in points (16.0)
    float2 _pad0;
};

struct PillUniforms {
    float2 panel_size;     // (640, 84) in points
    float2 canvas_size;    // (960, 256) in points
    float scale;           // Retina backing scale (2.0)
    uint elem_count;
    uint is_completed;
    uint has_ghost;
    float4 shape_rect;     // l, t, r, b in panel points
    float4 shape_meta;     // radius, alpha, blur, is_glass (1.0 = glass, 0.0 = black)
    float4 ghost_rect;     // l, t, r, b in panel points
    float4 ghost_meta;     // radius, alpha, 0, 0
};

struct ScreenVertexOut {
    float4 position [[position]];
    float2 uv;
};

vertex ScreenVertexOut pill_screen_vertex(uint vid [[vertex_id]]) {
    const float2 pos[3] = {
        float2(-1.0, -1.0),
        float2( 3.0, -1.0),
        float2(-1.0,  3.0)
    };
    ScreenVertexOut out;
    out.position = float4(pos[vid], 0.0, 1.0);
    out.uv = pos[vid] * float2(0.5, -0.5) + 0.5;
    return out;
}

inline float rrect_sdf(float2 p, float4 rect, float r) {
    float2 center = (rect.xy + rect.zw) * 0.5;
    float2 half_sz = max((rect.zw - rect.xy) * 0.5, float2(0.0));
    float cr = min(r, min(half_sz.x, half_sz.y));
    float2 q = abs(p - center) - (half_sz - float2(cr));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - cr;
}

inline float4 blend_over(float4 dst, float4 src) {
    return src + dst * (1.0 - src.a);
}

fragment float4 pill_composite_fragment(
    ScreenVertexOut in [[stage_in]],
    constant PillUniforms &u [[buffer(0)]],
    const device CompositeElem *elems [[buffer(1)]],
    texture2d<float> offscreen_b [[texture(0)]]
) {
    constexpr sampler s(mag_filter::linear, min_filter::linear, address::clamp_to_edge);
    float2 p = in.uv * u.panel_size;
    float4 out = float4(0.0);

    float shape_alpha = u.shape_meta.y;
    if (shape_alpha <= 0.001 && u.has_ghost == 0u) {
        return out;
    }

    float r = u.shape_meta.x;
    float is_glass = u.shape_meta.w;
    float sdf = rrect_sdf(p, u.shape_rect, r);

    // 1. Soft drop shadow behind the capsule (in black mode; NSGlassEffectView has its own material shadow).
    if (is_glass < 0.5) {
        float sdf_shadow = rrect_sdf(p - float2(0.0, 4.0), u.shape_rect, r);
        if (sdf_shadow < 22.0) {
            float sh_d = max(sdf_shadow, 0.0);
            float sh_u = sh_d / 7.0;
            float sh_a = 0.16 * shape_alpha * exp(-0.5 * sh_u * sh_u);
            out = blend_over(out, float4(0.0, 0.0, 0.0, sh_a));
        }
    }

    // 2. Optional Reduce Motion ghost silhouette (`shape.wgsl`).
    float3 bg_rgb = float3(20.0 / 255.0, 20.0 / 255.0, 22.0 / 255.0);
    if (is_glass < 0.5 && u.has_ghost != 0u && u.ghost_meta.y > 0.001) {
        float g_sdf = rrect_sdf(p, u.ghost_rect, u.ghost_meta.x);
        float g_cov = clamp(0.5 - g_sdf * u.scale, 0.0, 1.0);
        float g_a = 0.96 * u.ghost_meta.y * g_cov;
        out = blend_over(out, float4(bg_rgb * g_a, g_a));
    }

    // 3. Main capsule silhouette fill (opaque dark in black mode; transparent in glass mode where
    //    NSGlassEffectView renders the real Liquid Glass bottle + lensing behind the Metal layer).
    //    When `u.shape_meta.z` (`shape.blur`, sigma in pt) > 0 on Entry/Exit, feather the SDF over
    //    `±1.75 * sigma` (matching the cumulative normal distribution of a 2D Gaussian blur).
    float half_band = max(0.5 / u.scale, u.shape_meta.z * 1.75);
    float in_cov = smoothstep(half_band, -half_band, sdf);
    if (in_cov > 0.001) {
        if (is_glass < 0.5) {
            float fill_a = 0.96 * shape_alpha * in_cov;
            out = blend_over(out, float4(bg_rgb * fill_a, fill_a));
        }

        // 4. Composite per-element quads with 1D Vertical Gaussian blur from `offscreen_b`,
        //    clipped to the capsule interior (`in_cov`).
        float2 inv_canvas = 1.0 / u.canvas_size;
        uint count = min(u.elem_count, 16u);
        for (uint i = 0; i < count; ++i) {
            CompositeElem e = elems[i];
            float exp_pad = e.pad_pt * e.scale;
            float2 min_p = e.screen_rect.xy - float2(exp_pad);
            float2 max_p = e.screen_rect.xy + e.screen_rect.zw + float2(exp_pad);
            if (p.x < min_p.x || p.x > max_p.x || p.y < min_p.y || p.y > max_p.y) {
                continue;
            }
            float2 local_pt = (p - e.screen_rect.xy) / max(e.scale, 0.05) + float2(e.pad_pt);
            float2 tile_pt = e.tile_rect.xy + local_pt;
            float min_ty = e.tile_rect.y + 0.5;
            float max_ty = e.tile_rect.y + e.tile_rect.w - 0.5;
            if (tile_pt.x < e.tile_rect.x || tile_pt.x > e.tile_rect.x + e.tile_rect.z) {
                continue;
            }

            float4 sample_col = float4(0.0);
            float sigma = e.sigma_pt;
            if (sigma < 0.05) {
                if (tile_pt.y >= min_ty && tile_pt.y <= max_ty) {
                    sample_col = offscreen_b.sample(s, tile_pt * inv_canvas);
                }
            } else {
                int radius = min(24, int(ceil(sigma * 3.0 * u.scale)));
                float step_pt = 1.0 / u.scale;
                float inv_2s2 = 0.5 / max(sigma * sigma, 1e-4);
                float w_sum = 0.0;
                for (int k = -radius; k <= radius; ++k) {
                    float dy = float(k) * step_pt;
                    float w = exp(-(dy * dy) * inv_2s2);
                    float sy = clamp(tile_pt.y + dy, min_ty, max_ty);
                    sample_col += offscreen_b.sample(s, float2(tile_pt.x, sy) * inv_canvas) * w;
                    w_sum += w;
                }
                sample_col /= max(w_sum, 1e-5);
                // Compensate for 2D Gaussian peak-alpha dilution on thin (1.5 pt) glyph/icon strokes
                // so blurred text and chips remain bright and visually unmistakable in motion.
                if (sample_col.a > 1e-4) {
                    float blur_boost = 1.0 + min(sigma, 8.0) * 0.35;
                    float boosted_a = min(1.0, sample_col.a * blur_boost);
                    sample_col *= (boosted_a / sample_col.a);
                }
            }
            // In Liquid Glass mode, add a subtle 1pt downward dark text/icon backing shadow so
            // white labels and the red dot stay crisp over bright white pages as well as dark ones.
            if (is_glass >= 0.5 && sigma < 3.5) {
                float sh_y = clamp(tile_pt.y - 0.85, min_ty, max_ty);
                float sh_a = offscreen_b.sample(s, float2(tile_pt.x, sh_y) * inv_canvas).a;
                float backing_a = min(1.0, sh_a * 0.64) * e.alpha * in_cov;
                out = blend_over(out, float4(0.0, 0.0, 0.0, backing_a));
            }
            float eff_a = e.alpha * in_cov;
            out = blend_over(out, sample_col * eff_a);
        }
    }

    // 5. Analytic 1px specular hairline border on the capsule rim (fades out while the shell is blurred
    //    on Entry/Exit so a sharp 1px ring never sits around a blurred capsule).
    //    In glass mode, only tint the border green when Completed (NSGlassEffectView draws its own
    //    specular glass rim otherwise).
    if (is_glass < 0.5 || u.is_completed != 0u) {
        float border_sharp = clamp(1.0 - u.shape_meta.z * 0.22, 0.0, 1.0);
        float border_cov = clamp(1.0 + 0.5 / u.scale + sdf, 0.0, 1.0)
                         * clamp(0.5 / u.scale - sdf, 0.0, 1.0)
                         * border_sharp;
        if (border_cov > 0.001) {
            float3 border_rgb = (u.is_completed != 0u)
                ? float3(105.0 / 255.0, 216.0 / 255.0, 159.0 / 255.0)
                : float3(1.0, 1.0, 1.0);
            float border_max_a = (u.is_completed != 0u) ? 0.42 : 0.12;
            float ba = border_cov * border_max_a * shape_alpha;
            out = blend_over(out, float4(border_rgb * ba, ba));
        }
    }

    return out;
}
