#!/usr/bin/env bash
# Re-run the selected export recipe, then compare the two realized outputs.
set -euo pipefail
expected=$1
export_dir=$2
shift 2
rebuilt=$(nix-build --no-out-link --expr '
  args@{ exportDir, nonce, ... }:
  let graph = import (/. + exportDir) (removeAttrs args [ "exportDir" "nonce" ]);
  in builtins.derivation (graph.drvAttrs // { drvjsRebuild = nonce; })
' --argstr exportDir "$export_dir" --argstr nonce "$$-$RANDOM" "$@")
test "$rebuilt" = "$expected"
