"""Regrade one run's compiled solutions against the current oracle, through run.py's own grading calls."""
import hashlib, importlib.util, json, pathlib, sys
spec = importlib.util.spec_from_file_location("run", "run.py"); run = importlib.util.module_from_spec(spec); spec.loader.exec_module(run)
run_dir, work = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
oracle_raw = (run.TASKS_DIR / "recovery" / "oracle.rs").read_text()
names = run.oracle_test_names(oracle_raw)
out = []
for line in open(run_dir / "results.jsonl"):
    r = json.loads(line)
    trial_dir = work / r["trial_id"].replace("/", "_") if r["trial_id"] != "crashed" else None
    if not r["compiled"] or trial_dir is None or not trial_dir.is_dir():
        out.append({"trial_id": r["trial_id"], "compiled": r["compiled"], "regraded": False}); continue
    package = next(l.split('"')[1] for l in (trial_dir / "Cargo.toml").read_text().splitlines() if l.startswith("name"))
    (trial_dir / "tests" / "oracle.rs").write_text(oracle_raw.replace(run.ORACLE_CRATE_TOKEN, package))
    test = run.run_timed_cargo(["test", "--locked", "--offline", "--", "--test-threads=1"], trial_dir, work)
    text = (test.stdout or "") + (test.stderr or "")
    oracle = {n: "not_run" for n in names}
    for l in text.splitlines():
        m = run.TEST_LINE.match(l.strip())
        if m and m.group("name") in oracle: oracle[m.group("name")] = "pass" if m.group("status") == "ok" else "fail"
    lib_ok = hashlib.sha256((trial_dir / "lib.rs").read_bytes()).hexdigest() == hashlib.sha256(r["lib_rs"].encode()).hexdigest() if isinstance(r.get("lib_rs"), str) else None
    out.append({"trial_id": r["trial_id"], "model": r["model"], "profile": r["profile"], "trial": r["trial"], "compiled": True, "regraded": True,
                "lib_rs_matches_receipt": lib_ok, "oracle": oracle, "oracle_pass_count": sum(v == "pass" for v in oracle.values()), "oracle_total": len(oracle)})
    print(r["trial_id"], out[-1]["oracle_pass_count"], "/", len(oracle), "lib_matches", lib_ok)
sha = hashlib.sha256(oracle_raw.encode()).hexdigest()
with open(run_dir / "regraded.jsonl", "w") as f:
    for o in out: f.write(json.dumps({**o, "oracle_sha256": sha}) + "\n")
