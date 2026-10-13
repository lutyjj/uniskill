mod cli;
mod config;
mod error;
mod fetch_plan;
mod fetcher;
mod harnesses;
mod hook;
mod linker;
mod mcp;
mod skill;
mod state;
mod sync;
mod worktree;

fn main() {
    match cli::run() {
        Ok(()) => {}
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}
