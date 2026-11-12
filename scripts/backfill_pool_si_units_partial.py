#!/usr/bin/env python3
"""Deterministic si_units backfill for the reviewed partial-coverage pool entries.

Applies the human-reviewed SI unit table for entries whose si_units maps were
key-incomplete (have some keys, missing dimension-one and convention-unit
keys), plus the human-adjudicated semantic rewrite of the two symbolic-exponent
equilibrium-constant entries, to the migration pool. Sidecars the per-entry
transformation log + the frozen rules table, refreshes the structural report,
and re-anchors the pool manifest.

Single source: the REVIEWED_PARTIAL_SI_UNITS / DELTA_N_REWRITE constants below,
frozen from the reviewed precheck tables (candidate values approved verbatim by
the project owner on 2026-11-12; partial-derivative notation under the
denominator-dimension convention: u_tt=s^-2, u_xx=m^-2, u_t=s^-1).

Pure deterministic transformation: no network, no clock, no randomness;
byte-stable output for identical input. Line edits are surgical: only the
si_units value substring changes inside affected lines; all other lines are
preserved byte-for-byte. Entries whose formulas still fall outside the unit
engine grammar (function-call or additive notation) keep their approved values
and are expected to report check_law "skipped" -- that is the declared grammar
boundary, not a value defect.
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

NOTATION = {
    "dimensionless": '"1" (engine has no "dimensionless" token)',
    "form": "coherent SI symbols or base-unit power products (e.g. m*s^-1)",
    "prefixes": "none (SI prefixes unsupported by the unit engine)",
    "reserved_spellings": {"ohm": "ohm is the only accepted ohm spelling"},
}

# id -> {"units": full reviewed si_units map (have + approved fills),
#        "verdict": precheck three-state verdict of the completed fixture}
REVIEWED_PARTIAL_SI_UNITS = {
    "chem_electro_nernst": {
        "units": {
            "E": "V",
            "E0": "V",
            "F": "C/mol",
            "Q": "1",
            "R": "J/(mol*K)",
            "T": "K",
            "log": "1",
            "n": "1"
        },
        "verdict": "skipped"
    },
    "chem_electro_nernst_dup2": {
        "units": {
            "E": "V",
            "E0": "V",
            "F": "C/mol",
            "Q": "1",
            "R": "J/(mol*K)",
            "T": "K",
            "log": "1",
            "n": "1"
        },
        "verdict": "skipped"
    },
    "chem_eq_gibbs_eq": {
        "units": {
            "K": "1",
            "R": "J/(mol*K)",
            "T": "K",
            "delta_G": "J/mol",
            "log": "1"
        },
        "verdict": "skipped"
    },
    "chem_eq_gibbs_eq_dup2": {
        "units": {
            "K": "1",
            "R": "J/(mol*K)",
            "T": "K",
            "delta_G": "J/mol",
            "log": "1"
        },
        "verdict": "skipped"
    },
    "chem_eq_vant_hoff": {
        "units": {
            "K1": "1",
            "K2": "1",
            "R": "J/(mol*K)",
            "T1": "K",
            "T2": "K",
            "delta_H": "J/mol",
            "log": "1"
        },
        "verdict": "skipped"
    },
    "chem_eq_vant_hoff_dup2": {
        "units": {
            "K1": "1",
            "K2": "1",
            "R": "J/(mol*K)",
            "T1": "K",
            "T2": "K",
            "delta_H": "J/mol",
            "log": "1"
        },
        "verdict": "skipped"
    },
    "chem_kinetics_arrhenius": {
        "units": {
            "A": "s^-1",
            "E_a": "J/mol",
            "R": "J/(mol*K)",
            "T": "K",
            "exp": "1",
            "k": "s^-1"
        },
        "verdict": "skipped"
    },
    "chem_kinetics_arrhenius_dup2": {
        "units": {
            "A": "s^-1",
            "E_a": "J/mol",
            "R": "J/(mol*K)",
            "T": "K",
            "exp": "1",
            "k": "s^-1"
        },
        "verdict": "skipped"
    },
    "chem_kinetics_half_life_first": {
        "units": {
            "k": "s^-1",
            "log": "1",
            "t_half": "s"
        },
        "verdict": "skipped"
    },
    "chem_kinetics_half_life_first_dup2": {
        "units": {
            "k": "s^-1",
            "log": "1",
            "t_half": "s"
        },
        "verdict": "skipped"
    },
    "chem_sol_ph_def": {
        "units": {
            "H_plus": "mol/L",
            "log": "1",
            "pH": "1"
        },
        "verdict": "skipped"
    },
    "chem_sol_ph_def_dup2": {
        "units": {
            "H_plus": "mol/L",
            "log": "1",
            "pH": "1"
        },
        "verdict": "skipped"
    },
    "cross_astro_stefan_boltzmann_luminosity": {
        "units": {
            "L": "W",
            "R": "m",
            "T": "K",
            "pi": "1",
            "sigma": "W*m^-2*K^-4"
        },
        "verdict": "consistent"
    },
    "cross_bio_nernst_membrane": {
        "units": {
            "C_in": "mol/L",
            "C_out": "mol/L",
            "E_m": "V",
            "F": "C/mol",
            "R": "J/(mol*K)",
            "T": "K",
            "log": "1",
            "z": "1"
        },
        "verdict": "skipped"
    },
    "cross_bio_nernst_membrane_dup2": {
        "units": {
            "C_in": "mol/L",
            "C_out": "mol/L",
            "E_m": "V",
            "F": "C/mol",
            "R": "J/(mol*K)",
            "T": "K",
            "log": "1",
            "z": "1"
        },
        "verdict": "skipped"
    },
    "cross_compchem_boltzmann_factor": {
        "units": {
            "E": "J",
            "T": "K",
            "exp": "1",
            "k_B": "J/K",
            "w": "1"
        },
        "verdict": "skipped"
    },
    "cross_compchem_euler_angles": {
        "units": {
            "R": "1",
            "R_y": "1",
            "R_z": "1",
            "phi": "rad",
            "psi": "rad",
            "theta": "rad"
        },
        "verdict": "skipped"
    },
    "cross_compchem_lennard_jones": {
        "units": {
            "V": "J",
            "epsilon": "kg*m^2*s^-2",
            "sigma_over_r": "1"
        },
        "verdict": "skipped"
    },
    "cross_earth_barometric": {
        "units": {
            "H": "m",
            "P": "Pa",
            "P0": "Pa",
            "exp": "1",
            "h": "m"
        },
        "verdict": "skipped"
    },
    "cross_geo_barometric_height": {
        "units": {
            "H": "m",
            "P": "Pa",
            "P_0": "Pa",
            "exp": "1",
            "z": "m"
        },
        "verdict": "skipped"
    },
    "cross_info_channel_capacity": {
        "units": {
            "B": "Hz",
            "C": "s^-1",
            "S_over_N": "1",
            "log2": "1"
        },
        "verdict": "skipped"
    },
    "cross_info_channel_capacity_dup2": {
        "units": {
            "B": "Hz",
            "C": "s^-1",
            "S_N": "1",
            "log": "1"
        },
        "verdict": "skipped"
    },
    "cross_info_kullback_leibler": {
        "units": {
            "D_KL": "1",
            "log": "1",
            "p": "1",
            "q": "1"
        },
        "verdict": "skipped"
    },
    "cross_info_shannon_entropy": {
        "units": {
            "H": "1",
            "P": "1",
            "log2": "1",
            "sum": "1"
        },
        "verdict": "skipped"
    },
    "cross_info_shannon_entropy_dup2": {
        "units": {
            "H": "1",
            "log": "1",
            "p": "1"
        },
        "verdict": "skipped"
    },
    "cross_mathphys_diffusion_1d": {
        "units": {
            "D": "m^2*s^-1",
            "u_t": "s^-1",
            "u_xx": "m^-2"
        },
        "verdict": "consistent"
    },
    "cross_mathphys_schrodinger_time": {
        "units": {
            "H": "J",
            "h_bar": "J*s",
            "i": "1",
            "psi": "1",
            "psi_t": "s^-1"
        },
        "verdict": "consistent"
    },
    "cross_mathphys_wave_1d": {
        "units": {
            "c": "m/s",
            "u_tt": "s^-2",
            "u_xx": "m^-2"
        },
        "verdict": "consistent"
    },
    "math_geom_circle_area": {
        "units": {
            "A": "m^2",
            "pi": "1",
            "r": "m"
        },
        "verdict": "consistent"
    },
    "math_geom_circle_area_dup2": {
        "units": {
            "A": "m^2",
            "pi": "1",
            "r": "m"
        },
        "verdict": "consistent"
    },
    "math_geom_circle_circumference": {
        "units": {
            "C": "m",
            "pi": "1",
            "r": "m"
        },
        "verdict": "consistent"
    },
    "math_geom_circle_circumference_dup2": {
        "units": {
            "C": "m",
            "pi": "1",
            "r": "m"
        },
        "verdict": "consistent"
    },
    "math_geom_cone_volume": {
        "units": {
            "V": "m^3",
            "h": "m",
            "pi": "1",
            "r": "m"
        },
        "verdict": "consistent"
    },
    "math_geom_cone_volume_dup2": {
        "units": {
            "V": "m^3",
            "h": "m",
            "pi": "1",
            "r": "m"
        },
        "verdict": "consistent"
    },
    "math_geom_cylinder_volume": {
        "units": {
            "V": "m^3",
            "h": "m",
            "pi": "1",
            "r": "m"
        },
        "verdict": "consistent"
    },
    "math_geom_cylinder_volume_dup2": {
        "units": {
            "V": "m^3",
            "h": "m",
            "pi": "1",
            "r": "m"
        },
        "verdict": "consistent"
    },
    "math_geom_sphere_surface": {
        "units": {
            "S": "m^2",
            "pi": "1",
            "r": "m"
        },
        "verdict": "consistent"
    },
    "math_geom_sphere_surface_dup2": {
        "units": {
            "S": "m^2",
            "pi": "1",
            "r": "m"
        },
        "verdict": "consistent"
    },
    "math_geom_sphere_volume": {
        "units": {
            "V": "m^3",
            "pi": "1",
            "r": "m"
        },
        "verdict": "consistent"
    },
    "math_geom_sphere_volume_dup2": {
        "units": {
            "V": "m^3",
            "pi": "1",
            "r": "m"
        },
        "verdict": "consistent"
    },
    "math_trig_law_of_cosines": {
        "units": {
            "C": "rad",
            "a": "m",
            "b": "m",
            "c": "m",
            "cos": "1"
        },
        "verdict": "skipped"
    },
    "math_trig_law_of_cosines_dup2": {
        "units": {
            "C": "rad",
            "a": "m",
            "b": "m",
            "c": "m",
            "cos": "1"
        },
        "verdict": "skipped"
    },
    "phys_em_biot_savart": {
        "units": {
            "I": "A",
            "dB": "T",
            "dl": "m",
            "mu_0_over_4pi": "kg*m*s^-2*A^-2",
            "r": "m"
        },
        "verdict": "consistent"
    },
    "phys_quantum_schrodinger_time_indep": {
        "units": {
            "E": "J",
            "H": "kg*m^2*s^-2",
            "psi": "1"
        },
        "verdict": "consistent"
    },
    "phys_rel_length_contraction": {
        "units": {
            "L": "m",
            "L_prime": "m",
            "gamma": "1"
        },
        "verdict": "consistent"
    },
    "phys_rel_lorentz_factor": {
        "units": {
            "c": "m/s",
            "gamma": "1",
            "sqrt": "1",
            "v": "m/s"
        },
        "verdict": "skipped"
    },
    "phys_rel_lorentz_factor_dup2": {
        "units": {
            "c": "m/s",
            "gamma": "1",
            "sqrt": "1",
            "v": "m/s"
        },
        "verdict": "skipped"
    },
    "phys_rel_time_dilation": {
        "units": {
            "delta_t": "s",
            "delta_t_prime": "s",
            "gamma": "1"
        },
        "verdict": "consistent"
    },
    "phys_thermo_boltzmann": {
        "units": {
            "S": "J/K",
            "W": "1",
            "k": "J/K",
            "log": "1"
        },
        "verdict": "skipped"
    },
    "phys_thermo_boltzmann_entropy": {
        "units": {
            "S": "J/K",
            "W": "1",
            "k": "J/K",
            "log": "1"
        },
        "verdict": "skipped"
    },
    "phys_thermo_carnot_efficiency": {
        "units": {
            "T_cold": "K",
            "T_hot": "K",
            "eta": "1"
        },
        "verdict": "skipped"
    },
    "phys_wave_double_slit": {
        "units": {
            "d": "m",
            "lambda_": "m",
            "m_order": "1",
            "sin": "1",
            "theta": "rad"
        },
        "verdict": "skipped"
    },
    "phys_wave_snell": {
        "units": {
            "n1": "1",
            "n2": "1",
            "sin": "1",
            "theta1": "rad",
            "theta2": "rad"
        },
        "verdict": "skipped"
    },
    "phys_wave_snell_dup2": {
        "units": {
            "n1": "1",
            "n2": "1",
            "sin": "1",
            "theta1": "rad",
            "theta2": "rad"
        },
        "verdict": "skipped"
    },
    "phys_wave_young_double_slit": {
        "units": {
            "d": "m",
            "lambda_": "m",
            "n": "1",
            "sin": "1",
            "theta": "rad"
        },
        "verdict": "skipped"
    }
}

# id -> {"before": exact pre-rewrite si_units, "after": post-rewrite si_units}
# semantic rewrite: equilibrium constants are dimensionless activity ratios
# (IUPAC convention), so Kp/Kc become "1"; delta_n stays "1".
DELTA_N_REWRITE = {
    "chem_eq_kp_kc": {
        "after": {
            "Kc": "1",
            "Kp": "1",
            "R": "L*atm/(mol*K)",
            "T": "K",
            "delta_n": "1"
        },
        "before": {
            "Kc": "(mol/L)^Deltan",
            "Kp": "atm^Deltan",
            "R": "L*atm/(mol*K)",
            "T": "K",
            "delta_n": "1"
        },
        "verdict_expected": "skipped"
    },
    "chem_eq_kp_kc_dup2": {
        "after": {
            "Kc": "1",
            "Kp": "1",
            "R": "L*atm/(mol*K)",
            "T": "K",
            "delta_n": "1"
        },
        "before": {
            "Kc": "(mol/L)^Deltan",
            "Kp": "atm^Deltan",
            "R": "L*atm/(mol*K)",
            "T": "K",
            "delta_n": "1"
        },
        "verdict_expected": "skipped"
    }
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


def inline_units(units):
    """Serialize an si_units map exactly in the pool line style (sorted keys,
    default separators, raw non-ASCII) for surgical substring replacement."""
    return json.dumps({k: units[k] for k in sorted(units)}, ensure_ascii=False)


def main():
    rules_path = os.path.join(POOL, "si-units-backfill-partial-rules.json")
    log_path = os.path.join(POOL, "si-units-backfill-partial-log.jsonl")
    if os.path.exists(rules_path) or os.path.exists(log_path):
        sys.exit("sidecars already exist; rerun refused (idempotency guard)")

    with io.open(POOL_FILE, encoding="utf-8", newline="") as f:
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
    for tid in list(REVIEWED_PARTIAL_SI_UNITS) + list(DELTA_N_REWRITE):
        assert tid in by_id, "target missing from pool: %s" % tid

    log_rows = []
    changed = 0

    # -- partial-coverage backfill (approved candidate fills) --
    for tid in sorted(REVIEWED_PARTIAL_SI_UNITS):
        idx, row = by_id[tid]
        line = lines[idx]
        before = row.get("si_units") or {}
        assert before, "target si_units unexpectedly empty for %s" % tid
        full = REVIEWED_PARTIAL_SI_UNITS[tid]["units"]
        verdict = REVIEWED_PARTIAL_SI_UNITS[tid]["verdict"]
        # the pre-rewrite map must be exactly the have-subset of the frozen table
        for k, v in before.items():
            assert k in full and full[k] == v, (
                "have value drift vs frozen table for %s: %s" % (tid, k))
        assert len(before) < len(full), "nothing to backfill for %s" % tid
        marker = '"si_units": ' + inline_units(before)
        assert line.count(marker) == 1, (
            "expected exactly one si_units match in %s" % tid)
        new_line = line.replace(marker, '"si_units": ' + inline_units(full))
        assert new_line != line
        lines[idx] = new_line
        changed += 1
        log_rows.append({
            "after": {k: full[k] for k in sorted(full)},
            "before": {k: before[k] for k in sorted(before)},
            "engine_verdict": verdict,
            "field": "si_units",
            "file": "normalized-law-pool.jsonl",
            "id": tid,
            "reviewed": "2026-11-12",
            "rule": "human-reviewed candidate fill for key-incomplete si_units "
                    "(single source REVIEWED_PARTIAL_SI_UNITS)",
        })

    # -- semantic rewrite of the two symbolic-exponent entries --
    for tid in sorted(DELTA_N_REWRITE):
        idx, row = by_id[tid]
        line = lines[idx]
        before = row.get("si_units") or {}
        frozen = DELTA_N_REWRITE[tid]
        assert before == frozen["before"], (
            "delta-n before-state drift for %s" % tid)
        after = frozen["after"]
        marker = '"si_units": ' + inline_units(before)
        assert line.count(marker) == 1, (
            "expected exactly one si_units match in %s" % tid)
        new_line = line.replace(marker, '"si_units": ' + inline_units(after))
        assert new_line != line
        lines[idx] = new_line
        changed += 1
        log_rows.append({
            "after": {k: after[k] for k in sorted(after)},
            "before": {k: before[k] for k in sorted(before)},
            "engine_verdict": "skipped",
            "field": "si_units",
            "file": "normalized-law-pool.jsonl",
            "id": tid,
            "reviewed": "2026-11-12",
            "rule": "human-adjudicated semantic rewrite: equilibrium constants "
                    "are dimensionless activity ratios (IUPAC), Kp/Kc -> \"1\"; "
                    "delta_n stays \"1\"; symbolic exponents are outside the "
                    "unit-engine grammar, so check_law is expected to stay "
                    "skipped",
        })

    assert changed == len(REVIEWED_PARTIAL_SI_UNITS) + len(DELTA_N_REWRITE)

    text = "\n".join(lines) + ("\n" if had_final_nl else "")
    with io.open(POOL_FILE, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)

    n_consistent = sum(1 for t in REVIEWED_PARTIAL_SI_UNITS.values()
                       if t["verdict"] == "consistent")
    n_skipped = len(REVIEWED_PARTIAL_SI_UNITS) - n_consistent
    rules = {
        "delta_n_rewrite": {
            "after": {k: v for k, v in sorted(
                ((s, u) for tid in sorted(DELTA_N_REWRITE)
                 for s, u in DELTA_N_REWRITE[tid]["after"].items() if s in ("Kc", "Kp")))},
            "before": {k: v for k, v in sorted(
                ((s, u) for tid in sorted(DELTA_N_REWRITE)
                 for s, u in DELTA_N_REWRITE[tid]["before"].items() if s in ("Kc", "Kp")))},
            "rationale": "equilibrium constants are dimensionless activity ratios "
                         "(IUPAC convention); Kp/Kc -> \"1\"; delta_n stays \"1\"",
        },
        "notation": NOTATION,
        "reviewed": "2026-11-12",
        "reviewed_units": {tid: {s: v for s, v in sorted(t["units"].items())}
                           for tid, t in sorted(REVIEWED_PARTIAL_SI_UNITS.items())},
        "rule": "human-reviewed candidate fills for key-incomplete si_units plus the "
                "human-adjudicated semantic rewrite of the two symbolic-exponent "
                "equilibrium-constant entries; single source REVIEWED_PARTIAL_SI_UNITS "
                "+ DELTA_N_REWRITE in scripts/backfill_pool_si_units_partial.py; "
                "partial-derivative notation under the denominator-dimension convention "
                "(u_tt=s^-2, u_xx=m^-2, u_t=s^-1)",
        "unit_engine_precheck": "consistent %d/%d; skipped %d (formula-face grammar "
                                "boundary, declared); delta-n rewrite 2 expected skipped; "
                                "zero mismatch (dual-run SHA256-equal harness evidence)"
                                % (n_consistent, len(REVIEWED_PARTIAL_SI_UNITS), n_skipped),
    }
    dump_json(rules_path, rules)
    dump_jsonl(log_path, log_rows)

    # structural report: gap counter + backfill section
    report_path = os.path.join(POOL, "structural-report.json")
    with io.open(report_path, encoding="utf-8") as fh:
        report = json.load(fh)
    report["known_gaps"]["si_units_partial"] = 8
    report["si_units_backfill_partial"] = {
        "artifacts": ["si-units-backfill-partial-log.jsonl",
                      "si-units-backfill-partial-rules.json"],
        "delta_n_rewrite": rules["delta_n_rewrite"],
        "entries_backfilled": len(REVIEWED_PARTIAL_SI_UNITS),
        "entries_semantic_rewrite": len(DELTA_N_REWRITE),
        "notation": NOTATION,
        "reviewed": "2026-11-12",
        "unit_engine_precheck": rules["unit_engine_precheck"],
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
        "si-units-backfill-partial-log.jsonl",
        "si-units-backfill-partial-rules.json",
        "si-units-backfill-rules.json",
        "si-units-notation-normalize-log.jsonl",
        "si-units-notation-normalize-rules.json",
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

    print("backfilled %d entries (%d reviewed fills + %d delta-n rewrites); "
          "manifest re-anchored (%d files)"
          % (changed, len(REVIEWED_PARTIAL_SI_UNITS), len(DELTA_N_REWRITE), len(names)))


if __name__ == "__main__":
    sys.exit(main())
