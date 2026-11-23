{ pkgs, drvjs, dynamic }:
let
  inherit (pkgs) lib;
  workspace = src: import ./workspace.nix { inherit lib src; };
  mkJsPackage = args@{
    src, package ? null, target ? package, filters ? [], cachePackages ? false, cacheDependencies ? false, dependenciesOnly ? false,
    sharedDependencies ? false, keepVerificationStore ? false, zstdArchives ? false
  }:
    let
      configPath = toString src + "/drvlang.json";
      aliases = if builtins.pathExists configPath then (lib.importJSON configPath).commands or {} else {};
      selectors = map (s: aliases.${s} or s) (lib.optional (target != null) target ++ filters);
      selection = (workspace src).select selectors;
      options = { inherit cachePackages cacheDependencies sharedDependencies keepVerificationStore zstdArchives; };
      build = dynamic {
        name = "drvjs-installation";
        prepare = ''
          ${drvjs.unwrapped}/bin/drvjs export --plan-only --include-sources -C ${selection.src} -o graph \
            ${lib.escapeShellArgs (lib.concatMap (s: [ "--filter" s ]) selectors)}
        '';
        expression = ''
          let pkgs = import ${pkgs.path} { system = "${pkgs.stdenv.hostPlatform.system}"; };
              graph = import ./graph/default.nix ((builtins.fromJSON ${builtins.toJSON (builtins.toJSON options)}) // {
            inherit pkgs;
            drvjs.unwrapped = {
              outPath = builtins.storePath "${drvjs.unwrapped}";
              system = "${pkgs.stdenv.hostPlatform.system}";
            };
            project = builtins.path { name = "project"; path = ./graph/project; };
          });
          in ${if dependenciesOnly then "graph.dependencies" else "graph"}
        '';
      };
    in build // {
      inherit (selection) primary;
      dependencies = if dependenciesOnly then build else mkJsPackage (args // { dependenciesOnly = true; });
    };
  mkApp = package: script: {
    type = "app";
    program = toString (pkgs.writeShellScript "drvjs-run" ''
      exec ${drvjs}/bin/drvjs run-script ${package} ${lib.escapeShellArg package.primary} ${lib.escapeShellArg script} -- "$@"
    '');
    meta.description = "Run the ${script} package script";
  };
  mkTest = args: let package = mkJsPackage args; in pkgs.runCommand "drvjs-tests" {} ''
    export HOME="$TMPDIR"
    if ${pkgs.jq}/bin/jq -e '.scripts.test | type == "string"' ${package}/${lib.escapeShellArg package.primary}/package.json >/dev/null; then
      ${drvjs}/bin/drvjs run-script ${package} ${lib.escapeShellArg package.primary} test
    fi
    mkdir "$out"
  '';
in { inherit mkJsPackage mkTest mkApp workspace; }
