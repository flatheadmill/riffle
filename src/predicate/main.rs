mod calibration;
mod case;
mod engine;
mod longhand;
mod protocol;
mod runner;

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use protocol::{Identity, WorkerRequest};

#[derive(Parser)]
#[command(about = "Replay recorded spatial predicate cases")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Replay(runner::ReplayRequest),
    #[command(hide = true)]
    Identity,
    #[command(hide = true)]
    Worker(WorkerRequest),
}

fn main() -> ExitCode {
    let result = match Args::parse().command {
        Command::Replay(request) => {
            return match runner::replay(request, std::env::args().collect()) {
                Ok(code) => ExitCode::from(code),
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::FAILURE
                }
            };
        }
        Command::Identity => serde_json::to_writer(std::io::stdout(), &Identity::compiled())
            .map_err(|e| e.to_string()),
        Command::Worker(request) => {
            engine::run(request, std::env::args().collect(), Identity::compiled())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
