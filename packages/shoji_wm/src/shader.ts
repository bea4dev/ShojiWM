import type {
  BackdropBlurOptions,
  BackdropSourceHandle,
  XrayBackdropSourceHandle,
  BlendMode,
  BlendStageHandle,
  CompiledEffectHandle,
  DualKawaseBlurStageHandle,
  EffectInputHandle,
  NoiseKind,
  NoiseStageHandle,
  SaveStageHandle,
  ShaderModuleHandle,
  ShaderStageHandle,
  ShaderInputHandle,
  UnitStageHandle,
  ImageSourceHandle,
  NamedTextureHandle,
  ShaderUniformArrayElement,
  ShaderUniformArrayHandle,
  ShaderUniformArrayValues,
  ShaderUniformMap,
  EffectAlphaMode,
  EffectInvalidationPolicyHandle,
  EffectOutsets,
  LayerEffectHandle,
  LayerEffectInputHandle,
  LayerSourceHandle,
  PopupEffectHandle,
  PopupEffectInputHandle,
  PopupSourceHandle,
  WindowEffectHandle,
  WindowSourceHandle,
  MaybeSignal,
} from "./types";

let assetBaseDir = "/";

export interface CompileEffectOptions {
  input: EffectInputHandle;
  /**
   * Logical padding around the visible content used as the pipeline working
   * area. The final pipeline result is cropped back to the content rect.
   */
  capturePadding?: MaybeSignal<number>;
  invalidate?: EffectInvalidationPolicyHandle;
  pipeline: Array<
    | ShaderStageHandle
    | NoiseStageHandle
    | DualKawaseBlurStageHandle
    | SaveStageHandle
    | BlendStageHandle
    | UnitStageHandle
  >;
  /**
   * Output alpha handling. Defaults to `"opaque"`, which forces the result
   * to full opacity to hide capture/blur alpha noise at the edges — the
   * right choice for plain backdrop blurs. Declare `"preserve"` when the
   * pipeline intentionally produces transparency (e.g. masking the blur
   * against a layer's own alpha); the pipeline is then responsible for the
   * alpha of every pixel, including the blur edge regions.
   * See {@link EffectAlphaMode}.
   */
  alpha?: EffectAlphaMode;
}

export interface CompileWindowEffectOptions extends CompileEffectOptions {
  input: WindowSourceHandle;
  outsets?: EffectOutsets;
}

export interface CompileLayerEffectOptions extends CompileEffectOptions {
  input: LayerEffectInputHandle;
  outsets?: EffectOutsets;
}

// Base directory for relative asset paths (shaders, images, fonts). Callers
// pass the already-resolved config package root - typically the directory
// containing the nearest ancestor package.json of the entry config file.
/** @internal */
export function installAssetResolverBridge(configRoot: string): void {
  assetBaseDir = normalizePath(
    isAbsolutePath(configRoot) ? configRoot : resolvePath("/", configRoot),
  );
}

export function installShaderResolverBridge(configPath: string): void {
  assetBaseDir = dirnamePath(resolvePath(assetBaseDir, configPath));
}

export function resolveAssetPath(path: string): string {
  return isAbsolutePath(path) ? path : resolvePath(assetBaseDir, path);
}

/**
 * Load a GLSL shader from a file path (relative to the config package root).
 * Returns a handle that can be passed to `shaderStage` or `shaderInput`.
 * 設定パッケージルートからの相対パスで GLSL シェーダーをロードします。
 * `shaderStage` または `shaderInput` に渡せるハンドルを返します。
 *
 * @example
 * ```ts
 * const myShader = loadShader("shaders/frosted.glsl");
 * ```
 */
export function loadShader(path: string): ShaderModuleHandle {
  return {
    kind: "shader-module",
    path: resolveAssetPath(path),
  };
}

function createUniformArray<Element extends ShaderUniformArrayElement>(
  element: Element,
  values: ShaderUniformArrayHandle<Element>["values"],
): ShaderUniformArrayHandle<Element> {
  return { kind: "uniform-array", element, values };
}

/**
 * Create a GLSL scalar/vector uniform array. Its length is structural, while
 * the array, each element, and each vector component may be signals.
 * GLSL のスカラー・ベクトル uniform 配列を作成します。配列長は構造として扱われ、
 * 配列全体・各要素・各ベクトル成分には Signal を指定できます。
 *
 * @example
 * ```ts
 * const phase = signal(0);
 * shaderStage(loadShader("./waves.frag"), {
 *   uniforms: {
 *     weights: uniformArray.float([1, phase, 0.25]),
 *     points: uniformArray.vec2([[0, 0], [phase, 1]]),
 *   },
 * });
 * ```
 */
export const uniformArray = {
  float(
    values: ShaderUniformArrayHandle<"float">["values"],
  ): ShaderUniformArrayHandle<"float"> {
    return createUniformArray("float", values);
  },
  vec2(
    values: ShaderUniformArrayHandle<"vec2">["values"],
  ): ShaderUniformArrayHandle<"vec2"> {
    return createUniformArray("vec2", values);
  },
  vec3(
    values: ShaderUniformArrayHandle<"vec3">["values"],
  ): ShaderUniformArrayHandle<"vec3"> {
    return createUniformArray("vec3", values);
  },
  vec4(
    values: ShaderUniformArrayHandle<"vec4">["values"],
  ): ShaderUniformArrayHandle<"vec4"> {
    return createUniformArray("vec4", values);
  },
} satisfies {
  [Element in ShaderUniformArrayElement]: (
    values: MaybeSignal<ShaderUniformArrayValues<Element>>,
  ) => ShaderUniformArrayHandle<Element>;
};

/**
 * Capture the composited scene **beneath** the current surface as an effect
 * input. This is what you use to implement blur or tint that reads the
 * wallpaper + windows behind the current window/layer.
 * 現在のサーフェスの**下**の合成済みシーンをエフェクト入力としてキャプチャします。
 * 現在のウィンドウ・レイヤーの背後にある壁紙やウィンドウを読み取るブラーや
 * 色付けを実装するときに使います。
 *
 * @example
 * ```ts
 * compileEffect({
 *   input: backdropSource(),
 *   pipeline: [dualKawaseBlur({ passes: 3 })],
 * });
 * ```
 */
export function backdropSource(): BackdropSourceHandle {
  return { kind: "backdrop-source" };
}

/**
 * Like `backdropSource`, but samples the scene as if the current surface were
 * not present (X-ray through itself). Useful for overlay-style effects that
 * need the unobstructed background.
 * `backdropSource` と同様ですが、現在のサーフェスが存在しないかのようにシーンを
 * サンプリングします（自分自身を透過）。自身に遮られていない背景が必要な
 * オーバーレイスタイルのエフェクトに便利です。
 */
export function xrayBackdropSource(): XrayBackdropSourceHandle {
  return { kind: "xray-backdrop-source" };
}

/**
 * Capture the **window's own rendered content** as an effect input.
 * Use `include: "root-surface"` to exclude sub-surfaces (popups, etc.).
 * **ウィンドウ自身のレンダリング済みコンテンツ**をエフェクト入力としてキャプチャします。
 * `include: "root-surface"` でサブサーフェス（ポップアップ等）を除外できます。
 *
 * @example
 * ```ts
 * compileWindowEffect({
 *   input: windowSource(),
 *   pipeline: [shaderStage("shaders/outline.glsl")],
 * });
 * ```
 */
export function windowSource(
  options: { include?: "full" | "root-surface" } = {},
): WindowSourceHandle {
  return {
    kind: "window-source",
    include: options.include ?? "full",
  };
}

/**
 * Capture a **layer-shell surface's own rendered content** as an effect input.
 * **レイヤーシェルサーフェス自身のレンダリング済みコンテンツ**をエフェクト入力として
 * キャプチャします。
 */
export function layerSource(
  options: { include?: "full" | "root-surface" } = {},
): LayerSourceHandle {
  return {
    kind: "layer-source",
    include: options.include ?? "full",
  };
}

/**
 * Capture a **popup's own rendered content** as an effect input. Covers both
 * window-attached and layer-attached popups.
 * **ポップアップ自身のレンダリング済みコンテンツ**をエフェクト入力としてキャプチャします。
 * ウィンドウ・レイヤー両方に付いたポップアップが対象です。
 */
export function popupSource(
  options: { include?: "full" | "root-surface" } = {},
): PopupSourceHandle {
  return {
    kind: "popup-source",
    include: options.include ?? "full",
  };
}

/**
 * Load an image from a file path (relative to the config package root) as an
 * effect input texture. Useful for custom overlays or masks.
 * 設定パッケージルートからの相対パスで画像ファイルをエフェクト入力テクスチャとして
 * ロードします。カスタムオーバーレイやマスクに便利です。
 *
 * @example
 * ```ts
 * const mask = imageSource("assets/mask.png");
 * ```
 */
export function imageSource(path: string): ImageSourceHandle {
  return {
    kind: "image-source",
    path: resolveAssetPath(path),
  };
}

/**
 * Reference a named texture previously stored by a `save()` stage in the
 * same pipeline. Use this to reuse an intermediate result in a later stage.
 * 同じパイプライン内の `save()` ステージが保存した名前付きテクスチャを参照します。
 * 中間結果を後のステージで再利用するために使います。
 *
 * @example
 * ```ts
 * pipeline: [
 *   dualKawaseBlur({ passes: 2 }),
 *   save("blurred"),
 *   shaderStage("shaders/tint.glsl", { textures: { blurred: get("blurred") } }),
 * ]
 * ```
 */
export function get(name: string): NamedTextureHandle {
  return {
    kind: "named-texture",
    name,
  };
}

/**
 * Create a GLSL shader **pipeline stage** that reads the previous stage's output
 * (or the effect input) and writes to the next stage.
 * Accepts a path string or a pre-loaded `ShaderModuleHandle`.
 * 前のステージ（またはエフェクト入力）を読み取り、次のステージへ書き込む
 * GLSL シェーダー**パイプラインステージ**を作成します。
 * パス文字列または事前ロード済みの `ShaderModuleHandle` を渡せます。
 *
 * @example
 * ```ts
 * shaderStage("shaders/vignette.glsl", {
 *   uniforms: { strength: 0.4 },
 * })
 * ```
 */
export function shaderStage(
  shader: string | ShaderModuleHandle,
  options: {
    uniforms?: ShaderUniformMap;
    textures?: Record<string, EffectInputHandle>;
  } = {},
): ShaderStageHandle {
  return {
    kind: "shader-stage",
    shader: typeof shader === "string" ? loadShader(shader) : shader,
    uniforms: options.uniforms,
    textures: options.textures,
  };
}

/**
 * Like `shaderStage`, but used as the **input** slot of `compileEffect` rather
 * than in the pipeline array. The shader pre-processes the source texture before
 * the pipeline stages run.
 * `shaderStage` と同様ですが、パイプライン配列ではなく `compileEffect` の
 * **input** スロットに使います。パイプラインステージが実行される前に
 * ソーステクスチャを前処理します。
 */
export function shaderInput(
  shader: string | ShaderModuleHandle,
  options: {
    uniforms?: ShaderUniformMap;
    textures?: Record<string, EffectInputHandle>;
  } = {},
): ShaderInputHandle {
  return {
    kind: "shader-input",
    shader: typeof shader === "string" ? loadShader(shader) : shader,
    uniforms: options.uniforms,
    textures: options.textures,
  };
}

/**
 * Add a GPU noise overlay to the pipeline. Useful for adding film grain or
 * dithering to reduce banding on gradients/blurs.
 * パイプラインに GPU ノイズオーバーレイを追加します。フィルムグレインの追加や
 * グラデーション・ブラーのバンディング軽減のためのディザリングに便利です。
 *
 * @example
 * ```ts
 * pipeline: [dualKawaseBlur({ passes: 3 }), noise({ amount: 0.04 })]
 * ```
 */
export function noise(
  options: { kind?: NoiseKind; amount?: number } = {},
): NoiseStageHandle {
  return {
    kind: "noise",
    noiseKind: options.kind ?? "salt",
    amount: options.amount,
  };
}

/**
 * GPU dual-Kawase blur stage. Runs a downscale/upscale blur pyramid; increasing
 * `passes` spreads the blur radius, while `radius` increases each pass's
 * sampling offset. A good starting point is `{ passes: 3, radius: 4 }`.
 * GPU デュアル川瀬ブラーステージ。ダウンスケール・アップスケールのブラーピラミッドを
 * 実行します。`passes` を増やすとブラー範囲が広がり、`radius` を増やすと各パスの
 * サンプリング間隔が広がります。出発点として `{ passes: 3, radius: 4 }` が適切です。
 *
 * @example
 * ```ts
 * pipeline: [dualKawaseBlur({ passes: 4, radius: 3 })]
 * ```
 */
export function dualKawaseBlur(
  options: BackdropBlurOptions = {},
): DualKawaseBlurStageHandle {
  return {
    kind: "dual-kawase-blur",
    radius: options.radius,
    passes: options.passes,
  };
}

/**
 * Save the current pipeline output to a named slot for later retrieval with
 * `get(name)`. The pipeline continues from the saved value.
 * 現在のパイプライン出力を名前付きスロットに保存し、後で `get(name)` で取得できます。
 * パイプラインは保存した値から続きます。
 *
 * @example
 * ```ts
 * pipeline: [dualKawaseBlur({ passes: 2 }), save("blurred")]
 * ```
 */
export function save(name: string): SaveStageHandle {
  return {
    kind: "save",
    name,
  };
}

/**
 * Blend another `EffectInputHandle` over the current pipeline output using the
 * given blend mode and optional alpha.
 * 指定したブレンドモードとオプションのアルファを使って、別の `EffectInputHandle` を
 * 現在のパイプライン出力にブレンドします。
 *
 * @example Tint a blurred backdrop with semi-transparent color
 * ```ts
 * pipeline: [
 *   dualKawaseBlur({ passes: 3 }),
 *   blend(imageSource("assets/overlay.png"), { mode: "screen", alpha: 0.5 }),
 * ]
 * ```
 */
export function blend(
  input: EffectInputHandle,
  options: { mode?: BlendMode; alpha?: number } = {},
): BlendStageHandle {
  return {
    kind: "blend",
    input,
    mode: options.mode,
    alpha: options.alpha,
  };
}

/**
 * Wrap a compiled `CompiledEffectHandle` as a pipeline stage so it can be
 * embedded inside another effect's pipeline as a reusable sub-effect.
 * コンパイル済みの `CompiledEffectHandle` をパイプラインステージとしてラップし、
 * 別のエフェクトのパイプライン内に再利用可能なサブエフェクトとして組み込みます。
 */
export function unit(effect: CompiledEffectHandle): UnitStageHandle {
  return {
    kind: "unit",
    effect,
  };
}

function isAbsolutePath(path: string): boolean {
  return path.startsWith("/");
}

function dirnamePath(path: string): string {
  const normalized = normalizePath(path);
  if (normalized === "/") {
    return "/";
  }
  const index = normalized.lastIndexOf("/");
  return index <= 0 ? "/" : normalized.slice(0, index);
}

function resolvePath(...paths: string[]): string {
  return normalizePath(paths.filter(Boolean).join("/"));
}

function normalizePath(path: string): string {
  const absolute = path.startsWith("/");
  const parts = path
    .split("/")
    .filter((part) => part.length > 0 && part !== ".");
  const stack: string[] = [];

  for (const part of parts) {
    if (part === "..") {
      if (stack.length > 0) {
        stack.pop();
      }
      continue;
    }
    stack.push(part);
  }

  const joined = stack.join("/");
  if (absolute) {
    return joined ? `/${joined}` : "/";
  }
  return joined || ".";
}

/**
 * Compile a background effect from a source input and a pipeline of stages.
 * The result is assigned to `COMPOSITOR.effect.background_effect` or passed
 * to `unit()` to compose it inside another effect.
 * ソース入力とステージのパイプラインから背景エフェクトをコンパイルします。
 * 結果は `COMPOSITOR.effect.background_effect` に割り当てるか、`unit()` で
 * 別のエフェクト内に組み込みます。
 *
 * @example Frosted-glass backdrop blur / すりガラス背景ブラー
 * ```ts
 * COMPOSITOR.effect.background_effect = compileEffect({
 *   input: backdropSource(),
 *   capturePadding: 32,
 *   pipeline: [dualKawaseBlur({ passes: 3, radius: 4 }), noise({ amount: 0.03 })],
 * });
 * ```
 */
export function compileEffect(
  options: CompileEffectOptions,
): CompiledEffectHandle {
  return {
    kind: "compiled-effect",
    input: options.input,
    capturePadding: options.capturePadding ?? 0,
    invalidate: options.invalidate ?? {
      kind: "on-source-damage-box",
      damagePadding: 0,
    },
    pipeline: options.pipeline,
    alpha: options.alpha ?? "opaque",
  };
}

/**
 * Compile a per-window effect. Like `compileEffect` but scoped to a single
 * window's surface. Optionally specify `outsets` to render beyond the window
 * bounds (e.g. for a drop-shadow or glow).
 * ウィンドウごとのエフェクトをコンパイルします。`compileEffect` と同様ですが、
 * 1 つのウィンドウのサーフェスにスコープされます。`outsets` でウィンドウ境界の外側に
 * レンダリングできます（ドロップシャドウやグローなど）。
 *
 * @example Per-window drop shadow / ウィンドウごとのドロップシャドウ
 * ```ts
 * // The handle goes in an assignment slot: behind | behindRootSurface | inFront | replace.
 * COMPOSITOR.effect.window = () => ({
 *   behind: compileWindowEffect({
 *     input: windowSource(),
 *     pipeline: [shaderStage("shaders/shadow.glsl")],
 *     outsets: { top: 0, right: 20, bottom: 20, left: 20 },
 *   }),
 * });
 * ```
 */
export function compileWindowEffect(
  options: CompileWindowEffectOptions,
): WindowEffectHandle {
  return {
    kind: "window-effect",
    effect: compileEffect(options),
    outsets: options.outsets,
  };
}

/**
 * Compile a per-layer-shell-surface effect. Returned from
 * `COMPOSITOR.effect.layer` to apply an effect to a specific layer surface.
 * レイヤーシェルサーフェスごとのエフェクトをコンパイルします。
 * `COMPOSITOR.effect.layer` から返すことで特定のレイヤーサーフェスにエフェクトを適用します。
 *
 * @example Bar blur / バーブラー
 * ```ts
 * const barBlur = compileLayerEffect({
 *   input: backdropSource(),
 *   pipeline: [dualKawaseBlur({ passes: 2 })],
 * });
 * COMPOSITOR.effect.layer = (layer) =>
 *   layer.namespace.value === "bar" ? { behind: barBlur } : {};
 * ```
 */
export function compileLayerEffect(
  options: CompileLayerEffectOptions,
): LayerEffectHandle {
  return {
    kind: "layer-effect",
    effect: compileEffect(options),
    outsets: options.outsets,
  };
}

export interface CompilePopupEffectOptions extends CompileEffectOptions {
  input: PopupEffectInputHandle;
  outsets?: EffectOutsets;
}

/**
 * Compile a per-popup effect. Returned from `COMPOSITOR.effect.popup` to
 * apply an effect to a specific popup (tooltip, context menu, etc.).
 * ポップアップごとのエフェクトをコンパイルします。
 * `COMPOSITOR.effect.popup` から返すことで特定のポップアップ（ツールチップ・
 * コンテキストメニュー等）にエフェクトを適用します。
 */
export function compilePopupEffect(
  options: CompilePopupEffectOptions,
): PopupEffectHandle {
  return {
    kind: "popup-effect",
    effect: compileEffect(options),
    outsets: options.outsets,
  };
}
