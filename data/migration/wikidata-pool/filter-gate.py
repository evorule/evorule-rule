#!/usr/bin/env python3
"""Deterministic filter gate for the Wikidata P2534 (formula) entity dump.

Implements the filter-gate criteria stack from the approved filter-gate
design document (knowledge repo, filter-gate design, section 1-2):

  dedup stage       QID dedup (first occurrence kept, duplicates logged)
  shape stage       P2534 shape (exactly one claim, mainsnak=value, string,
      non-empty)
  noise stage       prime-noise exclusion (P31 contains Q49008, or
      label/description contains the substring "prime", case-insensitive)
  equation stage    equation shape (exactly one "=") + base charset check
  mapping stage     mechanical LaTeX-subset mapping via a token whitelist
                    (fail-closed: tokens outside the whitelist are rejected)
      Deviation from the design table, recorded here: \\over (infix division)
      is rejected (token:over) because infix operand boundaries are not
      mechanically well-defined; the design table had placed it under
      bracketed division.
  replay stage      structural replay: recursive-descent AST check on the
                    mapped formula. Dimensional verdicts require per-symbol
                    dimension declarations, which candidates do not carry
                    yet; verdict stays pending_dimensions until the
  poolcheck stage   pool duplicate check (text-level and mapped-level keys
                    against the law pool and knowledge bundle; hits are
                    logged as dup_of and are NOT rejected).

Determinism contract: no randomness, no timestamps, no locale dependence in
any output; entities are processed in input-file order and trace rows are
written in the same order. Re-running on the same input must produce
byte-identical outputs (the funnel-trace SHA256 is embedded in each
candidate row as filter_sha256).

All paths are passed explicitly via CLI arguments; nothing about the host
machine is baked in. The raw dump itself is never copied into any output.

Usage:
  python filter-gate.py --input <p2534_entities.json> \
      --law-pool <normalized-law-pool.jsonl> \
      --knowledge-bundle <science-knowledge-v1.bundle.json> \
      --out-dir <output directory>

Outputs (in --out-dir):
  funnel-trace.jsonl    one row per entity, in input order
  candidates.jsonl      surviving rows (candidates) with provenance
  structural-report.json  per-layer counters + accounting identity
"""

import argparse
import hashlib
import io
import json
import os
import re
from collections import Counter, OrderedDict

# ---------------------------------------------------------------------------
# token whitelist (mapping stage; design document section 2). Anything not
# listed here is rejected with reason code token:<name>.
# ---------------------------------------------------------------------------

SPACING = {"quad", "qquad", ",", "!", ";", ":", " "}
DIRECT = {"times": "*", "cdot": "*", "div": "/"}
FRAC_LIKE = {"frac", "dfrac", "tfrac", "cfrac"}
FUNCS = {"sqrt", "sin", "cos", "tan", "ln", "log", "exp",
         "max", "min", "det", "sup", "inf", "gcd", "sgn",
         "arctan", "arccos", "arcsin", "sec", "cot", "csc",
         "sinh", "cosh", "tanh", "coth", "lg"}
GREEK = {
    "alpha": "alpha", "beta": "beta", "gamma": "gamma", "delta": "delta",
    "epsilon": "epsilon", "varepsilon": "varepsilon", "zeta": "zeta",
    "eta": "eta", "theta": "theta", "vartheta": None, "iota": "iota",
    "kappa": "kappa", "varkappa": None, "lambda": "lambda", "mu": "mu",
    "nu": "nu", "xi": "xi", "pi": "pi", "rho": "rho", "varrho": None,
    "sigma": "sigma", "varsigma": None, "tau": "tau", "upsilon": "upsilon",
    "phi": "phi", "varphi": "varphi", "chi": "chi", "psi": "psi",
    "omega": "omega", "Gamma": "Gamma", "Delta": "Delta", "Theta": "Theta",
    "Lambda": "Lambda", "Xi": None, "Pi": "Pi", "Sigma": "Sigma",
    "Phi": "Phi", "Psi": "Psi", "Omega": "Omega",
}
SHELLS = {"mathrm", "text", "mathbf", "boldsymbol", "mathcal", "mathit",
          "mathsf", "mathfrak", "operatorname", "rm", "bold", "mbox",
          "textrm", "textsf", "textit", "mathop", "Bbb", "hbox"}
ACCENTS = {"hat": "_hat", "widehat": "_hat", "bar": "_bar", "overline": "_obar",
           "underline": "_ul", "tilde": "_tilde", "widetilde": "_tilde",
           "dot": "_dot", "ddot": "_ddot", "vec": "_vec", "acute": "_acute",
           "check": "_check", "prime": "_prime"}
BRACKET_SHELLS = {"left", "right", "big", "Big", "bigg", "Bigg",
                  "bigl", "bigr", "Bigl", "Bigr", "biggl", "biggr",
                  "Biggl", "Biggr", "lbrace", "rbrace"}
SPECIAL_KEEP = {"hbar": "hbar"}
REJECT_KNOWN = {"lfloor", "rfloor", "lceil", "rceil", "over"}

TOKEN_RE = re.compile(r"\\([a-zA-Z]+|.)")
BASE_CHARS = re.compile(r"[a-zA-Z0-9\\{}^_()+\-*/=,.!|:;' \t]")
BASE_CHARS_FULL = re.compile(r"^[a-zA-Z0-9\\{}^_()+\-*/=,.!|:;' \t]*$")
MAPPED_CHARS_FULL = re.compile(r"^[A-Za-z0-9_()+\-*/^,.;:= \t]*$")
IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
PRIME_SUBSTR = "prime"
PRIME_QID = "Q49008"


def norm_text(f):
    return re.sub(r"\s+", "", f).replace("\\times", "*").replace("\\cdot", "*")


def tokenize(f):
    """Split into (token, kind) pairs; kind is 'cmd' or 'char', in order."""
    result = []
    pos = 0
    for m in TOKEN_RE.finditer(f):
        if m.start() > pos:
            for ch in f[pos:m.start()]:
                result.append((ch, "char"))
        result.append((m.group(1), "cmd"))
        pos = m.end()
    if pos < len(f):
        for ch in f[pos:]:
            result.append((ch, "char"))
    return result


def read_group(tokens, i):
    """Read a balanced {...} group at tokens[i] ('{'). Returns (text, next)."""
    if i >= len(tokens) or tokens[i] != ("{", "char"):
        return None, i
    depth = 0
    buf = []
    j = i
    while j < len(tokens):
        t, kind = tokens[j]
        if kind == "char" and t == "{":
            depth += 1
            if depth > 1:
                buf.append(t)
        elif kind == "char" and t == "}":
            depth -= 1
            if depth == 0:
                return "".join(buf), j + 1
            buf.append(t)
        elif kind == "cmd":
            # rebuild the source text: command tokens keep their backslash so
            # the recursive re-tokenization sees the same commands
            buf.append("\\" + t)
        else:
            buf.append(t)
        j += 1
    return None, i


def brace_depth(s):
    d = md = 0
    for ch in s:
        if ch == "{":
            d += 1
            if d > md:
                md = d
        elif ch == "}":
            d -= 1
    return md


def map_tokens(tokens, depth=0):
    """Mechanical mapping pass. Returns (mapped_text or None, reason or None).

    Group parameters read by read_group are raw text and are re-mapped
    recursively (depth-limited) so nested \\frac/\\shell/_{sub} forms resolve.
    """
    if depth > 4:
        return None, "frac_depth"
    out = []
    i = 0
    n = len(tokens)
    while i < n:
        t, kind = tokens[i]
        if kind == "char":
            if t in "{}":
                # bare brace group = grouping shell around exponents/subscripts
                # (e.g. x^{2}, k_{B}); parameter groups were consumed by their
                # own readers -> strip the braces, keep the content
                i += 1
                continue
            if t == "'":
                # prime (derivative notation x') -> variant symbol name,
                # same convention as the \prime accent mapping
                out.append("_prime")
                i += 1
                continue
            out.append(t)
            i += 1
            continue
        if t in SPACING:
            i += 1
            continue
        if t in DIRECT:
            out.append(DIRECT[t])
            i += 1
            continue
        if t in FRAC_LIKE:
            a_raw, j = read_group(tokens, i + 1)
            if a_raw is None:
                return None, "frac_parse"
            b_raw, j2 = read_group(tokens, j)
            if b_raw is None:
                return None, "frac_parse"
            a, r1 = map_tokens(tokenize(a_raw), depth + 1)
            if a is None:
                return None, r1
            b, r2 = map_tokens(tokenize(b_raw), depth + 1)
            if b is None:
                return None, r2
            out.append("(" + a + ")/(" + b + ")")
            i = j2
            continue
        if t == "sqrt":
            a_raw, j = read_group(tokens, i + 1)
            if a_raw is None:
                return None, "sqrt_parse"
            a, r1 = map_tokens(tokenize(a_raw), depth + 1)
            if a is None:
                return None, r1
            out.append("sqrt(" + a + ")")
            i = j
            continue
        if t in FUNCS:
            if i + 1 < n and tokens[i + 1] == ("^", "char"):
                return None, "func_power"
            a_raw, j = read_group(tokens, i + 1)
            if a_raw is not None:
                a, r1 = map_tokens(tokenize(a_raw), depth + 1)
                if a is None:
                    return None, r1
                out.append(t + "(" + a + ")")
                i = j
            else:
                out.append(t)
                i += 1
            continue
        if t in GREEK:
            name = GREEK[t]
            if name is None:
                return None, "token:" + t
            out.append(name)
            i += 1
            continue
        if t in SHELLS:
            a_raw, j = read_group(tokens, i + 1)
            if a_raw is None:
                return None, "shell_parse:" + t
            a, r1 = map_tokens(tokenize(a_raw), depth + 1)
            if a is None:
                return None, r1
            out.append(a)
            i = j
            continue
        if t in ACCENTS:
            a_raw, j = read_group(tokens, i + 1)
            if a_raw is None:
                return None, "accent_parse:" + t
            a, r1 = map_tokens(tokenize(a_raw), depth + 1)
            if a is None:
                return None, r1
            if not IDENT_RE.fullmatch(a):
                return None, "accent_target:" + t
            out.append(a + ACCENTS[t])
            i = j
            continue
        if t in BRACKET_SHELLS:
            i += 1
            continue
        if t in SPECIAL_KEEP:
            out.append(SPECIAL_KEEP[t])
            i += 1
            continue
        if t in REJECT_KNOWN:
            return None, "token:" + t
        return None, "token:" + t
    return "".join(out), None


# ---------------------------------------------------------------------------
# structural replay stage: recursive-descent AST check.
# equality := expr '=' expr ; expr := term (('+'|'-') term)* ;
# term := unary (('*'|'/') unary)* ; unary := '-' unary | power ;
# power := atom ('^' exponent)? ; atom := number | ident | func '(' args ')'
#         | '(' expr ')' ; exponent := ['-'] ( number | ident | '(' expr ')' )
# ---------------------------------------------------------------------------

class AstParser(object):
    def __init__(self, s):
        self.s = s
        self.i = 0
        self.n = len(s)

    def fail(self, why):
        raise ValueError(why)

    def peek(self):
        while self.i < self.n and self.s[self.i] in " \t":
            self.i += 1
        # "\0" sentinel for end-of-input: "" must never be returned because
        # ('' in '*/') is True in Python (empty-string substring trap), which
        # would make the term/expr loops consume past the end
        return self.s[self.i] if self.i < self.n else "\0"

    def parse_all(self):
        self.parse_expr()
        if self.peek() != "=":
            self.fail("expected equality")
        self.i += 1
        self.parse_expr()
        if self.peek() != "\0":
            self.fail("trailing input")

    def parse_expr(self):
        self.parse_term()
        while True:
            c = self.peek()
            if c in "+-":
                self.i += 1
                self.parse_term()
            elif c in ("\0", "=", ")", ","):
                # stop at end / equality / closing paren / argument separator;
                # the caller (atom paren, arg list, parse_all) consumes them
                return
            else:
                self.fail("expr:" + repr(c))

    def parse_term(self):
        self.parse_unary()
        while True:
            c = self.peek()
            if c in "*/":
                self.i += 1
                self.parse_unary()
            elif c and (c.isalnum() or c in "(."):
                # juxtaposition = implicit multiplication (standard reading of
                # LaTeX math like "mv^2" or "1/2 rho v^2"); deterministic
                self.parse_unary()
            else:
                return

    def parse_unary(self):
        if self.peek() == "-":
            self.i += 1
            self.parse_unary()
        else:
            self.parse_power()

    def parse_power(self):
        self.parse_atom()
        if self.peek() == "^":
            self.i += 1
            self.parse_exponent()

    def parse_exponent(self):
        c = self.peek()
        if c == "-":
            self.i += 1
            self.parse_exponent()
            return
        if c == "(":
            self.i += 1
            self.parse_expr()
            if self.peek() != ")":
                self.fail("paren")
            self.i += 1
            return
        if c.isdigit():
            # digits only: a letter directly after digits is juxtaposition
            # (e.g. x^2y), never part of the exponent
            m = re.match(r"\d+(\.\d+)?", self.s[self.i:])
            self.i += m.end()
            return
        m = IDENT_RE.match(self.s[self.i:])
        if m:
            self.i += m.end()
            return
        self.fail("exponent")

    def parse_atom(self):
        c = self.peek()
        if c == "^":
            # leading caret (isotope notation like ^{143}Nd) is not part of
            # the criticality grammar -> rejected, reason recorded
            self.fail("leading_caret")
        if c == "(":
            self.i += 1
            self.parse_expr()
            if self.peek() != ")":
                self.fail("paren")
            self.i += 1
            return
        if c.isdigit() or c == ".":
            m = re.match(r"[0-9]*\.?[0-9]+", self.s[self.i:])
            if not m:
                self.fail("number")
            self.i += m.end()
            return
        m = IDENT_RE.match(self.s[self.i:])
        if m:
            word = m.group(0)
            self.i += m.end()
            if word.endswith("_"):
                # a trailing underscore means an empty/invalid subscript
                # (e.g. S_* consumed as S_ + '*') -> rejected
                self.fail("subscript")
            # subscript folding: ident ('_' (alnum | '{' alnum '}'))+ merges
            # into a single symbol name (criticality notation form x_ij);
            # non-alphanumeric subscript content (e.g. S_*) is rejected
            while self.peek() == "_":
                self.i += 1
                if self.peek() == "{":
                    self.i += 1
                    j = self.s.find("}", self.i)
                    if j < 0:
                        self.fail("subscript")
                    sub = self.s[self.i:j]
                    if not re.fullmatch(r"[A-Za-z0-9]+", sub):
                        self.fail("subscript")
                    word += "_" + sub
                    self.i = j + 1
                else:
                    m2 = re.match(r"[A-Za-z0-9]", self.s[self.i:])
                    if not m2:
                        self.fail("subscript")
                    word += "_" + m2.group(0)
                    self.i += 1
            nxt = self.peek()
            at_paren_raw = (self.i < self.n and self.s[self.i] == "(")
            if word in FUNCS:
                if nxt != "(":
                    self.fail("func needs parens")
                while self.s[self.i] != "(":
                    self.i += 1
                self.i += 1
                self.parse_expr()
                while self.peek() == ",":
                    self.i += 1
                    self.parse_expr()
                if self.peek() != ")":
                    self.fail("paren")
                self.i += 1
            elif at_paren_raw:
                # immediate '(' after an identifier that is not a whitelisted
                # function: function-call syntax outside the whitelist ->
                # rejected (fail-closed). 'ident (' with intervening space is
                # juxtaposition (constant times parenthesized expression) and
                # is handled by the term-level juxtaposition rule.
                self.fail("func not allowed: " + word)
            return
        self.fail("atom:" + repr(c))


def parse_check(mapped):
    try:
        AstParser(mapped).parse_all()
        return True, None
    except ValueError as e:
        return False, "parse_fail:" + str(e)


def load_pool_keys(law_pool_path, kbundle_path):
    keys = set()
    with io.open(law_pool_path, "r", encoding="utf-8") as f:
        for line in f:
            r = json.loads(line)
            v = r.get("formula") or r.get("notation_zh") or ""
            if v.strip():
                keys.add(norm_text(v))
    with io.open(kbundle_path, "r", encoding="utf-8") as f:
        b = json.load(f)
    for e in b["entries"]:
        v = e["rule_body"].get("notation_zh") or ""
        if v.strip():
            keys.add(norm_text(v))
    return keys


def row(obj):
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":")) + "\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--input", required=True)
    ap.add_argument("--law-pool", required=True)
    ap.add_argument("--knowledge-bundle", required=True)
    ap.add_argument("--out-dir", required=True)
    args = ap.parse_args()

    with io.open(args.input, "r", encoding="utf-8") as f:
        doc = json.load(f)
    ents = doc["entities"]

    pool_keys = load_pool_keys(args.law_pool, args.knowledge_bundle)

    c = Counter()
    seen_qid = set()
    cand_recs = []
    trace_rows = []

    for e in ents:
        qid = e.get("id")
        claims = e.get("claims", {})

        # dedup stage
        if qid in seen_qid:
            c["d0_dup_qid"] += 1
            trace_rows.append({"qid": qid, "decision": "reject",
                               "reason_code": "dup_qid"})
            continue
        seen_qid.add(qid)

        # shape stage
        c2534 = claims.get("P2534", [])
        if len(c2534) == 0:
            c["d1_no_p2534"] += 1
            trace_rows.append({"qid": qid, "decision": "reject",
                               "reason_code": "no_p2534"})
            continue
        if len(c2534) > 1:
            c["d1_multi_p2534"] += 1
            trace_rows.append({"qid": qid, "decision": "reject",
                               "reason_code": "multi_p2534"})
            continue
        ms = c2534[0].get("mainsnak", {})
        dv = ms.get("datavalue")
        if ms.get("snaktype") != "value" or not dv or dv.get("type") != "string" \
                or not str(dv.get("value", "")).strip():
            c["d1_empty_formula"] += 1
            trace_rows.append({"qid": qid, "decision": "reject",
                               "reason_code": "empty_formula"})
            continue
        formula = dv["value"]

        # prime-noise stage
        p31_ids = []
        for st in claims.get("P31", []):
            v = st.get("mainsnak", {}).get("datavalue", {}).get("value")
            if isinstance(v, dict) and v.get("id"):
                p31_ids.append(v["id"])
        label_en = e.get("labels", {}).get("en", {}).get("value", "")
        desc_en = e.get("descriptions", {}).get("en", {}).get("value", "")
        if PRIME_QID in p31_ids:
            c["d2_prime_p31"] += 1
            trace_rows.append({"qid": qid, "decision": "reject",
                               "reason_code": "prime_p31", "formula": formula})
            continue
        if PRIME_SUBSTR in label_en.lower():
            c["d2_prime_label"] += 1
            trace_rows.append({"qid": qid, "decision": "reject",
                               "reason_code": "prime_label", "formula": formula})
            continue
        if PRIME_SUBSTR in desc_en.lower():
            c["d2_prime_desc"] += 1
            trace_rows.append({"qid": qid, "decision": "reject",
                               "reason_code": "prime_desc", "formula": formula})
            continue

        # equation-shape stage
        if formula.count("=") != 1:
            code = "not_equation" if formula.count("=") == 0 else "multi_equals"
            c["d3_" + code] += 1
            trace_rows.append({"qid": qid, "decision": "reject",
                               "reason_code": code, "formula": formula})
            continue
        if not BASE_CHARS_FULL.match(formula):
            bad = next(ch for ch in formula if not BASE_CHARS.match(ch))
            c["d3_special_chars"] += 1
            trace_rows.append({"qid": qid, "decision": "reject",
                               "reason_code": "special_chars:" + repr(bad),
                               "formula": formula})
            continue

        # mechanical mapping stage
        mapped, reason = map_tokens(tokenize(formula))
        if mapped is None:
            c["d4_reject"] += 1
            c["d4_" + reason.split(":")[0]] += 1
            trace_rows.append({"qid": qid, "decision": "reject",
                               "reason_code": reason, "formula": formula})
            continue
        if not MAPPED_CHARS_FULL.match(mapped):
            c["d4_reject"] += 1
            c["d4_mapped_charset"] += 1
            trace_rows.append({"qid": qid, "decision": "reject",
                               "reason_code": "mapped_charset",
                               "formula": formula})
            continue

        # structural replay stage
        ok, perr = parse_check(mapped)
        if not ok:
            c["d5_parse_fail"] += 1
            trace_rows.append({"qid": qid, "decision": "reject",
                               "reason_code": perr, "formula": formula,
                               "mapped": mapped})
            continue

        # pool duplicate stage (log-only)
        dup = ""
        if norm_text(formula) in pool_keys:
            dup = "pool_text"
        elif norm_text(mapped) in pool_keys:
            dup = "pool_mapped"
        c["d6_dup_" + (dup if dup else "none")] += 1

        c["candidates"] += 1
        verdict = "pending_dimensions" + ("_dup_" + dup if dup else "")
        trace_rows.append({"qid": qid, "decision": "candidate",
                           "reason_code": "mapped_ok",
                           "formula": formula, "mapped": mapped, "dup": dup})
        rec = OrderedDict()
        rec["qid"] = qid
        rec["formula_orig"] = formula
        rec["formula_mapped"] = mapped
        rec["label_en"] = label_en
        rec["desc_en"] = desc_en
        rec["verdict"] = verdict
        if dup:
            rec["dup_of"] = dup
        rec["source"] = "wikidata P2534 (filtered)"
        cand_recs.append(rec)

    trace_path = os.path.join(args.out_dir, "funnel-trace.jsonl")
    with io.open(trace_path, "w", encoding="utf-8", newline="\n") as tf:
        for r0 in trace_rows:
            tf.write(row(r0))
    with io.open(trace_path, "rb") as fh:
        trace_sha = hashlib.sha256(fh.read()).hexdigest()

    cand_path = os.path.join(args.out_dir, "candidates.jsonl")
    with io.open(cand_path, "w", encoding="utf-8", newline="\n") as cf:
        for rec in cand_recs:
            rec["filter_sha256"] = trace_sha
            cf.write(row(rec))

    # accounting identity:
    #   unique_qids == shape-stage rejections + noise-stage rejections
    #                 + mapping-stage rejects + replay parse-fails + candidates
    d1 = c["d1_no_p2534"] + c["d1_multi_p2534"] + c["d1_empty_formula"]
    d2 = c["d2_prime_p31"] + c["d2_prime_label"] + c["d2_prime_desc"]
    d3 = c["d3_not_equation"] + c["d3_multi_equals"] + c["d3_special_chars"]
    d4 = c["d4_reject"]
    d5 = c["d5_parse_fail"]
    outflow = d1 + d2 + d3 + d4 + d5 + c["candidates"]
    assert len(seen_qid) == outflow, (len(seen_qid), outflow)

    report = OrderedDict()
    report["declared_count"] = doc.get("count")
    report["entities_read"] = len(ents)
    report["unique_qids"] = len(seen_qid)
    report["d0_dup_qid"] = c["d0_dup_qid"]
    report["layers"] = OrderedDict()
    report["layers"]["shape"] = OrderedDict(
        [("no_p2534", c["d1_no_p2534"]), ("multi_p2534", c["d1_multi_p2534"]),
         ("empty_formula", c["d1_empty_formula"]), ("subtotal", d1)])
    report["layers"]["noise"] = OrderedDict(
        [("prime_p31", c["d2_prime_p31"]), ("prime_label", c["d2_prime_label"]),
         ("prime_desc", c["d2_prime_desc"]), ("subtotal", d2)])
    report["layers"]["equation_shape"] = OrderedDict(
        [("not_equation", c["d3_not_equation"]),
         ("multi_equals", c["d3_multi_equals"]),
         ("special_chars", c["d3_special_chars"]), ("subtotal", d3)])
    report["layers"]["notation_mapping"] = OrderedDict(
        [("reject", c["d4_reject"]),
         ("by_code", OrderedDict(sorted((k[3:], v) for k, v in c.items()
                                        if k.startswith("d4_")
                                        and k != "d4_reject"))),
         ("subtotal", d4)])
    report["layers"]["structural_replay"] = OrderedDict(
        [("parse_fail", c["d5_parse_fail"]), ("subtotal", d5),
         ("dimensional_verdicts", "pending_dimensions (require per-symbol "
          "dimension declarations; deferred to the human-review batch)")])
    report["layers"]["pool_duplicates"] = OrderedDict(
        [("dup_pool_text", c["d6_dup_pool_text"]),
         ("dup_pool_mapped", c["d6_dup_pool_mapped"]),
         ("unique", c["d6_dup_none"]), ("note", "log-only, not rejected")])
    report["accounting_identity"] = OrderedDict(
        [("unique_qids", len(seen_qid)), ("outflow_sum", outflow),
         ("identity_holds", len(seen_qid) == outflow),
         ("candidates", c["candidates"])])
    report["trace_sha256"] = trace_sha

    report_path = os.path.join(args.out_dir, "structural-report.json")
    with io.open(report_path, "w", encoding="utf-8", newline="\n") as rf:
        json.dump(report, rf, ensure_ascii=False, indent=2)
        rf.write("\n")

    print("candidates=%d trace_sha256=%s identity=%s"
          % (c["candidates"], trace_sha[:16], len(seen_qid) == outflow))


if __name__ == "__main__":
    main()
