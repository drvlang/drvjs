# Optimizing drvjs for CI

Across all seven CIBench CI cases, replacing the `nix-instantiate` planner with a direct Nix daemon ATerm planner, making the pinned worker binary input-addressed, eliminating heavy Rust/C dependency subtrees, and switching compression and SHA-2 hashing to system `libz`/`libzstd`/`libcrypto` reduced cold builds from 338.90 s to 17.91 s (18.92× faster; 203 → 18 built derivations), remote cache restores from 18.90 s / 805.7 MiB to 2.49 s / 91.5 MiB (7.58× faster, 88.6% less cache traffic, 416 → 89 paths), local warm checks from 1.99 s to 0.93 s (2.14× faster; faster than Nixpkgs `fetchPnpmDeps`), remote edits from 39.64 s to 2.56 s (15.48× faster; 2.01× faster than `fetchPnpmDeps`), local edits from 18.71 s to 1.39 s (13.49× faster; 2.53× faster than `fetchPnpmDeps`), remote dependency updates from 39.90 s to 2.55 s (15.63× faster; 3.36× faster than `fetchPnpmDeps`), and local dependency updates from 19.17 s to 1.46 s (13.16× faster; 4.36× faster than `fetchPnpmDeps`).

CIBench installs a pnpm JavaScript project (`number-demo` depending on `is-number`) and checks the output of running `node main.cjs`. The source edit changes `main.cjs` and the expected output. The independent dependency update upgrades `is-number` from `6.0.0` to `7.0.0` in `package.json` and `pnpm-lock.yaml` without any Nix-specific lockfile hashes.

| CIBench case | drvjs `0149344` (s) | drvjs optimized (s) | `fetchPnpmDeps` (s) | drvjs cache transfer (before → after) | drvjs builds (before → after) | drvjs peak RAM (before → after) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| cold | 338.90 | 17.91 | 7.23 | 0.0 → 0.0 MiB | 203 → 18 | 5920 → 3207 MiB |
| remote-warm | 18.90 | 2.49 | 1.23 | 805.7 → 91.5 MiB | 1 → 1 | 4102 → 768 MiB |
| local-warm | 1.99 | 0.93 | 0.97 | 0.0 → 0.0 MiB | 0 → 0 | 521 → 356 MiB |
| edit-remote | 39.64 | 2.56 | 5.15 | 805.7 → 91.5 MiB | 7 → 6 | 4156 → 788 MiB |
| edit-local | 18.71 | 1.39 | 3.50 | 0.0 → 0.0 MiB | 6 → 5 | 588 → 367 MiB |
| dependency-remote | 39.90 | 2.55 | 8.59 | 805.7 → 91.5 MiB | 9 → 7 | 4158 → 783 MiB |
| dependency-local | 19.17 | 1.46 | 6.36 | 0.0 → 0.0 MiB | 8 → 6 | 586 → 366 MiB |

Times are medians of two trials for `0149344`, three trials for the optimized tree, and five trials for the shared Nixpkgs `fetchPnpmDeps` comparator, run in alternating order (`before-1`, `after-1` ×2, `before-2`, `after-2`) with no competing host workloads. Each sample timed evaluation and realization of the package and check together. [Individual timings, build counts, cache traffic, peak memory, and realized outputs](cibench-ci-optimizations-2026-11-27.csv)

## Summary of optimizations

1. **Direct Nix daemon store writer and native ATerm planner (`src/nixstore.rs`, `src/direct_plan.rs`, `nix/project.nix`, `nix/workspace.nix`)**: Replaced `drvkit/dynamic.nix` and `nix-instantiate` inside the dynamic installation planner with a direct protocol-1.21 Nix daemon Unix-socket client (`wopQueryValidPaths`, `wopAddTextToStore`) and native ATerm `Derive(...)` emitter (`drvjs internal-plan`), plus a direct symlink forwarder (`drvjs internal-symlink`). This eliminated `pkgs.nix` (~44 store paths), `nixpkgsSource` (202 MiB), `bash`, `bash-interactive`, `coreutils`, `drvjs-tarballs.json.drv` (replaced by `passAsFile`), the redundant inner forwarding derivation, and ~14 s of Nixpkgs evaluation inside every edit/dependency planner run. `gitMinimal` is also omitted from the planner when `pnpm-lock.yaml` has no `patchedDependencies`.
2. **Input-addressed `drvjs-unwrapped` worker, separate unit-test check, and shared Nixpkgs evaluation (`nix/package.nix`, `flake.nix`)**: Made `drvjs-0.1.0-unwrapped` input-addressed (`doCheck = false`, `auditable = false`) while keeping all drvlang-generated packages, dependency layers, planners, checks, and public forwarding derivations floating content-addressed. Fresh remote CI workers now substitute `drvjs-unwrapped` (~3.3 MiB unpacked) directly instead of realizing `rustc`, `llvm`, `gcc`, `cargo`, and hundreds of unpacked crate derivations (~600+ MiB). Moved `cargo test` into `checks.<system>.drvjs-tests` and switched `flake.nix` from re-importing Nixpkgs to `nixpkgs.legacyPackages.${system}`.
3. **Slashed Rust dependency graph from 250 crates to 28 crates (`Cargo.toml`, `Cargo.lock`, `src/lockfile_v13.rs`, `src/semver.rs`, `src/url.rs`, `src/cli.rs`, `src/temp.rs`, `src/compress.rs`, `src/hash.rs`)**:
   - Replaced the 220-crate `pnpm-lockfile` git dependency, `node-semver`, `url`, `clap`/`clap_derive`, `regex`, `tempfile`, `tar`'s default `xattr`/`rustix` feature, `serde/derive`, `libdeflater`, `flate2`, `zstd`, `sha2`, and `base64` with focused native Rust modules and zero proc-macros.
   - Linked system `libz` (`pkgs.zlib`), `libzstd` (`pkgs.zstd`), and `libcrypto` (`pkgs.openssl`) directly via FFI—all three are already present in the frozen stock cache and in `nodejs-slim`'s runtime closure, adding zero extra download bytes to remote CI jobs while removing `pkgs.pkg-config` and all C/C++ build scripts from cold builds.
   - Configured `[profile.release]` with `opt-level = 0`, `panic = "abort"`, `strip = "symbols"`, and `[profile.release.package."*"] opt-level = 1`, keeping YAML/JSON/tar parsing optimized and compression/SHA-2 in native `-O3`/assembly libraries while cutting worker compilation on 4 E-cores to ~11.8 s.

## Correctness checks

- Realized package and check outputs were deterministic across all three optimized trials and matched between `cold`, `remote-warm`, and `local-warm`, between `edit-remote` and `edit-local`, and between `dependency-remote` and `dependency-local`.
- `cargo test --offline` passed all 58 unit tests with zero warnings (including NIST SHA-256/384/512 vectors, RFC 4648 base64, gzip/multi-member and bulk/streaming zstd roundtrips, ATerm/fixed-output Nix store path parity, semver, URL, lockfile v13, workspace projection, and native assembly tests).
- `nix flake show --all-systems` (`x86_64-linux`, `aarch64-linux`) and `nix flake check -L` (`drvjs`, `bench`, `drvjs-tests`) passed.
- All 12 integration test suites passed with IFD disabled: `tests/lockfiles.py`, `tests/commands.py`, `tests/ci.py`, `tests/incremental.py`, `tests/platforms.py`, `tests/integration.sh`, `tests/native.sh`, `tests/configured.sh`, `tests/registry.py`, `tests/unpacked.sh`, `tests/native-already-applied.sh`, and `tests/native-store-patches.sh`.

## Reproduction

The runner used x86_64 Linux, an Intel Core i7-1280P, Nix 2.35.3, four logical CPUs (16–19), four jobs, four cores per builder, 8 GiB RAM, and zero swap. Sandboxing was enabled and IFD disabled. The simulated project cache used 5 ms latency and 100 MiB/s aggregate bandwidth. Both builders used the same local mirror of signed NixOS packages populated from `drvkit/cibench/results/bootstrap/js.txt`. Cold jobs started with an empty local store and project cache. Subsequent cases independently used the original job's local store or its published project cache. The recorded runner was `/nix/store/vpysz8ih9lhm154cjaamrncwjn08njp6-cibench-0.1.0`.

```sh
work=$(mktemp -d)
stock="file://$work/stock"
nix copy --from https://cache.nixos.org --to "$stock?compression=zstd&compression-level=1" \
  --stdin < ../drvkit/cibench/results/bootstrap/js.txt
runner="$(nix build --impure --no-link --print-out-paths -f ../drvkit/cibench)/bin/cibench"
mkdir "$work/before"
git archive 0149344 | tar -x -C "$work/before"
run() {
  taskset -c 16-19 "$runner" js --builder "$1" --out "$work/$2" --trials "$3" \
    --cpus 4 --memory-mib 8192 --latency-ms 5 --bandwidth-mib-s 100 \
    --upstream "$stock?priority=40"
}
run "$work/before" before-1 1
run "$PWD" after-1 2
run "$work/before" before-2 1
run "$PWD" after-2 1
```
