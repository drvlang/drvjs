{ pkgs, dynamicFile ? null }:
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
      fileset = pkgs.lib.fileset.unions [ ../Cargo.toml ../Cargo.lock ../src ./build.nix ./package.nix ];
    };
    cargoLock = {
      lockFile = ../Cargo.lock;
      allowBuiltinFetchGit = true;
    };
    nativeCheckInputs = [ pkgs.gitMinimal ];
    meta = {
      description = "Package-granular Nix fetching for pnpm";
      license = pkgs.lib.licenses.cc0;
      platforms = [ "x86_64-linux" "aarch64-linux" ];
      mainProgram = "drvjs";
    };
  };
  entry = pkgs.writeText "drvjs-project.nix" ''
    { directory, options }:
    let pkgs = import ${pkgs.path} { system = "${pkgs.stdenv.hostPlatform.system}"; };
        project = import ${./.}/project.nix {
          inherit pkgs;
          drvjs.unwrapped = { outPath = builtins.storePath "${unwrapped}"; system = "${pkgs.stdenv.hostPlatform.system}"; };
          dynamic = import ${dynamicFile} { inherit pkgs; };
        };
    in project.mkJsPackage ((builtins.fromJSON options) // { src = /. + directory; })
  '';
in pkgs.runCommand "drvjs-${version}" {
  nativeBuildInputs = [ pkgs.makeWrapper ];
  passthru = { inherit unwrapped; };
  inherit (unwrapped) meta;
} ''
  mkdir -p "$out/bin"
  makeWrapper ${unwrapped}/bin/drvjs "$out/bin/drvjs" ${pkgs.lib.optionalString (dynamicFile != null) "--set DRVJS_ENTRY ${entry}"} --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.pnpm_10 pkgs.nodejs pkgs.curl pkgs.nix pkgs.gitMinimal ]}
''
