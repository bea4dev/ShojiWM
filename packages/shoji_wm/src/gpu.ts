import type {
  GpuChangeListener,
  GpuController,
  GpuInfo,
  RenderGpuResolver,
} from "./types";

let currentGpus: readonly GpuInfo[] = [];
const changeListeners = new Set<GpuChangeListener>();
let renderResolver: RenderGpuResolver | null = null;

/** @internal The compositor reported the GPUs. */
export function emitGpuState(gpus: GpuInfo[]): void {
  currentGpus = Object.freeze(gpus.map((gpu) => Object.freeze({ ...gpu })));
  for (const listener of changeListeners) {
    listener(currentGpus);
  }
}

/**
 * @internal `COMPOSITOR.gpu.selectRender` for the GPUs the compositor found:
 * the id of the GPU to composite on, or `null` for ShojiWM's own pick.
 */
export function resolveRenderGpu(gpus: GpuInfo[]): string | null {
  if (!renderResolver) {
    return null;
  }
  const choice = renderResolver(gpus);
  if (choice === undefined || choice === null || choice === "auto") {
    return null;
  }
  if (typeof choice === "string") {
    return choice;
  }
  if (typeof choice === "object" && typeof choice.id === "string") {
    return choice.id;
  }
  throw new TypeError(
    'COMPOSITOR.gpu.selectRender must return a GPU, its id, "auto", or nothing',
  );
}

/** `DRI_PRIME` takes udev's `ID_PATH_TAG`: `pci-0000:03:00.0` is `pci-0000_03_00_0`. */
function idPathTag(id: string): string {
  return id.replace(/[^A-Za-z0-9-]/g, "_");
}

function hex4(value: number): string {
  return value.toString(16).padStart(4, "0");
}

export const GPU_CONTROLLER: GpuController = {
  get list() {
    return currentGpus;
  },

  get render() {
    return currentGpus.find((gpu) => gpu.isRender);
  },

  get(id) {
    return currentGpus.find((gpu) => gpu.id === id);
  },

  onChange(listener) {
    changeListeners.add(listener);
    return () => changeListeners.delete(listener);
  },

  selectRender(resolver) {
    if (typeof resolver !== "function") {
      throw new TypeError("COMPOSITOR.gpu.selectRender takes a function");
    }
    renderResolver = resolver;
  },

  offloadEnv(gpu) {
    // The proprietary NVIDIA driver goes through GLVND and its Vulkan layer;
    // Mesa drivers (nouveau too) through DRI_PRIME and the device-select layer.
    if (gpu.driver === "nvidia") {
      return {
        __NV_PRIME_RENDER_OFFLOAD: "1",
        __GLX_VENDOR_LIBRARY_NAME: "nvidia",
        __VK_LAYER_NV_optimus: "NVIDIA_only",
      };
    }
    const env: Record<string, string> = { DRI_PRIME: idPathTag(gpu.id) };
    if (gpu.vendorId !== 0) {
      env.MESA_VK_DEVICE_SELECT = `${hex4(gpu.vendorId)}:${hex4(gpu.deviceId)}`;
    }
    return env;
  },
};
