interface RuntimeHooks {
  markRuntimeDirty(): void;
  markWindowDirty(windowId: string): void;
  markLayerDirty(layerId: string): void;
}

let hooks: RuntimeHooks | null = null;
let activeWindowDependencyScope: string | null = null;
let activeLayerDependencyScope: string | null = null;
let activeWindowNodeDependencyScope: string | null = null;
let activeLayerNodeDependencyScope: string | null = null;
const windowSignalDependencies = new WeakMap<object, Set<string>>();
const layerSignalDependencies = new WeakMap<object, Set<string>>();
const windowNodeSignalDependencies = new WeakMap<object, Map<string, Set<string>>>();
const layerNodeSignalDependencies = new WeakMap<object, Map<string, Set<string>>>();
const windowDependencies = new Map<string, Set<object>>();
const layerDependencies = new Map<string, Set<object>>();
const windowNodeDependencies = new Map<string, Map<string, Set<object>>>();
const layerNodeDependencies = new Map<string, Map<string, Set<object>>>();
const dirtyWindowNodeIds = new Map<string, Set<string>>();
const dirtyLayerNodeIds = new Map<string, Set<string>>();

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
  activeWindowNodeDependencyScope = null;
  activeLayerDependencyScope = null;
  activeLayerNodeDependencyScope = null;
}

export function leaveWindowDependencyScope(): void {
  activeWindowDependencyScope = null;
  activeWindowNodeDependencyScope = null;
}

export function enterLayerDependencyScope(layerId: string): void {
  clearLayerDependencies(layerId);
  activeLayerDependencyScope = layerId;
  activeLayerNodeDependencyScope = null;
  activeWindowDependencyScope = null;
  activeWindowNodeDependencyScope = null;
}

export function leaveLayerDependencyScope(): void {
  activeLayerDependencyScope = null;
  activeLayerNodeDependencyScope = null;
}

export function enterWindowNodeDependencyScope(nodeId: string): void {
  activeWindowNodeDependencyScope =
    activeWindowDependencyScope ? nodeId : null;
  activeLayerNodeDependencyScope = null;
}

export function leaveWindowNodeDependencyScope(): void {
  activeWindowNodeDependencyScope = null;
}

export function enterLayerNodeDependencyScope(nodeId: string): void {
  activeLayerNodeDependencyScope =
    activeLayerDependencyScope ? nodeId : null;
  activeWindowNodeDependencyScope = null;
}

export function leaveLayerNodeDependencyScope(): void {
  activeLayerNodeDependencyScope = null;
}

export function dropWindowDependencies(windowId: string): void {
  clearWindowDependencies(windowId);
}

export function dropLayerDependencies(layerId: string): void {
  clearLayerDependencies(layerId);
}

export function takeDirtyWindowNodeIds(windowId: string): string[] {
  const dirty = dirtyWindowNodeIds.get(windowId);
  if (!dirty) {
    return [];
  }
  dirtyWindowNodeIds.delete(windowId);
  return Array.from(dirty);
}

export function takeDirtyLayerNodeIds(layerId: string): string[] {
  const dirty = dirtyLayerNodeIds.get(layerId);
  if (!dirty) {
    return [];
  }
  dirtyLayerNodeIds.delete(layerId);
  return Array.from(dirty);
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

    const nodeId = activeWindowNodeDependencyScope;
    if (nodeId) {
      let dependentNodesByWindow = windowNodeSignalDependencies.get(signal);
      if (!dependentNodesByWindow) {
        dependentNodesByWindow = new Map<string, Set<string>>();
        windowNodeSignalDependencies.set(signal, dependentNodesByWindow);
      }
      let dependentNodes = dependentNodesByWindow.get(windowId);
      if (!dependentNodes) {
        dependentNodes = new Set<string>();
        dependentNodesByWindow.set(windowId, dependentNodes);
      }
      dependentNodes.add(nodeId);

      let nodeDependenciesByWindow = windowNodeDependencies.get(windowId);
      if (!nodeDependenciesByWindow) {
        nodeDependenciesByWindow = new Map<string, Set<object>>();
        windowNodeDependencies.set(windowId, nodeDependenciesByWindow);
      }
      let nodeDependencies = nodeDependenciesByWindow.get(nodeId);
      if (!nodeDependencies) {
        nodeDependencies = new Set<object>();
        nodeDependenciesByWindow.set(nodeId, nodeDependencies);
      }
      nodeDependencies.add(signal);
    }
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

  const nodeId = activeLayerNodeDependencyScope;
  if (!nodeId) {
    return;
  }

  let dependentNodesByLayer = layerNodeSignalDependencies.get(signal);
  if (!dependentNodesByLayer) {
    dependentNodesByLayer = new Map<string, Set<string>>();
    layerNodeSignalDependencies.set(signal, dependentNodesByLayer);
  }
  let dependentNodes = dependentNodesByLayer.get(layerId);
  if (!dependentNodes) {
    dependentNodes = new Set<string>();
    dependentNodesByLayer.set(layerId, dependentNodes);
  }
  dependentNodes.add(nodeId);

  let nodeDependenciesByLayer = layerNodeDependencies.get(layerId);
  if (!nodeDependenciesByLayer) {
    nodeDependenciesByLayer = new Map<string, Set<object>>();
    layerNodeDependencies.set(layerId, nodeDependenciesByLayer);
  }
  let nodeDependencies = nodeDependenciesByLayer.get(nodeId);
  if (!nodeDependencies) {
    nodeDependencies = new Set<object>();
    nodeDependenciesByLayer.set(nodeId, nodeDependencies);
  }
  nodeDependencies.add(signal);
}

export function trackSignalWrite(signal: object): void {
  const dependentWindows = windowSignalDependencies.get(signal);
  const dependentLayers = layerSignalDependencies.get(signal);
  const dependentWindowNodes = windowNodeSignalDependencies.get(signal);
  const dependentLayerNodes = layerNodeSignalDependencies.get(signal);
  const hasWindowDeps = !!dependentWindows && dependentWindows.size > 0;
  const hasLayerDeps = !!dependentLayers && dependentLayers.size > 0;
  const hasWindowNodeDeps = !!dependentWindowNodes && dependentWindowNodes.size > 0;
  const hasLayerNodeDeps = !!dependentLayerNodes && dependentLayerNodes.size > 0;
  if (!hasWindowDeps && !hasLayerDeps && !hasWindowNodeDeps && !hasLayerNodeDeps) {
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
  if (dependentWindowNodes) {
    for (const [windowId, nodeIds] of dependentWindowNodes) {
      let dirtyNodes = dirtyWindowNodeIds.get(windowId);
      if (!dirtyNodes) {
        dirtyNodes = new Set<string>();
        dirtyWindowNodeIds.set(windowId, dirtyNodes);
      }
      for (const nodeId of nodeIds) {
        dirtyNodes.add(nodeId);
      }
    }
  }
  if (dependentLayerNodes) {
    for (const [layerId, nodeIds] of dependentLayerNodes) {
      let dirtyNodes = dirtyLayerNodeIds.get(layerId);
      if (!dirtyNodes) {
        dirtyNodes = new Set<string>();
        dirtyLayerNodeIds.set(layerId, dirtyNodes);
      }
      for (const nodeId of nodeIds) {
        dirtyNodes.add(nodeId);
      }
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
  dirtyWindowNodeIds.delete(windowId);

  const nodeDependenciesByWindow = windowNodeDependencies.get(windowId);
  if (nodeDependenciesByWindow) {
    for (const [nodeId, nodeDependencies] of nodeDependenciesByWindow) {
      for (const signal of nodeDependencies) {
        const dependentNodesByWindow = windowNodeSignalDependencies.get(signal);
        dependentNodesByWindow?.get(windowId)?.delete(nodeId);
        if (dependentNodesByWindow?.get(windowId)?.size === 0) {
          dependentNodesByWindow.delete(windowId);
        }
      }
    }
    windowNodeDependencies.delete(windowId);
  }
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
  dirtyLayerNodeIds.delete(layerId);

  const nodeDependenciesByLayer = layerNodeDependencies.get(layerId);
  if (nodeDependenciesByLayer) {
    for (const [nodeId, nodeDependencies] of nodeDependenciesByLayer) {
      for (const signal of nodeDependencies) {
        const dependentNodesByLayer = layerNodeSignalDependencies.get(signal);
        dependentNodesByLayer?.get(layerId)?.delete(nodeId);
        if (dependentNodesByLayer?.get(layerId)?.size === 0) {
          dependentNodesByLayer.delete(layerId);
        }
      }
    }
    layerNodeDependencies.delete(layerId);
  }
}
