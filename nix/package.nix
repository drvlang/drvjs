{ pkgs, dynamicFile ? null, nixpkgsSource ?
  # Standalone exports also accept a caller-supplied planner file.
  let path = toString pkgs.path; in
  if builtins.hasContext path then path
  else if pkgs.lib.isStorePath path then builtins.appendContext path { ${path}.path = true; }
  else builtins.path { path = pkgs.path; }
}:
let
  version = "0.1.0";
  ca = { __contentAddressed = true; outputHashMode = "recursive"; outputHashAlgo = "sha256"; };
  # This derivation is sufficient for production assembly. Keeping it separate
  # also avoids evaluating/building host tools when the helper is not cached.
  unwrapped = pkgs.rustPlatform.buildRustPackage (ca // {
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
  });
  projectFiles = pkgs.lib.fileset.toSource {
    root = ./.;
    fileset = pkgs.lib.fileset.unions [ ./project.nix ./workspace.nix ];
  };
  entry = (pkgs.writeText "drvjs-project.nix" ''
    { directory, options }:
    let nixpkgsSource = builtins.storePath "${nixpkgsSource}";
        pkgs = import nixpkgsSource { system = "${pkgs.stdenv.hostPlatform.system}"; };
        project = import ${projectFiles}/project.nix {
          inherit pkgs nixpkgsSource;
          drvjs.unwrapped = { outPath = builtins.storePath "${unwrapped}"; system = "${pkgs.stdenv.hostPlatform.system}"; };
          dynamic = import ${dynamicFile} { inherit pkgs; };
        };
    in project.mkJsPackage ((builtins.fromJSON options) // { src = /. + directory; })
  '').overrideAttrs (_: ca // { allowSubstitutes = true; });
in pkgs.runCommand "drvjs-${version}" (ca // {
  nativeBuildInputs = [ pkgs.makeWrapper ];
  passthru = { inherit unwrapped nixpkgsSource; };
  inherit (unwrapped) meta;
}) ''
  mkdir -p "$out/bin"
  makeWrapper ${unwrapped}/bin/drvjs "$out/bin/drvjs" ${pkgs.lib.optionalString (dynamicFile != null) "--set DRVJS_ENTRY ${entry}"} --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.pnpm_10 pkgs.nodejs pkgs.curl pkgs.nix pkgs.gitMinimal ]}
''
