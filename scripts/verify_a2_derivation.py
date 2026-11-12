#!/usr/bin/env python3
# A2 derivation acceptance check: independent re-derivation of a fixed
# 20-entry sample (spread across the derivation log), plus pool-wide
# invariants. Read-only; exit non-zero on any violation.

import json
import os
import sys

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

here = os.path.dirname(os.path.abspath(__file__))
repo = os.path.dirname(here)
pool = os.path.join(repo, "data", "migration", "law-pool")

CAT = {"math": "math", "physics": "physical", "chemistry": "chemistry",
       "mathematical_physics": "physical", "astrophysics": "physical",
       "earth_science": "physical", "biophysics": "physical",
       "computational_chemistry": "chemistry", "information_theory": "math"}
CROSS = {"computational_chemistry": "chemistry", "geoscience": "physical",
         "astronomy": "physical", "information_theory": "math"}
FIELD = {"physics": "physical", "mathematics": "math",
         "chemistry": "chemistry", "computer_science": "math"}
CATALOG = {"physical", "engineering", "economic", "math", "chemistry"}

rows = {r["id"]: r for r in (json.loads(l) for l in open(os.path.join(pool, "normalized-law-pool.jsonl"), encoding="utf-8") if l.strip())}
log = [json.loads(l) for l in open(os.path.join(pool, "a2-derivation-log.jsonl"), encoding="utf-8") if l.strip()]
prov = {}
prov_by_before = {}
for line in open(os.path.join(pool, "provenance-law-pool.jsonl"), encoding="utf-8"):
    if line.strip():
        p = json.loads(line)
        prov[p["id"]] = p
        if p.get("id_before"):
            prov_by_before[p["id_before"]] = p

def get_prov(pid):
    if pid in prov:
        return prov[pid]
    base = pid.rsplit("dup", 1)[0].rstrip("_") if "dup" in pid else pid
    if base in prov_by_before:
        return prov_by_before[base]
    for ib, p in prov_by_before.items():
        if pid.startswith(ib):
            return p
    raise KeyError(pid)

inputs = {}
for line in open(os.path.join(pool, "input-law-pool.jsonl"), encoding="utf-8"):
    if line.strip():
        rec = json.loads(line)
        inputs[(rec["_src"]["batch"], rec["_src"]["key"])] = rec

errors = []

# --- invariant 1: every domain in catalog ids ---
bad = [r["id"] for r in rows.values() if r["domain"] not in CATALOG]
if bad:
    errors.append(f"domain outside catalog: {bad[:5]}")

# --- invariant 2: no domain/tier/solvable gaps ---
for r in rows.values():
    if not r["domain"]:
        errors.append(f"{r['id']}: domain empty")
    if r["tier"] not in ("light", "full"):
        errors.append(f"{r['id']}: tier={r.get('tier')!r}")
    s = r.get("solvable")
    if not (isinstance(s, list) and all(isinstance(x, str) for x in s)):
        errors.append(f"{r['id']}: solvable={s!r}")

# --- invariant 3: derivation log complete (one entry per derived id) ---
derived_ids = {e["id"] for e in log}
if len(derived_ids) != len(log):
    errors.append("derivation log has duplicate ids")
for pid in derived_ids:
    if pid not in rows:
        errors.append(f"log id not in pool: {pid}")

# --- sample re-derivation: 20 entries spread across the log ---
step = max(1, len(log) // 20)
sample = log[::step][:20]
for e in sample:
    pid = e["id"]
    r = rows[pid]
    p = get_prov(pid)
    batch = e["batch"]
    rec = inputs.get((batch, p["src"]["key"]))
    if rec is None:
        errors.append(f"{pid}: input unresolvable")
        continue
    if r["domain"] in ("chemistry", "math", "physical") and e["batch"] == "C":
        # C row: re-derive from provenance field/subdomain composite
        comp = e["applied"][0]["source"]["composite_domain"]
        field, sub = [x.strip() for x in comp.split("/")]
        want_domain = FIELD[field]
        if r["domain"] != want_domain:
            errors.append(f"{pid}: C domain {r['domain']} != {want_domain}")
        if r["metadata"].get("subdomain") != sub:
            errors.append(f"{pid}: C subdomain {r['metadata'].get('subdomain')!r} != {sub!r}")
    else:
        call = rec.get("call") or {}
        rule = rec.get("rule") or {}
        rmeta = rule.get("metadata") or {}
        if call:
            cat, sub, out = call.get("category"), call.get("subcategory"), call.get("output")
        else:
            cat, sub, out = rmeta.get("category"), rmeta.get("subcategory"), (rule.get("domain") or {}).get("output")
        want_domain = CROSS[sub] if cat == "cross" else CAT[cat]
        if r["domain"] != want_domain:
            errors.append(f"{pid}: AB domain {r['domain']} != {want_domain}")
        applied_rules = {a["rule"] for a in e["applied"]}
        if "AB-D2" in applied_rules and r["tier"] != "light":
            errors.append(f"{pid}: tier {r['tier']} != light")
        if "AB-D3" in applied_rules:
            want_solvable = [out] if isinstance(out, str) and out.strip() else []
            if r["solvable"] != want_solvable:
                errors.append(f"{pid}: solvable {r['solvable']} != {want_solvable}")

# --- report ---
print(f"pool={len(rows)}  log={len(log)}  sample={len(sample)}")
if errors:
    print("VIOLATIONS:")
    for e in errors[:20]:
        print(" -", e)
    sys.exit(1)
print("ACCEPTANCE PASS: invariants + 20-entry independent re-derivation all consistent")
