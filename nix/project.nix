{ pkgs, drvjs ? null, unwrapped ? drvjs.unwrapped, readSource ? null, gitMinimal ? pkgs.gitMinimal }:
let
  inherit (pkgs) lib;
  system = pkgs.stdenv.hostPlatform.system;
  ca = { __contentAddressed = true; outputHashMode = "recursive"; outputHashAlgo = "sha256"; };
  workspace = src: import ./workspace.nix { inherit lib src readSource; };
  mkJsPackage = args@{
    src, package ? null, target ? package, filters ? [], cachePackages ? false, cacheDependencies ? false, dependenciesOnly ? false,
    sharedDependencies ? false, keepVerificationStore ? false, zstdArchives ? false
  }:
    let
      project = workspace src;
      aliases = project.config.commands or {};
      selectors = map (s: aliases.${s} or s) (lib.optional (target != null) target ++ filters);
      selection = project.select selectors;
      options = { inherit cachePackages cacheDependencies sharedDependencies keepVerificationStore zstdArchives dependenciesOnly; };
      # Only the graph output feeds dependency planning. Its CA path is stable
      # when application code changes, including on a fresh CI worker.
      projection = builtins.derivation (ca // {
        name = "drvjs-project";
        inherit system;
        outputs = [ "out" "graph" ];
        builder = "${unwrapped}/bin/drvjs";
        args = [ "project-inputs" "-C" selection.src "--sources" (builtins.placeholder "out")
          "--graph" (builtins.placeholder "graph") ]
          ++ lib.concatMap (s: [ "--filter" s ]) selectors;
      });
      planner = builtins.derivation ({
        name = "drvjs-installation-plan.drv";
        inherit system;
        builder = "${unwrapped}/bin/drvjs";
        args = [ "internal-plan" ];
        __contentAddressed = true;
        outputHashMode = "text";
        outputHashAlgo = "sha256";
        requiredSystemFeatures = [ "recursive-nix" ];
        preferLocalBuild = true;
        allowSubstitutes = true;
        drvjsOut = unwrapped;
        graphDir = projection.graph;
        projectDir = if dependenciesOnly then "${projection.graph}/project" else projection.out;
        optionsJson = builtins.toJSON options;
      } // lib.optionalAttrs selection.hasPatches {
        gitMinimalOut = gitMinimal;
      });
      build = builtins.derivation (ca // {
        name = "drvjs-installation";
        inherit system;
        preferLocalBuild = true;
        allowSubstitutes = true;
        builder = "${unwrapped}/bin/drvjs";
        args = [ "internal-symlink" (builtins.outputOf planner.outPath "out") (builtins.placeholder "out") ];
      }) // {
        inherit planner;
        passthru = { inherit planner; };
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
