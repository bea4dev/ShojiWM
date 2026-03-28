import type {
  DisplayConfigDraft,
  OutputConfigEntry,
  OutputController,
  OutputStateSnapshot,
} from "./types";

let currentOutputState: Record<string, OutputStateSnapshot> = {};
let desiredOutputConfig: DisplayConfigDraft = {};
let pendingDisplayConfig = false;
const deferredDisplayConfigMutators: Array<
  (display: DisplayConfigDraft) => void
> = [];

function cloneOutputState(
  state: Record<string, OutputStateSnapshot>,
): Record<string, OutputStateSnapshot> {
  return Object.fromEntries(
    Object.entries(state).map(([name, snapshot]) => [
      name,
      {
        resolution: snapshot.resolution
          ? { ...snapshot.resolution }
          : undefined,
        position: { ...snapshot.position },
        scale: snapshot.scale,
        availableModes: snapshot.availableModes.map((mode) => ({ ...mode })),
      },
    ]),
  );
}

function cloneDisplayDraft(draft: DisplayConfigDraft): DisplayConfigDraft {
  return Object.fromEntries(
    Object.entries(draft).map(([name, config]) => [
      name,
      config
        ? {
            resolution:
              typeof config.resolution === "string"
                ? config.resolution
                : config.resolution
                  ? { ...config.resolution }
                  : undefined,
            position:
              typeof config.position === "string"
                ? config.position
                : config.position
                  ? { ...config.position }
                  : undefined,
            scale: config.scale,
          }
        : null,
    ]),
  );
}

function normalizeOutputConfig(
  draft: DisplayConfigDraft,
): DisplayConfigDraft {
  const normalized: DisplayConfigDraft = {};
  for (const [name, config] of Object.entries(draft)) {
    if (config == null) {
      normalized[name] = null;
      continue;
    }
    normalized[name] = {
      resolution: config.resolution,
      position: config.position,
      scale: config.scale,
    };
  }
  return normalized;
}

export function updateOutputState(
  nextState: Record<string, OutputStateSnapshot>,
): void {
  currentOutputState = cloneOutputState(nextState);
  if (Object.keys(currentOutputState).length > 0 && deferredDisplayConfigMutators.length > 0) {
    const draft = cloneDisplayDraft(desiredOutputConfig);
    for (const mutator of deferredDisplayConfigMutators.splice(0, deferredDisplayConfigMutators.length)) {
      mutator(draft);
    }
    desiredOutputConfig = normalizeOutputConfig(draft);
    pendingDisplayConfig = true;
  }
}

export function takePendingDisplayConfig():
  | DisplayConfigDraft
  | undefined {
  if (!pendingDisplayConfig) {
    return undefined;
  }
  pendingDisplayConfig = false;
  return cloneDisplayDraft(desiredOutputConfig);
}

export const OUTPUT_CONTROLLER: OutputController = {
  get list() {
    return Object.keys(currentOutputState);
  },
  get current() {
    return cloneOutputState(currentOutputState);
  },
  availableModes(outputName) {
    return currentOutputState[outputName]?.availableModes.map((mode) => ({ ...mode })) ?? [];
  },
  applyDisplayConfig(mutator) {
    if (Object.keys(currentOutputState).length === 0) {
      deferredDisplayConfigMutators.push(mutator);
      return;
    }
    const draft = cloneDisplayDraft(desiredOutputConfig);
    mutator(draft);
    desiredOutputConfig = normalizeOutputConfig(draft);
    pendingDisplayConfig = true;
  },
};

export function getDesiredOutputConfig(): DisplayConfigDraft {
  return cloneDisplayDraft(desiredOutputConfig);
}

export function replaceDesiredOutputConfig(
  nextConfig: DisplayConfigDraft,
): void {
  desiredOutputConfig = normalizeOutputConfig(nextConfig);
  pendingDisplayConfig = true;
}
