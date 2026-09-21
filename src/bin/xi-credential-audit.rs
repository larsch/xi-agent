#[path = "../credential_audit.rs"]
mod credential_audit;

use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "xi-credential-audit",
    about = "Audit xi session files for credential guard hits"
)]
struct Args {
    #[arg(long)]
    sessions_dir: Option<PathBuf>,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    details: bool,
}

fn main() {
    let args = Args::parse();
    let sessions_dir = match args.sessions_dir {
        Some(path) => path,
        None => match credential_audit::default_sessions_dir() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("error: {error}");
                std::process::exit(2);
            }
        },
    };
    let report = credential_audit::scan(&sessions_dir);
    print!(
        "{}",
        credential_audit::render(&report, args.details, args.json)
    );
    if report.file_errors > 0 {
        std::process::exit(2);
    }
    if !report.hits.is_empty() {
        std::process::exit(1);
    }
}
