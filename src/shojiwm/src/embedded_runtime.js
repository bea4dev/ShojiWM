import {
  ShojiRuntimeBridge,
  op_shoji_current_dir,
  op_shoji_environment,
  op_shoji_path_exists,
  op_shoji_process_id,
  op_shoji_remove_unix_socket,
  op_shoji_ipc_listen,
  op_shoji_wake_compositor,
  op_shoji_output_power,
  op_shoji_input_grab,
  op_shoji_cursor_set_position,
  op_shoji_cursor_position,
} from "ext:core/ops";

globalThis.ShojiRuntimeBridge = ShojiRuntimeBridge;
globalThis.__SHOJI_EMBEDDED_RUNTIME__ = true;
globalThis.__SHOJI_PATH_EXISTS__ = op_shoji_path_exists;
globalThis.__SHOJI_REMOVE_UNIX_SOCKET__ = op_shoji_remove_unix_socket;
globalThis.__SHOJI_IPC_LISTEN__ = op_shoji_ipc_listen;
globalThis.__SHOJI_WAKE_COMPOSITOR__ = op_shoji_wake_compositor;
globalThis.__SHOJI_OUTPUT_POWER__ = op_shoji_output_power;
globalThis.__SHOJI_INPUT_GRAB__ = op_shoji_input_grab;
globalThis.__SHOJI_CURSOR_SET_POSITION__ = op_shoji_cursor_set_position;
globalThis.__SHOJI_CURSOR_POSITION__ = op_shoji_cursor_position;

const environment = new Map(Object.entries(op_shoji_environment()));
const env = {
  get(key) {
    return environment.get(String(key));
  },
  set(key, value) {
    environment.set(String(key), String(value));
  },
  delete(key) {
    environment.delete(String(key));
  },
  toObject() {
    return Object.fromEntries(environment);
  },
};

globalThis.Deno ??= {};
globalThis.Deno.env ??= env;
// RustyScript's current directory only affects module resolution. Deno's fs
// extension otherwise keeps the process cwd captured at startup, so expose the
// per-runtime directory maintained by the host.
globalThis.Deno.cwd = op_shoji_current_dir;
globalThis.Deno.pid ??= op_shoji_process_id();

// Keep existing user configs that read process.env working while all runtime
// I/O is provided by Deno. Writes are forwarded to the actual process
// environment so COMPOSITOR.env and preload.ts observe the same values.
if (!globalThis.process && globalThis.Deno) {
  const processEnv = new Proxy(
    {},
    {
      get(_target, key) {
        return typeof key === "string" ? Deno.env.get(key) : undefined;
      },
      set(_target, key, value) {
        if (typeof key === "string") {
          Deno.env.set(key, String(value));
        }
        return true;
      },
      deleteProperty(_target, key) {
        if (typeof key === "string") {
          Deno.env.delete(key);
        }
        return true;
      },
      ownKeys() {
        return Object.keys(Deno.env.toObject());
      },
      getOwnPropertyDescriptor() {
        return { configurable: true, enumerable: true };
      },
    },
  );
  globalThis.process = {
    env: processEnv,
    cwd: () => Deno.cwd(),
  };
}
