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
use clap::Parser;

#[path = "recipe/cli.rs"]
mod cli;
#[path = "recipe/commands.rs"]
mod commands;
#[path = "recipe/metadata.rs"]
mod metadata;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    commands::execute(cli)
}
