#!/usr/bin/env python3
"""physics-dim-v1 candidate pipeline (deterministic, stdlib-only).

Given the frozen inputs (byte-digested):
  ../input-proposals.jsonl          LLM proposals (41 entries, frozen input)
  ../gate-outputs/l1-verdicts.jsonl L1 structure gate verdicts (rule repo)
  ../gate-outputs/l2-verdicts.jsonl L2 dimensional gate verdicts (server repo)
  ../../../law_sets/*.json          in-repo seed law sets (pollution surface)

produces byte-reproducible artifacts in the batch directory:
  accepted-candidates.jsonl / rejected-side-list.jsonl / metrics.json /
  spot-check-table.json / provenance.json / manifest.sha256

Determinism contract: no timestamps, no randomness, ordering follows input
line order, JSON written with sorted-keys where free-form. Running this
script twice on the same inputs must yield a byte-identical manifest
(double-run digest equality, verified externally).

Verdict model per entry (first matching rule wins):
  1. L1 structure fail              -> type "structural"
  2. L2 dimensional unverified      -> type "unverified-dimensions" (third
     state: skip is neither pass nor reject; kept on the side list with the
     gate reason, excluded from accepted)
  3. formula in seed law-set pool   -> type "pollution" (normalized literal
     match; algebraic-equivalence pollution detection is future work)
  4. normalized literal dup earlier -> type "dup-exact"
  5. skeleton collision cluster member named in review-decisions.jsonl
     -> type "diversity-review" (position-canonical skeleton clusters are a
     deterministic review queue; cluster membership alone does NOT reject,
     because structurally identical laws can be semantically distinct --
     e.g. F=m*a vs W=m*g are algebraically and dimensionally isomorphic.
     Cluster resolution comes from the frozen review-decisions input,
     produced by the reviewer for the project owner to spot-check)
  6. otherwise                      -> accepted
"""

import hashlib
import json
import os
import re

BATCH = "physics-dim-v1"
HERE = os.path.dirname(os.path.abspath(__file__))
BATCH_DIR = os.path.dirname(HERE)
RULE_DATA = os.path.abspath(os.path.join(BATCH_DIR, "..", ".."))

IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def sha256_file(path):
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def norm_formula(s):
    return "".join(s.split()).lower()


def skeleton(formula, symbol_names):
    """Position-canonical form: rename declared symbols to s1..sn by first
    occurrence; keep numeric literals and operators verbatim; drop spaces."""
    flat = "".join(formula.split())
    pos = {}
    for tok in IDENT_RE.findall(flat):
        if tok in symbol_names and tok not in pos:
            pos[tok] = "s%d" % (len(pos) + 1)
    out = re.sub(
        r"[A-Za-z_][A-Za-z0-9_]*",
        lambda m: pos.get(m.group(0), m.group(0)),
        flat,
    )
    return out


def load_jsonl(path):
    items = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            if line.strip():
                items.append(json.loads(line))
    return items


def dump_jsonl(path, rows):
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        for row in rows:
            f.write(json.dumps(row, ensure_ascii=False, sort_keys=True,
                               separators=(", ", ": ")) + "\n")


def dump_json(path, obj):
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        json.dump(obj, f, ensure_ascii=False, sort_keys=True, indent=2)
        f.write("\n")


def main():
    proposals_path = os.path.join(BATCH_DIR, "input-proposals.jsonl")
    seed_digest = sha256_file(os.path.join(BATCH_DIR, "seed-excerpt.md"))
    proposals_digest = sha256_file(proposals_path)

    proposals = load_jsonl(proposals_path)
    l1 = {r["id"]: r for r in load_jsonl(
        os.path.join(BATCH_DIR, "gate-outputs", "l1-verdicts.jsonl"))}
    l2 = {r["id"]: r for r in load_jsonl(
        os.path.join(BATCH_DIR, "gate-outputs", "l2-verdicts.jsonl"))}

    # pollution surface: every seed law formula in data/law_sets
    seed_formulas = {}
    law_sets_dir = os.path.join(RULE_DATA, "law_sets")
    for name in sorted(os.listdir(law_sets_dir)):
        if not name.endswith(".json"):
            continue
        doc = json.load(open(os.path.join(law_sets_dir, name),
                             encoding="utf-8"))
        for law in doc.get("laws", []):
            seed_formulas[norm_formula(law["formula"])] = (
                doc.get("id", name) + "/" + law["id"])

    seen_norm = {}
    accepted, rejected = [], []

    # frozen review decisions (human-review layer over skeleton clusters)
    decisions_path = os.path.join(BATCH_DIR, "gate-outputs",
                                  "review-decisions.jsonl")
    review_reject = {}
    if os.path.exists(decisions_path):
        for d in load_jsonl(decisions_path):
            if d.get("reject"):
                review_reject[d["reject"]] = d.get("reason", "")

    # deterministic skeleton clusters (review queue artifact)
    clusters = {}
    for entry in proposals:
        skel = skeleton(entry["formula"], entry.get("symbols", {}))
        clusters.setdefault(skel, []).append(entry["id"])
    review_queue = [
        {"skeleton": s, "members": members}
        for s, members in sorted(clusters.items()) if len(members) > 1
    ]
    dump_json(os.path.join(BATCH_DIR, "review-queue.json"), {
        "clusters": review_queue,
        "note": "deterministic position-canonical skeleton clusters; "
                "membership is a review signal only, resolution is in "
                "gate-outputs/review-decisions.jsonl",
    })

    for entry in proposals:
        eid = entry["id"]
        formula = entry["formula"]
        nf = norm_formula(formula)
        reason = None
        rtype = None

        if l1.get(eid, {}).get("l1_struct") != "pass":
            rtype, reason = "structural", l1.get(eid, {}).get("reason")
        elif l2.get(eid, {}).get("l2_dim") == "unverified":
            rtype, reason = "unverified-dimensions", l2[eid].get("reason")
        elif nf in seed_formulas:
            rtype = "pollution"
            reason = "normalized formula matches seed pool entry %s" % (
                seed_formulas[nf])
        elif nf in seen_norm:
            rtype = "dup-exact"
            reason = "normalized formula duplicates earlier proposal %s" % (
                seen_norm[nf])
        elif eid in review_reject:
            rtype = "diversity-review"
            reason = review_reject[eid]
        else:
            seen_norm[nf] = eid
            accepted.append(entry)
            continue

        rejected.append({"law": entry, "verdict": {
            "l1_struct": l1.get(eid, {}).get("l1_struct"),
            "l2_dim": l2.get(eid, {}).get("l2_dim"),
            "type": rtype, "reason": reason}})

    total = len(proposals)
    acc_n = len(accepted)
    type_counts = {}
    for r in rejected:
        t = r["verdict"]["type"]
        type_counts[t] = type_counts.get(t, 0) + 1
    dup_flagged = sum(type_counts.get(t, 0) for t in
                      ("dup-exact", "diversity-review"))

    metrics = {
        "total_proposals": total,
        "accepted": acc_n,
        "rejected_total": len(rejected),
        "rejected_by_type": dict(sorted(type_counts.items())),
        "acceptance_rate": round(acc_n / total, 4) if total else None,
        "dup_flag_rate": round(dup_flagged / total, 4) if total else None,
        "diversity_index": round(1 - dup_flagged / total, 4) if total else None,
        "spot_check_consistency": None,
        "spot_check_note": ("pending manual spot-check by project owner "
                            "(see spot-check-table.json)"),
        "metric_definitions": {
            "acceptance_rate": "accepted / total proposals",
            "dup_flag_rate": "(dup-exact + diversity-review) / total",
            "diversity_index": "1 - dup_flag_rate",
            "spot_check_consistency": "human spot-check agreement with "
                                      "machine verdicts, filled on review",
        },
    }

    def prov_block():
        return {
            "llm_model": "GLM",
            "batch": BATCH,
            "source_seed_sha256": seed_digest,
            "proposals_sha256": proposals_digest,
            "si_units_discipline": ("proposed by LLM, machine-verified for "
                                    "dimensional consistency by the unit "
                                    "engine; accepted entries are marked "
                                    "machine-verified-pending-spot-check "
                                    "and do not enter the live pool"),
            "llm_generated_marking": ("all entries carry the candidate "
                                      "suffix and remain draft-grade until "
                                      "human review; Published status "
                                      "requires human ownership"),
        }

    acc_rows = []
    for e in accepted:
        eid = e["id"]
        law_json = json.dumps(e, ensure_ascii=False, sort_keys=True,
                              separators=(",", ":"))
        acc_rows.append({
            "law": e,
            "verdict": {
                "l1_struct": "pass", "l2_dim": "pass",
                "pollution": "clear", "dup": "clear",
            },
            "provenance": prov_block(),
            "digest": hashlib.sha256(law_json.encode("utf-8")).hexdigest(),
        })
    dump_jsonl(os.path.join(BATCH_DIR, "accepted-candidates.jsonl"), acc_rows)
    dump_jsonl(os.path.join(BATCH_DIR, "rejected-side-list.jsonl"), rejected)

    # deterministic spot-check sample: every 3rd accepted entry
    sample = [acc_rows[i] for i in range(0, len(acc_rows), 3)]
    rej_sample = [rejected[i] for i in range(0, len(rejected), max(
        1, len(rejected) // 5))] if rejected else []
    dump_json(os.path.join(BATCH_DIR, "spot-check-table.json"), {
        "accepted_sample": [
            {"id": r["law"]["id"], "formula": r["law"]["formula"],
             "machine_verdict": r["verdict"]} for r in sample],
        "rejected_sample": [
            {"id": r["law"]["id"], "formula": r["law"]["formula"],
             "machine_verdict": r["verdict"]} for r in rej_sample],
    })

    provenance = dict(prov_block())
    provenance.update({
        "gate_versions": {
            "l1_structure_gate": "evorule-rule 0.5.1 "
                                 "Validator::validate_law_structure "
                                 "(nine-element admission face)",
            "l2_dimensional_gate": "evorule-law-schema validate_law_set "
                                   "(schema + formula structure + domain "
                                   "edges + finiteness + monotone form + "
                                   "dimensional verdict)",
        },
        "pipeline": {
            "tool": "tools/pipeline.py",
            "tool_sha256": sha256_file(os.path.join(HERE, "pipeline.py")),
            "determinism": "double-run manifest equality (external check)",
        },
        "rejected_disposition": ("side-list with typed reasons only "
                                 "(no negative-example or preference-pair "
                                 "derivation in this batch)"),
    })
    dump_json(os.path.join(BATCH_DIR, "provenance.json"), provenance)

    # manifest over every batch artifact (recursive) except the manifest
    manifest_path = os.path.join(BATCH_DIR, "manifest.sha256")
    entries = []
    for root, dirs, files in os.walk(BATCH_DIR):
        dirs[:] = sorted(d for d in dirs if d != "__pycache__")
        for name in sorted(files):
            p = os.path.join(root, name)
            if name == "manifest.sha256":
                continue
            rel = os.path.relpath(p, BATCH_DIR).replace(os.sep, "/")
            entries.append("%s  %s" % (sha256_file(p), rel))
    entries.sort(key=lambda e: e.split("  ", 1)[1])
    with open(manifest_path, "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(entries) + "\n")

    print("pipeline: total=%d accepted=%d rejected=%d types=%s"
          % (total, acc_n, len(rejected), json.dumps(type_counts,
                                                     sort_keys=True)))
    print("metrics: acceptance_rate=%s diversity_index=%s dup_flag_rate=%s"
          % (metrics["acceptance_rate"], metrics["diversity_index"],
             metrics["dup_flag_rate"]))


if __name__ == "__main__":
    main()
