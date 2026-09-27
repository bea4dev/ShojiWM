import type {
  WorkspaceActivateEvent,
  WorkspaceConfig,
  WorkspaceConfigureFactory,
  WorkspaceController,
  WorkspaceTransition,
} from "./types";

let desiredWorkspaceConfig: WorkspaceConfig = { groups: [] };
let pendingWorkspaceConfig = false;
let configureFactory: WorkspaceConfigureFactory | null = null;
let stagedConfigureFactory: WorkspaceConfigureFactory | null | undefined;
const activateListeners = new Set<(event: WorkspaceActivateEvent) => void>();
const pendingTransitions = new Map<string, WorkspaceTransition>();

function cloneWorkspaceConfig(config: WorkspaceConfig): WorkspaceConfig {
  return {
    groups: config.groups.map((group) => ({
      id: group.id,
      outputs: [...group.outputs],
      workspaces: group.workspaces.map((workspace) => ({
        id: workspace.id,
        name: workspace.name,
        coordinates: workspace.coordinates
          ? [...workspace.coordinates]
          : undefined,
        active: workspace.active,
        urgent: workspace.urgent,
        hidden: workspace.hidden,
      })),
    })),
  };
}

function normalizeWorkspaceConfig(config: WorkspaceConfig): WorkspaceConfig {
  return {
    groups: config.groups.map((group) => ({
      id: String(group.id),
      outputs: group.outputs.map(String),
      workspaces: group.workspaces.map((workspace) => ({
        id: String(workspace.id),
        name: String(workspace.name),
        coordinates: workspace.coordinates?.map((coordinate) =>
          Math.max(0, Math.trunc(coordinate)),
        ),
        active: workspace.active === true,
        urgent: workspace.urgent === true,
        hidden: workspace.hidden === true,
      })),
    })),
  };
}

function workspaceConfigsEqual(
  a: WorkspaceConfig,
  b: WorkspaceConfig,
): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function evaluateConfigureFactory(force = false): void {
  const factory = stagedConfigureFactory ?? configureFactory;
  if (!factory) {
    return;
  }
  const nextConfig = normalizeWorkspaceConfig(factory());
  if (!force && workspaceConfigsEqual(nextConfig, desiredWorkspaceConfig)) {
    return;
  }
  desiredWorkspaceConfig = nextConfig;
  pendingWorkspaceConfig = true;
}

export function configureWorkspaces(factory: WorkspaceConfigureFactory): void {
  if (stagedConfigureFactory !== undefined) {
    stagedConfigureFactory = factory;
    return;
  }
  configureFactory = factory;
  evaluateConfigureFactory(true);
}

export function reconfigureWorkspaces(): void {
  evaluateConfigureFactory(true);
}

export function resetWorkspaceConfiguration(): void {
  pendingTransitions.clear();
  if (stagedConfigureFactory !== undefined) {
    stagedConfigureFactory = null;
    return;
  }
  configureFactory = null;
  desiredWorkspaceConfig = { groups: [] };
  pendingWorkspaceConfig = false;
}

export function beginWorkspaceConfigurationRegistration(): void {
  stagedConfigureFactory = null;
}

export function commitWorkspaceConfigurationRegistration(): void {
  if (stagedConfigureFactory === undefined) {
    return;
  }
  configureFactory = stagedConfigureFactory;
  stagedConfigureFactory = undefined;
  evaluateConfigureFactory(true);
}

export function takePendingWorkspaceConfig(): WorkspaceConfig | undefined {
  if (!pendingWorkspaceConfig && pendingTransitions.size === 0) {
    return undefined;
  }
  pendingWorkspaceConfig = false;
  const config = cloneWorkspaceConfig(desiredWorkspaceConfig);
  config.transitions = Array.from(pendingTransitions, ([output, transition]) => ({ output, ...transition }));
  pendingTransitions.clear();
  return config;
}

export function emitWorkspaceActivate(event: WorkspaceActivateEvent): boolean {
  let invoked = false;
  for (const listener of Array.from(activateListeners)) {
    listener(event);
    invoked = true;
  }
  return invoked;
}

export const WORKSPACE_CONTROLLER: WorkspaceController = {
  transition(output, transition) {
    if (typeof output !== "string" || !output || typeof transition?.id !== "string"
      || !transition.id || !Number.isFinite(transition.progress)
      || !Number.isFinite(transition.direction) || !Array.isArray(transition.accent) || transition.accent.length !== 3
      || transition.accent.some(value => !Number.isFinite(value))) {
      throw new Error("Invalid workspace transition");
    }
    pendingTransitions.set(output, {
      id: transition.id,
      progress: Math.max(0, Math.min(1, transition.progress)),
      direction: transition.direction < 0 ? -1 : 1,
      accent: transition.accent.map(value => Math.max(0, Math.min(1, value))) as [number, number, number],
    });
  },
  configure(factory) {
    configureWorkspaces(factory);
  },
  reconfigure() {
    reconfigureWorkspaces();
  },
  event: {
    onActivate(listener) {
      activateListeners.add(listener);
      return () => activateListeners.delete(listener);
    },
  },
};
