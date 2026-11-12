#!/usr/bin/env python3
"""Deterministic formula repair for and-artifact chain-split products.

The stage-2 chain splitter (R9) lacks the word connective "and" in its
statement-marker table, so Python-style and-joined equations
("a == b and c == d") were chain-split into overlapping pairs whose formulas
carry " and " fragments. This pass restores each product's formula to the
exact single equation it represents, derived mechanically from the two
products of the same parent:

    equation_1 = p1.formula before " and "
    equation_2 = p2.formula after " and "

with two cross-checks per pair (p1's " and "-right fragment must equal
equation_2's left-hand side; p2's " and "-left fragment must equal
equation_1's right-hand side). The "and" pseudo-symbol is removed from the
symbols object. For the eight geometry/trigonometry entries the
human-reviewed candidate values (approved 2026-11-12, recorded in the
precheck tables: s=m, sin=1, sqrt=1) are applied alongside, completing their
si_units maps; the six empty-si_units entries stay untouched on the value
face (they belong to the empty-si_units precheck track).

Sidecars a per-entry transformation log + the frozen rules table, refreshes
the structural report, and re-anchors the pool manifest.

Pure deterministic transformation: no network, no clock, no randomness;
byte-stable output for identical input. Line edits are surgical: only the
formula / symbols / si_units value substrings change inside affected lines;
all other lines are preserved byte-for-byte.
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

# parent id -> (p1 product id, p2 product id); 7 pairs = 14 entries
PAIRS = {
    "phy_rel_app_02": ("phy_rel_app_02_p1", "phy_rel_app_02_p2"),
    "math_geom_heron": ("math_geom_heron_p1", "math_geom_heron_p2"),
    "math_geom_heron_dup2": ("math_geom_heron_p1_dup2", "math_geom_heron_p2_dup2"),
    "math_trig_law_of_sines": ("math_trig_law_of_sines_p1", "math_trig_law_of_sines_p2"),
    "math_trig_law_of_sines_dup2": (
        "math_trig_law_of_sines_p1_dup2", "math_trig_law_of_sines_p2_dup2"),
    "math_calc_fundamental_theorem": (
        "math_calc_fundamental_theorem_p1", "math_calc_fundamental_theorem_p2"),
    "math_calc_fundamental_theorem_dup2": (
        "math_calc_fundamental_theorem_p1_dup2", "math_calc_fundamental_theorem_p2_dup2"),
}

# human-reviewed candidate fills (approved 2026-11-12), applied alongside the
# formula repair; the six empty-si_units entries are deliberately absent here
SI_FILLS = {
    "math_geom_heron_p1": {"s": "m"},
    "math_geom_heron_p1_dup2": {"s": "m"},
    "math_geom_heron_p2": {"s": "m", "sqrt": "1"},
    "math_geom_heron_p2_dup2": {"s": "m", "sqrt": "1"},
    "math_trig_law_of_sines_p1": {"sin": "1"},
    "math_trig_law_of_sines_p1_dup2": {"sin": "1"},
    "math_trig_law_of_sines_p2": {"sin": "1"},
    "math_trig_law_of_sines_p2_dup2": {"sin": "1"},
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


def inline_map(mapping, empty_value=None):
    """Serialize a map exactly in the pool line style (sorted keys, default
    separators, raw non-ASCII) for surgical substring replacement."""
    return json.dumps({k: (empty_value[k] if empty_value else mapping[k])
                       for k in sorted(mapping)}, ensure_ascii=False)


def replace_once(line, old, new, tid, what):
    assert line.count(old) == 1, (
        "expected exactly one %s match in %s" % (what, tid))
    return line.replace(old, new)


def main():
    rules_path = os.path.join(POOL, "formula-and-artifact-fix-rules.json")
    log_path = os.path.join(POOL, "formula-and-artifact-fix-log.jsonl")
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
    changed = 0

    for parent in sorted(PAIRS):
        p1_id, p2_id = PAIRS[parent]
        i1, r1 = rows[p1_id]
        i2, r2 = rows[p2_id]

        f1, f2 = r1["formula"], r2["formula"]
        assert f1.count(" and ") == 1 and f2.count(" and ") == 1, (
            "expected one and-join per product: %s" % parent)
        eq1 = f1.split(" and ")[0].strip()
        eq2 = f2.split(" and ")[1].strip()
        frag1 = f1.split(" and ")[1].strip()
        frag2 = f2.split(" and ")[0].strip()
        # cross-checks: fragments must be the equation halves
        assert frag1 == eq2.split("=")[0].strip(), (
            "p1 right fragment != equation_2 lhs for %s" % parent)
        assert frag2 == eq1.split("=")[1].strip(), (
            "p2 left fragment != equation_1 rhs for %s" % parent)
        assert eq1.count("=") == 1 and eq2.count("=") == 1, (
            "restored equations must have exactly one separator: %s" % parent)

        targets = ((p1_id, i1, r1, f1, eq1), (p2_id, i2, r2, f2, eq2))
        for tid, idx, row, before_f, after_f in targets:
            line = lines[idx]
            before_sym = row["symbols"]
            assert "and" in before_sym, "and pseudo-symbol missing in %s" % tid
            after_sym = {k: v for k, v in before_sym.items() if k != "and"}

            line = replace_once(
                line, '"formula": ' + json.dumps(before_f, ensure_ascii=False),
                '"formula": ' + json.dumps(after_f, ensure_ascii=False),
                tid, "formula")
            line = replace_once(
                line, '"symbols": ' + inline_map(before_sym),
                '"symbols": ' + inline_map(after_sym),
                tid, "symbols")

            before_si = dict(row.get("si_units") or {})
            fills = SI_FILLS.get(tid)
            entry_log = {
                "field": "formula",
                "file": "normalized-law-pool.jsonl",
                "formula_after": after_f,
                "formula_before": before_f,
                "id": tid,
                "parent": parent,
                "piece": 1 if tid == PAIRS[parent][0] else 2,
            }
            if fills:
                for k in fills:
                    assert k not in before_si, "fill key already present: %s/%s" % (tid, k)
                after_si = dict(before_si)
                after_si.update(fills)
                assert set(after_si) == set(after_sym), (
                    "reviewed fills must complete the keyset: %s" % tid)
                line = replace_once(
                    line, '"si_units": ' + inline_map(before_si),
                    '"si_units": ' + inline_map(after_si),
                    tid, "si_units")
                entry_log["si_units_after"] = {k: after_si[k] for k in sorted(after_si)}
                entry_log["si_units_before"] = {k: before_si[k] for k in sorted(before_si)}
                entry_log["si_units_reviewed_fill"] = {k: fills[k] for k in sorted(fills)}
            else:
                assert not before_si, (
                    "non-reviewed entry unexpectedly has si_units: %s" % tid)
                entry_log["si_units_after"] = {}
                entry_log["si_units_before"] = {}
                entry_log["si_units_note"] = (
                    "left empty for the empty-si_units precheck track")
            entry_log["symbols_removed"] = ["and"]
            lines[idx] = line
            changed += 1
            log_rows.append(entry_log)

    assert changed == 14, "expected 14 entries changed, got %d" % changed

    text = "\n".join(lines) + ("\n" if had_final_nl else "")
    with io.open(POOL_FILE, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)

    rules = {
        "applied": "2026-11-12",
        "cross_checks": {
            "equation_1": "p1 formula before the and-join",
            "equation_2": "p2 formula after the and-join",
            "check_1": "p1 and-right fragment == equation_2 left-hand side",
            "check_2": "p2 and-left fragment == equation_1 right-hand side",
        },
        "entries_fixed": 14,
        "parent_pairs": {k: list(v) for k, v in sorted(PAIRS.items())},
        "rule": "restore each chain-split product's formula to the exact single "
                "equation it represents (root cause: chain splitter statement-marker "
                "table lacks the word connective; generator fix registered "
                "separately, not part of this pass); remove the and pseudo-symbol "
                "from symbols; apply the human-reviewed candidate fills "
                "(s=m, sin=1, sqrt=1, approved 2026-11-12) to the eight "
                "geometry/trigonometry entries; empty-si_units entries stay "
                "untouched on the value face",
        "si_units_fills": {k: dict(sorted(v.items())) for k, v in sorted(SI_FILLS.items())},
    }
    dump_json(rules_path, rules)
    dump_jsonl(log_path, log_rows)

    # structural report: gap counters + repair section
    report_path = os.path.join(POOL, "structural-report.json")
    with io.open(report_path, encoding="utf-8") as fh:
        report = json.load(fh)
    report["known_gaps"]["si_units_partial"] = 0
    report["formula_and_artifact_fix"] = {
        "applied": "2026-11-12",
        "artifacts": ["formula-and-artifact-fix-log.jsonl",
                      "formula-and-artifact-fix-rules.json"],
        "entries_fixed": 14,
        "parent_pairs": 7,
        "root_cause": "chain-splitter statement-marker table lacks the word "
                      "connective; generator fix registered separately",
        "si_units_fills_applied": 8,
    }
    dump_json(report_path, report)

    # manifest re-anchor (file list kept alphabetical)
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

    print("fixed %d entries across %d parent pairs; manifest re-anchored (%d files)"
          % (changed, len(PAIRS), len(names)))


if __name__ == "__main__":
    sys.exit(main())
