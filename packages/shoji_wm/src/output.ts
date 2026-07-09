import type {
  DisplayConfigDraft,
  OutputConfigEntry,
  OutputConfigureContext,
  OutputConfigureFactory,
  OutputController,
  OutputInfo,
  OutputStateSnapshot,
} from "./types";
import type { OutputChangeEvent } from "./events";

let currentOutputState: Record<string, OutputInfo> = {};
let desiredOutputConfig: DisplayConfigDraft = {};
let pendingDisplayConfig = false;
let configureFactory: OutputConfigureFactory | null = null;
let stagedConfigureFactory: OutputConfigureFactory | null | undefined;
let outputChangeEmitter: ((event: OutputChangeEvent) => void) | null = null;

function cloneOutputState(
  state: Record<string, OutputInfo>,
): Record<string, OutputInfo> {
  return Object.fromEntries(
    Object.entries(state).map(([name, snapshot]) => [
      name,
      {
        name: snapshot.name,
        description: snapshot.description,
        make: snapshot.make,
        model: snapshot.model,
        serial: snapshot.serial,
        connector: snapshot.connector,
        enabled: snapshot.enabled,
        resolution: snapshot.resolution
          ? { ...snapshot.resolution }
          : undefined,
        position: { ...snapshot.position },
        scale: snapshot.scale,
        transform: snapshot.transform,
        availableModes: snapshot.availableModes.map((mode) => ({ ...mode })),
        hdrSupported: snapshot.hdrSupported,
      },
    ]),
  );
}

function cloneOutputInfo(output: OutputInfo): OutputInfo {
  return cloneOutputState({ [output.name]: output })[output.name]!;
}

function normalizeOutputState(
  state: Record<string, OutputStateSnapshot>,
): Record<string, OutputInfo> {
  return Object.fromEntries(
    Object.entries(state).map(([name, snapshot]) => [
      name,
      {
        name: snapshot.name ?? name,
        description: snapshot.description,
        make: snapshot.make,
        model: snapshot.model,
        serial: snapshot.serial,
        connector: snapshot.connector,
        enabled: snapshot.enabled ?? true,
        resolution: snapshot.resolution
          ? { ...snapshot.resolution }
          : undefined,
        position: { ...snapshot.position },
        scale: snapshot.scale,
        transform: snapshot.transform ?? "normal",
        availableModes: snapshot.availableModes.map((mode) => ({ ...mode })),
        hdrSupported: snapshot.hdrSupported,
      },
    ]),
  );
}

function cloneDisplayDraft(draft: DisplayConfigDraft): DisplayConfigDraft {
  return Object.fromEntries(
    Object.entries(draft).map(([name, config]) => [
      name,
      config ? cloneOutputConfigEntry(config) : null,
    ]),
  );
}

function cloneOutputConfigEntry(config: OutputConfigEntry): OutputConfigEntry {
  if (config.mode === "disabled") {
    return { mode: "disabled" };
  }
  if (config.mode === "mirror") {
    return { mode: "mirror", source: config.source };
  }
  return {
    mode: "extend",
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
    transform: config.transform,
    hdr: config.hdr,
  };
}

function normalizeOutputConfig(draft: DisplayConfigDraft): DisplayConfigDraft {
  const normalized: DisplayConfigDraft = {};
  for (const [name, config] of Object.entries(draft)) {
    if (config == null) {
      normalized[name] = null;
      continue;
    }
    normalized[name] = cloneOutputConfigEntry(config);
  }
  return normalized;
}

function outputStatesEqual(
  a: Record<string, OutputInfo>,
  b: Record<string, OutputInfo>,
): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function displayDraftsEqual(
  a: DisplayConfigDraft,
  b: DisplayConfigDraft,
): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function configureContext(): OutputConfigureContext {
  const current = cloneOutputState(currentOutputState);
  const connected = Object.values(current);
  return {
    connected,
    outputs: connected.filter((output) => output.enabled),
    current,
  };
}

function evaluateConfigureFactory(force = false): void {
  const factory = stagedConfigureFactory ?? configureFactory;
  if (!factory || Object.keys(currentOutputState).length === 0) {
    return;
  }
  const nextConfig = normalizeOutputConfig(factory(configureContext()));
  if (!force && displayDraftsEqual(nextConfig, desiredOutputConfig)) {
    return;
  }
  desiredOutputConfig = nextConfig;
  pendingDisplayConfig = true;
}

function outputChangeEvent(
  previous: Record<string, OutputInfo>,
  current: Record<string, OutputInfo>,
): OutputChangeEvent {
  const previousNames = new Set(Object.keys(previous));
  const currentNames = new Set(Object.keys(current));
  const added = Object.values(current)
    .filter((output) => !previousNames.has(output.name))
    .map(cloneOutputInfo);
  const removed = Object.values(previous)
    .filter((output) => !currentNames.has(output.name))
    .map(cloneOutputInfo);
  const changed = Object.values(current)
    .filter((output) => {
      const before = previous[output.name];
      return (
        before &&
        !outputStatesEqual({ [output.name]: before }, { [output.name]: output })
      );
    })
    .map(cloneOutputInfo);
  return {
    outputs: Object.values(cloneOutputState(current)),
    current: cloneOutputState(current),
    added,
    removed,
    changed,
  };
}

export function updateOutputState(
  nextState: Record<string, OutputStateSnapshot>,
): void {
  const normalized = normalizeOutputState(nextState);
  if (outputStatesEqual(currentOutputState, normalized)) {
    return;
  }
  const previous = currentOutputState;
  currentOutputState = normalized;
  evaluateConfigureFactory();
  outputChangeEmitter?.(outputChangeEvent(previous, currentOutputState));
}

export function installOutputChangeEmitter(
  emitter: (event: OutputChangeEvent) => void,
): void {
  outputChangeEmitter = emitter;
}

export function clearOutputChangeEmitter(): void {
  outputChangeEmitter = null;
}

export function reconfigureOutputs(): void {
  evaluateConfigureFactory(true);
}

export function configureOutputs(factory: OutputConfigureFactory): void {
  if (stagedConfigureFactory !== undefined) {
    stagedConfigureFactory = factory;
    return;
  }
  configureFactory = factory;
  evaluateConfigureFactory(true);
}

export function resetOutputConfiguration(): void {
  if (stagedConfigureFactory !== undefined) {
    stagedConfigureFactory = null;
    return;
  }
  configureFactory = null;
  desiredOutputConfig = {};
  pendingDisplayConfig = false;
}

export function hasOutputConfiguration(): boolean {
  return configureFactory !== null;
}

export function beginOutputConfigurationRegistration(): void {
  stagedConfigureFactory = null;
}

export function commitOutputConfigurationRegistration(): void {
  if (stagedConfigureFactory === undefined) {
    return;
  }
  configureFactory = stagedConfigureFactory;
  stagedConfigureFactory = undefined;
  evaluateConfigureFactory(true);
}

export function outputConfigureContext(): OutputConfigureContext {
  return configureContext();
}

export function currentOutputList(): OutputInfo[] {
  return Object.values(cloneOutputState(currentOutputState));
}

export function currentOutputRecord(): Record<string, OutputInfo> {
  return cloneOutputState(currentOutputState);
}

export function outputByName(outputName: string): OutputInfo | undefined {
  const output = currentOutputState[outputName];
  return output ? cloneOutputInfo(output) : undefined;
}

export function findOutput(
  predicate: (output: OutputInfo) => boolean,
): OutputInfo | undefined {
  for (const output of Object.values(currentOutputState)) {
    if (predicate(cloneOutputInfo(output))) {
      return cloneOutputInfo(output);
    }
  }
  return undefined;
}

export function takePendingDisplayConfig(): DisplayConfigDraft | undefined {
  if (!pendingDisplayConfig) {
    return undefined;
  }
  pendingDisplayConfig = false;
  return cloneDisplayDraft(desiredOutputConfig);
}

export const OUTPUT_CONTROLLER: OutputController = {
  get list() {
    return Object.values(currentOutputState)
      .filter((output) => output.enabled)
      .map((output) => output.name);
  },
  get outputs() {
    return currentOutputList().filter((output) => output.enabled);
  },
  get current() {
    return currentOutputRecord();
  },
  get(outputName) {
    return outputByName(outputName);
  },
  find(predicate) {
    return findOutput(predicate);
  },
  availableModes(outputName) {
    return (
      currentOutputState[outputName]?.availableModes.map((mode) => ({
        ...mode,
      })) ?? []
    );
  },
  configure(factory) {
    configureOutputs(factory);
  },
  reconfigure() {
    reconfigureOutputs();
  },
};

/**
 * Internal: peek at the currently-stored snapshot for `outputName` without
 * cloning. Callers MUST treat the result as read-only — mutating it would
 * corrupt `OUTPUT_CONTROLLER.current`.
 */
export function peekOutputState(outputName: string): OutputInfo | undefined {
  return currentOutputState[outputName];
}

export function getDesiredOutputConfig(): DisplayConfigDraft {
  return cloneDisplayDraft(desiredOutputConfig);
}

export function replaceDesiredOutputConfig(
  nextConfig: DisplayConfigDraft,
): void {
  desiredOutputConfig = normalizeOutputConfig(nextConfig);
  pendingDisplayConfig = true;
}
