interface RuntimeHooks {
  markRuntimeDirty(): void;
  markWindowDirty(windowId: string): void;
  markLayerDirty(layerId: string): void;
}

let hooks: RuntimeHooks | null = null;
let activeWindowDependencyScope: string | null = null;
let activeLayerDependencyScope: string | null = null;
const windowSignalDependencies = new WeakMap<object, Set<string>>();
const layerSignalDependencies = new WeakMap<object, Set<string>>();
const windowDependencies = new Map<string, Set<object>>();
const layerDependencies = new Map<string, Set<object>>();

export function installRuntimeHooks(nextHooks: RuntimeHooks | null): void {
  hooks = nextHooks;
}

export function markRuntimeDirty(): void {
  hooks?.markRuntimeDirty();
}

export function markWindowDirty(windowId: string): void {
  hooks?.markWindowDirty(windowId);
}

export function markLayerDirty(layerId: string): void {
  hooks?.markLayerDirty(layerId);
}

export function enterWindowDependencyScope(windowId: string): void {
  clearWindowDependencies(windowId);
  activeWindowDependencyScope = windowId;
  activeLayerDependencyScope = null;
}

export function leaveWindowDependencyScope(): void {
  activeWindowDependencyScope = null;
}

export function enterLayerDependencyScope(layerId: string): void {
  clearLayerDependencies(layerId);
  activeLayerDependencyScope = layerId;
  activeWindowDependencyScope = null;
}

export function leaveLayerDependencyScope(): void {
  activeLayerDependencyScope = null;
}

export function dropWindowDependencies(windowId: string): void {
  clearWindowDependencies(windowId);
}

export function dropLayerDependencies(layerId: string): void {
  clearLayerDependencies(layerId);
}

export function trackSignalRead(signal: object): void {
  const windowId = activeWindowDependencyScope;
  if (windowId) {
    let dependentWindows = windowSignalDependencies.get(signal);
    if (!dependentWindows) {
      dependentWindows = new Set<string>();
      windowSignalDependencies.set(signal, dependentWindows);
    }
    dependentWindows.add(windowId);

    let dependencies = windowDependencies.get(windowId);
    if (!dependencies) {
      dependencies = new Set<object>();
      windowDependencies.set(windowId, dependencies);
    }
    dependencies.add(signal);
    return;
  }

  const layerId = activeLayerDependencyScope;
  if (!layerId) {
    return;
  }

  let dependentLayers = layerSignalDependencies.get(signal);
  if (!dependentLayers) {
    dependentLayers = new Set<string>();
    layerSignalDependencies.set(signal, dependentLayers);
  }
  dependentLayers.add(layerId);

  let dependencies = layerDependencies.get(layerId);
  if (!dependencies) {
    dependencies = new Set<object>();
    layerDependencies.set(layerId, dependencies);
  }
  dependencies.add(signal);
}

export function trackSignalWrite(signal: object): void {
  const dependentWindows = windowSignalDependencies.get(signal);
  const dependentLayers = layerSignalDependencies.get(signal);
  const hasWindowDeps = !!dependentWindows && dependentWindows.size > 0;
  const hasLayerDeps = !!dependentLayers && dependentLayers.size > 0;
  if (!hasWindowDeps && !hasLayerDeps) {
    markRuntimeDirty();
    return;
  }

  if (dependentWindows) {
    for (const windowId of dependentWindows) {
      markWindowDirty(windowId);
    }
  }
  if (dependentLayers) {
    for (const layerId of dependentLayers) {
      markLayerDirty(layerId);
    }
  }
}

function clearWindowDependencies(windowId: string): void {
  const dependencies = windowDependencies.get(windowId);
  if (!dependencies) {
    return;
  }

  for (const signal of dependencies) {
    const dependentWindows = windowSignalDependencies.get(signal);
    dependentWindows?.delete(windowId);
  }

  windowDependencies.delete(windowId);
}

function clearLayerDependencies(layerId: string): void {
  const dependencies = layerDependencies.get(layerId);
  if (!dependencies) {
    return;
  }

  for (const signal of dependencies) {
    const dependentLayers = layerSignalDependencies.get(signal);
    dependentLayers?.delete(layerId);
  }

  layerDependencies.delete(layerId);
}
