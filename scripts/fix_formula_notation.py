#!/usr/bin/env python3
"""Deterministic formula-notation fix for the law pool (survey batch follow-up).

Approved 2026-11-12 (survey ruling: tiered repair, semantics-first split,
abstract-math rows excluded). Three face repairs, one deferred registry:

1. Collapsed-product split (15 rows with empty si_units): product tokens
   (RT2/RT/lnQ/nF/kBT/V0e/Idl/rhovL/rhogh/rhov2/rhog/Gm1m2/N0e/lambdat/hnu/
   pihnu3/c3/kT/mv2/pikT) are split into factors, symbols objects updated in
   lockstep; degree/dagger/hat marks dropped with rationale recorded in the
   log (established precedent from the symbol-cleanup pass); function-call
   notation ln(Q) and e^(...) follows pool precedents (chem_sol_ph_def log(),
   exponential-decay rows). Missing multiplication signs are inserted and
   ambiguous groupings made explicit (semantics-preserving re-bracketing,
   recorded per row).
2. Two special rows: chem_bio_rul_01 formula keeps only the reaction
   equation; the embedded standard free-energy value moves to the derivation
   annotation. chem_sto_emp_01 formula loses the natural-language tail; the
   isothermal-isobaric condition moves to the derivation annotation. Symbols
   and si_units untouched.
3. Fragment row phy_qm_law_03: missing multiplication sign inserted
   (m_e c -> m_e*c).
4. Deferred (registered, untouched here): the four rows whose split would
   require new reviewed si keys (chem_sol_thm_02, phy_ast_con_01,
   phy_rel_app_04 -- reviewed-value face; phy_opt_law_03 -- ambiguous
   collapse), and the rows whose dominant defect is outside ASCII-algebra
   reach (math_info_def_01 summation, math_pde_def_02 partials,
   phy_qm_the_01 hbar+partials); exempt/agnostic/abstract-math rows are out
   of scope per the survey ruling.

Sidecars a per-entry log + the frozen rules table, refresh the structural
report, and re-anchor the pool manifest.

Pure deterministic transformation: no network, no clock, no randomness;
byte-stable output for identical input. Line edits are surgical: only the
formula / symbols / derivation value substrings change inside affected
lines; all other lines are preserved byte-for-byte. si_units untouched.
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

# approved si values already backfilled on the two special rows (untouched)
FILLS_EXPECT = {
    "chem_bio_rul_01": {"DeltaG": "J*mol^-1"},
    "chem_sto_emp_01": {"P": "kg*m^-1*s^-2", "T": "K", "V": "m^3",
                        "n": "mol", "constant": "m^3*mol^-1"},
}

# id -> (symbols_before, symbols_after|None, formula_before, formula_after,
#        derivation_after|None)   (None = field unchanged)
FIXES = {
    "chem_app_thm_01": (
        {"DeltaH": "", "RT2": "", "d": "", "dT": "", "lnK": ""},
        {"DeltaH": "", "R": "", "T2": "", "d": "", "dT": "", "lnK": ""},
        "d(lnK)/dT = DeltaH°/RT2",
        "d(lnK)/dT = DeltaH/(R * T2)",
        None,
    ),
    "chem_ele_thm_01": (
        {"E": "", "RT": "", "lnQ": "", "nF": ""},
        {"E": "", "E0": "", "R": "", "T": "", "n": "", "F": "", "ln": "", "Q": ""},
        "E = E° - (RT/nF)*lnQ",
        "E = E0 - (R*T/(n*F))*ln(Q)",
        None,
    ),
    "chem_kin_kin_03": (
        {"Ea": "", "P": "", "RT": "", "Z": "", "e": "", "k": ""},
        {"Ea": "", "P": "", "R": "", "T": "", "Z": "", "e": "", "k": ""},
        "k = P*Z*e^(-Ea/RT)",
        "k = P*Z*e^(-Ea/(R*T))",
        None,
    ),
    "chem_kin_kin_04": (
        {"DeltaG": "", "RT": "", "e": "", "h": "", "k": "", "kBT": ""},
        {"DeltaG_act": "", "R": "", "T": "", "e": "", "h": "", "k": "", "k_B": ""},
        "k = (kBT/h)*e^(-DeltaG‡/RT)",
        "k = (k_B*T/h)*e^(-DeltaG_act/(R*T))",
        None,
    ),
    "phy_elm_app_01": (
        {"RC": "", "V": "", "V0e": "", "t": ""},
        {"V": "", "V0": "", "e": "", "t": "", "R": "", "C": ""},
        "V(t) = V0e^(-t/RC)",
        "V = V0*e^(-t/(R*C))",
        None,
    ),
    "phy_elm_law_03": (
        {"Idl": "", "dB": "", "mu0": "", "pi": "", "r": "", "r2": ""},
        {"dB": "", "mu0": "", "pi": "", "I": "", "dl": "", "r": "", "r_hat": ""},
        "dB = (mu0/4*pi)*Idl*r̂/r2",
        "dB = (mu0/(4*pi))*I*dl*r_hat/r^2",
        None,
    ),
    "phy_elm_prin_03": (
        {"B": "", "dl": "", "mu0I": ""},
        {"B": "", "dl": "", "mu0": "", "I": ""},
        "∮B*dl = mu0I",
        "∮B*dl = mu0*I",
        None,
    ),
    "phy_flu_app_01": (
        {"Re": "", "mu": "", "rhovL": ""},
        {"Re": "", "mu": "", "rho": "", "v": "", "L": ""},
        "Re = rhovL/mu",
        "Re = rho*v*L/mu",
        None,
    ),
    "phy_flu_prin_02": (
        {"P": "", "constant": "", "rhogh": "", "rhov2": ""},
        {"P": "", "constant": "", "rho": "", "v": "", "g": "", "h": ""},
        "P + 1⁄2*rhov2 + rhogh = constant",
        "P + 0.5*rho*v^2 + rho*g*h = constant",
        None,
    ),
    "phy_flu_prin_03": (
        {"P": "", "constant": "", "g": "", "rhog": "", "v2": "", "z": ""},
        {"P": "", "constant": "", "g": "", "rho": "", "v": "", "z": ""},
        "P/rhog + v2/2*g + z = constant",
        "P/(rho*g) + v^2/(2*g) + z = constant",
        None,
    ),
    "phy_gra_app_01": (
        {"F": "", "Gm1m2": "", "r2": ""},
        {"F": "", "G": "", "m1": "", "m2": "", "r": ""},
        "F = Gm1m2/r2",
        "F = G*m1*m2/r^2",
        None,
    ),
    "phy_nuc_law_01": (
        {"N": "", "N0e": "", "lambdat": "", "t": ""},
        {"N": "", "N0": "", "e": "", "lambda": "", "t": ""},
        "N(t) = N0e^(-lambdat)",
        "N = N0*e^(-lambda*t)",
        None,
    ),
    "phy_qm_law_01": (
        {"T": "", "c3": "", "e": "", "hnu": "", "kT": "", "nu": "",
         "pihnu3": "", "rho": ""},
        {"T": "", "c": "", "e": "", "h": "", "k": "", "nu": "", "rho": ""},
        "rho(nu,T) = (8*pihnu3/c3) / (e^(hnu/kT) - 1)",
        "rho = (8*pi*h*nu^3/c^3) / (e^(h*nu/(k*T)) - 1)",
        None,
    ),
    "phy_rel_app_05": (
        {"K": "", "gamma": "", "m0c2": ""},
        {"K": "", "gamma": "", "m0": "", "c": ""},
        "K = (gamma - 1)*m0c2",
        "K = (gamma - 1)*m0*c^2",
        None,
    ),
    "phy_sta_prin_02": (
        {"e": "", "f": "", "kT": "", "m": "", "mv2": "", "pi": "", "pikT": "",
         "v": "", "v2": ""},
        {"e": "", "f": "", "k": "", "T": "", "m": "", "pi": "", "v": ""},
        "f(v) = 4*pi (m/2*pikT)^(3/2) v2 e^(-mv2/2*kT)",
        "f = 4*pi*(m/(2*pi*k*T))^(3/2)*v^2*e^(-m*v^2/(2*k*T))",
        None,
    ),
    "chem_bio_rul_01": (
        {"DeltaG": ""},
        None,
        "ATP + H2O → ADP + Pi, DeltaG°' = -30.5 kJ/mol",
        "ATP + H2O → ADP + Pi",
        ["Biochemistry",
         "standard biochemical transformation free energy: DeltaG' = -30.5 kJ/mol"],
    ),
    "chem_sto_emp_01": (
        {"P": "", "T": "", "V": "", "constant": "", "n": ""},
        None,
        "V/n = constant at same T, P",
        "V/n = constant",
        ["Avogadro, 1811",
         "the constant holds at the same temperature and pressure"],
    ),
    "phy_qm_law_03": (
        {"Deltalambda": "", "c": "", "costheta": "", "h": "", "m_e": ""},
        None,
        "Deltalambda = h/(m_e c)*(1 - costheta)",
        "Deltalambda = h/(m_e*c)*(1 - costheta)",
        None,
    ),
}

LOG_NOTES = {
    "chem_app_thm_01": "RT2 -> R * T2; degree mark on DeltaH dropped (standard-state rationale recorded; precedent from the symbol-cleanup pass)",
    "chem_ele_thm_01": "RT/nF -> (R*T/(n*F)); E0 names the standard potential (degree mark dropped); lnQ -> ln(Q) function-call notation per pool precedent",
    "chem_kin_kin_03": "RT -> R * T inside the exponential denominator; grouping made explicit",
    "chem_kin_kin_04": "kBT -> k_B * T; RT -> R * T; DeltaG dagger mark renamed to DeltaG_act (activation free energy), symbol key renamed in lockstep",
    "phy_elm_app_01": "V0e -> V0 * e^(...) semantics-first split (exponential-embedded collapse); RC -> R * C; V(t) quantity notation -> V",
    "phy_elm_law_03": "Idl -> I * dl; r2 -> r^2; hat mark renamed to r_hat; grouping mu0/4*pi corrected to mu0/(4*pi) (semantics-preserving re-bracketing)",
    "phy_elm_prin_03": "mu0I -> mu0 * I; the loop-integral glyph stays (deferred: glyph outside ASCII-algebra reach)",
    "phy_flu_app_01": "rhovL -> rho * v * L (Reynolds numerator)",
    "phy_flu_prin_02": "rhov2 -> rho * v^2; rhogh -> rho * g * h; the 1/2 fraction glyph -> 0.5 (additive grammar boundary keeps this row skipped, semantics first)",
    "phy_flu_prin_03": "rhog -> rho * g; v2 -> v^2; groupings made explicit (P/(rho*g), v^2/(2*g))",
    "phy_gra_app_01": "Gm1m2 -> G * m1 * m2; r2 -> r^2",
    "phy_nuc_law_01": "N0e -> N0 * e^(...) semantics-first split; lambdat -> lambda * t; N(t) -> N",
    "phy_qm_law_01": "pihnu3 -> pi*h*nu^3; c3 -> c^3; hnu -> h*nu; kT -> k*T; rho(nu,T) -> rho (Planck distribution)",
    "phy_rel_app_05": "m0c2 -> m0 * c^2 (additive grammar boundary keeps this row skipped, semantics first)",
    "phy_sta_prin_02": "pikT -> pi*k*T with grouping corrected to m/(2*pi*k*T); mv2 -> m*v^2; kT -> k*T; v2 -> v^2; f(v) -> f (Maxwell distribution)",
    "chem_bio_rul_01": "two-segment formula split: reaction equation stays in formula; the standard free-energy value moved to the derivation annotation; symbols and si_units untouched",
    "chem_sto_emp_01": "natural-language tail removed; the isothermal-isobaric condition moved to the derivation annotation; symbols and si_units untouched",
    "phy_qm_law_03": "missing multiplication sign inserted (m_e c -> m_e*c)",
}

DEFERRED = [
    "split requires new reviewed si keys (reviewed-value face): "
    "chem_sol_thm_02 (iCRT reviewed as one key), phy_ast_con_01 (mc2 "
    "reviewed as one key), phy_rel_app_04 (gammam0v reviewed as one key)",
    "ambiguous collapse: phy_opt_law_03 (I0cos2theta has no unique split)",
    "glyph-dominant rows: math_info_def_01 (summation), math_pde_def_02 "
    "(partial derivatives), phy_qm_the_01 (hbar + partials)",
    "excluded per survey ruling: exempt/agnostic tier rows and pure "
    "abstract-math rows (index laws, commutators, measure theory, graph "
    "definitions, automata)",
    "engine grammar boundaries stay as established: unary minus, binary "
    "additive operators, function calls, non-constant exponents (rows reach "
    "verification after the engine-notation support batch)",
]

MANIFEST_NAMES = [
    "a2-derivation-log.jsonl",
    "a2-derivation-rules.json",
    "excluded-non-identity-forms.jsonl",
    "formula-and-artifact-fix-log.jsonl",
    "formula-and-artifact-fix-rules.json",
    "formula-notation-fix-log.jsonl",
    "formula-notation-fix-rules.json",
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
    "si-units-empty-backfill-split3-log.jsonl",
    "si-units-empty-backfill-split3-rules.json",
    "si-units-empty-backfill-split4-log.jsonl",
    "si-units-empty-backfill-split4-rules.json",
    "si-units-exempt-entries.jsonl",
    "si-units-exempt-rules.json",
    "si-units-notation-normalize-log.jsonl",
    "si-units-notation-normalize-rules.json",
    "si-units-symbol-cleanup-backfill-log.jsonl",
    "si-units-symbol-cleanup-backfill-rules.json",
    "structural-report.json",
    "symbols-cleanup-log.jsonl",
    "symbols-cleanup-rules.json",
    "x3-split-map.json",
    "x5-mult-map.json",
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


def inline_map(mapping):
    return json.dumps({k: mapping[k] for k in sorted(mapping)}, ensure_ascii=False)


def replace_once(line, old, new, tid, what):
    assert line.count(old) == 1, (
        "expected exactly one %s match in %s" % (what, tid))
    return line.replace(old, new)


def main():
    rules_path = os.path.join(POOL, "formula-notation-fix-rules.json")
    log_path = os.path.join(POOL, "formula-notation-fix-log.jsonl")
    if os.path.exists(rules_path) or os.path.exists(log_path):
        sys.exit("sidecars already exist; rerun refused (idempotency guard)")

    with io.open(POOL_FILE, encoding="utf-8", newline="") as f:
        lines = f.read().split("\n")
    had_final_nl = lines and lines[-1] == ""
    if had_final_nl:
        lines = lines[:-1]

    rows = {}
    for idx, line in enumerate(lines):
        if not line.strip():
            continue
        r = json.loads(line)
        rows[r["id"]] = (idx, r)
    assert len(rows) == 409, "unexpected pool size: %d" % len(rows)

    log_rows = []
    for tid in sorted(FIXES):
        sym_before, sym_after, f_before, f_after, der_after = FIXES[tid]
        idx, row = rows[tid]
        line = lines[idx]

        assert row["symbols"] == sym_before, (
            "symbols drifted from frozen expectation: %s" % tid)
        assert row["formula"] == f_before, (
            "formula drifted from frozen expectation: %s" % tid)
        if tid in ("chem_bio_rul_01", "chem_sto_emp_01"):
            # approved si values backfilled by the symbol-cleanup backfill
            # pass; untouched here
            assert row.get("si_units") == FILLS_EXPECT[tid], (
                "approved si_units drifted: %s" % tid)
        else:
            assert row.get("si_units") == {}, (
                "target entry unexpectedly has si_units: %s" % tid)
        if der_after is not None:
            assert row["derivation"] == der_after[:len(row["derivation"])], (
                "derivation drifted: %s" % tid)

        line = replace_once(line, '"formula": ' + json.dumps(f_before, ensure_ascii=False),
                            '"formula": ' + json.dumps(f_after, ensure_ascii=False),
                            tid, "formula")
        if sym_after is not None:
            line = replace_once(line, '"symbols": ' + inline_map(sym_before),
                                '"symbols": ' + inline_map(sym_after), tid, "symbols")
        if der_after is not None:
            line = replace_once(
                line,
                '"derivation": ' + json.dumps(row["derivation"], ensure_ascii=False),
                '"derivation": ' + json.dumps(der_after, ensure_ascii=False),
                tid, "derivation")

        entry = {
            "field": "formula",
            "file": "normalized-law-pool.jsonl",
            "id": tid,
            "note": LOG_NOTES[tid],
            "formula_after": f_after,
            "formula_before": f_before,
        }
        if sym_after is not None:
            entry["field"] = "formula+symbols"
            entry["symbols_after"] = {k: sym_after[k] for k in sorted(sym_after)}
            entry["symbols_before"] = {k: sym_before[k] for k in sorted(sym_before)}
        if der_after is not None:
            entry["field"] += "+derivation"
            entry["derivation_after"] = der_after
        lines[idx] = line
        log_rows.append(entry)

    text = "\n".join(lines) + ("\n" if had_final_nl else "")
    with io.open(POOL_FILE, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)

    rules = {
        "applied": "2026-11-12",
        "deferred": DEFERRED,
        "derivation_entries": 2,
        "entries_fixed": 18,
        "notation": (
            "collapsed product tokens are split into factors with the symbols "
            "object updated in lockstep; degree/dagger/hat marks are dropped "
            "with per-row rationale recorded in the log (established "
            "precedent); function-call notation follows pool precedents; "
            "semantics-preserving re-bracketing replaces ambiguous "
            "groupings; missing multiplication signs are inserted"),
        "si_units_touched": False,
        "symbols_entries": 15,
    }
    dump_json(rules_path, rules)
    dump_jsonl(log_path, log_rows)

    report_path = os.path.join(POOL, "structural-report.json")
    with io.open(report_path, encoding="utf-8") as fh:
        report = json.load(fh)
    report["formula_notation_fix"] = {
        "applied": "2026-11-12",
        "artifacts": ["formula-notation-fix-log.jsonl",
                      "formula-notation-fix-rules.json"],
        "derivation_entries": 2,
        "entries_fixed": 18,
        "root_cause": "stage-2 formula extractor collapses products into "
                      "single tokens, embeds degree/dagger/hat marks and "
                      "natural-language tails, and omits multiplication signs",
        "si_units_touched": False,
        "symbols_entries": 15,
    }
    dump_json(report_path, report)

    out_lines = []
    for name in MANIFEST_NAMES:
        out_lines.append("%s  %s" % (sha256_file(os.path.join(POOL, name)), name))
    combined = hashlib.sha256("\n".join(out_lines).encode("utf-8")).hexdigest()
    out_lines.append("%s  COMBINED" % combined)
    with io.open(os.path.join(POOL, "manifest.sha256"), "w", encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(out_lines) + "\n")

    print("fixed %d entries (symbols: 15, derivation: 2, si_units untouched); "
          "manifest re-anchored (%d files)" % (len(FIXES), len(MANIFEST_NAMES)))


if __name__ == "__main__":
    sys.exit(main())
