#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Law-pool notation normalization pipeline (discipline-asset migration, stage 2).

Deterministic and replayable: consumes the committed raw export
``data/migration/law-pool/input-law-pool.jsonl`` and emits normalized law products
with per-product provenance, declared transform rules, chain-split / implicit-multiplication
mapping tables, a structural verification report and a SHA256 manifest.

Structural gate semantics (the stage-2 target: structural pass rate 100%):
  - no comparison-form operators (``==`` / ``!=`` / ``<=`` / ``>=``) remain;
  - exactly one equation separator, both sides non-empty, length >= 3;
  - every identifier appearing in the formula is declared in ``symbols``;
  - product id match ``^[a-z][a-z0-9_]*$``.

Fields intentionally left open for later stages (recorded as known gaps, not fabricated):
  - ``si_units`` truth values (dimensional engineering stage, human-reviewed);
  - ``domain`` / ``tier`` / ``solvable`` for A/B-batch products (mechanical assembly stage);
  - refined citations/derivation (citation-completion stage).

Standard library only. Exit codes: 0 = all gates green; 1 = gate red (fail-visible);
2 = input/count contract violation.
"""
import hashlib
import json
import os
import re
import sys

# ── declared deterministic transforms ────────────────────────────────────────

GREEK = {"α": "alpha", "β": "beta", "γ": "gamma", "δ": "delta", "ε": "epsilon",
         "ζ": "zeta", "η": "eta", "θ": "theta", "ι": "iota", "κ": "kappa",
         "λ": "lambda", "μ": "mu", "ν": "nu", "ξ": "xi", "π": "pi", "ρ": "rho",
         "σ": "sigma", "τ": "tau", "υ": "upsilon", "φ": "phi", "χ": "chi",
         "ψ": "psi", "ω": "omega", "Γ": "Gamma", "Δ": "Delta", "Θ": "Theta",
         "Λ": "Lambda", "Ξ": "Xi", "Π": "Pi", "Σ": "Sigma", "Φ": "Phi",
         "Ψ": "Psi", "Ω": "Omega"}
SUP = {"⁰": "^0", "¹": "^1", "²": "^2", "³": "^3", "⁴": "^4", "⁵": "^5",
       "⁶": "^6", "⁷": "^7", "⁸": "^8", "⁹": "^9", "ⁿ": "^n"}
BANNED_OPS = ["==", "!=", "<=", ">="]
BANNED_OP_RE = re.compile(r"[<>!]\s*=|\s==\s*|==")
IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
SCI_EXP_RE = re.compile(r"[A-Za-z][-+]?\d")  # scientific-notation exponent tail

RULES = [
    {"id": "R1", "name": "nfkc", "def": "unicodedata NFKC normalization of the formula string"},
    {"id": "R2", "name": "superscript_transcribe",
     "def": "superscript digits mapped to caret form via fixed table (11 entries)"},
    {"id": "R3", "name": "mult_sign_unify", "def": "interpunct/middot/times (· ⋅ ×) mapped to '*'"},
    {"id": "R4", "name": "minus_unify", "def": "unicode minus/en-dash/em-dash (− – —) mapped to '-'"},
    {"id": "R5", "name": "pow_star_unify", "def": "'**' mapped to '^'"},
    {"id": "R6", "name": "greek_transcribe",
     "def": "greek letters mapped to latin names via fixed table (33 entries); same table applied to si_units keys"},
    {"id": "R7", "name": "eq_separator_unify", "def": "'==' mapped to '=' (equality-identity semantics)"},
    {"id": "R8", "name": "ws_collapse", "def": "whitespace runs collapsed to single space, strip both ends"},
    {"id": "R9", "name": "chain_split",
     "def": "formula with >1 equation separator, no banned comparison operator and no statement "
            "markers (set-builder/quantifier/logic-connective characters) is split into consecutive "
            "pairs (a=b=c -> a=b, b=c); product ids append _p<k>; multi-'=' formulas carrying "
            "statement markers are routed out by R13 instead (non-chain form)"},
    {"id": "R10", "name": "implicit_mult_insert",
     "def": "insert '*' at lexical boundaries only: a maximal number token (digit run not preceded "
            "by an identifier character) followed by an identifier-start letter, or ')' followed by "
            "identifier/digit/'('; identifier tokens are never split (t2g/H2O/x2 intact); skipped "
            "when the letter run is a scientific-notation exponent (e.g. 1e5, 1E-10)"},
    {"id": "R11", "name": "symbols_rebuild",
     "def": "symbols object keys = ordered union of formula identifiers and valid source symbol names "
            "(^[A-Za-z_][A-Za-z0-9_]*$); values from source symbol descriptions when available else empty string; "
            "source names that are not bare identifiers are recorded as dropped, never invented"},
    {"id": "R12", "name": "uniform_declarations",
     "def": "max_version_depth=1, monotone_form=true (uniform finiteness/monotone declarations); "
            "metadata.source = source citation when the source provides one, else 'pending-citation:<id>' "
            "(flagged source_status=pending for the citation-completion stage)"},
    {"id": "R13", "name": "non_identity_routing",
     "def": "pool entries whose normalized formula still contains a banned comparison operator "
            "(!=, <=, >=) or has no equation separator are not equation-identity laws: they are "
            "excluded to a documented side-list (rule_set semantics), never rewritten"},
]

RULE_SEQ = ["R1", "R2", "R3", "R4", "R5", "R6", "R7", "R8"]  # applied to every formula


def charset_norm(s):
    """R1-R8 applied verbatim (also used for si_units keys/values)."""
    import unicodedata
    out = unicodedata.normalize("NFKC", s or "")
    for k, v in SUP.items():
        out = out.replace(k, v)
    out = out.replace("·", "*").replace("⋅", "*").replace("×", "*")
    out = out.replace("−", "-").replace("–", "-").replace("—", "-")
    out = out.replace("**", "^")
    for k, v in GREEK.items():
        out = out.replace(k, v)
    out = out.replace("==", "=")
    return re.sub(r"\s+", " ", out).strip()


def charset_pipeline(s):
    """R1-R8 step-wise with per-rule fired flags (deterministic order)."""
    import unicodedata
    fired = []
    out = unicodedata.normalize("NFKC", s or "")
    if out != (s or ""):
        fired.append("R1")
    pre = out
    for k, v in SUP.items():
        out = out.replace(k, v)
    if out != pre:
        fired.append("R2")
        pre = out
    for a, b in (("·", "*"), ("⋅", "*"), ("×", "*")):
        out = out.replace(a, b)
    if out != pre:
        fired.append("R3")
        pre = out
    for a, b in (("−", "-"), ("–", "-"), ("—", "-")):
        out = out.replace(a, b)
    if out != pre:
        fired.append("R4")
        pre = out
    out = out.replace("**", "^")
    if out != pre:
        fired.append("R5")
        pre = out
    for k, v in GREEK.items():
        out = out.replace(k, v)
    if out != pre:
        fired.append("R6")
        pre = out
    out = out.replace("==", "=")
    if out != pre:
        fired.append("R7")
    ws_before = bool(re.search(r"\s{2,}|^\s|\s$", s or ""))
    out = re.sub(r"\s+", " ", out).strip()
    if ws_before:
        fired.append("R8")
    return out, fired


def implicit_mult_insert(f):
    """R10; returns (formula, applied?).

    Insert '*' only at lexical boundaries: a maximal number token
    (digit run, not preceded by an identifier character) followed by an
    identifier-start letter, or ')' followed by identifier/digit/'('.
    This never splits an identifier token (t2g, H2O, x2 stay intact);
    skipped when the letter run is a scientific-notation exponent (1e5).
    """
    before = f
    f = re.sub(r"(?<![A-Za-z0-9_])(\d[\d.]*)(?=[A-Za-z_](?![-+]?\d))", r"\1*", f)
    f = re.sub(r"\)(?=[A-Za-z0-9_(])", ")*", f)
    return f, f != before


def norm_id(raw):
    s = re.sub(r"[^a-z0-9_]+", "_", (raw or "").strip().lower())
    s = re.sub(r"_+", "_", s).strip("_")
    if not s:
        s = "law"
    if not re.match(r"^[a-z]", s):
        s = "law_" + s
    return s


# ── structural gates ─────────────────────────────────────────────────────────

def idents_of(f):
    return IDENT_RE.findall(f)


def check_structural(law):
    """Stage-2 structural gate (fail reasons returned as list)."""
    errs = []
    f = law["formula"]
    for op in BANNED_OPS:
        if op in f:
            errs.append(f"banned comparison operator `{op}`")
            break
    if len(f.strip()) < 3:
        errs.append("formula too short (<3)")
    parts = f.split("=")
    if len(parts) == 2:
        if not parts[0].strip() or not parts[1].strip():
            errs.append("bare equation separator (one side empty)")
    elif len(parts) == 1:
        errs.append("missing equation separator")
    else:
        errs.append(f"multiple equation separators ({len(parts) - 1})")
    missing = sorted({i for i in idents_of(f) if i not in law["symbols"]})
    if missing:
        errs.append("undeclared symbols: " + ", ".join(missing))
    if not re.match(r"^[a-z][a-z0-9_]*$", law["id"]):
        errs.append(f"id not matching ^[a-z][a-z0-9_]*$: {law['id']}")
    return errs


def check_machine_gate(law):
    """Full nine-element admission mirror (rule-side entry gate)."""
    errs = check_structural(law)
    if len(law["symbols"]) < 2:
        errs.append(f"symbols must have >= 2 entries (has {len(law['symbols'])})")
    if any(not isinstance(v, str) for v in law["symbols"].values()):
        errs.append("symbols values must all be strings")
    if not isinstance(law.get("si_units"), dict):
        errs.append("si_units must be an object")
    elif any(not isinstance(v, str) or not v.strip() for v in law["si_units"].values()):
        errs.append("si_units values must all be non-empty strings")
    if not isinstance(law.get("domain"), str) or not law.get("domain", "").strip():
        errs.append("domain missing or empty")
    for key in ("solvable", "derivation"):
        v = law.get(key)
        if not isinstance(v, list) or any(not isinstance(x, str) for x in v):
            errs.append(f"{key} missing or not an array of strings")
    if law.get("tier") not in ("light", "full"):
        errs.append("tier missing or not light|full")
    if not (isinstance(law.get("max_version_depth"), int) and law["max_version_depth"] >= 1):
        errs.append("max_version_depth must be an integer >= 1")
    if law.get("monotone_form") is not True:
        errs.append("monotone_form must be true")
    src = (law.get("metadata") or {}).get("source")
    if not isinstance(src, str) or not src.strip():
        errs.append("metadata.source missing or empty")
    return errs


def substitution_grammar_ok(formula):
    """Substitution-grammar conformance probe (strategy report, not a gate).

    Conformance = LHS is a bare identifier and the RHS parses under the
    multiplication/division/power grammar (factors joined by * or /, factor :=
    identifier | number | (expr) | factor^integer-or-identifier).
    """
    lhs, _, rhs = formula.partition("=")
    lhs = lhs.strip()
    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", lhs):
        return False
    s = rhs.strip().replace(" ", "")
    if not s:
        return False
    pos = 0

    def factor():
        nonlocal pos
        if pos < len(s) and s[pos] == "(":
            pos += 1
            if not expr():
                return False
            if pos < len(s) and s[pos] == ")":
                pos += 1
                return True
            return False
        if pos < len(s) and s[pos] == "-":
            pos += 1
        m_num = re.match(r"\d+(\.\d+)?", s[pos:])
        if not m_num:
            m_id = re.match(r"[A-Za-z_][A-Za-z0-9_]*", s[pos:])
            if not m_id:
                return False
            pos += m_id.end()
        else:
            pos += m_num.end()
        return True

    def pow_tail():
        nonlocal pos
        while pos < len(s) and s[pos] == "^":
            m = re.match(r"\^\(?[A-Za-z0-9_.+-]+\)?", s[pos:])
            if not m:
                return False
            pos += m.end()
        return True

    def expr():
        nonlocal pos
        if not factor():
            return False
        if not pow_tail():
            return False
        while pos < len(s) and s[pos] in "*/":
            pos += 1
            if not factor():
                return False
            if not pow_tail():
                return False
        return True

    if not expr():
        return False
    return pos == len(s)


# ── pool assembly ────────────────────────────────────────────────────────────

def classify_c(row):
    """Mirror of the stage-0 classification (eval_formula first, then notation)."""
    ef = (row.get("eval_formula") or "").strip()
    nz = charset_norm(row.get("notation_zh") or "")
    if not ef and not nz:
        return "X0", None, nz, ef
    if not ef:
        has_cjk = any("\u4e00" <= ch <= "\u9fff" for ch in nz)
        if has_cjk:
            return "X1", None, nz, ef
        eqs = nz.count("=")
        extra = sorted({ch for ch in nz if ch not in
                        set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
                            "0123456789_*/^()+-= .")})
        implicit = bool(re.search(r"\d[A-Za-z_]", nz) or re.search(r"\)[A-Za-z_]", nz))
        if eqs == 0:
            return "X2", None, nz, ef
        if eqs > 1:
            return "X3", nz, nz, ef
        if extra:
            return "X4", nz, nz, ef
        if implicit:
            return "X5", nz, nz, ef
        return "C2a", nz, nz, ef
    return "C1", ef, nz, ef


def build_products(records):
    products = []
    provenance = []
    x3_map = {}
    x5_map = {}
    excluded = []       # K-side exclusions (X0/X1/X2) with reasons
    excluded_forms = []          # R13: non-identity forms routed out
    excluded_forms_by_tag = {}
    counters = {"A": 0, "B": 0, "C1": 0, "C2a": 0, "X3": 0, "X4": 0, "X5": 0}
    seen_ids = {}

    def emit(payload, prov):
        pid = payload["id"]
        if pid in seen_ids:
            n = 2
            while f"{pid}_dup{n}" in seen_ids:
                n += 1
            prov["id_collision"] = True
            prov["id_before"] = pid
            pid = f"{pid}_dup{n}"
            payload["id"] = pid
        seen_ids[pid] = True
        products.append(payload)
        provenance.append(prov)

    for rec in records:
        src = rec["_src"]
        batch = src["batch"]
        tag = None
        formula_in = None

        if batch == "C":
            row = rec["row"]
            tag, formula_in, nz, ef = classify_c(row)
            if tag in ("X0", "X1", "X2"):
                reason = {"X0": "no eval_formula and no notation",
                          "X1": "CJK prose notation",
                          "X2": "no equation separator"}[tag]
                excluded.append({"src": src, "tag": tag, "reason": reason})
                continue
            counters[tag] += 1
            rule_id = row.get("id") or ""
            name = row.get("name_zh") or ""
            field = row.get("field") or ""
            subdomain = row.get("subdomain") or ""
            computability = row.get("computability") or ""
            src_ref = (row.get("source_ref_zh") or "").strip()
            ver_ref = (row.get("verification_ref") or "").strip()
            eval_output = (row.get("eval_output") or "").strip()
            deriv = [x for x in (src_ref, ver_ref) if x]
            solvable = [eval_output] if eval_output else []
            domain = " / ".join(x for x in (field, subdomain) if x)
            tier = "light" if computability == "partial" else "full"
            si_units_in = {}
            src_symbols = []
            src_desc = {}
        elif batch == "A":
            rule = rec["rule"]
            tag = "A"
            counters[tag] += 1
            dom = rule.get("domain") or {}
            meta = rule.get("metadata") or {}
            rule_id = rule.get("rule_id") or ""
            name = rule.get("name") or ""
            formula_in = dom.get("formula") or ""
            src_symbols = [s for s in (dom.get("symbols") or []) if isinstance(s, str)]
            si_units_in = meta.get("si_units") if isinstance(meta.get("si_units"), dict) else {}
            eval_output = (dom.get("output") or "").strip()
            src_ref = ""
            ver_ref = ""
            deriv = []
            solvable = [eval_output] if eval_output else []
            domain = ""
            tier = None
            src_desc = {}
            computability = ""
            field = ""
            subdomain = ""
        else:  # B
            call = rec["call"]
            tag = "B"
            counters[tag] += 1
            rule_id = call.get("rule_id") or ""
            name = call.get("name") or ""
            formula_in = call.get("formula") or ""
            src_symbols = [s for s in (call.get("symbols") or []) if isinstance(s, str)]
            si_in = call.get("si_units")
            si_units_in = si_in if isinstance(si_in, dict) else {}
            eval_output = (call.get("output") or "").strip()
            refs = [r for r in (call.get("references") or []) if isinstance(r, str) and r.strip()]
            src_ref = refs[0] if refs else ""
            ver_ref = ""
            deriv = refs
            solvable = [eval_output] if eval_output else []
            domain = ""
            tier = None
            src_desc = call.get("symbol_descriptions") if isinstance(call.get("symbol_descriptions"), dict) else {}
            computability = ""
            field = ""
            subdomain = ""

        # ── transforms ──
        applied = []
        f, fired = charset_pipeline(formula_in)
        applied.extend(fired)

        base_id = norm_id(rule_id)
        name_out = charset_norm(name)

        # R13: non-identity forms are structurally outside the law_set contract
        # (equation-always-true semantics; inequality/condition statements belong
        # to rule_set entries). Routed to a documented side-list, never rewritten.
        has_banned = any(op in f for op in BANNED_OPS)
        segs = f.split("=")
        if has_banned or len(segs) < 2:
            route = ("inequality-form (banned comparison operator present)" if has_banned
                     else "no equation separator (non-identity form)")
            excluded_forms.append({
                "src": src, "rule_id": rule_id, "tag": tag,
                "formula": f, "route": route,
                "dest": "rule_set semantics (not law_set material)",
            })
            excluded_forms_by_tag[tag] = excluded_forms_by_tag.get(tag, 0) + 1
            continue

        pieces = []
        split_info = None
        if len(segs) > 2:
            # chain split is only valid for plain equality chains; a multi-'='
            # formula carrying statement markers (set-builder, quantifiers,
            # logic connectives) is a non-identity statement -> R13 route
            statement_markers = "{}:;,∀∃∈∧∨→←↔⟺¬⇒⇔"
            if any(ch in f for ch in statement_markers):
                excluded_forms.append({
                    "src": src, "rule_id": rule_id, "tag": tag,
                    "formula": f,
                    "route": "multiple equation separators with statement markers "
                             "(non-chain form)",
                    "dest": "rule_set semantics (not law_set material)",
                })
                excluded_forms_by_tag[tag] = excluded_forms_by_tag.get(tag, 0) + 1
                continue
            segs2 = [s.strip() for s in segs]
            for k in range(len(segs2) - 1):
                pid = f"{base_id}_p{k + 1}"
                pieces.append((pid, f"{segs2[k]} = {segs2[k + 1]}"))
            split_info = {"parent_formula_before": f,
                          "products": [{"id": p[0], "formula_after": p[1]} for p in pieces]}
            x3_entry = {
                "rule_id": rule_id, "tag": tag, "pieces": len(pieces),
                "parent_formula_before": f,
                "products": split_info["products"],
            }
            x3_map[f"{src['path']}#{src['key']}"] = x3_entry
            applied.append("R9")
        else:
            pieces = [(base_id, f)]

        for k, (pid, pf) in enumerate(pieces):
            pf_before = pf
            mf, mult_applied = implicit_mult_insert(pf)
            if mult_applied:
                applied.append("R10")
                x5_map[pid] = {"before": pf_before, "after": mf}
            if split_info:
                # mapping table always mirrors the final product formula (post-R10)
                split_info["products"][k]["formula_after"] = mf
            applied_prod = list(dict.fromkeys(applied))

            # R11 symbols rebuild
            f_idents = list(dict.fromkeys(idents_of(mf)))
            keys = list(f_idents)
            kept, dropped = [], []
            for s in src_symbols:
                if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", s):
                    if s in keys:
                        kept.append(s)
                    else:
                        keys.append(s)
                        kept.append(s)
                else:
                    dropped.append(s)
            sym_obj = {key: (charset_norm(src_desc.get(key)) if src_desc.get(key) else "")
                       for key in keys}

            # si_units: verbatim values with charset normalization on keys/values
            si_out = {}
            for uk, uv in si_units_in.items():
                nk, nv = charset_norm(str(uk)), charset_norm(str(uv))
                si_out[nk] = nv

            # R12 uniform declarations + source mapping
            source_status = "source" if src_ref else "pending"
            source = src_ref if src_ref else f"pending-citation:{pid}"
            payload = {
                "id": pid,
                "formula": mf,
                "symbols": sym_obj,
                "si_units": si_out,
                "max_version_depth": 1,
                "monotone_form": True,
                "metadata": {"source": source},
            }
            if domain:
                payload["domain"] = domain
            if solvable:
                payload["solvable"] = solvable
            if deriv:
                payload["derivation"] = deriv
            if tier:
                payload["tier"] = tier
            prov = {
                "id": pid,
                "src": src,
                "tag": tag,
                "rule_id": rule_id,
                "name": name_out,
                "applied_rules": applied_prod,
                "formula_before": formula_in,
                "formula_after": mf,
                "symbols_added": [s for s in f_idents if s not in kept],
                "symbols_kept": kept,
                "symbols_dropped": dropped,
                "source_status": source_status,
                "si_units_status": "source" if si_units_in else "placeholder-empty",
                "output": eval_output,
                "computability": computability,
                "field": field,
                "subdomain": subdomain,
            }
            if split_info:
                prov["split"] = {"parent_id": base_id, "piece": k + 1,
                                 "pieces": len(pieces)}
            emit(payload, prov)

    return products, provenance, x3_map, x5_map, excluded, counters, excluded_forms, excluded_forms_by_tag


def dump_jsonl(path, rows):
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        for r in rows:
            fh.write(json.dumps(r, ensure_ascii=False, sort_keys=True) + "\n")


def dump_json(path, obj):
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(json.dumps(obj, ensure_ascii=False, sort_keys=True, indent=2) + "\n")


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    repo = os.path.dirname(here)
    pool_dir = os.path.join(repo, "data", "migration", "law-pool")
    inp = os.path.join(pool_dir, "input-law-pool.jsonl")
    if not os.path.exists(inp):
        print(f"input raw export not found: {inp}")
        return 2

    with open(inp, "r", encoding="utf-8-sig") as fh:
        records = [json.loads(line) for line in fh if line.strip()]

    products, provenance, x3_map, x5_map, excluded, counters, excluded_forms, \
        excluded_forms_by_tag = build_products(records)

    pool_in = counters["A"] + counters["B"] + counters["C1"] + counters["C2a"] \
        + counters["X3"] + counters["X4"] + counters["X5"]
    expect = {"A": 135, "B": 146, "C1": 25, "C2a": 30, "X3": 10, "X4": 60, "X5": 13}
    mismatch = {k: (counters[k], expect[k]) for k in expect if counters[k] != expect[k]}
    if mismatch or pool_in != 419 or len(excluded) != 209:
        print("INPUT/COUNT CONTRACT VIOLATION:")
        print(f"  pool_in={pool_in} (expect 419)  excluded={len(excluded)} (expect 209)")
        print(f"  per-tag counts={counters}")
        print(f"  expected={expect}  mismatch={mismatch}")
        return 2

    # ── structural gate over products ──
    per_product = []
    fails = []
    mg_reasons = {}
    gate_stats = {"products": len(products), "structural_pass": 0,
                  "machine_gate_pass": 0, "substitution_grammar_ok": 0}
    for p in products:
        s_errs = check_structural(p)
        m_errs = check_machine_gate(p)
        gram = substitution_grammar_ok(p["formula"])
        gate_stats["structural_pass"] += 1 if not s_errs else 0
        gate_stats["machine_gate_pass"] += 1 if not m_errs else 0
        gate_stats["substitution_grammar_ok"] += 1 if gram else 0
        for e in m_errs:
            key = e.split(":")[0].split("(")[0].strip()
            mg_reasons[key] = mg_reasons.get(key, 0) + 1
        per_product.append({"id": p["id"], "tag": None, "structural": s_errs,
                            "machine_gate": m_errs, "substitution_grammar": gram})
        if s_errs:
            fails.append({"id": p["id"], "errors": s_errs})
    tag_by_id = {pr["id"]: pr["tag"] for pr in provenance}
    for row in per_product:
        row["tag"] = tag_by_id.get(row["id"])

    known_gaps = []
    for p in products:
        if len(p["symbols"]) < 2:
            known_gaps.append({"id": p["id"], "tag": tag_by_id.get(p["id"]),
                               "gap": "symbols<2 (deployment schema minProperties: 2)"})
        if not p.get("domain"):
            known_gaps.append({"id": p["id"], "tag": tag_by_id.get(p["id"]),
                               "gap": "domain missing (mechanical assembly stage)"})
    no_si = [p["id"] for p in products if not p["si_units"]]

    report = {
        "input_records": len(records),
        "pool_in": pool_in,
        "pool_in_by_tag": counters,
        "excluded_k_side": len(excluded),
        "excluded_non_identity_forms": {
            "count": len(excluded_forms),
            "by_tag": excluded_forms_by_tag,
            "declared_rule": "R13",
        },
        "products": len(products),
        "split_products": sum(1 for pr in provenance if "split" in pr),
        "accounting": "pool_in (419) - non_identity_excluded + extra_split_products = products",
        "gates": gate_stats,
        "machine_gate_fail_reasons": mg_reasons,
        "structural_failures": fails,
        "known_gaps": {
            "symbols_lt2": sum(1 for g in known_gaps if g["gap"].startswith("symbols")),
            "domain_missing": sum(1 for g in known_gaps if g["gap"].startswith("domain")),
            "si_units_empty": len(no_si),
            "detail": known_gaps,
        },
        "x3_split_entries": len(x3_map),
        "x5_mult_products": len(x5_map),
    }

    # ── artifacts ──
    dump_jsonl(os.path.join(pool_dir, "normalized-law-pool.jsonl"), products)
    dump_jsonl(os.path.join(pool_dir, "provenance-law-pool.jsonl"), provenance)
    dump_jsonl(os.path.join(pool_dir, "excluded-non-identity-forms.jsonl"), excluded_forms)
    dump_json(os.path.join(pool_dir, "x3-split-map.json"),
              {"declared_rule": "R9", "entries": x3_map})
    dump_json(os.path.join(pool_dir, "x5-mult-map.json"),
              {"declared_rule": "R10", "products": x5_map})
    dump_json(os.path.join(pool_dir, "structural-report.json"), report)
    dump_json(os.path.join(pool_dir, "normalization-rules.json"),
              {"version": "1.0", "charset_rule_seq": RULE_SEQ, "rules": RULES})

    manifest_files = ["input-law-pool.jsonl", "normalization-rules.json",
                      "normalized-law-pool.jsonl", "provenance-law-pool.jsonl",
                      "excluded-non-identity-forms.jsonl",
                      "x3-split-map.json", "x5-mult-map.json",
                      "structural-report.json"]
    lines = []
    for name in manifest_files:
        digest = sha256_file(os.path.join(pool_dir, name))
        lines.append(f"{digest}  {name}")
    combined = hashlib.sha256("\n".join(lines).encode("utf-8")).hexdigest()
    lines.append(f"{combined}  COMBINED")
    with open(os.path.join(pool_dir, "manifest.sha256"), "w",
              encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(lines) + "\n")

    print(f"pool_in={pool_in} products={len(products)} "
          f"structural_pass={gate_stats['structural_pass']}/{len(products)} "
          f"machine_gate_pass={gate_stats['machine_gate_pass']}/{len(products)} "
          f"substitution_grammar_ok={gate_stats['substitution_grammar_ok']}")
    print(f"excluded_k_side={len(excluded)} x3_entries={len(x3_map)} "
          f"x5_products={len(x5_map)} symbols_lt2={report['known_gaps']['symbols_lt2']} "
          f"domain_missing={report['known_gaps']['domain_missing']} "
          f"si_units_empty={len(no_si)}")
    print(f"combined_digest={combined}")
    if fails:
        print("STRUCTURAL FAILURES (fail-visible):")
        for f_ in fails[:20]:
            print(f"  {f_['id']}: {'; '.join(f_['errors'])}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
