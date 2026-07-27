#!/usr/bin/env bash
set -euo pipefail

duration="${1:-15}"
out_dir="${2:-"$HOME/shoji_wm/logs/perf-$(date +%Y%m%d-%H%M%S)"}"
freq="${PERF_FREQ:-99}"
call_graph="${PERF_CALL_GRAPH:-dwarf}"

if ! command -v perf >/dev/null 2>&1; then
  echo "perf was not found. Install linux perf for your kernel package." >&2
  exit 1
fi

mkdir -p "$out_dir"

if [[ -n "${PIDS:-}" ]]; then
  pid_list="$PIDS"
else
  mapfile -t detected_pids < <(
    pgrep -x shoji_wm | awk '!seen[$0]++ && $0 ~ /^[0-9]+$/' || true
  )

  if [[ "${#detected_pids[@]}" -eq 0 ]]; then
    echo "No target process found. Re-run with PIDS=<pid[,pid...]> $0 [seconds] [out_dir]." >&2
    exit 1
  fi

  pid_list="$(IFS=,; echo "${detected_pids[*]}")"
fi

echo "duration=${duration}s" | tee "$out_dir/summary.txt"
echo "freq=${freq}" | tee -a "$out_dir/summary.txt"
echo "call_graph=${call_graph}" | tee -a "$out_dir/summary.txt"
echo "pids=${pid_list}" | tee -a "$out_dir/summary.txt"
ps -o pid,ppid,pcpu,pmem,comm,args -p "$pid_list" | tee "$out_dir/processes.txt"

record_cmd=(
  perf record
  -F "$freq"
  -g
  --call-graph "$call_graph"
  -p "$pid_list"
  -o "$out_dir/perf.data"
  --
  sleep "$duration"
)

echo "record command: ${record_cmd[*]}" | tee -a "$out_dir/summary.txt"
if ! "${record_cmd[@]}" 2>&1 | tee "$out_dir/perf-record.log"; then
  cat >&2 <<EOF

perf record failed.
Common fixes:
  sudo sysctl kernel.perf_event_paranoid=1
  sudo sysctl kernel.kptr_restrict=0

If permission is still denied, run this script with sudo:
  sudo env HOME="$HOME" PIDS="$pid_list" $0 "$duration" "$out_dir"
EOF
  exit 1
fi

perf report --stdio --no-children --sort comm,dso,symbol -i "$out_dir/perf.data" \
  > "$out_dir/report.self.txt"
perf report --stdio --children --sort comm,dso,symbol -i "$out_dir/perf.data" \
  > "$out_dir/report.children.txt"

awk '
  /^[[:space:]]*[0-9]+([.][0-9]+)?%/ {
    print
    count++
    if (count >= 10) exit
  }
' "$out_dir/report.self.txt" > "$out_dir/top10.self.txt"

awk '
  /^[[:space:]]*[0-9]+([.][0-9]+)?%/ {
    print
    count++
    if (count >= 10) exit
  }
' "$out_dir/report.children.txt" > "$out_dir/top10.children.txt"

{
  echo
  echo "Top 10 self-time symbols:"
  cat "$out_dir/top10.self.txt"
  echo
  echo "Top 10 inclusive symbols:"
  cat "$out_dir/top10.children.txt"
  echo
  echo "Saved files:"
  find "$out_dir" -maxdepth 1 -type f | sort
} | tee -a "$out_dir/summary.txt"
