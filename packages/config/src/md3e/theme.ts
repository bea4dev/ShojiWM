/**
 * The Material 3 color scheme the frames are drawn in.
 *
 * shoji-bar-4 makes the scheme (from its seed color, variant and light / dark
 * mode) and sends it here over the IPC socket as `theme.set`, again whenever
 * it changes and whenever it connects, so the frames always match the shell.
 * Until it does, the baseline dark scheme of Material 3 is used. A new scheme
 * is moved to smoothly, every color at once.
 */
import {
  COMPOSITOR,
  computed,
  createPoll,
  read,
  signal,
  type ReadonlySignal,
  type ShaderUniformVec4,
} from "shoji_wm";
import { EFFECTS_DEFAULT, type Spring } from "./motion";

/** The roles the frames use, as `#rrggbb`. */
export interface Scheme {
  dark: boolean;
  primary: string;
  onPrimary: string;
  primaryContainer: string;
  onPrimaryContainer: string;
  secondary: string;
  onSecondary: string;
  secondaryContainer: string;
  onSecondaryContainer: string;
  tertiary: string;
  tertiaryContainer: string;
  onTertiaryContainer: string;
  error: string;
  onError: string;
  errorContainer: string;
  onErrorContainer: string;
  surface: string;
  surfaceDim: string;
  surfaceBright: string;
  surfaceContainerLowest: string;
  surfaceContainerLow: string;
  surfaceContainer: string;
  surfaceContainerHigh: string;
  surfaceContainerHighest: string;
  onSurface: string;
  onSurfaceVariant: string;
  inverseSurface: string;
  inverseOnSurface: string;
  outline: string;
  outlineVariant: string;
  scrim: string;
}

export type Role = Exclude<keyof Scheme, "dark">;

/** Material 3's baseline dark scheme (seed #6750a4). */
export const BASELINE_DARK: Scheme = {
  dark: true,
  primary: "#d0bcff",
  onPrimary: "#381e72",
  primaryContainer: "#4f378b",
  onPrimaryContainer: "#eaddff",
  secondary: "#ccc2dc",
  onSecondary: "#332d41",
  secondaryContainer: "#4a4458",
  onSecondaryContainer: "#e8def8",
  tertiary: "#efb8c8",
  tertiaryContainer: "#633b48",
  onTertiaryContainer: "#ffd8e4",
  error: "#f2b8b5",
  onError: "#601410",
  errorContainer: "#8c1d18",
  onErrorContainer: "#f9dedc",
  surface: "#141218",
  surfaceDim: "#141218",
  surfaceBright: "#3b383e",
  surfaceContainerLowest: "#0f0d13",
  surfaceContainerLow: "#1d1b20",
  surfaceContainer: "#211f26",
  surfaceContainerHigh: "#2b2930",
  surfaceContainerHighest: "#36343b",
  onSurface: "#e6e0e9",
  onSurfaceVariant: "#cac4d0",
  inverseSurface: "#e6e0e9",
  inverseOnSurface: "#322f35",
  outline: "#938f99",
  outlineVariant: "#49454f",
  scrim: "#000000",
};

/** RGBA in 0..1, straight alpha. */
export type Rgba = [number, number, number, number];

function parse(hex: string): Rgba {
  const value = hex.replace("#", "");
  const channel = (i: number) => Number.parseInt(value.slice(i, i + 2), 16) / 255;
  return [channel(0), channel(2), channel(4), value.length >= 8 ? channel(6) : 1];
}

function format([r, g, b, a]: Rgba): string {
  const byte = (v: number) =>
    Math.round(Math.min(1, Math.max(0, v)) * 255)
      .toString(16)
      .padStart(2, "0");
  return `#${byte(r)}${byte(g)}${byte(b)}${byte(a)}`;
}

function mix(a: Rgba, b: Rgba, t: number): Rgba {
  return [0, 1, 2, 3].map((i) => a[i] + (b[i] - a[i]) * t) as Rgba;
}

type Parsed = Record<Role, Rgba>;

function parsed(scheme: Scheme): Parsed {
  const out = {} as Parsed;
  for (const role of Object.keys(BASELINE_DARK) as (keyof Scheme)[]) {
    if (role !== "dark") {
      out[role] = parse(scheme[role] ?? BASELINE_DARK[role]);
    }
  }
  return out;
}

const [scheme, setScheme] = signal<Scheme>(BASELINE_DARK);
/** Where the colors come from and go to, and how far along they are. */
const [from, setFrom] = signal<Parsed>(parsed(BASELINE_DARK));
const [to, setTo] = signal<Parsed>(parsed(BASELINE_DARK));
const [progress, setProgress] = signal(1);
let moving: { cancel(): void } | null = null;

/** The scheme in use (the one being moved to). */
export const currentScheme: ReadonlySignal<Scheme> = scheme;

/** Whether the scheme is dark. */
export const isDark: ReadonlySignal<boolean> = computed(() => scheme().dark);

function current(role: Role): Rgba {
  const t = progress();
  return t >= 1 ? to()[role] : mix(from()[role], to()[role], t);
}

/** Moves to `next`, from wherever the colors are now. */
export function setTheme(next: Scheme, spring: Spring = EFFECTS_DEFAULT): void {
  if (JSON.stringify(next) === JSON.stringify(scheme.peek())) {
    return;
  }
  const now = {} as Parsed;
  const t = progress.peek();
  for (const role of Object.keys(to.peek()) as Role[]) {
    now[role] = t >= 1 ? to.peek()[role] : mix(from.peek()[role], to.peek()[role], t);
  }
  moving?.cancel();
  setScheme(next);
  setFrom(now);
  setTo(parsed(next));
  const output = COMPOSITOR.output.list[0];
  if (!output) {
    setProgress(1);
    return;
  }
  setProgress(0);
  let start: number | null = null;
  moving = createPoll(
    1,
    (handle) => {
      start ??= handle.nowMs;
      const p = (handle.nowMs - start) / spring.duration;
      setProgress(p >= 1 ? 1 : spring.easing(p));
      if (p >= 1) {
        handle.cancel();
        moving = null;
      }
    },
    { output: () => COMPOSITOR.output.list[0], dirty: "none" },
  );
}

/** Takes a scheme sent by the shell, keeping what it leaves out. */
export function schemeFrom(params: unknown): Scheme | null {
  if (typeof params !== "object" || params === null) {
    return null;
  }
  const given = params as Record<string, unknown>;
  const next: Scheme = { ...BASELINE_DARK, dark: given.dark !== false };
  for (const role of Object.keys(BASELINE_DARK) as Role[]) {
    const value = given[role];
    if (typeof value === "string" && /^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$/.test(value)) {
      next[role] = value;
    }
  }
  return next;
}

/** A role as `#rrggbbaa`, at `alpha` (which may change). */
export function color(role: Role, alpha: number | ReadonlySignal<number> = 1): ReadonlySignal<string> {
  return computed(() => {
    const [r, g, b, a] = current(role);
    return format([r, g, b, a * read(alpha)]);
  });
}

/**
 * A role as `#rrggbbaa` now, `t` of the way from `a` to `b`, at `alpha`. It
 * reads the colors where it is called, making no signal: for a tree that is
 * built again whenever what it reads changes (a drag view's content), where a
 * signal made on every build would pile up.
 */
export function colorNow(a: Role, alpha = 1, b: Role = a, t = 0): string {
  const [r, g, bl, al] = t === 0 ? current(a) : mix(current(a), current(b), t);
  return format([r, g, bl, al * alpha]);
}

/** A role as RGBA for a shader now, straight alpha, making no signal (see `colorNow`). */
export function rgbaNow(role: Role, alpha = 1): Rgba {
  const [r, g, b, a] = current(role);
  return [r, g, b, a * alpha];
}

/** A role as RGBA for a shader, straight alpha. */
export function rgba(role: Role, alpha: number | ReadonlySignal<number> = 1): ReadonlySignal<Rgba> {
  return computed(() => {
    const [r, g, b, a] = current(role);
    return [r, g, b, a * read(alpha)];
  });
}

/** Between two roles, `t` of the way from `a` to `b`, as RGBA for a shader. */
export function rgbaBetween(a: Role, b: Role, t: ReadonlySignal<number>): ReadonlySignal<Rgba> {
  return computed(() => mix(current(a), current(b), t()));
}

/** Between two roles as `#rrggbbaa`, at `alpha`. */
export function colorBetween(
  a: Role,
  b: Role,
  t: ReadonlySignal<number>,
  alpha = 1,
): ReadonlySignal<string> {
  return computed(() => {
    const [r, g, bl, al] = mix(current(a), current(b), t());
    return format([r, g, bl, al * alpha]);
  });
}

/** A color for a `vec4` shader uniform. */
export function vec4(value: ReadonlySignal<Rgba>): ShaderUniformVec4 {
  return [0, 1, 2, 3].map((i) => computed(() => value()[i])) as unknown as ShaderUniformVec4;
}
