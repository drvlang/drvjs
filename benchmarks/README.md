# pnpm benchmarks

Run `nix run .#bench -- --help` from the repository root to see the Rust benchmark suite. Each subcommand lists its inputs, trial count, and report options with `--help`.

```sh
nix run .#bench -- lockfiles --baseline /path/to/baseline/drvjs \
  --optimized /path/to/current/drvjs --project /path/to/vue \
  --filter @vue/compiler-sfc --runs 9
```

Available workloads include `cache-compression`, `cache-updates`, `ci-workspace`, `closure`, `compare-native`, `evaluate`, `experiment`, `lockfiles`, `materialize`, `prepare-zstd`, `rebuild`, and `unpacked-store`. Comparisons accept explicit before/after inputs so the compiler, projects, and cache conditions can be held constant. The dated files here contain the measurements and workloads used for earlier releases.

## Source edits in Vue CI

The `dynamic-dependencies` benchmark edits `packages/compiler-sfc/src/index.ts` in a temporary checkout of [Vue 3.5.22](https://github.com/vuejs/core/tree/5a8aa0b2ba575e098cbb63b396e9bcb751eb3a0f). It alternates revisions on the same edit with warm tools and dependencies, strict sandboxing, substitutions disabled, and one excluded warmup.

| Consumer workload | Trials | drvjs `5191e37` median | Source projection median |
| --- | ---: | ---: | ---: |
| Dependency layer for `@vue/compiler-sfc` | 7 | 4.19 s | 1.82 s |
| Full workspace package and 28 parser tests together | 5 | 36.14 s | 14.29 s |

All dependency outputs resolved to the same store path. On an additional source edit, the baseline ran its recursive installation planner. The new implementation ran the Rust source projection and reused the existing planner and dependency assembly. The full-workspace run checked that each installed package contained the edited source and that the upstream parser suite succeeded. The full-workspace trials ranged from 12.53–54.93 s before and 10.02–34.37 s after on a shared x86_64-linux host running Nix 2.35.3.

```sh
nix run .#bench -- dynamic-dependencies --before ../drvjs-before --after . \
  --project ../vue --filter @vue/compiler-sfc \
  --edit-file packages/compiler-sfc/src/index.ts --runs 7
```

For the package-and-check comparison, set the test script in the disposable Vue checkout to `vitest run --project unit --maxWorkers 2 packages/compiler-sfc/__tests__/parse.spec.ts`. Run the same command with `--package-and-test --runs 5` and omit the workspace filter so Vitest has its root configuration and development dependencies.
