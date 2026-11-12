#!/usr/bin/env python3
"""Deterministic notation normalization for si_units values in the migration pool.

Rewrites unit-engine-unparseable si_units notation into engine-canonical
coherent-SI form (declared rule table below), sidecars a per-value
transformation log + the frozen rules table, refreshes the structural
report, and re-anchors the pool manifest.

Pure deterministic transformation: no network, no clock, no randomness;
byte-stable output for identical input. Line edits are surgical: only the
si_units value substring changes inside affected lines; all other lines are
preserved byte-for-byte. Values that already parse in the unit engine are
never touched. Symbolic-variable exponents (e.g. Deltan) are deferred
untouched and logged as such; they need a human-reviewed decision, not a
mechanical rewrite.

Every rewrite target form listed in REWRITE_MAP was probe-verified against
the real unit engine (evorule-law-schema dimensional parser) before this
table was frozen; the post-rewrite acceptance census must show exactly the
deferred values as the only remaining parse failures.
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

RULE_CLASSES = ("notation-resyntax", "dimension-equivalent")

# unit_engine-unparseable notation -> (engine-canonical form, rule class,
# rationale). Frozen rule table; every target form probe-verified parseable.
REWRITE_MAP = {
    # -- notation resyntax (mechanical re-spelling, same dimension) --
    "dimensionless": ("1", "notation-resyntax",
                      "dimension-one values are recorded as \"1\"; the engine has no dimensionless token"),
    "m2": ("m^2", "notation-resyntax", "power notation: caret syntax only"),
    "m3": ("m^3", "notation-resyntax", "power notation: caret syntax only"),
    "s-1": ("s^-1", "notation-resyntax", "trailing negative exponent rewritten to caret syntax"),
    "mol-1": ("mol^-1", "notation-resyntax", "trailing negative exponent rewritten to caret syntax"),
    "m/s2": ("m*s^-2", "notation-resyntax", "slash division and trailing exponent to negative-power product"),
    "m2/s": ("m^2*s^-1", "notation-resyntax", "slash division to negative-power product"),
    "N*m2/kg2": ("N*m^2*kg^-2", "notation-resyntax", "inline powers and slash division to caret product"),
    "N*m2/C2": ("N*m^2*C^-2", "notation-resyntax", "inline powers and slash division to caret product"),
    "N*m2/C": ("N*m^2*C^-1", "notation-resyntax", "inline powers and slash division to caret product"),
    "mol/(m2*s)": ("mol*m^-2*s^-1", "notation-resyntax", "slash division to negative-power product"),
    "mol/m4": ("mol*m^-4", "notation-resyntax", "slash division to negative-power product"),
    "1/m2": ("m^-2", "notation-resyntax", "unit numerator 1 is a dimension-one factor, dropped"),
    "1/s2": ("s^-2", "notation-resyntax", "unit numerator 1 is a dimension-one factor, dropped"),
    "J/m2": ("J*m^-2", "notation-resyntax", "slash division to negative-power product"),
    "W/(m2*K4)": ("W*m^-2*K^-4", "notation-resyntax", "parenthesized divisor to negative-power product"),
    "kJ/mol": ("J*mol^-1", "notation-resyntax", "coherent SI: drop kilo prefix; slash division to negative power"),
    "kJ/(mol*K)": ("J*mol^-1*K^-1", "notation-resyntax", "coherent SI: drop kilo prefix; divisor to negative powers"),
    "km": ("m", "notation-resyntax", "coherent SI: drop kilo prefix"),
    "km/s": ("m*s^-1", "notation-resyntax", "coherent SI: drop kilo prefix; slash division to negative power"),
    "Omega": ("ohm", "notation-resyntax", "ohm is the only accepted ohm spelling in the engine vocabulary"),
    # -- dimension-equivalent substitution (engine vocabulary lacks the unit;
    #    target preserves the exact dimension vector) --
    "°C": ("K", "dimension-equivalent",
           "celsius temperature has the kelvin dimension vector; degree sign is a non-ASCII token"),
    "°C/km": ("K*m^-1", "dimension-equivalent",
              "celsius temperature has the kelvin dimension vector; slash division to negative power"),
    "bit": ("1", "dimension-equivalent", "information quantities are dimensionless in the SI convention"),
    "bits": ("1", "dimension-equivalent", "information quantities are dimensionless in the SI convention"),
    "bit/s": ("s^-1", "dimension-equivalent",
              "information quantities are dimensionless in the SI convention; slash division to negative power"),
    "bits/s": ("s^-1", "dimension-equivalent",
               "information quantities are dimensionless in the SI convention; slash division to negative power"),
    "Mpc": ("m", "dimension-equivalent", "parsec is a length unit; dimension vector equals meter"),
    "km/(s*Mpc)": ("s^-1", "dimension-equivalent",
                   "length cancels between kilometer and megaparsec, leaving time^-1"),
}

# symbolic-variable exponents: not a fixed unit, needs human review; untouched.
DEFER_MAP = {
    "atm^Deltan": "symbolic variable exponent (Deltan) is not a fixed unit; deferred for human review",
    "(mol/L)^Deltan": "symbolic variable exponent (Deltan) is not a fixed unit; deferred for human review",
}

# target forms probe-verified against the real unit engine before freezing
PROBE_VERIFIED = {
    "1", "m^2", "m^3", "m*s^-2", "m^2*s^-1", "N*m^2*kg^-2", "N*m^2*C^-2",
    "N*m^2*C^-1", "mol^-1", "J*mol^-1", "J*mol^-1*K^-1", "mol*m^-2*s^-1",
    "mol*m^-4", "m^-2", "s^-1", "s^-2", "J*m^-2", "W*m^-2*K^-4", "m",
    "m*s^-1", "K", "K*m^-1", "ohm",
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
    """Serialize an si_units map exactly in the pool line style (sorted keys,
    default separators, raw non-ASCII) for surgical substring replacement."""
    return json.dumps({k: units[k] for k in sorted(units)}, ensure_ascii=False)


def main():
    rules_path = os.path.join(POOL, "si-units-notation-normalize-rules.json")
    log_path = os.path.join(POOL, "si-units-notation-normalize-log.jsonl")
    if os.path.exists(rules_path) or os.path.exists(log_path):
        sys.exit("sidecars already exist; rerun refused (idempotency guard)")

    for after, _, _ in REWRITE_MAP.values():
        assert after in PROBE_VERIFIED, "rewrite target not probe-verified: %r" % after

    with io.open(POOL_FILE, encoding="utf-8", newline="") as f:
        lines = f.read().split("\n")
    had_final_nl = lines and lines[-1] == ""
    if had_final_nl:
        lines = lines[:-1]

    rows = []
    for idx, line in enumerate(lines):
        if not line.strip():
            continue
        rows.append((idx, json.loads(line)))
    assert len(rows) == 409, "unexpected pool size: %d" % len(rows)

    log_rows = []
    entries_touched = 0
    values_rewritten = 0
    values_deferred = 0
    for idx, row in rows:
        si = row.get("si_units") or {}
        if not si:
            continue
        before_map = {k: si[k] for k in sorted(si)}
        after_map = dict(before_map)
        line_logs = []
        for sym in sorted(si):
            unit = si[sym]
            if unit in REWRITE_MAP:
                after, cls, rationale = REWRITE_MAP[unit]
                after_map[sym] = after
                line_logs.append({
                    "after": after,
                    "before": unit,
                    "field": "si_units",
                    "file": "normalized-law-pool.jsonl",
                    "id": row["id"],
                    "rationale": rationale,
                    "rule": "%s: %s -> %s" % (cls, unit, after),
                    "rule_class": cls,
                    "symbol": sym,
                })
            elif unit in DEFER_MAP:
                line_logs.append({
                    "action": "deferred",
                    "before": unit,
                    "field": "si_units",
                    "file": "normalized-law-pool.jsonl",
                    "id": row["id"],
                    "reason": DEFER_MAP[unit],
                    "symbol": sym,
                })
        if not line_logs:
            continue
        old_marker = '"si_units": ' + inline_units(before_map)
        assert lines[idx].count(old_marker) == 1, (
            "expected exactly one si_units match in %s" % row["id"])
        new_marker = '"si_units": ' + inline_units(after_map)
        assert new_marker != old_marker or any(r.get("action") == "deferred" for r in line_logs)
        lines[idx] = lines[idx].replace(old_marker, new_marker)
        log_rows.extend(line_logs)
        entries_touched += 1
        values_rewritten += sum(1 for r in line_logs if "after" in r)
        values_deferred += sum(1 for r in line_logs if r.get("action") == "deferred")

    assert values_rewritten > 0, "no notation targets found in pool"
    log_rows.sort(key=lambda r: (r["id"], r["symbol"]))

    text = "\n".join(lines) + ("\n" if had_final_nl else "")
    with io.open(POOL_FILE, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)

    notation_block = {
        "dimensionless": '"1" (engine has no "dimensionless" token)',
        "form": "coherent SI symbols or base-unit power products (e.g. m*s^-1)",
        "prefixes": "none (SI prefixes unsupported by the unit engine)",
        "reserved_spellings": {"ohm": "ohm is the only accepted ohm spelling"},
    }
    rules = {
        "applied": "2026-11-12",
        "deferred": {k: v for k, v in sorted(DEFER_MAP.items())},
        "notation": notation_block,
        "probe_verified_forms": sorted(PROBE_VERIFIED),
        "rule": "deterministic notation rewrite of unit-engine-unparseable si_units "
                "values into engine-canonical coherent-SI form; values already parseable "
                "in the unit engine are never touched; symbolic-variable exponents are "
                "deferred untouched for human review",
        "rule_classes": {
            "dimension-equivalent": "engine vocabulary lacks the unit; rewrite target preserves "
                                    "the exact dimension vector",
            "notation-resyntax": "mechanical re-spelling of the same dimension (coherent SI form)",
        },
        "rewrite_map": {k: {"after": v[0], "class": v[1], "rationale": v[2]}
                        for k, v in sorted(REWRITE_MAP.items())},
    }
    dump_json(rules_path, rules)
    dump_jsonl(log_path, log_rows)

    # structural report: append the normalization section
    report_path = os.path.join(POOL, "structural-report.json")
    with io.open(report_path, encoding="utf-8") as fh:
        report = json.load(fh)
    report["si_units_notation_normalize"] = {
        "applied": "2026-11-12",
        "artifacts": ["si-units-notation-normalize-log.jsonl",
                      "si-units-notation-normalize-rules.json"],
        "census_before": {"parse_fail": 160, "parse_ok": 468, "unit_values": 628},
        "entries_touched": entries_touched,
        "notation": notation_block,
        "rule_classes": sorted(RULE_CLASSES),
        "values_deferred": values_deferred,
        "values_rewritten": values_rewritten,
    }
    dump_json(report_path, report)

    # manifest re-anchor (file list kept alphabetical)
    names = [
        "a2-derivation-log.jsonl",
        "a2-derivation-rules.json",
        "excluded-non-identity-forms.jsonl",
        "input-law-pool.jsonl",
        "normalization-rules.json",
        "normalized-law-pool.jsonl",
        "provenance-law-pool.jsonl",
        "si-units-backfill-log.jsonl",
        "si-units-backfill-rules.json",
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

    print("normalized %d si_units values across %d pool entries "
          "(deferred untouched: %d); manifest re-anchored (%d files)"
          % (values_rewritten, entries_touched, values_deferred, len(names)))


if __name__ == "__main__":
    sys.exit(main())
