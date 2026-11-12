#!/usr/bin/env python3
"""Deterministic metadata backfill: function-argument dimensionless exemption.

Approved 2026-11-13 (strict-args ruling carries an explicit exemption list):
the two pH-definition rows declare metadata.fn_arg_dimless_exempt = true --
log(H_plus) takes H_plus = mol/L, which is not dimensionless; the physical
convention [pX] = 1 makes the exemption the reviewed intent. The engine
consumes the key via serde default (opt-in, zero hardcoded row ids).

Pure deterministic transformation: only the metadata value substrings of the
two target lines change; all other lines are preserved byte-for-byte.
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

TARGETS = ["chem_sol_ph_def", "chem_sol_ph_def_dup2"]

MANIFEST_NAMES = [
    "a2-derivation-log.jsonl",
    "a2-derivation-rules.json",
    "excluded-non-identity-forms.jsonl",
    "formula-and-artifact-fix-log.jsonl",
    "formula-and-artifact-fix-rules.json",
    "formula-notation-fix-log.jsonl",
    "formula-notation-fix-rules.json",
    "fn-arg-exempt-backfill-log.jsonl",
    "fn-arg-exempt-backfill-rules.json",
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


def main():
    rules_path = os.path.join(POOL, "fn-arg-exempt-backfill-rules.json")
    log_path = os.path.join(POOL, "fn-arg-exempt-backfill-log.jsonl")
    if os.path.exists(rules_path) or os.path.exists(log_path):
        sys.exit("sidecars already exist; rerun refused (idempotency guard)")

    with io.open(POOL_FILE, encoding="utf-8", newline="") as f:
        lines = f.read().split("\n")
    had_final_nl = lines and lines[-1] == ""
    if had_final_nl:
        lines = lines[:-1]

    log_rows = []
    for idx, line in enumerate(lines):
        if not line.strip():
            continue
        row = json.loads(line)
        if row["id"] not in TARGETS:
            continue
        before = row.get("metadata") or {}
        assert "fn_arg_dimless_exempt" not in before, (
            "exemption key already present: %s" % row["id"])
        after = {k: before[k] for k in sorted(before)}
        after["fn_arg_dimless_exempt"] = True
        new_line = line.replace(
            '"metadata": ' + json.dumps(before, ensure_ascii=False),
            '"metadata": ' + json.dumps(after, ensure_ascii=False))
        assert new_line != line, "metadata replacement failed: %s" % row["id"]
        lines[idx] = new_line
        log_rows.append({
            "field": "metadata.fn_arg_dimless_exempt",
            "file": "normalized-law-pool.jsonl",
            "id": row["id"],
            "metadata_after": after,
            "metadata_before": before,
            "note": "log(H_plus) with H_plus = mol/L is not dimensionless; "
                    "physical convention [pX] = 1; strict-args exemption "
                    "declared per the approved ruling",
        })

    assert len(log_rows) == 2, "expected 2 entries changed, got %d" % len(log_rows)

    text = "\n".join(lines) + ("\n" if had_final_nl else "")
    with io.open(POOL_FILE, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)

    rules = {
        "applied": "2026-11-13",
        "entries_fixed": 2,
        "engine_side": "evorule-law-schema metadata.fn_arg_dimless_exempt "
                       "(serde default false; strict-args hard Mismatch unless declared)",
        "fills": {t: {"fn_arg_dimless_exempt": True} for t in TARGETS},
        "rule": "declare the reviewed exemption explicitly on the law face; "
                "the strict dimensionless-argument check is overridden only "
                "where the key is present (default-invariant preserved)",
    }
    dump_json(rules_path, rules)
    dump_jsonl(log_path, log_rows)

    out_lines = []
    for name in MANIFEST_NAMES:
        out_lines.append("%s  %s" % (sha256_file(os.path.join(POOL, name)), name))
    combined = hashlib.sha256("\n".join(out_lines).encode("utf-8")).hexdigest()
    out_lines.append("%s  COMBINED" % combined)
    with io.open(os.path.join(POOL, "manifest.sha256"), "w", encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(out_lines) + "\n")

    print("backfilled %d entries; manifest re-anchored (%d files)"
          % (len(TARGETS), len(MANIFEST_NAMES)))


if __name__ == "__main__":
    sys.exit(main())
