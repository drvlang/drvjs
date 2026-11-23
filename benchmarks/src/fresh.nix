{ export, nonce }: let
              graph = import export {};
              fresh = g: args: builtins.derivation {
                inherit (g) name system builder;
                outputs = g.outputs or [ "out" ];
                inherit args;
                PATH = g.PATH or "";
                preferLocalBuild = true;
                DRVJS_BENCHMARK_NONCE = nonce;
              };
              args = if builtins.head graph.args == "attach" then
                builtins.genList (i: if i == 2 then fresh graph.dependencies graph.dependencies.args
                  else builtins.elemAt graph.args i) (builtins.length graph.args)
                else graph.args;
            in fresh graph args
