{ export, nonce, freshDependencies ? true }:
let
  graph = import export {};
  fresh = g: args: builtins.derivation (g.drvAttrs // {
    inherit args;
    DRVJS_BENCHMARK_NONCE = nonce;
  });
  args = if freshDependencies && builtins.head graph.args == "attach" then
    builtins.genList (i: if i == 2 then fresh graph.dependencies graph.dependencies.args
      else builtins.elemAt graph.args i) (builtins.length graph.args)
    else graph.args;
in fresh graph args
