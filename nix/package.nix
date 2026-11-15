{ pkgs }:
let
  version = "0.1.0";
  # This derivation is sufficient for production assembly. Keeping it separate
  # also avoids evaluating/building host tools when the helper is not cached.
  unwrapped = pkgs.rustPlatform.buildRustPackage {
    pname = "drvjs";
    inherit version;
    name = "drvjs-${version}-unwrapped";
    src = pkgs.lib.fileset.toSource {
      root = ../.;
      fileset = pkgs.lib.fileset.unions [ ../Cargo.toml ../Cargo.lock ../src ../nix ];
    };
    cargoLock.lockFile = ../Cargo.lock;
    nativeCheckInputs = [ pkgs.gitMinimal ];
    meta = {
      description = "Package-granular Nix fetching for pnpm";
      license = pkgs.lib.licenses.cc0;
      platforms = [ "x86_64-linux" "aarch64-linux" ];
      mainProgram = "drvjs";
    };
  };
  # Check compilation identity when prewarming the wrapper, not on each export.
  canonical = import ./package.nix {
    pkgs = import pkgs.path {
      system = pkgs.stdenv.hostPlatform.system;
      config = {};
      overlays = [];
    };
  };
  helperHint = if unwrapped.drvPath == canonical.unwrapped.drvPath then builtins.toJSON {
    path = toString unwrapped;
    source = builtins.hashString "sha256" (toString unwrapped.src);
    nixpkgs = builtins.hashString "sha256" (toString pkgs.path);
    inherit (unwrapped) system;
  } else "";
in pkgs.runCommand "drvjs-${version}" {
  nativeBuildInputs = [ pkgs.makeWrapper ];
  passthru = { inherit unwrapped; };
  inherit (unwrapped) meta;
} ''
  mkdir -p "$out/bin"
  makeWrapper ${unwrapped}/bin/drvjs "$out/bin/drvjs" --set DRVJS_NIX_HELPER ${pkgs.lib.escapeShellArg helperHint} --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.pnpm_10 pkgs.nodejs pkgs.curl pkgs.nix pkgs.gitMinimal ]}
''
