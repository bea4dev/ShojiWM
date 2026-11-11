/**
 * Custom output composition: `COMPOSITOR.rendering.composition`.
 *
 * An output is normally drawn as layer-shell Background and Bottom surfaces,
 * the windows, Top and Overlay surfaces, then layer popups. A composition
 * function returns that stacking as JSX and can rearrange it, render parts
 * of it into textures (`renderTexture`), and draw those textures flat
 * (`<TextureView>`) or in 3D (`<Scene3D>` with `<Plane>`s).
 *
 * The function runs again only when signals it reads change, never per
 * frame by itself; animate by driving signals (e.g. from `createPoll`).
 * Children draw back to front: later children are on top.
 *
 * 出力のカスタム合成: `COMPOSITOR.rendering.composition`。
 * 出力は通常、レイヤーシェルの Background・Bottom、ウィンドウ、Top・Overlay、
 * レイヤーのポップアップの順に描かれます。合成関数はこの重なりを JSX で返し、
 * 並べ替えたり、一部をテクスチャに描いたり（`renderTexture`）、テクスチャを平面
 * （`<TextureView>`）や 3D（`<Scene3D>` と `<Plane>`）で描いたりできます。
 * 関数は読んだ signal が変わったときだけ再評価されます（毎フレームではありません）。
 * 子要素は奥から手前の順で、後の子ほど上に描かれます。
 *
 * @example Cube between two workspaces / 2 つのワークスペース間のキューブ
 * ```tsx
 * COMPOSITOR.rendering.composition = (output) => {
 *   if (!cube.active()) return <DefaultComposition />;
 *   const { width, height } = outputLogicalSize(output);
 *   const face = (windows: string[]) =>
 *     renderTexture({
 *       content: (
 *         <>
 *           <Layers layers={["background", "bottom"]} />
 *           <Windows windows={windows} />
 *         </>
 *       ),
 *     });
 *   return (
 *     <>
 *       <Scene3D camera={screenCamera(output)}>
 *         <Plane texture={face(cube.from())} width={width} height={height}
 *           transform={transform3d().rotateY(cube.angle()).translate(0, 0, width / 2)} />
 *         <Plane texture={face(cube.to())} width={width} height={height}
 *           transform={transform3d().rotateY(cube.angle() + 90).translate(0, 0, width / 2)} />
 *       </Scene3D>
 *       <Layers layers={["top", "overlay"]} />
 *       <LayerPopups />
 *     </>
 *   );
 * };
 * ```
 */
import { createElementNode } from "./runtime";
import { isSignal, read, type ReadonlySignal } from "./signals";
import type {
  CompositionChild,
  CompositionElementNode,
  CompositionNodeType,
  CompositionRenderable,
  OutputInfo,
  WaylandWindow,
} from "./types";

type MaybeSignal<T> = T | ReadonlySignal<T>;

/** A layer-shell layer. / レイヤーシェルの層。 */
export type LayerName = "background" | "bottom" | "top" | "overlay";

/** Column-major 4x4 matrix (16 numbers). / 列優先の 4x4 行列（16 要素）。 */
export type Mat4 = readonly number[];

/** RGBA in 0..1 (straight alpha), or `#rgb`, `#rrggbb`, `#rrggbbaa`. */
export type CompositionColor = string | readonly [number, number, number, number];

/** A rectangle in logical pixels relative to the composition. / 合成内の論理ピクセル矩形。 */
export interface CompositionRect {
  x?: number;
  y?: number;
  width?: number;
  height?: number;
}

/** Camera matrices of a `<Scene3D>`. / `<Scene3D>` のカメラ行列。 */
export interface Camera {
  projection: Mat4;
  view: Mat4;
}

export interface LayersProps {
  /** Back to front. / 奥から手前の順。 */
  layers: MaybeSignal<LayerName | readonly LayerName[]>;
}

export interface WindowsProps {
  /**
   * Exactly these windows (or window ids), drawn in stacking order even when
   * hidden — e.g. the windows of another workspace. Omit for the windows the
   * output normally shows, including closing windows.
   * これらのウィンドウ（または ID）だけを、非表示でも重なり順で描きます（別の
   * ワークスペースのウィンドウなど）。省略すると出力が通常表示するウィンドウ。
   */
  windows?: MaybeSignal<readonly (WaylandWindow | string)[]>;
  /** Shift the windows, in logical pixels. / ウィンドウをずらす量（論理ピクセル）。 */
  offsetX?: MaybeSignal<number>;
  offsetY?: MaybeSignal<number>;
}

export interface TextureViewProps extends CompositionRect {
  texture: RenderTexture;
  opacity?: MaybeSignal<number>;
}

export interface SolidProps extends CompositionRect {
  color: MaybeSignal<CompositionColor>;
}

export interface Scene3DProps extends CompositionRect {
  camera: MaybeSignal<Camera>;
  /** Default transparent. / 既定は透明。 */
  clearColor?: MaybeSignal<CompositionColor>;
  /** Multisampled edges (default true). / エッジのアンチエイリアス（既定 true）。 */
  antialias?: MaybeSignal<boolean>;

  children?: CompositionRenderable | CompositionRenderable[];
}

export interface PlaneProps {
  texture: RenderTexture;
  /** Size in world units, centred on the plane's origin. / ワールド単位のサイズ（原点中心）。 */
  width: MaybeSignal<number>;
  height: MaybeSignal<number>;
  /** Placement in the world (default identity). / ワールドでの配置（既定は単位行列）。 */
  transform?: MaybeSignal<Mat4 | Transform3D>;
  opacity?: MaybeSignal<number>;
  /** Draw the back face too (default true). / 裏面も描く（既定 true）。 */
  doubleSided?: MaybeSignal<boolean>;
}

export interface RenderTextureOptions {
  /** What the texture shows: composition nodes (`<Windows>`, `<Layers>`, ...). */
  content: CompositionRenderable | CompositionRenderable[];
  /**
   * Keeps the GPU texture across re-evaluations. Defaults to the order of
   * `renderTexture` calls, which is stable as long as the composition's
   * shape is.
   * 再評価をまたいで GPU テクスチャを保つためのキー。既定は呼び出し順。
   */
  key?: string;
  /** Logical size; the output's by default. / 論理サイズ（既定は出力のサイズ）。 */
  width?: number;
  height?: number;
  /** Pixel density; the output's scale by default. / 画素密度（既定は出力の scale）。 */
  scale?: number;
  clearColor?: CompositionColor;
}

const RENDER_TEXTURE = Symbol("shoji.renderTexture");

/** A texture rendered from composition nodes. / 合成ノードから描かれるテクスチャ。 */
export interface RenderTexture {
  readonly [RENDER_TEXTURE]: true;
  readonly options: RenderTextureOptions;
}

/**
 * Render composition nodes into a texture, to draw with `<TextureView>` or a
 * `<Plane>`. The same texture used in several places renders once per frame,
 * and only re-renders where its content changed.
 * 合成ノードをテクスチャに描きます。`<TextureView>` や `<Plane>` で使います。複数箇所で
 * 使っても 1 フレームに 1 回だけ、内容が変わった部分だけ描き直されます。
 */
export function renderTexture(options: RenderTextureOptions): RenderTexture {
  return { [RENDER_TEXTURE]: true, options };
}

function isRenderTexture(value: unknown): value is RenderTexture {
  return typeof value === "object" && value !== null && RENDER_TEXTURE in value;
}

function intrinsic<TProps>(type: string) {
  return function OutputCompositionComponent(props: TProps): CompositionElementNode {
    return createElementNode(type as CompositionNodeType, props as Record<string, unknown>);
  };
}

/** Layer-shell surfaces of the output. / 出力のレイヤーシェルサーフェス。 */
export const Layers = intrinsic<LayersProps>("Layers");
/** The window stack. / ウィンドウの重なり。 */
export const Windows = intrinsic<WindowsProps>("Windows");
/** Popups of layer-shell surfaces (tooltips of bars, ...). / レイヤーのポップアップ。 */
export const LayerPopups = intrinsic<Record<string, never>>("LayerPopups");
/** A texture drawn flat. / テクスチャを平面で描く。 */
export const TextureView = intrinsic<TextureViewProps>("TextureView");
/** A solid color fill. / 単色の塗り。 */
export const Solid = intrinsic<SolidProps>("Solid");
/**
 * A 3D scene of `<Plane>`s, rendered with depth and drawn as one layer of
 * the composition.
 * `<Plane>` からなる 3D シーン。深度付きで描かれ、合成の 1 層として描かれます。
 */
export const Scene3D = intrinsic<Scene3DProps>("Scene3D");
/** A textured rectangle in a `<Scene3D>`. / `<Scene3D>` 内のテクスチャ付き矩形。 */
export const Plane = intrinsic<PlaneProps>("Plane");

/**
 * The stacking every output has without a custom composition.
 * カスタム合成が無いときの出力の重なりそのもの。
 */
export function DefaultComposition(): CompositionRenderable {
  return [
    Layers({ layers: ["background", "bottom"] }),
    Windows({}),
    Layers({ layers: ["top", "overlay"] }),
    LayerPopups({}),
  ] as unknown as CompositionRenderable;
}

// ---------------------------------------------------------------------------
// Matrices

const IDENTITY: Mat4 = Object.freeze([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);

/** `a * b` (column-major). */
export function multiplyMat4(a: Mat4, b: Mat4): number[] {
  const out = new Array<number>(16).fill(0);
  for (let column = 0; column < 4; column++) {
    for (let row = 0; row < 4; row++) {
      let sum = 0;
      for (let k = 0; k < 4; k++) sum += a[k * 4 + row] * b[column * 4 + k];
      out[column * 4 + row] = sum;
    }
  }
  return out;
}

const DEG = Math.PI / 180;

/**
 * A 3D transform built like CSS `transform`: each call applies in the local
 * space of the ones before it, so `transform3d().rotateY(30).translate(0, 0,
 * 100)` turns the plane, then pushes it 100 units along its own normal.
 * Angles are in degrees; +Y is up, +Z towards the camera.
 * CSS の `transform` と同じ要領で組み立てる 3D 変換。各呼び出しはそれまでの変換の
 * ローカル空間で適用されます。角度は度、+Y が上、+Z がカメラ側です。
 */
export class Transform3D {
  readonly matrix: number[];

  constructor(matrix: Mat4 = IDENTITY) {
    this.matrix = [...matrix];
  }

  multiply(other: Mat4 | Transform3D): Transform3D {
    const matrix = other instanceof Transform3D ? other.matrix : other;
    return new Transform3D(multiplyMat4(this.matrix, matrix));
  }

  translate(x: number, y = 0, z = 0): Transform3D {
    return this.multiply([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, x, y, z, 1]);
  }

  scale(x: number, y = x, z = 1): Transform3D {
    return this.multiply([x, 0, 0, 0, 0, y, 0, 0, 0, 0, z, 0, 0, 0, 0, 1]);
  }

  rotateX(degrees: number): Transform3D {
    const c = Math.cos(degrees * DEG);
    const s = Math.sin(degrees * DEG);
    return this.multiply([1, 0, 0, 0, 0, c, s, 0, 0, -s, c, 0, 0, 0, 0, 1]);
  }

  rotateY(degrees: number): Transform3D {
    const c = Math.cos(degrees * DEG);
    const s = Math.sin(degrees * DEG);
    return this.multiply([c, 0, -s, 0, 0, 1, 0, 0, s, 0, c, 0, 0, 0, 0, 1]);
  }

  rotateZ(degrees: number): Transform3D {
    const c = Math.cos(degrees * DEG);
    const s = Math.sin(degrees * DEG);
    return this.multiply([c, s, 0, 0, -s, c, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
  }
}

/** Start a `Transform3D` (identity). / 単位変換から `Transform3D` を始めます。 */
export function transform3d(matrix?: Mat4): Transform3D {
  return new Transform3D(matrix);
}

/** OpenGL-style perspective projection. `fovY` in degrees. */
export function perspective(fovY: number, aspect: number, near: number, far: number): number[] {
  const f = 1 / Math.tan((fovY * DEG) / 2);
  const depth = 1 / (near - far);
  return [f / aspect, 0, 0, 0, 0, f, 0, 0, 0, 0, (far + near) * depth, -1, 0, 0, 2 * far * near * depth, 0];
}

type Vec3 = readonly [number, number, number];

/** View matrix of a camera at `eye` looking at `target`. */
export function lookAt(eye: Vec3, target: Vec3, up: Vec3 = [0, 1, 0]): number[] {
  const sub = (a: Vec3, b: Vec3): Vec3 => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
  const normalize = (v: Vec3): Vec3 => {
    const length = Math.hypot(v[0], v[1], v[2]) || 1;
    return [v[0] / length, v[1] / length, v[2] / length];
  };
  const cross = (a: Vec3, b: Vec3): Vec3 => [
    a[1] * b[2] - a[2] * b[1],
    a[2] * b[0] - a[0] * b[2],
    a[0] * b[1] - a[1] * b[0],
  ];
  const dot = (a: Vec3, b: Vec3) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
  const z = normalize(sub(eye, target));
  const x = normalize(cross(up, z));
  const y = cross(z, x);
  return [x[0], y[0], z[0], 0, x[1], y[1], z[1], 0, x[2], y[2], z[2], 0, -dot(x, eye), -dot(y, eye), -dot(z, eye), 1];
}

/** Logical size of an output (its resolution over its scale). / 出力の論理サイズ。 */
export function outputLogicalSize(output: OutputInfo): { width: number; height: number } {
  const scale = output.scale || 1;
  return {
    width: Math.round((output.resolution?.width ?? 0) / scale),
    height: Math.round((output.resolution?.height ?? 0) / scale),
  };
}

export interface ScreenCameraOptions {
  /** Vertical field of view in degrees (default 45). / 縦の視野角（度、既定 45）。 */
  fov?: number;
  /** Move the camera back by this many units (zoom out). / カメラを引く量。 */
  distance?: number;
}

/**
 * A camera that maps the world's XY plane at z = 0 onto the screen 1:1 in
 * logical pixels: a `<Plane>` the size of the output at the origin covers it
 * exactly. Use it to start 3D effects from the flat desktop.
 * z = 0 の XY 平面を論理ピクセル 1:1 で画面に写すカメラ。原点に出力サイズの
 * `<Plane>` を置くとちょうど画面を覆います。平らなデスクトップから始める 3D 演出に。
 */
export function screenCamera(
  size: OutputInfo | { width: number; height: number },
  options: ScreenCameraOptions = {},
): Camera {
  const { width, height } = "resolution" in size || "scale" in size
    ? outputLogicalSize(size as OutputInfo)
    : (size as { width: number; height: number });
  const fov = options.fov ?? 45;
  const fit = height / 2 / Math.tan((fov * DEG) / 2);
  const distance = fit + (options.distance ?? 0);
  const depth = Math.max(width, height) * 4 + distance;
  return {
    projection: perspective(fov, width / Math.max(height, 1), Math.max(distance / 100, 0.1), depth),
    view: lookAt([0, 0, distance], [0, 0, 0]),
  };
}

/**
 * Where a world point lands on a scene's viewport, in logical pixels from its
 * top-left corner, with its depth (NDC z, smaller is nearer). `null` behind
 * the camera.
 * ワールドの点がビューポートのどこに写るか（左上からの論理ピクセル）と深度。
 * カメラの後ろなら `null`。
 */
export function projectPoint(
  camera: Camera,
  viewport: { width: number; height: number },
  point: Vec3,
): { x: number; y: number; depth: number } | null {
  const clip = transformPoint(multiplyMat4(camera.projection, camera.view), point);
  if (clip[3] <= 1e-6) {
    return null;
  }
  const ndcX = clip[0] / clip[3];
  const ndcY = clip[1] / clip[3];
  return {
    x: ((ndcX + 1) / 2) * viewport.width,
    y: ((1 - ndcY) / 2) * viewport.height,
    depth: clip[2] / clip[3],
  };
}

function transformPoint(matrix: Mat4, point: Vec3): [number, number, number, number] {
  const [x, y, z] = point;
  return [
    matrix[0] * x + matrix[4] * y + matrix[8] * z + matrix[12],
    matrix[1] * x + matrix[5] * y + matrix[9] * z + matrix[13],
    matrix[2] * x + matrix[6] * y + matrix[10] * z + matrix[14],
    matrix[3] * x + matrix[7] * y + matrix[11] * z + matrix[15],
  ];
}

/** A plane as `pickPlane` sees it: the same numbers as its `<Plane>` props. */
export interface PickablePlane {
  width: number;
  height: number;
  transform?: Mat4 | Transform3D;
}

/**
 * Hit-test planes the way a `<Scene3D>` with `camera` draws them: the index
 * of the nearest plane under `(x, y)` (logical pixels from the viewport's
 * top-left corner), or `null`. Use it to make a 3D layout clickable under an
 * input grab.
 * `camera` で描いた `<Scene3D>` の平面を当たり判定します。`(x, y)`（ビューポート
 * 左上からの論理ピクセル）の下で一番手前の平面の添字、無ければ `null`。
 */
export function pickPlane(
  camera: Camera,
  viewport: { width: number; height: number },
  planes: readonly PickablePlane[],
  x: number,
  y: number,
): number | null {
  let best: { index: number; depth: number } | null = null;
  planes.forEach((plane, index) => {
    const matrix =
      plane.transform instanceof Transform3D
        ? plane.transform.matrix
        : (plane.transform ?? IDENTITY);
    const halfWidth = plane.width / 2;
    const halfHeight = plane.height / 2;
    const corners: Vec3[] = [
      [-halfWidth, halfHeight, 0],
      [halfWidth, halfHeight, 0],
      [halfWidth, -halfHeight, 0],
      [-halfWidth, -halfHeight, 0],
    ];
    const projected = corners.map((corner) => {
      const world = transformPoint(matrix, corner);
      return projectPoint(camera, viewport, [world[0], world[1], world[2]]);
    });
    if (projected.some((point) => point === null)) {
      return;
    }
    const quad = projected as { x: number; y: number; depth: number }[];
    if (!pointInConvexQuad(quad, x, y)) {
      return;
    }
    const depth = quad.reduce((sum, point) => sum + point.depth, 0) / 4;
    if (!best || depth < best.depth) {
      best = { index, depth };
    }
  });
  return (best as { index: number; depth: number } | null)?.index ?? null;
}

function pointInConvexQuad(quad: readonly { x: number; y: number }[], x: number, y: number): boolean {
  let sign = 0;
  for (let i = 0; i < quad.length; i++) {
    const a = quad[i];
    const b = quad[(i + 1) % quad.length];
    const cross = (b.x - a.x) * (y - a.y) - (b.y - a.y) * (x - a.x);
    if (cross === 0) {
      continue;
    }
    const side = Math.sign(cross);
    if (sign === 0) {
      sign = side;
    } else if (side !== sign) {
      return false;
    }
  }
  return true;
}

// ---------------------------------------------------------------------------
// Wire format (see `backend/composition.rs`)

interface WireRect {
  x: number;
  y: number;
  width?: number;
  height?: number;
}

export type WireCompositionNode =
  | { kind: "layers"; layers: LayerName[] }
  | { kind: "layer-popups" }
  | { kind: "windows"; windows?: string[]; offsetX?: number; offsetY?: number }
  | { kind: "texture-view"; texture: number; rect?: WireRect; opacity: number }
  | { kind: "solid"; rect?: WireRect; color: number[] }
  | {
      kind: "scene3d";
      rect?: WireRect;
      projection: number[];
      view: number[];
      clearColor: number[];
      antialias: boolean;
      objects: WirePlane[];
    };

interface WirePlane {
  kind: "plane";
  texture: number;
  width: number;
  height: number;
  model: number[];
  opacity: number;
  doubleSided: boolean;
}

interface WireTexture {
  key: string;
  width?: number;
  height?: number;
  scale?: number;
  clearColor: number[];
  nodes: WireCompositionNode[];
}

export interface WireOutputComposition {
  nodes: WireCompositionNode[];
  textures: WireTexture[];
}

const LAYER_NAMES = new Set<LayerName>(["background", "bottom", "top", "overlay"]);

/** Premultiplied RGBA in 0..1. */
export function parseColor(value: CompositionColor): number[] {
  let rgba: number[];
  if (typeof value === "string") {
    const hex = value.trim().replace(/^#/, "");
    const expand = hex.length === 3 || hex.length === 4
      ? hex.split("").map((digit) => digit + digit).join("")
      : hex;
    if (!/^[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$/.test(expand)) {
      throw new TypeError(`Invalid color ${JSON.stringify(value)}`);
    }
    rgba = [0, 2, 4, 6].map((index) =>
      index < expand.length ? parseInt(expand.slice(index, index + 2), 16) / 255 : 1
    );
  } else {
    rgba = [...value];
  }
  const clamp = (channel: number) => Math.min(1, Math.max(0, Number.isFinite(channel) ? channel : 0));
  const [r, g, b, a] = rgba.map(clamp);
  return [r * a, g * a, b * a, a];
}

function finite(value: unknown, name: string): number {
  const number = read(value as MaybeSignal<number>);
  if (typeof number !== "number" || !Number.isFinite(number)) {
    throw new TypeError(`${name} must be a finite number`);
  }
  return number;
}

function unit(value: unknown, name: string, fallback: number): number {
  if (value === undefined) return fallback;
  return Math.min(1, Math.max(0, finite(value, name)));
}

function matrix(value: unknown, name: string): number[] {
  const resolved = read(value as MaybeSignal<Mat4 | Transform3D>);
  const values = resolved instanceof Transform3D ? resolved.matrix : resolved;
  if (!Array.isArray(values) || values.length !== 16 || !values.every(Number.isFinite)) {
    throw new TypeError(`${name} must be a 4x4 matrix of 16 finite numbers`);
  }
  return [...values];
}

function rect(props: Record<string, unknown>): WireRect | undefined {
  const keys = ["x", "y", "width", "height"] as const;
  if (keys.every((key) => props[key] === undefined)) return undefined;
  const value = (key: (typeof keys)[number]) =>
    props[key] === undefined ? undefined : finite(props[key], key);
  // Missing extents reach to the composition's far edge.
  return {
    x: value("x") ?? 0,
    y: value("y") ?? 0,
    ...(props.width === undefined ? {} : { width: value("width") }),
    ...(props.height === undefined ? {} : { height: value("height") }),
  };
}

function windowId(window: WaylandWindow | string): string {
  return typeof window === "string" ? window : window.id;
}

/** Turn a composition tree into the plan the compositor walks. */
export function serializeOutputComposition(
  root: CompositionRenderable | CompositionRenderable[],
): WireOutputComposition {
  const textures: WireTexture[] = [];
  const indices = new Map<RenderTexture, number>();
  const building = new Set<RenderTexture>();

  const textureIndex = (texture: unknown): number => {
    if (!isRenderTexture(texture)) {
      throw new TypeError("texture must come from renderTexture()");
    }
    const existing = indices.get(texture);
    if (existing !== undefined) return existing;
    if (building.has(texture)) {
      throw new Error("A render texture cannot show itself");
    }
    building.add(texture);
    const options = texture.options;
    const nodes = serializeChildren(options.content, false);
    building.delete(texture);
    const index = textures.length;
    textures.push({
      key: options.key ?? `#${index}`,
      ...(options.width === undefined ? {} : { width: finite(options.width, "width") }),
      ...(options.height === undefined ? {} : { height: finite(options.height, "height") }),
      ...(options.scale === undefined ? {} : { scale: finite(options.scale, "scale") }),
      clearColor: options.clearColor === undefined ? [0, 0, 0, 0] : parseColor(options.clearColor),
      nodes,
    });
    indices.set(texture, index);
    return index;
  };

  const serializeChildren = (children: unknown, inScene: boolean): WireCompositionNode[] => {
    const out: WireCompositionNode[] = [];
    const visit = (child: unknown) => {
      if (child == null || child === false || child === true) return;
      if (Array.isArray(child)) {
        child.forEach(visit);
        return;
      }
      if (isSignal(child)) {
        visit(read(child));
        return;
      }
      if (typeof child !== "object" || (child as CompositionChild & { kind?: string }).kind !== "element") {
        return;
      }
      const node = child as CompositionElementNode;
      const props = node.props as Record<string, unknown>;
      const type = node.type as string;
      if (type === "Fragment") {
        node.children.forEach(visit);
        return;
      }
      if (inScene !== (type === "Plane")) {
        throw new Error(
          inScene ? `<${type}> cannot be placed in a <Scene3D>` : "<Plane> must be placed in a <Scene3D>",
        );
      }
      switch (type) {
        case "Layers": {
          const value = read(props.layers as MaybeSignal<LayerName | readonly LayerName[]>);
          const layers = (Array.isArray(value) ? value : [value]) as LayerName[];
          for (const layer of layers) {
            if (!LAYER_NAMES.has(layer)) throw new TypeError(`Unknown layer ${JSON.stringify(layer)}`);
          }
          out.push({ kind: "layers", layers: [...layers] });
          break;
        }
        case "LayerPopups":
          out.push({ kind: "layer-popups" });
          break;
        case "Windows": {
          const windows = read(props.windows as MaybeSignal<readonly (WaylandWindow | string)[]> | undefined);
          const offsetX = props.offsetX === undefined ? 0 : Math.round(finite(props.offsetX, "offsetX"));
          const offsetY = props.offsetY === undefined ? 0 : Math.round(finite(props.offsetY, "offsetY"));
          out.push({
            kind: "windows",
            ...(windows === undefined ? {} : { windows: windows.map(windowId) }),
            ...(offsetX ? { offsetX } : {}),
            ...(offsetY ? { offsetY } : {}),
          });
          break;
        }
        case "TextureView":
          out.push({
            kind: "texture-view",
            texture: textureIndex(props.texture),
            ...(rect(props) ? { rect: rect(props) } : {}),
            opacity: unit(props.opacity, "opacity", 1),
          });
          break;
        case "Solid":
          out.push({
            kind: "solid",
            ...(rect(props) ? { rect: rect(props) } : {}),
            color: parseColor(read(props.color as MaybeSignal<CompositionColor>)),
          });
          break;
        case "Scene3D": {
          const camera = read(props.camera as MaybeSignal<Camera>);
          if (!camera) throw new TypeError("<Scene3D> needs a camera");
          const objects = serializeChildren(node.children, true) as unknown as WirePlane[];
          const clear = read(props.clearColor as MaybeSignal<CompositionColor> | undefined);
          out.push({
            kind: "scene3d",
            ...(rect(props) ? { rect: rect(props) } : {}),
            projection: matrix(camera.projection, "camera.projection"),
            view: matrix(camera.view, "camera.view"),
            clearColor: clear === undefined ? [0, 0, 0, 0] : parseColor(clear),
            antialias: read(props.antialias as MaybeSignal<boolean> | undefined) ?? true,
            objects,
          });
          break;
        }
        case "Plane":
          (out as unknown as WirePlane[]).push({
            kind: "plane",
            texture: textureIndex(props.texture),
            width: finite(props.width, "width"),
            height: finite(props.height, "height"),
            model: props.transform === undefined ? [...IDENTITY] : matrix(props.transform, "transform"),
            opacity: unit(props.opacity, "opacity", 1),
            doubleSided: read(props.doubleSided as MaybeSignal<boolean> | undefined) ?? true,
          });
          break;
        default:
          throw new Error(`<${type}> cannot be used in an output composition`);
      }
    };
    visit(children);
    return out;
  };

  const nodes = serializeChildren(root, false);
  return { nodes, textures };
}
