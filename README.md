# drvjs

drvjs builds a pnpm project's `node_modules` with Nix. Each locked npm tarball has its own store path, so a fresh CI runner can reuse unchanged packages from your binary cache. In a monorepo, you can build or test one workspace package together with the local packages it imports.

## Try it

From a project with `package.json` and `pnpm-lock.yaml`, use Nix with flakes on Linux (x86-64 or arm64). drvjs reads lockfile formats **9.0**, **12.0** and experimental **13.0**, including files with a separate pnpm version pin. The CLI supplies Node and pnpm. Enable `nix-command flakes ca-derivations dynamic-derivations recursive-nix` in `extra-experimental-features` and `recursive-nix ca-derivations` in `extra-system-features` on the client and builder.

```sh
nix shell github:drvlang/drvjs
drvjs build                     # project and installed dependencies in result/
drvjs run -- --help             # pass --help to the start script
drvjs test                      # run the test script
```

`run` and `test` execute scripts from `package.json` in a temporary writable copy. They reuse the installed dependencies across source edits. Use `--script` to run a different script. Dependency installation skips lifecycle scripts, so packages that require a postinstall build may need an extra step.

```sh
drvjs build --cache-dependencies ./apps/api
drvjs run --script lint @acme/api
drvjs test @acme/api --filter .  # include root development tools
drvjs export @acme/api           # standalone build in .drvjs/
nix-build .drvjs
```

Omitting the package builds the whole project and runs scripts at the root. A package name or root-relative path selects its dependency closure. The export is self-contained and builds with import-from-derivation disabled. For a single-package export, pass `--include-sources` to include application files. See `drvjs help build` for cache options.

## Build from a flake

Add this `flake.nix` to your project. Stage it and the project files in Git so Nix can see them.

```nix
{
  inputs.drvjs.url = "github:drvlang/drvjs";
  outputs = { drvjs, ... }: drvjs.lib.mkFlake {
    src = drvjs.lib.projectSource {
      root = ./.;
      files = [ ./package.json ./pnpm-lock.yaml ./src ];
    };
  };
}
```

Run `nix build` for the installed project, or `nix run . -- --help` for its start script. Set `package = "./apps/api";` to select a workspace, `script = "serve";` to choose its script, or `cacheDependencies = true;` to reuse the installed dependencies across source edits. To add a package to existing outputs, use `drvjs.lib.mkPackage { inherit system src; }`.

The example keeps application code in `src/`. List the actual source directories, tests and assets your package scripts read. The recursive builder plans directly from the lockfile with import-from-derivation disabled. Flake builds use the registry integrity values in the lockfile. Keep registry credentials outside the flake source.

### Multiple packages in one pnpm workspace

Use `drvjs.lib.mkMonorepoFlake` to expose each child workspace by its package name. For a workspace with `apps/` and `packages/`, use `files = [ ./package.json ./pnpm-lock.yaml ./pnpm-workspace.yaml ./apps ./packages ];`. Include `./drvlang.json` if you define command aliases. The build includes the selected package and the workspace packages it depends on.

```sh
nix build .#@acme/api
nix run .#@acme/worker -- --help
nix flake check                 # run each declared test script
```

Set `commands = [ "@acme/api" "@acme/worker" ];` to publish only those workspace names, or `commands = { api = "./apps/api"; };` to choose a shorter output name. The CLI and flake also read an output-name map from a root `drvlang.json`, for example `{"commands":{"api":"./apps/api"}}`. Put shared libraries anywhere in the workspace and declare them as workspace dependencies.

For per-command source lists, set `srcForTarget = { name, target }: ...;` to return a `projectSource` containing the selected command, shared code and root manifests. With an explicit `commands` map, you can omit the project-wide `src`.

[Reference, private registries and CI caching](REFERENCE.md) · [CC0-1.0 license](LICENSE)
