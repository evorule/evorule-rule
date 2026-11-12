#!/usr/bin/env python3
"""Deterministic symbol-face cleanup for the normalized law pool.

The stage-2 symbol extractor (R9 family) mixes physical-quantity symbols with
unit tokens and chemical species. A full-pool census (409 rows, 398 distinct
symbol keys, 2026-11-12) found exactly four rows whose symbols object carries
contamination; every other suspect token is a legitimate quantity symbol that
merely collides with a unit abbreviation (M = molar mass / Lagrangian / Turing
machine, L = length / inductance, psi = wavefunction / Euler angle, I0/K1/H0 =
subscripted quantities, H_plus/rate/depth/Ksp/K_m = domain-verified literal
symbols, several with human-reviewed si_units already in the pool).

Fixes applied (approved 2026-11-12, plan A + literal-symbol extension):

1. chem_bio_rul_01  symbols: drop species keys ATP/ADP/H2O/Pi and unit keys
   kJ/mol; keep DeltaG. Formula untouched (its two-segment comma join is a
   formula-face artifact, deferred to the formula-repair batch).
2. phy_atm_law_01   symbols: drop the eV pseudo-symbol; formula loses the
   "*eV" factor, keeping the embedded constant (-13.6), aligned with the
   phys_quantum_energy_level precedent (embedded constant in formula, unit on
   the si_units review track).
3. chem_sto_emp_01  symbols: drop preposition residue keys at/same. Formula
   untouched (natural-language sentence, deferred to the formula-repair batch).
4. chem_pcd_thm_03  subscript collapse repair: E/anode/cathode/cell become
   E_cell/E_cathode/E_anode; formula rewritten to E_cell = E_cathode - E_anode.
   The degree-mark standard-state notation is dropped here and recorded in the
   log; pool-wide degree-mark normalization stays with the formula-repair batch.

math_complex_euler_identity keeps its "result" symbol: it is a legitimate
output-variable name with a human-reviewed si_units value already attached
("1"); renaming would churn the reviewed value face for zero gain.

Deferred to the formula-repair batch (registered, not fixed here): the
collapsed-product family (22 rows: kBT/RT2/m0c2/rhogh/Gm1m2/N0e/V0e/iCRT/Idl/
mu0I/hnu/pihnv3/pikT/HPsi/gammam0v/kDeltau/Sigma_ as symbol keys plus RT/lnQ/nF
formula literals), the chem_bio_rul_01 comma-joined two-segment formula, the
chem_sto_emp_01 natural-language formula, and degree-mark normalization.

Sidecars a per-entry transformation log + the frozen rules table, refreshes
the structural report, and re-anchors the pool manifest.

Pure deterministic transformation: no network, no clock, no randomness;
byte-stable output for identical input. Line edits are surgical: only the
formula / symbols value substrings change inside affected lines; all other
lines are preserved byte-for-byte.
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

# id -> (symbols_before, symbols_after, formula_before or None, formula_after or None)
FIXES = {
    "chem_bio_rul_01": (
        {"ADP": "", "ATP": "", "DeltaG": "", "H2O": "", "Pi": "", "kJ": "", "mol": ""},
        {"DeltaG": ""},
        None,
        None,
    ),
    "phy_atm_law_01": (
        {"E_n": "", "eV": "", "n2": ""},
        {"E_n": "", "n2": ""},
        "E_n = -13.6*eV / n2",
        "E_n = -13.6 / n2",
    ),
    "chem_sto_emp_01": (
        {"P": "", "T": "", "V": "", "at": "", "constant": "", "n": "", "same": ""},
        {"P": "", "T": "", "V": "", "constant": "", "n": ""},
        None,
        None,
    ),
    "chem_pcd_thm_03": (
        {"E": "", "anode": "", "cathode": "", "cell": ""},
        {"E_anode": "", "E_cathode": "", "E_cell": ""},
        "E°cell = E°cathode - E°anode",
        "E_cell = E_cathode - E_anode",
    ),
}

LOG_NOTES = {
    "chem_bio_rul_01": (
        "species keys (ATP/ADP/H2O/Pi) and unit keys (kJ/mol) removed from the "
        "symbol face; species semantics stay in the formula text; the unit "
        "value -30.5 kJ/mol and the DeltaG unit decision belong to the "
        "si_units review track; the comma-joined two-segment formula is a "
        "formula-face artifact deferred to the formula-repair batch"),
    "phy_atm_law_01": (
        "eV pseudo-symbol removed from symbols and formula; embedded constant "
        "-13.6 kept, aligned with the phys_quantum_energy_level precedent "
        "(E = -13.6 / n ^ 2, unit on the si_units face)"),
    "chem_sto_emp_01": (
        "preposition residue keys at/same removed; the natural-language "
        "formula sentence (constant at same T, P) is deferred to the "
        "formula-repair batch"),
    "chem_pcd_thm_03": (
        "subscript collapse repaired: E/anode/cathode/cell reconstructed as "
        "E_cell/E_cathode/E_anode; the degree-mark standard-state notation is "
        "dropped and recorded here, pool-wide degree-mark normalization is "
        "deferred to the formula-repair batch"),
}

MANIFEST_NAMES = [
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
    "si-units-empty-backfill-split3-log.jsonl",
    "si-units-empty-backfill-split3-rules.json",
    "si-units-empty-backfill-split4-log.jsonl",
    "si-units-empty-backfill-split4-rules.json",
    "si-units-exempt-entries.jsonl",
    "si-units-exempt-rules.json",
    "si-units-notation-normalize-log.jsonl",
    "si-units-notation-normalize-rules.json",
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
    """Serialize a map exactly in the pool line style (sorted keys, default
    separators, raw non-ASCII) for surgical substring replacement."""
    return json.dumps({k: mapping[k] for k in sorted(mapping)}, ensure_ascii=False)


def replace_once(line, old, new, tid, what):
    assert line.count(old) == 1, (
        "expected exactly one %s match in %s" % (what, tid))
    return line.replace(old, new)


def main():
    rules_path = os.path.join(POOL, "symbols-cleanup-rules.json")
    log_path = os.path.join(POOL, "symbols-cleanup-log.jsonl")
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
        sym_before, sym_after, f_before, f_after = FIXES[tid]
        idx, row = rows[tid]
        line = lines[idx]

        assert row["symbols"] == sym_before, (
            "symbols drifted from frozen expectation: %s" % tid)
        assert row.get("si_units") == {}, (
            "target entry unexpectedly has si_units: %s" % tid)
        if f_before is None:
            assert "symbols" in line and "formula" in line

        line = replace_once(line, '"symbols": ' + inline_map(sym_before),
                            '"symbols": ' + inline_map(sym_after), tid, "symbols")
        if f_before is not None:
            assert row["formula"] == f_before, (
                "formula drifted from frozen expectation: %s" % tid)
            line = replace_once(line, '"formula": ' + json.dumps(f_before, ensure_ascii=False),
                                '"formula": ' + json.dumps(f_after, ensure_ascii=False),
                                tid, "formula")

        entry_log = {
            "field": "symbols" if f_before is None else "symbols+formula",
            "file": "normalized-law-pool.jsonl",
            "id": tid,
            "note": LOG_NOTES[tid],
            "symbols_after": {k: sym_after[k] for k in sorted(sym_after)},
            "symbols_before": {k: sym_before[k] for k in sorted(sym_before)},
        }
        if f_before is not None:
            entry_log["formula_after"] = f_after
            entry_log["formula_before"] = f_before
        lines[idx] = line
        log_rows.append(entry_log)

    text = "\n".join(lines) + ("\n" if had_final_nl else "")
    with io.open(POOL_FILE, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)

    rules = {
        "applied": "2026-11-12",
        "deferred_to_formula_repair_batch": [
            "collapsed-product family: 22 rows with kBT/RT2/m0c2/rhogh/Gm1m2/"
            "N0e/V0e/iCRT/Idl/mu0I/hnu/pihnv3/pikT/HPsi/gammam0v/kDeltau/Sigma_ "
            "symbol keys and RT/lnQ/nF formula literals",
            "chem_bio_rul_01: comma-joined two-segment formula (reaction "
            "equation + value assignment)",
            "chem_sto_emp_01: natural-language formula sentence",
            "degree-mark normalization (chem_app_thm_01, chem_bio_rul_01, "
            "chem_ele_thm_01; dropped in chem_pcd_thm_03 by this pass)",
        ],
        "entries_fixed": 4,
        "kept_with_rationale": {
            "math_complex_euler_identity": (
                "result symbol kept: legitimate output-variable name with a "
                "human-reviewed si_units value (\"1\") already attached; "
                "renaming would churn the reviewed value face"),
            "single-letter-quantity collisions": (
                "M/L/psi/I0/K1/H0/P0-style keys are legitimate subscripted or "
                "domain-standard quantity symbols, several with reviewed "
                "si_units in the pool; not contamination"),
        },
        "keys_added": 3,
        "keys_removed": 13,
        "rule": "symbol face carries physical-quantity symbols only: unit "
                "tokens and chemical species are removed (their semantics stay "
                "in the formula text; unit values belong to the si_units "
                "review track); preposition residue keys are removed; "
                "subscript-collapsed quantity names are reconstructed and the "
                "formula reference updated in lockstep; embedded numeric "
                "constants are kept, aligned with the "
                "phys_quantum_energy_level precedent",
    }
    dump_json(rules_path, rules)
    dump_jsonl(log_path, log_rows)

    # structural report: new pass section
    report_path = os.path.join(POOL, "structural-report.json")
    with io.open(report_path, encoding="utf-8") as fh:
        report = json.load(fh)
    report["symbols_cleanup"] = {
        "applied": "2026-11-12",
        "artifacts": ["symbols-cleanup-log.jsonl", "symbols-cleanup-rules.json"],
        "census_basis": "409 rows, 398 distinct symbol keys (2026-11-12)",
        "entries_fixed": 4,
        "formula_entries_touched": 2,
        "keys_added": 3,
        "keys_removed": 13,
        "root_cause": "stage-2 symbol extractor (R9 family) mixes physical-"
                      "quantity symbols with unit tokens, chemical species, "
                      "preposition residue, and subscript-collapsed names",
    }
    dump_json(report_path, report)

    # manifest re-anchor (file list kept alphabetical)
    out_lines = []
    for name in MANIFEST_NAMES:
        out_lines.append("%s  %s" % (sha256_file(os.path.join(POOL, name)), name))
    combined = hashlib.sha256("\n".join(out_lines).encode("utf-8")).hexdigest()
    out_lines.append("%s  COMBINED" % combined)
    with io.open(os.path.join(POOL, "manifest.sha256"), "w", encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(out_lines) + "\n")

    print("cleaned %d entries (formula touched: 2); manifest re-anchored (%d files)"
          % (len(FIXES), len(MANIFEST_NAMES)))


if __name__ == "__main__":
    sys.exit(main())
