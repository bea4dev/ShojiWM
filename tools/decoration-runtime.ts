import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { createInterface } from "node:readline";

import {
  createDecorationEvaluationCache,
  type DecorationEvaluationCache,
  type DecorationFunction,
  type WaylandWindowActions,
  type WaylandWindowSnapshot,
  type WindowTransform,
} from "shoji_wm";

interface RuntimeRequest {
  requestId: number;
  snapshot: WaylandWindowSnapshot;
}

interface RuntimeSuccess {
  requestId: number;
  ok: true;
  serialized: unknown;
  transform: WindowTransform;
}

interface RuntimeFailure {
  requestId: number;
  ok: false;
  error: string;
}

const cacheByWindowId = new Map<string, RuntimeCacheEntry>();

interface RuntimeCacheEntry {
  latestSnapshot: WaylandWindowSnapshot;
  cache: DecorationEvaluationCache;
}

async function main() {
  const configPath = process.argv[2];
  if (!configPath) {
    throw new Error("usage: tsx tools/decoration-runtime.ts <config-path>");
  }

  const moduleUrl = pathToFileURL(resolve(configPath)).href;
  const loaded = await import(moduleUrl);
  const decoration = resolveDecoration(loaded);

  const rl = createInterface({
    input: process.stdin,
    crlfDelay: Infinity,
  });

  for await (const line of rl) {
    if (!line.trim()) {
      continue;
    }

    let request: RuntimeRequest;
    try {
      request = JSON.parse(line) as RuntimeRequest;
    } catch (error) {
      writeResponse({
        requestId: -1,
        ok: false,
        error: error instanceof Error ? error.message : String(error),
      });
      continue;
    }

    try {
      const serialized = evaluateSnapshot(decoration, request.snapshot);
      writeResponse({
        requestId: request.requestId,
        ok: true,
        serialized,
        transform: cacheByWindowId.get(request.snapshot.id)?.cache.lastTransform ??
          identityTransform(),
      });
    } catch (error) {
      writeResponse({
        requestId: request.requestId,
        ok: false,
        error: error instanceof Error ? error.stack ?? error.message : String(error),
      });
    }
  }
}

function evaluateSnapshot(
  decoration: DecorationFunction,
  snapshot: WaylandWindowSnapshot,
): unknown {
  const existing = cacheByWindowId.get(snapshot.id);
  if (!existing) {
    const entry = createRuntimeCacheEntry(snapshot, decoration);
    cacheByWindowId.set(snapshot.id, entry);
    return entry.cache.lastSerialized;
  }

  existing.latestSnapshot = snapshot;
  const updated = existing.cache.update(snapshot);
  return updated?.serialized ?? existing.cache.lastSerialized;
}

function createRuntimeCacheEntry(
  snapshot: WaylandWindowSnapshot,
  decoration: DecorationFunction,
): RuntimeCacheEntry {
  let latestSnapshot = snapshot;
  const actions: WaylandWindowActions = {
    close() {},
    maximize() {},
    minimize() {},
    isXWayland() {
      return latestSnapshot.isXwayland;
    },
  };

  const cache = createDecorationEvaluationCache(snapshot, actions, decoration);
  return {
    latestSnapshot,
    cache,
  };
}

function identityTransform(): WindowTransform {
  return {
    origin: { x: 0.5, y: 0.5 },
    translateX: 0,
    translateY: 0,
    scaleX: 1,
    scaleY: 1,
    opacity: 1,
  };
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

function writeResponse(response: RuntimeSuccess | RuntimeFailure) {
  process.stdout.write(`${JSON.stringify(response)}\n`);
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
