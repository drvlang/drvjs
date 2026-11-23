# drvjs reference

Nix determines the content-addressed store path of each installed project, dependency layer, and prepared package when it builds that output. Use `nix build --print-out-paths --no-link` to obtain the realized path. Registry downloads keep the integrity hashes from pnpm.

## Standalone exports

Run directly from GitHub with `nix run github:drvlang/drvjs -- --help`, or
use the commands below from a checkout.

```sh
# Whole project or workspace
nix run . -- export -C /path/to/project -o /tmp/project-deps
nix-build /tmp/project-deps -o result

# One workspace package, including its workspace and registry dependencies
nix run . -- export -C /path/to/vue --filter @vue/compiler-sfc -o /tmp/vue-deps
nix-build /tmp/vue-deps -o result

# Reuse package extraction and patching across CI builds and workspace subsets
nix run . -- export -C /path/to/project --filter my-app --cache-packages -o /tmp/ci-deps
nix-build /tmp/ci-deps -o result

# Select a union; explicitly add root tools when needed
nix run . -- export -C /path/to/vue \
  --filter packages/compiler-sfc --filter . -o /tmp/vue-with-tools
```

Filters match exact package names or root-relative importer paths. Repeat
`--filter` to select multiple packages. Dependency closure is always included;
`--filter '@vue/compiler-sfc...'` is also accepted. Other pnpm filter syntax,
globs, dependents, and exclusions are not implemented. A missing or ambiguous
selector is an error. Omitting filters selects the whole workspace.

Closure includes dependencies, devDependencies and optionalDependencies of
selected workspaces, plus registry snapshot dependencies (including peer
contexts), optional dependencies, and workspace links. Cycles are supported.
Selecting a child does **not** implicitly install root development tools; add
`--filter .` to request them.

The output contains the selected workspace sources/manifests at their original
relative paths, root and per-workspace `node_modules`, a projected frozen
lockfile, and workspace configuration. The build-time `.pnpm-store` is discarded
by default; installed package files remain usable without it.
Relative workspace links point to sources inside the immutable output. A
synthetic root with no dependencies is used when the root is not selected;
root pnpm settings, resolutions, and engine requirements are retained.
Selected workspace source changes invalidate assembly; excluded workspace
sources and excluded lockfile/catalog entries do not. The project root's sources
are included only when the root is selected. `.git`, existing `node_modules`,
and `.pnpm-store` are excluded from source snapshots.

Use the output tree in a downstream Nix build. Simply moving one workspace's
`node_modules` out of that tree can break its relative workspace links. Lifecycle
scripts and workspace compilation are not run, so TypeScript workspaces may
need a subsequent build before they can be imported as JavaScript.

Exports include the Rust helper's source, Cargo.lock, the projected project,
and pinned Nix expressions. They do not reference the original checkout. No
IFD, dynamic derivations, or recursive Nix are required.

For source-based flakes, `drvjs.lib.projectSource` takes a project root and an explicit list of input files and directories. Include the lockfile, manifests, workspace configuration, selected packages and any assets the scripts read. The source-based builder and CLI require dynamic and recursive derivations.

## Nix flake distribution

Versioned releases are published to
[FlakeHub](https://flakehub.com/flake/ryantm/drvjs) and
[Flakestry](https://flakestry.dev/flake/github/ryantm/drvjs).
To consume a release as an input:

```nix
inputs.drvjs.url = "https://flakehub.com/f/ryantm/drvjs/0.1.0.tar.gz";
# GitHub is also supported:
# inputs.drvjs.url = "github:drvlang/drvjs/v0.1.0";
```

Use `drvjs.packages.${system}.default` or `drvjs.packages.${system}.drvjs`.
Outputs are declared for `x86_64-linux` and `aarch64-linux`; ARM64 has not yet
been validated by the integration suite.

Maintainers publish a new version by pushing a `v*.*.*` tag. The
[publication workflow](.github/workflows/publish.yml) checks the flake before
publishing to both registries. It can also retry an existing tag through
`workflow_dispatch`.

## Native assembly

Native Rust assembly is the only supported installation path. It reads the projected lockfile, verifies
and decompresses Nix-fetched tarballs, applies locked patches with `git apply`,
and creates the isolated package layout, workspace links, hoists, executable
shims, and pnpm metadata. **Assembly does not launch pnpm.** Lifecycle scripts
remain disabled; a downstream build must produce any required generated files.

```sh
drvjs export -C /path/to/project -o /tmp/native-deps
nix-build /tmp/native-deps -o result

# Independent compatibility check, only when requested:
nix-build /tmp/native-deps --arg keepVerificationStore true -o result-check
drvjs verify result-check
```

`--arg unpackedStore /nix/store/…/.pnpm-store` also works: drvjs reads and
integrity-checks cached content without modifying the supplied store. Nix makes
the completed output immutable. Retaining the verification store is optional
and off by default; pnpm writes only to the temporary copy created by `verify`.
Dependency changes require exporting/building a new tree, rather than running
`pnpm add` or `pnpm install` inside the immutable result.

You can verify an output that has no retained store using a compatible existing
store; verification copies that store to its temporary directory:

```sh
drvjs verify result --store /nix/store/…/.pnpm-store
```

Native assembly runs independent package work in a bounded worker pool. It uses
up to four workers by default, bounded by `NIX_BUILD_CORES` when set or
available parallelism otherwise; `DRVJS_JOBS` changes the requested worker count
within that budget. Cached files are integrity-checked in
the same pass that copies them. With tarball inputs and no retained store,
verified archives extract directly into package directories; peer instances
reuse hardlinks. Patches use independent copies, and changing a bin's executable
permissions first detaches its file from shared content.

This implements pnpm 10.34.5's isolated layout on Linux and consumes its locked
dependency graph; drvjs is not a dependency resolver.
Hooks are checksum-checked rather than executed, and pnpm remains the authority
for generating/updating lockfiles. Native frozen-configuration validation is
not yet equivalent to every pnpm check (notably package-extension configuration
checksums and engine requirements). Use independent verification before adopting
it for a new project. pnpm is used only by the optional independent verifier.

The former `--native` CLI flag and `nativeAssembler` Nix argument have been
removed. Re-export old graphs to use the current implementation; old exports
retain their bundled helper and behavior.

Root `allowedDeprecatedVersions` and `neverBuiltDependencies` are accepted and
preserved: native assembly emits no deprecation warnings and runs no lifecycle
scripts. Frozen validation follows pnpm's dependency-section precedence:
`optionalDependencies` overrides `dependencies`, which overrides
`devDependencies`. Shadowed declarations need no separate importer entry; the
winning declaration must still match the lockfile. Focused regressions for these
behaviors and store reuse are tested against pnpm 10.6.1 as well as the pinned
version; native output metadata and independent verification still target the
pinned pnpm version.

`tests/native.sh` covers tarballs, retained caches, patches, independent offline
verification, and Nix reproducibility. `tests/ci.py` checks the sandbox inputs
exclude pnpm and Node, built-in fetches, and optional iets compatibility.

Native output passed independent pnpm verification for the public Vue compiler-sfc
subset (97 package instances, 6 importers), including a larger selection with
root CI tools (611 package instances).

The current native path uses libdeflate for gzip archives up to 256 MiB expanded
per worker, with streaming fallback for larger or unusual streams. It traverses
memory-backed tar data with seeks and writes borrowed slices, retaining the tar
parser's support for extended paths. Direct outputs avoid unused per-file store
metadata and reuse parsed manifests. SRI checks precede extraction; retained
verification stores still contain complete pnpm metadata.

For optional packages, assembly fills missing `os`, `cpu`, and `libc` constraints
from integrity-checked package manifests, then recomputes dependency reachability.
This handles older lockfiles that omit `libc`, avoiding unused musl binaries on
glibc Linux. Required packages and existing lockfile constraints are preserved;
explicit `supportedArchitectures` selections remain effective. The output's
lockfile records the added constraints so independent offline pnpm verification
uses the same selection. The exported inputs and original checkout are unchanged.
`tests/platforms.py` covers all assembly modes, optional descendants, shared and
required packages, peer contexts, explicit architectures, integrity checks, and
pnpm parity.

### Installed dependency cache for CI

Use `--cache-dependencies` when many source revisions share the same dependency
graph:

```sh
drvjs export -C /path/to/project --filter my-app --cache-dependencies -o /tmp/ci-deps
nix-build /tmp/ci-deps -o result
# Or enable it for an existing current-version export:
nix-build /tmp/ci-deps --arg cacheDependencies true -o result
```

The result remains a complete workspace with normal pnpm links, package files,
source files, and runnable bins. An intermediate `dependencies` derivation keeps
only the inputs assembly reads: projected manifests and lockfile, configuration,
hooks, patches, and existing workspace bin files. Other source files and new
source directories do not change its identity. Manifest, configuration,
dependency, patch, or bin changes rebuild the layer.

Final attachment validates those inputs, copies the installed packages alongside
the selected sources, and relocates generated bin shims and metadata to the new
root. Relative workspace and peer links retain their meaning. No package files
are stripped, and the final output does not retain the layer in its runtime
closure. Independent verification checks the attached workspace against pnpm.

For a CI job that runs workspace tests or builds bundles in a writable
directory, build the layer for the current export and materialize the
workspace in one copy.

```sh
drvjs export -C ./checkout --filter my-app --cache-dependencies -o ./export
nix-build ./export -A dependencies -o ./dependency-cache
drvjs materialize ./dependency-cache --project ./export/project -o ./work
```

Run the job in `./work`. Source-only revisions reuse the same layer. This path
skips building and registering a separate workspace output in the Nix store.
`--project` takes the projected sources from the export. Single-package
exports without a workspace configuration include only installation inputs.
The command checks the dependency inputs against the layer, restores the
installed lockfile and workspace bin modes, and expands shared packages into
writable local copies.
The destination must be a new directory outside the inputs.

The [Vue v3.5.22 benchmark](benchmarks/ci-workspace-2026-11-19.json) times a
source edit from export through workspace preparation and, for the full
workspace, two compiler-sfc Vitest files. The baseline at `c4b9f65` built
the cached-dependency workspace output and ran `drvjs materialize` on it.
Both paths used four build cores on an x86_64-linux i7-1280P with the tools,
archives and dependency layers already realized.

| Work per source revision | Before, median | With `--project`, median |
|---|---:|---:|
| Prepare compiler-sfc plus root tools (611 package instances) | 2.90 s | 0.51 s |
| Prepare the same selection with Nix substitution disabled | 1.42 s | 0.50 s |
| Prepare the full workspace (631 instances) and run 50 tests | 4.31 s | 1.80 s |

Each measurement used nine alternating source edits, or five for the
substitution-disabled case. The full job spent 1.27 s in Vitest on both
versions. The filtered tree passed offline pnpm verification. The full
workspace reproduced the existing `@vitejs/plugin-vue` link and shim
differences against pnpm on both versions.

```sh
nix run .#bench -- ci-workspace --baseline /path/to/old/drvjs --optimized drvjs \
  --project /path/to/vue --edit-file packages/compiler-sfc/src/index.ts --runs 9 \
  --command node node_modules/vitest/vitest.mjs run --project unit --maxWorkers 4 \
    packages/compiler-sfc/__tests__/parse.spec.ts packages/compiler-sfc/__tests__/compileTemplate.spec.ts
```

Keep or publish the layer itself so subsequent CI workers can substitute it:

```sh
nix-build /tmp/ci-deps -A dependencies -o dependency-cache
# Example local binary cache; use your CI cache's destination instead:
nix copy --to 'file:///tmp/drvjs-cache?compression=zstd&compression-level=3' ./dependency-cache
```

Caching only `result` does not retain this build input. The intermediate layer is
an installation cache; its workspace source files are intentionally incomplete.
Use the normal final output to run the application or perform independent
verification, especially when hooks load additional source files.

This option trades an extra stored installation and first-time attachment for
less work on source updates and fewer, smaller compressed build inputs. The
uncompressed input closure grows; the final output size stays the same. It is
opt-in because initial builds and small projects may not benefit. It can be
combined with `--cache-packages` to reuse extraction after dependency changes;
retained verification stores, zstd archives, and external stores also work.

### Shared registry graph for repeated transfers

`--shared-dependencies` changes the output layout: immutable registry package
instances live in a separate Nix output, and the workspace links to them.
Source-only changes rebuild and transfer the workspace; they reuse the registry
graph. Nix tracks that graph as a runtime reference, so copying or rooting the
result also includes it. This is opt-in and implies the installed dependency
cache.

```sh
drvjs export -C /path/to/project --shared-dependencies -o /tmp/shared-deps
nix-build /tmp/shared-deps -o result
# Equivalent for a current export:
nix-build /tmp/shared-deps --arg sharedDependencies true -o result
# Full closure, including the shared graph on the first publication:
nix copy --to 'file:///tmp/drvjs-cache?compression=zstd&compression-level=3' ./result
```

Registry dependencies, peer contexts, aliases, registry hoists and executable
shims stay together. Workspace dependencies still link to the current output's
complete sources. Package files are neither stripped nor compressed in place.
The first transfer includes both outputs; later source updates reuse the graph.
A lockfile, manifest, patch, configuration or workspace-bin change invalidates
it. `--cache-packages` can additionally reuse extraction across those changes.
The graph is shared as a whole, not independently per package version.

**Runtime boundary:** Node resolves registry modules to paths in the shared
output, with shared module identity across outputs using the same graph.
Registry packages cannot implicitly import workspace packages through
ancestor hoists or assume that the application lives above their own directory.
Explicit registry-to-workspace links are rejected. Normal workspace-to-workspace
and workspace-to-registry imports work without a loader or Node flags. Tools
which write into installed dependencies, depend on their exact physical paths,
or need those implicit workspace lookups should use the standalone layout.

Expand either layout into a standalone, writable directory when needed:

```sh
drvjs materialize result -o ./build-workspace
```

This copies registry files, restores relative package links and relocates bin
shims and metadata; it leaves the immutable outputs unchanged. It also restores
the ordinary pnpm lookup layout. `drvjs verify` expands shared outputs before
its full comparison with an independent offline pnpm installation. That checks
installed contents, modes and declared links; application runtime tests remain
necessary for the physical-layout differences above.
The destination must be a new directory outside the inputs. Paths containing
quotes, backslashes, dollar signs, backticks, newlines or colons are rejected
because generated bin shims cannot safely represent them.

The `dependencies` build output contains only attachment metadata and links to
`dependencies.packages`. Cache both build metadata and runtime packages for
workers that will rebuild source revisions:

```sh
nix-build /tmp/shared-deps -A dependencies -o dependency-cache
nix copy --to 'file:///tmp/drvjs-cache?compression=zstd&compression-level=3' ./dependency-cache
```

### Package contents cache for CI

`drvjs export --cache-packages` opts into package preparation in separate Nix
derivations. You can also enable it for an export with
`nix-build EXPORT --arg cachePackages true`. Unchanged package outputs are reused
across dependency updates and independently exported workspace selections:

- A transitive dependency update prepares the new dependency. Its parent's
  unchanged archive contents remain cached even when its dependency edges change.
- Workspace source edits reuse every package output.
- A patch edit rebuilds that patched variant, reusing the unpatched extraction.
- Peer instances share the same archive extraction; their dependency links are
  still assembled in the appropriate peer context.

Preparation verifies the locked tarball digest and patch checksum. Assembly reads
the resulting immutable Nix inputs, checks their package/integrity/patch identity,
and copies real files into the normal pnpm layout. It does not rehash or decompress
those contents. Dependency links, hoists, binaries, frozen validation, and metadata
are rebuilt for the selected graph. No package lifecycle scripts are executed.

Use `nix-build EXPORT -A contentsManifest -o package-cache` to realize and retain
the cache without assembling a workspace. The manifest references all package
outputs; copy its closure to your CI binary cache to preserve them between jobs.
`packageContents` exposes the individual derivations, keyed by package ID (and
patch hash for patched variants). These are build inputs; the final copied tree
does not retain their store paths in its runtime closure. Keeping only the final
result in a binary cache does not preserve preparation outputs for future
dependency updates.

This option trades one-time preparation, extra store space, and Nix graph overhead
for less repeated extraction and patching. It is off by default: small graphs can
be faster with direct archive assembly. Final assembly still copies package files
and reconstructs links, so this does not eliminate all work after a dependency
change. `--arg cachePackages false` restores direct assembly. Retained verification
stores, `unpackedStore`, and the explicit `zstdArchives` option use their existing
assembly paths even when package caching is enabled.

The internal `--package-contents` assembler input is trusted Nix-generated cache
metadata, not a general-purpose untrusted directory cache. Supplied pnpm stores
continue to receive per-file integrity checks.

Before the manifest-constraint and direct-builder improvements below, seven
alternating warm `nix-build --check` runs on the pinned Vue checkout gave these
medians on x86_64 Linux, with the same helper and source in both modes:

| Selection | Package inputs | Direct assembly | Package cache |
|---|---:|---:|---:|
| Compiler-sfc subset | 98 | 0.508 s | 0.609 s |
| Compiler-sfc plus root CI tools (6 of 18 importers) | 615 | 1.625 s | 1.548 s |
| Full workspace | 635 | 1.936 s | 1.801 s |

The CI subset's assembler-only median fell from 0.296 s to 0.183 s; Nix overhead
reduces that to about a 5% end-to-end improvement. Initial cache preparation and
downloads are excluded. The small subset regression is why caching is opt-in.
These are local measurements, not a prediction for a particular CI runner.
[Samples and methodology](benchmarks/incremental-2026-11-13.json) are recorded;
`nix run .#bench -- rebuild` reproduces the alternating rebuild comparison.
`tests/incremental.py` separately checks invalidation after dependency, patch,
and source changes, reuse across selections, target pruning, and cache closures.

### Optional zstd archive cache

For repeated native reconstruction, Nix can normalize downloaded gzip tarballs
into separately cached zstd archives:

```sh
drvjs export --zstd-archives -C ./project -o ./export
nix-build ./export -o result
```

The Nix argument `--arg zstdArchives true` also enables this for an existing export.
`nix-build ./export -A archiveSources --no-out-link` realizes just its archive
manifest and package cache. The original download is checked by `fetchurl`;
normalization runs in Nix, and assembly checks the normalized payload's SHA-512
before decoding. Prepared manifests are trusted cache metadata, like supplied
pnpm stores. Retained verification stores keep the original pnpm integrity keys.
This option supports both retained stores and external-store verification.

Normalization adds a one-time preparation step and can change the cache size.
Whether it improves repeated reconstruction depends on the packages, CPU and
filesystem. Measure cache preparation separately from assembly. The
[cache preparation prototype](benchmarks/README.md) and
[alternating experiment harness](benchmarks/README.md) support comparisons
using your own exported project and already-fetched inputs.

## How it works

1. Flake workspace discovery reads importer paths from the project document.
   Selection and export restore compact v13 peer snapshots, match selectors,
   and walk importer links and package snapshots. The projected lockfile retains
   only reachable importers, package resolutions, peer-context snapshots, and
   used catalog entries. Version specifiers,
   integrity hashes and selected dependency edges are preserved. Declared patches
   are pruned with the selection; root settings and the pnpm hook are retained.
2. Nix fetches retained tarballs reachable under the locked platform constraints
   and verifies their SRI hashes. Unrelated workspace packages are absent from
   the derivation graph.
3. The native assembler validates the projected manifests and lockfile, checks
   every selected archive's SRI hash, and extracts supported packages directly
   into the isolated virtual store. Peer instances reuse hardlinks. With
   `--cache-packages`, verification/extraction and patching happen in reusable
   package derivations before final assembly.
4. Locked patches are checksum-checked and applied with Git. Native assembly
   then creates dependency/workspace links, hoists, executable shims, and pnpm
   metadata. No registry server, Node process, or pnpm process is started.
5. Frozen manifest/configuration checks run on every assembly. Archive integrity
   checks run during extraction (cached by Nix with `--cache-packages`).
   Retaining a verification store is optional; `drvjs verify` performs
   independent offline pnpm reconstruction and compares the installed tree.

An unfiltered single-package v9 export can preserve the original lockfile bytes.
Newer formats are projected to v9. drvjs creates the fetch plan directly from
the in-memory graph. For a URL-only resolution it fetches the tarball during
export and records SHA-512 in the exported lockfile.
Review and retain that export to pin the bytes of URLs that have no upstream
integrity hash. Workspace exports deliberately contain a **projection**, not a
byte-identical copy of the complete original lockfile. The assembled output's
lockfile can additionally record missing optional platform constraints learned
from package manifests. The original checkout is never edited.

## Optional offline verification

Retaining the verification store is **opt-in** because it keeps another copy of
package contents. It retains the contents of all initially selected packages,
including those later excluded by manifest platform constraints. This lets it
seed the original export or a selection requesting additional architectures when
their packages are available in the store. To enable it when exporting:

```sh
drvjs export -C /path/to/project -o /tmp/project-deps --keep-verification-store
nix-build /tmp/project-deps -o result
drvjs verify ./result
```

Or override the choice for an existing export without exporting again:

```sh
nix-build /tmp/project-deps --arg keepVerificationStore true -o result-with-store
drvjs verify ./result-with-store
```

`--arg keepVerificationStore false` produces the smaller output even if the
export opted in. Both variants reuse the same Nix tarball fetches. Default builds still verify fetched tarball integrity and frozen manifests.
The independent pnpm installed-file scan runs only through `drvjs verify`. `verify` on a default output explains how to rebuild with the cache.
The optional cache is retained inside that output, not a separate runtime dependency.

`drvjs verify RESULT` copies sources and package-store contents to a temporary
writable directory, reconstructs node_modules offline with pnpm, checks the
store, and compares every root and workspace installation. Comparison includes
file hashes, executable flags, symlink targets and pnpm metadata; only timestamps,
store location, and generated root paths are normalized. The original result
is never modified. This detects installed-file changes that `pnpm store status`
alone would not detect. pnpm 10's `store status` compares installed files with
the unpatched tarball index and reports intentional patches as mutations. drvjs
runs that command against a temporary view excluding patched snapshots; its
offline reconstruction comparison still covers **every patched package**.

## Registries, hooks, and patches

Root `.npmrc` registry mappings and supported install settings are preserved.
Authentication settings are stripped from exports. For private registries,
preload the integrity-checked tarballs into Nix on the host:

```sh
drvjs export -C /path/to/project --filter pkg/app -o /tmp/app-deps
drvjs fetch /tmp/app-deps --npmrc ~/.npmrc
nix-build /tmp/app-deps -o result
```

`fetch` defaults to custom registries; `--all` includes npmjs. Credentials default
to `~/.npmrc`, supporting scoped `:_authToken` and `:_auth`, including `${VAR}`
values. Credentials go to curl through stdin, never into Nix expressions, process
arguments, or store files. Each redirect is checked against credential scopes.
Already imported tarballs are reused without a download. Binary caches containing
private package bytes need the same access restrictions as the packages. For
URL-only packages requiring auth during export, also pass `export --npmrc PATH`.

Host prefetch computes all selected store paths in one Nix evaluation and checks
their validity in one query. It downloads each missing store path only once,
using up to four workers by default. `DRVJS_JOBS` requests a different worker count,
bounded by `NIX_BUILD_CORES` when set or available parallelism otherwise. Every
download still passes SRI verification before import, and imported paths must
match the original fixed-output paths. This changes `drvjs fetch`, not Nix's own
fetcher or the assembly layout. Current exports use the same target reachability
as Nix assembly, including explicitly requested `supportedArchitectures`. The
default target is the current Linux host; use `--system aarch64-linux` to prepare
another declared target or `--all-platforms` to populate every platform's cache.
`--all` still controls public versus private registries independently. Old exports
without target metadata retain their original all-platform behavior.

`benchmarks/fetch.mjs` compares two binaries on identical exported package plans:

```sh
node benchmarks/fetch.mjs \
  --before /path/to/before/drvjs --after /path/to/after/drvjs \
  --before-export /tmp/export-before --after-export /tmp/export-after \
  --runs 5 --cold-runs 3 --out /tmp/fetch-results
```

The harness prepares both helpers before timing. It alternates warm-cache host
fetches and ordinary source-change Nix rebuilds, recording each step and the
combined workflow. Cold-fetch trials use separate empty local Nix stores without
deleting the user's cache. Results include every command and timing; cold-fetch
timings include downloads and import but not assembly. Export, tool compilation,
application builds, and remote binary-cache transfers are not timed.

On full Vue v3.5.22 (741 archives, 635 installed package instances, 18 importers),
the [2026-11-13 measurements](benchmarks/fetch-vue-2026-11-13.json), taken before
the platform filtering and direct-builder improvements below, found:

| Measurement | Before | Batched prefetch | Trials per version |
| --- | ---: | ---: | ---: |
| Host prefetch, all archives cached | 18.393 s | 0.078 s | 5 |
| Host prefetch into an empty local Nix store | 113.357 s | 24.890 s | 3 |
| Cached prefetch plus ordinary source-change Nix rebuild | 23.242 s | 4.700 s | 5 |

These are medians on four x86-64 vCPUs with 12 GiB RAM. Every cold trial imported
all 741 archives. Cached prefetch replaces 1,482 Nix CLI calls with two; missing
archives download and import through the bounded worker pool. Bare `nix-build`
and assembly are unchanged. The existing full-Vue workspace-link verification
failure remains; these timings do not establish pnpm compatibility for that
workspace.

Overrides (including root `resolutions`), package extensions, peer rules,
architecture settings, hoist settings, and declared patches are retained. Only
patches reachable in a workspace selection are copied. Native assembly applies locked patches and validates the hook checksum.
The root `.pnpmfile.cjs` is copied but runs only during independent pnpm
verification; export and assembly do not execute it.
Hooks must be self-contained or use files included in the selection.
`verify` runs them locally in its temporary reconstruction. Lifecycle scripts remain
disabled. Hook environments are cleared, with `CI=true` and
`PNPM_SKIP_AUTH_CHECK=true`; private-package stub flags are not inherited.

## Real-world validation: Vue

Tested with [Vue v3.5.22](https://github.com/vuejs/core/tree/5a8aa0b2ba575e098cbb63b396e9bcb751eb3a0f),
whose repository declares pnpm 10.17.1. drvjs uses pinned pnpm 10.34.5 with
automatic package-manager switching disabled.

| Selection | Selected importers | Nix tarball fetches on x86_64 Linux | Installed registry package instances |
|---|---:|---:|---:|
| Whole Vue workspace | 18 | 635 | 631 |
| `@vue/compiler-sfc` and dependencies | 5 | 98 | 97 |
| `@vue/reactivity` and dependencies | 2 | 0 | 0 |

Workspace projection retains 108 of 741 locked packages for the compiler subset;
target filtering further reduces that subset to 98 downloads and manifest
constraints reduce it to 97 installed packages. The full workspace needs 635
downloads and installs 631 packages on x86_64 Linux. pnpm also
sees an empty synthetic root, so it reports six projects for the five selected
compiler workspaces. These are dependency-count measurements, not timing claims.

Validation includes actual sandboxed builds of both subsets, fresh offline pnpm
reconstruction, working local links, Babel parser/PostCSS imports, Nix `--check`
rebuilds, stale-manifest rejection, and unchanged derivation identity after
excluded source/manifest/lock/catalog edits. A selected-source edit changes
assembly while preserving all tarball identities. Vue's own compilation and test
suite were not run.

The test is reproducible:

```sh
# In the project development environment, with drvjs on PATH:
bash tests/vue-workspace.sh
# Or reuse an existing checkout at the exact pinned revision:
bash tests/vue-workspace.sh /path/to/vue-v3.5.22
```

## Lockfile planning performance

These are median CLI times on an Intel i7-1280P running Linux. Both release
binaries used the same Vue v3.5.22 checkout with 18 workspace projects and
741 locked packages. The v9 input is Vue's original lockfile. The v12 input
changes its version header. The v13 input uses pnpm's compact writer and
includes the package-manager boot document.

| Format | List workspace paths | Export `@vue/compiler-sfc` | Export whole workspace |
|---|---:|---:|---:|
| 9.0 | 8.1 → 6.1 ms | 15.1 → 12.4 ms | 30.2 → 24.6 ms |
| 12.0 | 8.5 → 6.1 ms | 15.1 → 13.2 ms | 30.5 → 24.8 ms |
| 13.0 | 14.8 → 2.3 ms | 23.2 → 18.7 ms | 41.0 → 31.5 ms |

The benchmark alternates the previous and optimized binaries for 15 runs each
after warm-up. It compares workspace metadata and the bytes of each projected
lockfile and fetch plan. Export timings end when the CLI finishes writing the
export. On pnpm v12.21.0's own v13 lockfile, listing all 237 projects took
39.5 → 5.5 ms and peak memory fell from 23.7 to 7.2 MiB (nine-run medians).
[Benchmark script](benchmarks/README.md) · [Inputs and samples](benchmarks/lockfiles-2026-11-22.json)

## Materialization performance

The native assembler verifies and extracts each archive once, uses hardlinks for
peer instances, and writes pnpm-compatible links and metadata. Tarball integrity
and frozen-manifest checks remain enabled. Nix prefers local assembly to avoid
transferring a large completed tree back from a remote builder; binary-cache
substitution remains available.

Tarball fetches use Nix's built-in fetcher, avoiding a curl/stdenv closure for each
fixed-output derivation. Hashes and output names stay unchanged, so previously
fetched or host-imported tarballs remain reusable. The final derivation invokes
the separate `unwrapped` Rust executable directly, without shell or stdenv setup.
Git is included only when assembly must apply locked patches; Node, pnpm, curl,
and the Nix CLI are absent from the assembly inputs. The default CLI output still provides
those host tools for exporting, fetching, and independent verification. Its
wrapper is a separate derivation, so evaluating or building the assembler does
not require building Node or pnpm.

`nix run .#bench -- materialize` runs alternating before/after comparisons with
existing project and tarball-manifest inputs. Downloads, export and Nix
realization overhead should be measured separately. When comparing against an
older native assembler, pass `--baseline-arg=--native`.

### Shared graph measurements

The [shared-graph measurements](benchmarks/shared-graph-2026-11-14.json) compare
`f38d92c` with the shared layout at `b40474b` on Vue v3.5.22. Both use the
compiler-sfc plus root-tools selection (611 registry instances, six importers). On x86_64-linux
with an i7-1280P and four build cores, seven alternating source updates gave:

| Metric | Direct | Copied dependency cache | Shared graph |
|---|---:|---:|---:|
| Source-update rebuild, median | 3.295 s | 2.984 s | **1.987 s** |
| New compressed NAR payload per update, median | 50.360 MB | 50.360 MB | **1.134 MB** |
| Local cache publication per update, median | 1.126 s | 1.136 s | **0.100 s** |
| First complete compressed runtime closure | 50.358 MB | 50.358 MB | 50.474 MB |
| Initial assembly with tools/archives cached, median of three | 3.331 s | 5.556 s | 4.904 s |

The shared graph reduces repeated NAR payloads by **97.7%** and source-update
build time by **40%** versus direct assembly (33% versus the copied cache).
The workspace output is 4.03 MB uncompressed, with a separately referenced
230.11 MB registry graph, versus a 233.54 MB standalone output. The first
transfer includes both outputs and is nearly unchanged. Initial preparation is
47% slower than direct assembly; this mode benefits repeated source revisions.
Graph changes still require a new complete graph.
Across the initial publication plus seven updates, the total NAR payload was
58.4 MB versus 402.9 MB. Assembly alone fell from 0.394 s to 0.051 s in seven
alternating runs, excluding graph preparation and all Nix overhead.

Every cache publication includes all runtime references. Each mode starts with
an empty local file cache using zstd level 3; subsequent revisions reuse its
existing NARs. These are compressed payload sizes and local publication times,
not remote network timings. Rebuild timing includes evaluation, attachment and
NAR registration; tool compilation and archive downloads are excluded. Neither
package contents nor workspace sources are stripped. Source-update builds keep
both attachment metadata and the registry graph cached; publish `dependencies`
as described above when rebuilding on fresh CI workers.

To compare exported selections with your own graph and cache:

```sh
nix run .#bench -- cache-updates \
  --case direct=/tmp/direct-export --case shared=/tmp/shared-export \
  --runs 7 --out /tmp/transfer-results
nix run .#bench -- rebuild \
  --case direct=/tmp/direct-export --case shared=/tmp/shared-export \
  --fresh --runs 3 --out /tmp/first-build-results
```

Expanded shared trees match all 2,523 / 21,935 / 25,880 entries of the standalone
Vue subset / CI / full-workspace outputs, including file hashes, executable bits,
directories and relative links. The CI selection passes independent offline
pnpm verification. `tests/vue-shared.py` compares real builds of five Vue
compiler packages, checks their byte-identical bundles and compiled SFC runtime,
exercises SWC and Rollup native code, and runs 50 Vue tests with the shared graph.
It accepts `--standalone RESULT --shared RESULT --out NEW-DIRECTORY` for built
full-workspace outputs. These checks do not remove the shared-layout runtime
limits described above.

### Further assembly and transfer improvements

The [2026-11-14 measurements](benchmarks/assembly-transfer-2026-11-14.json)
compare commit `6863c43` with reusable hoist matchers, fewer link syscalls,
largest-first archive scheduling, and a release profile using thin LTO and one
codegen unit. These optimizations are enabled by default. The profile trades
longer helper compilation for a smaller, faster executable. The optional installed
dependency cache adds a separate path for source updates.

On the same pinned Vue checkout and x86_64-linux i7-1280P, seven alternating warm
`nix-build --check` trials per mode produced these medians:

| Selection | Previous direct | Current direct | Cached dependency layer |
|---|---:|---:|---:|
| Compiler-sfc subset | 0.481 s | 0.480 s | 0.452 s |
| Compiler-sfc plus root CI tools | 1.493 s | 1.435 s | 1.234 s |
| Full workspace | 1.817 s | 1.725 s | 1.581 s |

The default path improves the larger selections by about 4–5%; the small selection
is essentially unchanged. Layer reuse improves these warm rebuilds by about 6–17%.
Tools, downloads, and layer preparation are excluded; evaluation, assembly, NAR
checking, and daemon overhead are included. All 63 forced rebuilds passed
reproducibility checks. These are local measurements, not remote-network timings.

Seven ordinary CI source-edit rebuilds measured **2.706 s before, 2.705 s with
the current direct path, and 2.454 s with the layer**: about 9% faster with layer
reuse, and essentially unchanged without it. Assembler-only medians fell from
about **0.292 s to 0.262 s**; attaching an existing layer took about **0.114 s**.
Nix import and registration work limits the end-to-end gain. In three fresh
builds with tools and archives cached, preparing and attaching a new layer took
**4.29 s versus 2.50 s** for direct assembly. Repeated reuse, rather than initial
preparation, is the reason to enable this option.

For the CI subset, the layer reduces the compressed assembly input closure from
**96.2 MB across 624 store paths to 66.7 MB across 9 paths**, at zstd level 3.
The uncompressed input closure grows from about **136.6 MB to 287.2 MB** because
the layer contains extracted files. Final output NAR size stays **233.54 MB**;
the package contents, workspace sources, and link layout are preserved. Keep the
layer in the CI binary cache to obtain the transfer benefit.

Target-aware host prefetch also reduces full-Vue downloads from **741 archives /
432.09 MB to 635 archives / 100.93 MB** on x86_64 Linux, about 77% fewer archive
bytes. Those counts include an empty host cache and preserve every archive
selected by the exported target graph. Tarballs whose missing platform metadata
is learned only during assembly still require an initial fetch.

To reduce transfers of the final output itself, a cache can use stronger
compression without changing installed files. Three alternating publications of
the CI result into fresh local file caches measured:

| zstd level | Compressed output | Publication time |
|---|---:|---:|
| 3 | 50.36 MB | 0.72 s |
| 9 | 42.95 MB | 2.69 s |
| 19 | 36.85 MB | 45.06 s |

Level 9 saves about 15% of the output payload for roughly two additional seconds
of publication work. Level 19 saves about 27% but takes much longer to publish.
This cost is paid when populating the cache; the byte savings apply to subsequent
downloads. Cache policy is unchanged automatically. For a new file cache:

```sh
nix copy --to 'file:///tmp/drvjs-cache?compression=zstd&compression-level=9' ./result
nix run .#bench -- cache-compression ./result --out compression.json
```

`nix run .#bench -- rebuild --case baseline=EXPORT --case direct=EXPORT --case layer=EXPORT`
compares multiple exports; add `--fresh` to include initial layer preparation with
tools and downloads already cached, or `--source-edits` to measure ordinary source
updates in copied exports. `nix run .#bench -- materialize --dependency-layer PATH`
compares direct assembly with attachment outside Nix. The JSON report records
samples, commands, sizes, and validation scope.

### Speed and output size

The [2026-11-13 speed and size measurements](benchmarks/speed-size-2026-11-13.json)
compare the previous direct assembler with manifest-based optional platform
filtering, a direct Rust Nix builder, and bounded executable-header reads. These
improvements are enabled by default; re-export to include the updated helper.
The benchmark uses Vue v3.5.22 on an x86_64-linux Intel i7-1280P:

| Selection | Warm rebuild before → after | Output NAR before → after |
|---|---:|---:|
| Compiler-sfc subset | 0.500 → 0.465 s | 21.69 → 21.18 MB |
| Compiler-sfc plus root CI tools | 1.540 → 1.443 s | 294.63 → 233.54 MB |
| Full workspace | 1.874 → 1.795 s | 421.44 → 360.35 MB |

The CI subset rebuild is about **6% faster**, with a **20.7% smaller output**.
Its zstd level 3 binary-cache payload falls from **69.33 MB to 50.36 MB** (27.4%).
The assembly input closure falls from **170.10 MB to 136.59 MB**, and the final
`.drv` file shrinks from **2,012 to 848 bytes**. All sizes here use decimal MB.
Output runtime closures contain only the outputs themselves; files have not been
moved into additional runtime dependencies. The output reduction comes from
excluding incompatible optional native packages, preserving required packages
and every file of installed packages.

Timings are medians of seven alternating warm `nix-build --check` runs with four
cores. They include evaluation, assembly, NAR checking, and daemon overhead;
downloads, helper compilation, and package-cache preparation are excluded.
Package caching is disabled in both modes. Assembler-only CI time is nearly
unchanged (0.294 → 0.290 s). These local measurements do not predict network or
cold-build timings. All forced rebuilds passed reproducibility checks, and the
611-package CI output passed independent offline pnpm reconstruction plus native
module checks for SWC, Rollup, unrs-resolver, and Parcel watcher.

### CI measurements and iets

The [2026-11-10 measurements](benchmarks/ci-2026-11-10.json) compare commit
`0ed94385e5884fdd6ccd618f9a833f55dc4a2ee3` with the native-only implementation on
an x86_64-linux Intel i7-1280P. The project is Vue v3.5.22, revision
`5a8aa0b2ba575e098cbb63b396e9bcb751eb3a0f`: 741 tarballs for evaluation of the
whole workspace, and 108 tarballs / 98 installed package instances for the
compiler-sfc assembly and closure measurements.

| Measurement (median of 5 runs) | Before | Native-only |
| --- | ---: | ---: |
| Full Vue evaluation, Nix | 0.451 s | 0.348 s |
| Full Vue evaluation, iets, 1 worker | 0.226 s | 0.136 s |
| Full Vue evaluation, iets, 4 workers | 0.113 s | 0.084 s |
| Compiler-sfc Nix rebuild with cached inputs | 1.719 s | 0.492 s |
| Compiler-sfc assembler alone, default path | 1.089 s | 0.040 s |

The previous default used pnpm. A separate comparison of the old opt-in native
assembler and the current native assembler measured about 41–42 ms for both;
the default-path speedup comes from removing pnpm assembly. Nix rebuild samples
ranged from 1.70–5.58 s before and 0.48–0.50 s after. Every forced rebuild passed
Nix's reproducibility check.

The realized assembly input closure falls from **438.25 MiB to 90.85 MiB**
(256 to 159 store paths). Copying that closure to an empty local binary cache
with zstd level 3 produces **131.76 MiB before and 28.18 MiB after**, a 78.6%
reduction in compressed payload. These are transfer byte counts, not network
timings. They include the tool runtime closures, project, and downloaded
archives; they exclude the compiler used to build drvjs itself. The installed
output remains 20.69 MiB NAR / 3.50 MiB compressed. No package files were pruned
to obtain the tool-closure reduction.

All 741 original tarball output paths are preserved. Authenticated host
prefetching still reuses those paths, and tests exercise fresh daemon downloads
with SHA-256 and SHA-512, bad-digest rejection, and offline reuse.

Compatibility was tested with [replit/iets PR #3](https://github.com/replit/iets/pull/3),
commit `69883a64bfb0d9a95dce253dae304878e07b076e`. Nix and iets produce identical
derivation paths for each export. The iets tests disable fallback and IFD, and
cover native builds of previously unfetched packages and source-change rebuilds
that reuse the tarballs. drvjs exports are static derivations: this validates
them with the dynamic-derivation-capable iets version, without introducing a
dynamic derivation or recursive evaluator into drvjs.

Evaluation measurements use five alternating warm runs, disable iets's tracing
cache, and force the full assembly derivation. Build/download time is excluded;
the compiled-file and OS caches remain warm. Assembly measurements use five
alternating runs with identical realized inputs, fresh output directories, and
four native workers. They exclude Nix evaluation, registration, downloads, and
cleanup. The raw report includes every timing and command.
The Nix rebuild measurement uses `nix run .#bench -- rebuild`, which realizes tools
and archives first, then times alternating `nix-build --check` runs with four
cores, including evaluation, assembly, NAR checking, and daemon overhead.

```sh
python3 tests/ci.py --iets /path/to/iets
nix run .#bench -- evaluate --before /tmp/vue-before --after /tmp/vue-after \
  --iets /path/to/iets --runs 5 --out evaluation.json
nix-build /tmp/vue-after --no-out-link
nix run .#bench -- closure /tmp/vue-after --out closure.json
nix run .#bench -- rebuild --before /tmp/vue-before --after /tmp/vue-after \
  --runs 5 --out rebuild-results
```

CI runs the native integration suite, independent pnpm verification, reproducible
rebuilds, and fetcher checks. The optional iets check uses the explicit binary
passed above; it does not silently fall back to invoking Nix for evaluation.

### Unpacked-store reuse

A retained pnpm store can seed another assembly without fetching or
decompressing tarballs. This is opt-in:

```sh
# Prepare the reusable store once; this also builds the initial node_modules.
cache=$(nix-build ./export --no-out-link --arg keepVerificationStore true)
# Reuse it for this export or another export with compatible dependencies.
nix-build ./export --no-out-link --arg unpackedStore "$cache/.pnpm-store"
```

The new output still omits its verification store unless
`--arg keepVerificationStore true` is also supplied. The seed remains a separate,
large Nix output; retain a GC root if you want to keep it. This path reuses
an entire retained store, rather than producing independent unpack derivations
for each package.

Nix store files stay immutable. Native assembly reads the supplied base indexes,
validates contents while copying, and applies patches only to private copies.
Retained verification stores remain self-contained. Missing packages fail offline
rather than triggering downloads; regenerate the seed when dependencies require
it. Use trusted caches produced with the pinned pnpm version and compatible
project settings.

Native store reuse reads the base index's `files` map and ignores `sideEffects`
overlays. For both fetched tarballs and cached files, native patching verifies
the locked patch checksum and checks the entire patch before writing. A matching
preimage is patched normally. If that fails, a successful whole-patch reverse
check verifies the complete postimage, which is accepted unchanged; drvjs does
not actually reverse the patch. This handles registry tarballs that already
contain a locked patch's changes. Already-applied content alone does not imply
store contamination.

If neither direction validates, drvjs rejects the partial or incompatible patch
without applying any hunks. This includes mixtures of applied/unapplied hunks or
files. Malformed patches and incorrect checksums remain errors. Supplied stores
are never modified; failures identify the patch and, for cache reuse, the
package and recovery instructions.

To regenerate a compatible store from the locked downloads:

```sh
nix-build ./export --arg unpackedStore null --arg keepVerificationStore true -o fresh
# A later build can use fresh/.pnpm-store as its unpackedStore.
```

`nix run .#bench -- unpacked-store` compares this path with cached tarballs using
fresh outputs and alternating trials. `tests/unpacked.sh` covers independent
pnpm verification, patches/hooks, Nix reproducibility, optional retention, and
incomplete-cache rejection. This mode remains opt-in.
The final Nix output is immutable: read-only pnpm commands can inspect it,
but commands that modify dependencies need a writable project/install directory.

## Current limits

- Linux. The CLI and flake helpers accept lockfile formats 9.0, 12.0 and
  experimental 13.0. drvjs reads the project document, expands compact workspace
  links and peer snapshots, and writes a format 9.0 lockfile inside the export.
  Offline verification uses pnpm 10.34.5. ARM64 is declared but not tested.
- A boot document can pin the package manager. Its packages stay out of the
  application graph. Boot config dependencies, pnpr revisions and uncompressed
  tar integrity are unsupported. npm registry prefixes can be declared through
  `namedRegistries` or `registries` in `pnpm-workspace.yaml`.
- Registry packages and explicit HTTP(S) tarballs with SHA-256/384/512 integrity.
  HTTPS is required except for loopback test registries. URL-only resolutions are
  hashed during export; Git dependencies remain unsupported.
- Workspace `link:` dependencies must point to importers inside the project.
- Supports workspace catalogs, root pnpm settings, hooks, patches and scoped
  private registries as described above. Unknown settings and repeated array `.npmrc`
  keys fail explicitly; scalar keys use the last value; nested `.npmrc`/hooks in selected workspaces are not supported.
  Configuration in excluded workspaces does not block subset export.
- Git/local-file dependencies, injected workspaces and source symlinks are rejected.
- Registry URLs containing credentials, query strings or fragments are rejected.
  Authenticated fetches require host access before a sandboxed build.
- Export records target reachability for x86_64-linux and aarch64-linux. Re-export
  after dependency or architecture-setting changes. Native modules needing
  postinstall may not work yet.
- Large graphs can exhibit pnpm private-hoisting nondeterminism. Exact offline
  comparison and Nix `--check` can report differences between otherwise equivalent
  installs; drvjs does not resolve upstream hoisting nondeterminism.
- Root tools are opt-in for subsets. Full pnpm filter grammar and production-only
  selection are future work.

## Development

`devenv.nix` declares Rust, Node, pnpm and Nix for this project.

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
nix build
PATH="$PWD/result/bin:$PATH" bash tests/integration.sh
PATH="$PWD/result/bin:$PATH" python3 tests/lockfiles.py
PATH="$PWD/result/bin:$PATH" bash tests/vue-workspace.sh
PATH="$PWD/result/bin:$PATH" bash tests/configured.sh
PATH="$PWD/result/bin:$PATH" bash tests/unpacked.sh
PATH="$PWD/result/bin:$PATH" bash tests/native.sh
PATH="$PWD/result/bin:$PATH" bash tests/native-compat.sh
PATH="$PWD/result/bin:$PATH" bash tests/native-store-patches.sh
PATH="$PWD/result/bin:$PATH" bash tests/native-already-applied.sh
PATH="$PWD/result/bin:$PATH" python3 tests/registry.py
PATH="$PWD/result/bin:$PATH" python3 tests/incremental.py
PATH="$PWD/result/bin:$PATH" python3 tests/platforms.py
```

Set `PNPM_COMPAT_BIN=/path/to/pnpm-10.6.1/bin/pnpm.cjs` for the native
compatibility scripts to exercise that exact release. The store-patch script
downloads the public `is-number` fixture, checks normal and complete-postimage
store reuse, then constructs an incompatible base index and verifies rejection.
The already-applied script tests `micro-memoize@4.0.10` against pnpm, direct
Nix-fetched tarball assembly, sandboxed Nix builds, pnpm verification and store
reuse. Its six-line hunk uses matching counts in its header. Unit tests cover
multi-hunk and multi-file normal, fully applied, partial and incompatible states,
including rejection before any file changes.

The smaller integration fixture covers aliases, scoped packages, peer contexts,
transitive dependencies, CLI shims, platform-specific optional dependencies,
offline verification, reproducibility, and tamper detection. The Vue integration
test covers real workspace projection and cache isolation.

## License

[CC0 1.0 Universal](LICENSE) (`CC0-1.0`): dedicated to the public domain.
