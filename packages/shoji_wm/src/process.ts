import type {
  ManagedProcessReloadPolicy,
  ManagedProcessRestartPolicy,
  ProcessController,
  ProcessEnv,
  ProcessLaunchSpec,
  ProcessSpawnSpec,
  StartupOnceSpec,
  StartupProcessRunPolicy,
  StartupServiceSpec,
} from "./types";

interface RuntimeProcessConfigEntryCommon {
  id: string;
  cwd?: string;
  env?: ProcessEnv;
}

interface RuntimeOnceProcessConfigEntry extends RuntimeProcessConfigEntryCommon {
  kind: "once";
  runPolicy: StartupProcessRunPolicy;
  command?: string[];
  shell?: string;
}

interface RuntimeServiceProcessConfigEntry extends RuntimeProcessConfigEntryCommon {
  kind: "service";
  restart: ManagedProcessRestartPolicy;
  reload: ManagedProcessReloadPolicy;
  command?: string[];
  shell?: string;
}

type RuntimeProcessConfigEntry =
  | RuntimeOnceProcessConfigEntry
  | RuntimeServiceProcessConfigEntry;

interface RuntimeSpawnProcessAction {
  cwd?: string;
  env?: ProcessEnv;
  command?: string[];
  shell?: string;
}

let processBaseDir = "/";
let desiredProcessEntries = new Map<string, RuntimeProcessConfigEntry>();
let stagedProcessEntries: Map<string, RuntimeProcessConfigEntry> | null = null;
let pendingProcessConfig = false;
const pendingSpawnActions: RuntimeSpawnProcessAction[] = [];

function registrationTarget(): Map<string, RuntimeProcessConfigEntry> {
  return stagedProcessEntries ?? desiredProcessEntries;
}

function cloneEnv(env: ProcessEnv | undefined): ProcessEnv | undefined {
  if (!env) {
    return undefined;
  }
  return Object.fromEntries(Object.entries(env).map(([key, value]) => [key, String(value)]));
}

function cloneLaunch(spec: ProcessLaunchSpec): {
  command?: string[];
  shell?: string;
} {
  if ("command" in spec) {
    return {
      command: spec.command.map((part) => String(part)),
    };
  }

  return {
    shell: String(spec.shell),
  };
}

function normalizeCwd(cwd: string | undefined): string | undefined {
  if (!cwd) {
    return undefined;
  }
  return isAbsolutePath(cwd) ? cwd : resolvePath(processBaseDir, cwd);
}

function cloneConfigEntry(
  entry: RuntimeProcessConfigEntry,
): RuntimeProcessConfigEntry {
  return {
    ...entry,
    command: entry.command ? [...entry.command] : undefined,
    env: cloneEnv(entry.env),
  };
}

function normalizeOnceEntry(
  id: string,
  spec: StartupOnceSpec,
): RuntimeOnceProcessConfigEntry {
  return {
    id,
    kind: "once",
    runPolicy: spec.runPolicy ?? "once-per-session",
    cwd: normalizeCwd(spec.cwd),
    env: cloneEnv(spec.env),
    ...cloneLaunch(spec),
  };
}

function normalizeServiceEntry(
  id: string,
  spec: StartupServiceSpec,
): RuntimeServiceProcessConfigEntry {
  return {
    id,
    kind: "service",
    restart: spec.restart ?? "on-exit",
    reload: spec.reload ?? "keep-if-unchanged",
    cwd: normalizeCwd(spec.cwd),
    env: cloneEnv(spec.env),
    ...cloneLaunch(spec),
  };
}

function normalizeSpawnAction(
  spec: ProcessSpawnSpec,
): RuntimeSpawnProcessAction {
  return {
    cwd: normalizeCwd(spec.cwd),
    env: cloneEnv(spec.env),
    ...cloneLaunch(spec),
  };
}

export const PROCESS_CONTROLLER: ProcessController = {
  once(id, spec) {
    registrationTarget().set(id, normalizeOnceEntry(id, spec));
    if (!stagedProcessEntries) {
      pendingProcessConfig = true;
    }
  },
  service(id, spec) {
    registrationTarget().set(id, normalizeServiceEntry(id, spec));
    if (!stagedProcessEntries) {
      pendingProcessConfig = true;
    }
  },
  spawn(spec) {
    pendingSpawnActions.push(normalizeSpawnAction(spec));
  },
};

export function installProcessResolverBridge(configPath: string): void {
  processBaseDir = dirnamePath(resolvePath(processBaseDir, configPath));
}

export function beginProcessConfigRegistration(): void {
  stagedProcessEntries = new Map();
}

export function commitProcessConfigRegistration(): void {
  if (!stagedProcessEntries) {
    return;
  }

  desiredProcessEntries = stagedProcessEntries;
  stagedProcessEntries = null;
  pendingProcessConfig = true;
}

export function takePendingProcessConfig():
  | RuntimeProcessConfigEntry[]
  | undefined {
  if (!pendingProcessConfig) {
    return undefined;
  }

  pendingProcessConfig = false;
  return Array.from(desiredProcessEntries.values())
    .sort((left, right) => left.id.localeCompare(right.id))
    .map(cloneConfigEntry);
}

export function drainPendingProcessActions(): RuntimeSpawnProcessAction[] {
  return pendingSpawnActions.splice(0, pendingSpawnActions.length).map((action) => ({
    ...action,
    command: action.command ? [...action.command] : undefined,
    env: cloneEnv(action.env),
  }));
}

function isAbsolutePath(path: string): boolean {
  return path.startsWith("/");
}

function dirnamePath(path: string): string {
  const normalized = normalizePath(path);
  if (normalized === "/") {
    return "/";
  }
  const index = normalized.lastIndexOf("/");
  return index <= 0 ? "/" : normalized.slice(0, index);
}

function resolvePath(...paths: string[]): string {
  return normalizePath(paths.filter(Boolean).join("/"));
}

function normalizePath(path: string): string {
  const absolute = path.startsWith("/");
  const parts = path.split("/").filter((part) => part.length > 0 && part !== ".");
  const stack: string[] = [];

  for (const part of parts) {
    if (part === "..") {
      if (stack.length > 0) {
        stack.pop();
      }
      continue;
    }
    stack.push(part);
  }

  const joined = stack.join("/");
  if (absolute) {
    return joined ? `/${joined}` : "/";
  }
  return joined || ".";
}
