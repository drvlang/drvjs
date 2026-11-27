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
  workerSrc = pkgs.lib.fileset.toSource {
    root = ../.;
    fileset = pkgs.lib.fileset.unions [ ../Cargo.toml ../Cargo.lock ../src ./build.nix ./package.nix ];
  };
  cargoLock = {
    lockFile = ../Cargo.lock;
  };
  meta = {
    description = "Package-granular Nix fetching for pnpm";
    license = pkgs.lib.licenses.cc0;
    platforms = [ "x86_64-linux" "aarch64-linux" ];
    mainProgram = "drvjs";
  };
  # Input-addressed so fresh CI workers substitute drvjs-unwrapped directly
  # without realizing rustc, cargo, gcc, or crate sources. Unit tests run in
  # checks.<system>.drvjs-tests so consumer builds never pay test compilation.
  unwrapped = pkgs.rustPlatform.buildRustPackage {
    pname = "drvjs";
    inherit version cargoLock meta;
    name = "drvjs-${version}-unwrapped";
    src = workerSrc;
    buildInputs = [ pkgs.zstd pkgs.zlib pkgs.openssl ];
    auditable = false;
    doCheck = false;
  };
  tests = pkgs.rustPlatform.buildRustPackage (ca // {
    pname = "drvjs-tests";
    inherit version cargoLock meta;
    src = workerSrc;
    buildInputs = [ pkgs.zstd pkgs.zlib pkgs.openssl ];
    nativeCheckInputs = [ pkgs.gitMinimal ];
    auditable = false;
    doBuild = false;
    doInstall = false;
    buildPhase = "true";
    installPhase = "mkdir -p \"$out\"";
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
          inherit pkgs;
          unwrapped = builtins.storePath "${unwrapped}";
          gitMinimal = builtins.storePath "${pkgs.gitMinimal}";
        };
    in project.mkJsPackage ((builtins.fromJSON options) // { src = /. + directory; })
  '').overrideAttrs (_: ca // { allowSubstitutes = true; });
in pkgs.runCommand "drvjs-${version}" (ca // {
  nativeBuildInputs = [ pkgs.makeWrapper ];
  passthru = { inherit unwrapped tests nixpkgsSource; };
  inherit meta;
}) ''
  mkdir -p "$out/bin"
  makeWrapper ${unwrapped}/bin/drvjs "$out/bin/drvjs" --set DRVJS_ENTRY ${entry} --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.pnpm_10 pkgs.nodejs pkgs.curl pkgs.nix pkgs.gitMinimal ]}
''
