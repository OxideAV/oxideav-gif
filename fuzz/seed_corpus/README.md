# Seed Corpus

Spec-derived seed files for the `oxideav-gif` cargo-fuzz targets.

`fuzz/corpus/` is gitignored (per `.gitignore`) because it is the
local fuzz flywheel — every interesting input the fuzzer discovers
gets added there during a run, and pinning that whole tree in git
would balloon repo size. The seeds in this directory are different:
they are a small, hand-derived, audit-grade set of inputs that
exercise the spec's classic problem areas, and they are tracked so
every contributor (and CI) can bootstrap a fresh corpus from a known
baseline.

## Layout

One subdirectory per fuzz target. Each subdirectory mirrors the
`fuzz/corpus/<target>/` layout, so re-seeding is a flat copy:

```
fuzz/seed_corpus/decode/...
fuzz/seed_corpus/decode_panic_free/...
fuzz/seed_corpus/decode_lenient_panic_free/...
```

## Bootstrapping `fuzz/corpus/<target>/`

Run once per target before invoking `cargo fuzz run`:

```
cp -n fuzz/seed_corpus/<target>/* fuzz/corpus/<target>/
cargo fuzz run <target>
```

`-n` is `--no-clobber`: it never overwrites an existing fuzz-flywheel
input, so re-seeding is idempotent and never blows away the local
flywheel's accumulated corpus.

## Seed inventory

Every file is content-addressed by SHA-1 of its bytes (the libFuzzer
default naming scheme), so the on-disk name is meaningless to a
human. The labels below correspond to the bytes-for-bytes payloads
emitted by `tools/seedgen.py` (run from the crate root); see that
script for the spec section each blob walks.

| Label                                | Bytes | Spec sections walked                        |
|--------------------------------------|-------|---------------------------------------------|
| `spec_1x1_gif87a_minimal`            | 35    | §17 / §18 / §19 / §20 / §22 / §15 / §27     |
| `spec_2x2_gif89a_with_gce`           | 50    | §17 / §18 / §19 / §23 / §20 / §22 / §15 / §27 |
| `malformed_truncated_trailer`        | 34    | §27 truncation — EOF on trailer state machine |
| `malformed_oversize_lzw_min_code`    | 35    | §22.c.i Appendix F width clamp (illegal min=12) |
| `malformed_app_ext_subblock_overrun` | 54    | §26 / §15 — App Ext sub-block claims 0x42 B, has 3 |

The two well-formed fixtures are byte-for-byte copies of the test
arrays in `tests/spec_fixtures.rs`, derived by walking each field's
syntax diagram in the GIF89a specification (no external decoder
consulted). The three malformed fixtures perturb a single field of
the 1×1 GIF87a or wrap a §26 Application Extension around it.

## Verification (round 153)

Each well-formed seed decodes via both `decode()` and
`decode_lenient()`. Each malformed seed surfaces `Err(_)` on
`decode()` (no panic) and a recovered `Ok(_)` on `decode_lenient()`
(no panic) — exactly the contract the `decode_panic_free` and
`decode_lenient_panic_free` fuzz targets assert.

## Regenerating

```
python3 tools/seedgen.py
```

run from the crate root. The script is pure-Python with no external
deps; the seeds are produced by literal spec walks, not by invoking
any GIF library.
