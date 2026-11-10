#!/usr/bin/env python3
"""Deterministic format bridge for migrated physics law slices.

Reads the raw exported slice rows (data/migration/law-slices/input-physics-slices.jsonl)
and emits law_set v1.0 deployment artifacts in the golden format, plus a
per-law provenance manifest recording every normalization applied.

Pure deterministic transformation:
  - no network, no clock, no randomness; byte-stable output for identical input
  - raw source rows are exported verbatim data (read-only), never referenced at
    runtime by any service; only the bridge outputs below enter the repository

Normalization rules (fixed, applied in order):
  1. law id          : source rule_id upper-dash form -> lowercase underscore
  2. formula         : '==' -> '=' (exactly one '='), '**' -> '^'
  3. symbols         : object values {'type': 'input'} -> 'input quantity'
                       (string values pass through unchanged)
  4. si_units        : {} placeholder (dimensional verification pending; the
                       validator reports these entries in the explicit
                       'unverified' third state, never as silent passes)
  5. domains         : ['physical'] (must be a registered domain-catalog key)
  6. solvable        : [source output symbol]
  7. derivation      : [source statement text]
  8. tier            : 'full'
  9. max_version_depth: 1 (source carries no version chain)
 10. monotone_form   : true (plain equations, no negation constructs)
 11. metadata.source : source provenance string

Slices are bucketed into two sets (empirical / approximation) with version
0.1.0 to mark the pre-golden placeholder state.
"""

import io
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
INPUT = os.path.join(ROOT, "data", "migration", "law-slices", "input-physics-slices.jsonl")
OUT_DIR = os.path.join(ROOT, "data", "law_sets")
PROV_DIR = os.path.join(ROOT, "data", "migration", "law-slices")

LAW_SET_SCHEMA = "https://evorule.org/schemas/law_set/v1.0.json"
REGISTERED_DOMAINS = {"physical", "engineering", "economic"}

ID_SET_RE = re.compile(r"^[a-z][a-z0-9-]*$")
ID_LAW_RE = re.compile(r"^[a-z][a-z0-9_]*$")
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+$")
BANNED_OPS = ("==", "!=", ">=", "<=", "=>", "**", "->", "?", "%", "!")


def normalize_law_id(raw_id):
    law_id = raw_id.strip().lower().replace("-", "_")
    assert ID_LAW_RE.match(law_id), "law id does not match ^[a-z][a-z0-9_]*$: %r" % law_id
    return law_id, ["law id: %s -> %s (lowercase underscore)" % (raw_id, law_id)]


def normalize_formula(raw_formula, symbol_keys, law_id):
    ops = []
    formula = raw_formula.strip()
    assert formula.count("==") <= 1, "multiple '==' in formula: %s" % law_id
    if "==" in formula:
        formula = formula.replace("==", "=")
        ops.append("formula: '==' -> '='")
    if "**" in formula:
        formula = formula.replace("**", "^")
        ops.append("formula: '**' -> '^'")
    assert formula.count("=") == 1, "formula must contain exactly one '=': %s" % law_id
    for op in BANNED_OPS:
        assert op not in formula, "banned operator %r in formula: %s" % (op, law_id)
    refs = set(re.findall(r"[A-Za-z_][A-Za-z0-9_]*", formula)) - {"mod", "abs", "sqrt"}
    missing = refs - set(symbol_keys)
    assert not missing, "formula symbols missing from symbols object: %s -> %s" % (law_id, sorted(missing))
    return formula, ops


def normalize_symbols(raw_symbols, law_id):
    symbols = {}
    ops = []
    for key, value in raw_symbols.items():
        k = key.strip()
        if isinstance(value, dict):
            assert value.get("type") == "input", "unsupported symbol shape for %s.%s: %r" % (law_id, k, value)
            symbols[k] = "input quantity"
            ops.append("symbols.%s: {'type': 'input'} -> 'input quantity'" % k)
        else:
            assert isinstance(value, str) and value.strip(), "symbol %s.%s must be a non-empty string" % (law_id, k)
            symbols[k] = value.strip()
    assert len(symbols) >= 2, "symbols must have at least 2 entries: %s" % law_id
    return symbols, ops


def build_law(row):
    law_id, ops = normalize_law_id(row["rule_id"])
    symbols, ops2 = normalize_symbols(row["symbols"], law_id)
    formula, ops3 = normalize_formula(row["evaluation"]["formula"], symbols.keys(), law_id)
    ops = ops + ops2 + ops3

    output = row.get("output")
    solvable = [output.strip()] if isinstance(output, str) and output.strip() else []
    if solvable:
        assert solvable[0] in symbols, "output symbol not in symbols: %s" % law_id
        ops.append("solvable: [%s] (derived from source output)" % solvable[0])

    derivation = []
    statement = (row.get("name") or {}).get("zh")
    if isinstance(statement, str) and statement.strip():
        derivation.append(statement.strip())
        ops.append("derivation: source statement text")

    source = (row.get("provenance") or {}).get("source")
    assert isinstance(source, str) and source.strip(), "missing provenance.source: %s" % law_id

    law = {
        "id": law_id,
        "formula": formula,
        "symbols": symbols,
        "si_units": {},
        "domain": "physical",
        "domains": ["physical"],
        "solvable": solvable,
        "derivation": derivation,
        "tier": "full",
        "max_version_depth": 1,
        "monotone_form": True,
        "metadata": {"source": source.strip()},
    }
    ops += [
        "si_units: {} placeholder (dimensional verification pending -> unverified third state)",
        "domain/domains: 'physical' (registered domain-catalog key)",
        "tier: 'full'; max_version_depth: 1; monotone_form: true",
        "metadata.source: %s" % source.strip(),
    ]
    return law, ops


BUCKETS = {
    "physical-empirical-slices": [
        "PHY-CLA-LAW-02",
        "PHY-ELM-LAW-01",
        "PHY-ELM-LAW-02",
    ],
    "physical-approximation-slices": [
        "PHY-REL-APP-01",
        "PHY-OPT-APP-03",
    ],
}

BUCKET_TITLES = {
    "physical-empirical-slices": "Empirical physics law slices (migrated)",
    "physical-approximation-slices": "Approximation physics law slices (migrated)",
}


def load_rows():
    rows = {}
    with io.open(INPUT, encoding="utf-8") as f:
        for line in f:
            s = line.strip()
            if not s:
                continue
            row = json.loads(s)
            rid = row["rule_id"]
            assert rid not in rows, "duplicate rule_id in input: %s" % rid
            rows[rid] = row
    return rows


def main():
    rows = load_rows()
    all_ids = set()
    for ids in BUCKETS.values():
        all_ids.update(ids)
    missing = all_ids - set(rows)
    extra = set(rows) - all_ids
    assert not missing, "input rows missing: %s" % sorted(missing)
    assert not extra, "unexpected input rows: %s" % sorted(extra)

    provenance = {"sets": {}, "laws": {}}
    for set_id, ids in BUCKETS.items():
        assert ID_SET_RE.match(set_id), "bad set id: %s" % set_id
        laws = []
        for rid in ids:
            law, ops = build_law(rows[rid])
            laws.append(law)
            provenance["laws"][law["id"]] = {
                "source_rule_id": rid,
                "transformations": ops,
            }
        laws.sort(key=lambda x: x["id"])
        sources = sorted({l["metadata"]["source"] for l in laws})
        doc = {
            "$schema": LAW_SET_SCHEMA,
            "kind": "law_set",
            "id": set_id,
            "version": "0.1.0",
            "metadata": {
                "title": BUCKET_TITLES[set_id],
                "description": (
                    "Law slices migrated from the archived science asset dump; "
                    "si_units are placeholders, so every entry sits in the "
                    "explicit unverified (dimensional third) state until the "
                    "dimension pass lands."
                ),
                "domain": "physical",
                "source": "; ".join(sources),
                "unit_system": "SI",
                "verified": "pending: si_units placeholder (dimensional verification not yet applied)",
            },
            "laws": laws,
        }
        assert VERSION_RE.match(doc["version"])
        assert len(laws) <= 128
        assert all(l["domains"][0] in REGISTERED_DOMAINS for l in laws)
        provenance["sets"][set_id] = {"version": doc["version"], "law_ids": [l["id"] for l in laws]}

        out_path = os.path.join(OUT_DIR, "%s-%s.json" % (set_id, doc["version"]))
        text = json.dumps(doc, ensure_ascii=False, indent=2) + "\n"
        with io.open(out_path, "w", encoding="utf-8", newline="\n") as f:
            f.write(text)
        print("wrote %s (%d laws)" % (out_path, len(laws)))

    prov_path = os.path.join(PROV_DIR, "provenance-physics-slices.json")
    with io.open(prov_path, "w", encoding="utf-8", newline="\n") as f:
        f.write(json.dumps(provenance, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    print("wrote %s" % prov_path)


if __name__ == "__main__":
    sys.exit(main())
