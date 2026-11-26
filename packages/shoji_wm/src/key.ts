import type {
  KeyBindingController,
  KeyBindingEventPhase,
  KeyBindingOptions,
} from "./types";

interface RuntimeKeyBindingConfigEntry {
  id: string;
  shortcut: string;
  on: KeyBindingEventPhase;
  within?: number;
  locked?: boolean;
  repeat?: boolean;
  transparent?: boolean;
  non_consuming?: boolean;
}

type KeyBindingHandler = () => void;

interface KeyBridge {
  isKeyRepeat(): boolean;
}

let bridge: KeyBridge | undefined;

export function installKeyBridge(value: KeyBridge): void { bridge = value; }

let desiredKeyBindingEntries = new Map<string, RuntimeKeyBindingConfigEntry>();
let desiredKeyBindingHandlers = new Map<string, KeyBindingHandler>();
let stagedKeyBindingEntries: Map<string, RuntimeKeyBindingConfigEntry> | null = null;
let stagedKeyBindingHandlers: Map<string, KeyBindingHandler> | null = null;
let pendingKeyBindingConfig = false;

function registrationEntriesTarget(): Map<string, RuntimeKeyBindingConfigEntry> {
  return stagedKeyBindingEntries ?? desiredKeyBindingEntries;
}

function registrationHandlersTarget(): Map<string, KeyBindingHandler> {
  return stagedKeyBindingHandlers ?? desiredKeyBindingHandlers;
}

function normalizeShortcut(shortcut: string): string {
  const parts = shortcut
    .split("+")
    .map((part) => part.trim())
    .filter((part) => part.length > 0);

  if (parts.length === 0) {
    throw new Error("key binding shortcut must not be empty");
  }

  return parts.join("+");
}

function normalizePhase(options: KeyBindingOptions | undefined): KeyBindingEventPhase {
  return options?.on ?? "press";
}

function normalizeWithin(within: number): number {
  if (!Number.isFinite(within) || within < 0) {
    throw new Error("key binding `within` must be a non-negative number of milliseconds");
  }
  return Math.round(within);
}

function cloneConfigEntry(
  entry: RuntimeKeyBindingConfigEntry,
): RuntimeKeyBindingConfigEntry {
  return { ...entry };
}

export const KEY_BINDING_CONTROLLER: KeyBindingController = {
  bind(id, shortcut, handler, options) {
    if (!id.trim()) {
      throw new Error("key binding id must not be empty");
    }

    const entry: RuntimeKeyBindingConfigEntry = {
      id,
      shortcut: normalizeShortcut(shortcut),
      on: normalizePhase(options),
      ...(options?.within === undefined ? {} : { within: normalizeWithin(options.within) }),
      ...(options?.locked ? { locked: true } : {}),
      ...(options?.repeat ? { repeat: true } : {}),
      ...(options?.transparent ? { transparent: true } : {}),
      ...(options?.nonConsuming ? { non_consuming: true } : {}),
    };

    registrationEntriesTarget().set(id, entry);
    registrationHandlersTarget().set(id, handler);

    if (!stagedKeyBindingEntries) {
      pendingKeyBindingConfig = true;
    }
  },
  isRepeat() {
    if (!bridge) throw new Error("Read key repeat after embedded runtime initialization");
    return bridge.isKeyRepeat();
  },
};

export function beginKeyBindingRegistration(): void {
  stagedKeyBindingEntries = new Map();
  stagedKeyBindingHandlers = new Map();
}

export function commitKeyBindingRegistration(): void {
  if (!stagedKeyBindingEntries || !stagedKeyBindingHandlers) {
    return;
  }

  desiredKeyBindingEntries = stagedKeyBindingEntries;
  desiredKeyBindingHandlers = stagedKeyBindingHandlers;
  stagedKeyBindingEntries = null;
  stagedKeyBindingHandlers = null;
  pendingKeyBindingConfig = true;
}

export function takePendingKeyBindingConfig():
  | RuntimeKeyBindingConfigEntry[]
  | undefined {
  if (!pendingKeyBindingConfig) {
    return undefined;
  }

  pendingKeyBindingConfig = false;
  return Array.from(desiredKeyBindingEntries.values())
    .sort((left, right) => left.id.localeCompare(right.id))
    .map(cloneConfigEntry);
}

export function invokeKeyBinding(bindingId: string): boolean {
  const handler = desiredKeyBindingHandlers.get(bindingId);
  if (!handler) {
    return false;
  }

  handler();
  return true;
}
