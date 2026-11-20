{ pkgs, drvjs }:
let
  inherit (pkgs) lib;
  # Include ancestor directories so builtins.path can reach explicitly requested files.
  ancestors = path: [ path ] ++ lib.optionals (path != ".") (ancestors (dirOf path));
  pathSet = files: lib.genAttrs (lib.concatMap ancestors files) (_: true);

  # Use the same lockfile parser and workspace closure as the CLI. Only
  # manifests and configuration are needed to determine which sources to keep.
  workspace = src:
    let
      root = toString src;
      snapshot = name: keep: builtins.path {
        inherit name;
        path = src;
        filter = path: type: let rel = lib.removePrefix "${root}/" path; in
          path == root || !(builtins.elem (baseNameOf path) [ ".git" "node_modules" ".pnpm-store" ])
          && rel != ".drvjs" && !(type == "symlink" && rel == "result") && keep rel;
      };
      info = input: flags: builtins.fromJSON (builtins.readFile (pkgs.runCommand "drvjs-workspaces.json" {} ''
        ${drvjs.unwrapped}/bin/drvjs workspace-info ${input} ${lib.escapeShellArgs flags} > "$out"
      ''));
      all = info (snapshot "drvjs-lock" (p: p == "pnpm-lock.yaml")) [ "--list" ];
      importers = lib.genAttrs all (_: true);
      inDir = dir: file: if dir == "." then file else "${dir}/${file}";
      metadata = pathSet ([ "pnpm-lock.yaml" "pnpm-workspace.yaml" ] ++ lib.concatMap
        (p: map (inDir p) [ "package.json" ".npmrc" ".pnpmfile.cjs" ]) all);
      manifests = snapshot "drvjs-manifests" (p: metadata ? ${p});
    in {
      commands = let
        entry = path: {
          name = (lib.importJSON "${root}/${path}/package.json").name or path;
          value = "./${path}";
        };
        entries = map entry (builtins.filter (p: p != ".") all);
        result = builtins.listToAttrs entries;
      in if builtins.length entries != builtins.length (builtins.attrNames result)
        then throw "drvjs: duplicate workspace package name; set commands to an attribute set of paths"
        else result;

      select = filters: if filters == [] then { primary = "."; src = snapshot "js-source" (_: true); }
        else let
          selection = info manifests ([ "--" ] ++ filters);
          selected = lib.genAttrs selection.selected (_: true);
          # Nested importers own their own files, even when their parent is selected.
          included = path: if importers ? ${path} then selected ? ${path}
            else if path == "." then false else included (dirOf path);
          patches = map (p: lib.removePrefix "./" (lib.path.subpath.normalise p)) selection.patches;
          support = metadata // pathSet patches;
        in {
          inherit (selection) primary;
          src = snapshot "js-source" (p: support ? ${p} || included p);
        };
    };

  mkJsPackage = {
    src, package ? null, filters ? [], cachePackages ? false, cacheDependencies ? false,
    sharedDependencies ? false, keepVerificationStore ? false, zstdArchives ? false
  }:
    let
      selectors = lib.optional (package != null) package ++ filters;
      selection = (workspace src).select selectors;
      graph = pkgs.runCommand "drvjs-export" {} ''
        ${drvjs.unwrapped}/bin/drvjs export --include-sources -C ${selection.src} -o "$TMPDIR/export" \
          ${lib.escapeShellArgs (lib.concatMap (s: [ "--filter" s ]) selectors)}
        mv "$TMPDIR/export" "$out"
      '';
      # Re-import the projected tree by content, just like a standalone export.
      # Workspace edits outside the selection cannot invalidate the installation.
      project = builtins.path { name = "project"; path = "${graph}/project"; };
    in (import ./build.nix {
      inherit pkgs drvjs project cachePackages cacheDependencies sharedDependencies keepVerificationStore zstdArchives;
      plan = lib.importJSON "${graph}/plan.json";
    }) // { inherit (selection) primary; planner = graph; };

  mkApp = package: script: {
    type = "app";
    program = toString (pkgs.writeShellScript "drvjs-run" ''
      exec ${drvjs}/bin/drvjs run-script ${package} ${lib.escapeShellArg package.primary} ${lib.escapeShellArg script} -- "$@"
    '');
    meta.description = "Run the ${script} package script";
  };
in { inherit mkJsPackage mkApp workspace; }
