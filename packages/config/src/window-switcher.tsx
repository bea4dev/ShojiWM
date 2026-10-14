/**
 * The window switchers' common core (`Super+Tab`).
 *
 * A switcher lifts every window (all workspaces, minimized ones too, most
 * recently used first) off the desktop and lays them out in a scene; a
 * `SwitcherLayout` decides where each window goes and how input moves the
 * selection (`./flip-3d.tsx`: a receding 3D stack, `./window-grid.tsx`: a
 * flat grid). The core does the rest, the same for every layout:
 *
 * - `Escape` returns to the desktop unchanged
 * - `Return` / `Space` or a click on a window switches
 *
 * Each window is a render texture of its own, framed with a margin for its
 * shadow, laid out as a plane of a 3D scene. Opening and closing ease with
 * the window manager's easing; the layout starts and ends exactly on the
 * windows' real positions, so the desktop morphs into it and back.
 *
 * A layout with `cardActions` (the grid) also acts on the windows: the
 * title bar buttons of the selected card minimize, maximize or close its
 * window, a middle click closes it, and the other cards make room when one
 * closes. A window that asks before closing ("save changes?") ends the
 * switcher on it, so its dialog is not hidden behind the cards.
 *
 * Backdrop blur cannot work inside a texture that holds one window (nothing
 * lies under the window there), so window blur is switched off while the
 * switcher is open (`blurSuspended`) and faded back in afterwards
 * (`backdropStrength`): the hand-off back to the real desktop happens in the
 * slow tail of the closing animation, and the blur fades in over that tail.
 */
import {
  COMPOSITOR,
  createPoll,
  easeOutCubic,
  LayerPopups,
  Layers,
  outputLogicalSize,
  pickPlane,
  Plane,
  projectPoint,
  renderTexture,
  Scene3D,
  lookAt,
  perspective,
  signal,
  Solid,
  transform3d,
  Windows,
  type Camera,
  type CompositionRenderable,
  type GestureSwipeEvent,
  type InputGrab,
  type InputGrabKeyEvent,
  type InputGrabPointerMotionEvent,
  type InputGrabScrollEvent,
  type OutputInfo,
  type PollHandle,
  type ReadonlySignal,
  type WaylandWindow,
} from "shoji_wm";
import {
  WINDOW_MANAGEMENT_EASING,
  WINDOW_STATE_MINIMIZED,
  type HybridWindowManager,
} from "./window-manager";

/** Opening and closing, in ms. */
const TRANSITION_MS = 450;
/** The blur fading back in after closing, in ms. */
const BLUR_FADE_MS = 350;
/** Room around each window's texture for its shadow (logical px). */
export const MARGIN = 48;
/** Closing hands back to the real desktop once every window is this close (px). */
const HANDOFF_EPSILON_PX = 0.75;
/**
 * World units between windows of the real stack. On the desktop the windows are
 * pulled towards the camera by their stacking rank (and shrunk to match, so
 * they still cover exactly their pixels): the depth test then lays them over
 * each other in their real order as they settle, instead of z-fighting.
 */
const STACK_DEPTH_STEP = 2;
/** The camera's vertical field of view: narrow, for Vista's long-lens look. */
const CAMERA_FOV_DEGREES = 12;
/** Cards making room after one closes, and the closed card fading out (ms). */
const RELAYOUT_MS = 300;
/** A card fading to or from its minimized look (ms). */
const CARD_FADE_MS = 200;

/** A window's placement in the scene (world units; z = 0 is the screen, 1:1). */
export interface Pose {
  x: number;
  y: number;
  z: number;
  rotateX: number;
  rotateY: number;
  scale: number;
  opacity: number;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** The output a switcher is open on, with its logical size. */
export interface Viewport {
  output: OutputInfo;
  width: number;
  height: number;
}

/** What a layout sees of the open switcher, and what it can do with it. */
export interface SwitcherControls {
  readonly output: string;
  readonly windows: readonly WaylandWindow[];
  /** The selected window's index, unbounded (taken modulo the window count). */
  readonly selected: number;
  /** Select window `index` (unbounded); ignored once closing. */
  select(index: number): void;
  /** Switch to the selected window. */
  commit(): void;
  /** Switch to `window`. */
  activate(window: WaylandWindow): void;
  /** Back to the desktop unchanged. */
  cancel(): void;
  /** The window drawn under a global logical point. */
  windowAt(globalX: number, globalY: number): WaylandWindow | undefined;
  /** The layout's own animation moved: draw frames until `tick` settles. */
  animate(): void;
}

/** Where a switcher puts the windows, and how input moves the selection. */
export interface SwitcherLayout<State> {
  /** Names the window textures. */
  readonly name: string;
  /** How far the desktop dims behind the windows. */
  readonly dimAlpha: number;
  /** Fresh per-session state. */
  begin(windows: readonly WaylandWindow[], output: string): State;
  /** The window selected on opening. */
  initialSelection(count: number): number;
  /**
   * Window `index`'s pose while the switcher is shown. Windows not on screen
   * before opening (or after closing) fade in from (out to) behind it.
   */
  pose(state: State, index: number, rect: Rect, viewport: Viewport, selected: number): Pose;
  /** Advance the layout's own animation by `dtMs`; true while it still moves. */
  tick?(state: State, selected: number, dtMs: number): boolean;
  /** Closing started: stop the layout's own animation where it is headed. */
  settle?(state: State, selected: number): void;
  /** Keys besides `Escape` / `Return` / `Space`; true when handled. */
  onKey?(state: State, event: InputGrabKeyEvent, controls: SwitcherControls): boolean;
  /** `Super` went up. */
  onSuperReleased(state: State, controls: SwitcherControls): void;
  onPointerMotion?(state: State, event: InputGrabPointerMotionEvent, controls: SwitcherControls): void;
  onScroll?(state: State, event: InputGrabScrollEvent, controls: SwitcherControls): void;
  onSwipe?(state: State, event: GestureSwipeEvent, controls: SwitcherControls): void;
  /** A left click that hit no window. */
  onClickOutside?(state: State, controls: SwitcherControls): void;
  /**
   * The windows act on clicks (title bar buttons, middle click) and stay in
   * the layout when minimized. Needs `windowsChanged`.
   */
  readonly cardActions?: boolean;
  /** How opaque a minimized window's card is (default 1). */
  readonly minimizedOpacity?: number;
  /** A window left `state.windows` (it closed): lay out the rest again. */
  windowsChanged?(state: State, controls: SwitcherControls): void;
}

/** A title bar button. */
export type TitlebarButton = "minimize" | "maximize" | "close";

/** A window switcher, whatever its layout. */
export interface WindowSwitcher {
  /** Open the switcher on `output` (no-op while open). */
  open(output: string): void;
  /** The switcher's composition for `output` while open, else `null`. */
  compose(output: OutputInfo): CompositionRenderable | null;
  /** True while window backdrop effects should be off. */
  readonly blurSuspended: ReadonlySignal<boolean>;
  /** Scale for window backdrop effects (0..1); fades them back in after closing. */
  readonly backdropStrength: ReadonlySignal<number>;
  /** The selected window while the switcher is open, else `null`. */
  readonly selectedWindowId: ReadonlySignal<string | null>;
}

export interface WindowSwitcherOptions {
  windowManager: HybridWindowManager;
  /** A window's place in the real stack, higher on top (its decoration's zIndex). */
  stackOrder: (window: WaylandWindow) => number;
  /** The title bar button at `(x, y)`: logical px from `window.rect`'s top-left corner. */
  titlebarButtonAt?: (window: WaylandWindow, x: number, y: number) => TitlebarButton | null;
}

/** A card whose window closed, fading out where it was. */
interface DepartingCard {
  window: WaylandWindow;
  rect: Rect;
  from: Pose;
}

type Phase = "entering" | "shown" | "leaving";

interface Session<State> {
  output: string;
  windows: WaylandWindow[];
  /** Where each window was on screen when the switcher opened (`null`: not shown). */
  startPoses: Map<string, Pose | null>;
  /** Each window's rank in the real stack (0 = bottom), once the switcher closes. */
  ranksAfter: Map<string, number>;
  /** The pose each window had when closing started. */
  leaveFrom: Map<string, Pose>;
  /** Windows on screen once the switcher closes. */
  shownAfter: Set<string>;
  phase: Phase;
  /** Linear progress of the current opening/closing, 0..1. */
  progress: number;
  phaseStartMs: number | null;
  /** The selected window's index, unbounded (taken modulo the window count). */
  selected: number;
  /** False after the selected card closed, until something selects a window. */
  hasSelection: boolean;
  lastTickMs: number | null;
  grab: InputGrab | null;
  poll: PollHandle | null;
  controls: SwitcherControls;
  state: State;
  /** Where each card moves from while the cards make room; progress 0..1. */
  moveFrom: Map<string, Pose>;
  moveProgress: number;
  departing: DepartingCard[];
  /** Each card's opacity factor (a minimized card is dimmed). */
  cardAlpha: Map<string, number>;
  /** Windows asked to close from a card that have not closed yet, oldest first. */
  closeRequested: string[];
  /** Stops listening for windows that open and close. */
  unlisten: (() => void) | null;
}

export function createWindowSwitcher<State>(
  layout: SwitcherLayout<State>,
  options: WindowSwitcherOptions,
): WindowSwitcher {
  const wm = options.windowManager;
  const [current, setCurrent] = signal<Session<State> | null>(null);
  // Bumped whenever the session's animated state changes, so the composition
  // re-evaluates once per frame while something moves.
  const [revision, setRevision] = signal(0);
  const [blurSuspended, setBlurSuspended] = signal(false);
  const [backdropStrength, setBackdropStrength] = signal(1);
  const [selectedWindowId, setSelectedWindowId] = signal<string | null>(null);
  let blurFade: PollHandle | null = null;

  const touch = () => setRevision(revision.peek() + 1);

  function viewportOf(name: string): Viewport | null {
    const output = COMPOSITOR.output.get(name);
    return output ? { output, ...outputLogicalSize(output) } : null;
  }

  /** Rank of each window in the real stack, bottom first. */
  function stackRanks(windows: WaylandWindow[]): Map<string, number> {
    const ordered = windows
      .map((window, recency) => ({ window, recency, order: options.stackOrder(window) }))
      // Ties: the more recently used window is on top.
      .sort((a, b) => a.order - b.order || b.recency - a.recency);
    return new Map(ordered.map(({ window }, rank) => [window.id, rank]));
  }

  /** Every window's pose for this frame. */
  function poses(session: Session<State>, viewport: Viewport) {
    const eased = WINDOW_MANAGEMENT_EASING(session.progress);
    const moved = WINDOW_MANAGEMENT_EASING(session.moveProgress);
    return session.windows.map((window, index) => {
      const rect = rectOf(window);
      const shown = layout.pose(session.state, index, rect, viewport, session.selected);
      let pose: Pose;
      if (session.phase === "leaving") {
        const from = session.leaveFrom.get(window.id) ?? shown;
        const target = session.shownAfter.has(window.id)
          ? realPose(rect, viewport, session.ranksAfter.get(window.id) ?? 0)
          : { ...from, z: from.z - viewport.height * 0.15, opacity: 0 };
        pose = lerpPose(from, target, eased);
      } else {
        const start = session.startPoses.get(window.id);
        const from = start ?? { ...shown, z: shown.z - viewport.height * 0.3, opacity: 0 };
        pose = session.phase === "entering" ? lerpPose(from, shown, eased) : shown;
        const movingFrom = session.moveFrom.get(window.id);
        if (movingFrom && session.phase === "shown") {
          pose = lerpPose(movingFrom, shown, moved);
        }
      }
      const alpha = session.cardAlpha.get(window.id) ?? 1;
      return { window, rect, pose: alpha === 1 ? pose : { ...pose, opacity: pose.opacity * alpha } };
    });
  }

  /** How opaque a card wants to be: dim when minimized. */
  function cardAlphaTarget(window: WaylandWindow): number {
    if (layout.minimizedOpacity !== undefined && window.state[WINDOW_STATE_MINIMIZED].peek()) {
      return layout.minimizedOpacity;
    }
    return 1;
  }

  /** Move the cards' opacities towards their targets; true while one moves. */
  function stepCardAlphas(session: Session<State>, dtMs: number): boolean {
    let moving = false;
    for (const window of session.windows) {
      const target = cardAlphaTarget(window);
      const now = session.cardAlpha.get(window.id) ?? target;
      const step = dtMs / CARD_FADE_MS;
      const next = now < target ? Math.min(target, now + step) : Math.max(target, now - step);
      session.cardAlpha.set(window.id, next);
      moving ||= next !== target;
    }
    return moving;
  }

  function dimAlpha(session: Session<State>): number {
    const eased = WINDOW_MANAGEMENT_EASING(session.progress);
    switch (session.phase) {
      case "entering":
        return layout.dimAlpha * eased;
      case "shown":
        return layout.dimAlpha;
      case "leaving":
        return layout.dimAlpha * (1 - eased);
    }
  }

  function ensureTicking(session: Session<State>) {
    if (session.poll) {
      return;
    }
    session.lastTickMs = null;
    session.poll = createPoll(1, (handle) => tick(session, handle.nowMs), {
      output: session.output,
      dirty: "none",
    });
  }

  function stopTicking(session: Session<State>) {
    session.poll?.cancel();
    session.poll = null;
  }

  function tick(session: Session<State>, nowMs: number) {
    if (current.peek() !== session) {
      stopTicking(session);
      return;
    }
    const dt = session.lastTickMs === null ? 0 : nowMs - session.lastTickMs;
    session.lastTickMs = nowMs;

    if (session.phase !== "shown") {
      session.phaseStartMs ??= nowMs;
      session.progress = Math.min(1, (nowMs - session.phaseStartMs) / TRANSITION_MS);
    }
    let moving = layout.tick?.(session.state, session.selected, dt) ?? false;
    if (session.moveProgress < 1) {
      session.moveProgress = Math.min(1, session.moveProgress + dt / RELAYOUT_MS);
      moving = true;
      if (session.moveProgress >= 1) {
        relayoutDone(session);
        if (current.peek() !== session) {
          return;
        }
      }
    }
    if (stepCardAlphas(session, dt)) {
      moving = true;
    }

    if (session.phase === "entering" && session.progress >= 1) {
      session.phase = "shown";
      session.progress = 1;
    }
    if (session.phase === "leaving" && readyToHandOff(session)) {
      finish(session);
      return;
    }
    touch();
    if (session.phase === "shown" && !moving) {
      stopTicking(session);
    }
  }

  /** Every window is (visually) where the real desktop will show it. */
  function readyToHandOff(session: Session<State>): boolean {
    if (session.progress >= 1) {
      return true;
    }
    const viewport = viewportOf(session.output);
    if (!viewport) {
      return true;
    }
    if (dimAlpha(session) > 0.01) {
      return false;
    }
    return poses(session, viewport).every(({ window, rect, pose }) => {
      if (!session.shownAfter.has(window.id)) {
        return pose.opacity < 0.02;
      }
      const target = realPose(rect, viewport, session.ranksAfter.get(window.id) ?? 0);
      const halfExtent = Math.max(rect.width, rect.height) / 2 + MARGIN;
      const deviation =
        Math.abs(pose.x - target.x) +
        Math.abs(pose.y - target.y) +
        Math.abs(pose.z - target.z) +
        (Math.abs(pose.rotateX) + Math.abs(pose.rotateY)) * (Math.PI / 180) * halfExtent +
        Math.abs(pose.scale - target.scale) * halfExtent;
      return deviation < HANDOFF_EPSILON_PX;
    });
  }

  function open(outputName: string) {
    if (current.peek()) {
      return;
    }
    const viewport = viewportOf(outputName);
    const windows = wm.listWindowsByRecentUse();
    if (!viewport || windows.length === 0) {
      return;
    }
    const ranks = stackRanks(windows);
    const startPoses = new Map<string, Pose | null>();
    for (const window of windows) {
      startPoses.set(
        window.id,
        wm.isWindowShownOn(window, outputName)
          ? realPose(rectOf(window), viewport, ranks.get(window.id) ?? 0)
          : null,
      );
    }
    const session: Session<State> = {
      output: outputName,
      windows,
      startPoses,
      ranksAfter: ranks,
      leaveFrom: new Map(),
      shownAfter: new Set(),
      phase: "entering",
      progress: 0,
      phaseStartMs: null,
      selected: layout.initialSelection(windows.length),
      hasSelection: true,
      lastTickMs: null,
      grab: null,
      poll: null,
      controls: undefined as unknown as SwitcherControls,
      state: layout.begin(windows, outputName),
      moveFrom: new Map(),
      moveProgress: 1,
      departing: [],
      cardAlpha: new Map(),
      closeRequested: [],
      unlisten: null,
    };
    for (const window of windows) {
      session.cardAlpha.set(window.id, cardAlphaTarget(window));
    }
    if (layout.cardActions && layout.windowsChanged) {
      const stopStart = COMPOSITOR.event.onStartClose((window) => removeCard(session, window));
      const stopClose = COMPOSITOR.event.onClose((window) => removeCard(session, window));
      const stopOpen = COMPOSITOR.event.onOpen((window) => windowOpened(session, window));
      session.unlisten = () => {
        stopStart();
        stopClose();
        stopOpen();
      };
    }
    session.controls = {
      output: outputName,
      windows,
      get selected() {
        return session.selected;
      },
      select: (index) => select(session, index),
      commit: () => commit(session),
      activate: (window) => close(session, window),
      cancel: () => close(session, null),
      windowAt: (x, y) => windowAt(session, x, y),
      animate: () => {
        if (current.peek() === session) {
          ensureTicking(session);
        }
      },
    };
    const { state, controls } = session;
    blurFade?.cancel();
    blurFade = null;
    setBackdropStrength(0);
    setBlurSuspended(true);
    session.grab = COMPOSITOR.input.grab({
      onKey: (event) => {
        if (event.state === "released") {
          if (event.key === "Super_L" || event.key === "Super_R") {
            layout.onSuperReleased(state, controls);
          }
          return;
        }
        if (layout.onKey?.(state, event, controls)) {
          return;
        }
        switch (event.key) {
          case "Return":
          case "KP_Enter":
          case "space":
            commit(session);
            break;
          case "Escape":
            close(session, null);
            break;
        }
      },
      onPointerMotion: (event) => layout.onPointerMotion?.(state, event, controls),
      onPointerButton: (event) => {
        if (event.state !== "pressed") {
          return;
        }
        const hit = cardAt(session, event.position.x, event.position.y);
        if (event.buttonName === "middle") {
          // KWin: a middle click closes the window.
          if (hit && layout.cardActions) {
            requestClose(session, hit.window);
          }
          return;
        }
        if (event.buttonName !== "left") {
          return;
        }
        // Only the selected card's title bar buttons work.
        if (hit && layout.cardActions && session.phase === "shown" && hit.window === selectedWindow(session)) {
          const button = options.titlebarButtonAt?.(hit.window, hit.x, hit.y) ?? null;
          if (button) {
            pressCardButton(session, hit.window, button);
            return;
          }
        }
        if (hit) {
          close(session, hit.window);
        } else {
          layout.onClickOutside?.(state, controls);
        }
      },
      onScroll: (event) => layout.onScroll?.(state, event, controls),
      onSwipe: (event) => layout.onSwipe?.(state, event, controls),
      onCancel: () => {
        // The screen locked or a handler failed: drop the switcher at once.
        session.grab = null;
        finish(session);
      },
    });
    setCurrent(session);
    setSelectedWindowId(selectedWindow(session)?.id ?? null);
    ensureTicking(session);
  }

  /** A title bar button of a card. */
  function pressCardButton(session: Session<State>, window: WaylandWindow, button: TitlebarButton) {
    switch (button) {
      case "close":
        // The card leaves once the window really closes (it may ask first).
        requestClose(session, window);
        return;
      case "maximize":
        if (!window.isResizable.peek()) {
          return;
        }
        close(session, window);
        if (window.isMaximized.peek()) {
          window.unmaximize();
        } else {
          window.maximize();
        }
        return;
      case "minimize":
        // The card stays (dimmed with `minimizedOpacity`); a click restores it.
        if (!window.state[WINDOW_STATE_MINIMIZED].peek()) {
          window.minimize();
          ensureTicking(session);
        }
        return;
    }
  }

  /** Ask a card's window to close; its card leaves when it really closes. */
  function requestClose(session: Session<State>, window: WaylandWindow) {
    if (!session.closeRequested.includes(window.id)) {
      session.closeRequested.push(window.id);
    }
    window.close();
  }

  /**
   * A window opened while a card's window was asked to close: most likely
   * its "save changes?" dialog, which the switcher would cover. Switch to
   * the window that asks, so the dialog shows.
   */
  function windowOpened(session: Session<State>, opened: WaylandWindow) {
    if (session.closeRequested.length === 0) {
      return;
    }
    const parentId = opened.parentId.peek();
    const ownerId =
      parentId !== undefined && session.closeRequested.includes(parentId)
        ? parentId
        : session.closeRequested[session.closeRequested.length - 1];
    close(session, session.windows.find((window) => window.id === ownerId) ?? null);
  }

  /** A window closed: its card fades out and the others make room. */
  function removeCard(session: Session<State>, window: WaylandWindow) {
    const index = session.windows.findIndex((entry) => entry.id === window.id);
    const viewport = viewportOf(session.output);
    if (current.peek() !== session || index < 0 || !viewport) {
      return;
    }
    session.moveFrom.clear();
    for (const placed of poses(session, viewport)) {
      if (placed.window === window) {
        session.departing.push({ window, rect: placed.rect, from: placed.pose });
      } else {
        session.moveFrom.set(placed.window.id, placed.pose);
      }
    }
    session.windows.splice(index, 1);
    session.cardAlpha.delete(window.id);
    session.closeRequested = session.closeRequested.filter((id) => id !== window.id);
    session.hasSelection = false;
    setSelectedWindowId(null);
    layout.windowsChanged?.(session.state, session.controls);
    session.moveProgress = 0;
    ensureTicking(session);
    touch();
  }

  /** The cards are in their new places: select the one under the pointer. */
  function relayoutDone(session: Session<State>) {
    session.moveFrom.clear();
    session.departing = [];
    if (session.windows.length === 0) {
      close(session, null);
      return;
    }
    if (session.hasSelection) {
      return; // A key or the pointer picked the next window already.
    }
    const cursor = COMPOSITOR.cursor.getPosition();
    const window = cursor ? windowAt(session, cursor.x, cursor.y) : undefined;
    if (window) {
      select(session, session.windows.indexOf(window));
    }
  }

  function select(session: Session<State>, index: number) {
    if (
      current.peek() !== session ||
      session.phase === "leaving" ||
      (session.hasSelection && index === session.selected)
    ) {
      return;
    }
    session.selected = index;
    session.hasSelection = true;
    setSelectedWindowId(selectedWindow(session)?.id ?? null);
    ensureTicking(session);
  }

  function selectedWindow(session: Session<State>): WaylandWindow | undefined {
    if (!session.hasSelection) {
      return undefined;
    }
    return session.windows[mod(session.selected, session.windows.length)];
  }

  function commit(session: Session<State>) {
    close(session, selectedWindow(session) ?? null);
  }

  /** Close the switcher, switching to `target` (`null`: back to the desktop as it was). */
  function close(session: Session<State>, target: WaylandWindow | null) {
    if (current.peek() !== session || session.phase === "leaving") {
      return;
    }
    const viewport = viewportOf(session.output);
    session.grab?.release();
    session.grab = null;
    if (!viewport) {
      finish(session);
      return;
    }
    // Freeze where everything is, then switch for real.
    for (const { window, pose } of poses(session, viewport)) {
      session.leaveFrom.set(window.id, pose);
    }
    layout.settle?.(session.state, session.selected);
    if (target) {
      wm.activateWindowById(target.id, { instant: true });
    }
    for (const window of session.windows) {
      if (wm.isWindowShownOn(window, session.output)) {
        session.shownAfter.add(window.id);
      }
    }
    session.ranksAfter = stackRanks(session.windows);
    if (target) {
      // Activation puts it on top; its focus may land a moment later.
      session.ranksAfter.set(target.id, session.windows.length);
    }
    session.phase = "leaving";
    setSelectedWindowId(null);
    session.progress = 0;
    session.phaseStartMs = null;
    ensureTicking(session);
    touch();
  }

  /** Back to the real desktop, and fade window blur in. */
  function finish(session: Session<State>) {
    stopTicking(session);
    session.grab?.release();
    session.grab = null;
    session.unlisten?.();
    session.unlisten = null;
    if (current.peek() !== session) {
      return;
    }
    setBackdropStrength(0);
    setBlurSuspended(false);
    setCurrent(null);
    setSelectedWindowId(null);
    let fadeStartMs: number | null = null;
    blurFade = createPoll(
      1,
      (handle) => {
        fadeStartMs ??= handle.nowMs;
        const linear = Math.min(1, (handle.nowMs - fadeStartMs) / BLUR_FADE_MS);
        setBackdropStrength(easeOutCubic(linear));
        if (linear >= 1) {
          handle.cancel();
          if (blurFade === handle) {
            blurFade = null;
          }
        }
      },
      { output: session.output, dirty: "none" },
    );
  }

  function planeAt(session: Session<State>, globalX: number, globalY: number) {
    const viewport = viewportOf(session.output);
    if (!viewport) {
      return undefined;
    }
    const { output, width, height } = viewport;
    const placed = visiblePlanes(session, viewport);
    const hit = pickPlane(
      switcherCamera(width, height),
      { width, height },
      placed.map(({ planeWidth, planeHeight, transform }) => ({
        width: planeWidth,
        height: planeHeight,
        transform,
      })),
      globalX - output.position.x,
      globalY - output.position.y,
    );
    return hit === null ? undefined : placed[hit];
  }

  function windowAt(session: Session<State>, globalX: number, globalY: number) {
    return planeAt(session, globalX, globalY)?.window;
  }

  /**
   * The card under a global point, with the point in its window's own
   * logical px (from the top-left corner of `window.rect`, unscaled).
   */
  function cardAt(session: Session<State>, globalX: number, globalY: number) {
    const placed = planeAt(session, globalX, globalY);
    const viewport = viewportOf(session.output);
    if (!placed || !viewport) {
      return undefined;
    }
    const { width, height, output } = viewport;
    const view = switcherCamera(width, height);
    const matrix = placed.transform.matrix;
    // The plane's corners on screen: top-left, top-right, bottom-left.
    const corner = (x: number, y: number) => {
      const world = [
        matrix[0] * x + matrix[4] * y + matrix[12],
        matrix[1] * x + matrix[5] * y + matrix[13],
        matrix[2] * x + matrix[6] * y + matrix[14],
      ] as [number, number, number];
      return projectPoint(view, { width, height }, world);
    };
    const halfWidth = placed.planeWidth / 2;
    const halfHeight = placed.planeHeight / 2;
    const origin = corner(-halfWidth, halfHeight);
    const right = corner(halfWidth, halfHeight);
    const down = corner(-halfWidth, -halfHeight);
    if (!origin || !right || !down) {
      return undefined;
    }
    // Solve point = origin + u * (right - origin) + v * (down - origin).
    const ax = right.x - origin.x;
    const ay = right.y - origin.y;
    const bx = down.x - origin.x;
    const by = down.y - origin.y;
    const px = globalX - output.position.x - origin.x;
    const py = globalY - output.position.y - origin.y;
    const det = ax * by - ay * bx;
    if (Math.abs(det) < 1e-6) {
      return undefined;
    }
    const u = (px * by - py * bx) / det;
    const v = (ax * py - ay * px) / det;
    return {
      window: placed.window,
      x: u * placed.planeWidth - MARGIN,
      y: v * placed.planeHeight - MARGIN,
    };
  }

  function visiblePlanes(session: Session<State>, viewport: Viewport) {
    const alive = new Set(wm.listWindows().map((window) => window.id));
    return poses(session, viewport)
      .filter(({ window, pose }) => alive.has(window.id) && pose.opacity > 0.001)
      .map(({ window, rect, pose }) => ({
        window,
        rect,
        pose,
        planeWidth: rect.width + MARGIN * 2,
        planeHeight: rect.height + MARGIN * 2,
        transform: transform3d()
          .translate(pose.x, pose.y, pose.z)
          .rotateX(pose.rotateX)
          .rotateY(pose.rotateY)
          .scale(pose.scale),
      }));
  }

  /** Cards whose window closed, fading out where they were. */
  function departingPlanes(session: Session<State>) {
    const eased = WINDOW_MANAGEMENT_EASING(session.moveProgress);
    return session.departing.map(({ window, rect, from }) => {
      const pose = lerpPose(from, { ...from, scale: from.scale * 0.85, opacity: 0 }, eased);
      return {
        window,
        rect,
        pose,
        planeWidth: rect.width + MARGIN * 2,
        planeHeight: rect.height + MARGIN * 2,
        transform: transform3d()
          .translate(pose.x, pose.y, pose.z)
          .rotateX(pose.rotateX)
          .rotateY(pose.rotateY)
          .scale(pose.scale),
      };
    });
  }

  function windowTexture(window: WaylandWindow, rect: Rect, output: OutputInfo) {
    return renderTexture({
      key: `${layout.name}-${window.id}`,
      width: rect.width + MARGIN * 2,
      height: rect.height + MARGIN * 2,
      content: (
        <Windows
          windows={[window]}
          offsetX={output.position.x - rect.x + MARGIN}
          offsetY={output.position.y - rect.y + MARGIN}
        />
      ),
    });
  }

  function compose(output: OutputInfo): CompositionRenderable | null {
    const session = current();
    if (!session || session.output !== output.name) {
      return null;
    }
    revision();
    const { width, height } = outputLogicalSize(output);
    const planes = [
      ...departingPlanes(session),
      ...visiblePlanes(session, { output, width, height }),
    ];
    return (
      <>
        <Layers layers={["background", "bottom"]} />
        <Solid color={[0, 0, 0, dimAlpha(session)]} />
        <Scene3D camera={switcherCamera(width, height)}>
          {planes.map(({ window, rect, pose, planeWidth, planeHeight, transform }) => (
            <Plane
              texture={windowTexture(window, rect, output)}
              width={planeWidth}
              height={planeHeight}
              transform={transform}
              opacity={pose.opacity}
            />
          ))}
        </Scene3D>
        <Layers layers={["top", "overlay"]} />
        <LayerPopups />
      </>
    );
  }

  return {
    open,
    compose,
    blurSuspended,
    backdropStrength,
    selectedWindowId,
  };
}

function rectOf(window: WaylandWindow): Rect {
  const rect = window.rect;
  return { x: rect.x, y: rect.y, width: rect.width, height: rect.height };
}

/** Distance at which the camera sees the plane z = 0 1:1 in logical pixels. */
export function cameraDistance(height: number): number {
  return height / 2 / Math.tan((CAMERA_FOV_DEGREES * Math.PI) / 360);
}

/**
 * Like `screenCamera` with a narrow field of view, but with room for a layout
 * that recedes several times the camera distance.
 */
function switcherCamera(width: number, height: number): Camera {
  const distance = cameraDistance(height);
  return {
    projection: perspective(CAMERA_FOV_DEGREES, width / Math.max(height, 1), distance / 100, distance * 6),
    view: lookAt([0, 0, distance], [0, 0, 0]),
  };
}

/**
 * A flat pose on the screen plane: the window centred on `(centerX, centerY)`
 * (logical px from the viewport's top-left corner), `scale` times its size.
 */
export function flatPose(
  centerX: number,
  centerY: number,
  scale: number,
  viewport: Viewport,
): Pose {
  return {
    x: centerX - viewport.width / 2,
    y: viewport.height / 2 - centerY,
    z: 0,
    rotateX: 0,
    rotateY: 0,
    scale,
    opacity: 1,
  };
}

/**
 * The window exactly where it is on screen. The camera maps z = 0 1:1; a
 * window `rank` steps up the stack sits that much nearer the camera, shrunk
 * by the perspective it gains so it covers the same pixels.
 */
function realPose(rect: Rect, viewport: Viewport, rank: number): Pose {
  const { output, width, height } = viewport;
  const centerX = rect.x - output.position.x + rect.width / 2;
  const centerY = rect.y - output.position.y + rect.height / 2;
  const distance = cameraDistance(height);
  const z = rank * STACK_DEPTH_STEP;
  const shrink = (distance - z) / distance;
  return {
    x: (centerX - width / 2) * shrink,
    y: (height / 2 - centerY) * shrink,
    z,
    rotateX: 0,
    rotateY: 0,
    scale: shrink,
    opacity: 1,
  };
}

function lerp(from: number, to: number, t: number): number {
  return from + (to - from) * t;
}

function lerpPose(from: Pose, to: Pose, t: number): Pose {
  return {
    x: lerp(from.x, to.x, t),
    y: lerp(from.y, to.y, t),
    z: lerp(from.z, to.z, t),
    rotateX: lerp(from.rotateX, to.rotateX, t),
    rotateY: lerp(from.rotateY, to.rotateY, t),
    scale: lerp(from.scale, to.scale, t),
    opacity: lerp(from.opacity, to.opacity, t),
  };
}

export function clamp01(value: number): number {
  return Math.min(1, Math.max(0, value));
}

export function mod(value: number, divisor: number): number {
  return ((value % divisor) + divisor) % divisor;
}
