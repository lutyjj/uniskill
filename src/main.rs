mod cache;
mod cli;
mod config;
mod error;
mod fetch_plan;
mod fetcher;
mod filesystem;
mod harnesses;
mod hook;
mod linker;
mod mcp;
mod persistence;
mod skill;
mod state;
mod sync;
mod worktree;

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;

fn main() {
    match cli::run() {
        Ok(()) => {}
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}
