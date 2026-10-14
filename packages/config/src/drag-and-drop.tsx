/**
 * What is drawn while data is dragged between windows (COMPOSITOR.dnd).
 *
 * The application's drag image sits on a small frosted card (what is behind
 * it, blurred and tinted) with a badge for the
 * action a drop would take (+ copy, → move, ? ask, × refused), and a pill
 * below names what is dragged (text, file, image, link, data) with an icon.
 * The card pops in when the drag starts. An accepted drop sinks into the
 * target; a drop nobody accepts flies back to where the drag started, with
 * the easing every window movement uses, and fades out there (like macOS).
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
  cubicBezier,
  dualKawaseBlur,
  paintShader,
  type CompositionRenderable,
  type DragAction,
  type DragSession,
} from "shoji_wm";
import { WINDOW_MANAGEMENT_EASING } from "./window-manager";

/** Pop-in when the drag starts, in ms. */
const APPEAR_MS = 220;
/** Flying back after a drop nobody accepted, in ms. */
const RETURN_MS = 420;
/** Sinking into the target of an accepted drop, in ms. */
const DROP_MS = 180;
/** How much of the flight back is spent fading out, at its end. */
const RETURN_FADE = 0.3;

const DROP_EASING = cubicBezier(0.4, 0, 1, 1);

const APPEAR = animationVariable("drag-appear");
const FINISH = animationVariable("drag-finish");

const FONT = ["Noto Sans CJK JP", "Noto Color Emoji"];
/** Over the blur, so text stays readable on any background. */
const CARD_TINT = "#1f243099";
const CARD_BORDER = { px: 1, color: "#ffffff1f" };
const CARD_SHADOW = [{ y: 6, blur: 18, color: "#00000080" }];

type DragKind = "text" | "file" | "image" | "link" | "data";

const KINDS: Record<
  DragKind,
  { label: string; index: number; color: [number, number, number, number] }
> = {
  text: { label: "Text", index: 0, color: [0.55, 0.76, 1.0, 1.0] },
  file: { label: "File", index: 1, color: [0.98, 0.8, 0.45, 1.0] },
  image: { label: "Image", index: 2, color: [0.6, 0.88, 0.62, 1.0] },
  link: { label: "Link", index: 3, color: [0.78, 0.66, 1.0, 1.0] },
  data: { label: "Data", index: 4, color: [0.75, 0.78, 0.84, 1.0] },
};

const KIND_ICONS = Object.fromEntries(
  (Object.keys(KINDS) as DragKind[]).map((kind) => [
    kind,
    paintShader("./src/effect/dnd-kind-icon.frag", {
      uniforms: { kind: KINDS[kind].index, color: KINDS[kind].color },
    }),
  ]),
) as Record<DragKind, ReturnType<typeof paintShader>>;

const ACTION_BADGES: Partial<Record<DragAction, { text: string; color: string }>> = {
  copy: { text: "+", color: "#30d158" },
  move: { text: "→", color: "#0a84ff" },
  ask: { text: "?", color: "#ff9f0a" },
};
const REFUSED_BADGE = { text: "×", color: "#ff453a" };

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

function smoothstep(edge0: number, edge1: number, x: number): number {
  const t = Math.min(1, Math.max(0, (x - edge0) / (edge1 - edge0)));
  return t * t * (3 - 2 * t);
}

/** What is behind the card, blurred like the windows' backdrops. */
const CARD_BLUR = compileEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: { kind: "on-source-damage-box", damagePadding: 8 },
  alpha: "preserve",
  pipeline: [dualKawaseBlur({ radius: 4, passes: 2 })],
});

/** A blurred, tinted rounded panel laying its children out like a `<Box>`. */
const FrostedPanel = ({
  direction = "column",
  radius,
  style,
  children,
}: {
  direction?: "row" | "column";
  radius: number;
  style?: Record<string, unknown>;
  children?: CompositionRenderable | CompositionRenderable[];
}) => (
  <Box style={{ borderRadius: radius, overflow: "hidden", boxShadow: CARD_SHADOW }}>
    <ShaderEffect shader={CARD_BLUR} direction="column">
      <Box
        direction={direction}
        style={{ ...style, background: CARD_TINT, borderRadius: radius, border: CARD_BORDER }}
      >
        {children}
      </Box>
    </ShaderEffect>
  </Box>
);

const KindIcon = ({ kind, size }: { kind: DragKind; size: number }) => (
  <Box paint={KIND_ICONS[kind]} style={{ width: size, height: size }} />
);

/** The badge on the card's corner; none while the drag is over nothing. */
function badgeFor(drag: DragSession) {
  const action = drag.action.value;
  const badge = ACTION_BADGES[action];
  if (badge) {
    return badge;
  }
  return action === "none" && drag.target.value !== null ? REFUSED_BADGE : null;
}

function DragCard({ drag, kind }: { drag: DragSession; kind: DragKind }) {
  const badge = badgeFor(drag);
  return (
    <Box direction="column" style={{ alignItems: "center", gap: 6 }}>
      <Box style={{ position: "relative" }}>
        <FrostedPanel radius={10} style={{ padding: 6 }}>
          <DragIcon>
            {/* No picture from the application: a tile with the kind's icon. */}
            <Box
              style={{
                width: 40,
                height: 40,
                alignItems: "center",
                justifyContent: "center",
              }}
            >
              <KindIcon kind={kind} size={26} />
            </Box>
          </DragIcon>
        </FrostedPanel>
        {badge ? (
          // A dark disc with the coloured one inside it, rather than a
          // coloured disc with a border: both edges of a border are
          // antialiased over the fill, which would show a coloured fringe
          // outside the ring.
          <Box
            style={{
              position: "absolute",
              top: -7,
              right: -7,
              width: 20,
              height: 20,
              padding: 2,
              borderRadius: 10,
              background: "#1f2430",
              boxShadow: [{ y: 1, blur: 4, color: "#00000080" }],
            }}
          >
            <Box
              style={{
                width: 16,
                height: 16,
                borderRadius: 8,
                background: badge.color,
                alignItems: "center",
                justifyContent: "center",
              }}
            >
              <Label
                text={badge.text}
                style={{ color: "#ffffff", fontFamily: FONT, fontSize: 12, fontWeight: "bold" }}
              />
            </Box>
          </Box>
        ) : null}
      </Box>
      <FrostedPanel
        direction="row"
        radius={11}
        style={{ paddingLeft: 8, paddingRight: 10, paddingY: 3, gap: 6, alignItems: "center" }}
      >
        <KindIcon kind={kind} size={13} />
        <Label
          text={KINDS[kind].label}
          style={{ color: "#e6e9ef", fontFamily: FONT, fontSize: 12 }}
        />
      </FrostedPanel>
    </Box>
  );
}

export function installDragAndDrop(): void {
  COMPOSITOR.dnd.event.onStart((drag) => {
    const kind = dragKind(drag.mimeTypes);
    const appear = drag.animation.variable(APPEAR);
    const finish = drag.animation.variable(FINISH);
    drag.animation.start(APPEAR, {
      from: 0,
      to: 1,
      duration: APPEAR_MS,
      easing: WINDOW_MANAGEMENT_EASING,
    });

    // Until the drag ends the view rides on the pointer; then it is pinned
    // to the screen and either sinks in place (accepted) or flies back to
    // where the drag started (refused or cancelled).
    const ended = drag.ended;
    const position = (axis: "x" | "y") =>
      computed(() => {
        const end = ended.value;
        if (!end) {
          return 0;
        }
        return end.accepted
          ? end.position[axis]
          : lerp(end.position[axis], drag.origin[axis], finish.value);
      });

    const view = drag.show({
      anchor: computed(() => (ended.value ? "global" : "pointer")),
      x: position("x"),
      y: position("y"),
      opacity: computed(() => {
        const end = ended.value;
        const shown = appear.value;
        // A refusing window only gets the × badge: fading the card would
        // let what is behind show through its blur.
        if (!end) {
          return shown;
        }
        return end.accepted
          ? 1 - finish.value
          : 1 - smoothstep(1 - RETURN_FADE, 1, finish.value);
      }),
      scale: computed(() => {
        const end = ended.value;
        const popped = 0.85 + 0.15 * appear.value;
        if (!end) {
          return popped;
        }
        return end.accepted
          ? popped * (1 - 0.4 * finish.value)
          : popped * (1 - 0.08 * finish.value);
      }),
      content: () => <DragCard drag={drag} kind={kind} />,
    });

    drag.onEnd((end) => {
      drag.animation
        .start(FINISH, {
          from: 0,
          to: 1,
          duration: end.accepted ? DROP_MS : RETURN_MS,
          easing: end.accepted ? DROP_EASING : WINDOW_MANAGEMENT_EASING,
        })
        .then(() => view.dispose());
    });
  });
}
