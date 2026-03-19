interface RuntimeHooks {
  markRuntimeDirty(): void;
  markWindowDirty(windowId: string): void;
}

let hooks: RuntimeHooks | null = null;
let activeWindowDependencyScope: string | null = null;
const signalDependencies = new WeakMap<object, Set<string>>();
const windowDependencies = new Map<string, Set<object>>();

export function installRuntimeHooks(nextHooks: RuntimeHooks | null): void {
  hooks = nextHooks;
}

export function markRuntimeDirty(): void {
  hooks?.markRuntimeDirty();
}

export function markWindowDirty(windowId: string): void {
  hooks?.markWindowDirty(windowId);
}

export function enterWindowDependencyScope(windowId: string): void {
  clearWindowDependencies(windowId);
  activeWindowDependencyScope = windowId;
}

export function leaveWindowDependencyScope(): void {
  activeWindowDependencyScope = null;
}

export function dropWindowDependencies(windowId: string): void {
  clearWindowDependencies(windowId);
}

export function trackSignalRead(signal: object): void {
  const windowId = activeWindowDependencyScope;
  if (!windowId) {
    return;
  }

  let dependentWindows = signalDependencies.get(signal);
  if (!dependentWindows) {
    dependentWindows = new Set<string>();
    signalDependencies.set(signal, dependentWindows);
  }
  dependentWindows.add(windowId);

  let dependencies = windowDependencies.get(windowId);
  if (!dependencies) {
    dependencies = new Set<object>();
    windowDependencies.set(windowId, dependencies);
  }
  dependencies.add(signal);
}

export function trackSignalWrite(signal: object): void {
  const dependentWindows = signalDependencies.get(signal);
  if (!dependentWindows || dependentWindows.size === 0) {
    markRuntimeDirty();
    return;
  }

  for (const windowId of dependentWindows) {
    markWindowDirty(windowId);
  }
}

function clearWindowDependencies(windowId: string): void {
  const dependencies = windowDependencies.get(windowId);
  if (!dependencies) {
    return;
  }

  for (const signal of dependencies) {
    const dependentWindows = signalDependencies.get(signal);
    dependentWindows?.delete(windowId);
  }

  windowDependencies.delete(windowId);
}
