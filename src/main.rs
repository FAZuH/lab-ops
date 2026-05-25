//! Binary entrypoint for lab-ops.
//!
//! Parses CLI arguments and dispatches to the appropriate subcommand handler.

use std::io::IsTerminal;
use std::path::Path;

use clap::CommandFactory;
use clap::Parser;
use color_eyre::Result;
use lab_ops::cli::Cli;
use lab_ops::cli::ColorMode;
use lab_ops::cli::Command;
use lab_ops::cmd::cf2ansible;
use lab_ops::cmd::cf2terra;
use lab_ops::cmd::dockernet;

fn main() -> Result<()> {
    color_eyre::install()?;

    clap_complete::CompleteEnv::with_factory(Cli::command).complete();

    let cli = Cli::parse();

    // Respect RUST_LOG env var; otherwise derive level from verbosity count.
    let filter = if let Ok(rust_log) = std::env::var("RUST_LOG") {
        tracing_subscriber::EnvFilter::new(rust_log)
    } else {
        let filter_str = match cli.verbose {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        };
        tracing_subscriber::EnvFilter::new(filter_str)
    };

    let ansi = match cli.color {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => std::io::stderr().is_terminal(),
    };

    // NO_COLOR and CLICOLOR env var overrides (per spec).
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let clicolor = std::env::var("CLICOLOR").ok();
    let ansi = match (no_color, clicolor.as_deref()) {
        (true, _) | (_, Some("0")) => false,
        _ => ansi,
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(ansi)
        .init();

    // Use color for table output based on the resolved ANSI setting.
    let use_color = ansi;

    match cli.command {
        Command::Completions { shell, dir } => {
            generate_completions(shell, dir.as_deref())?;
        }
        Command::Cf2Ansible {
            zone_file,
            zone_name,
        } => cf2ansible::run(zone_file, zone_name)?,
        Command::Cf2Terra {
            zone_file,
            zone_name,
            zone_id_var,
        } => cf2terra::run(zone_file, zone_name, zone_id_var)?,
        Command::DockerNet => {
            use tokio::runtime::Builder;

            let rt = Builder::new_current_thread().enable_all().build()?;
            rt.block_on(dockernet::run(use_color))?;
        }
        Command::NatMap { args } => {
            use tokio::runtime::Builder;
            let rt = Builder::new_current_thread().enable_all().build()?;
            rt.block_on(natmap::cli::run_cli(args, use_color))?;
        }
        Command::AutoDiscover { args } => {
            use tokio::runtime::Builder;
            let rt = Builder::new_current_thread().enable_all().build()?;
            rt.block_on(auto_discover::cli::run_cli(args))?;
        }
    };

    Ok(())
}

/// Generate shell completion script for the given shell, optionally writing to `dir`.
fn generate_completions(shell: clap_complete::Shell, dir: Option<&Path>) -> Result<()> {
    use clap_complete::env::EnvCompleter;

    let cmd = Cli::command();
    let name = cmd.get_name().to_string();

    let mut buf = Vec::new();

    // Output dynamic completion registration scripts
    let completer: &dyn EnvCompleter = match shell {
        clap_complete::Shell::Bash => &clap_complete::env::Bash,
        clap_complete::Shell::Elvish => &clap_complete::env::Elvish,
        clap_complete::Shell::Fish => &clap_complete::env::Fish,
        clap_complete::Shell::PowerShell => &clap_complete::env::Powershell,
        clap_complete::Shell::Zsh => &clap_complete::env::Zsh,
        _ => color_eyre::eyre::bail!("Unsupported shell for dynamic completions"),
    };

    completer.write_registration("COMPLETE", &name, &name, &name, &mut buf)?;
    let mut out = String::from_utf8(buf)?;

    // For zsh, if writing to stdout, strip the #compdef line because `eval $(...)` without
    // quotes in zsh will treat it as a comment for the entire collapsed string.
    // We also append `compdef _name name` to register it properly in the eval context.
    if shell == clap_complete::Shell::Zsh && dir.is_none() {
        if let Some(stripped) = out.strip_prefix(&format!("#compdef {name}\n")) {
            out = stripped.to_string();
        }
        out.push_str(&format!("\ncompdef _{name} {name}\n"));
    }

    match dir {
        Some(dir) => {
            let filename = completion_filename(shell, &name);
            let path = dir.join(&filename);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, out)?;
            eprintln!("Wrote completions to {}", path.display());
        }
        None => {
            print!("{out}");
        }
    }
    Ok(())
}

/// Return the filename convention for a given shell's completion file.
fn completion_filename(shell: clap_complete::Shell, bin_name: &str) -> String {
    match shell {
        clap_complete::Shell::Bash => bin_name.to_string(),
        clap_complete::Shell::Zsh => format!("_{bin_name}"),
        clap_complete::Shell::Fish => format!("{bin_name}.fish"),
        clap_complete::Shell::PowerShell => format!("_{bin_name}.ps1"),
        clap_complete::Shell::Elvish => format!("{bin_name}.elv"),
        _ => bin_name.to_string(),
    }
}
