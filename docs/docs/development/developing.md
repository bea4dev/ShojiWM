---
sidebar_position: 1
---

# Development

This page covers running ShojiWM from a local checkout and measuring its
performance. For how the codebase is organized, see
[the architecture overview](../architecture/shojiwm.md).

## Running from source

Run ShojiWM from your local checkout with **`--dev --tty`**. The two flags are
independent and complementary:

- **`--dev`** runs the TypeScript decoration runtime and config from the **local
  repository** (instead of the installed copy under `/usr`), so your in-tree
  changes take effect. It must be run from the repo root.
- **`--tty`** uses the **DRM/KMS backend** on a real TTY. The default winit
  backend is currently unreliable, so prefer `--tty`.

Switch to a free virtual terminal (e.g. `Ctrl`+`Alt`+`F3`), log in, `cd` into the
repository, and run:

```bash
cargo run -p shoji_wm -- --dev --tty
```

A plain (debug) build compiles fastest, which is what you want while iterating on
functionality. For performance work, build with `--release` (see below).

## Build profile & performance

:::warning[Always measure with --release]
Debug builds are **dramatically** slower than release builds — often by an order
of magnitude. Any performance number from a debug build is meaningless. Whenever
you evaluate performance, add `--release`:

```bash
cargo run --release -p shoji_wm -- --dev --tty
```

:::

## Profiling

ShojiWM runs the Rust compositor and the RustyScript/Deno decoration runtime in
one `shoji_wm` process. The helper script
[`tools/perf-top-functions.sh`](https://github.com/bea4dev/ShojiWM/blob/main/tools/perf-top-functions.sh)
profiles that process with Linux `perf`.

1. Start ShojiWM with a **release** build and put it under the load you want to
   measure.
2. While it is running, profile for N seconds (default `15`):

   ```bash
   tools/perf-top-functions.sh 20
   ```

   The script auto-detects the `shoji_wm` PID, records with `perf`, and writes
   top-10 self-time and inclusive symbol reports (you can also pin targets with
   `PIDS=<pid,pid> tools/perf-top-functions.sh`).

:::note
`perf` may need relaxed kernel settings (e.g.
`sudo sysctl kernel.perf_event_paranoid=1`). The script prints the exact fixes if
recording fails.
:::

V8 JIT frames may still appear as raw addresses in `perf`. Use ShojiWM's
timescope profiler when you need timings for the compositor/runtime protocol
boundaries.
