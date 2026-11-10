// The window's border, which for a moment after the window takes focus grows
// thicker outward (inward it would be under the client) and thin again. At
// `thicken` 0 this is exactly the built-in border.

uniform float thicken;  // logical px added to the border's width, outward

vec4 paint_main(PaintContext ctx) {
    vec2 p = ctx.frag_phy_px;
    float inner = shoji_rrect_sdf(p, ctx.inner_rect_phy_px, ctx.inner_radius_phy_px);
    vec4 fill = ctx.background * shoji_fill_coverage(ctx);
    // Well inside, where the border never comes.
    if (inner < -1.0) {
        return fill;
    }
    float outer = shoji_rrect_sdf(p, vec4(vec2(0.0), ctx.size_phy_px), ctx.radius_phy_px);
    float wider = max(thicken, 0.0) * ctx.scale;
    float ring = shoji_coverage(outer - wider) - shoji_coverage(inner);
    return shoji_over(ctx.border_color * max(ring, 0.0), fill);
}
