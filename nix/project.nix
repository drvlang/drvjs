{ pkgs, drvjs, dynamic }:
let
  inherit (pkgs) lib;
  ca = { __contentAddressed = true; outputHashMode = "recursive"; outputHashAlgo = "sha256"; };
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
      # Only the graph output feeds dependency planning. Its CA path is stable
      # when application code changes, including on a fresh CI worker.
      projection = builtins.derivation (ca // {
        name = "drvjs-project";
        inherit (drvjs.unwrapped) system;
        outputs = [ "out" "graph" ];
        builder = "${drvjs.unwrapped}/bin/drvjs";
        args = [ "project-inputs" "-C" selection.src "--sources" (builtins.placeholder "out")
          "--graph" (builtins.placeholder "graph") ]
          ++ lib.concatMap (s: [ "--filter" s ]) selectors;
      });
      sourceExpression = if dependenciesOnly then
        ''(builtins.storePath "${projection.graph}") + "/project"''
        else ''builtins.storePath "${projection.out}"'';
      build = dynamic {
        name = "drvjs-installation";
        expression = ''
          let pkgs = import ${pkgs.path} { system = "${pkgs.stdenv.hostPlatform.system}"; };
              graph = import ${projection.graph}/default.nix ((builtins.fromJSON ${builtins.toJSON (builtins.toJSON options)}) // {
            inherit pkgs;
            drvjs.unwrapped = {
              outPath = builtins.storePath "${drvjs.unwrapped}";
              system = "${pkgs.stdenv.hostPlatform.system}";
            };
            project = ${sourceExpression};
          });
          in ${if dependenciesOnly then "graph.dependencies" else "graph"}
        '';
      };
    in build // {
      inherit (selection) primary;
      project = projection.out;
      dependencies = if dependenciesOnly then build else mkJsPackage (args // { dependenciesOnly = true; });
    };
  mkApp = package: script: {
    type = "app";
    program = toString ((pkgs.writeShellScript "drvjs-run" ''
      exec ${drvjs}/bin/drvjs run-script --project ${package.project} ${package.dependencies} ${lib.escapeShellArg package.primary} ${lib.escapeShellArg script} -- "$@"
    '').overrideAttrs (_: ca // { allowSubstitutes = true; }));
    meta.description = "Run the ${script} package script";
  };
  mkTest = args: let package = mkJsPackage (args // { dependenciesOnly = true; }); in pkgs.runCommand "drvjs-tests" ca ''
    export HOME="$TMPDIR"
    if ${pkgs.jq}/bin/jq -e '.scripts.test | type == "string"' ${package.project}/${lib.escapeShellArg package.primary}/package.json >/dev/null; then
      ${drvjs}/bin/drvjs run-script --project ${package.project} ${package} ${lib.escapeShellArg package.primary} test
    fi
    mkdir "$out"
  '';
in { inherit mkJsPackage mkTest mkApp workspace; }
