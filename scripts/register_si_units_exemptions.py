#!/usr/bin/env python3
"""Register the SI-unit exemption list for three non-physical entries.

Per the human adjudication of 2026-11-12, three entries whose formulas are
pure logical/set-theoretic statements (a fixed-point theorem and two
set-theory axioms) are exempted from SI-dimension review entirely: their
formulas quantify over abstract sets and functions, and assigning SI units
to them is meaningless. This pass sidecars the exemption list + rules,
records the exemption in the structural report (moving the three entries
from the "empty, awaiting review" gap into an explicit exemption gap), and
re-anchors the pool manifest. Pool lines themselves are untouched.
"""

import hashlib
import io
import json
import os
import sys

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
POOL = os.path.join(ROOT, "data", "migration", "law-pool")
POOL_FILE = os.path.join(POOL, "normalized-law-pool.jsonl")

EXEMPT = [
    {"id": "math_at_thm_01", "subdomain": "algebraic_topology",
     "title_zh": "不动点定理（Brouwer 型）",
     "rationale": "存在量词陈述（∃x∈D^2: f(x)=x）：量词辖域内为抽象集合与连续映射，无 SI 量纲语义"},
    {"id": "math_set_ax_01", "subdomain": "set_theory",
     "title_zh": "外延公理",
     "rationale": "集合论公理（∀A∀B(A=B ↔ ∀x(x∈A ↔ x∈B))）：纯集合等价陈述，无 SI 量纲语义"},
    {"id": "math_set_ax_08", "subdomain": "set_theory",
     "title_zh": "正则公理",
     "rationale": "集合论公理（∀x(x≠∅ → ∃y∈x(x∩y=∅))）：纯集合陈述，无 SI 量纲语义"},
]


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def dump_json(path, obj):
    with io.open(path, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(json.dumps(obj, ensure_ascii=False, sort_keys=True, indent=2) + "\n")


def dump_jsonl(path, rows):
    with io.open(path, "w", encoding="utf-8", newline="\n") as fh:
        for r in rows:
            fh.write(json.dumps(r, ensure_ascii=False, sort_keys=True) + "\n")


def main():
    list_path = os.path.join(POOL, "si-units-exempt-entries.jsonl")
    rules_path = os.path.join(POOL, "si-units-exempt-rules.json")
    if os.path.exists(list_path) or os.path.exists(rules_path):
        sys.exit("exemption sidecars already exist; rerun refused (idempotency guard)")

    # pool must contain exactly these three entries, all with empty si_units;
    # pool lines themselves stay untouched
    with io.open(POOL_FILE, encoding="utf-8", newline="") as f:
        lines = f.read().split("\n")
    rows = {}
    for line in lines:
        if line.strip():
            r = json.loads(line)
            rows[r["id"]] = r
    assert len(rows) == 409, "unexpected pool size"
    for e in EXEMPT:
        r = rows[e["id"]]
        assert r.get("si_units") == {}, "exempt entry unexpectedly filled: %s" % e["id"]
        assert (r.get("metadata") or {}).get("subdomain") == e["subdomain"], e["id"]

    for e in EXEMPT:
        e["reviewed"] = "2026-11-12"
    dump_jsonl(list_path, EXEMPT)
    dump_json(rules_path, {
        "notation": {
            "dimensionless": '"1" (engine has no "dimensionless" token)',
            "form": "coherent SI symbols or base-unit power products (e.g. m*s^-1)",
        },
        "reviewed": "2026-11-12",
        "rule": "human-adjudicated SI-dimension exemptions: entries whose formulas "
                "are pure logical/quantified/set-theoretic statements carry no SI "
                "dimension semantics; they are exempted from unit review entirely "
                "(si_units intentionally left empty, no candidate derivation, "
                "check_law not applicable), recorded here and in the structural "
                "report gap counter",
    })

    report_path = os.path.join(POOL, "structural-report.json")
    with io.open(report_path, encoding="utf-8") as fh:
        report = json.load(fh)
    empty_before = report["known_gaps"]["si_units_empty"]
    report["known_gaps"]["si_units_empty"] = empty_before - len(EXEMPT)
    report["known_gaps"]["si_units_exempt"] = len(EXEMPT)
    report["si_units_exempt"] = {
        "applied": "2026-11-12",
        "artifacts": ["si-units-exempt-entries.jsonl", "si-units-exempt-rules.json"],
        "entries": [e["id"] for e in EXEMPT],
        "rationale": "pure logical/quantified/set-theoretic statements carry no SI "
                     "dimension semantics; exempted from unit review entirely",
    }
    dump_json(report_path, report)

    names = [
        "a2-derivation-log.jsonl",
        "a2-derivation-rules.json",
        "excluded-non-identity-forms.jsonl",
        "formula-and-artifact-fix-log.jsonl",
        "formula-and-artifact-fix-rules.json",
        "input-law-pool.jsonl",
        "normalization-rules.json",
        "normalized-law-pool.jsonl",
        "provenance-law-pool.jsonl",
        "si-units-backfill-log.jsonl",
        "si-units-backfill-partial-log.jsonl",
        "si-units-backfill-partial-rules.json",
        "si-units-backfill-rules.json",
        "si-units-empty-backfill-log.jsonl",
        "si-units-empty-backfill-rules.json",
        "si-units-exempt-entries.jsonl",
        "si-units-exempt-rules.json",
        "si-units-notation-normalize-log.jsonl",
        "si-units-notation-normalize-rules.json",
        "structural-report.json",
        "x3-split-map.json",
        "x5-mult-map.json",
    ]
    out_lines = []
    for name in names:
        out_lines.append("%s  %s" % (sha256_file(os.path.join(POOL, name)), name))
    combined = hashlib.sha256("\n".join(out_lines).encode("utf-8")).hexdigest()
    out_lines.append("%s  COMBINED" % combined)
    with io.open(os.path.join(POOL, "manifest.sha256"), "w", encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(out_lines) + "\n")

    print("exempted %d entries; manifest re-anchored (%d files); pool lines untouched"
          % (len(EXEMPT), len(names)))


if __name__ == "__main__":
    sys.exit(main())
