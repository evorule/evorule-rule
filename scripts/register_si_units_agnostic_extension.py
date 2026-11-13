#!/usr/bin/env python3
"""Extend the SI-unit exemption register with the math-domain agnostic tier.

Per the human adjudication of 2026-11-13 (review request 77 ruling 4; per-row
evidence in research memo 79 section 5), the 46 math-domain rows that pass the
grammar probe are registered as tier "agnostic": pure-math identities
(arithmetic laws, derivative-rule collapsed-notation family, probability,
linear algebra, geometry/graph identities) are dimension-arbitrary but
self-consistent as whole expressions, committing to no physical unit
semantics. This pass appends 46 rows to the tiered exemption sidecar,
records the extension in the structural report (moving the rows from the
"empty, awaiting review" gap into the tiered-review counter), updates the
exemption-rules tier census, adds its own log/rules sidecars, and re-anchors
the pool manifest. Pool lines themselves are untouched.
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
ENTRIES_FILE = os.path.join(POOL, "si-units-exempt-entries.jsonl")
RULES_FILE = os.path.join(POOL, "si-units-exempt-rules.json")
LOG_FILE = os.path.join(POOL, "si-units-agnostic-extension-log.jsonl")
EXT_RULES_FILE = os.path.join(POOL, "si-units-agnostic-extension-rules.json")
REPORT_FILE = os.path.join(POOL, "structural-report.json")
MANIFEST_FILE = os.path.join(POOL, "manifest.sha256")

APPLIED = "2026-11-13"

# Primary rows only; dup2 copies are derived (same tier, dup_of -> primary).
PRIMARY = [
    {"id": "math_ana_thm_02", "subdomain": "analysis_complex",
     "title_zh": "欧拉公式",
     "rationale": "复分析恒等式（e^(ix)=cos(x)+i*sin(x)）：欧拉公式，两侧同为纯数恒等；量纲任意-自洽，符号映射保留、单位不预设，若给定须整式自洽"},
    {"id": "math_arith_add_inverse", "subdomain": "arithmetic",
     "title_zh": "加法逆元恒等式",
     "rationale": "纯算术恒等式（a+(-1*a)=0）：两侧同为抽象数，量纲任意-自洽，符号映射保留、单位不预设，若给定须整式自洽"},
    {"id": "math_arith_distributive", "subdomain": "arithmetic",
     "title_zh": "分配律",
     "rationale": "纯算术恒等式（a*(b+c)=a*b+a*c）：两侧同为抽象数线性组合，量纲任意-自洽；同上口径"},
    {"id": "math_arith_mul_assoc", "subdomain": "arithmetic",
     "title_zh": "乘法结合律",
     "rationale": "纯算术恒等式（(a*b)*c=a*(b*c)）：两侧同为抽象数乘积，量纲任意-自洽；同上口径"},
    {"id": "math_arith_mul_comm", "subdomain": "arithmetic",
     "title_zh": "乘法交换律",
     "rationale": "纯算术恒等式（a*b=b*a）：两侧同为抽象数乘积，量纲任意-自洽；同上口径"},
    {"id": "math_arith_mul_identity", "subdomain": "arithmetic",
     "title_zh": "乘法单位元恒等式",
     "rationale": "纯算术恒等式（a*1=a）：两侧同为抽象数，量纲任意-自洽；同上口径"},
    {"id": "math_calc_chain_rule", "subdomain": "calculus",
     "title_zh": "链式法则（坍缩记号）",
     "rationale": "微分规则坍缩记号族（d=df*dg）：两侧同属微分算子作用语义，量纲自洽性由整式保证，量纲任意-自洽；单位不预设（同 math_ana_thm_03 口径）"},
    {"id": "math_calc_derivative_power", "subdomain": "calculus",
     "title_zh": "幂函数求导法则（坍缩记号）",
     "rationale": "微分规则坍缩记号族（d=n*x^(n-1)）：两侧同属微分算子作用语义，量纲自洽性由整式保证，量纲任意-自洽；同上口径"},
    {"id": "math_calc_fundamental_theorem_p1", "subdomain": "calculus",
     "title_zh": "微积分基本定理（第一部分）",
     "rationale": "微分规则坍缩记号族（F=integral）：两侧同属变限积分语义，量纲自洽性由整式保证，量纲任意-自洽；同上口径"},
    {"id": "math_calc_fundamental_theorem_p2", "subdomain": "calculus",
     "title_zh": "微积分基本定理（第二部分）",
     "rationale": "微分规则坍缩记号族（dF=f）：两侧同属微分算子作用语义，量纲自洽性由整式保证，量纲任意-自洽；同上口径"},
    {"id": "math_calc_product_rule", "subdomain": "calculus",
     "title_zh": "乘积法则（坍缩记号）",
     "rationale": "微分规则坍缩记号族（d=f*dg+g*df）：两侧同属微分算子作用语义，量纲自洽性由整式保证，量纲任意-自洽；同上口径"},
    {"id": "math_calc_quotient_rule", "subdomain": "calculus",
     "title_zh": "商法则（坍缩记号）",
     "rationale": "微分规则坍缩记号族（d=(g*df-f*dg)/g^2）：两侧同属微分算子作用语义，量纲自洽性由整式保证，量纲任意-自洽；同上口径"},
    {"id": "math_exp_zero_exp", "subdomain": "exponential",
     "title_zh": "零指数恒等式",
     "rationale": "纯算术恒等式（a^0=1）：两侧同为抽象数，量纲任意-自洽；同上口径"},
    {"id": "math_geo_thm_01", "subdomain": "geometry_euclidean",
     "title_zh": "勾股定理",
     "rationale": "欧氏几何度量恒等式（a^2+b^2=c^2）：两侧同为长度平方和，任一同量纲标度下整式自洽，量纲任意-自洽；单位不预设"},
    {"id": "math_geo_thm_02", "subdomain": "geometry_euclidean",
     "title_zh": "余弦定理",
     "rationale": "欧氏几何度量恒等式（c^2=a^2+b^2-2*a*b*cos_C）：两侧同为长度平方，任一同量纲标度下整式自洽，量纲任意-自洽；同上口径"},
    {"id": "math_graph_thm_01", "subdomain": "graph_theory",
     "title_zh": "欧拉多面体公式",
     "rationale": "纯组合计数恒等式（F+V-E=2）：面/顶点/棱计数均为纯数，无 SI 量纲语义；量纲任意-自洽，单位不预设"},
    {"id": "math_linalg_det_2x2", "subdomain": "linear_algebra",
     "title_zh": "二阶行列式展开",
     "rationale": "线性代数恒等式（det=a*d-b*c）：两侧随矩阵元同量纲语义整式自洽，量纲任意-自洽；单位不预设"},
    {"id": "math_linalg_det_product", "subdomain": "linear_algebra",
     "title_zh": "行列式乘积恒等式",
     "rationale": "线性代数恒等式（det_AB=det_A*det_B）：两侧随矩阵语义整式自洽，量纲任意-自洽；同上口径"},
    {"id": "math_linalg_dot_product", "subdomain": "linear_algebra",
     "title_zh": "内积展开恒等式",
     "rationale": "线性代数恒等式（dot=x1*x2+y1*y2）：两侧随分量同量纲语义整式自洽，量纲任意-自洽；同上口径"},
    {"id": "math_linalg_trace_identity", "subdomain": "linear_algebra",
     "title_zh": "迹与特征值和恒等式",
     "rationale": "线性代数恒等式（tr=l1+l2）：两侧随矩阵语义整式自洽，量纲任意-自洽；同上口径"},
    {"id": "math_prob_bayes", "subdomain": "probability",
     "title_zh": "贝叶斯定理",
     "rationale": "概率恒等式（P_A_given_B=P_B_given_A*P_A/P_B）：概率与条件概率均为纯数测度，量纲任意-自洽；单位不预设，若给定须整式自洽"},
    {"id": "math_prob_expectation_linear", "subdomain": "probability",
     "title_zh": "期望线性性",
     "rationale": "概率恒等式（E_XY=E_X+E_Y）：期望均为纯数测度，量纲任意-自洽；同上口径"},
    {"id": "math_prob_normal_pdf", "subdomain": "probability",
     "title_zh": "正态分布密度函数",
     "rationale": "概率恒等式（f=(1/sqrt(2*pi))*exp(-1*x^2/2)）：归一化密度整式自洽，量纲任意-自洽；同上口径"},
    {"id": "math_prob_total_prob", "subdomain": "probability",
     "title_zh": "全概率公式",
     "rationale": "概率恒等式（P_B=P_B_A1*P_A1+P_B_A2*P_A2）：两侧同为纯数测度线性组合，量纲任意-自洽；同上口径"},
    {"id": "math_prob_variance_def", "subdomain": "probability",
     "title_zh": "方差定义",
     "rationale": "概率恒等式（Var=E_X2-E_X^2）：两侧同为纯数测度，量纲任意-自洽；同上口径"},
    {"id": "math_prob_variance_sum", "subdomain": "probability",
     "title_zh": "独立和方差可加性",
     "rationale": "概率恒等式（Var_XY=Var_X+Var_Y）：两侧同为纯数测度，量纲任意-自洽；同上口径"},
]

# Row order for the appended block (kept aligned with research memo 79 §5).
ORDER = [
    "math_ana_thm_02",
    "math_arith_add_inverse", "math_arith_add_inverse_dup2",
    "math_arith_distributive", "math_arith_distributive_dup2",
    "math_arith_mul_assoc", "math_arith_mul_assoc_dup2",
    "math_arith_mul_comm", "math_arith_mul_comm_dup2",
    "math_arith_mul_identity", "math_arith_mul_identity_dup2",
    "math_calc_chain_rule", "math_calc_chain_rule_dup2",
    "math_calc_derivative_power",
    "math_calc_fundamental_theorem_p1", "math_calc_fundamental_theorem_p1_dup2",
    "math_calc_fundamental_theorem_p2", "math_calc_fundamental_theorem_p2_dup2",
    "math_calc_product_rule", "math_calc_product_rule_dup2",
    "math_calc_quotient_rule", "math_calc_quotient_rule_dup2",
    "math_exp_zero_exp", "math_exp_zero_exp_dup2",
    "math_geo_thm_01", "math_geo_thm_02", "math_graph_thm_01",
    "math_linalg_det_2x2", "math_linalg_det_2x2_dup2",
    "math_linalg_det_product", "math_linalg_det_product_dup2",
    "math_linalg_dot_product", "math_linalg_dot_product_dup2",
    "math_linalg_trace_identity", "math_linalg_trace_identity_dup2",
    "math_prob_bayes",
    "math_prob_expectation_linear", "math_prob_expectation_linear_dup2",
    "math_prob_normal_pdf", "math_prob_normal_pdf_dup2",
    "math_prob_total_prob", "math_prob_total_prob_dup2",
    "math_prob_variance_def", "math_prob_variance_def_dup2",
    "math_prob_variance_sum", "math_prob_variance_sum_dup2",
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
    if os.path.exists(LOG_FILE) or os.path.exists(EXT_RULES_FILE):
        sys.exit("agnostic extension sidecars already exist; rerun refused (idempotency guard)")

    with io.open(POOL_FILE, encoding="utf-8", newline="") as f:
        pool_rows = {}
        for line in f:
            if line.strip():
                r = json.loads(line)
                pool_rows[r["id"]] = r
    assert len(pool_rows) == 409, "unexpected pool size"
    empty_si = [i for i, r in pool_rows.items() if r.get("si_units") == {}]
    assert len(empty_si) == 163, "unexpected empty-si census: %d" % len(empty_si)
    for p in PRIMARY:
        r = pool_rows[p["id"]]
        assert r["domain"] == "math", p["id"]
        assert r.get("si_units") == {}, p["id"]
        assert (r.get("metadata") or {}).get("subdomain") == p["subdomain"], p["id"]

    with io.open(ENTRIES_FILE, encoding="utf-8", newline="") as f:
        existing = [json.loads(line) for line in f if line.strip()]
    assert len(existing) == 12, "unexpected existing exemption register size: %d" % len(existing)
    registered = {e["id"] for e in existing}
    assert not (registered & set(ORDER)), "overlap with existing register"
    for i in ORDER:
        assert pool_rows[i].get("si_units") == {}, i

    prim_by_id = {p["id"]: p for p in PRIMARY}
    new_rows = []
    for i in ORDER:
        if i.endswith("_dup2"):
            base = i[:-len("_dup2")]
            p = prim_by_id[base]
            new_rows.append({
                "dup_of": base,
                "id": i,
                "rationale": "同主条；dup2 副本随主条同档",
                "reviewed": APPLIED,
                "subdomain": p["subdomain"],
                "tier": "agnostic",
                "title_zh": p["title_zh"] + "（池内重复条目副本）",
            })
        else:
            p = prim_by_id[i]
            new_rows.append({
                "dup_of": None,
                "id": i,
                "rationale": p["rationale"],
                "reviewed": APPLIED,
                "subdomain": p["subdomain"],
                "tier": "agnostic",
                "title_zh": p["title_zh"],
            })
    assert len(new_rows) == 46

    # append to the tiered exemption register (existing 12 rows kept verbatim)
    dump_jsonl(ENTRIES_FILE, existing + new_rows)

    # this batch's own sidecars
    dump_jsonl(LOG_FILE, [{
        "action": "registered",
        "dup_of": r["dup_of"],
        "file": "si-units-exempt-entries.jsonl",
        "id": r["id"],
        "pool_si_units": "untouched (intentionally empty)",
        "tier": "agnostic",
    } for r in new_rows])
    dump_json(EXT_RULES_FILE, {
        "applied": APPLIED,
        "criterion": "pure-math identities (arithmetic laws, derivative-rule "
                     "collapsed-notation family, probability, linear algebra, "
                     "geometry/graph identities): dimension-arbitrary but "
                     "self-consistent as whole expressions; no physical unit "
                     "semantics committed; symbolic mappings kept, units not "
                     "presupposed, any later unit assignment must leave the "
                     "whole equation self-consistent",
        "entries_added": 46,
        "notation": {
            "dimensionless": '"1" (engine has no "dimensionless" token)',
            "form": "coherent SI symbols or base-unit power products (e.g. m*s^-1)",
        },
        "reviewed": APPLIED,
        "source": "approved 2026-11-13 (review request 77 ruling 4; per-row "
                  "evidence: research memo 79 section 5)",
        "supersedes": "none (extends the tiered register in place)",
        "tier": "agnostic",
    })

    # exemption-rules tier census: agnostic 3 -> 49
    with io.open(RULES_FILE, encoding="utf-8") as fh:
        rules = json.load(fh)
    rules["tiers"]["agnostic"]["count"] = 49
    rules["tiers"]["agnostic"]["extension"] = (
        "extended 2026-11-13 by 46 math-domain rows "
        "(si-units-agnostic-extension-log.jsonl / -rules.json)")
    dump_json(RULES_FILE, rules)

    # structural report: gap counters move + this batch's own section
    with io.open(REPORT_FILE, encoding="utf-8") as fh:
        report = json.load(fh)
    assert report["known_gaps"]["si_units_empty"] == 151, "unexpected gap baseline"
    assert report["known_gaps"]["si_units_tiered_review"] == 9, "unexpected tiered baseline"
    report["known_gaps"]["si_units_empty"] = 151 - 46
    report["known_gaps"]["si_units_tiered_review"] = 9 + 46
    report["si_units_agnostic_extension"] = {
        "applied": APPLIED,
        "artifacts": [
            "si-units-agnostic-extension-log.jsonl",
            "si-units-agnostic-extension-rules.json",
        ],
        "entries_added": 46,
        "entries": list(ORDER),
        "tier": "agnostic",
        "source": "approved 2026-11-13 (review request 77 ruling 4; per-row "
                  "evidence: research memo 79 section 5)",
    }
    dump_json(REPORT_FILE, report)

    # manifest re-anchor: same file set and order, all hashes recomputed
    with io.open(MANIFEST_FILE, encoding="utf-8", newline="") as fh:
        manifest_lines = [line for line in fh.read().split("\n") if line.strip()]
    names = []
    for line in manifest_lines:
        digest, name = line.split("  ", 1)
        if name != "COMBINED":
            names.append(name)
    out_lines = ["%s  %s" % (sha256_file(os.path.join(POOL, n)), n) for n in names]
    combined = hashlib.sha256("\n".join(out_lines).encode("utf-8")).hexdigest()
    out_lines.append("%s  COMBINED" % combined)
    with io.open(MANIFEST_FILE, "w", encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(out_lines) + "\n")

    print("registered %d agnostic rows (register 12 -> 58); "
          "gaps empty 151 -> 105, tiered_review 9 -> 55; "
          "manifest re-anchored (%d files); pool lines untouched"
          % (len(new_rows), len(names)))


if __name__ == "__main__":
    sys.exit(main())
