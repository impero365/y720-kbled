mod cli;
mod client;
mod config;
mod error;
mod hw;
mod paths;
mod proc;
mod proto;
mod service;
mod ui;

fn main() {
    if let Err(error) = cli::run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
