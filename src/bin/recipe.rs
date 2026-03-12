//! Recipe CLI - Local-first package manager
//!
//! Usage:
//!   recipe install <path>           Install a recipe
//!   recipe remove <path>            Remove an installed package
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

fn default_help_footer() -> String {
    format!(
        "Defaults:\n  Recipes directory: {}\n  Build directory: temporary directory if --build-dir is not set\n  LLM profiles: $XDG_CONFIG_HOME/recipe/llm.toml (or ~/.config/recipe/llm.toml)\n  JSON output: stdout by default; recipe logs and helper output stay on stderr\n\nExamples:\n  recipe install kitty\n  recipe install ./custom/foo.rhai --define VERSION=1.2.3\n  recipe isbuilt kitty --no-persist-ctx\n  recipe list\n\nManual pages:\n  man recipe\n  man 5 recipe-recipe\n  man 7 recipe-helpers\n\nRun 'recipe <command> --help' for command-specific options.",
        metadata::default_recipes_path().display()
    )
}

fn main() -> Result<()> {
    let matches = cli::Cli::command()
        .after_help(default_help_footer())
        .get_matches();
    let cli = cli::Cli::from_arg_matches(&matches).expect("clap validated arguments");
    commands::execute(cli)
}
