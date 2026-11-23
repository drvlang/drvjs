{ pkgs }:
let
  inherit (pkgs) lib;
in pkgs.rustPlatform.buildRustPackage {
  __contentAddressed = true; outputHashMode = "recursive"; outputHashAlgo = "sha256";
  pname = "drvjs-bench";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./src ];
  };
  cargoLock = { lockFile = ./Cargo.lock; allowBuiltinFetchGit = true; };
  nativeBuildInputs = [ pkgs.makeWrapper ];
  postInstall = ''
    wrapProgram "$out/bin/drvjs-bench" \
      --suffix PATH : ${lib.makeBinPath [ pkgs.nix pkgs.gitMinimal pkgs.coreutils pkgs.zstd ]}
  '';
  meta.mainProgram = "drvjs-bench";
}
