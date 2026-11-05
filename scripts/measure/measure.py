#!/usr/bin/env python3
"""Milestone 1 measurements on macOS: zWorkTracker 2.0 (Slint) against 1.x (Tauri), PLAN.md section 10.

Python 3 standard library only. Every launch runs under sandbox-exec with scripts/measure/sandbox.sb, on a copy of a
sample store in a fresh folder under --run-dir/work. The 1.x app must be a release build with the test-only `smoke`
feature (it honours ZWT_SMOKE_DATA_DIR only then) and also gets a temporary HOME and CFFIXED_USER_HOME, so its WebKit
folders land in that temporary home too. Never pass the real data folder; the harness refuses to run if an
instance of either app it did not start is running.

  measure.py cold    --app slint|tauri [--n 11]               exec to first frame / first render
  measure.py idle    --app slint|tauri [--n 10] [--cpu-secs 60] RSS 10 s after launch, then CPU over a window, nothing running
  measure.py running --app slint|tauri [--n 10] [--cpu-secs 60] [--rss-at 300 --rss-runs 3]
  measure.py latency --app slint|tauri [--n 10] [--rounds 20] [--store sample|s5k|s50k]
  measure.py proof                                             where the 1.x run wrote (redirect proof)

Inputs (under --run-dir): bin/zwt-app-macos, bin/zWorkTracker-tauri-smoke, stores/<name>/z-work-data.db (made by the
storegen helper: the zwt-bridge sample store, optionally with synthetic segments). Raw JSON lines go to raw/.

The display and the machine are kept awake for as long as this script runs (`caffeinate -d -i -w <pid>`): on macOS
both apps draw from the display's refresh (Slint's CADisplayLink, WebKit's requestAnimationFrame), which stops while
the display sleeps, so a frame would wait for the display to wake (docs/MEASUREMENTS.md, latency outlier).
"""
import argparse
import json
import os
import re
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SANDBOX = Path(__file__).resolve().parent / "sandbox.sb"
DEFAULT_RUN = REPO / ".stack/out/orchestrator/20261104-0756-m1-slint/nodes/M1"
REAL_HOME = os.path.expanduser("~")
FOREIGN = re.compile(r"(zWorkTracker|z-work-tracker)", re.I)


def now_ms():
    return time.time_ns() / 1e6


def sandbox_cmd():
    cache = os.path.realpath(os.popen("getconf DARWIN_USER_CACHE_DIR").read().strip()) + "/"
    temp = os.path.realpath(os.popen("getconf DARWIN_USER_TEMP_DIR").read().strip()) + "/"
    return ["sandbox-exec", "-D", f"REAL_HOME={REAL_HOME}", "-D", f"USER_CACHE={cache}", "-D",
            f"USER_TEMP={temp}", "-f", str(SANDBOX)]


def ps_table():
    out = subprocess.run(["ps", "-axo", "pid=,ppid=,comm="], capture_output=True, text=True).stdout
    rows = []
    for line in out.splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) == 3:
            rows.append((int(parts[0]), int(parts[1]), parts[2]))
    return rows


def xpc_children(main_pid):
    """XPC services launchd runs in the app's own domain (`launchctl print pid/<pid>`): WebKit's WebContent,
    Networking and GPU processes for this app. launchd starts them (ppid 1), so the parent pid cannot find them, and
    their responsible pid is the terminal that started the app, not the app."""
    out = subprocess.run(["launchctl", "print", f"pid/{main_pid}"], capture_output=True, text=True).stdout
    pids = {}
    for m in re.finditer(r"^\s+(\d+)\s+\S+\s+(com\.apple\.WebKit\.[\w.-]+)\s*$", out, flags=re.M):
        if int(m.group(1)) > 0:
            pids[int(m.group(1))] = m.group(2)
    return pids


def family(main_pid):
    """The main process, its children, and the XPC services in its launchd domain."""
    xpc = xpc_children(main_pid)
    procs = []
    for pid, ppid, comm in ps_table():
        if pid == main_pid or ppid == main_pid or pid in xpc:
            procs.append({"pid": pid, "ppid": ppid, "name": comm.rsplit("/", 1)[-1]})
    return procs


def guard_foreign(own=()):
    for pid, _, comm in ps_table():
        if pid not in own and FOREIGN.search(comm.rsplit("/", 1)[-1]) and "storegen" not in comm:
            sys.exit(f"refusing: a zWorkTracker process I did not start is running: {pid} {comm}")


def cpu_secs_text(t):
    # ps TIME: [[dd-]hh:]mm:ss.cc
    days = 0
    if "-" in t:
        d, t = t.split("-", 1)
        days = int(d)
    parts = [float(x) for x in t.split(":")]
    s = 0.0
    for p in parts:
        s = s * 60 + p
    return days * 86400 + s


def sample(procs):
    """RSS (KiB, ps) and cumulative CPU seconds per process."""
    if not procs:
        return {}
    pids = ",".join(str(p["pid"]) for p in procs)
    out = subprocess.run(["ps", "-o", "pid=,rss=,time=", "-p", pids], capture_output=True, text=True).stdout
    res = {}
    for line in out.splitlines():
        pid, rss, t = line.split()
        res[int(pid)] = {"rss_kb": int(rss), "cpu_s": cpu_secs_text(t)}
    return res


FOOTPRINT_TIMEOUT = 4


def footprint(procs):
    """phys_footprint per process (what Activity Monitor calls Memory), from `footprint -j`; None if unavailable."""
    res = {}
    for p in procs:
        with tempfile.NamedTemporaryFile(suffix=".json") as f:
            try:
                # footprint can take minutes or hang on these GPU-using processes (seen on both apps): bounded by
                # FOOTPRINT_TIMEOUT (--footprint-timeout, default 4 s), then recorded as null.
                subprocess.run(["footprint", "-p", str(p["pid"]), "-j", f.name], capture_output=True, text=True,
                               timeout=FOOTPRINT_TIMEOUT)
                data = json.load(open(f.name))
                total = data.get("total footprint")
                if total is None:
                    total = sum(proc.get("footprint", 0) for proc in data.get("processes", []))
                res[p["pid"]] = total
            except Exception:  # timed out or unreadable
                res[p["pid"]] = None
    return res


def fp_total(fp):
    """Sum in MB only when every process was read; a partial sum would undercount."""
    return sum(fp.values()) / 2**20 if fp and all(v is not None for v in fp.values()) else None


def top_window(procs, secs):
    """top's second sample covers `secs`: %CPU and idle wakeups (count over the interval) per pid."""
    out = subprocess.run(["top", "-l", "2", "-s", str(secs), "-stats", "pid,cpu,idlew"], capture_output=True,
                         text=True).stdout
    last = re.split(r"^PID\s+", out, flags=re.M)[-1].splitlines()[1:]
    want = {p["pid"] for p in procs}
    res = {}
    for line in last:
        m = re.match(r"\s*(\d+)\S*\s+([\d.]+)\s+(\d+)", line)
        if m and int(m.group(1)) in want:
            res[int(m.group(1))] = {"cpu_pct": float(m.group(2)), "idlew": int(m.group(3))}
    return res


def load_avg():
    return os.getloadavg()


# ---------------------------------------------------------------- launching

TAURI_HELPERS = """
const inv = window.__TAURI_INTERNALS__.invoke;
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const until = async (f, what) => { for (let i = 0; i < 600; i++) { const v = await f(); if (v) return v; await wait(50); } throw new Error('timed out: ' + what); };
const report = (o) => inv('smoke_report', { text: JSON.stringify(o) });
const run = async (f) => { try { await report(await f()); } catch (e) { await report({ error: String(e && e.message || e) }); } };
const afterFrame = () => new Promise((r) => requestAnimationFrame(() => setTimeout(r, 0)));
const frameUntil = async (f) => { while (!f()) await new Promise((r) => requestAnimationFrame(r)); await new Promise((r) => setTimeout(r, 0)); };
"""

COLD_JS = """run(async () => {
  await until(() => performance.getEntriesByName('z-first-render').length > 0, 'first render');
  const mark = performance.getEntriesByName('z-first-render')[0].startTime;
  // The window starts hidden; the page asks to show it after a requestAnimationFrame, and the backend shows it
  // anyway after 4 s. Poll when it became visible (this script runs about 1.5 s after setup).
  let shown = null;
  for (let i = 0; i < 1000 && shown === null; i++) { if (await inv('smoke_window_visible')) shown = Date.now(); else await wait(5); }
  return { timeOrigin: performance.timeOrigin, firstRenderEpochMs: performance.timeOrigin + mark, scriptEpochMs: Date.now(), windowShownEpochMs: shown };
});"""

START_JS = """run(async () => {
  await until(() => document.querySelector('#root') && document.querySelector('#root').children.length, 'render');
  await inv('start_task', { id: 't-social' });
  await wait(500);
  const s = await inv('get_snapshot');
  return { visible: await inv('smoke_window_visible'), running: s.tasks.filter((t) => t.state === 'running').map((t) => t.id), segments: s.segments.length };
});"""

LATENCY_JS = """run(async () => {
  const N = %(rounds)d;
  await until(() => document.querySelector('[role=tab]'), 'tabs');
  await until(() => inv('smoke_window_visible'), 'window visible');
  const tab = (name) => [...document.querySelectorAll('[role=tab]')].find((b) => b.textContent.trim().startsWith(name));
  await inv('start_task', { id: 't-social' });
  const card = () => document.querySelector('article[data-task-id="t-social"]');
  const state = () => card() && card().dataset.state;
  await until(() => state() === 'running', 'running');
  const button = (label) => card() && [...card().querySelectorAll('button')].find((b) => b.textContent.trim() === label && !b.disabled);
  // rAF does not fire for a hidden or fully covered WebKit window: a 250 ms timeout stands in and is counted.
  let misses = 0;
  const nextFrame = () => new Promise((r) => { let d = false; requestAnimationFrame(() => { if (!d) { d = true; r(true); } }); setTimeout(() => { if (!d) { d = true; r(false); } }, 250); });
  const frameUntil = async (f) => { let m = 0; while (!f()) { if (!(await nextFrame())) m++; if (m > 40) throw new Error('no frames'); } await new Promise((r) => setTimeout(r, 0)); misses += m; };
  const timed = async (click, done) => { if (!(await nextFrame())) misses++; await new Promise((r) => setTimeout(r, 0)); const t = performance.now(); click(); await frameUntil(done); return performance.now() - t; };
  const out = { pause: [], resume: [], 'share-level': [] };
  for (let i = 0; i < N; i++) {
    let b = await until(() => button('Pause'), 'Pause enabled');
    out.pause.push(await timed(() => b.click(), () => state() === 'paused'));
    b = await until(() => button('Resume'), 'Resume enabled');
    out.resume.push(await timed(() => b.click(), () => state() === 'running'));
  }
  tab('Analysis').click();
  const group = () => document.querySelector('[role=radiogroup][aria-label="Share of time by"]');
  await until(() => group(), 'share switch');
  const radio = (label) => [...group().querySelectorAll('[role=radio]')].find((b) => b.textContent.trim() === label);
  await wait(1000);
  for (let i = 0; i < 2 * N; i++) {
    const label = i %% 2 === 0 ? 'Tags' : 'Projects';
    out['share-level'].push(await timed(() => radio(label).click(), () => radio(label).getAttribute('aria-checked') === 'true'));
  }
  out.rafMisses = misses;
  return out;
});"""


class Run:
    def __init__(self, args, app, store, env=None, js=None, tag=""):
        self.app = app
        self.dir = Path(tempfile.mkdtemp(prefix=f"{app}-{tag}-", dir=args.run_dir / "work"))
        self.data = self.dir / "data"
        self.data.mkdir()
        if store != "fresh":
            shutil.copy2(args.run_dir / "stores" / store / "z-work-data.db", self.data / "z-work-data.db")
        self.stderr = open(self.dir / "stderr.txt", "w")
        e = dict(os.environ)
        e.update(env or {})
        if app == "slint":
            cmd = sandbox_cmd() + [str(args.run_dir / "bin/zwt-app-macos"), "--sample", str(self.data)]
        else:
            home = self.dir / "home"
            home.mkdir()
            self.out = self.dir / "out.json"
            e.update({"HOME": str(home), "CFFIXED_USER_HOME": str(home), "ZWT_SMOKE_DATA_DIR": str(self.data),
                      "ZWT_SMOKE_OUT": str(self.out)})
            if js:
                script = self.dir / "script.js"
                script.write_text(TAURI_HELPERS + js)
                e["ZWT_SMOKE_SCRIPT"] = str(script)
            cmd = sandbox_cmd() + [str(args.run_dir / "bin/zWorkTracker-tauri-smoke")]
        guard_foreign()
        self.t0 = now_ms()
        self.proc = subprocess.Popen(cmd, env=e, stdout=subprocess.DEVNULL, stderr=self.stderr, cwd=self.dir)

    def wait_out(self, timeout=90):
        end = time.time() + timeout
        while time.time() < end:
            if self.out.exists():
                try:
                    return json.loads(self.out.read_text())
                except ValueError:
                    pass
            if self.proc.poll() is not None:
                return {"error": f"exited {self.proc.returncode}"}
            time.sleep(0.05)
        return {"error": "timeout"}

    def stderr_lines(self):
        if not self.stderr.closed:
            self.stderr.flush()
        return (self.dir / "stderr.txt").read_text().splitlines()

    def wait_exit(self, timeout=120):
        try:
            self.proc.wait(timeout)
        except subprocess.TimeoutExpired:
            self.quit()

    def quit(self):
        fam = family(self.proc.pid)
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
            self.killed = True
        time.sleep(1.0)
        # Helpers of ours (by responsibility, recorded while the app ran) that outlived it.
        alive = {pid for pid, _, _ in ps_table()}
        for p in fam:
            if p["pid"] != self.proc.pid and p["pid"] in alive and p["ppid"] == 1 and "WebKit" in p["name"]:
                os.kill(p["pid"], signal.SIGKILL)
        if not self.stderr.closed:
            self.stderr.close()


def record(args, name, obj):
    obj = dict(obj)
    obj["at"] = time.strftime("%Y-%m-%dT%H:%M:%S")
    obj["load"] = load_avg()
    with open(args.raw / f"{name}.jsonl", "a") as f:
        f.write(json.dumps(obj) + "\n")
    print(json.dumps(obj), flush=True)


def stats(xs):
    xs = sorted(xs)
    n = len(xs)
    if n == 0:
        return None
    q = statistics.quantiles(xs, n=4, method="inclusive") if n > 1 else [xs[0]] * 3
    return {"n": n, "median": statistics.median(xs), "min": xs[0], "max": xs[-1], "q1": q[0], "q3": q[2],
            "p95": xs[max(1, -(-95 * n // 100)) - 1]}


# ---------------------------------------------------------------- metrics

def cmd_cold(args):
    for i in range(args.n):
        if args.app == "slint":
            r = Run(args, "slint", args.store, {"ZWT_MEASURE": "1", "ZWT_MEASURE_EXIT": "1"}, tag="cold")
            r.wait_exit(60)
            line = next((l for l in r.stderr_lines() if "first-frame" in l), "")
            m = re.search(r"epoch-ms=(\d+) since-main-ms=([\d.]+)", line)
            res = {"first_frame_ms": int(m.group(1)) - r.t0, "since_main_ms": float(m.group(2))} if m else {"error": line}
            r.stderr.close()
        else:
            r = Run(args, "tauri", args.store, js=COLD_JS, tag="cold")
            o = r.wait_out()
            r.quit()
            res = ({"first_frame_ms": o["firstRenderEpochMs"] - r.t0, "page_start_ms": o["timeOrigin"] - r.t0,
                    "window_shown_ms": (o["windowShownEpochMs"] - r.t0) if o.get("windowShownEpochMs") else None}
                   if "error" not in o else o)
        record(args, f"cold-{args.app}", {"run": i, "store": args.store, **res})
        time.sleep(1)


def settle_until(r, secs):
    left = r.t0 / 1000 + secs - time.time()
    if left > 0:
        time.sleep(left)


def measure_state(args, r, label, cpu_secs, rss_at=None):
    settle_until(r, 10)
    fam = family(r.proc.pid)
    s0 = sample(fam)
    fp = footprint(fam)
    rss10 = sum(v["rss_kb"] for v in s0.values())
    w0 = time.time()
    top = top_window(fam, cpu_secs) if cpu_secs else {}
    w1 = time.time()
    s1 = sample(fam)
    cpu = sum(s1[p]["cpu_s"] - s0[p]["cpu_s"] for p in s1 if p in s0)
    res = {"procs": [(p["pid"], p["name"]) for p in fam], "rss10_mb": rss10 / 1024,
           "rss10_per": {p["name"]: s0.get(p["pid"], {}).get("rss_kb", 0) / 1024 for p in fam},
           "footprint10_mb": fp_total(fp), "footprint10_per": {str(k): v for k, v in fp.items()},
           "cpu_window_s": w1 - w0, "cpu_pct": 100 * cpu / (w1 - w0) if cpu_secs else None,
           "top_cpu_pct": sum(v["cpu_pct"] for v in top.values()) if top else None,
           "idlew_per_s": sum(v["idlew"] for v in top.values()) / cpu_secs if top else None,
           "top_per": {str(k): v for k, v in top.items()}}
    if rss_at:
        settle_until(r, rss_at)
        fam2 = family(r.proc.pid)
        s2 = sample(fam2)
        fp2 = footprint(fam2)
        res.update({"rss_late_s": rss_at, "rss_late_mb": sum(v["rss_kb"] for v in s2.values()) / 1024,
                    "footprint_late_mb": fp_total(fp2),
                    "procs_late": [(p["pid"], p["name"]) for p in fam2]})
    return res


def cmd_idle(args):
    for i in range(args.n):
        r = Run(args, args.app, args.store, tag="idle")
        res = measure_state(args, r, "idle", args.cpu_secs)
        r.quit()
        record(args, f"idle-{args.app}", {"run": i, "store": args.store, **res})


def cmd_running(args):
    for i in range(args.n):
        late = args.rss_at if i < args.rss_runs else None
        if args.app == "slint":
            r = Run(args, "slint", "fresh", tag="run")  # a fresh sample folder: seeded, then t-social runs
            check = {"started": "seeded"}
        else:
            r = Run(args, "tauri", args.store, js=START_JS, tag="run")
            check = r.wait_out()
            if "error" in check:
                r.quit()
                record(args, f"running-{args.app}", {"run": i, "error": check})
                continue
        res = measure_state(args, r, "running", args.cpu_secs, late)
        r.quit()
        record(args, f"running-{args.app}", {"run": i, "store": args.store, "check": check, **res})


def cmd_latency(args):
    for i in range(args.n):
        if args.app == "slint":
            store = "fresh" if args.store == "sample" else args.store
            r = Run(args, "slint", store, {"ZWT_MEASURE": "1", "ZWT_MEASURE_LATENCY": str(args.rounds)}, tag="lat")
            # The hook prints its stats only at the end. It stops by itself, with a "latency stalled" line and no
            # stats, when an applied answer gets no frame within 1 s (the display asleep); a run that has not
            # finished in 120 s is stopped here. Both are recorded as errors.
            r.wait_exit(120)
            lines = [l for l in r.stderr_lines() if l.startswith("zwt-measure")]
            if not r.stderr.closed:
                r.stderr.close()
            res = {"lines": lines}
            stalled = [l for l in lines if l.startswith("zwt-measure latency stalled")]
            if stalled:
                res["error"] = stalled[0].removeprefix("zwt-measure latency ")
            elif getattr(r, "killed", False) or not any(" median-ms=" in l for l in lines):
                res["error"] = "no latency lines within 120 s (stopped)"
        else:
            r = Run(args, "tauri", args.store, js=LATENCY_JS % {"rounds": args.rounds}, tag="lat")
            res = r.wait_out(30 + 3 * args.rounds)
            r.quit()
        record(args, f"latency-{args.app}-{args.store}", {"run": i, "store": args.store, **res})


def cmd_proof(args):
    """Launch 1.x once with a start action, then show where it wrote: the temporary data folder and HOME."""
    r = Run(args, "tauri", "sample", js=START_JS, tag="proof")
    o = r.wait_out()
    time.sleep(2)
    fam = family(r.proc.pid)
    r.quit()
    files = sorted(str(p.relative_to(r.dir)) for p in r.dir.rglob("*") if p.is_file())
    db = r.data / "z-work-data.db"
    q = subprocess.run(["sqlite3", str(db), ".tables"], capture_output=True, text=True).stdout.split()
    record(args, "proof-tauri", {"dir": str(r.dir), "script": o, "family": [(p["pid"], p["name"]) for p in fam],
                                  "db_mtime_after_t0": db.stat().st_mtime * 1000 > r.t0, "tables": q,
                                  "files": files[:80], "nfiles": len(files)})


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["cold", "idle", "running", "latency", "proof"])
    ap.add_argument("--app", choices=["slint", "tauri"], default="slint")
    ap.add_argument("--run-dir", type=Path, default=DEFAULT_RUN)
    ap.add_argument("--raw", type=Path, default=None, help="where the JSON lines go (default <run-dir>/raw)")
    ap.add_argument("--store", default="sample")
    ap.add_argument("--n", type=int, default=10)
    ap.add_argument("--rounds", type=int, default=20)
    ap.add_argument("--cpu-secs", type=int, default=60)
    ap.add_argument("--rss-at", type=int, default=300)
    ap.add_argument("--rss-runs", type=int, default=3)
    ap.add_argument("--footprint-timeout", type=float, default=4)
    args = ap.parse_args()
    global FOOTPRINT_TIMEOUT
    FOOTPRINT_TIMEOUT = args.footprint_timeout
    (args.run_dir / "work").mkdir(parents=True, exist_ok=True)
    args.raw = args.raw or args.run_dir / "raw"
    args.raw.mkdir(parents=True, exist_ok=True)
    # Ends by itself when this process does.
    subprocess.Popen(["caffeinate", "-d", "-i", "-w", str(os.getpid())], stdout=subprocess.DEVNULL,
                     stderr=subprocess.DEVNULL)
    {"cold": cmd_cold, "idle": cmd_idle, "running": cmd_running, "latency": cmd_latency, "proof": cmd_proof}[args.cmd](args)


if __name__ == "__main__":
    main()
