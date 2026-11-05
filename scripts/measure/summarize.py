#!/usr/bin/env python3
"""Summarises measure.py's raw JSON lines: median, IQR, min-max and run count per metric and app.

  summarize.py [RUN_DIR]        reads RUN_DIR/raw/*.jsonl (default: the M1 node folder), prints Markdown tables
"""
import json
import re
import statistics
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
RUN = Path(sys.argv[1]) if len(sys.argv) > 1 else REPO / ".stack/out/orchestrator/20261104-0756-m1-slint/nodes/M1"
RAW = RUN / "raw"


def load(name):
    p = RAW / f"{name}.jsonl"
    return [json.loads(l) for l in p.read_text().splitlines() if l.strip()] if p.exists() else []


def st(xs):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return "n=0"
    n = len(xs)
    if n >= 2:
        q = statistics.quantiles(xs, n=4, method="inclusive")
        iqr = f"IQR {q[0]:.4g}-{q[2]:.4g}, "
    else:
        iqr = ""
    return f"{statistics.median(xs):.4g} ({iqr}min-max {xs[0]:.4g}-{xs[-1]:.4g}, n={n})"


def row(label, slint, tauri):
    def med(xs):
        xs = [x for x in xs if x is not None]
        return statistics.median(xs) if xs else None
    a, b = med(slint), med(tauri)
    ratio = f"{a / b:.2f}" if a is not None and b else "-"
    print(f"| {label} | {st(slint)} | {st(tauri)} | {ratio} |")


def main():
    print("| Metric | Slint | Tauri | Slint/Tauri |\n|---|---|---|---|")
    cs, ct = load("cold-slint"), load("cold-tauri")
    # The first launch of each app warms the page cache for its binary and is reported on its own.
    row("cold: exec to first frame / first render, ms (launches 2-11)",
        [r.get("first_frame_ms") for r in cs[1:]], [r.get("first_frame_ms") for r in ct[1:]])
    row("cold: first launch only, ms", [r.get("first_frame_ms") for r in cs[:1]], [r.get("first_frame_ms") for r in ct[:1]])
    row("cold: Slint main() to first frame / Tauri exec to page start, ms",
        [r.get("since_main_ms") for r in cs[1:]], [r.get("page_start_ms") for r in ct[1:]])
    row("cold: exec to window visible, ms (Tauri polled)", [r.get("first_frame_ms") for r in cs[1:]],
        [r.get("window_shown_ms") for r in ct[1:]])
    for name, label in (("idle", "nothing running"), ("running", "task running")):
        s, t = load(f"{name}-slint"), load(f"{name}-tauri")
        s = [r for r in s if "error" not in r]
        t = [r for r in t if "error" not in r]
        row(f"{label}: RSS at 10 s, MB (sum of processes)", [r["rss10_mb"] for r in s], [r["rss10_mb"] for r in t])
        row(f"{label}: phys_footprint at 10 s, MB", [r["footprint10_mb"] for r in s], [r["footprint10_mb"] for r in t])
        row(f"{label}: CPU % of one core, 60 s (ps cputime)", [r["cpu_pct"] for r in s], [r["cpu_pct"] for r in t])
        row(f"{label}: CPU % (top)", [r["top_cpu_pct"] for r in s], [r["top_cpu_pct"] for r in t])
        row(f"{label}: idle wakeups/s (top)", [r["idlew_per_s"] for r in s], [r["idlew_per_s"] for r in t])
        if name == "running":
            row("task running: RSS at 300 s, MB", [r.get("rss_late_mb") for r in s], [r.get("rss_late_mb") for r in t])
            row("task running: phys_footprint at 300 s, MB", [r.get("footprint_late_mb") for r in s],
                [r.get("footprint_late_mb") for r in t])
        procs = sorted({p[1] for r in t for p in r["procs"]})
        print(f"\n<!-- {name} tauri processes: {procs}; per-process RSS medians: " + ", ".join(
            f"{k} {statistics.median([r['rss10_per'].get(k, 0) for r in t]):.1f}" for k in procs) + " -->\n")
    for store in ("sample", "s5k", "s50k"):
        s, t = load(f"latency-slint-{store}"), load(f"latency-tauri-{store}")
        errs = [r.get("error") for r in t if "error" in r] + [r.get("error") for r in s if "error" in r]
        s = [r for r in s if "error" not in r]
        for action in ("start", "pause", "resume", "share-level"):
            per_med, per_p95, tall = [], [], []
            for r in s:
                for line in r.get("lines", []):
                    m = re.search(rf"action={action} n=\d+ median-ms=([\d.]+) p95-ms=([\d.]+)", line)
                    if m:
                        per_med.append(float(m.group(1)))
                        per_p95.append(float(m.group(2)))
            for r in t:
                tall.extend(r.get(action, []))
            tp95 = []
            for r in t:
                xs = sorted(r.get(action, []))
                if xs:
                    tp95.append(xs[max(1, -(-95 * len(xs) // 100)) - 1])
            tmed = [statistics.median(r[action]) for r in t if r.get(action)]
            if per_med or tmed:
                row(f"latency {store} {action}: per-run median, ms", per_med, tmed)
                row(f"latency {store} {action}: per-run p95, ms", per_p95, tp95)
        misses = sum(r.get("rafMisses", 0) for r in t)
        print(f"\n<!-- latency {store}: errors {errs}; tauri rAF misses {misses} -->\n")
    views = RAW / "views.txt"
    if views.exists():
        print("\n```\n" + views.read_text() + "```")


if __name__ == "__main__":
    main()
