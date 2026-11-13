//! Recipe CLI - Local-first package manager
//!
//! Usage:
//!   recipe install <path>           Install a recipe
//!   recipe update <name>            Update an active package from its catalog recipe
//!   recipe remove <name>            Deactivate an installed package
//!   recipe installed                List active packages
//!   recipe cleanup <path>           Clean up build artifacts
//!   recipe isinstalled <path>       Execute is_installed(ctx)
//!   recipe isbuilt <path>           Execute is_built(ctx)
//!   recipe isacquired <path>        Execute is_acquired(ctx)
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
#[path = "recipe/runtime.rs"]
mod runtime;

fn default_help_footer() -> String {
    format!(
        "Defaults:\n  Override recipes directory: {}\n  Shipped sources: {}\n  Build destination: stable directory under state storage unless --build-dir is set\n  LLM profiles: $XDG_CONFIG_HOME/recipe/llm.toml (or ~/.config/recipe/llm.toml)\n  Results: readable summaries in terminals; JSON when piped or with --json\n  Logs and helper output stay on stderr; --json-output FILE saves the full result\n  list, info and hash print text\n\nExamples:\n  recipe list\n  recipe info ripgrep\n  recipe install ripgrep\n  recipe install ripgrep --json\n  recipe install ./custom/foo.rhai --define VERSION=1.2.3\n  recipe update ripgrep\n  recipe installed\n  recipe remove ripgrep\n\nManual pages:\n  man recipe\n  man 5 recipe-recipe\n  man 7 recipe-helpers\n\nRun 'recipe <command> --help' for command-specific options.",
        metadata::default_recipes_path().display(),
        levitate_recipe::runtime::shipped_sources().display()
    )
}

fn run() -> Result<()> {
    // Private worker command: same binary, protocol-driven; environment
    // variables alone never change public CLI dispatch. The worker reads one
    // request frame from stdin, runs the existing engine in-process under the
    // worker execution context, and terminates stdout with exactly one result frame.
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
