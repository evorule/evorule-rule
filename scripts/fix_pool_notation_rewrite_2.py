#!/usr/bin/env python3
"""Deterministic pool fixes: notation rewrite for 14 review-face rows
(approved 2026-11-13, review request 77 ruling 3; B-327-style continuation).

Formula rewrites (all rows have empty si_units; derivations untouched):
- bracket-concentration -> bare symbol:  pH = -1 * log(Hconc)
                                         v  = Vmax * S_conc/(Km + S_conc)
- implicit multiplication -> explicit:   R * (...), eta * (...), -1 * k * (...)
- function-call notation:                r(theta) -> r_orb, f(E) -> f_E
- collapsed trig:                        costheta -> cos(theta)
- collapsed constants:                   piG -> pi*G, c4 -> c^4, c2 -> c^2,
                                         kT -> k*T
- summation prefix:                      SigmaDeltaH_X -> sum(DeltaH_X)
- misc:                                  #eg -> n_eg, #t2g -> n_t2g,
                                         E ^ (-Ea/(R*T)) -> e^(-Ea/(R*T))

Excluded from this batch (2 rows): chem_eqb_rul_02 / chem_eqb_thm_01 — after
rewriting the brackets they would still contain symbolic exponents (^m/^c),
which the engine permanently rejects (M1c ruling); they stay recorded as-is.

Pure deterministic transformation: surgical value-substring edits inside the
14 target lines only. Sidecars (log + rules), structural-report refresh, and
manifest re-anchor.
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

# id -> (formula_before, formula_after, symbols_before, symbols_after)
FIXES = {
    "chem_acb_rul_01": (
        "pH = -log[H+]",
        "pH = -1 * log(Hconc)",
        ["H", "log", "pH"],
        ["Hconc", "pH"],
    ),
    "chem_pcd_kin_02": (
        "v = Vmax[S]/(Km + [S])",
        "v = Vmax * S_conc/(Km + S_conc)",
        ["Km", "S", "Vmax", "v"],
        ["Km", "S_conc", "Vmax", "v"],
    ),
    "phy_atm_law_02": (
        "1/lambda = R(1/n12 - 1/n22)",
        "1/lambda = R * (1/n12 - 1/n22)",
        ["R", "lambda", "n12", "n22"],
        ["R", "lambda", "n12", "n22"],
    ),
    "phy_flu_law_01": (
        "tau = eta(dv/dn)",
        "tau = eta * (dv/dn)",
        ["dn", "dv", "eta", "tau"],
        ["dn", "dv", "eta", "tau"],
    ),
    "phy_thr_law_03": (
        "dT/dt = -k(T - T_env)",
        "dT/dt = -1 * k * (T - T_env)",
        ["T", "T_env", "dT", "dt", "k"],
        ["T", "T_env", "dT", "dt", "k"],
    ),
    "phy_ast_law_01": (
        "r(theta) = p/(1 + e*costheta)",
        "r_orb = p/(1 + e*cos(theta))",
        ["costheta", "e", "p", "r", "theta"],
        ["e", "p", "r_orb", "theta"],
    ),
    "phy_qm_law_03": (
        "Deltalambda = h/(m_e*c)*(1 - costheta)",
        "Deltalambda = h/(m_e*c)*(1 - cos(theta))",
        ["Deltalambda", "c", "costheta", "h", "m_e"],
        ["Deltalambda", "c", "cos", "h", "m_e", "theta"],
    ),
    "phy_sta_prin_03": (
        "f(E) = 1/(e^((E-E_F)/kT) + 1)",
        "f_E = 1/(e^((E - E_F)/(k*T)) + 1)",
        ["E", "E_F", "e", "f", "kT"],
        ["E", "E_F", "f_E", "k", "T"],
    ),
    "phy_sta_prin_04": (
        "f(E) = 1/(e^(E/kT) - 1)",
        "f_E = 1/(e^(E/(k*T)) - 1)",
        ["E", "e", "f", "kT"],
        ["E", "f_E", "k", "T"],
    ),
    "phy_rel_the_02": (
        "G_munu = (8*piG/c4)*T_munu",
        "G_munu = (8*pi*G/c^4)*T_munu",
        ["G_munu", "T_munu", "c4", "piG"],
        ["G", "G_munu", "T_munu", "c", "pi"],
    ),
    "phy_gra_app_02": (
        "Deltanu/nu = -DeltaPhi/c2",
        "Deltanu/nu = -1 * DeltaPhi/c^2",
        ["DeltaPhi", "Deltanu", "c2", "nu"],
        ["DeltaPhi", "Deltanu", "c", "nu"],
    ),
    "chem_thr_thm_01": (
        "DeltaH_reaction = SigmaDeltaH_products - SigmaDeltaH_reactants",
        "DeltaH_reaction = sum(DeltaH_products) - sum(DeltaH_reactants)",
        ["DeltaH_reaction", "SigmaDeltaH_products", "SigmaDeltaH_reactants"],
        ["DeltaH_products", "DeltaH_reaction", "DeltaH_reactants"],
    ),
    "chem_cft_prin_01": (
        "CFSE = (#eg * -0.6*Deltao) + (#t2g * -0.4*Deltao)",
        "CFSE = (n_eg * -0.6 * Deltao) + (n_t2g * -0.4 * Deltao)",
        ["CFSE", "Deltao", "eg", "t2g"],
        ["CFSE", "Deltao", "n_eg", "n_t2g"],
    ),
    "chem_rxn_kin_01": (
        "k = A * E ^ (-Ea / (R * T))",
        "k = A * e^(-Ea/(R*T))",
        ["A", "E", "Ea", "R", "T", "k"],
        ["A", "Ea", "R", "T", "k"],
    ),
}


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


def inline_map(keys):
    return json.dumps({k: "" for k in sorted(keys)}, ensure_ascii=False)


def replace_once(line, old, new, tid, what):
    assert line.count(old) == 1, (
        "expected exactly one %s match in %s" % (what, tid))
    return line.replace(old, new)


def main():
    rules_path = os.path.join(POOL, "notation-rewrite-2-rules.json")
    log_path = os.path.join(POOL, "notation-rewrite-2-log.jsonl")
    if os.path.exists(rules_path) or os.path.exists(log_path):
        sys.exit("sidecars already exist; rerun refused (idempotency guard)")

    with io.open(POOL_FILE, encoding="utf-8", newline="") as f:
        lines = f.read().split("\n")
    had_final_nl = lines and lines[-1] == ""
    if had_final_nl:
        lines = lines[:-1]

    log_rows = []
    for tid in sorted(FIXES):
        f_before, f_after, sym_before, sym_after = FIXES[tid]
        idx = next(i for i, l in enumerate(lines)
                   if l.strip() and json.loads(l)["id"] == tid)
        row = json.loads(lines[idx])
        line = lines[idx]

        assert row["formula"] == f_before, "formula drifted: %s" % tid
        assert sorted(row["symbols"]) == sorted(sym_before), (
            "symbols drifted: %s" % tid)
        assert row.get("si_units") == {}, "si not empty: %s" % tid

        line = replace_once(line, '"formula": ' + json.dumps(f_before, ensure_ascii=False),
                            '"formula": ' + json.dumps(f_after, ensure_ascii=False),
                            tid, "formula")
        line = replace_once(line, '"symbols": ' + inline_map(sym_before),
                            '"symbols": ' + inline_map(sym_after), tid, "symbols")

        lines[idx] = line
        log_rows.append({
            "file": "normalized-law-pool.jsonl", "id": tid,
            "field": "formula+symbols",
            "formula_after": f_after, "formula_before": f_before,
            "symbols_after": sym_after, "symbols_before": sym_before,
        })

    assert len(log_rows) == 14, "expected 14 entries changed, got %d" % len(log_rows)

    text = "\n".join(lines) + ("\n" if had_final_nl else "")
    with io.open(POOL_FILE, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)

    rules = {
        "applied": "2026-11-13",
        "approval": "review request 77 ruling 3 (user directive 2026-11-13: "
                    "T4 rewrite batch first)",
        "entries_fixed": 14,
        "excluded": ["chem_eqb_rul_02", "chem_eqb_thm_01"],
        "exclusion_reason": "symbolic exponents (^m/^c) remain after bracket "
                            "rewrite; permanently rejected by the M1c ruling; "
                            "recorded as-is with the out-of-grammar face",
        "fixes": {
            "brackets": "concentration brackets -> bare symbols "
                        "(chem_acb_rul_01, chem_pcd_kin_02)",
            "implicit-mul": "R(...)/eta(...)/-k(...) -> explicit forms "
                            "(phy_atm_law_02, phy_flu_law_01, phy_thr_law_03)",
            "function-notation": "r(theta) -> r_orb, f(E) -> f_E "
                                 "(phy_ast_law_01, phy_sta_prin_03, phy_sta_prin_04)",
            "collapsed-trig": "costheta -> cos(theta) "
                              "(phy_ast_law_01, phy_qm_law_03)",
            "collapsed-constants": "piG -> pi*G, c4 -> c^4, c2 -> c^2, kT -> k*T "
                                   "(phy_rel_the_02, phy_gra_app_02, "
                                   "phy_sta_prin_03, phy_sta_prin_04)",
            "summation": "SigmaDeltaH_X -> sum(DeltaH_X) (chem_thr_thm_01)",
            "misc": "#eg -> n_eg, #t2g -> n_t2g (chem_cft_prin_01); "
                    "E ^ -> e^ mis-recording (chem_rxn_kin_01, symbol E removed)",
        },
    }
    dump_json(rules_path, rules)
    dump_jsonl(log_path, log_rows)

    report_path = os.path.join(POOL, "structural-report.json")
    with io.open(report_path, encoding="utf-8") as fh:
        report = json.load(fh)
    report["notation_rewrite_2"] = {
        "applied": "2026-11-13",
        "artifacts": ["notation-rewrite-2-log.jsonl",
                      "notation-rewrite-2-rules.json"],
        "entries_fixed": 14,
        "source": "approved 2026-11-13 (review request 77 ruling 3)",
    }
    dump_json(report_path, report)

    names = sorted(n for n in os.listdir(POOL)
                   if n != "manifest.sha256" and os.path.isfile(os.path.join(POOL, n)))
    out_lines = ["%s  %s" % (sha256_file(os.path.join(POOL, n)), n) for n in names]
    combined = hashlib.sha256("\n".join(out_lines).encode("utf-8")).hexdigest()
    out_lines.append("%s  COMBINED" % combined)
    with io.open(os.path.join(POOL, "manifest.sha256"), "w",
                 encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(out_lines) + "\n")

    print("fixed %d entries; manifest re-anchored (%d files)"
          % (len(FIXES), len(names)))


if __name__ == "__main__":
    sys.exit(main())
