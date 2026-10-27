//! Command-line entry point for `sandbox`.
//!
//! Parsing stays here. Process control lives in [`runner`] so later
//! milestones can grow it without rewriting the CLI.

mod error;
mod runner;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::error::Error;
use crate::runner::Spec;

#[cfg(not(target_os = "linux"))]
compile_error!("sandbox supports Linux only; WSL2 works, other systems do not");

fn main() -> ExitCode {
    let code = match try_main() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("sandbox: {err}");
            err.exit_code()
        }
    };
    // Unix only keeps the low 8 bits. Every code we produce (a child's
    // status, 126, 127, or 128 + signal) already fits in that byte.
    ExitCode::from(code as u8)
}

fn try_main() -> Result<i32, Error> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { argv } => {
            let spec = Spec::from_args(&argv)?;
            runner::run(&spec)
        }
    }
}

#[derive(Parser)]
#[command(
    name = "sandbox",
    version,
    about = "Run a process inside a Linux sandbox"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a command as a child of the sandbox.
    Run {
        /// Program to execute, followed by its arguments.
        ///
        /// Put `--` before the program if its arguments start with `-`.
        #[arg(
            required = true,
            trailing_var_arg = true,
            num_args = 1..,
            value_name = "COMMAND"
        )]
        argv: Vec<OsString>,
    },
}
