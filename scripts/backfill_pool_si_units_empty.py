#!/usr/bin/env python3
"""Deterministic si_units backfill for the reviewed empty-coverage pool entries
(empty-si_units workbook, second split, dimension-pure subset).

Applies the human-reviewed unit table for 16 entries whose si_units maps were
empty (approved via the precheck workbook on 2026-11-12; candidate derivation:
pool-internal reviewed-value priors with vector-cluster disambiguation,
symbol-level semantic judgments anchored to previously approved precedents,
and algebraic back-substitution). The remaining empty entries stay untouched
pending the review of their open questions.

Single source: the REVIEWED_EMPTY_SI_UNITS constant below, frozen from the
approved precheck tables. Sidecars a per-entry provenance log + rules table,
refreshes the structural report, and re-anchors the pool manifest.

Pure deterministic transformation: no network, no clock, no randomness;
byte-stable output for identical input. Line edits are surgical: only the
si_units value substring changes inside affected lines; all other lines are
preserved byte-for-byte.
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

REVIEWED_EMPTY_SI_UNITS = {
    "chem_ana_emp_01": {
        "A": "s^-1",
        "epsilonbc": "s^-1"
    },
    "chem_ele_thm_02": {
        "F": "s*A*mol^-1",
        "I": "A",
        "M": "kg*mol^-1",
        "m": "kg",
        "n": "1",
        "t": "s"
    },
    "chem_sol_prin_01": {
        "C": "m^-3*mol",
        "P": "kg*m^-1*s^-2",
        "k": "kg^-1*m^-2*s^2*mol"
    },
    "chem_sol_thm_01": {
        "DeltaP": "kg*m^-1*s^-2",
        "P_0": "kg*m^-1*s^-2",
        "x_2": "1"
    },
    "chem_sol_thm_02": {
        "iCRT": "1",
        "pi": "1"
    },
    "math_calc_derivative_ln": {
        "d": "m",
        "x": "m^-1"
    },
    "math_calc_derivative_ln_dup2": {
        "d": "m",
        "x": "m^-1"
    },
    "phy_ast_con_01": {
        "E": "kg*m^2*s^-2",
        "mc2": "kg*m^2*s^-2"
    },
    "phy_mag_law_01": {
        "M": "kg",
        "chiH": "kg"
    },
    "phy_opt_law_01": {
        "n1": "1",
        "n2": "1",
        "sin1": "1",
        "sin2": "1"
    },
    "phy_opt_law_02": {
        "theta1": "1",
        "theta2": "1"
    },
    "phy_opt_law_03": {
        "I": "A",
        "I0cos2theta": "A"
    },
    "phy_opt_prin_04": {
        "n1": "1",
        "n2": "1",
        "tantheta_B": "1"
    },
    "phy_rel_app_02_p1": {
        "dt": "s",
        "dt1": "s",
        "gamma": "1"
    },
    "phy_rel_app_04": {
        "gammam0v": "kg*m*s^-1",
        "p": "kg*m*s^-1"
    },
    "phy_wav_law_01": {
        "lambdaf": "m*s^-1",
        "v": "m*s^-1"
    }
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


def inline_units(units):
    return json.dumps({k: units[k] for k in sorted(units)}, ensure_ascii=False)


def main():
    rules_path = os.path.join(POOL, "si-units-empty-backfill-rules.json")
    log_path = os.path.join(POOL, "si-units-empty-backfill-log.jsonl")
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
    for tid in REVIEWED_EMPTY_SI_UNITS:
        assert tid in rows, "target missing: %s" % tid

    log_rows = []
    changed = 0
    for tid in sorted(REVIEWED_EMPTY_SI_UNITS):
        idx, row = rows[tid]
        line = lines[idx]
        before = row.get("si_units") or {}
        assert before == {}, "target si_units unexpectedly non-empty: %s" % tid
        full = REVIEWED_EMPTY_SI_UNITS[tid]
        marker = '"si_units": ' + inline_units(before)
        assert line.count(marker) == 1, (
            "expected exactly one empty-si_units match in %s" % tid)
        new_line = line.replace(marker, '"si_units": ' + inline_units(full))
        lines[idx] = new_line
        changed += 1
        log_rows.append({
            "after": {k: full[k] for k in sorted(full)},
            "before": {},
            "engine_verdict": "consistent",
            "field": "si_units",
            "file": "normalized-law-pool.jsonl",
            "id": tid,
            "reviewed": "2026-11-12",
            "rule": "human-reviewed candidate fill for empty si_units "
                    "(dimension-pure subset; single source REVIEWED_EMPTY_SI_UNITS)",
        })

    assert changed == len(REVIEWED_EMPTY_SI_UNITS)

    text = "\n".join(lines) + ("\n" if had_final_nl else "")
    with io.open(POOL_FILE, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)

    rules = {
        "notation": {
            "dimensionless": '"1" (engine has no "dimensionless" token)',
            "form": "coherent SI symbols or base-unit power products (e.g. m*s^-1)",
            "prefixes": "none (SI prefixes unsupported by the unit engine)",
        },
        "reviewed": "2026-11-12",
        "reviewed_units": {tid: {s: v for s, v in sorted(m.items())}
                           for tid, m in sorted(REVIEWED_EMPTY_SI_UNITS.items())},
        "rule": "human-reviewed candidate fills for empty si_units entries "
                "(dimension-pure subset, second workbook split); derivation = "
                "pool-internal reviewed-value priors (vector-cluster disambiguation, "
                "subdomain/domain/global layers, underscore-variant normalization) + "
                "symbol-level semantic judgments anchored to previously approved "
                "precedents + algebraic back-substitution; constant-equation forms "
                "(rhs is a bare number) are excluded and stay under review",
        "unit_engine_precheck": "consistent 16/16 (dual-run SHA256-equal harness "
                                "evidence); zero mismatch; 4 formula-face fragment "
                                "defects and 53 open questions remain under review",
    }
    dump_json(rules_path, rules)
    dump_jsonl(log_path, log_rows)

    report_path = os.path.join(POOL, "structural-report.json")
    with io.open(report_path, encoding="utf-8") as fh:
        report = json.load(fh)
    empty_before = report["known_gaps"]["si_units_empty"]
    report["known_gaps"]["si_units_empty"] = empty_before - len(REVIEWED_EMPTY_SI_UNITS)
    report["si_units_empty_backfill"] = {
        "artifacts": ["si-units-empty-backfill-log.jsonl",
                      "si-units-empty-backfill-rules.json"],
        "entries_backfilled": len(REVIEWED_EMPTY_SI_UNITS),
        "notation": rules["notation"],
        "reviewed": "2026-11-12",
        "unit_engine_precheck": rules["unit_engine_precheck"],
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

    print("backfilled %d entries; manifest re-anchored (%d files)" % (changed, len(names)))


if __name__ == "__main__":
    sys.exit(main())
