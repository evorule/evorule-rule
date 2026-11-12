#!/usr/bin/env python3
"""Revise the SI-unit review registry into the three-tier form.

Per the human adjudication of 2026-11-12 ("three-tier scheme"), the unified
criterion is: whether the formula contains symbols that commit to physical
dimensions. Exemption does not mean skipping review -- it means the review
lens differs per tier:

- tier "exempt" (3 rows, already registered): pure logical/quantified/
  set-theoretic statements -- no SI dimension semantics at all.
- tier "agnostic" (3 new rows): abstract analysis statements (limit
  definition, intermediate value theorem, mean value theorem) -- symbolic
  mappings are kept but no units are presupposed; any later assignment must
  leave the whole equation self-consistent.
- tier "rule_defined" (6 new rows): statements whose dimensions are governed
  by function-notation family rules (trig identities require dimensionless
  arguments; norm/modulus dimensions follow their arguments and are never
  preset to 1). These stay out of unit-value review until the function-notation
  engine-support milestone lands, and are NOT exempt.

Rewrites the sidecars, updates the structural report tier counters (moving
the nine tier-2/3 rows out of the plain "empty, awaiting SI-value review"
gap), and re-anchors the pool manifest. Pool lines remain byte-untouched.
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

TIER1 = [
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
TIER2 = [
    {"id": "math_ana_def_01", "subdomain": "analysis_real",
     "title_zh": "极限定义（数列形式）",
     "rationale": "ε-N 量词定义：只约束 |an-a|<epsilon 的序结构，不承诺物理量纲；符号映射保留、单位不预设，若给定须整式自洽"},
    {"id": "math_ana_thm_01", "subdomain": "analysis_real",
     "title_zh": "介值定理",
     "rationale": "连续性与零点存在陈述（f∈C[a,b] ∧ f(a)*f(b)<0 → ∃c: f(c)=0）：抽象函数值域语义，不承诺物理量纲；同上口径"},
    {"id": "math_ana_thm_03", "subdomain": "analysis_real",
     "title_zh": "中值定理（Lagrange）",
     "rationale": "导数商等式 f'(c)=(f(b)-f(a))/(b-a)：两侧同为函数增量比，量纲自洽性由整式保证，单位不预设"},
]
TIER3 = [
    {"id": "math_trig_pythagorean_identity", "dup_of": None, "subdomain": "trigonometry",
     "title_zh": "三角恒等式（勾股恒等式）",
     "rationale": "函数记号族规则：三角函数宗量须无量纲，等式为纯数恒等式；量纲面由函数记号规则给出，值审归 M1 函数记号引擎支持批之后，不豁免"},
    {"id": "math_trig_pythagorean_identity_dup2", "dup_of": "math_trig_pythagorean_identity",
     "subdomain": "trigonometry", "title_zh": "三角恒等式（池内重复条目副本）", "rationale": "同主条；dup2 副本随主条同档"},
    {"id": "math_linalg_norm_euclidean", "dup_of": None, "subdomain": "linear_algebra",
     "title_zh": "欧氏范数",
     "rationale": "函数记号族规则：norm 量纲随宗量（向量分量同量纲时 norm 同该量纲），不预设 1；值审归 M1 之后，不豁免"},
    {"id": "math_linalg_norm_euclidean_dup2", "dup_of": "math_linalg_norm_euclidean",
     "subdomain": "linear_algebra", "title_zh": "欧氏范数（池内重复条目副本）", "rationale": "同主条；dup2 副本随主条同档"},
    {"id": "math_complex_modulus", "dup_of": None, "subdomain": "complex_analysis",
     "title_zh": "复模长",
     "rationale": "函数记号族规则：mod 量纲随宗量（实部/虚部同量纲时模同该量纲），不预设 1；值审归 M1 之后，不豁免"},
    {"id": "math_complex_modulus_dup2", "dup_of": "math_complex_modulus",
     "subdomain": "complex_analysis", "title_zh": "复模长（池内重复条目副本）", "rationale": "同主条；dup2 副本随主条同档"},
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

    # all twelve rows must exist in the pool with empty si_units
    with io.open(POOL_FILE, encoding="utf-8", newline="") as f:
        lines = f.read().split("\n")
    rows = {}
    for line in lines:
        if line.strip():
            r = json.loads(line)
            rows[r["id"]] = r
    assert len(rows) == 409, "unexpected pool size"
    all_rows = TIER1 + TIER2 + TIER3
    for e in all_rows:
        r = rows[e["id"]]
        assert r.get("si_units") == {}, "tiered entry unexpectedly filled: %s" % e["id"]
        assert (r.get("metadata") or {}).get("subdomain") == e["subdomain"], e["id"]

    rows_out = []
    for e in all_rows:
        row = dict(e)
        row["reviewed"] = "2026-11-12"
        row["tier"] = ("exempt" if e in TIER1
                       else "agnostic" if e in TIER2
                       else "rule_defined")
        rows_out.append(row)
    dump_jsonl(list_path, rows_out)

    dump_json(rules_path, {
        "notation": {
            "dimensionless": '"1" (engine has no "dimensionless" token)',
            "form": "coherent SI symbols or base-unit power products (e.g. m*s^-1)",
        },
        "reviewed": "2026-11-12",
        "criterion": "whether the formula contains symbols that commit to physical "
                     "dimensions; exemption does not mean skipping review -- the "
                     "review lens differs per tier",
        "tiers": {
            "exempt": {
                "count": len(TIER1),
                "rule": "pure logical/quantified/set-theoretic statements carry no SI "
                        "dimension semantics; exempted from unit review entirely "
                        "(si_units intentionally left empty, no candidate derivation)",
            },
            "agnostic": {
                "count": len(TIER2),
                "rule": "abstract analysis statements: symbolic mappings are kept but "
                        "no units are presupposed; any later unit assignment must "
                        "leave the whole equation self-consistent (engine check_law "
                        "lens: consistency-only, no dimension presupposition)",
            },
            "rule_defined": {
                "count": len(TIER3),
                "rule": "function-notation family rules: trigonometric functions "
                        "require dimensionless arguments; norm/modulus dimensions "
                        "follow their arguments and are never preset to 1; these "
                        "rows stay out of unit-value review until the "
                        "function-notation engine-support milestone (M1) lands, "
                        "and are NOT exempt",
            },
        },
        "supersedes": "single-tier exempt list registered 2026-11-12 (3 rows, now "
                      "tiered with the same ids)",
    })

    report_path = os.path.join(POOL, "structural-report.json")
    with io.open(report_path, encoding="utf-8") as fh:
        report = json.load(fh)
    empty_before = report["known_gaps"]["si_units_empty"]
    report["known_gaps"]["si_units_empty"] = empty_before - len(TIER2) - len(TIER3)
    report["known_gaps"]["si_units_exempt"] = len(TIER1)
    report["known_gaps"]["si_units_tiered_review"] = len(TIER2) + len(TIER3)
    report["si_units_exempt"] = {
        "applied": "2026-11-12",
        "artifacts": ["si-units-exempt-entries.jsonl", "si-units-exempt-rules.json"],
        "entries": [e["id"] for e in TIER1],
        "rationale": "pure logical/quantified/set-theoretic statements carry no SI "
                     "dimension semantics; exempted from unit review entirely",
        "tiered_review_addendum": {
            "applied": "2026-11-12",
            "criterion": "formula symbols commit to physical dimensions or not; "
                         "review lens differs per tier",
            "agnostic": [e["id"] for e in TIER2],
            "rule_defined": [e["id"] for e in TIER3],
            "milestone_dependency": "rule_defined rows await unit-value review until "
                                    "M1 (function-notation engine support) lands",
        },
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

    print("tiered registry written: exempt=%d agnostic=%d rule_defined=%d; "
          "manifest re-anchored (%d files); pool lines untouched"
          % (len(TIER1), len(TIER2), len(TIER3), len(names)))


if __name__ == "__main__":
    sys.exit(main())
