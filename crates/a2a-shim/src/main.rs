mod cli;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let parsed = cli::Cli::parse();
    match parsed.command {
        cli::Command::Serve(_) => {
            eprintln!("a2a-shim serve: implementation lands in Task 27");
            std::process::exit(2);
        }
        cli::Command::Client(_) => {
            eprintln!("a2a-shim client: implementation lands in Task 33");
            std::process::exit(2);
        }
    }
}
