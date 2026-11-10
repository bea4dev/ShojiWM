/**
 * What is drawn while data is dragged between windows (COMPOSITOR.dnd), in
 * the style of Material 3 Expressive.
 *
 * The application's drag image sits on a card of the scheme's surface over
 * what is behind it, blurred, with a pill below naming what is dragged (text,
 * file, image, link, data). A badge on the card's corner says what a drop
 * would do: + copy (primary), → move (tertiary container), ? ask (secondary)
 * and × on a window that refuses it (error). Everything moves on springs:
 *
 * - the card pops out of the pointer when the drag starts;
 * - over a window that takes the data it grows a little, rounds off and gets
 *   a ring of the primary color, and the badge pops in (and pops again when
 *   the action changes);
 * - over a window that refuses it, the card shakes its head;
 * - an accepted drop sinks into the target; a drop nobody takes flies back to
 *   where the drag started, a little past it and back, and fades out there.
 */
import {
  Box,
  COMPOSITOR,
  DragIcon,
  Label,
  ShaderEffect,
  animationVariable,
  backdropSource,
  compileEffect,
  computed,
  dualKawaseBlur,
  effect,
  linear,
  loadShader,
  paintShader,
  signal,
  type AnimationVariable,
  type BoxShadowValue,
  type CompositionRenderable,
  type DragAction,
  type DragSession,
  type PaintShaderHandle,
} from "shoji_wm";
import { UI_FONT } from "./md3e/decoration";
import {
  EFFECTS_FAST,
  EMPHASIZED_ACCELERATE,
  SPATIAL_DEFAULT,
  SPATIAL_FAST,
  type Spring,
} from "./md3e/motion";
import { colorNow, isDark, rgba, rgbaNow, vec4, type Role } from "./md3e/theme";

/** Sinking into the target of an accepted drop, in ms. */
const DROP_MS = 200;
/** How much of the flight back is spent fading out, at its end. */
const RETURN_FADE = 0.3;
/** The head shake over a window that refuses the data: ms, logical px and swings. */
const SHAKE_MS = 420;
const SHAKE_PX = 6;
const SHAKE_SWINGS = 2;
/** How much larger the card is over a window that takes the data. */
const OVER_GROW = 0.06;

/** 0 to 1 when the drag starts: on a spatial spring for the size, an effects one for the opacity. */
const APPEAR = animationVariable("drag-appear");
const APPEAR_FADE = animationVariable("drag-appear-fade");
/** 1 while over a window that takes the data. */
const OVER = animationVariable("drag-over");
/** The badge's size, 0 when there is none. */
const BADGE = animationVariable("drag-badge");
/** 0 to 1 over one head shake. */
const SHAKE = animationVariable("drag-shake");
/** 0 to 1 over the drop or the flight back. */
const FINISH = animationVariable("drag-finish");

type DragKind = "text" | "file" | "image" | "link" | "data";

const KINDS: Record<DragKind, { label: string; index: number }> = {
  text: { label: "Text", index: 0 },
  file: { label: "File", index: 1 },
  image: { label: "Image", index: 2 },
  link: { label: "Link", index: 3 },
  data: { label: "Data", index: 4 },
};

interface Badge {
  /** Which glyph `dnd-badge-glyph.frag` draws. */
  glyph: number;
  /** The badge's color, and the glyph's on it. */
  role: Role;
  on: Role;
}

const ACTION_BADGES: Partial<Record<DragAction, Badge>> = {
  copy: { glyph: 0, role: "primary", on: "onPrimary" },
  move: { glyph: 1, role: "tertiaryContainer", on: "onTertiaryContainer" },
  ask: { glyph: 2, role: "secondary", on: "onSecondary" },
};
const REFUSED_BADGE: Badge = { glyph: 3, role: "error", on: "onError" };
/** The badge's size, logical px. */
const BADGE_SIZE = 28;
/** Drawn rather than set in a font, so that it sits in the middle of the badge. */
const BADGE_GLYPH = loadShader("./src/effect/dnd-badge-glyph.frag");

/** What the data is, from the formats it is offered in. */
function dragKind(mimeTypes: readonly string[]): DragKind {
  const has = (type: string) => mimeTypes.includes(type);
  if (has("text/x-moz-url") || has("_NETSCAPE_URL") || has("application/x-chromium-url")) {
    return "link";
  }
  if (has("text/uri-list")) {
    return "file";
  }
  if (mimeTypes.some((type) => type.startsWith("image/"))) {
    return "image";
  }
  if (
    mimeTypes.some(
      (type) => type.startsWith("text/") || type === "UTF8_STRING" || type === "STRING",
    )
  ) {
    return "text";
  }
  return "data";
}

function lerp(from: number, to: number, t: number): number {
  return from + (to - from) * t;
}

function clamp01(value: number): number {
  return Math.min(1, Math.max(0, value));
}

function smoothstep(edge0: number, edge1: number, x: number): number {
  const t = clamp01((x - edge0) / (edge1 - edge0));
  return t * t * (3 - 2 * t);
}

/** Runs one of the drag's animation variables to `to` on a spring, from `from` when given. */
function springTo(
  drag: DragSession,
  variable: AnimationVariable,
  to: number,
  spring: Spring,
  from?: number,
): Promise<boolean> {
  const timing = { to, duration: spring.duration, easing: spring.easing };
  return drag.animation.start(variable, from === undefined ? timing : { ...timing, from });
}

/** What is behind the card, blurred like the windows' backdrops. */
const CARD_BLUR = compileEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: { kind: "on-source-damage-box", damagePadding: 8 },
  alpha: "preserve",
  pipeline: [dualKawaseBlur({ radius: 4, passes: 2 })],
});

/** Material 3's level 3 shadow, rising toward level 4 as `t` goes to 1; lighter on light schemes. */
function cardShadow(t: number): BoxShadowValue[] {
  const k = clamp01(t);
  const strength = isDark() ? 1 : 0.55;
  const alpha = (a: number) =>
    Math.round(Math.min(1, a * strength) * 255)
      .toString(16)
      .padStart(2, "0");
  return [
    { y: lerp(6, 10, k), blur: lerp(16, 24, k), color: `#000000${alpha(0.36)}` },
    { y: 2, blur: 4, color: `#000000${alpha(0.24)}` },
  ];
}

/** The kind's line icon in a role's color, as a paint shader to make once per drag. */
function kindIcon(kind: DragKind, role: Role): PaintShaderHandle {
  return paintShader("./src/effect/dnd-kind-icon.frag", {
    uniforms: { kind: KINDS[kind].index, color: vec4(rgba(role)) },
  });
}

/** What the card looks like now. */
interface Look {
  kind: DragKind;
  /** 0..1, past 1 while a spring overshoots. */
  over: number;
  badge: Badge;
  badgeSize: number;
  tileIcon: PaintShaderHandle;
  pillIcon: PaintShaderHandle;
}

/**
 * The card, built again whenever what it reads changes. It is made of plain
 * values: a signal made here would be made on every build, and each one
 * would make it build again.
 */
function dragCard({ kind, over, badge, badgeSize, tileIcon, pillIcon }: Look): CompositionRenderable {
  const o = clamp01(over);
  const radius = 16 + 8 * o;
  return (
    <Box direction="column" style={{ alignItems: "center", gap: 8 }}>
      <Box style={{ position: "relative" }}>
        <Box style={{ borderRadius: radius, overflow: "hidden", boxShadow: cardShadow(o) }}>
          <ShaderEffect shader={CARD_BLUR} direction="column">
            <Box
              style={{
                padding: 8,
                borderRadius: radius,
                background: colorNow("surfaceContainerHigh", 0.9),
                border: { px: 2, color: colorNow("primary", o) },
              }}
            >
              <DragIcon>
                {/* No picture from the application: a tile with the kind's icon. */}
                <Box
                  style={{
                    width: 48,
                    height: 48,
                    borderRadius: 12 + 6 * o,
                    background: colorNow("primaryContainer"),
                    alignItems: "center",
                    justifyContent: "center",
                  }}
                >
                  <Box paint={tileIcon} style={{ width: 26, height: 26 }} />
                </Box>
              </DragIcon>
            </Box>
          </ShaderEffect>
        </Box>
        {/* Material 3's large badge on the corner, popping in and out. */}
        <Box
          style={{
            position: "absolute",
            top: -10,
            right: -10,
            width: BADGE_SIZE,
            height: BADGE_SIZE,
            borderRadius: BADGE_SIZE / 2,
            background: colorNow(badge.role),
            boxShadow: [{ y: 1, blur: 3, color: "#0000004d" }],
            alignItems: "center",
            justifyContent: "center",
            opacity: clamp01(badgeSize * 2),
            transform: { scale: Math.max(0, badgeSize) },
          }}
        >
          {/* Plain uniforms: the content is built again when the badge changes. */}
          <Box
            paint={paintShader(BADGE_GLYPH, {
              uniforms: { kind: badge.glyph, color: rgbaNow(badge.on) },
            })}
            style={{ width: BADGE_SIZE, height: BADGE_SIZE }}
          />
        </Box>
      </Box>
      <Box
        direction="row"
        style={{
          paddingLeft: 10,
          paddingRight: 12,
          height: 28,
          gap: 6,
          alignItems: "center",
          borderRadius: 14,
          background: colorNow("secondaryContainer", 1, "primaryContainer", o),
          boxShadow: [{ y: 1, blur: 3, color: "#00000040" }],
        }}
      >
        <Box paint={pillIcon} style={{ width: 14, height: 14 }} />
        <Label
          text={KINDS[kind].label}
          style={{
            color: colorNow("onSecondaryContainer"),
            fontFamily: UI_FONT,
            fontSize: 12,
            fontWeight: 500,
          }}
        />
      </Box>
    </Box>
  );
}

export function installDragAndDrop(): void {
  COMPOSITOR.dnd.event.onStart((drag) => {
    const kind = dragKind(drag.mimeTypes);
    const tileIcon = kindIcon(kind, "onPrimaryContainer");
    const pillIcon = kindIcon(kind, "onSecondaryContainer");
    const appear = drag.animation.variable(APPEAR);
    const appearFade = drag.animation.variable(APPEAR_FADE);
    const over = drag.animation.variable(OVER);
    const badgeSize = drag.animation.variable(BADGE);
    const shake = drag.animation.variable(SHAKE);
    const finish = drag.animation.variable(FINISH);
    void springTo(drag, APPEAR, 1, SPATIAL_FAST, 0);
    void springTo(drag, APPEAR_FADE, 1, EFFECTS_FAST, 0);

    // What a drop would do now; the last badge stays while it shrinks away.
    const wanted = computed<Badge | null>(() => {
      const action = drag.action();
      if (ACTION_BADGES[action]) {
        return ACTION_BADGES[action];
      }
      return action === "none" && drag.target() !== null ? REFUSED_BADGE : null;
    });
    const [shownBadge, setShownBadge] = signal<Badge>(REFUSED_BADGE);
    let shown: Badge | null = null;
    const stopWatching = effect(() => {
      const next = wanted();
      if (next === shown) {
        return;
      }
      const takes = (b: Badge | null) => b !== null && b !== REFUSED_BADGE;
      if (next === null) {
        void springTo(drag, BADGE, 0, EFFECTS_FAST);
      } else {
        setShownBadge(next);
        // In from nothing, or again from a little smaller for another action.
        void springTo(drag, BADGE, 1, SPATIAL_FAST, shown === null ? 0 : 0.5);
      }
      if (takes(next) !== takes(shown)) {
        void springTo(drag, OVER, takes(next) ? 1 : 0, takes(next) ? SPATIAL_FAST : SPATIAL_DEFAULT);
      }
      if (next === REFUSED_BADGE) {
        void drag.animation.start(SHAKE, { from: 0, to: 1, duration: SHAKE_MS, easing: linear });
      }
      shown = next;
    });

    // Until the drag ends the view rides on the pointer; then it is pinned
    // to the screen and either sinks in place (accepted) or flies back to
    // where the drag started (refused or cancelled).
    const ended = drag.ended;
    const position = (axis: "x" | "y") =>
      computed(() => {
        const end = ended();
        if (!end) {
          // A head shake: swings that die down.
          const t = shake();
          return axis === "y" || t <= 0 || t >= 1
            ? 0
            : Math.sin(t * Math.PI * 2 * SHAKE_SWINGS) * (1 - t) * SHAKE_PX;
        }
        return end.accepted
          ? end.position[axis]
          : lerp(end.position[axis], drag.origin[axis], finish());
      });

    const view = drag.show({
      anchor: computed(() => (ended() ? "global" : "pointer")),
      x: position("x"),
      y: position("y"),
      opacity: computed(() => {
        const end = ended();
        const shownNow = clamp01(appearFade());
        if (!end) {
          return shownNow;
        }
        return end.accepted
          ? shownNow * (1 - clamp01(finish()))
          : shownNow * (1 - smoothstep(1 - RETURN_FADE, 1, finish()));
      }),
      scale: computed(() => {
        const end = ended();
        const popped = (0.5 + 0.5 * appear()) * (1 + OVER_GROW * over());
        if (!end) {
          return popped;
        }
        return end.accepted
          ? popped * (1 - 0.4 * clamp01(finish()))
          : popped * (1 - 0.1 * clamp01(finish()));
      }),
      content: () =>
        dragCard({
          kind,
          over: over(),
          badge: shownBadge(),
          badgeSize: badgeSize(),
          tileIcon,
          pillIcon,
        }),
    });

    drag.onEnd((end) => {
      stopWatching();
      const done = end.accepted
        ? drag.animation.start(FINISH, {
            from: 0,
            to: 1,
            duration: DROP_MS,
            easing: EMPHASIZED_ACCELERATE,
          })
        : springTo(drag, FINISH, 1, SPATIAL_DEFAULT, 0);
      if (!end.accepted) {
        void springTo(drag, OVER, 0, SPATIAL_DEFAULT);
        void springTo(drag, BADGE, 0, EFFECTS_FAST);
      }
      void done.then(() => view.dispose());
    });
  });
}
