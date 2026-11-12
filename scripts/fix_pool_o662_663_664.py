#!/usr/bin/env python3
"""Deterministic pool fixes for O-662 / O-663 / O-664 (approved 2026-11-13).

- O-662  phys_em_maxwell_ampere: si d_phi_E_dt "Wb/s" -> "V*m/s"
  (electric-flux time derivative, not magnetic flux; eps0 * dPhiE/dt = A)
- O-664  phy_rel_app_02_p2: si v "m" -> "m*s^-1" (velocity dimension; makes
  v^2/c^2 dimensionless inside the Lorentz factor)
- O-663  embedded-constant rows rewritten to symbol-reference form:
  phys_quantum_energy_level / phy_atm_law_01 formulas reference the new
  reviewed key E_rydberg (si = eV); the value semantics (-13.6 eV) move to
  the derivation annotation.

Pure deterministic transformation: surgical value-substring edits inside the
four target lines only; all other lines are preserved byte-for-byte. Sidecars
a per-entry log + the frozen rules table, refresh the structural report, and
re-anchor the pool manifest.
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

RYDBERG_NOTE = "Rydberg energy E_rydberg = -13.6 eV (ground-state level)"

# id -> (si_before, si_after | None, formula_before, formula_after | None,
#        symbols_after | None, derivation_after | None)
FIXES = {
    "phys_em_maxwell_ampere": (
        {"I": "A", "d_phi_E_dt": "Wb/s", "epsilon_0": "F/m", "mu_0": "H/m",
         "phi_B": "T*m"},
        {"I": "A", "d_phi_E_dt": "V*m/s", "epsilon_0": "F/m", "mu_0": "H/m",
         "phi_B": "T*m"},
        None, None, None, None,
    ),
    "phy_rel_app_02_p2": (
        {"c": "m*s^-1", "dt": "s", "gamma": "1", "sqrt": "1", "v": "m"},
        {"c": "m*s^-1", "dt": "s", "gamma": "1", "sqrt": "1", "v": "m*s^-1"},
        None, None, None, None,
    ),
    "phys_quantum_energy_level": (
        {"E": "eV", "n": "1"},
        {"E": "eV", "n": "1", "E_rydberg": "eV"},
        "E = -13.6 / n ^ 2",
        "E = E_rydberg / n ^ 2",
        ["E", "n", "E_rydberg"],
        [RYDBERG_NOTE],
    ),
    "phy_atm_law_01": (
        {"E_n": "eV", "n2": "1"},
        {"E_n": "eV", "n2": "1", "E_rydberg": "eV"},
        "E_n = -13.6 / n2",
        "E_n = E_rydberg / n2",
        ["E_n", "n2", "E_rydberg"],
        ["Bohr, 1913", RYDBERG_NOTE],
    ),
}

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
    "o662-663-664-pool-fixes-log.jsonl",
    "o662-663-664-pool-fixes-rules.json",
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
    rules_path = os.path.join(POOL, "o662-663-664-pool-fixes-rules.json")
    log_path = os.path.join(POOL, "o662-663-664-pool-fixes-log.jsonl")
    if os.path.exists(rules_path) or os.path.exists(log_path):
        sys.exit("sidecars already exist; rerun refused (idempotency guard)")

    with io.open(POOL_FILE, encoding="utf-8", newline="") as f:
        lines = f.read().split("\n")
    had_final_nl = lines and lines[-1] == ""
    if had_final_nl:
        lines = lines[:-1]

    log_rows = []
    for tid in sorted(FIXES):
        si_before, si_after, f_before, f_after, sym_after, der_after = FIXES[tid]
        idx = next(i for i, l in enumerate(lines)
                   if l.strip() and json.loads(l)["id"] == tid)
        row = json.loads(lines[idx])
        line = lines[idx]

        assert row.get("si_units") == si_before, (
            "si drifted from frozen expectation: %s" % tid)
        if f_before is not None:
            assert row["formula"] == f_before, (
                "formula drifted: %s" % tid)
            line = replace_once(line, '"formula": ' + json.dumps(f_before, ensure_ascii=False),
                                '"formula": ' + json.dumps(f_after, ensure_ascii=False),
                                tid, "formula")
        line = replace_once(line, '"si_units": ' + inline_map(si_before),
                            '"si_units": ' + inline_map(si_after), tid, "si_units")
        if sym_after is not None:
            line = replace_once(line, '"symbols": ' + inline_map({k: "" for k in row["symbols"]}),
                                '"symbols": ' + inline_map({k: "" for k in sym_after}),
                                tid, "symbols")
        if der_after is not None:
            if "derivation" in row:
                line = replace_once(
                    line,
                    '"derivation": ' + json.dumps(row["derivation"], ensure_ascii=False),
                    '"derivation": ' + json.dumps(der_after, ensure_ascii=False),
                    tid, "derivation")
            else:
                # derivation key absent (serde default empty): insert after si_units
                line = replace_once(
                    line,
                    '"si_units": ' + inline_map(si_after),
                    '"si_units": ' + inline_map(si_after)
                    + ', "derivation": ' + json.dumps(der_after, ensure_ascii=False),
                    tid, "derivation-insert")

        entry = {"file": "normalized-law-pool.jsonl", "id": tid,
                 "field": "si_units", "si_after": si_after, "si_before": si_before}
        if f_after is not None:
            entry["field"] = "si_units+formula+symbols+derivation"
            entry["formula_after"] = f_after
            entry["formula_before"] = f_before
            entry["derivation_after"] = der_after
        lines[idx] = line
        log_rows.append(entry)

    assert len(log_rows) == 4, "expected 4 entries changed, got %d" % len(log_rows)

    text = "\n".join(lines) + ("\n" if had_final_nl else "")
    with io.open(POOL_FILE, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)

    rules = {
        "applied": "2026-11-13",
        "approval": "user directive 2026-11-13 to complete O-662/O-663/O-664 "
                    "per the registered remediation paths",
        "entries_fixed": 4,
        "fixes": {
            "O-662": "phys_em_maxwell_ampere si d_phi_E_dt Wb/s -> V*m/s "
                     "(electric-flux derivative; eps0 * dPhiE/dt = A)",
            "O-663": "embedded-constant rows reference the new reviewed key "
                     "E_rydberg (si = eV); value semantics (-13.6 eV) moved to "
                     "the derivation annotation",
            "O-664": "phy_rel_app_02_p2 si v m -> m*s^-1 (velocity dimension)",
        },
    }
    dump_json(rules_path, rules)
    dump_jsonl(log_path, log_rows)

    report_path = os.path.join(POOL, "structural-report.json")
    with io.open(report_path, encoding="utf-8") as fh:
        report = json.load(fh)
    report["o662_663_664_pool_fixes"] = {
        "applied": "2026-11-13",
        "artifacts": ["o662-663-664-pool-fixes-log.jsonl",
                      "o662-663-664-pool-fixes-rules.json"],
        "entries_fixed": 4,
        "source": "approved 2026-11-13 (O-662/663/664 remediation)",
    }
    dump_json(report_path, report)

    out_lines = []
    for name in MANIFEST_NAMES:
        out_lines.append("%s  %s" % (sha256_file(os.path.join(POOL, name)), name))
    combined = hashlib.sha256("\n".join(out_lines).encode("utf-8")).hexdigest()
    out_lines.append("%s  COMBINED" % combined)
    with io.open(os.path.join(POOL, "manifest.sha256"), "w", encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(out_lines) + "\n")

    print("fixed %d entries; manifest re-anchored (%d files)"
          % (len(FIXES), len(MANIFEST_NAMES)))


if __name__ == "__main__":
    sys.exit(main())
