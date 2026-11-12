#!/usr/bin/env python3
# A2 metadata derivation pipeline (deterministic, double-run reproducible).
#
# Determinism statement:
#   - No timestamps, no randomness, no environment-dependent behavior.
#   - Inputs: data/migration/law-pool/{normalized-law-pool.jsonl,
#     provenance-law-pool.jsonl, input-law-pool.jsonl} only.
#   - All output JSON uses sort_keys + LF + trailing newline (A1 convention).
#   - Pre-flight asserts the A1 gate baseline before mutating anything
#     (fail-visible on mismatch, prevents accidental double-apply).
#
# Scope (design doc: A2 legacy backfill):
#   - Fill domain/tier/solvable gaps (277/277/202) for A/B-batch products via
#     two closed derivation rule tables (AB: category+subcategory assembly,
#     C: field+subdomain combination).
#   - Normalize the whole pool's domain vocabulary to domain-catalog ids
#     (project decision D-1/D-2), moving the sub-domain component into
#     metadata.subdomain (side channel; law top level is forbidden by A3).
#   - tier grant: A/B batch = "light" default (L2 ruling; "full" awaits the
#     service-family channel backfill, hook = metadata.intake.tier_grant_basis).
#   - Out of scope: derivation (307, later batch), si_units (233, dimension
#     batch, human-review only), symbols<2 (13, registered legacy gap).

import hashlib
import json
import os
import sys

DOMAIN_CATALOG_IDS = {"physical", "engineering", "economic", "math", "chemistry"}

# Closed category -> domain-catalog id map (approved D-2; 'cross' category is
# resolved by subcategory via CROSS_SUBCATEGORY_MAP, closed 4-value extension).
CATEGORY_MAP = {
    "math": "math",
    "physics": "physical",
    "chemistry": "chemistry",
    "mathematical_physics": "physical",
    "astrophysics": "physical",
    "earth_science": "physical",
    "biophysics": "physical",
    "computational_chemistry": "chemistry",
    "information_theory": "math",
}

# 'cross' category entries: subcategory -> domain-catalog id (closed, 4 values).
CROSS_SUBCATEGORY_MAP = {
    "computational_chemistry": "chemistry",
    "geoscience": "physical",
    "astronomy": "physical",
    "information_theory": "math",
}

# Closed field -> domain-catalog id map (C batch composite normalization, D-1).
FIELD_MAP = {
    "physics": "physical",
    "mathematics": "math",
    "chemistry": "chemistry",
    "computer_science": "math",
}

# A1 gate baseline that must reproduce exactly before any mutation.
A1_BASELINE = {
    "machine_gate_pass": 13,
    "derivation": 307,
    "domain": 277,
    "solvable": 202,
    "symbols": 13,
    "tier": 277,
}

RULES_DOC = {
    "title": "A2 metadata derivation rule tables (two tables, never mixed)",
    "scope": "legacy pool domain/tier/solvable backfill + domain vocabulary normalization",
    "tables": {
        "AB": {
            "applies_to": "products whose source batch is A or B (domain empty)",
            "rules": [
                {"id": "AB-D1", "def": "domain = CATEGORY_MAP[category]; category read from call.category (B batch) or rule.metadata.category (A batch) via provenance.src back-link", "source_columns": ["call.category", "rule.metadata.category"]},
                {"id": "AB-D2", "def": "tier = 'light' (default grant; full awaits service-family channel backfill, hook metadata.intake.tier_grant_basis)", "source_columns": []},
                {"id": "AB-D3", "def": "solvable = [output] when output non-empty else []; output read from call.output (B batch) or rule.domain.output (A batch)", "source_columns": ["call.output", "rule.domain.output"]},
                {"id": "AB-D4", "def": "metadata.subdomain = subcategory when subcategory non-empty (side-channel preservation)", "source_columns": ["call.subcategory", "rule.metadata.subcategory"]},
            ],
        },
        "C": {
            "applies_to": "products whose source batch is C (composite domain 'field / subdomain' filled by A1)",
            "rules": [
                {"id": "C-D1", "def": "domain = FIELD_MAP[field]; the composite 'field / subdomain' value is split, field mapped to domain-catalog id", "source_columns": ["domain (composite)"]},
                {"id": "C-D2", "def": "tier untouched (A1 legacy values: computability partial->light x118, full->full x14)", "source_columns": []},
                {"id": "C-D3", "def": "solvable: fill [] when missing (source eval_output/provenance output empty); A1 legacy non-empty values untouched", "source_columns": ["row.eval_output", "output (provenance)"]},
                {"id": "C-D4", "def": "metadata.subdomain = composite sub-domain component when absent (side-channel preservation)", "source_columns": ["domain (composite)"]},
            ],
        },
    },
    "category_map": CATEGORY_MAP,
    "cross_subcategory_map": CROSS_SUBCATEGORY_MAP,
    "field_map": FIELD_MAP,
    "domain_catalog_ids": sorted(DOMAIN_CATALOG_IDS),
    "notes": "closed maps only; no per-entry special cases; every derived value is traceable to source columns recorded in a2-derivation-log.jsonl",
}


def dump_json(path, obj):
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(json.dumps(obj, ensure_ascii=False, sort_keys=True, indent=2) + "\n")


def dump_jsonl(path, rows):
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        for r in rows:
            fh.write(json.dumps(r, ensure_ascii=False, sort_keys=True) + "\n")


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def write_manifest(pool_dir):
    names = [
        "a2-derivation-log.jsonl",
        "a2-derivation-rules.json",
        "excluded-non-identity-forms.jsonl",
        "input-law-pool.jsonl",
        "normalization-rules.json",
        "normalized-law-pool.jsonl",
        "provenance-law-pool.jsonl",
        "structural-report.json",
        "x3-split-map.json",
        "x5-mult-map.json",
    ]
    lines = []
    for name in names:
        digest = sha256_file(os.path.join(pool_dir, name))
        lines.append(f"{digest}  {name}")
    combined = hashlib.sha256("\n".join(lines).encode("utf-8")).hexdigest()
    lines.append(f"{combined}  COMBINED")
    with open(os.path.join(pool_dir, "manifest.sha256"), "w", encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(lines) + "\n")


def is_str_array(v):
    return isinstance(v, list) and all(isinstance(x, str) for x in v)


def gate_counters(rows):
    """Recompute the five A1 machine-gate fail reasons + pass count."""
    fails = {"derivation": 0, "domain": 0, "solvable": 0, "symbols": 0, "tier": 0}
    passed = 0
    for r in rows:
        ok = True
        if not is_str_array(r.get("derivation")):
            fails["derivation"] += 1
            ok = False
        if not r.get("domain"):
            fails["domain"] += 1
            ok = False
        if not is_str_array(r.get("solvable")):
            fails["solvable"] += 1
            ok = False
        sym = r.get("symbols")
        if not isinstance(sym, dict) or len(sym) < 2:
            fails["symbols"] += 1
            ok = False
        if r.get("tier") not in ("light", "full"):
            fails["tier"] += 1
            ok = False
        if ok:
            passed += 1
    return passed, fails


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    repo = os.path.dirname(here)
    pool = os.path.join(repo, "data", "migration", "law-pool")

    rows = [json.loads(l) for l in open(os.path.join(pool, "normalized-law-pool.jsonl"), encoding="utf-8") if l.strip()]
    prov = {}
    prov_by_before = {}
    for line in open(os.path.join(pool, "provenance-law-pool.jsonl"), encoding="utf-8"):
        if line.strip():
            p = json.loads(line)
            prov[p["id"]] = p
            if p.get("id_before"):
                prov_by_before[p["id_before"]] = p

    inputs = {}
    for line in open(os.path.join(pool, "input-law-pool.jsonl"), encoding="utf-8"):
        if line.strip():
            rec = json.loads(line)
            inputs[(rec["_src"]["batch"], rec["_src"]["key"])] = rec

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

    # ---- pre-flight: A1 gate baseline must reproduce exactly ----
    passed, fails = gate_counters(rows)
    observed = {"machine_gate_pass": passed, **fails}
    if observed != A1_BASELINE:
        print("PRE-FLIGHT GATE BASELINE MISMATCH (refusing to run on a non-A1 pool):")
        print(f"  observed={observed}")
        print(f"  expected={A1_BASELINE}")
        return 2
    print(f"pre-flight OK: {observed}")

    by_id = {r["id"]: r for r in rows}
    log = []
    counters = {"AB-D1": 0, "AB-D2": 0, "AB-D3": 0, "AB-D4": 0,
                "C-D1": 0, "C-D2": 0, "C-D3": 0, "C-D4": 0}

    for r in rows:
        pid = r["id"]
        p = get_prov(pid)
        batch = (p.get("src") or {}).get("batch")
        entry_log = {"id": pid, "batch": batch, "applied": []}
        meta = r.setdefault("metadata", {})

        if not r.get("domain"):
            # ---- AB table ----
            rec = inputs.get((batch, p["src"]["key"]))
            if rec is None:
                print(f"FAIL-VISIBLE: input record not resolvable for {pid} ({batch}/{p['src']['key']})")
                return 2
            call = rec.get("call") or {}
            rule = rec.get("rule") or {}
            rmeta = rule.get("metadata") or {}
            if call:
                category = call.get("category")
                subcategory = call.get("subcategory")
                output = call.get("output")
            elif rule:
                category = rmeta.get("category")
                subcategory = rmeta.get("subcategory")
                output = (rule.get("domain") or {}).get("output")
            else:
                print(f"FAIL-VISIBLE: unknown input shape for {pid}")
                return 2
            if category == "cross":
                if subcategory not in CROSS_SUBCATEGORY_MAP:
                    print(f"FAIL-VISIBLE: cross subcategory {subcategory!r} not in closed map for {pid}")
                    return 2
                mapped_domain = CROSS_SUBCATEGORY_MAP[subcategory]
            elif category in CATEGORY_MAP:
                mapped_domain = CATEGORY_MAP[category]
            else:
                print(f"FAIL-VISIBLE: category {category!r} not in closed map for {pid}")
                return 2
            r["domain"] = mapped_domain
            entry_log["applied"].append({"rule": "AB-D1", "source": {"category": category, "subcategory": subcategory}, "derived": {"domain": r["domain"]}})
            counters["AB-D1"] += 1

            if r.get("tier") not in ("light", "full"):
                r["tier"] = "light"
                entry_log["applied"].append({"rule": "AB-D2", "source": {}, "derived": {"tier": "light"}})
                counters["AB-D2"] += 1

            if not is_str_array(r.get("solvable")):
                solvable = [output] if isinstance(output, str) and output.strip() else []
                r["solvable"] = solvable
                entry_log["applied"].append({"rule": "AB-D3", "source": {"output": output}, "derived": {"solvable": solvable}})
                counters["AB-D3"] += 1

            if subcategory and "subdomain" not in meta:
                meta["subdomain"] = subcategory
                entry_log["applied"].append({"rule": "AB-D4", "source": {"subcategory": subcategory}, "derived": {"metadata.subdomain": subcategory}})
                counters["AB-D4"] += 1

        elif r["domain"] not in DOMAIN_CATALOG_IDS:
            # ---- C table: composite vocabulary normalization ----
            parts = r["domain"].split("/")
            if len(parts) != 2:
                print(f"FAIL-VISIBLE: unexpected composite domain {r['domain']!r} for {pid}")
                return 2
            field = parts[0].strip()
            subdomain = parts[1].strip()
            if field not in FIELD_MAP:
                print(f"FAIL-VISIBLE: field {field!r} not in closed map for {pid}")
                return 2
            new_domain = FIELD_MAP[field]
            entry_log["applied"].append({"rule": "C-D1", "source": {"composite_domain": r["domain"]}, "derived": {"domain": new_domain}})
            counters["C-D1"] += 1
            r["domain"] = new_domain
            if subdomain and "subdomain" not in meta:
                meta["subdomain"] = subdomain
                entry_log["applied"].append({"rule": "C-D4", "source": {"composite_domain": f"{field} / {subdomain}"}, "derived": {"metadata.subdomain": subdomain}})
                counters["C-D4"] += 1

            if not is_str_array(r.get("solvable")):
                prov_output = p.get("output")
                solvable = [prov_output] if isinstance(prov_output, str) and prov_output.strip() else []
                r["solvable"] = solvable
                entry_log["applied"].append({"rule": "C-D3", "source": {"output": prov_output}, "derived": {"solvable": solvable}})
                counters["C-D3"] += 1

        if entry_log["applied"]:
            log.append(entry_log)

    # ---- post-flight: gaps must be zero ----
    passed2, fails2 = gate_counters(rows)
    if fails2["domain"] or fails2["tier"] or fails2["solvable"]:
        print("POST-FLIGHT VIOLATION: domain/tier/solvable gaps not zero:", fails2)
        return 2
    bad_vocab = [r["id"] for r in rows if r["domain"] not in DOMAIN_CATALOG_IDS]
    if bad_vocab:
        print("POST-FLIGHT VIOLATION: domain outside catalog ids:", bad_vocab[:10])
        return 2
    print(f"post-flight OK: gate pass {passed2} (was {passed}); remaining fails: {fails2}")

    # ---- write artifacts ----
    dump_jsonl(os.path.join(pool, "normalized-law-pool.jsonl"), rows)
    dump_json(os.path.join(pool, "a2-derivation-rules.json"), RULES_DOC)
    dump_jsonl(os.path.join(pool, "a2-derivation-log.jsonl"), log)

    report_path = os.path.join(pool, "structural-report.json")
    report = json.load(open(report_path, encoding="utf-8"))
    report["gates"]["machine_gate_pass"] = passed2
    report["machine_gate_fail_reasons"] = {
        "derivation missing or not an array of strings": fails2["derivation"],
        "domain missing or empty": fails2["domain"],
        "solvable missing or not an array of strings": fails2["solvable"],
        "symbols must have >= 2 entries": fails2["symbols"],
        "tier missing or not light|full": fails2["tier"],
    }
    kg = report.get("known_gaps", {})
    kg["domain_missing"] = fails2["domain"]
    kg["detail"] = [d for d in kg.get("detail", []) if "domain" not in str(d.get("gap", "")).lower()]
    kg["a2_note"] = "domain/tier/solvable gaps backfilled and vocabulary normalized to domain-catalog ids; sub-domain moved to metadata.subdomain; see a2-derivation-rules.json + a2-derivation-log.jsonl"
    report["known_gaps"] = kg
    report["a2_metadata_derivation"] = {
        "entries_derived": len(log),
        "counters": counters,
        "gate_pass_before": passed,
        "gate_pass_after": passed2,
        "fail_reasons_after": fails2,
    }
    dump_json(report_path, report)
    write_manifest(pool)
    print("artifacts written + manifest re-anchored")
    return 0


if __name__ == "__main__":
    sys.exit(main())
