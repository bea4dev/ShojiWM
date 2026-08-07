#!/bin/bash
# Report a process's GPU memory from /proc/PID/fdinfo (amdgpu/i915 expose
# drm-memory-vram / drm-memory-gtt per drm client) plus its dmabuf fd count.
# Useful for chasing VRAM-leak-like behavior (smithay/smithay#1562):
#
#   watch -n 5 './tools/measure-drm-memory.sh $(pgrep -x shoji_wm)'
#
# Interpreting results: open/close windows repeatedly and compare. Transient
# growth that settles back after ~30s is caching / deferred cleanup, not a
# leak. A leak shows as monotonic growth proportional to the number of
# closed windows that never comes back down.
PID=${1:?usage: measure-drm-memory.sh <pid>}
declare -A seen
total_vram=0
total_gtt=0
dmabuf_count=$(ls -la /proc/$PID/fd 2>/dev/null | grep -c dmabuf)
for fd in /proc/$PID/fdinfo/*; do
    content=$(cat "$fd" 2>/dev/null) || continue
    client=$(echo "$content" | awk '/drm-client-id/ {print $2}')
    [ -z "$client" ] && continue
    # Multiple fds can point at the same drm client; count each client once.
    [ -n "${seen[$client]}" ] && continue
    seen[$client]=1
    vram=$(echo "$content" | awk '/drm-memory-vram/ {print $2}')
    gtt=$(echo "$content" | awk '/drm-memory-gtt/ {print $2}')
    total_vram=$((total_vram + ${vram:-0}))
    total_gtt=$((total_gtt + ${gtt:-0}))
done
echo "vram_kib=$total_vram gtt_kib=$total_gtt dmabuf_fds=$dmabuf_count"
