import type {
  VirtualDesktopConfig,
  WorkspaceActivateEvent,
  WorkspaceConfig,
  WorkspaceConfigureFactory,
  WorkspaceController,
} from "./types";

let desiredWorkspaceConfig: WorkspaceConfig = { groups: [] };
let pendingWorkspaceConfig = false;
let configureFactory: WorkspaceConfigureFactory | null = null;
let stagedConfigureFactory: WorkspaceConfigureFactory | null | undefined;
const activateListeners = new Set<(event: WorkspaceActivateEvent) => void>();

function cloneVirtualDesktops(
  config: VirtualDesktopConfig | undefined,
): VirtualDesktopConfig | undefined {
  if (!config) {
    return undefined;
  }
  return {
    desktops: config.desktops.map((desktop) => ({
      id: String(desktop.id),
      name: String(desktop.name),
      active: desktop.active === true,
      windowIds: (desktop.windowIds ?? []).map(String),
    })),
    rows: Math.max(1, Math.trunc(config.rows ?? 1)),
    minimizedWindowIds: (config.minimizedWindowIds ?? []).map(String),
  };
}

function cloneWorkspaceConfig(config: WorkspaceConfig): WorkspaceConfig {
  return {
    virtualDesktops: cloneVirtualDesktops(config.virtualDesktops),
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
    virtualDesktops: cloneVirtualDesktops(config.virtualDesktops),
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
  if (!pendingWorkspaceConfig) {
    return undefined;
  }
  pendingWorkspaceConfig = false;
  return cloneWorkspaceConfig(desiredWorkspaceConfig);
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
