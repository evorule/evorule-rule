#!/usr/bin/env python3
"""Deterministic si_units backfill for the four symbol-face-cleaned rows.

Approved 2026-11-12 (precheck rerun on the four symbol-face-cleaned rows,
all four rows approved; frozen single source mirrored below):

    phy_atm_law_01   E_n=eV, n2=1
    chem_pcd_thm_03  E_anode=E_cathode=E_cell=V
    chem_bio_rul_01  DeltaG=J*mol^-1
    chem_sto_emp_01  P=kg*m^-1*s^-2, T=K, V=m^3, n=mol, constant=m^3*mol^-1

Engine expectation (established boundaries, recorded as skipped): the four
rows keep formula-face defects or hit engine grammar boundaries (unary minus
/ binary additive operators / untouched formula defects), so check_law =
Skipped with zero Mismatch is the verified behavior (precedent probes run
during the symbol-cleanup precheck).

Sidecars a per-entry log + the frozen rules table, refreshes the structural
report (si_units_empty 155 -> 151), and re-anchors the pool manifest.

Pure deterministic transformation: no network, no clock, no randomness;
byte-stable output for identical input. Line edits are surgical: only the
si_units value substrings change inside affected lines; all other lines are
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

# id -> {symbol: unit}; frozen single source (approved 2026-11-12)
FILLS = {
    "phy_atm_law_01": {"E_n": "eV", "n2": "1"},
    "chem_pcd_thm_03": {"E_anode": "V", "E_cathode": "V", "E_cell": "V"},
    "chem_bio_rul_01": {"DeltaG": "J*mol^-1"},
    "chem_sto_emp_01": {"P": "kg*m^-1*s^-2", "T": "K", "V": "m^3",
                        "n": "mol", "constant": "m^3*mol^-1"},
}

LOG_NOTES = {
    "phy_atm_law_01": (
        "embedded-constant precedent phys_quantum_energy_level (si E = eV); "
        "n2 is the notation-collapsed n^2 index, dimensionless"),
    "chem_pcd_thm_03": (
        "cell/cathode/anode potentials; subscript-collapse reconstruction "
        "completed in the symbol-cleanup pass"),
    "chem_bio_rul_01": (
        "Gibbs energy per mole in the notation-normalized form (kJ/mol -> "
        "J*mol^-1); the kJ/mol formula literal stays with the "
        "kJ/mol formula literal stays with the formula-repair batch"),
    "chem_sto_emp_01": (
        "molar-volume law; V prior mis-hit (volt) overridden by row context; "
        "n anchored to chem_bond_moles_def (si n = mol); constant follows "
        "the left-hand side V/n"),
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
    """Serialize a map exactly in the pool line style (sorted keys, default
    separators, raw non-ASCII) for surgical substring replacement."""
    return json.dumps({k: mapping[k] for k in sorted(mapping)}, ensure_ascii=False)


def replace_once(line, old, new, tid, what):
    assert line.count(old) == 1, (
        "expected exactly one %s match in %s" % (what, tid))
    return line.replace(old, new)


def main():
    rules_path = os.path.join(POOL, "si-units-symbol-cleanup-backfill-rules.json")
    log_path = os.path.join(POOL, "si-units-symbol-cleanup-backfill-log.jsonl")
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
    for tid in sorted(FILLS):
        fills = FILLS[tid]
        idx, row = rows[tid]
        line = lines[idx]

        before_si = dict(row.get("si_units") or {})
        assert before_si == {}, "target entry unexpectedly has si_units: %s" % tid
        assert set(row["symbols"]) == set(fills), (
            "candidate keyset must complete the symbol face: %s" % tid)

        line = replace_once(line, '"si_units": ' + inline_map(before_si),
                            '"si_units": ' + inline_map(fills), tid, "si_units")

        log_rows.append({
            "field": "si_units",
            "file": "normalized-law-pool.jsonl",
            "id": tid,
            "note": LOG_NOTES[tid],
            "si_units_after": {k: fills[k] for k in sorted(fills)},
            "si_units_before": {},
            "source": "approved 2026-11-12 precheck rerun (frozen single source)",
        })
        lines[idx] = line

    assert len(log_rows) == 4, "expected 4 entries changed, got %d" % len(log_rows)

    text = "\n".join(lines) + ("\n" if had_final_nl else "")
    with io.open(POOL_FILE, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)

    rules = {
        "applied": "2026-11-12",
        "entries_fixed": 4,
        "engine_boundary": (
            "all four rows keep check_law = Skipped (unary-minus grammar "
            "boundary / binary additive-operator grammar boundary / "
            "untouched formula-face defects deferred to the formula-repair "
            "batch); zero mismatch, verified by the precedent probes run "
            "during the symbol-cleanup precheck"),
        "fills": {k: dict(sorted(v.items())) for k, v in sorted(FILLS.items())},
        "rule": "apply the approved candidate values (frozen single source) "
                "as surgical si_units fills; candidate keyset must complete "
                "the cleaned symbol face of each row",
    }
    dump_json(rules_path, rules)
    dump_jsonl(log_path, log_rows)

    # structural report: gap counter + backfill section
    report_path = os.path.join(POOL, "structural-report.json")
    with io.open(report_path, encoding="utf-8") as fh:
        report = json.load(fh)
    assert report["known_gaps"]["si_units_empty"] == 155, (
        "unexpected si_units_empty counter")
    report["known_gaps"]["si_units_empty"] = 151
    report["si_units_backfill_symbol_cleanup"] = {
        "applied": "2026-11-12",
        "artifacts": ["si-units-symbol-cleanup-backfill-log.jsonl",
                      "si-units-symbol-cleanup-backfill-rules.json"],
        "entries_fixed": 4,
        "source": "approved 2026-11-12 precheck rerun (frozen single source)",
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

    print("backfilled %d entries; si_units_empty 155->151; manifest re-anchored (%d files)"
          % (len(FILLS), len(MANIFEST_NAMES)))


if __name__ == "__main__":
    sys.exit(main())
