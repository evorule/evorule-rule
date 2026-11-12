#!/usr/bin/env python3
"""Deterministic si_units backfill for the five reviewed migration pool entries.

Applies the human-reviewed SI unit table (single source: REVIEWED_SI_UNITS in
bridge_law_slices.py; every value unit-engine prechecked `consistent` before
human approval on 2026-11-12) to the migration pool, writes the declared
rules table + per-entry transformation log sidecars (same pattern as the
metadata derivation pass), refreshes the structural report counters, and
re-anchors the pool manifest.

Pure deterministic transformation: no network, no clock, no randomness;
byte-stable output for identical input. Line edits are surgical: only the
si_units value substring changes inside the five target lines; all other
lines are preserved byte-for-byte.
"""

import hashlib
import io
import json
import os
import sys

sys.dont_write_bytecode = True  # keep the tree clean when importing the reviewed-units single source
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from bridge_law_slices import REVIEWED_SI_UNITS  # single source of reviewed values

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

ROOT = os.path.dirname(HERE)
POOL = os.path.join(ROOT, "data", "migration", "law-pool")

TARGETS = [
    "phy_cla_law_02",
    "phy_elm_law_01",
    "phy_elm_law_02",
    "phy_opt_app_03",
    "phy_rel_app_01",
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


def inline_units(units):
    """Serialize a reviewed table exactly in the pool line style (sorted keys,
    default separators) for surgical substring replacement."""
    return json.dumps({k: units[k] for k in sorted(units)}, ensure_ascii=False)


def main():
    pool_path = os.path.join(POOL, "normalized-law-pool.jsonl")
    with io.open(pool_path, encoding="utf-8", newline="") as f:
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

    by_id = {r["id"]: (idx, r) for idx, r in rows}
    for tid in TARGETS:
        assert tid in by_id, "target missing from pool: %s" % tid

    log_rows = []
    changed = 0
    for tid in TARGETS:
        idx, row = by_id[tid]
        line = lines[idx]
        before = row.get("si_units")
        assert before == {}, "target si_units not empty for %s: %r" % (tid, before)
        units = REVIEWED_SI_UNITS[tid]
        assert set(units) == set(row["symbols"]), (
            "reviewed keyset must cover symbols exactly: %s" % tid)
        marker = '"si_units": {}'
        assert line.count(marker) == 1, "expected exactly one empty si_units in %s" % tid
        new_line = line.replace(marker, '"si_units": ' + inline_units(units))
        assert new_line != line
        lines[idx] = new_line
        changed += 1
        log_rows.append({
            "after": {k: units[k] for k in sorted(units)},
            "before": {},
            "engine_verdict": "consistent",
            "field": "si_units",
            "file": "normalized-law-pool.jsonl",
            "id": tid,
            "reviewed": "2026-11-12",
            "rule": "human-reviewed units table (REVIEWED_SI_UNITS in bridge_law_slices.py)",
        })
    assert changed == len(TARGETS)

    text = "\n".join(lines) + ("\n" if had_final_nl else "")
    with io.open(pool_path, "w", encoding="utf-8", newline="") as fh:
        fh.write(text)

    # declared rules table (notation spec + frozen reviewed mapping)
    rules = {
        "notation": {
            "dimensionless": '"1" (engine has no "dimensionless" token)',
            "form": "coherent SI symbols or base-unit power products (e.g. m*s^-1)",
            "prefixes": "none (SI prefixes unsupported by the unit engine)",
            "reserved_spellings": {"ohm": "ohm is the only accepted ohm spelling"},
        },
        "reviewed": "2026-11-12",
        "reviewed_units": {k: {s: v for s, v in sorted(u.items())} for k, u in sorted(REVIEWED_SI_UNITS.items())},
        "rule": "human-reviewed units table; single source REVIEWED_SI_UNITS in bridge_law_slices.py; applied to pool and both slice deployment files (v0.2.0)",
        "unit_engine_precheck": "consistent 5/5 (dual-run SHA256-equal harness evidence)",
    }
    dump_json(os.path.join(POOL, "si-units-backfill-rules.json"), rules)
    dump_jsonl(os.path.join(POOL, "si-units-backfill-log.jsonl"), log_rows)

    # structural report: gap counter + backfill section
    report_path = os.path.join(POOL, "structural-report.json")
    with io.open(report_path, encoding="utf-8") as fh:
        report = json.load(fh)
    report["known_gaps"]["si_units_empty"] = 228
    report["si_units_backfill"] = {
        "artifacts": ["si-units-backfill-log.jsonl", "si-units-backfill-rules.json"],
        "entries_backfilled": 5,
        "notation": rules["notation"],
        "reviewed": "2026-11-12",
        "slices_version": "0.2.0",
        "unit_engine_precheck": "consistent 5/5 (dual-run SHA256-equal harness evidence)",
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

    print("backfilled %d pool entries; manifest re-anchored (%d files)" % (changed, len(names)))


if __name__ == "__main__":
    sys.exit(main())
