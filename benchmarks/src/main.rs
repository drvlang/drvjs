mod assembly;
mod cache;
mod cli;
mod evaluation;
mod project;
use anyhow::Result;
use clap::Parser;
use cli::{Cli, Suite};
fn main() -> Result<()> {
    match Cli::parse().command {
        Suite::CacheCompression(a) => cache::compression(a),
        Suite::CacheUpdates(a) => cache::updates(a),
        Suite::CiWorkspace(a) => project::workspace(a),
        Suite::DynamicDependencies(a) => project::dynamic_dependencies(a),
        Suite::Closure(a) => cache::closure(a),
        Suite::CompareNative(a) => assembly::compare(a),
        Suite::Evaluate(a) => evaluation::evaluate(a),
        Suite::Experiment(a) => assembly::experiment(a),
        Suite::Lockfiles(a) => project::lockfiles(a),
        Suite::Materialize(a) => assembly::materialize(a),
        Suite::PrepareZstd(a) => assembly::prepare(a),
        Suite::Rebuild(a) => evaluation::rebuild(a),
        Suite::UnpackedStore(a) => assembly::unpacked(a),
    }
}
