#!/usr/bin/env python3
"""Eval harness for rsc: run a fixed question set and diff the results across commits.

This spends ChatGPT plan usage (every question is a full research run), so it only runs by hand and never in CI.

    python3 evals/eval.py run [--rsc PATH] [--only ID,ID] [--label TEXT] [--verify] [--timeout SECS]
    python3 evals/eval.py diff [OLD NEW]
"""

import argparse
import datetime
import json
import os
import signal
import statistics
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EVALS = ROOT / "evals"
RESULTS = EVALS / "results"


def git(*args):
    try:
        r = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True, check=True)
        return r.stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def die(msg, code=2):
    print(msg, file=sys.stderr)
    sys.exit(code)


def write_atomic(path, data):
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(json.dumps(data, indent=2) + "\n")
    os.replace(tmp, path)


def entry(q, code, j):
    """One result row from `rsc run --json` output `j`."""
    base = {"id": q["id"], "type": q["type"], "exit_code": code}
    if j is None:
        return {**base, "error": "no JSON output"}
    c = j.get("citations", {})
    return {
        **base,
        "status": j.get("status"),
        "research_id": j.get("id"),
        "elapsed_secs": j.get("elapsed_secs"),
        "tokens": j.get("tokens"),
        "cost_usd": j.get("cost_usd"),
        "cost_partial": j.get("cost_partial"),
        "budget_hit": j.get("budget_hit"),
        "agents": len(j.get("agents", [])),
        "agents_failed": j.get("agents_failed"),
        "gap_agents": j.get("gap_agents"),
        "report_words": j.get("report_words"),
        "sources_total": j.get("sources", {}).get("total"),
        "cited_sources": c.get("cited_sources"),
        "invalid_numbers": c.get("invalid_numbers"),
        "links": c.get("links"),
        "model_supplied": c.get("model_supplied"),
        "verdicts": c.get("verdicts"),
        "config": j.get("config"),
    }


def run_question(rsc, q, verify, timeout, current):
    cmd = [rsc, "run", q["question"], "--yes", "--no-clarify", "--json"] + (["--verify"] if verify else [])
    proc = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, text=True)
    current[0] = proc
    try:
        try:
            out, _ = proc.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            proc.send_signal(signal.SIGINT)
            try:
                out, _ = proc.communicate(timeout=30)
            except subprocess.TimeoutExpired:
                proc.kill()
                out, _ = proc.communicate()
    finally:
        current[0] = None
    try:
        j = json.loads(out)
    except (json.JSONDecodeError, TypeError):
        j = None
    return entry(q, proc.returncode, j)


def cmd_run(a):
    questions = json.loads((EVALS / "questions.json").read_text())
    if a.only:
        want = [x for x in a.only.split(",") if x]
        known = {q["id"] for q in questions}
        for x in want:
            if x not in known:
                die(f"Unknown question id: {x}")
        questions = [q for q in questions if q["id"] in want]
    rsc = Path(a.rsc) if a.rsc else ROOT / "target" / "release" / "rsc"
    if not rsc.exists():
        die("rsc binary not found: build with cargo build --release or pass --rsc")
    sha = git("rev-parse", "--short", "HEAD")
    now = datetime.datetime.now(datetime.timezone.utc)
    name = f"{now.strftime('%Y%m%d-%H%M%S')}-{sha or 'unknown'}" + (f"-{a.label}" if a.label else "")
    RESULTS.mkdir(parents=True, exist_ok=True)
    path = RESULTS / f"{name}.json"
    try:
        version = subprocess.run([str(rsc), "--version"], capture_output=True, text=True).stdout.strip()
    except OSError:
        version = ""
    data = {
        "commit": sha or "unknown",
        "dirty": bool(git("status", "--porcelain")),
        "rsc_version": version,
        "started_at": now.isoformat(),
        "label": a.label,
        "verify": a.verify,
        "results": [],
    }
    stop = [False]
    current = [None]

    def on_sigint(signum, frame):
        # A terminal Ctrl+C reaches rsc too; forwarding covers a signal sent to the harness alone.
        stop[0] = True
        if current[0] is not None and current[0].poll() is None:
            current[0].send_signal(signal.SIGINT)

    signal.signal(signal.SIGINT, on_sigint)
    for q in questions:
        e = run_question(str(rsc), q, a.verify, a.timeout, current)
        data["results"].append(e)
        write_atomic(path, data)
        cost = e.get("cost_usd") or 0.0
        print(f"{q['id']}: exit {e['exit_code']}, {e.get('cited_sources')} cited, {e.get('invalid_numbers')} invalid, ${cost:.2f}")
        if stop[0]:
            print(f"interrupted; partial results in {path}", file=sys.stderr)
            sys.exit(130)
    print(f"wrote {path}")


def mean(xs):
    xs = [x for x in xs if x is not None]
    return statistics.fmean(xs) if xs else None


def total(xs):
    xs = [x for x in xs if x is not None]
    return sum(xs) if xs else None


def get(e, *keys):
    for k in keys:
        e = e.get(k) if isinstance(e, dict) else None
    return e


def support_rate(rows):
    sup = sum(get(r, "verdicts", "supported") or 0 for r in rows)
    allv = sum(sum((r.get("verdicts") or {}).values()) for r in rows)
    return sup / allv if allv else None


def fmt(v, kind="n"):
    if v is None:
        return "—"
    if kind == "pct":
        return f"{v * 100:.0f}%"
    if kind == "usd":
        return f"${v:.2f}"
    if isinstance(v, float) and not v.is_integer():
        return f"{v:.1f}"
    return str(int(v)) if isinstance(v, float) else str(v)


def delta(old, new, kind="n"):
    if old is None or new is None:
        return "—"
    d = new - old
    if kind == "pct":
        return f"{d * 100:+.0f} pp"
    if kind == "usd":
        return f"{d:+.2f}"
    return f"{d:+.1f}" if isinstance(d, float) and not float(d).is_integer() else f"{d:+.0f}"


def cmd_diff(a):
    if a.files:
        if len(a.files) != 2:
            die("diff takes either no files or OLD NEW")
        old_p, new_p = (Path(f) for f in a.files)
    else:
        files = sorted(RESULTS.glob("*.json")) if RESULTS.exists() else []
        if len(files) < 2:
            die("Need two results files")
        old_p, new_p = files[-2:]
    old, new = (json.loads(p.read_text()) for p in (old_p, new_p))
    print(f"old: {old_p.name} ({old.get('commit')}{' dirty' if old.get('dirty') else ''})")
    print(f"new: {new_p.name} ({new.get('commit')}{' dirty' if new.get('dirty') else ''})")
    print()
    o_rows = {r["id"]: r for r in old["results"]}
    n_rows = {r["id"]: r for r in new["results"]}
    ids = [i for i in o_rows if i in n_rows]
    O = [o_rows[i] for i in ids]
    N = [n_rows[i] for i in ids]
    done = lambda rows: sum(1 for r in rows if r.get("exit_code") in (0, 2))
    col = lambda rows, *k: [get(r, *k) for r in rows]
    metrics = [
        ("completed", done(O), done(N), "n"),
        ("mean elapsed_secs", mean(col(O, "elapsed_secs")), mean(col(N, "elapsed_secs")), "n"),
        ("mean tokens", mean(col(O, "tokens")), mean(col(N, "tokens")), "n"),
        ("total cost_usd", total(col(O, "cost_usd")), total(col(N, "cost_usd")), "usd"),
        ("agents failed", total(col(O, "agents_failed")), total(col(N, "agents_failed")), "n"),
        ("mean cited sources", mean(col(O, "cited_sources")), mean(col(N, "cited_sources")), "n"),
        ("invalid numbers", total(col(O, "invalid_numbers")), total(col(N, "invalid_numbers")), "n"),
        ("dead links", total(col(O, "links", "dead")), total(col(N, "links", "dead")), "n"),
        ("model-supplied", total(col(O, "model_supplied")), total(col(N, "model_supplied")), "n"),
        ("support rate", support_rate(O), support_rate(N), "pct"),
        ("mean report words", mean(col(O, "report_words")), mean(col(N, "report_words")), "n"),
    ]
    print(f"{len(ids)} questions in both files\n")
    print("| metric | old | new | Δ |")
    print("|---|---|---|---|")
    for name, o, n, kind in metrics:
        print(f"| {name} | {fmt(o, kind)} | {fmt(n, kind)} | {delta(o, n, kind)} |")
    print()
    print("| id | cited | invalid | dead | words | cost |")
    print("|---|---|---|---|---|---|")
    cells = [
        (("cited_sources",), "n"),
        (("invalid_numbers",), "n"),
        (("links", "dead"), "n"),
        (("report_words",), "n"),
        (("cost_usd",), "usd"),
    ]
    for i in ids:
        row = [f"{fmt(get(o_rows[i], *k), kind)}→{fmt(get(n_rows[i], *k), kind)}" for k, kind in cells]
        print(f"| {i} | " + " | ".join(row) + " |")


def main():
    p = argparse.ArgumentParser(description="rsc eval harness (spends plan usage; run by hand)")
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run", help="run the question set and record results")
    r.add_argument("--rsc", help="path to the rsc binary (default: target/release/rsc)")
    r.add_argument("--only", help="comma-separated question ids")
    r.add_argument("--label", help="suffix for the results file name")
    r.add_argument("--verify", action="store_true", help="pass --verify to rsc run")
    r.add_argument("--timeout", type=int, default=3600, help="seconds per question")
    r.set_defaults(fn=cmd_run)
    d = sub.add_parser("diff", help="compare two results files")
    d.add_argument("files", nargs="*", metavar="OLD NEW")
    d.set_defaults(fn=cmd_diff)
    a = p.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
