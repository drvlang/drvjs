{
  description = "drvjs: package-granular Nix fetching for pnpm";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/6b5e5b7a6631f065bf6908986990b37d845f847f";
  inputs.drvkit = { url = "github:drvlang/drvkit"; flake = false; };
  outputs = { self, nixpkgs, drvkit }:
    let
      inherit (nixpkgs) lib;
      eachSystem = lib.genAttrs [ "x86_64-linux" "aarch64-linux" ];
      packageSets = eachSystem (system: import nixpkgs { inherit system; });
      projects = eachSystem (system: import ./nix/project.nix {
        pkgs = packageSets.${system};
        dynamic = import (drvkit + "/dynamic.nix") { pkgs = packageSets.${system}; };
        drvjs = self.packages.${system}.drvjs;
      });
      commandFlake = perSystem: script: rec {
        packages = eachSystem (system: builtins.mapAttrs (_: projects.${system}.mkJsPackage) (perSystem system));
        apps = eachSystem (system: builtins.mapAttrs (_: p: projects.${system}.mkApp p script) packages.${system});
        checks = eachSystem (system: builtins.mapAttrs (_: projects.${system}.mkTest) (perSystem system));
      };
    in {
      packages = eachSystem (system: rec {
        drvjs = import ./nix/package.nix { pkgs = packageSets.${system}; dynamicFile = drvkit + "/dynamic.nix"; };
        default = drvjs;
        bench = import ./benchmarks/package.nix { pkgs = packageSets.${system}; };
      });
      apps = eachSystem (system: rec {
        drvjs = {
          type = "app";
          program = "${self.packages.${system}.drvjs}/bin/drvjs";
          meta.description = "Build pnpm projects with Nix";
        };
        default = drvjs;
      });
      checks = eachSystem (system: { inherit (self.packages.${system}) drvjs bench; });

      lib.projectSource = import (drvkit + "/project-source.nix") nixpkgs.lib;
      lib.mkPackage = self.lib.mkJsPackage;
      lib.mkApp = { system, script ? "start", ... }@args: projects.${system}.mkApp
        (self.lib.mkPackage (removeAttrs args [ "script" ])) script;
      lib.mkTest = { system, ... }@args: projects.${system}.mkTest (removeAttrs args [ "system" ]);
      lib.mkJsPackage = { system, ... }@args: projects.${system}.mkJsPackage (removeAttrs args [ "system" ]);
      lib.mkFlake = { script ? "start", ... }@args:
        commandFlake (_: { default = removeAttrs args [ "script" ]; }) script;
      lib.mkMonorepoFlake = { src ? null, commands ? null, srcForTarget ? null, script ? "start", ... }@args:
        let
          project = if src == null then throw "drvjs: specify src for discovery or commands and srcForTarget" else src;
          declared = if commands != null then commands else
            (if builtins.pathExists (toString project + "/drvlang.json") then
              (lib.importJSON (toString project + "/drvlang.json")).commands or null else null);
          perSystem = system: let
            workspace = projects.${system}.workspace project;
            targets = if declared == null then workspace.commands
              else if builtins.isList declared then lib.genAttrs declared (s: (workspace.select [ s ]).primary)
              else declared;
          in builtins.mapAttrs (name: path: let
            target = let relative = lib.removePrefix "./" path; in
              if relative == "." || builtins.all (part: !(builtins.elem part [ "" "." ".." ])) (lib.splitString "/" relative)
              then relative else throw "drvjs: invalid workspace target path ${path}";
            input = if srcForTarget == null then project else srcForTarget { inherit name target; };
          in (removeAttrs args [ "commands" "srcForTarget" "script" ]) // {
            src = input; package = if target == "." then "." else "./${target}";
          }) targets;
        in commandFlake perSystem script;
    };
}
