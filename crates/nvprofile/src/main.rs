//! nvprofile: CLI + TUI front-end. All behaviour lives in `nvprofile-engine`.

mod cli;
mod ctx;
mod tui;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "nvprofile", version, about = "Power/fan/V-F profiles for NVIDIA GPUs via the LACT daemon")]
struct Args {
    /// LACT GPU id (default: the first NVIDIA GPU)
    #[arg(long, global = true)]
    gpu: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Live status: GPU, stats, active preset
    Status {
        #[arg(long)]
        json: bool,
    },
    /// List presets (active one marked)
    Presets {
        #[arg(long)]
        json: bool,
    },
    /// Show one preset
    Show {
        preset: String,
        #[arg(long)]
        json: bool,
    },
    /// Apply a preset
    Apply {
        preset: String,
        /// Print the plan and the config that would be written; change nothing
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Reset to stock (same as `apply stock`)
    Reset {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Save the currently applied settings as a user preset
    Save {
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// Overwrite an existing user preset
        #[arg(long)]
        force: bool,
    },
    /// Delete a user preset
    Delete { preset: String },
    /// Compare the applied config with a preset
    Diff {
        preset: String,
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let args = Args::parse();
    let json_errors = matches!(
        &args.cmd,
        Some(Cmd::Status { json: true } | Cmd::Presets { json: true } | Cmd::Show { json: true, .. }
            | Cmd::Apply { json: true, .. } | Cmd::Reset { json: true, .. } | Cmd::Diff { json: true, .. })
    );
    let result = ctx::Ctx::new(args.gpu).and_then(|ctx| match args.cmd {
        None => tui::run(&ctx),
        Some(Cmd::Status { json }) => cli::status(&ctx, json),
        Some(Cmd::Presets { json }) => cli::presets(&ctx, json),
        Some(Cmd::Show { preset, json }) => cli::show(&ctx, &preset, json),
        Some(Cmd::Apply { preset, dry_run, json }) => cli::apply(&ctx, &preset, dry_run, json),
        Some(Cmd::Reset { dry_run, json }) => cli::reset(&ctx, dry_run, json),
        Some(Cmd::Save { name, description, force }) => cli::save(&ctx, &name, &description, force),
        Some(Cmd::Delete { preset }) => cli::delete(&ctx, &preset),
        Some(Cmd::Diff { preset, json }) => cli::diff(&ctx, &preset, json),
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if json_errors {
                eprintln!("{}", serde_json::to_string(&e).unwrap_or_else(|_| e.to_string()));
            } else {
                eprintln!("nvprofile: {e}");
            }
            ExitCode::FAILURE
        }
    }
}
