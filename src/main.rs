mod cli;
mod config;
mod error;
mod fetcher;
mod harnesses;
mod linker;
mod state;

fn main() {
    match cli::run() {
        Ok(()) => {}
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}
