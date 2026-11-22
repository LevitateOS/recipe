//! Recipe CLI - Local-first package manager
//!
//! Usage:
//!   recipe install <path>           Do every missing step and activate the package
//!   recipe download <path>          The download step only
//!   recipe build <path>             Download what is missing, then build; keep the result
//!   recipe update <name>            Update an active package from its catalog recipe
//!   recipe remove <name>            Remove an installed package and its material
//!   recipe installed                List active packages
//!   recipe cleanup [<path>]         Delete downloads and build material
//!   recipe state <path>             Print the state: none, downloaded, built, installed
//!   recipe list                     List recipes in directory
//!   recipe info <path>              Show recipe info

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches};

#[path = "recipe/cli.rs"]
mod cli;
#[path = "recipe/commands.rs"]
mod commands;
#[path = "recipe/metadata.rs"]
mod metadata;
#[path = "recipe/release_route.rs"]
mod release_route;
#[path = "recipe/runtime.rs"]
mod runtime;
#[path = "recipe/system_route.rs"]
mod system_route;

fn default_help_footer() -> String {
    format!(
        "Defaults:\n  Override recipes directory: {}\n  Shipped sources: {}\n  Build destination: stable directory under state storage unless --build-dir is set\n  Results: readable summaries in terminals; JSON when piped or with --json\n  Logs and helper output stay on stderr; --json-output FILE saves the full result\n  list, info and hash print text\n\nExamples:\n  recipe list\n  recipe info ripgrep\n  recipe install ripgrep\n  recipe install ripgrep --json\n  recipe install ./custom/foo.rhai --define VERSION=1.2.3\n  recipe update ripgrep\n  recipe installed\n  recipe remove ripgrep\n\nManual pages:\n  man recipe\n  man 5 recipe-recipe\n  man 7 recipe-helpers\n\nRun 'recipe <command> --help' for command-specific options.",
        metadata::default_recipes_path().display(),
        levitate_recipe::runtime::shipped_sources().display()
    )
}

fn run() -> Result<()> {
    // Private worker command: same binary, driven by its request file; environment
    // variables alone never change public CLI dispatch. The worker reads the
    // request file, runs the existing engine in-process under the worker
    // execution context, writes line records on stdout and exactly one result
    // file.
    // Private dispatch precedes public CLI parsing; the recipe path follows
    // the coordinator-supplied marker.
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new(
            levitate_recipe::bin_support::WORKER_ENTRY_ARG,
        ))
    {
        return levitate_recipe::bin_support::run_worker();
    }
    levitate_recipe::bin_support::install_cli_handlers()?;
    let matches = cli::Cli::command()
        .after_help(default_help_footer())
        .get_matches();
    let cli = cli::Cli::from_arg_matches(&matches).expect("clap validated arguments");
    commands::execute(cli)
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:#}");
            let signal = levitate_recipe::bin_support::signal();
            std::process::ExitCode::from(if signal != 0 { (128 + signal) as u8 } else { 1 })
        }
    }
}
