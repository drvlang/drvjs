use std::path::PathBuf;

fn positive(arg: &str) -> Result<usize, String> {
    arg.parse::<usize>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| "expected a positive count".into())
}

#[derive(clap::Args)]
pub struct CacheCompression {
    pub output: PathBuf,
    #[arg(long, num_args = 1.., default_values_t = [3, 9, 19])]
    pub levels: Vec<i32>,
    #[arg(long, value_parser = positive, default_value_t = 3)]
    pub runs: usize,
    #[arg(long)]
    pub out: PathBuf,
}

#[derive(clap::Args)]
pub struct CacheUpdates {
    #[arg(long, required = true)]
    pub case: Vec<String>,
    #[arg(long, value_parser = positive, default_value_t = 5)]
    pub runs: usize,
    #[arg(long, default_value_t = 3)]
    pub level: i32,
    #[arg(long)]
    pub out: PathBuf,
}

#[derive(clap::Args)]
pub struct CiWorkspace {
    #[arg(long)]
    pub baseline: String,
    #[arg(long)]
    pub optimized: String,
    #[arg(long)]
    pub project: PathBuf,
    #[arg(long)]
    pub filter: Vec<String>,
    #[arg(long)]
    pub edit_file: PathBuf,
    #[arg(long, value_parser = positive, default_value_t = 7)]
    pub runs: usize,
    #[arg(long, num_args = 1.., allow_hyphen_values = true)]
    pub command: Vec<String>,
}

#[derive(clap::Args)]
pub struct Closure {
    pub export: PathBuf,
    #[arg(long)]
    pub out: PathBuf,
}

#[derive(clap::Args)]
pub struct CompareNative {
    pub reference: PathBuf,
    pub candidate: PathBuf,
}

#[derive(clap::Args)]
pub struct Evaluate {
    #[arg(long)]
    pub before: PathBuf,
    #[arg(long)]
    pub after: PathBuf,
    #[arg(long)]
    pub iets: String,
    #[arg(long, value_parser = positive, default_value_t = 5)]
    pub runs: usize,
    #[arg(long)]
    pub out: PathBuf,
}

#[derive(clap::Args)]
pub struct Experiment {
    #[arg(long)]
    pub cases: PathBuf,
    #[arg(long)]
    pub source: PathBuf,
    #[arg(long)]
    pub tarballs: PathBuf,
    #[arg(long)]
    pub out: PathBuf,
    #[arg(long, value_parser = positive, default_value_t = 3)]
    pub runs: usize,
}

#[derive(clap::Args)]
pub struct Lockfiles {
    #[arg(long)]
    pub baseline: String,
    #[arg(long)]
    pub optimized: String,
    #[arg(long, required = true)]
    pub project: Vec<PathBuf>,
    #[arg(long)]
    pub filter: String,
    #[arg(long, value_parser = positive, default_value_t = 9)]
    pub runs: usize,
}

#[derive(clap::Args)]
pub struct Materialize {
    #[arg(long)]
    pub baseline: String,
    #[arg(long)]
    pub optimized: String,
    #[arg(long)]
    pub source: PathBuf,
    #[arg(long)]
    pub tarballs: PathBuf,
    #[arg(long)]
    pub out: PathBuf,
    #[arg(long, value_parser = positive, default_value_t = 3)]
    pub runs: usize,
    #[arg(long, allow_hyphen_values = true)]
    pub baseline_arg: Vec<String>,
    #[arg(long, allow_hyphen_values = true)]
    pub optimized_arg: Vec<String>,
    #[arg(long)]
    pub dependency_layer: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct PrepareZstd {
    #[arg(long)]
    pub plan: PathBuf,
    #[arg(long)]
    pub tarballs: PathBuf,
    #[arg(long)]
    pub out: PathBuf,
    #[arg(long, default_value = "zstd")]
    pub zstd: String,
    #[arg(long, value_parser = positive, default_value_t = 4)]
    pub jobs: usize,
}

#[derive(clap::Args)]
pub struct Rebuild {
    #[arg(long)]
    pub before: Option<PathBuf>,
    #[arg(long)]
    pub after: Option<PathBuf>,
    #[arg(long)]
    pub case: Vec<String>,
    #[arg(long)]
    pub fresh: bool,
    #[arg(long)]
    pub source_edits: bool,
    #[arg(long, value_parser = positive, default_value_t = 5)]
    pub runs: usize,
    #[arg(long)]
    pub out: PathBuf,
}

#[derive(clap::Args)]
pub struct UnpackedStore {
    #[arg(long)]
    pub drvjs: PathBuf,
    #[arg(long)]
    pub source: PathBuf,
    #[arg(long)]
    pub tarballs: PathBuf,
    #[arg(long)]
    pub store: PathBuf,
    #[arg(long)]
    pub out: PathBuf,
    #[arg(long, value_parser = positive, default_value_t = 3)]
    pub runs: usize,
}

#[derive(clap::Parser)]
#[command(version, about = "Run the drvjs performance benchmarks")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Suite,
}

#[derive(clap::Subcommand)]
pub enum Suite {
    /// Measure Nix binary-cache bytes and publication wall time without changing files.
    CacheCompression(CacheCompression),
    /// Measure first publication and incremental source-update cache payloads.
    CacheUpdates(CacheUpdates),
    /// Time export, writable workspace preparation and an optional CI command after source edits.
    CiWorkspace(CiWorkspace),
    /// Measure realized assembly inputs and their compressed binary-cache payload.
    Closure(Closure),
    /// Compare native installed trees, normalizing generated output-root paths only.
    CompareNative(CompareNative),
    /// Alternate warm Nix/iets evaluations of two exports with IFD disabled.
    Evaluate(Evaluate),
    /// Alternate assembler experiments on identical inputs and fresh outputs.
    Experiment(Experiment),
    /// Compare release binaries on existing pnpm workspaces, without fetching or building.
    Lockfiles(Lockfiles),
    /// Compare assembler wall time with already-realized Nix tarballs.
    Materialize(Materialize),
    /// Prepare a measured zstd cache prototype; production uses nix/build.nix.
    PrepareZstd(PrepareZstd),
    /// Alternate forced Nix rebuilds with already-realized tools and archives.
    Rebuild(Rebuild),
    /// Compare cached tarballs with a previously realized, unpacked pnpm store.
    UnpackedStore(UnpackedStore),
}
