import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

import {
  createComponentStateStore,
  createReactiveWindow,
  installShaderResolverBridge,
  serializeDecorationTree,
  withComponentRenderRoot,
  type DecorationSerializationContext,
  type DecorationFunction,
  type WaylandWindowActions,
  type WaylandWindowSnapshot,
} from "shoji_wm";

const DEFAULT_SNAPSHOT: WaylandWindowSnapshot = {
  id: "demo-window-1",
  title: "Kitty",
  appId: "kitty",
  position: {
    x: 100,
    y: 80,
    width: 900,
    height: 600,
  },
  isFocused: true,
  isFloating: true,
  isMaximized: false,
  isFullscreen: false,
  isXwayland: false,
  icon: undefined,
  interaction: {
    hoveredIds: [],
    activeIds: [],
  },
};

async function main() {
  const configPath = process.argv[2];
  if (!configPath) {
    throw new Error("usage: npm run ssd:eval -- <config-path> [snapshot-json]");
  }

  const snapshot = process.argv[3]
    ? (JSON.parse(process.argv[3]) as WaylandWindowSnapshot)
    : DEFAULT_SNAPSHOT;

  const moduleUrl = pathToFileURL(resolve(configPath)).href;
  installShaderResolverBridge(resolve(configPath));
  const loaded = await import(moduleUrl);
  const decoration = resolveDecoration(loaded);

  const actions: WaylandWindowActions = {
    close() {
      console.log("[runtime] close() requested");
    },
    maximize() {
      console.log("[runtime] maximize() requested");
    },
    minimize() {
      console.log("[runtime] minimize() requested");
    },
    setCloseAnimationDuration(durationMs) {
      console.log(`[runtime] setCloseAnimationDuration(${durationMs}) requested`);
    },
    isXWayland() {
      return snapshot.isXwayland;
    },
  };

  const handle = createReactiveWindow(snapshot, actions);
  const componentStateStore = createComponentStateStore();
  const tree = withComponentRenderRoot(snapshot.id, componentStateStore, () =>
    decoration(handle.window)
  );
  let nextHandlerId = 1;
  const context: DecorationSerializationContext = {
    registerClickHandler(key) {
      return `${key}-eval-${nextHandlerId++}`;
    },
  };
  const serialized = serializeDecorationTree(tree, context);

  console.log(JSON.stringify(serialized, null, 2));
}

function resolveDecoration(
  loaded: Record<string, unknown>,
): DecorationFunction {
  const maybeDecoration =
    (loaded.WINDOW_MANAGER as { decoration?: DecorationFunction } | undefined)
      ?.decoration ??
    (loaded.default as { decoration?: DecorationFunction } | undefined)?.decoration ??
    (loaded.decoration as DecorationFunction | undefined);

  if (!maybeDecoration) {
    throw new Error(
      "config module did not export WINDOW_MANAGER.decoration",
    );
  }

  return maybeDecoration;
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
