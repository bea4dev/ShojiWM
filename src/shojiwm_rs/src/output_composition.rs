//! Custom output composition: `COMPOSITOR.rendering.composition`.
//!
//! An output is normally drawn as layer-shell Background and Bottom surfaces,
//! the windows, Top and Overlay surfaces, then layer popups. A composition
//! function returns that stacking as an [`OutputStack`] and can rearrange it,
//! render parts of it into textures ([`RenderTexture`]), and draw those
//! textures flat ([`TextureView`]) or in 3D ([`Scene3D`] with [`Plane`]s).
//!
//! | TSX                         | Rust                                   |
//! |-----------------------------|----------------------------------------|
//! | `<DefaultComposition />`    | [`OutputStack::default_stacking`]      |
//! | `<Layers layers={…} />`     | [`Layers::new`]                        |
//! | `<Windows windows={…} />`   | [`Windows::all`] / [`Windows::only`]   |
//! | `<LayerPopups />`           | [`LayerPopups`]                        |
//! | `<TextureView texture={…}>` | [`TextureView::new`]                   |
//! | `<Solid color={…} />`       | [`Solid::new`]                         |
//! | `<Scene3D camera={…}>`      | [`Scene3D::new`]                       |
//! | `<Plane texture={…} …>`     | [`Plane::new`]                         |
//! | `renderTexture({ … })`      | [`RenderTexture::new`]                 |
//!
//! The function runs again only when signals it read change, never per
//! frame by itself; animate by driving signals (e.g. from
//! [`set_interval`](crate::animation::set_interval)). Children draw back to
//! front: later children are on top.
//!
//! ```no_run
//! use shojiwm_rs::prelude::*;
//!
//! let cube = signal(false);
//! COMPOSITOR.rendering.composition(move |output| {
//!     if !cube.get() {
//!         return OutputStack::default_stacking();
//!     }
//!     let (width, height) = output_logical_size(output);
//!     let face = RenderTexture::new()
//!         .child(Layers::new([LayerName::Background, LayerName::Bottom]))
//!         .child(Windows::all());
//!     OutputStack::new()
//!         .child(
//!             Scene3D::new(screen_camera(width, height, ScreenCamera::default()))
//!                 .plane(Plane::new(&face, width, height).transform(transform3d().rotate_y(30.0))),
//!         )
//!         .child(Layers::new([LayerName::Top, LayerName::Overlay]))
//!         .child(LayerPopups)
//! });
//! ```

use std::rc::Rc;

use shojiwm_lib::{
    backend::composition::{
        CompositionNode, Object3d, OutputComposition, RectF, Scene3dSpec, TextureSpec,
    },
    ssd::{Color, WaylandOutputSnapshot},
};

use crate::window::Window;

/// The compiled plan types the compositor consumes.
pub use shojiwm_lib::backend::composition as plan;
pub use shojiwm_lib::backend::composition::LayerKind as LayerName;

/// Column-major 4x4 matrix (16 numbers).
pub type Mat4 = [f64; 16];

const IDENTITY: Mat4 = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];

/// A color for a composition node: straight-alpha RGBA in 0..1, or a
/// [`Color`] (`hex("#00000080")`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompositionColor(pub [f64; 4]);

impl From<[f64; 4]> for CompositionColor {
    fn from(rgba: [f64; 4]) -> Self {
        Self(rgba)
    }
}

impl From<Color> for CompositionColor {
    fn from(color: Color) -> Self {
        Self([color.r, color.g, color.b, color.a].map(|channel| channel as f64 / 255.0))
    }
}

impl CompositionColor {
    pub const TRANSPARENT: Self = Self([0.0; 4]);

    /// Premultiplied, clamped to 0..1.
    fn premultiplied(self) -> [f32; 4] {
        let [r, g, b, a] = self.0.map(|channel| {
            if channel.is_finite() {
                channel.clamp(0.0, 1.0)
            } else {
                0.0
            }
        });
        [(r * a) as f32, (g * a) as f32, (b * a) as f32, a as f32]
    }
}

/// A rectangle in logical pixels relative to the composition. Missing
/// extents reach to the composition's far edge.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CompositionRect {
    pub x: f64,
    pub y: f64,
    pub width: Option<f64>,
    pub height: Option<f64>,
}

impl CompositionRect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width: Some(width),
            height: Some(height),
        }
    }

    fn wire(self) -> RectF {
        RectF {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }
}

/// Camera matrices of a [`Scene3D`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub projection: Mat4,
    pub view: Mat4,
}

/// One child of an [`OutputStack`] or a [`RenderTexture`].
#[derive(Debug, Clone)]
pub enum OutputNode {
    Layers(Layers),
    Windows(Windows),
    LayerPopups,
    TextureView(TextureView),
    Solid(Solid),
    Scene3D(Box<Scene3D>),
}

/// The children of a composition, back to front.
#[derive(Debug, Clone, Default)]
pub struct OutputStack {
    children: Vec<OutputNode>,
}

impl OutputStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// The stacking every output has without a custom composition
    /// (`<DefaultComposition />`).
    pub fn default_stacking() -> Self {
        Self::new().children(default_nodes())
    }

    pub fn child(mut self, child: impl Into<OutputNode>) -> Self {
        self.children.push(child.into());
        self
    }

    pub fn children<N: Into<OutputNode>>(mut self, children: impl IntoIterator<Item = N>) -> Self {
        self.children.extend(children.into_iter().map(Into::into));
        self
    }
}

/// The nodes of [`OutputStack::default_stacking`], to splice into a stack.
pub fn default_nodes() -> Vec<OutputNode> {
    vec![
        Layers::new([LayerName::Background, LayerName::Bottom]).into(),
        Windows::all().into(),
        Layers::new([LayerName::Top, LayerName::Overlay]).into(),
        OutputNode::LayerPopups,
    ]
}

/// Layer-shell surfaces of the output (`<Layers>`), back to front.
#[derive(Debug, Clone)]
pub struct Layers(Vec<LayerName>);

impl Layers {
    pub fn new(layers: impl IntoIterator<Item = LayerName>) -> Self {
        Self(layers.into_iter().collect())
    }
}

/// Popups of layer-shell surfaces (tooltips of bars, ...), `<LayerPopups />`.
#[derive(Debug, Clone, Copy)]
pub struct LayerPopups;

/// The window stack (`<Windows>`).
#[derive(Debug, Clone, Default)]
pub struct Windows {
    windows: Option<Vec<String>>,
    offset_x: f64,
    offset_y: f64,
}

impl Windows {
    /// The windows the output normally shows, including closing windows.
    pub fn all() -> Self {
        Self::default()
    }

    /// Exactly these windows (or window ids), drawn in stacking order even
    /// when hidden — e.g. the windows of another workspace.
    pub fn only<W: Into<WindowRef>>(windows: impl IntoIterator<Item = W>) -> Self {
        Self {
            windows: Some(windows.into_iter().map(|window| window.into().0).collect()),
            ..Self::default()
        }
    }

    /// Shift the windows, in logical pixels.
    pub fn offset(mut self, x: f64, y: f64) -> Self {
        self.offset_x = x;
        self.offset_y = y;
        self
    }
}

/// A window or a window id, for [`Windows::only`].
pub struct WindowRef(String);

impl From<Window> for WindowRef {
    fn from(window: Window) -> Self {
        Self(window.id())
    }
}

impl From<String> for WindowRef {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<&str> for WindowRef {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

/// A texture drawn flat (`<TextureView>`), by default over the whole
/// composition.
#[derive(Debug, Clone)]
pub struct TextureView {
    texture: RenderTexture,
    rect: Option<CompositionRect>,
    opacity: f64,
}

impl TextureView {
    pub fn new(texture: &RenderTexture) -> Self {
        Self {
            texture: texture.clone(),
            rect: None,
            opacity: 1.0,
        }
    }

    pub fn rect(mut self, rect: CompositionRect) -> Self {
        self.rect = Some(rect);
        self
    }

    pub fn opacity(mut self, opacity: f64) -> Self {
        self.opacity = opacity;
        self
    }
}

/// A solid color fill (`<Solid>`), by default over the whole composition.
#[derive(Debug, Clone)]
pub struct Solid {
    color: CompositionColor,
    rect: Option<CompositionRect>,
}

impl Solid {
    pub fn new(color: impl Into<CompositionColor>) -> Self {
        Self {
            color: color.into(),
            rect: None,
        }
    }

    pub fn rect(mut self, rect: CompositionRect) -> Self {
        self.rect = Some(rect);
        self
    }
}

/// A 3D scene of [`Plane`]s, rendered with depth and drawn as one layer of
/// the composition (`<Scene3D>`).
#[derive(Debug, Clone)]
pub struct Scene3D {
    camera: Camera,
    rect: Option<CompositionRect>,
    clear_color: CompositionColor,
    antialias: bool,
    planes: Vec<Plane>,
}

impl Scene3D {
    pub fn new(camera: Camera) -> Self {
        Self {
            camera,
            rect: None,
            clear_color: CompositionColor::TRANSPARENT,
            antialias: true,
            planes: Vec::new(),
        }
    }

    pub fn rect(mut self, rect: CompositionRect) -> Self {
        self.rect = Some(rect);
        self
    }

    /// Default transparent.
    pub fn clear_color(mut self, color: impl Into<CompositionColor>) -> Self {
        self.clear_color = color.into();
        self
    }

    /// Multisampled edges (default true).
    pub fn antialias(mut self, antialias: bool) -> Self {
        self.antialias = antialias;
        self
    }

    pub fn plane(mut self, plane: Plane) -> Self {
        self.planes.push(plane);
        self
    }

    pub fn planes(mut self, planes: impl IntoIterator<Item = Plane>) -> Self {
        self.planes.extend(planes);
        self
    }
}

/// A textured rectangle in a [`Scene3D`] (`<Plane>`), centred on its
/// origin in the XY plane.
#[derive(Debug, Clone)]
pub struct Plane {
    texture: RenderTexture,
    width: f64,
    height: f64,
    transform: Mat4,
    opacity: f64,
    double_sided: bool,
}

impl Plane {
    /// `width` x `height` world units.
    pub fn new(texture: &RenderTexture, width: f64, height: f64) -> Self {
        Self {
            texture: texture.clone(),
            width,
            height,
            transform: IDENTITY,
            opacity: 1.0,
            double_sided: true,
        }
    }

    /// Placement in the world (default identity).
    pub fn transform(mut self, transform: impl Into<Mat4Like>) -> Self {
        self.transform = transform.into().0;
        self
    }

    pub fn opacity(mut self, opacity: f64) -> Self {
        self.opacity = opacity;
        self
    }

    /// Draw the back face too (default true).
    pub fn double_sided(mut self, double_sided: bool) -> Self {
        self.double_sided = double_sided;
        self
    }
}

/// A matrix or a [`Transform3D`].
pub struct Mat4Like(pub Mat4);

impl From<Mat4> for Mat4Like {
    fn from(matrix: Mat4) -> Self {
        Self(matrix)
    }
}

impl From<Transform3D> for Mat4Like {
    fn from(transform: Transform3D) -> Self {
        Self(transform.matrix)
    }
}

impl From<&Transform3D> for Mat4Like {
    fn from(transform: &Transform3D) -> Self {
        Self(transform.matrix)
    }
}

#[derive(Debug, Clone)]
struct RenderTextureSpec {
    key: Option<String>,
    width: Option<f64>,
    height: Option<f64>,
    scale: Option<f64>,
    clear_color: CompositionColor,
    children: Vec<OutputNode>,
}

/// Composition nodes rendered into a texture (`renderTexture()`), to draw
/// with a [`TextureView`] or a [`Plane`]. The same texture used in several
/// places renders once per frame, and only re-renders where its content
/// changed. Build it completely before handing it to a view or plane:
/// clones share the texture.
#[derive(Debug, Clone)]
pub struct RenderTexture(Rc<RenderTextureSpec>);

impl Default for RenderTexture {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderTexture {
    pub fn new() -> Self {
        Self(Rc::new(RenderTextureSpec {
            key: None,
            width: None,
            height: None,
            scale: None,
            clear_color: CompositionColor::TRANSPARENT,
            children: Vec::new(),
        }))
    }

    fn edit(mut self, f: impl FnOnce(&mut RenderTextureSpec)) -> Self {
        f(Rc::make_mut(&mut self.0));
        self
    }

    /// Keeps the GPU texture across re-evaluations. Defaults to the order
    /// textures are first used in, which is stable as long as the
    /// composition's shape is.
    pub fn key(self, key: impl Into<String>) -> Self {
        let key = key.into();
        self.edit(|spec| spec.key = Some(key))
    }

    /// Logical size; the output's by default.
    pub fn size(self, width: f64, height: f64) -> Self {
        self.edit(|spec| {
            spec.width = Some(width);
            spec.height = Some(height);
        })
    }

    /// Pixel density; the output's scale by default.
    pub fn scale(self, scale: f64) -> Self {
        self.edit(|spec| spec.scale = Some(scale))
    }

    pub fn clear_color(self, color: impl Into<CompositionColor>) -> Self {
        let color = color.into();
        self.edit(|spec| spec.clear_color = color)
    }

    pub fn child(self, child: impl Into<OutputNode>) -> Self {
        let child = child.into();
        self.edit(|spec| spec.children.push(child))
    }

    pub fn children<N: Into<OutputNode>>(self, children: impl IntoIterator<Item = N>) -> Self {
        let children: Vec<OutputNode> = children.into_iter().map(Into::into).collect();
        self.edit(|spec| spec.children.extend(children))
    }
}

macro_rules! into_node {
    ($($ty:ident),*) => {
        $(impl From<$ty> for OutputNode {
            fn from(node: $ty) -> Self {
                Self::$ty(node)
            }
        })*
    };
}

into_node!(Layers, Windows, TextureView, Solid);

impl From<Scene3D> for OutputNode {
    fn from(scene: Scene3D) -> Self {
        Self::Scene3D(Box::new(scene))
    }
}

impl From<LayerPopups> for OutputNode {
    fn from(_: LayerPopups) -> Self {
        Self::LayerPopups
    }
}

// ---------------------------------------------------------------------------
// Matrices

/// `a * b` (column-major).
pub fn multiply_mat4(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            out[column * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[column * 4 + k]).sum();
        }
    }
    out
}

/// A 3D transform built like CSS `transform`: each call applies in the local
/// space of the ones before it, so `transform3d().rotate_y(30.0).translate(0.0,
/// 0.0, 100.0)` turns the plane, then pushes it 100 units along its own
/// normal. Angles are in degrees; +Y is up, +Z towards the camera.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform3D {
    pub matrix: Mat4,
}

impl Default for Transform3D {
    fn default() -> Self {
        Self { matrix: IDENTITY }
    }
}

impl Transform3D {
    pub fn new(matrix: Mat4) -> Self {
        Self { matrix }
    }

    pub fn multiply(self, other: impl Into<Mat4Like>) -> Self {
        Self::new(multiply_mat4(&self.matrix, &other.into().0))
    }

    pub fn translate(self, x: f64, y: f64, z: f64) -> Self {
        self.multiply([
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, x, y, z, 1.0,
        ])
    }

    /// Scale X and Y; Z stays (like the TypeScript `scale(x)`).
    pub fn scale(self, factor: f64) -> Self {
        self.scale_xyz(factor, factor, 1.0)
    }

    pub fn scale_xyz(self, x: f64, y: f64, z: f64) -> Self {
        self.multiply([
            x, 0.0, 0.0, 0.0, 0.0, y, 0.0, 0.0, 0.0, 0.0, z, 0.0, 0.0, 0.0, 0.0, 1.0,
        ])
    }

    pub fn rotate_x(self, degrees: f64) -> Self {
        let (s, c) = degrees.to_radians().sin_cos();
        self.multiply([
            1.0, 0.0, 0.0, 0.0, 0.0, c, s, 0.0, 0.0, -s, c, 0.0, 0.0, 0.0, 0.0, 1.0,
        ])
    }

    pub fn rotate_y(self, degrees: f64) -> Self {
        let (s, c) = degrees.to_radians().sin_cos();
        self.multiply([
            c, 0.0, -s, 0.0, 0.0, 1.0, 0.0, 0.0, s, 0.0, c, 0.0, 0.0, 0.0, 0.0, 1.0,
        ])
    }

    pub fn rotate_z(self, degrees: f64) -> Self {
        let (s, c) = degrees.to_radians().sin_cos();
        self.multiply([
            c, s, 0.0, 0.0, -s, c, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ])
    }
}

/// Start a [`Transform3D`] (identity).
pub fn transform3d() -> Transform3D {
    Transform3D::default()
}

/// OpenGL-style perspective projection. `fov_y` in degrees.
pub fn perspective(fov_y: f64, aspect: f64, near: f64, far: f64) -> Mat4 {
    let f = 1.0 / (fov_y.to_radians() / 2.0).tan();
    let depth = 1.0 / (near - far);
    [
        f / aspect,
        0.0,
        0.0,
        0.0, //
        0.0,
        f,
        0.0,
        0.0, //
        0.0,
        0.0,
        (far + near) * depth,
        -1.0, //
        0.0,
        0.0,
        2.0 * far * near * depth,
        0.0,
    ]
}

type Vec3 = [f64; 3];

/// View matrix of a camera at `eye` looking at `target`, +Y up.
pub fn look_at(eye: Vec3, target: Vec3) -> Mat4 {
    look_at_up(eye, target, [0.0, 1.0, 0.0])
}

pub fn look_at_up(eye: Vec3, target: Vec3, up: Vec3) -> Mat4 {
    let sub = |a: Vec3, b: Vec3| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let normalize = |v: Vec3| {
        let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        let length = if length == 0.0 { 1.0 } else { length };
        [v[0] / length, v[1] / length, v[2] / length]
    };
    let cross = |a: Vec3, b: Vec3| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let dot = |a: Vec3, b: Vec3| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let z = normalize(sub(eye, target));
    let x = normalize(cross(up, z));
    let y = cross(z, x);
    [
        x[0],
        y[0],
        z[0],
        0.0, //
        x[1],
        y[1],
        z[1],
        0.0, //
        x[2],
        y[2],
        z[2],
        0.0, //
        -dot(x, eye),
        -dot(y, eye),
        -dot(z, eye),
        1.0,
    ]
}

/// Logical size of an output (its resolution over its scale), `(width,
/// height)`; `(0, 0)` without a mode.
pub fn output_logical_size(output: &WaylandOutputSnapshot) -> (f64, f64) {
    let scale = if output.scale > 0.0 {
        output.scale
    } else {
        1.0
    };
    output
        .resolution
        .map(|resolution| {
            (
                (resolution.width as f64 / scale).round(),
                (resolution.height as f64 / scale).round(),
            )
        })
        .unwrap_or((0.0, 0.0))
}

/// Options of [`screen_camera`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenCamera {
    /// Vertical field of view in degrees (default 45).
    pub fov: f64,
    /// Move the camera back by this many units (zoom out).
    pub distance: f64,
}

impl Default for ScreenCamera {
    fn default() -> Self {
        Self {
            fov: 45.0,
            distance: 0.0,
        }
    }
}

/// A camera that maps the world's XY plane at z = 0 onto a `width` x
/// `height` viewport 1:1 in logical pixels: a [`Plane`] of that size at the
/// origin covers it exactly. Use it to start 3D effects from the flat
/// desktop.
pub fn screen_camera(width: f64, height: f64, options: ScreenCamera) -> Camera {
    let fit = height / 2.0 / (options.fov.to_radians() / 2.0).tan();
    let distance = fit + options.distance;
    let depth = width.max(height) * 4.0 + distance;
    Camera {
        projection: perspective(
            options.fov,
            width / height.max(1.0),
            (distance / 100.0).max(0.1),
            depth,
        ),
        view: look_at([0.0, 0.0, distance], [0.0, 0.0, 0.0]),
    }
}

/// Where a world point lands on a scene's viewport.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProjectedPoint {
    /// Logical pixels from the viewport's top-left corner.
    pub x: f64,
    pub y: f64,
    /// NDC z; smaller is nearer.
    pub depth: f64,
}

/// Where `point` lands on a `(width, height)` viewport seen through `camera`;
/// `None` behind the camera.
pub fn project_point(camera: &Camera, viewport: (f64, f64), point: Vec3) -> Option<ProjectedPoint> {
    let clip = transform_point(&multiply_mat4(&camera.projection, &camera.view), point);
    if clip[3] <= 1e-6 {
        return None;
    }
    let ndc_x = clip[0] / clip[3];
    let ndc_y = clip[1] / clip[3];
    Some(ProjectedPoint {
        x: (ndc_x + 1.0) / 2.0 * viewport.0,
        y: (1.0 - ndc_y) / 2.0 * viewport.1,
        depth: clip[2] / clip[3],
    })
}

fn transform_point(matrix: &Mat4, [x, y, z]: Vec3) -> [f64; 4] {
    [
        matrix[0] * x + matrix[4] * y + matrix[8] * z + matrix[12],
        matrix[1] * x + matrix[5] * y + matrix[9] * z + matrix[13],
        matrix[2] * x + matrix[6] * y + matrix[10] * z + matrix[14],
        matrix[3] * x + matrix[7] * y + matrix[11] * z + matrix[15],
    ]
}

/// A plane as [`pick_plane`] sees it: the same numbers as its [`Plane`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PickablePlane {
    pub width: f64,
    pub height: f64,
    pub transform: Mat4,
}

/// Hit-test planes the way a [`Scene3D`] with `camera` draws them: the index
/// of the nearest plane under `(x, y)` (logical pixels from the viewport's
/// top-left corner). Use it to make a 3D layout clickable under an input
/// grab.
pub fn pick_plane(
    camera: &Camera,
    viewport: (f64, f64),
    planes: &[PickablePlane],
    x: f64,
    y: f64,
) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (index, plane) in planes.iter().enumerate() {
        let (half_width, half_height) = (plane.width / 2.0, plane.height / 2.0);
        let corners = [
            [-half_width, half_height, 0.0],
            [half_width, half_height, 0.0],
            [half_width, -half_height, 0.0],
            [-half_width, -half_height, 0.0],
        ];
        let projected: Option<Vec<ProjectedPoint>> = corners
            .iter()
            .map(|corner| {
                let world = transform_point(&plane.transform, *corner);
                project_point(camera, viewport, [world[0], world[1], world[2]])
            })
            .collect();
        let Some(quad) = projected else {
            continue;
        };
        if !point_in_convex_quad(&quad, x, y) {
            continue;
        }
        let depth = quad.iter().map(|point| point.depth).sum::<f64>() / 4.0;
        if best.is_none_or(|(_, best_depth)| depth < best_depth) {
            best = Some((index, depth));
        }
    }
    best.map(|(index, _)| index)
}

fn point_in_convex_quad(quad: &[ProjectedPoint], x: f64, y: f64) -> bool {
    let mut sign = 0.0;
    for (i, a) in quad.iter().enumerate() {
        let b = &quad[(i + 1) % quad.len()];
        let cross = (b.x - a.x) * (y - a.y) - (b.y - a.y) * (x - a.x);
        if cross == 0.0 {
            continue;
        }
        let side = cross.signum();
        if sign == 0.0 {
            sign = side;
        } else if side != sign {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Plan

fn to_f32(matrix: &Mat4) -> [f32; 16] {
    matrix.map(|value| value as f32)
}

fn finite(value: f64, name: &str) -> Result<f64, String> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(format!("{name} must be a finite number"))
    }
}

fn unit(value: f64, name: &str) -> Result<f32, String> {
    Ok(finite(value, name)?.clamp(0.0, 1.0) as f32)
}

fn matrix(value: &Mat4, name: &str) -> Result<[f32; 16], String> {
    if value.iter().all(|entry| entry.is_finite()) {
        Ok(to_f32(value))
    } else {
        Err(format!("{name} must be a 4x4 matrix of finite numbers"))
    }
}

fn rect(rect: Option<CompositionRect>) -> Result<Option<RectF>, String> {
    let Some(rect) = rect else {
        return Ok(None);
    };
    finite(rect.x, "x")?;
    finite(rect.y, "y")?;
    if let Some(width) = rect.width {
        finite(width, "width")?;
    }
    if let Some(height) = rect.height {
        finite(height, "height")?;
    }
    Ok(Some(rect.wire()))
}

#[derive(Default)]
struct PlanBuilder {
    textures: Vec<TextureSpec>,
    indices: Vec<(Rc<RenderTextureSpec>, usize)>,
    building: Vec<Rc<RenderTextureSpec>>,
}

impl PlanBuilder {
    fn texture_index(&mut self, texture: &RenderTexture) -> Result<usize, String> {
        if let Some((_, index)) = self
            .indices
            .iter()
            .find(|(spec, _)| Rc::ptr_eq(spec, &texture.0))
        {
            return Ok(*index);
        }
        if self
            .building
            .iter()
            .any(|spec| Rc::ptr_eq(spec, &texture.0))
        {
            return Err("A render texture cannot show itself".to_owned());
        }
        self.building.push(texture.0.clone());
        let nodes = self.nodes(&texture.0.children);
        self.building.pop();
        let nodes = nodes?;
        let spec = &texture.0;
        let index = self.textures.len();
        self.textures.push(TextureSpec {
            key: spec.key.clone().unwrap_or_else(|| format!("#{index}")),
            width: spec.width.map(|width| finite(width, "width")).transpose()?,
            height: spec
                .height
                .map(|height| finite(height, "height"))
                .transpose()?,
            scale: spec.scale.map(|scale| finite(scale, "scale")).transpose()?,
            clear_color: spec.clear_color.premultiplied(),
            nodes,
        });
        self.indices.push((texture.0.clone(), index));
        Ok(index)
    }

    fn nodes(&mut self, children: &[OutputNode]) -> Result<Vec<CompositionNode>, String> {
        children.iter().map(|child| self.node(child)).collect()
    }

    fn node(&mut self, node: &OutputNode) -> Result<CompositionNode, String> {
        Ok(match node {
            OutputNode::Layers(Layers(layers)) => CompositionNode::Layers {
                layers: layers.to_vec(),
            },
            OutputNode::LayerPopups => CompositionNode::LayerPopups,
            OutputNode::Windows(windows) => CompositionNode::Windows {
                windows: windows.windows.clone(),
                offset_x: finite(windows.offset_x, "offset_x")?.round() as i32,
                offset_y: finite(windows.offset_y, "offset_y")?.round() as i32,
            },
            OutputNode::TextureView(view) => CompositionNode::TextureView {
                texture: self.texture_index(&view.texture)?,
                rect: rect(view.rect)?,
                opacity: unit(view.opacity, "opacity")?,
            },
            OutputNode::Solid(solid) => CompositionNode::Solid {
                rect: rect(solid.rect)?,
                color: solid.color.premultiplied(),
            },
            OutputNode::Scene3D(scene) => {
                let objects = scene
                    .planes
                    .iter()
                    .map(|plane| {
                        Ok(Object3d::Plane {
                            texture: self.texture_index(&plane.texture)?,
                            width: finite(plane.width, "width")? as f32,
                            height: finite(plane.height, "height")? as f32,
                            model: matrix(&plane.transform, "transform")?,
                            opacity: unit(plane.opacity, "opacity")?,
                            double_sided: plane.double_sided,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                CompositionNode::Scene3d(Scene3dSpec {
                    rect: rect(scene.rect)?,
                    projection: matrix(&scene.camera.projection, "camera.projection")?,
                    view: matrix(&scene.camera.view, "camera.view")?,
                    clear_color: scene.clear_color.premultiplied(),
                    antialias: scene.antialias,
                    objects,
                })
            }
        })
    }
}

impl OutputStack {
    /// The plan the compositor walks.
    pub fn compile(&self) -> Result<OutputComposition, String> {
        let mut builder = PlanBuilder::default();
        let nodes = builder.nodes(&self.children)?;
        let plan = OutputComposition {
            nodes,
            textures: builder.textures,
        };
        plan.validate()?;
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_stacking_matches_the_compositor_default() {
        let plan = OutputStack::default_stacking().compile().unwrap();
        assert_eq!(plan, *OutputComposition::default_plan());
    }

    #[test]
    fn shared_textures_are_declared_once() {
        let texture = RenderTexture::new().key("face").child(Windows::only(["a"]));
        let plan = OutputStack::new()
            .child(TextureView::new(&texture))
            .child(
                Scene3D::new(screen_camera(800.0, 600.0, ScreenCamera::default()))
                    .plane(Plane::new(&texture, 800.0, 600.0)),
            )
            .compile()
            .unwrap();
        assert_eq!(plan.textures.len(), 1);
        assert_eq!(plan.textures[0].key, "face");
    }

    #[test]
    fn screen_camera_maps_the_plane_one_to_one() {
        let camera = screen_camera(800.0, 600.0, ScreenCamera::default());
        let corner = project_point(&camera, (800.0, 600.0), [-400.0, 300.0, 0.0]).unwrap();
        assert!(corner.x.abs() < 1e-6 && corner.y.abs() < 1e-6);
        let planes = [PickablePlane {
            width: 100.0,
            height: 100.0,
            transform: IDENTITY,
        }];
        assert_eq!(
            pick_plane(&camera, (800.0, 600.0), &planes, 400.0, 300.0),
            Some(0)
        );
        assert_eq!(
            pick_plane(&camera, (800.0, 600.0), &planes, 10.0, 10.0),
            None
        );
    }

    #[test]
    fn colors_are_premultiplied() {
        let plan = OutputStack::new()
            .child(Solid::new([1.0, 0.5, 0.0, 0.5]))
            .compile()
            .unwrap();
        assert_eq!(
            plan.nodes[0],
            CompositionNode::Solid {
                rect: None,
                color: [0.5, 0.25, 0.0, 0.5]
            }
        );
    }
}
