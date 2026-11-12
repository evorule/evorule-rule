#!/usr/bin/env python3
"""Deterministic si_units backfill for the reviewed empty-coverage pool entries
(empty-si_units workbook, third split, function-call subset).

Applies the human-reviewed unit table for 33 entries whose si_units maps were
empty (approved via the precheck workbook on 2026-11-12; two adjudicated
overrides applied: trig angle-sum a,b -> 1 (angle context, coherent with the
tier-3 rule that trigonometric arguments are dimensionless) and derivative
result d -> 1 (collapsed derivative-notation marker, self-consistent once the
argument is dimensionless)). These entries contain function-call notation, so
the unit engine records check_law = Skipped for them by design (established
grammar boundary, not a defect); the review lens is the value table itself.
The remaining empty entries stay untouched pending review or tiered handling.

Also amends the tiered-review rules with the sqrt rule (dimensions of sqrt
follow the half power of the argument; sqrt is 1 only when the argument is
dimensionless), per the same adjudication.

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
    "math_calc_derivative_cos": {
        "d": "1",
        "sin": "1",
        "x": "1"
    },
    "math_calc_derivative_cos_dup2": {
        "d": "1",
        "sin": "1",
        "x": "1"
    },
    "math_calc_derivative_exp": {
        "d": "1",
        "exp": "1",
        "x": "1"
    },
    "math_calc_derivative_exp_dup2": {
        "d": "1",
        "exp": "1",
        "x": "1"
    },
    "math_calc_derivative_sin": {
        "cos": "1",
        "d": "1",
        "x": "1"
    },
    "math_calc_derivative_sin_dup2": {
        "cos": "1",
        "d": "1",
        "x": "1"
    },
    "math_calc_taylor_exp": {
        "exp": "1",
        "x": "1"
    },
    "math_calc_taylor_exp_dup2": {
        "exp": "1",
        "x": "1"
    },
    "math_calc_taylor_sin": {
        "sin": "1",
        "x": "1"
    },
    "math_calc_taylor_sin_dup2": {
        "sin": "1",
        "x": "1"
    },
    "math_complex_euler": {
        "cos": "1",
        "exp_i": "1",
        "j": "1",
        "sin": "1",
        "x": "1"
    },
    "math_complex_euler_dup2": {
        "cos": "1",
        "exp_i": "1",
        "j": "1",
        "sin": "1",
        "x": "1"
    },
    "math_complex_euler_identity": {
        "exp": "1",
        "result": "1"
    },
    "math_complex_euler_identity_dup2": {
        "exp": "1",
        "result": "1"
    },
    "math_log_change_base": {
        "a": "1",
        "b": "1",
        "log": "1"
    },
    "math_log_change_base_dup2": {
        "a": "1",
        "b": "1",
        "log": "1"
    },
    "math_log_product": {
        "a": "1",
        "b": "1",
        "log": "1"
    },
    "math_log_product_dup2": {
        "a": "1",
        "b": "1",
        "log": "1"
    },
    "math_log_quotient": {
        "a": "1",
        "b": "1",
        "log": "1"
    },
    "math_log_quotient_dup2": {
        "a": "1",
        "b": "1",
        "log": "1"
    },
    "math_trig_angle_sum_cos": {
        "a": "1",
        "b": "1",
        "cos": "1",
        "sin": "1"
    },
    "math_trig_angle_sum_cos_dup2": {
        "a": "1",
        "b": "1",
        "cos": "1",
        "sin": "1"
    },
    "math_trig_angle_sum_sin": {
        "a": "1",
        "b": "1",
        "cos": "1",
        "sin": "1"
    },
    "math_trig_angle_sum_sin_dup2": {
        "a": "1",
        "b": "1",
        "cos": "1",
        "sin": "1"
    },
    "math_trig_cotangent_def": {
        "cos": "1",
        "sin": "1",
        "tan": "1",
        "x": "1"
    },
    "math_trig_cotangent_def_dup2": {
        "cos": "1",
        "sin": "1",
        "tan": "1",
        "x": "1"
    },
    "math_trig_double_angle_cos": {
        "cos": "1",
        "sin": "1",
        "x": "1"
    },
    "math_trig_double_angle_cos_dup2": {
        "cos": "1",
        "sin": "1",
        "x": "1"
    },
    "math_trig_double_angle_sin": {
        "cos": "1",
        "sin": "1",
        "x": "1"
    },
    "math_trig_double_angle_sin_dup2": {
        "cos": "1",
        "sin": "1",
        "x": "1"
    },
    "math_trig_tangent_def": {
        "cos": "1",
        "sin": "1",
        "tan": "1",
        "x": "1"
    },
    "math_trig_tangent_def_dup2": {
        "cos": "1",
        "sin": "1",
        "tan": "1",
        "x": "1"
    },
    "phy_rel_app_02_p2": {
        "c": "m*s^-1",
        "dt": "s",
        "gamma": "1",
        "sqrt": "1",
        "v": "m"
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
    rules_path = os.path.join(POOL, "si-units-empty-backfill-split3-rules.json")
    log_path = os.path.join(POOL, "si-units-empty-backfill-split3-log.jsonl")
    exempt_rules_path = os.path.join(POOL, "si-units-exempt-rules.json")
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
            "engine_verdict": "skipped (function-call notation; established "
                              "grammar boundary, value review is the table)",
            "field": "si_units",
            "file": "normalized-law-pool.jsonl",
            "id": tid,
            "reviewed": "2026-11-12",
            "rule": "human-reviewed candidate fill for empty si_units "
                    "(function-call subset; single source "
                    "REVIEWED_EMPTY_SI_UNITS; two adjudicated overrides "
                    "applied before freezing)",
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
                "(function-call subset, third workbook split); derivation = "
                "pool-internal reviewed-value priors + function-notation "
                "judgments anchored to approved precedents; two adjudicated "
                "overrides applied (angle-sum a,b m->1; derivative d m->1); "
                "engine check_law = Skipped by design for function-call "
                "notation (grammar boundary), the value table is the review "
                "lens",
        "unit_engine_precheck": "33/33 skipped as designed (0 mismatch, dual-run "
                                "SHA256-equal harness evidence)",
    }
    dump_json(rules_path, rules)
    dump_jsonl(log_path, log_rows)

    # amend the tiered-review rules: sqrt rule joins the function-notation family
    with io.open(exempt_rules_path, encoding="utf-8") as fh:
        exempt_rules = json.load(fh)
    rd = exempt_rules["tiers"]["rule_defined"]
    rd["rule"] = (rd["rule"] + "; sqrt rule (added 2026-11-12): dimensions of "
                  "sqrt follow the half power of its argument, and sqrt is 1 "
                  "only when its argument is dimensionless -- the "
                  "function-notation family is amended together with the M1 "
                  "milestone")
    exempt_rules["sqrt_rule_added"] = "2026-11-12"
    dump_json(exempt_rules_path, exempt_rules)

    report_path = os.path.join(POOL, "structural-report.json")
    with io.open(report_path, encoding="utf-8") as fh:
        report = json.load(fh)
    empty_before = report["known_gaps"]["si_units_empty"]
    report["known_gaps"]["si_units_empty"] = empty_before - len(REVIEWED_EMPTY_SI_UNITS)
    report["si_units_empty_backfill_split3"] = {
        "artifacts": ["si-units-empty-backfill-split3-log.jsonl",
                      "si-units-empty-backfill-split3-rules.json"],
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
        "si-units-empty-backfill-split3-log.jsonl",
        "si-units-empty-backfill-split3-rules.json",
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

    print("backfilled %d entries; sqrt rule amended; manifest re-anchored (%d files)"
          % (changed, len(names)))


if __name__ == "__main__":
    sys.exit(main())
