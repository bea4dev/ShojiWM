import { dirname, isAbsolute, resolve } from "node:path";

import type { BackdropBlurOptions, CompiledShaderHandle, ShaderType } from "./types";

let shaderBaseDir = process.cwd();

export interface CompileShaderOptions {
  type?: ShaderType;
  blur?: BackdropBlurOptions;
}

export function installShaderResolverBridge(configPath: string): void {
  shaderBaseDir = dirname(resolve(configPath));
}

export function compileShader(
  path: string,
  options: CompileShaderOptions = {},
): CompiledShaderHandle {
  return {
    kind: "compiled-shader",
    shaderType: options.type ?? "pixel",
    path: isAbsolute(path) ? path : resolve(shaderBaseDir, path),
    blur: options.blur,
  };
}
