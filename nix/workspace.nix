# Only read workspace membership during flake evaluation. Rust owns resolution,
# package integrity and the final projection inside the recursive planner.
{ lib, src }:
let
  root = toString src;
  lock = builtins.readFile (root + "/pnpm-lock.yaml");
  document = lib.removePrefix "\n" (lib.last (lib.splitString "\n---\n" lock));
  canonicalCompact = lib.hasPrefix "{\n\t\"lockfileVersion\"" document;
  isJson = lib.hasPrefix "{" document && !canonicalCompact;
  json = builtins.fromJSON document;
  # pnpm writes ordinary YAML block maps or JSON-with-trailing-commas (v13).
  # Only the last workspace section belongs to the project in a two-document lock.
  # Skip the registry packages and peer snapshots. They can account for
  # hundreds of thousands of lines in a large workspace.
  sectionStart = if canonicalCompact then "\n\t\"projects\": {\n" else "\nimporters:\n";
  sections = lib.splitString sectionStart ("\n" + document);
  rest = if builtins.length sections > 1 then lib.last sections else "";
  boundary = if canonicalCompact then "\n\t[}]" else "\n[^[:space:]#]";
  lines = lib.splitString "\n" (builtins.head (builtins.split boundary rest));
  keyOf = line: let
    yaml = builtins.match "  ([^[:space:]].*):[[:blank:]]*([{][}])?" line;
    compact = builtins.match "\t\t(\".*\"):[[:blank:]]*[{].*" line;
    key = if yaml == null then null else builtins.head yaml;
  in if compact != null then builtins.fromJSON (builtins.head compact)
     else if key == null then null
     else if lib.hasPrefix "\"" key then builtins.fromJSON key
     else if lib.hasPrefix "'" key then builtins.replaceStrings [ "''" ] [ "'" ] (lib.removeSuffix "'" (lib.removePrefix "'" key))
     else key;
  importers = if isJson then json.projects or json.importers else {};
  paths = if isJson then builtins.attrNames importers else builtins.filter (p: p != null) (map keyOf lines);
  safe = p: p == "." || (p != "" && !(lib.hasPrefix "/" p)
    && builtins.all (s: !(builtins.elem s [ "." ".." "" ])) (lib.splitString "/" p));
  manifests = lib.genAttrs paths (p: if !safe p then throw "drvjs: invalid importer path ${p}"
    else lib.importJSON (root + "/${p}/package.json"));
  commands = builtins.listToAttrs (map (p: {
    name = manifests.${p}.name or p; value = "./${p}";
  }) (builtins.filter (p: p != ".") paths));
  locate = selector: let
    s = lib.removeSuffix "..." selector;
    matches = builtins.filter (p: p == lib.removePrefix "./" s ||
      (builtins.pathExists (root + "/${p}/package.json") && (manifests.${p}.name or null) == s)) paths;
  in if lib.hasPrefix "./" s && builtins.pathExists (root + "/${s}/package.json")
     then lib.removePrefix "./" s
     else if builtins.length matches == 1 then builtins.head matches
     else throw "drvjs: expected exactly one workspace named ${selector}";
  # Prune sources conservatively using concrete workspace links. A patch or
  # local link in a registry snapshot can bring in files outside this map.
  has = needle: text: builtins.length (builtins.split needle text) > 1;
  yamlGraph = builtins.foldl' (state: line: let
    key = keyOf line;
    link = builtins.match "[[:blank:]]+version:[[:blank:]]+link:([-a-zA-Z0-9_./@]+)" line;
  in if key != null then state // { current = key; }
     else if link != null && state.current != null then state // {
       edges = state.edges // { ${state.current} = [ (builtins.head link) ] ++ (state.edges.${state.current} or []); };
     }
     else if has "link:" line && builtins.match "[[:blank:]]+specifier:.*" line == null
     then state // { certain = false; } else state
  ) { current = null; edges = {}; certain = true; } lines;
  snapshotParts = lib.splitString "\nsnapshots:\n" document;
  yamlPrune = !canonicalCompact && paths != [] && yamlGraph.certain &&
    !(has "patchedDependencies:" document) &&
    (builtins.length snapshotParts == 1 || !(has "link:" (lib.last snapshotParts)));
  links = value: if builtins.isAttrs value then lib.concatMap links (builtins.attrValues value)
    else if builtins.isString value && lib.hasPrefix "link:" value then [ (lib.removePrefix "link:" value) ] else [];
  localLinks = key: if isJson then links importers.${key} else yamlGraph.edges.${key} or [];
  select = selectors: let
    chosen = map locate selectors;
    prune = selectors != [] && (if isJson then json ? importers &&
      links (json.snapshots or {}) == [] && !(json ? patchedDependencies) else yamlPrune);
    closure = builtins.genericClosure {
      startSet = map (key: { inherit key; }) chosen;
      operator = item: map (link: {
        key = let path = toString (/. + "/workspace/${item.key}/${link}");
              in if path == "/workspace" then "." else lib.removePrefix "/workspace/" path;
      }) (localLinks item.key);
    };
    kept = lib.genAttrs (map (item: item.key) closure) (_: true);
    include = rel: if rel == "." then true else if manifests ? ${rel} then kept ? ${rel}
      else include (dirOf rel);
    metadata = lib.genAttrs (lib.concatMap (p: [ "${p}/package.json" "${p}/.npmrc" "${p}/.pnpmfile.cjs" ]) paths) (_: true);
  in {
    primary = if chosen == [] then "." else builtins.head chosen;
    src = lib.cleanSourceWith {
      name = "js-source"; inherit src;
      filter = path: type: let rel = lib.removePrefix (toString (src.origSrc or src) + "/") path; in
        !(builtins.elem (baseNameOf path) [ ".git" "node_modules" ".pnpm-store" ".drvjs" ])
        && !(type == "symlink" && (rel == "result" || lib.hasPrefix "result-" rel))
        && (!prune || type == "directory" || metadata ? ${rel} || include rel);
    };
  };
in {
  inherit select;
  commands = if builtins.length (builtins.attrNames commands) == builtins.length (builtins.filter (p: p != ".") paths)
    then commands else throw "drvjs: duplicate workspace name; set commands explicitly";
}
