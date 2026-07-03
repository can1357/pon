#!/usr/bin/env python3.14
"""Generates pon-runtime/src/native/unicodedata_tables.rs from the HOST
CPython 3.14 `unicodedata` module (the differential-suite oracle).

Zero new deps (L4 pin): everything is derived by probing the host module —
no raw UCD files are parsed.  Encodings chosen for compactness (<1MB):

- category / east_asian_width: full-coverage run tables — sorted `STARTS`
  (u32) plus parallel `INDEX` (u8 into a name table); a run extends to the
  next start.  Lookup = partition_point - 1.
- combining: sparse inclusive ranges (start, end, ccc) — 0 elsewhere.
- decimal / digit: sparse runs (start, end, first_value) where the value
  increments by 1 per codepoint (verified).
- numeric: sparse runs (start, end, f64-bits of first value) where the value
  increments by exactly +1.0 per codepoint (verified; fractions become
  singleton runs).
- NFD / NFKD: sorted codepoint arrays + packed (pool_offset << 5 | len)
  slices into a shared u32 pool.  Entries are FULL host normalizations of
  the single codepoint (recursion pre-expanded), so the Rust side never
  recurses; Hangul syllables (U+AC00..U+D7A3) are excluded (algorithmic).
- canonical composition: sorted (starter << 32 | combiner) u64 keys with
  parallel u32 composed values.  Derived from raw first-level canonical
  pairs and validated with the host (`NFC(a+b) == c`), which bakes in the
  Full_Composition_Exclusion set; Hangul targets excluded (algorithmic).

The script re-implements UAX #15 normalization over the generated tables and
differentially checks it against the host for every codepoint and a seeded
random sample of combining sequences before writing the .rs file.
"""

import random
import sys
import unicodedata as u

MAX_CP = 0x110000
HANGUL_S_BASE, HANGUL_S_COUNT = 0xAC00, 11172
OUT = "pon-runtime/src/native/unicodedata/tables.rs"

# --------------------------------------------------------------------------
# Probe the host.

print("probing host unicodedata", u.unidata_version, file=sys.stderr)

category = [u.category(chr(cp)) for cp in range(MAX_CP)]
eaw = [u.east_asian_width(chr(cp)) for cp in range(MAX_CP)]
ccc = [u.combining(chr(cp)) for cp in range(MAX_CP)]

SENTINEL = object()
decimal = {cp: v for cp in range(MAX_CP) if (v := u.decimal(chr(cp), SENTINEL)) is not SENTINEL}
digit = {cp: v for cp in range(MAX_CP) if (v := u.digit(chr(cp), SENTINEL)) is not SENTINEL}
numeric = {cp: v for cp in range(MAX_CP) if (v := u.numeric(chr(cp), SENTINEL)) is not SENTINEL}

def is_hangul_syllable(cp):
    return HANGUL_S_BASE <= cp < HANGUL_S_BASE + HANGUL_S_COUNT

nfd = {}
nfkd = {}
for cp in range(MAX_CP):
    if is_hangul_syllable(cp):
        continue
    ch = chr(cp)
    d = u.normalize("NFD", ch)
    if d != ch:
        nfd[cp] = [ord(c) for c in d]
    kd = u.normalize("NFKD", ch)
    if kd != ch:
        nfkd[cp] = [ord(c) for c in kd]

# Canonical composition pairs: raw first-level canonical (untagged)
# 2-codepoint decompositions whose recomposition the host confirms.
compose = {}
for cp in range(MAX_CP):
    if is_hangul_syllable(cp):
        continue
    raw = u.decomposition(chr(cp))
    if not raw or raw.startswith("<"):
        continue
    parts = raw.split()
    if len(parts) != 2:
        continue
    a, b = int(parts[0], 16), int(parts[1], 16)
    if u.normalize("NFC", chr(a) + chr(b)) == chr(cp):
        assert (a, b) not in compose, hex(cp)
        compose[(a, b)] = cp
print(f"compose pairs: {len(compose)}", file=sys.stderr)

# --------------------------------------------------------------------------
# Reference implementation over the generated data model (validates tables).

def ref_decompose(text, table):
    out = []
    for ch in text:
        cp = ord(ch)
        if is_hangul_syllable(cp):
            s = cp - HANGUL_S_BASE
            out.extend((0x1100 + s // 588, 0x1161 + s % 588 // 28))
            if s % 28:
                out.append(0x11A7 + s % 28)
        elif cp in table:
            out.extend(table[cp])
        else:
            out.append(cp)
    # canonical ordering (stable insertion by ccc over nonzero runs)
    for i in range(1, len(out)):
        c = ccc[out[i]]
        if c == 0:
            continue
        j = i
        while j > 0 and ccc[out[j - 1]] > c:
            out[j - 1], out[j] = out[j], out[j - 1]
            j -= 1
    return out

def ref_compose_pair(a, b):
    if 0x1100 <= a <= 0x1112 and 0x1161 <= b <= 0x1175:
        return HANGUL_S_BASE + ((a - 0x1100) * 21 + (b - 0x1161)) * 28
    if is_hangul_syllable(a) and (a - HANGUL_S_BASE) % 28 == 0 and 0x11A8 <= b <= 0x11C2:
        return a + (b - 0x11A7)
    return compose.get((a, b))

def ref_normalize(form, text):
    buf = ref_decompose(text, nfkd if form in ("NFKC", "NFKD") else nfd)
    if form in ("NFD", "NFKD"):
        return "".join(map(chr, buf))
    out = []
    starter = -1  # index into out of the last starter
    last_ccc = 0
    for cp in buf:
        c = ccc[cp]
        if starter >= 0 and (len(out) == starter + 1 or last_ccc < c):
            composed = ref_compose_pair(out[starter], cp)
            if composed is not None:
                out[starter] = composed
                continue
        if c == 0:
            starter = len(out)
            last_ccc = 0
        else:
            last_ccc = c
        out.append(cp)
    return "".join(map(chr, out))

print("validating reference vs host (full range, all forms)", file=sys.stderr)
for cp in range(MAX_CP):
    ch = chr(cp)
    for form in ("NFC", "NFD", "NFKC", "NFKD"):
        want = u.normalize(form, ch)
        got = ref_normalize(form, ch)
        assert got == want, f"{form} U+{cp:04X}: got {[hex(ord(c)) for c in got]} want {[hex(ord(c)) for c in want]}"

print("validating reference vs host (random sequences)", file=sys.stderr)
rng = random.Random(20260703)
interesting = sorted(set(list(nfd) + list(nfkd) + [b for _, b in compose] + [a for a, _ in compose]
                         + [cp for cp in range(MAX_CP) if ccc[cp]]
                         + list(range(HANGUL_S_BASE, HANGUL_S_BASE + 64))
                         + list(range(0x1100, 0x1176)) + list(range(0x11A7, 0x11C3))))
for _ in range(20000):
    s = "".join(chr(rng.choice(interesting)) for _ in range(rng.randrange(1, 8)))
    for form in ("NFC", "NFD", "NFKC", "NFKD"):
        want = u.normalize(form, s)
        got = ref_normalize(form, s)
        assert got == want, f"{form} {[hex(ord(c)) for c in s]}: got {[hex(ord(c)) for c in got]} want {[hex(ord(c)) for c in want]}"
print("reference implementation matches host", file=sys.stderr)

# --------------------------------------------------------------------------
# Encode tables.

def runs_full(values):
    """Full-coverage run table: (starts, indexes, names)."""
    names = sorted(set(values))
    idx = {n: i for i, n in enumerate(names)}
    starts, indexes = [], []
    prev = None
    for cp, v in enumerate(values):
        if v != prev:
            starts.append(cp)
            indexes.append(idx[v])
            prev = v
    return starts, indexes, names

def runs_incrementing(mapping, cast):
    """Sparse (start, end, first_value) runs with +1-per-cp values."""
    runs = []
    for cp in sorted(mapping):
        v = mapping[cp]
        if runs and runs[-1][1] + 1 == cp and cast(v) == cast(runs[-1][2] + (cp - runs[-1][0])):
            runs[-1] = (runs[-1][0], cp, runs[-1][2])
        else:
            runs.append((cp, cp, v))
    return runs

cat_starts, cat_idx, cat_names = runs_full(category)
eaw_starts, eaw_idx, eaw_names = runs_full(eaw)

ccc_runs = []
for cp, c in enumerate(ccc):
    if c == 0:
        continue
    if ccc_runs and ccc_runs[-1][1] + 1 == cp and ccc_runs[-1][2] == c:
        ccc_runs[-1] = (ccc_runs[-1][0], cp, c)
    else:
        ccc_runs.append((cp, cp, c))

dec_runs = runs_incrementing(decimal, int)
dig_runs = runs_incrementing(digit, int)
num_runs = runs_incrementing(numeric, float)
import struct
num_runs = [(s, e, struct.unpack("<Q", struct.pack("<d", float(v)))[0]) for s, e, v in num_runs]

# Shared decomposition pool with exact-sequence dedup.
pool = []
pool_index = {}
def pool_slice(seq):
    key = tuple(seq)
    if key not in pool_index:
        pool_index[key] = len(pool)
        pool.extend(seq)
    off = pool_index[key]
    assert len(seq) < 32 and off < (1 << 27)
    return (off << 5) | len(seq)

nfd_cps = sorted(nfd)
nfd_slices = [pool_slice(nfd[cp]) for cp in nfd_cps]
nfkd_cps = sorted(nfkd)
nfkd_slices = [pool_slice(nfkd[cp]) for cp in nfkd_cps]

compose_keys = sorted((a << 32) | b for a, b in compose)
compose_vals = [compose[(k >> 32, k & 0xFFFFFFFF)] for k in compose_keys]

# --------------------------------------------------------------------------
# Emit Rust.

def fmt_ints(values, per_line=16):
    lines = []
    for i in range(0, len(values), per_line):
        lines.append("    " + " ".join(f"{v}," for v in values[i:i + per_line]))
    return "\n".join(lines)

def fmt_triples(triples, fmt, per_line=4):
    lines = []
    for i in range(0, len(triples), per_line):
        lines.append("    " + " ".join(fmt.format(*t) for t in triples[i:i + per_line]))
    return "\n".join(lines)

sizes = {
    "category": len(cat_starts) * 5,
    "east_asian_width": len(eaw_starts) * 5,
    "combining": len(ccc_runs) * 9,
    "decimal+digit": (len(dec_runs) + len(dig_runs)) * 9,
    "numeric": len(num_runs) * 16,
    "decomp pool": len(pool) * 4,
    "nfd index": len(nfd_cps) * 8,
    "nfkd index": len(nfkd_cps) * 8,
    "compose": len(compose_keys) * 12,
}
total = sum(sizes.values())

body = f'''//! Generated Unicode {u.unidata_version} tables for native `unicodedata`.
//!
//! DO NOT EDIT: regenerate with `python3.14 scratch/gen_unicodedata_tables.py`
//! (probes the HOST oracle's unicodedata module; see that script for the
//! encodings).  Table payload: {total} bytes ({total / 1024:.0f} KiB).

pub(super) const UNIDATA_VERSION: &str = "{u.unidata_version}";

/// General-category names indexed by `CATEGORY_INDEX` entries.
pub(super) static CATEGORY_NAMES: [&str; {len(cat_names)}] = [{", ".join(f'"{n}"' for n in cat_names)}];

/// Run starts (sorted, full-coverage from U+0000): run i spans
/// `CATEGORY_STARTS[i]..CATEGORY_STARTS[i+1]`.
pub(super) static CATEGORY_STARTS: [u32; {len(cat_starts)}] = [
{fmt_ints(cat_starts)}
];

/// Category of run i, as an index into [`CATEGORY_NAMES`].
pub(super) static CATEGORY_INDEX: [u8; {len(cat_idx)}] = [
{fmt_ints(cat_idx)}
];

/// East-Asian-width names indexed by `EAW_INDEX` entries.
pub(super) static EAW_NAMES: [&str; {len(eaw_names)}] = [{", ".join(f'"{n}"' for n in eaw_names)}];

/// Run starts for `east_asian_width` (same scheme as [`CATEGORY_STARTS`]).
pub(super) static EAW_STARTS: [u32; {len(eaw_starts)}] = [
{fmt_ints(eaw_starts)}
];

/// Width of run i, as an index into [`EAW_NAMES`].
pub(super) static EAW_INDEX: [u8; {len(eaw_idx)}] = [
{fmt_ints(eaw_idx)}
];

/// (start, end inclusive, canonical combining class); 0 for anything absent.
pub(super) static COMBINING_RANGES: [(u32, u32, u8); {len(ccc_runs)}] = [
{fmt_triples(ccc_runs, "({}, {}, {}),")}
];

/// (start, end inclusive, decimal value of `start`); +1 per codepoint.
pub(super) static DECIMAL_RANGES: [(u32, u32, u8); {len(dec_runs)}] = [
{fmt_triples(dec_runs, "({}, {}, {}),")}
];

/// (start, end inclusive, digit value of `start`); +1 per codepoint.
pub(super) static DIGIT_RANGES: [(u32, u32, u8); {len(dig_runs)}] = [
{fmt_triples(dig_runs, "({}, {}, {}),")}
];

/// (start, end inclusive, f64 bits of the numeric value of `start`);
/// +1.0 per codepoint (fractional values are singleton runs).
pub(super) static NUMERIC_RANGES: [(u32, u32, u64); {len(num_runs)}] = [
{fmt_triples(num_runs, "({}, {}, {:#x}),", per_line=3)}
];

/// Shared codepoint pool for the FULL (host-pre-expanded, canonically
/// ordered) NFD/NFKD expansions referenced by the packed slices below.
pub(super) static DECOMP_POOL: [u32; {len(pool)}] = [
{fmt_ints(pool)}
];

/// Codepoints with a canonical (NFD) expansion, sorted.
pub(super) static NFD_CPS: [u32; {len(nfd_cps)}] = [
{fmt_ints(nfd_cps)}
];

/// Parallel to [`NFD_CPS`]: `pool_offset << 5 | len`.
pub(super) static NFD_SLICES: [u32; {len(nfd_slices)}] = [
{fmt_ints(nfd_slices)}
];

/// Codepoints with a compatibility (NFKD) expansion, sorted.
pub(super) static NFKD_CPS: [u32; {len(nfkd_cps)}] = [
{fmt_ints(nfkd_cps)}
];

/// Parallel to [`NFKD_CPS`]: `pool_offset << 5 | len`.
pub(super) static NFKD_SLICES: [u32; {len(nfkd_slices)}] = [
{fmt_ints(nfkd_slices)}
];

/// Canonical composition pairs `(starter << 32) | combiner`, sorted;
/// `Full_Composition_Exclusion` already applied (host-validated), Hangul
/// excluded (algorithmic).
pub(super) static COMPOSE_KEYS: [u64; {len(compose_keys)}] = [
{fmt_ints(compose_keys, per_line=8)}
];

/// Parallel to [`COMPOSE_KEYS`]: the composed codepoint.
pub(super) static COMPOSE_VALUES: [u32; {len(compose_vals)}] = [
{fmt_ints(compose_vals)}
];
'''

with open(OUT, "w") as f:
    f.write(body)

print(f"wrote {OUT}", file=sys.stderr)
for k, v in sizes.items():
    print(f"  {k:18} {v:>8} bytes", file=sys.stderr)
print(f"  {'TOTAL':18} {total:>8} bytes ({total / 1024:.0f} KiB)", file=sys.stderr)
print(f"  runs: cat={len(cat_starts)} eaw={len(eaw_starts)} ccc={len(ccc_runs)} "
      f"dec={len(dec_runs)} dig={len(dig_runs)} num={len(num_runs)} "
      f"nfd={len(nfd_cps)} nfkd={len(nfkd_cps)} pool={len(pool)} compose={len(compose_keys)}",
      file=sys.stderr)
