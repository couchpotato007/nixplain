//! nixplain: better error messages for Nix.
//!
//! Runs Nix as usual. Only when something fails, it starts an instrumented
//! diagnostic second run and prints a rustc-style explanation.

mod context;
mod diag;
mod engines;
mod explain;
mod graph;
mod nixast;
mod nixlog;
mod render;
mod runner;
mod util;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use context::{Ownership, Target};
use diag::Diagnostic;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

#[derive(Parser)]
#[command(name = "nixplain", version, about = "Better error messages for Nix")]
struct Cli {
    /// Print diagnostics as JSON (for CI and editors)
    #[arg(long, global = true)]
    json: bool,
    /// One line per error
    #[arg(long, global = true, conflicts_with = "json")]
    short: bool,
    /// Apply machine-applicable fixes (e.g. outdated hashes)
    #[arg(long, global = true)]
    fix: bool,
    /// Don't start the diagnostic second run (only use what the log says)
    #[arg(long, global = true)]
    no_rerun: bool,
    /// Print Nix's own error with its trace before the diagnosis:
    /// `full` exactly as Nix prints it, `short` one line per frame
    #[arg(
        long,
        global = true,
        alias = "raw",
        value_enum,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "full",
        value_name = "full|short"
    )]
    show_trace: Option<TraceMode>,
    /// Save the raw internal-json log of the Nix run to this file
    #[arg(long, global = true, value_name = "FILE")]
    save_log: Option<PathBuf>,
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    color: ColorChoice,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum TraceMode {
    Full,
    Short,
}

#[derive(Clone, Copy, ValueEnum)]
enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Subcommand)]
enum Cmd {
    /// Wrap `nix build`
    Build {
        /// Flake installable, e.g. `.#mypkg`
        installable: String,
        /// Extra arguments passed to nix build (e.g. --no-link, -L)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        nix_args: Vec<String>,
    },
    /// Build a NixOS configuration (like nixos-rebuild)
    Os {
        #[arg(value_enum, default_value_t = OsAction::Build)]
        action: OsAction,
        /// Flake containing the configuration (default: `.` if it has a flake.nix, else /etc/nixos)
        #[arg(long)]
        flake: Option<String>,
        /// nixosConfigurations attribute (default: hostname)
        #[arg(long)]
        host: Option<String>,
        /// Extra arguments passed to nix build
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        nix_args: Vec<String>,
    },
    /// Build a standalone home-manager configuration (like home-manager switch)
    Home {
        #[arg(value_enum, default_value_t = HomeAction::Build)]
        action: HomeAction,
        /// Flake containing `homeConfigurations` (default: `.`)
        #[arg(long)]
        flake: Option<String>,
        /// homeConfigurations attribute (default: $USER)
        #[arg(long)]
        user: Option<String>,
        /// Extra arguments passed to nix build
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        nix_args: Vec<String>,
    },
    /// Diagnose a saved `--log-format internal-json` log
    Diagnose {
        log: PathBuf,
        #[arg(long)]
        flake: Option<String>,
        #[arg(long, requires = "flake", conflicts_with = "user")]
        host: Option<String>,
        /// a homeConfigurations attribute
        #[arg(long, requires = "flake")]
        user: Option<String>,
        #[arg(long, conflicts_with_all = ["flake", "host"])]
        installable: Option<String>,
    },
    /// Explain an error code, e.g. `nixplain explain meta/insecure`
    Explain { code: Option<String> },
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum HomeAction {
    Build,
    Switch,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum OsAction {
    Build,
    Switch,
    Boot,
    Test,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let color = match cli.color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    };
    render::set_color(color);
    match real_main(&cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("nixplain: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "nixos".into())
}

fn default_flake() -> String {
    if std::path::Path::new("flake.nix").exists() {
        ".".into()
    } else {
        "/etc/nixos".into()
    }
}

fn real_main(cli: &Cli) -> Result<ExitCode> {
    match &cli.cmd {
        Cmd::Explain { code } => {
            match code.as_deref() {
                Some(c) => match explain::explain(c) {
                    Some(t) => println!("{t}"),
                    None => bail!("unknown code `{c}`; run `nixplain explain` for a list"),
                },
                None => {
                    for (c, t) in explain::CODES {
                        println!("{c:<26} {}", t.lines().next().unwrap_or(""));
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Diagnose {
            log,
            flake,
            host,
            user,
            installable,
        } => {
            let target = match (flake, host, installable) {
                (Some(f), None, _) if user.is_some() => Target::Home {
                    flake: f.clone(),
                    user: user.clone().unwrap(),
                },
                (Some(f), Some(h), _) => Target::Nixos {
                    flake: f.clone(),
                    host: h.clone(),
                },
                (Some(f), None, _) => Target::Installable(f.clone()),
                (_, _, Some(i)) => Target::Installable(i.clone()),
                _ => Target::Unknown,
            };
            let text = std::fs::read_to_string(log)?;
            let st = nixlog::parse_str(&text);
            report(cli, &target, &st);
            Ok(ExitCode::from(1))
        }
        Cmd::Build { installable, nix_args } => {
            let target = Target::Installable(installable.clone());
            let mut args = vec!["build".to_string(), installable.clone()];
            args.extend(nix_args.iter().cloned());
            let out = runner::run_nix(&args, cli.save_log.as_deref())?;
            print!("{}", out.stdout);
            if out.success {
                return Ok(ExitCode::SUCCESS);
            }
            report(cli, &target, &out.log);
            Ok(ExitCode::from(out.code.clamp(1, 255) as u8))
        }
        Cmd::Os {
            action,
            flake,
            host,
            nix_args,
        } => {
            let flake = flake.clone().unwrap_or_else(default_flake);
            let host = host.clone().unwrap_or_else(hostname);
            let target = Target::Nixos {
                flake: flake.clone(),
                host: host.clone(),
            };
            let mut args = vec!["build".to_string(), "--no-link".into(), "--print-out-paths".into()];
            args.push(target.toplevel_attr().unwrap());
            args.extend(nix_args.iter().cloned());
            let out = runner::run_nix(&args, cli.save_log.as_deref())?;
            if !out.success {
                report(cli, &target, &out.log);
                return Ok(ExitCode::from(out.code.clamp(1, 255) as u8));
            }
            let system = out.stdout.trim().to_string();
            if *action == OsAction::Build {
                println!("{system}");
                return Ok(ExitCode::SUCCESS);
            }
            activate(&system, *action)
        }
        Cmd::Home {
            action,
            flake,
            user,
            nix_args,
        } => {
            let flake = flake.clone().unwrap_or_else(|| ".".into());
            let user = user
                .clone()
                .or_else(|| std::env::var("USER").ok())
                .unwrap_or_else(|| "user".into());
            let target = Target::Home { flake, user };
            let mut args = vec!["build".to_string(), "--no-link".into(), "--print-out-paths".into()];
            args.push(target.toplevel_attr().unwrap());
            args.extend(nix_args.iter().cloned());
            let out = runner::run_nix(&args, cli.save_log.as_deref())?;
            if !out.success {
                report(cli, &target, &out.log);
                return Ok(ExitCode::from(out.code.clamp(1, 255) as u8));
            }
            let generation = out.stdout.trim().to_string();
            if *action == HomeAction::Build {
                println!("{generation}");
                return Ok(ExitCode::SUCCESS);
            }
            // what `home-manager switch` does after building
            let ok = Command::new(format!("{generation}/activate")).status()?.success();
            Ok(if ok { ExitCode::SUCCESS } else { ExitCode::from(1) })
        }
    }
}

/// What nixos-rebuild does after a successful build.
fn activate(system: &str, action: OsAction) -> Result<ExitCode> {
    let sudo = runner::sudo_prefix();
    let run = |argv: Vec<String>| -> Result<bool> {
        let (prog, rest) = argv.split_first().unwrap();
        Ok(Command::new(prog).args(rest).status()?.success())
    };
    if matches!(action, OsAction::Switch | OsAction::Boot) {
        let mut argv = sudo.clone();
        argv.extend(["nix-env", "-p", "/nix/var/nix/profiles/system", "--set", system].map(String::from));
        if !run(argv)? {
            bail!("failed to set the system profile");
        }
    }
    let verb = match action {
        OsAction::Switch => "switch",
        OsAction::Boot => "boot",
        OsAction::Test => "test",
        OsAction::Build => unreachable!(),
    };
    let mut argv = sudo;
    argv.push(format!("{system}/bin/switch-to-configuration"));
    argv.push(verb.into());
    Ok(if run(argv)? {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn report(cli: &Cli, target: &Target, log: &nixlog::LogState) {
    if log.errors.is_empty() {
        eprintln!("nixplain: Nix failed without a structured error; its output was:");
        for l in log.plain.iter().rev().take(30).rev() {
            eprintln!("{l}");
        }
        return;
    }
    let mut own = Ownership::for_flake(target.flake());
    if !cli.no_rerun {
        own.scope_to(target);
    }
    // Nix's own view first, on stdout so the order survives pipes
    if let Some(mode) = cli.show_trace.filter(|_| !cli.json) {
        for e in &log.errors {
            match mode {
                TraceMode::Full => println!("{}", render::nix_message(&e.formatted)),
                TraceMode::Short => print!("{}", render::trace_short(e, &own)),
            }
            println!();
        }
        println!("{}", render::separator("nixplain"));
        println!();
    }
    if !cli.no_rerun && !cli.json && std::io::stderr().is_terminal() {
        eprint!("\x1b[2mnixplain: diagnosing…\x1b[0m");
    }
    let ctx = engines::Ctx {
        target,
        own: &own,
        log,
        no_rerun: cli.no_rerun,
    };
    let diags = engines::diagnose_all(&ctx);
    if !cli.no_rerun && !cli.json && std::io::stderr().is_terminal() {
        eprint!("\r\x1b[2K");
    }

    if cli.json {
        println!("{}", render::json(&diags));
    } else if cli.short {
        for d in &diags {
            println!("{}", render::short(d));
        }
    } else {
        for (i, d) in diags.iter().enumerate() {
            if i > 0 {
                println!();
            }
            print!("{}", render::human(d));
        }
    }
    if cli.fix {
        apply_fixes(&diags);
    }
}

fn apply_fixes(diags: &[Diagnostic]) {
    for d in diags {
        for e in &d.edits {
            let Ok(src) = std::fs::read_to_string(&e.file) else {
                eprintln!("nixplain: cannot read {}", e.file.display());
                continue;
            };
            let mut lines: Vec<String> = src.split('\n').map(String::from).collect();
            let Some(line) = lines.get_mut(e.line - 1) else {
                continue;
            };
            if !line.contains(&e.old) {
                eprintln!("nixplain: {}:{} changed, not applying fix", e.file.display(), e.line);
                continue;
            }
            *line = line.replacen(&e.old, &e.new, 1);
            if std::fs::write(&e.file, lines.join("\n")).is_ok() {
                println!("fixed {}:{}", render::display_path(&e.file), e.line);
            }
        }
    }
}
