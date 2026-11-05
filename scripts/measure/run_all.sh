#!/bin/bash
# Milestone 1 Mac measurements, all phases in sequence (about 85 minutes). Runs one launch at a time and alternates
# the two apps run by run (Slint, Tauri, Slint, ...), so drift and machine load hit both alike.
#   scripts/measure/run_all.sh [RUN_DIR] [phases...]     phases: cold idle running latency views (default: all)
# RUN_DIR holds bin/ (zwt-app-macos, zWorkTracker-tauri-smoke), stores/ (sample, s5k, s50k) and storegen/;
# results are appended to RUN_DIR/raw/*.jsonl. See docs/MEASUREMENTS.md for how to build those inputs.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
RUN="${1:-$REPO/.stack/out/orchestrator/20261104-0756-m1-slint/nodes/M1}"
shift || true
PHASES="${*:-cold idle running latency views}"
M() { python3 "$HERE/measure.py" "$@" --run-dir "$RUN"; }
# One assertion for the whole suite: each measure.py holds its own, but a display that slept between two of them
# would stay asleep (caffeinate -d prevents idle sleep, it does not wake), and then no frame is drawn.
caffeinate -d -i -w $$ &

for phase in $PHASES; do
  echo "== $phase $(date +%H:%M:%S)"
  case $phase in
    cold)
      for i in $(seq 1 11); do M cold --app slint --n 1; M cold --app tauri --n 1; done ;;
    idle)
      for i in $(seq 1 10); do M idle --app slint --n 1; M idle --app tauri --n 1; done ;;
    running)
      # The first three runs of each app stay up 5 minutes for the late RSS; all ten give the 60 s CPU window.
      for i in $(seq 1 10); do
        late=0; [ "$i" -le 3 ] && late=1
        M running --app slint --n 1 --rss-runs $late
        M running --app tauri --n 1 --rss-runs $late
      done ;;
    latency)
      for store in sample s5k s50k; do
        for i in $(seq 1 10); do
          M latency --app slint --n 1 --rounds 20 --store $store
          M latency --app tauri --n 1 --rounds 20 --store $store
        done
      done ;;
    views)
      G="$RUN/storegen/target/release/zwt-storegen"
      for store in sample s5k s50k; do
        D="$(mktemp -d "$RUN/work/views-$store.XXXX")"
        cp "$RUN/stores/$store/z-work-data.db" "$D/"
        SB=(sandbox-exec -D "REAL_HOME=$HOME" -D "USER_CACHE=$(realpath "$(getconf DARWIN_USER_CACHE_DIR)")/" \
            -D "USER_TEMP=$(realpath "$(getconf DARWIN_USER_TEMP_DIR)")/" -f "$HERE/sandbox.sb")
        "${SB[@]}" "$G" bench "$D" 50 Europe/Lisbon | sed "s/^/store=$store /" | tee -a "$RUN/raw/views.txt"
      done ;;
  esac
done
echo "== done $(date +%H:%M:%S)"
