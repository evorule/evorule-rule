#!/usr/bin/env python3
"""End-to-end proof: law_set dataset remote publication and pull-side loading.

Drives the full governance -> distribution -> assembly chain against real
binaries (no mocks):

  1. rule serve (governance API, SQLite) — create law_set dataset, ingest the
     machine-gate-pass subset of the normalized law pool, transition to
     Active, publish (two-step confirm), and mint a pull-scope API key.
  2. evorule-server (execution side) — --law-sets-remote pulls the published
     dataset snapshot bundle via x-api-key (pull scope) and registers the
     law_set through the same local-path validation (validate_law_set).

Every assertion result and artifact is written to the evidence directory
(default D:/tmp/b265-remote-publish-e2e). Credentials are generated per run,
kept in memory + evidence (local temp dir only), never printed to stdout.

Pure experiment tooling: reads repository artifacts (normalized pool,
domain schemas) read-only; writes nothing into the repository tree.

Usage:
  python scripts/experiments/run_law_pool_remote_publish.py \
      [--rule-serve <exe>] [--server <exe>] [--evidence <dir>]

Prerequisite: both release binaries must be built from source trees whose
governance face knows the law_set dataset kind (rule serve >= the law-hosting
expansion) and whose pull side consumes the snapshot bundle endpoint.
Default binaries: <this repo>/target/release/evorule-rule-serve.exe and
D:/wt-b265-server/target/release/evorule-server.exe.
"""

import argparse
import io
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, os.path.dirname(HERE))  # repo scripts/ (normalize_law_pool)

from normalize_law_pool import check_machine_gate  # noqa: E402

SCHEMA_REF_LAW = "https://evorule.org/schemas/domain/law_set/law/v1.0.json"
DATASET_ID = "law-pool-e2e"
TENANT = "default"
ADMIN_USER = "admin"
HTTP_TIMEOUT = 30
STARTUP_TIMEOUT = 60

# unary-minus probe mirroring the law-layer negation gate (assembly side):
# a minus is unary when it opens the formula or follows '=', '(', '*', '/', '^'
UNARY_MINUS = re.compile(r"(?:^|[=(*/^])\s*-\s*\S")


def has_negation_construct(formula):
    return bool(UNARY_MINUS.search(formula))

DEFAULT_RULE_SERVE = os.path.join(ROOT, "target", "release", "evorule-rule-serve.exe")
DEFAULT_SERVER = r"D:\wt-b265-server\target\release\evorule-server.exe"
DEFAULT_SERVER_ROOT = r"D:\wt-b265-server"
DEFAULT_EVIDENCE = r"D:\tmp\b265-remote-publish-e2e"


def free_port():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def http(method, url, token=None, api_key=None, body=None, timeout=HTTP_TIMEOUT):
    """JSON HTTP helper. Returns (status, parsed_json_or_raw_text)."""
    data = None
    headers = {}
    if body is not None:
        data = json.dumps(body, ensure_ascii=False).encode("utf-8")
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = "Bearer " + token
    if api_key:
        headers["x-api-key"] = api_key
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read().decode("utf-8")
            return resp.status, (json.loads(raw) if raw.strip() else None)
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        try:
            return e.code, json.loads(raw)
        except ValueError:
            return e.code, {"raw": raw}


def wait_health(url, proc, deadline=STARTUP_TIMEOUT):
    t0 = time.time()
    while time.time() - t0 < deadline:
        if proc.poll() is not None:
            raise RuntimeError("process exited early (code %s)" % proc.returncode)
        try:
            status, body = http("GET", url, timeout=3)
            if status == 200 and body and body.get("ok"):
                return time.time() - t0
        except (urllib.error.URLError, OSError, ValueError):
            pass
        time.sleep(0.3)
    raise RuntimeError("health probe timeout: %s" % url)


def start_proc(args, env_extra, log_path, cwd):
    env = dict(os.environ)
    env.update(env_extra)
    log = io.open(log_path, "w", encoding="utf-8", newline="\n")
    proc = subprocess.Popen(
        args, stdout=log, stderr=subprocess.STDOUT, cwd=cwd, env=env
    )
    return proc, log


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rule-serve", default=DEFAULT_RULE_SERVE)
    ap.add_argument("--server", default=DEFAULT_SERVER)
    ap.add_argument("--server-root", default=DEFAULT_SERVER_ROOT)
    ap.add_argument("--evidence", default=DEFAULT_EVIDENCE)
    args = ap.parse_args()

    ev = args.evidence
    rule_db_dir = os.path.join(ev, "rule-db")
    server_dir = os.path.join(ev, "server-data")
    for d in (rule_db_dir, server_dir):
        if os.path.isdir(d):
            shutil.rmtree(d)
        os.makedirs(d)
    for sub in ("domain_schemas",):
        os.makedirs(os.path.join(rule_db_dir, sub))
    # server runtime cwd needs ./resources/server_eval.json + ./rules + ./data
    os.makedirs(os.path.join(server_dir, "resources"))
    os.makedirs(os.path.join(server_dir, "rules"))
    os.makedirs(os.path.join(server_dir, "data", "memory"))
    shutil.copy2(os.path.join(args.server_root, "resources", "server_eval.json"),
                 os.path.join(server_dir, "resources", "server_eval.json"))
    for name in ("law_set-v1.0.json", "law_set-constant-v1.0.json"):
        src = os.path.join(ROOT, "data", "domain_schemas", name)
        shutil.copy2(src, os.path.join(rule_db_dir, "domain_schemas", name))

    # --- remote-loadable subset of the normalized pool ----------------------
    # gate 1: rule-side machine gate (nine-element structural check)
    # gate 2: law-layer negation gate mirror (unary minus) — deterministic
    #         exclusion, content is never rewritten (same discipline as R13)
    pool_path = os.path.join(ROOT, "data", "migration", "law-pool", "normalized-law-pool.jsonl")
    machine_pass, laws = [], []
    with io.open(pool_path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            law = json.loads(line)
            if check_machine_gate(law):  # returns the error list; empty = pass
                continue
            machine_pass.append(law)
            if not has_negation_construct(law.get("formula", "")):
                laws.append(law)
    excluded_negation = sorted(l["id"] for l in machine_pass
                               if has_negation_construct(l.get("formula", "")))
    assert len(machine_pass) == 13, (
        "expected 13 machine-gate-pass laws, got %d" % len(machine_pass))
    law_ids = sorted(l["id"] for l in laws)

    # --- 1. governance side: rule serve ------------------------------------
    rule_port = free_port()
    rule_base = "http://127.0.0.1:%d" % rule_port
    rule_db = os.path.join(rule_db_dir, "rule.db")
    secret = "e2e-%d" % int(time.time() * 1000)
    admin_pw = "e2e-password-%d" % int(time.time() * 1000)
    rule_proc, rule_log = start_proc(
        [args.rule_serve, "--db", rule_db, "--port", str(rule_port),
         "--secret", secret, "--tenant", TENANT,
         "--admin-user", ADMIN_USER, "--admin-password", admin_pw],
        {}, os.path.join(ev, "rule-serve.log"), ev,
    )
    evidence = {"rule_serve": {"exe": args.rule_serve, "port": rule_port, "db": rule_db}}

    server_proc = server_log = None
    try:
        elapsed = wait_health(rule_base + "/v1/health", rule_proc)
        evidence["rule_serve"]["startup_seconds"] = round(elapsed, 2)

        status, body = http("POST", rule_base + "/v1/auth/login",
                            body={"tenant_id": TENANT, "username": ADMIN_USER,
                                  "password": admin_pw})
        assert status == 200, "login failed: %s %s" % (status, body)
        token = body["access_token"]
        evidence["login"] = status

        status, body = http("POST", rule_base + "/v1/api_keys",
                            token=token, body={"name": "law-pool-e2e-pull"})
        assert status == 201, "api key mint failed: %s %s" % (status, body)
        pull_key = body["key"]
        assert body.get("scope") == "pull", "expected pull scope: %s" % body.get("scope")
        evidence["api_key"] = {"scope": body["scope"], "prefix": pull_key[:8] + "..."}

        status, body = http("POST", rule_base + "/v1/datasets", token=token, body={
            "dataset_id": DATASET_ID,
            "name": "Law pool E2E slice",
            "description": "Machine-gate-pass subset of the normalized law pool "
                           "(remote publication proof)",
            "dataset_kind": "law_set",
            "version_selection": {"mode": "pinned"},
        })
        assert status == 201, "dataset create failed: %s %s" % (status, body)
        assert body.get("dataset_kind") == "law_set", body.get("dataset_kind")
        evidence["dataset_create"] = {"status": status, "kind": body.get("dataset_kind")}

        entry_results = []
        for law in laws:
            status, body = http(
                "POST", rule_base + "/v1/datasets/%s/entries" % DATASET_ID,
                token=token,
                body={"entry_id": law["id"], "version": 1, "payload": law,
                      "schema_ref": SCHEMA_REF_LAW},
            )
            entry_results.append({"entry_id": law["id"], "status": status})
            assert status == 201, "entry %s ingest failed: %s %s" % (law["id"], status, body)
        evidence["entries_ingest"] = {"count": len(entry_results), "all_201": True}

        for target in ("candidate", "active"):  # Draft→Candidate→Active 两跳
            status, body = http("PATCH",
                                rule_base + "/v1/datasets/%s/lifecycle" % DATASET_ID,
                                token=token, body={"to": target})
            assert status == 200, "lifecycle->%s failed: %s %s" % (target, status, body)
        evidence["lifecycle_active"] = 200

        status, body = http("POST", rule_base + "/v1/datasets/%s/publish" % DATASET_ID,
                            token=token,
                            body={"confirm": True, "reason": "law pool remote publication proof"})
        assert status == 200, "publish failed: %s %s" % (status, body)
        lifecycle = body.get("lifecycle") or {}
        assert (lifecycle.get("status") or "").lower() == "published", lifecycle
        evidence["publish"] = {"status": status, "lifecycle": lifecycle.get("status"),
                               "published_as": lifecycle.get("state_history", [{}])[-1]
                                               .get("published_as")}

        # bundle face (the endpoint the server will pull): x-api-key, pull scope
        status, bundle = http("GET", rule_base + "/v1/datasets/%s/bundle" % DATASET_ID,
                              api_key=pull_key)
        assert status == 200, "bundle pull failed: %s" % status
        entries = bundle.get("entries") or []
        assert len(entries) == len(laws), (
            "bundle entries: %d != laws %d" % (len(entries), len(laws)))
        assert all(e.get("rule_body") for e in entries), "bundle entry payload missing"
        evidence["bundle_pull"] = {"status": status, "entries": len(entries),
                                   "version": (bundle.get("dataset") or {})
                                              .get("versioning", {}).get("current")}

        # --- 2. execution side: evorule-server remote assembly --------------
        server_port = free_port()
        server_proc, server_log = start_proc(
            [args.server,
             "--addr", "127.0.0.1:%d" % server_port,
             "--profile", "development", "--insecure-serve",
             "--db-path", os.path.join(server_dir, "evorule.db"),
             "--law-sets-remote", rule_base,
             "--law-sets-remote-datasets", DATASET_ID],
            {"EVORULE_RULE_SERVE_API_KEY": pull_key},
            os.path.join(ev, "server.log"), server_dir,
        )
        # remote assembly happens during startup: poll the log until the
        # registration line (success or rejection) appears
        deadline = time.time() + 30
        log_text = ""
        while time.time() < deadline:
            assert server_proc.poll() is None, (
                "server exited during remote assembly (code %s)" % server_proc.returncode)
            time.sleep(0.5)
            log_text = io.open(os.path.join(ev, "server.log"), encoding="utf-8",
                               errors="replace").read()
            if "远端登记" in log_text:
                break
        loaded_line = [ln for ln in log_text.splitlines()
                       if DATASET_ID in ln and "valid=true" in ln]
        rejected_line = [ln for ln in log_text.splitlines()
                         if DATASET_ID in ln and ("拒收" in ln or "reject" in ln.lower())]
        summary_line = [ln for ln in log_text.splitlines()
                        if "远端登记完成" in ln]
        evidence["server_remote_assembly"] = {
            "alive_after_assembly": server_proc.poll() is None,
            "port": server_port,
            "log_lines_loaded": loaded_line[:10],
            "log_lines_rejected": rejected_line[:10],
            "log_lines_summary": summary_line[:5],
        }
        # hard assertions: the dataset registered valid, never rejected
        assert not rejected_line, "server log shows rejection: %s" % rejected_line[:3]
        assert loaded_line, "server log never shows a valid remote registration: %s" % (
            log_text[-2000:])

        evidence["verdict"] = "PASS"
        evidence["law_ids"] = law_ids
        evidence["excluded_negation_constructs"] = excluded_negation
    finally:
        for proc in (server_proc, rule_proc):
            if proc is not None and proc.poll() is None:
                proc.kill()
        for log in (server_log, rule_log):
            if log is not None:
                try:
                    log.close()
                except OSError:
                    pass

    out_path = os.path.join(ev, "report.json")
    with io.open(out_path, "w", encoding="utf-8", newline="\n") as f:
        f.write(json.dumps(evidence, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    print("E2E PASS — evidence: %s" % out_path)
    print(json.dumps({k: evidence[k] for k in
                      ("dataset_create", "publish", "bundle_pull", "server_remote_assembly")},
                     ensure_ascii=False, indent=2)[:1200])


if __name__ == "__main__":
    sys.exit(main())
