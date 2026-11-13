#!/usr/bin/env python3
"""Deterministic pool fixes: split four collapsed-notation rows into
symbol-reference form, with reviewed values (approved 2026-11-13).

- chem_sol_thm_02  "pi = iCRT"  -> "pi = i * C * R * T"
  Reviewed correction: pi is the osmotic pressure (van't Hoff, 1887;
  metadata.source anchor), not the circle constant. pi 1 -> kg*m^-1*s^-2.
- phy_ast_con_01   "E = mc2"   -> "E = m * c ^ 2"
- phy_opt_law_03   "I = I0cos2theta" -> "I = I0 * cos(2 * theta)"
  (kept reviewed value I = A; theta dimensionless, M1b cos-whitelist)
- phy_rel_app_04   "p = gammam0v" -> "p = gamma * m0 * v"

Pure deterministic transformation: surgical value-substring edits inside the
four target lines only; all other lines are preserved byte-for-byte. Sidecars
(a per-entry log + the frozen rules table), refresh the structural report,
and re-anchor the pool manifest.
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

# id -> (formula_before, formula_after, si_before, si_after, symbols_after)
FIXES = {
    "chem_sol_thm_02": (
        "pi = iCRT",
        "pi = i * C * R * T",
        {"iCRT": "1", "pi": "1"},
        {"C": "mol*m^-3", "i": "1", "pi": "kg*m^-1*s^-2",
         "R": "kg*m^2*s^-2*K^-1*mol^-1", "T": "K"},
        ["C", "i", "pi", "R", "T"],
    ),
    "phy_ast_con_01": (
        "E = mc2",
        "E = m * c ^ 2",
        {"E": "kg*m^2*s^-2", "mc2": "kg*m^2*s^-2"},
        {"c": "m*s^-1", "E": "kg*m^2*s^-2", "m": "kg"},
        ["c", "E", "m"],
    ),
    "phy_opt_law_03": (
        "I = I0cos2theta",
        "I = I0 * cos(2 * theta)",
        {"I": "A", "I0cos2theta": "A"},
        {"I": "A", "I0": "A", "theta": "1"},
        ["I", "I0", "theta"],
    ),
    "phy_rel_app_04": (
        "p = gammam0v",
        "p = gamma * m0 * v",
        {"gammam0v": "kg*m*s^-1", "p": "kg*m*s^-1"},
        {"gamma": "1", "m0": "kg", "p": "kg*m*s^-1", "v": "m*s^-1"},
        ["gamma", "m0", "p", "v"],
    ),
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


def inline_map(mapping):
    return json.dumps({k: mapping[k] for k in sorted(mapping)}, ensure_ascii=False)


def replace_once(line, old, new, tid, what):
    assert line.count(old) == 1, (
        "expected exactly one %s match in %s" % (what, tid))
    return line.replace(old, new)


def main():
    rules_path = os.path.join(POOL, "collapsed-split-review-rules.json")
    log_path = os.path.join(POOL, "collapsed-split-review-log.jsonl")
    if os.path.exists(rules_path) or os.path.exists(log_path):
        sys.exit("sidecars already exist; rerun refused (idempotency guard)")

    with io.open(POOL_FILE, encoding="utf-8", newline="") as f:
        lines = f.read().split("\n")
    had_final_nl = lines and lines[-1] == ""
    if had_final_nl:
        lines = lines[:-1]

    log_rows = []
    for tid in sorted(FIXES):
        f_before, f_after, si_before, si_after, sym_after = FIXES[tid]
        idx = next(i for i, l in enumerate(lines)
                   if l.strip() and json.loads(l)["id"] == tid)
        row = json.loads(lines[idx])
        line = lines[idx]

        assert row["formula"] == f_before, "formula drifted: %s" % tid
        assert row.get("si_units") == si_before, (
            "si drifted from frozen expectation: %s" % tid)

        line = replace_once(line, '"formula": ' + json.dumps(f_before, ensure_ascii=False),
                            '"formula": ' + json.dumps(f_after, ensure_ascii=False),
                            tid, "formula")
        line = replace_once(line, '"si_units": ' + inline_map(si_before),
                            '"si_units": ' + inline_map(si_after), tid, "si_units")
        line = replace_once(line, '"symbols": ' + inline_map({k: "" for k in row["symbols"]}),
                            '"symbols": ' + inline_map({k: "" for k in sym_after}),
                            tid, "symbols")

        lines[idx] = line
        log_rows.append({
            "file": "normalized-law-pool.jsonl", "id": tid,
            "field": "formula+si_units+symbols",
            "formula_after": f_after, "formula_before": f_before,
            "si_after": si_after, "si_before": si_before,
        })

    assert len(log_rows) == 4, "expected 4 entries changed, got %d" % len(log_rows)

    text = "\n".join(lines) + ("\n" if had_final_nl else "")
    with io.open(POOL_FILE, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)

    rules = {
        "applied": "2026-11-13",
        "approval": "user directive 2026-11-13 (review request 75: corrections "
                    "A and B approved; shapes confirmed)",
        "entries_fixed": 4,
        "fixes": {
            "chem_sol_thm_02": "pi = i * C * R * T; reviewed correction "
                               "pi 1 -> kg*m^-1*s^-2 (van't Hoff, 1887: osmotic "
                               "pressure, not the circle constant)",
            "phy_ast_con_01": "E = m * c ^ 2 (subsymbol values derived "
                              "algebraically from the reviewed E)",
            "phy_opt_law_03": "I = I0 * cos(2 * theta); reviewed value I = A "
                              "kept (dimensionless angle, cos whitelist)",
            "phy_rel_app_04": "p = gamma * m0 * v (momentum dimension, "
                              "algebraically consistent)",
        },
    }
    dump_json(rules_path, rules)
    dump_jsonl(log_path, log_rows)

    report_path = os.path.join(POOL, "structural-report.json")
    with io.open(report_path, encoding="utf-8") as fh:
        report = json.load(fh)
    report["collapsed_split_review"] = {
        "applied": "2026-11-13",
        "artifacts": ["collapsed-split-review-log.jsonl",
                      "collapsed-split-review-rules.json"],
        "entries_fixed": 4,
        "source": "approved 2026-11-13 (review request 75)",
    }
    dump_json(report_path, report)

    names = sorted(n for n in os.listdir(POOL)
                   if n != "manifest.sha256" and os.path.isfile(os.path.join(POOL, n)))
    out_lines = ["%s  %s" % (sha256_file(os.path.join(POOL, n)), n) for n in names]
    combined = hashlib.sha256("\n".join(out_lines).encode("utf-8")).hexdigest()
    out_lines.append("%s  COMBINED" % combined)
    with io.open(os.path.join(POOL, "manifest.sha256"), "w",
                 encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(out_lines) + "\n")

    print("fixed %d entries; manifest re-anchored (%d files)"
          % (len(FIXES), len(names)))


if __name__ == "__main__":
    sys.exit(main())
