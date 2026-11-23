# pnpm benchmarks

Run `nix run .#bench -- --help` from the repository root to see the Rust benchmark suite. Each subcommand lists its inputs, trial count, and report options with `--help`.

```sh
nix run .#bench -- lockfiles --baseline /path/to/baseline/drvjs \
  --optimized /path/to/current/drvjs --project /path/to/vue \
  --filter @vue/compiler-sfc --runs 9
```

Available workloads include `cache-compression`, `cache-updates`, `ci-workspace`, `closure`, `compare-native`, `evaluate`, `experiment`, `lockfiles`, `materialize`, `prepare-zstd`, `rebuild`, and `unpacked-store`. Comparisons accept explicit before/after inputs so the compiler, projects, and cache conditions can be held constant. The dated files here contain the measurements and workloads used for earlier releases.
