/**
 * Window frames in the style of Material 3 Expressive.
 *
 * - A client that wants server-side decorations gets a title bar of the
 *   scheme's surface colors: its icon and title, and minimize / maximize /
 *   close as icon buttons whose bubbles spring out of the middle.
 * - A client that draws its own decorations keeps them.
 *
 * Both get large round corners, a hairline just inside the edge and a soft
 * shadow that rises when the window has focus. Everything fades between the
 * looks with and without focus, and for a moment after a window takes focus
 * its border grows thicker.
 *
 * What is behind a window is blurred under it, so a translucent client (kitty
 * with `background_opacity`) and the title bar show it frosted. One sheet
 * behind the whole window: blurring only the visible strips would cost more
 * captures, and an opaque client simply hides it.
 */
import {
  AppIcon,
  Box,
  Button,
  COMPOSITOR,
  ClientWindow,
  Label,
  Popup,
  ShaderEffect,
  WindowBorder,
  animationVariable,
  backdropSource,
  compileEffect,
  dualKawaseBlur,
  computed,
  linear,
  loadShader,
  paintShader,
  read,
  signal,
  useEffect,
  useRef,
  useState,
  type AnimationVariable,
  type BoxShadowValue,
  type CompositionRenderable,
  type MaybeSignal,
  type ReadonlySignal,
  type WaylandWindow,
  type WindowDecorationContext,
  type WindowDecorationState,
} from "shoji_wm";
import type { ManagedWindowRect } from "shoji_wm/types";
import { EFFECTS_DEFAULT, EFFECTS_FAST, SPATIAL_FAST, SPATIAL_DEFAULT, type Spring } from "./motion";
import { color, colorBetween, isDark, rgba, rgbaBetween, vec4, type Rgba } from "./theme";

export type CaptionButtonKind = "minimize" | "maximize" | "close";

/** Lengths of the frame, logical px. */
export const FRAME = {
  titleBar: 36,
  /** The corners of a window, Material 3's large-increased shape. */
  radius: 16,
  /** A client's own decorations are cut at a smaller radius. */
  clientRadius: 14,
  /** The buttons' places, and the room after the last one. */
  buttonWidth: 36,
  buttonsRight: 6,
} as const;

export const UI_FONT: string[] = [
  "Roboto Flex",
  "Google Sans",
  "Roboto",
  "Noto Sans",
  "Noto Sans CJK JP",
  "Noto Color Emoji",
];

export interface FrameOptions {
  window: WaylandWindow;
  /** The frame's rect on screen (logical px). */
  rect: ReadonlySignal<ManagedWindowRect>;
  /** The window is drawn into a texture of its own (the switcher). */
  backdropless: boolean;
  /** Takes the place of the shadow while it holds one (the switcher's pick). */
  shadow?: MaybeSignal<BoxShadowValue[] | undefined>;
}

const BUTTON_SHADER = loadShader("./src/md3e/shaders/caption-button.frag");
const BORDER_SHADER = loadShader("./src/md3e/shaders/focus-border.frag");

const FROSTED = compileEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: { kind: "on-source-damage-box", damagePadding: 8 },
  alpha: "preserve",
  pipeline: [dualKawaseBlur({ radius: 4, passes: 2 })],
});

/** How opaque the title bar is over the blur. */
const TITLE_BAR_ALPHA = 1.0;

/**
 * The window's content over the blurred backdrop; without one (inside the
 * switcher's textures) there is nothing behind to blur.
 */
function frosted(options: FrameOptions, children: CompositionRenderable[]): CompositionRenderable {
  if (options.backdropless) {
    return (
      <Box direction="column" style={{ flexGrow: 1 }}>
        {children}
      </Box>
    );
  }
  return (
    <ShaderEffect shader={FROSTED} direction="column" style={{ flexGrow: 1 }}>
      {children}
    </ShaderEffect>
  );
}

const FRAME_RESIZE = { edgePx: 6, cornerPx: 22 };
const NO_RESIZE = { edgePx: 0, cornerPx: 0 };

/**
 * The decoration policy: whatever the client asks for, server-side when it
 * does not say. A Wayland client without a decoration protocol (GTK 4,
 * libadwaita) always draws its own frame.
 */
export function decorationPolicy(
  _window: WaylandWindow,
  context: WindowDecorationContext,
): { mode: "client" | "server" } {
  if (context.protocol === "none") {
    return { mode: "client" };
  }
  return { mode: context.clientPreference ?? "server" };
}

/** Runs a window's animation variable to `to` on a spring. */
function springTo(window: WaylandWindow, variable: AnimationVariable, to: number, spring: Spring) {
  void window.animation.start(variable, { to, duration: spring.duration, easing: spring.easing });
}

const FOCUS = animationVariable("md3e-focus");
/** 0 to 1, once, over the border's swell after the window takes focus. */
const SWELL_TIME = animationVariable("md3e-border-swell");

/** Moves frames between their looks with and without focus. Call once from the config. */
export function installFocusFade(): () => void {
  return COMPOSITOR.event.onFocus((window, focused) => {
    springTo(window, FOCUS, focused ? 1 : 0, EFFECTS_DEFAULT);
    if (focused) {
      window.animation.set(SWELL_TIME, 0);
      void window.animation.start(SWELL_TIME, { to: 1, duration: BORDER_SWELL.duration, easing: linear });
    }
  });
}

/** 1 while the window has focus, 0 without, moving in between. */
function activeness(window: WaylandWindow): ReadonlySignal<number> {
  const fade = window.animation.variable(FOCUS);
  return computed(() => {
    const focused = window.isFocused() ? 1 : 0;
    return window.animation.running(FOCUS) ? fade() : focused;
  });
}

/** The client draws its own frame (rather than asking for ours). */
export function usesClientDecoration(decoration: WindowDecorationState): boolean {
  return (
    decoration.mode === "client" &&
    !(decoration.clientPreference === "server" && decoration.configuredMode === "server")
  );
}

/** How far the frame reaches above the client: the title bar, or nothing. */
function titleBarHeight(window: WaylandWindow): number {
  return usesClientDecoration(window.decoration.peek()) ? 0 : FRAME.titleBar;
}

/** The frame rect around a window's client, for the window manager. */
export function naturalRootRect(window: WaylandWindow): ManagedWindowRect {
  const client = window.position;
  const top = titleBarHeight(window);
  const b = BORDER.width;
  return {
    x: client.x - b,
    y: client.y - top - b,
    width: client.width + 2 * b,
    height: client.height + top + 2 * b,
  };
}

/** A dialog (a window with a parent) only has a close button. */
function buttonsOf(window: WaylandWindow, peek: boolean): readonly CaptionButtonKind[] {
  const transient = peek ? window.isTransient.peek() : window.isTransient();
  return transient ? ["close"] : ["minimize", "maximize", "close"];
}

/**
 * The caption button at `(x, y)`, logical px from the top-left corner of the
 * frame, for code that draws the window itself (the window switcher).
 */
export function captionButtonAt(window: WaylandWindow, x: number, y: number): CaptionButtonKind | null {
  if (window.isFullscreen.peek() || titleBarHeight(window) === 0) {
    return null;
  }
  if (y < BORDER.width || y >= BORDER.width + FRAME.titleBar) {
    return null;
  }
  const buttons = buttonsOf(window, true);
  const left =
    window.rect.width - BORDER.width - FRAME.buttonsRight - buttons.length * FRAME.buttonWidth;
  const index = Math.floor((x - left) / FRAME.buttonWidth);
  return index >= 0 && index < buttons.length ? buttons[index] : null;
}

/**
 * The shadow under a frame: Material 3's level 3 with focus and level 1
 * without, moving between them with the focus. Light schemes get lighter
 * shadows.
 */
function frameShadow(options: FrameOptions): MaybeSignal<BoxShadowValue[]> {
  const { window, shadow } = options;
  const active = activeness(window);
  const focusShadow = computed<BoxShadowValue[]>(() => {
    const t = active();
    const strength = isDark() ? 1 : 0.55;
    const lerp = (a: number, b: number) => a + (b - a) * t;
    const alpha = (a: number) =>
      Math.round(Math.min(1, a * strength) * 255)
        .toString(16)
        .padStart(2, "0");
    return [
      { y: lerp(2, 10), blur: lerp(8, 30), spread: lerp(0, 2), color: `#000000${alpha(lerp(0.28, 0.42))}` },
      { y: lerp(1, 2), blur: lerp(3, 6), color: `#000000${alpha(lerp(0.2, 0.26))}` },
    ];
  });
  if (shadow === undefined) {
    return focusShadow;
  }
  return computed(() => read(shadow) ?? focusShadow());
}

/**
 * The border every window has, white on the focused one and gray on the
 * others. It is the frame's own border, around the client: anything drawn
 * over the client area would be under the client's surface.
 */
export const BORDER = {
  /** Logical px; twice it is whole (it is on both sides), so the client keeps a whole size. */
  width: 1.5,
  focused: [1, 1, 1, 0.5] as Rgba,
  unfocused: [0.5, 0.5, 0.5, 0.5] as Rgba,
};

/**
 * How the border grows thicker after the window takes focus: quickly, then
 * back slowly to its own width before it stops; back early when the window
 * loses focus. Every value here may be tuned.
 */
export const BORDER_SWELL = {
  /** How long it lasts, ms. */
  duration: 500,
  /** How much wider than the border the line is at its fullest, outward; logical px. */
  thicken: 2,
  /** The part of `duration` it takes to grow (0..1). */
  rise: 0.15,
  /** The part of `duration` it takes to thin again, at the end (0..1). */
  fall: 0.55,
};

/** How much of the swell there is at `t` of its time: out quickly, back slowly, smooth at both ends. */
function swellAt(t: number): number {
  if (t <= 0 || t >= 1) {
    return 0;
  }
  const out = Math.min(1, t / Math.max(BORDER_SWELL.rise, 0.001));
  const back = Math.min(1, (1 - t) / Math.max(BORDER_SWELL.fall, 0.001));
  return (1 - (1 - out) ** 3) * back * back * (3 - 2 * back);
}

/** The border's paint: the plain border, wider while it swells. */
function borderPaint(window: WaylandWindow) {
  const active = activeness(window);
  const t = window.animation.variable(SWELL_TIME);
  return paintShader(BORDER_SHADER, {
    uniforms: {
      thicken: computed(() => BORDER_SWELL.thicken * swellAt(t()) * active()),
    },
    // Room for the wider line outside the frame.
    outsets: Math.ceil(BORDER_SWELL.thicken + 1),
  });
}

function borderOf(window: WaylandWindow) {
  const active = activeness(window);
  const color = computed(() => {
    const t = active();
    const byte = (i: number) =>
      Math.round((BORDER.unfocused[i] + (BORDER.focused[i] - BORDER.unfocused[i]) * t) * 255)
        .toString(16)
        .padStart(2, "0");
    return `#${byte(0)}${byte(1)}${byte(2)}${byte(3)}`;
  });
  return { px: BORDER.width, color };
}

const HOVER: Record<CaptionButtonKind, AnimationVariable> = {
  minimize: animationVariable("md3e-hover-minimize"),
  maximize: animationVariable("md3e-hover-maximize"),
  close: animationVariable("md3e-hover-close"),
};
const PRESS: Record<CaptionButtonKind, AnimationVariable> = {
  minimize: animationVariable("md3e-press-minimize"),
  maximize: animationVariable("md3e-press-maximize"),
  close: animationVariable("md3e-press-close"),
};
const RESTORE = animationVariable("md3e-restore-glyph");

const [forcedButton, setForcedButton] = signal<{ kind: CaptionButtonKind; pressed: boolean } | null>(
  null,
);

/**
 * Draw caption button `kind` of focused windows as hovered or pressed, as if
 * the pointer were on it (`null` to stop): for screenshots and the preview.
 */
export function forceCaptionButtonState(kind: CaptionButtonKind | null, pressed = false): void {
  setForcedButton(kind ? { kind, pressed } : null);
}

const TOOLTIPS: Record<CaptionButtonKind, string> = {
  minimize: "Minimize",
  maximize: "Maximize",
  close: "Close",
};

/** A plain tooltip of Material 3, under the button once the pointer rests on it. */
const Tooltip = ({ text }: { text: MaybeSignal<string> }) => (
  <Popup trigger="hover" openDelay={600} closeDelay={0} placement="bottom" offset={4}>
    <Box
      style={{
        background: color("inverseSurface"),
        borderRadius: 4,
        paddingX: 8,
        paddingTop: 4,
        paddingBottom: 5,
      }}
    >
      <Label text={text} style={{ color: color("inverseOnSurface"), fontFamily: UI_FONT, fontSize: 12 }} />
    </Box>
  </Popup>
);

const CaptionButton = ({ window, kind }: { window: WaylandWindow; kind: CaptionButtonKind }) => {
  const [hovered, setHovered] = useState(false);
  const disabled = kind === "maximize" && !window.isResizable();
  const active = activeness(window);
  const hover = window.animation.variable(HOVER[kind]);
  const press = window.animation.variable(PRESS[kind]);
  const close = kind === "close";
  // The maximize glyph follows the window into and out of being maximized,
  // however that happens; it starts where the window is.
  const maximized = kind === "maximize" && window.isMaximized();
  const first = useRef(true);
  useEffect(() => {
    if (kind !== "maximize") {
      return;
    }
    if (first.current) {
      first.current = false;
      window.animation.set(RESTORE, maximized ? 1 : 0);
    } else {
      springTo(window, RESTORE, maximized ? 1 : 0, SPATIAL_DEFAULT);
    }
  }, [maximized]);
  const restore = kind === "maximize" ? window.animation.variable(RESTORE) : 0;
  const forced = computed(() => {
    const state = forcedButton();
    return state?.kind === kind && window.isFocused() ? state : null;
  });
  const face = paintShader(BUTTON_SHADER, {
    uniforms: {
      kind: kind === "minimize" ? 0 : kind === "maximize" ? 1 : 2,
      hover: disabled ? 0 : computed(() => (forced() ? 1 : hover())),
      press: disabled ? 0 : computed(() => (forced()?.pressed ? 1 : press())),
      restore,
      bubble: vec4(close ? rgba("error") : rgba("secondaryContainer")),
      glyph: vec4(
        rgbaBetween(
          "outline",
          "onSurfaceVariant",
          computed(() => (disabled ? 0 : active())),
        ),
      ),
      glyph_on: vec4(close ? rgba("onError") : rgba("onSecondaryContainer")),
    },
  });
  const onHoverChange = (on: boolean) => {
    setHovered(on);
    springTo(window, HOVER[kind], on ? 1 : 0, on ? SPATIAL_FAST : EFFECTS_FAST);
    if (!on) {
      springTo(window, PRESS[kind], 0, SPATIAL_FAST);
    }
  };
  const onActiveChange = (down: boolean) => {
    springTo(window, PRESS[kind], down && hovered.peek() ? 1 : 0, SPATIAL_FAST);
  };
  const onClick = () => {
    if (kind === "minimize") {
      window.minimize();
    } else if (kind === "close") {
      window.close();
    } else if (read(window.isResizable)) {
      if (read(window.isMaximized)) {
        window.unmaximize();
      } else {
        window.maximize();
      }
    }
  };
  const tooltip =
    kind === "maximize"
      ? window.isMaximized((maximized) => (maximized ? "Restore" : "Maximize"))
      : TOOLTIPS[kind];
  return (
    <Button
      paint={face}
      onHoverChange={disabled ? undefined : onHoverChange}
      onActiveChange={disabled ? undefined : onActiveChange}
      onClick={onClick}
      style={{ width: FRAME.buttonWidth, height: FRAME.titleBar, flexShrink: 0 }}
    >
      {disabled ? null : <Tooltip text={tooltip} />}
    </Button>
  );
};

const TitleBar = ({ window, alpha }: { window: WaylandWindow; alpha: number }) => {
  const active = activeness(window);
  const buttons = buttonsOf(window, false);
  return (
    <Box
      direction="row"
      style={{
        height: FRAME.titleBar,
        flexShrink: 0,
        paddingLeft: 14,
        paddingRight: FRAME.buttonsRight,
        gap: 10,
        alignItems: "center",
        background: colorBetween("surfaceContainer", "surfaceContainerHigh", active, alpha),
      }}
    >
      <AppIcon
        icon={window.icon}
        style={{ width: 18, height: 18, flexShrink: 0, opacity: computed(() => 0.7 + 0.3 * active()) }}
      />
      <Label
        text={window.title}
        style={{
          color: colorBetween("onSurfaceVariant", "onSurface", active),
          fontFamily: UI_FONT,
          fontSize: 13,
          fontWeight: 500,
          flexGrow: 1,
          flexShrink: 1,
          minWidth: 0,
          textOverflow: "ellipsis",
        }}
      />
      <Box direction="row" style={{ flexShrink: 0 }}>
        {buttons.map((kind) => (
          <CaptionButton window={window} kind={kind} />
        ))}
      </Box>
    </Box>
  );
};

/** The frame, for a client that asked for server-side decorations. */
export function Md3Frame(options: FrameOptions): CompositionRenderable {
  const { window } = options;
  const maximized = window.isMaximized();
  return (
    <WindowBorder
      style={{
        borderRadius: FRAME.radius,
        boxShadow: frameShadow(options),
        // Only the title bar is painted: a translucent client shows what is
        // behind the window, not the frame.
        background: "#00000000",
        border: borderOf(window),
        overflow: "hidden",
      }}
      paint={borderPaint(window)}
      interaction={{ resizeHitArea: maximized ? NO_RESIZE : FRAME_RESIZE }}
    >
      {frosted(options, [
        <TitleBar window={window} alpha={options.backdropless ? 1 : TITLE_BAR_ALPHA} />,
        <ClientWindow />,
      ])}
    </WindowBorder>
  );
}

/** A client-decorated window: its own title bar, our corners, hairline and shadow. */
export function Md3Rim(options: FrameOptions): CompositionRenderable {
  const { window } = options;
  const maximized = window.isMaximized();
  return (
    <WindowBorder
      style={{
        borderRadius: FRAME.clientRadius,
        boxShadow: frameShadow(options),
        background: "#00000000",
        border: borderOf(window),
        overflow: "hidden",
      }}
      paint={borderPaint(window)}
      interaction={{ resizeHitArea: maximized ? NO_RESIZE : FRAME_RESIZE }}
    >
      {frosted(options, [<ClientWindow />])}
    </WindowBorder>
  );
}
