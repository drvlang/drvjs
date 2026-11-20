{
  description = "drvjs: package-granular Nix fetching for pnpm";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/6b5e5b7a6631f065bf6908986990b37d845f847f";
  outputs = { self, nixpkgs }:
    let
      inherit (nixpkgs) lib;
      eachSystem = lib.genAttrs [ "x86_64-linux" "aarch64-linux" ];
      packageSets = eachSystem (system: import nixpkgs { inherit system; });
      projects = eachSystem (system: import ./nix/project.nix {
        pkgs = packageSets.${system};
        drvjs = self.packages.${system}.drvjs;
      });
      commandFlake = perSystem: script: rec {
        packages = eachSystem (system: builtins.mapAttrs (_: projects.${system}.mkJsPackage) (perSystem system));
        apps = eachSystem (system: builtins.mapAttrs (_: p: projects.${system}.mkApp p script) packages.${system});
      };
    in {
      packages = eachSystem (system: rec {
        drvjs = import ./nix/package.nix { pkgs = packageSets.${system}; };
        default = drvjs;
      });
      apps = eachSystem (system: rec {
        drvjs = {
          type = "app";
          program = "${self.packages.${system}.drvjs}/bin/drvjs";
          meta.description = "Build pnpm projects with Nix";
        };
        default = drvjs;
      });
      checks = eachSystem (system: { inherit (self.packages.${system}) drvjs; });

      lib.mkJsPackage = { system, ... }@args: projects.${system}.mkJsPackage (removeAttrs args [ "system" ]);
      lib.mkFlake = { script ? "start", ... }@args:
        commandFlake (_: { default = removeAttrs args [ "script" ]; }) script;
      lib.mkMonorepoFlake = { src, commands ? null, script ? "start", ... }@args:
        let
          perSystem = system: let
            targets = if commands == null then (projects.${system}.workspace src).commands
              else if builtins.isList commands then lib.genAttrs commands (name: name)
              else commands;
          in builtins.mapAttrs (_: package: (removeAttrs args [ "commands" "script" ]) // { inherit package; }) targets;
          outputs = commandFlake perSystem script;
        in outputs // { checks = outputs.packages; };
    };
}
