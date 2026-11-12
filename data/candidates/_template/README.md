# Proposal batch template (law intake, LLM channel)

Template + discipline for LLM proposal batches feeding the law_set
publication chain (`candidates-publish`). One proposal per line: either a
bare law payload or a `{law: {...}, provenance: {...}}` envelope
(see `proposal-template.jsonl`).

## Writing discipline (apply at authoring time, not after)

1. Subscripts: `A₁` -> `A_1` (ASCII underscore form only).
2. Greek letters: transliterate to latin names (`α` -> `alpha`).
3. Chemistry concentration: `[X]` -> `X` (brackets reserved for nothing).
4. Implicit multiplication: make explicit (`mc²` -> `m * c * c` is wrong
   physics; write the correct explicit form, e.g. `E = m * c ^ 2`).
5. Exactly one equation separator per formula; both sides non-empty.
6. Negation constructs (bare `!=`, `!`, `≠` semantics) are rejected unless
   `time_anchor` is declared non-empty (time-anchored rewrite).

## Required nine elements

`id` / `formula` / `symbols` (>= 2) / `si_units` / `domain` / `solvable` /
`derivation` / `tier` (light|full) / `max_version_depth` (>= 1) /
`monotone_form` (true). `metadata.source` required.

## Multi-proposer discipline

`provenance.llm_model` must name the actual proposer for the batch. Batches
from different proposers are separate batches; cross-proposer consistency
statistics are computed at review time, never mixed into one batch.

## Non-goals (do not include in proposals)

- `form` / `intake_channel`: injected by the publication chain at batch
  level (origin declared once per batch, never carried per-entry).
- `verdict` fields: produced by the gates, not by the proposer.

## Pipeline

```
proposals.jsonl -> candidates-gate (this repo, proposals-gate CLI:
  structure nine / formula separator / negation scan; rejects get
  per-line reasons, revise and resubmit)
  -> accepted side + machine gates (L2 dimensional / dup / pollution:
     server distribution face, unit engine)
  -> accepted-candidates.jsonl (with verdicts)
  -> candidates-publish (form + intake_channel injected, v1.1 intake face)
  -> data/law_sets/<id>-v<semver>.json (main pool, law_set v1.1)
```
