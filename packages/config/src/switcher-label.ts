/**
 * The title under the Coverflow switcher's selected card.
 *
 * The output composition cannot draw text, so `./helpers/switcher-label.py`
 * draws it on a layer-shell surface. This tells the helper, over the IPC
 * socket, which window is selected:
 *
 *   "switcher.label" { visible: true, title, appId }   a window is selected
 *   "switcher.label" { visible: false }                the switcher closes
 */
import { effect } from "shoji_wm";
import type { IpcServer } from "shoji_wm/ipc";
import type { HybridWindowManager } from "./window-manager";
import type { WindowSwitcher } from "./window-switcher";

/** Broadcast the switcher's selected window; returns a stop function. */
export function broadcastSwitcherLabel(
  ipc: IpcServer,
  switcher: WindowSwitcher,
  windowManager: HybridWindowManager,
): () => void {
  return effect(() => {
    const id = switcher.selectedWindowId();
    const window = id === null ? undefined : windowManager.findWindowById(id);
    ipc.broadcast(
      "switcher.label",
      window
        ? { visible: true, title: window.title.peek(), appId: window.appId.peek() ?? "" }
        : { visible: false },
    );
  });
}
