import { ENV_CONTROLLER } from "./env";
import { bindAppLockIgnoreModifier, setAppLockIgnored } from "./pointer";
import type {
  CursorConfig,
  CursorController,
  CursorPosition,
  CursorPositionTarget,
  RuntimeCursorConfigUpdate,
} from "./types";

type NativeCursorSetPosition = (target: CursorPositionTarget) => void;
type NativeCursorPosition = () => CursorPosition | null | undefined;

function normalizePositionTarget(target: CursorPositionTarget): CursorPositionTarget {
  if ("output" in target) {
    if (typeof target.output !== "string" || target.output.length === 0) {
      throw new Error("cursor setPosition: output must be a non-empty string");
    }
    const x = target.offset?.x ?? 0;
    const y = target.offset?.y ?? 0;
    if (!Number.isFinite(x) || !Number.isFinite(y)) {
      throw new Error("cursor setPosition: offset must be finite");
    }
    return { output: target.output, anchor: target.anchor ?? "center", offset: { x, y } };
  }
  if (!Number.isFinite(target.x) || !Number.isFinite(target.y)) {
    throw new Error("cursor setPosition: x and y must be finite numbers");
  }
  return { x: target.x, y: target.y };
}

const DEFAULT_CURSOR_THEME = "default";
const DEFAULT_CURSOR_SIZE = 24;
const MAX_CURSOR_SIZE = 512;

function initialConfig(): CursorConfig {
  const theme = ENV_CONTROLLER.get("XCURSOR_THEME")?.trim();
  const size = Number(ENV_CONTROLLER.get("XCURSOR_SIZE"));
  return {
    theme: theme || DEFAULT_CURSOR_THEME,
    size:
      Number.isInteger(size) && size >= 1 && size <= MAX_CURSOR_SIZE
        ? size
        : DEFAULT_CURSOR_SIZE,
  };
}

let currentConfig: CursorConfig = initialConfig();
let pendingConfig: RuntimeCursorConfigUpdate | undefined;

function normalizeConfig(config: CursorConfig): CursorConfig {
  const theme = String(config.theme).trim();
  if (!theme || theme.includes("\0")) {
    throw new Error("cursor theme must be a non-empty string without NUL bytes");
  }

  const size = Number(config.size);
  if (!Number.isInteger(size) || size < 1 || size > MAX_CURSOR_SIZE) {
    throw new Error(
      `cursor size must be an integer between 1 and ${MAX_CURSOR_SIZE}`,
    );
  }

  return { theme, size };
}

function cloneConfig(config: CursorConfig): CursorConfig {
  return { theme: config.theme, size: config.size };
}

function publishCursorEnvironment(config: CursorConfig): void {
  ENV_CONTROLLER.apply({
    XCURSOR_THEME: config.theme,
    XCURSOR_SIZE: config.size,
  });
  ENV_CONTROLLER.publish(["XCURSOR_THEME", "XCURSOR_SIZE"]);
}

export const CURSOR_CONTROLLER: CursorController = {
  configure(config) {
    currentConfig = normalizeConfig(config);
    publishCursorEnvironment(currentConfig);
    pendingConfig = { ...currentConfig, reload: false };
  },
  reload() {
    publishCursorEnvironment(currentConfig);
    pendingConfig = { ...currentConfig, reload: true };
  },
  setPosition(position) {
    const native = (
      globalThis as { __SHOJI_CURSOR_SET_POSITION__?: NativeCursorSetPosition }
    ).__SHOJI_CURSOR_SET_POSITION__;
    native?.(normalizePositionTarget(position));
  },
  getPosition() {
    const native = (
      globalThis as { __SHOJI_CURSOR_POSITION__?: NativeCursorPosition }
    ).__SHOJI_CURSOR_POSITION__;
    const position = native?.();
    if (!position) {
      return undefined;
    }
    return {
      x: position.x,
      y: position.y,
      ...(position.outputName ? { outputName: position.outputName } : {}),
    };
  },
  setAppLockIgnored(ignored) {
    setAppLockIgnored(ignored);
  },
  bindAppLockIgnoreModifier(modifier) {
    bindAppLockIgnoreModifier(modifier);
  },
};

export function takePendingCursorConfig():
  | RuntimeCursorConfigUpdate
  | undefined {
  const pending = pendingConfig;
  pendingConfig = undefined;
  return pending ? { ...pending } : undefined;
}

export function currentCursorConfig(): CursorConfig {
  return cloneConfig(currentConfig);
}
