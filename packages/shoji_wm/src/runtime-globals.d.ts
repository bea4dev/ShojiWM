// Globals the embedded runtime installs before a config is evaluated.
//
// Referenced from `index.ts` so that importing `shoji_wm` is enough to see
// them. A config package cannot pick these up on its own: the declarations
// live in this package's `src`, outside the `include` of both
// `packages/config/tsconfig.json` and the tsconfig `dist/install.sh` writes
// into `~/.config/shojiwm`, so `process` resolved at runtime but not to tsc.
//
// Declare only what `src/shojiwm/src/ssd/embedded_runtime.js` actually
// provides. Anything declared here that the runtime does not install would
// typecheck cleanly and then throw at config-evaluation time, which takes the
// whole session down — the same failure mode as the `node:` imports this file
// replaced.

/**
 * Node-shaped view of the process environment. The runtime installs this as a
 * `Proxy` over `Deno.env` (`embedded_runtime.js`), so reads and writes hit the
 * real environment and stay consistent with `COMPOSITOR.env`.
 *
 * Prefer `COMPOSITOR.env` in new config code; this exists so configs written
 * against the old Node-based runtime keep working.
 */
declare const process: {
  cwd(): string;
  env: Record<string, string | undefined>;
};