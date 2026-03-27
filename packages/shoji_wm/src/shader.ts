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
  ShaderUniformMap,
  EffectInvalidationPolicyHandle,
} from "./types";

let shaderBaseDir = "/";

export interface CompileEffectOptions {
  input: EffectInputHandle;
  invalidate?: EffectInvalidationPolicyHandle;
  pipeline: Array<
    | ShaderStageHandle
    | NoiseStageHandle
    | DualKawaseBlurStageHandle
    | SaveStageHandle
    | BlendStageHandle
    | UnitStageHandle
  >;
}

export function installShaderResolverBridge(configPath: string): void {
  shaderBaseDir = dirnamePath(resolvePath(shaderBaseDir, configPath));
}

function resolveShaderPath(path: string): string {
  return isAbsolutePath(path) ? path : resolvePath(shaderBaseDir, path);
}

export function loadShader(path: string): ShaderModuleHandle {
  return {
    kind: "shader-module",
    path: resolveShaderPath(path),
  };
}

export function backdropSource(): BackdropSourceHandle {
  return { kind: "backdrop-source" };
}

export function xrayBackdropSource(): XrayBackdropSourceHandle {
  return { kind: "xray-backdrop-source" };
}

export function imageSource(path: string): ImageSourceHandle {
  return {
    kind: "image-source",
    path: resolveShaderPath(path),
  };
}

export function get(name: string): NamedTextureHandle {
  return {
    kind: "named-texture",
    name,
  };
}

export function shaderStage(
  shader: string | ShaderModuleHandle,
  options: { uniforms?: ShaderUniformMap } = {},
): ShaderStageHandle {
  return {
    kind: "shader-stage",
    shader: typeof shader === "string" ? loadShader(shader) : shader,
    uniforms: options.uniforms,
  };
}

export function shaderInput(
  shader: string | ShaderModuleHandle,
  options: { uniforms?: ShaderUniformMap } = {},
): ShaderInputHandle {
  return {
    kind: "shader-input",
    shader: typeof shader === "string" ? loadShader(shader) : shader,
    uniforms: options.uniforms,
  };
}

export function noise(options: { kind?: NoiseKind; amount?: number } = {}): NoiseStageHandle {
  return {
    kind: "noise",
    noiseKind: options.kind ?? "salt",
    amount: options.amount,
  };
}

export function dualKawaseBlur(options: BackdropBlurOptions = {}): DualKawaseBlurStageHandle {
  return {
    kind: "dual-kawase-blur",
    radius: options.radius,
    passes: options.passes,
  };
}

export function save(name: string): SaveStageHandle {
  return {
    kind: "save",
    name,
  };
}

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
  const parts = path.split("/").filter((part) => part.length > 0 && part !== ".");
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

export function compileEffect(options: CompileEffectOptions): CompiledEffectHandle {
  return {
    kind: "compiled-effect",
    input: options.input,
    invalidate: options.invalidate ?? {
      kind: "on-source-damage-box",
      antiArtifactMargin: 0,
    },
    pipeline: options.pipeline,
  };
}
