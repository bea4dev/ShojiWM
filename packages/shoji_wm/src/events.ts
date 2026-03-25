import type { WaylandLayer, WaylandWindow } from "./types";

export type WindowOpenListener = (window: WaylandWindow) => void;
export type WindowCloseListener = (window: WaylandWindow) => void;
export type WindowFocusListener = (window: WaylandWindow, focused: boolean) => void;
export type WindowStartCloseListener = (window: WaylandWindow) => void;
export type LayerCreateListener = (layer: WaylandLayer) => void;
export type LayerDestroyListener = (layer: WaylandLayer) => void;

export interface WindowManagerEventController {
  onOpen(listener: WindowOpenListener): () => void;
  onClose(listener: WindowCloseListener): () => void;
  onFocus(listener: WindowFocusListener): () => void;
  onStartClose(listener: WindowStartCloseListener): () => void;
  onCreateLayer(listener: LayerCreateListener): () => void;
  onDestroyLayer(listener: LayerDestroyListener): () => void;
  emitOpen(window: WaylandWindow): void;
  emitClose(window: WaylandWindow): void;
  emitFocus(window: WaylandWindow, focused: boolean): void;
  emitStartClose(window: WaylandWindow): void;
  emitCreateLayer(layer: WaylandLayer): void;
  emitDestroyLayer(layer: WaylandLayer): void;
}

export function createWindowManagerEventController(): WindowManagerEventController {
  const openListeners = new Set<WindowOpenListener>();
  const closeListeners = new Set<WindowCloseListener>();
  const focusListeners = new Set<WindowFocusListener>();
  const startCloseListeners = new Set<WindowStartCloseListener>();
  const createLayerListeners = new Set<LayerCreateListener>();
  const destroyLayerListeners = new Set<LayerDestroyListener>();

  return {
    onOpen(listener) {
      openListeners.add(listener);
      return () => openListeners.delete(listener);
    },
    onClose(listener) {
      closeListeners.add(listener);
      return () => closeListeners.delete(listener);
    },
    onFocus(listener) {
      focusListeners.add(listener);
      return () => focusListeners.delete(listener);
    },
    onStartClose(listener) {
      startCloseListeners.add(listener);
      return () => startCloseListeners.delete(listener);
    },
    onCreateLayer(listener) {
      createLayerListeners.add(listener);
      return () => createLayerListeners.delete(listener);
    },
    onDestroyLayer(listener) {
      destroyLayerListeners.add(listener);
      return () => destroyLayerListeners.delete(listener);
    },
    emitOpen(window) {
      for (const listener of openListeners) {
        listener(window);
      }
    },
    emitClose(window) {
      for (const listener of closeListeners) {
        listener(window);
      }
    },
    emitFocus(window, focused) {
      for (const listener of focusListeners) {
        listener(window, focused);
      }
    },
    emitStartClose(window) {
      for (const listener of startCloseListeners) {
        listener(window);
      }
    },
    emitCreateLayer(layer) {
      for (const listener of createLayerListeners) {
        listener(layer);
      }
    },
    emitDestroyLayer(layer) {
      for (const listener of destroyLayerListeners) {
        listener(layer);
      }
    },
  };
}
