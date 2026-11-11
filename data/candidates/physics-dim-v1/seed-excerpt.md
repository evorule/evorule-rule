# Seed Excerpt — Dimensional Analysis in Classical Physics (frozen input, physics-dim-v1)

> Frozen seed input for the physics-dim-v1 candidate batch. Original composition
> (project-authored textbook-style excerpt), assembled 2026-11-11. This file is
> an input artifact: the pipeline promises byte-reproducible outputs given this
> file plus `input-proposals.jsonl` and the recorded gate versions.

## 1. Physical quantities and SI base units

Every physical quantity carries a unit. The SI system defines seven base
quantities, each with a base unit:

| Quantity | Unit name | Symbol |
|---|---|---|
| length | metre | m |
| mass | kilogram | kg |
| time | second | s |
| electric current | ampere | A |
| thermodynamic temperature | kelvin | K |
| amount of substance | mole | mol |
| luminous intensity | candela | cd |

Derived units are combinations of base units: velocity m/s, acceleration
m/s^2, force kg*m/s^2 (= newton, N), pressure N/m^2 (= pascal, Pa), energy
N*m (= joule, J), power J/s (= watt, W), frequency 1/s (= hertz, Hz),
charge A*s (= coulomb, C), potential V = J/C, resistance ohm = V/A,
capacitance farad = C/V, magnetic flux density tesla = kg/(s^2*A),
magnetic flux weber = T*m^2, inductance henry = Wb/A.

## 2. Dimensional homogeneity

A physically meaningful equation that equates quantities must be
*dimensionally homogeneous*: both sides must carry the same dimensions.
Multiplying quantities multiplies their dimensions; dividing divides them;
raising a quantity to a power raises its dimensions to that power (including
fractional powers); adding or subtracting quantities is only meaningful
between like dimensions.

Examples of homogeneous identities:

- force equals mass times acceleration: F = m * a
- work equals force times displacement: W = F * d
- power equals work per time: P = W / t
- kinetic energy: E = 0.5 * m * v^2
- gravitational force: F = G * m1 * m2 / r^2
- mass-energy: E = m * c^2
- wave speed: v = f * lambda
- photon energy: E = h * f
- de Broglie wavelength: lambda = h / p
- heat added: Q = c * m * delta_T
- blackbody radiated power: P = sigma * A * T^4
- Wien displacement: lambda_max * T = b

## 3. Checking an identity

To check a proposed identity: write each symbol's unit as a product of SI
base units (with powers), expand both sides of the equation using the algebra
of dimensions, and compare. If the two dimension expressions agree, the
identity is dimensionally consistent and is a candidate law statement; if
they disagree, the identity as written is wrong.

Constant coefficients of the relations above (G, c, h, sigma, b, R) each
carry their own fixed units, e.g. G in m^3/(kg*s^2), h in J*s,
sigma in W/(m^2*K^4), b in m*K, R in J/(mol*K).

## 4. Scope note

This excerpt restricts itself to *equation identities* (always-true
relations among quantities). Conditional decision rules (if-then forms) are
out of scope here and belong to a different entry family.
