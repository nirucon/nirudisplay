//! NIRU Display — graphical monitor settings for the NIRUX desktop.
//!
//! The window edits a draft and uses `niructl display` for every change. It
//! owns no profile store, no xrandr parser and no rollback timer of its own.

mod backend;
mod text;
mod theme;
mod window;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("open") => run_window(),
        Some("status") => print_status(),
        Some("list") => print_list(),
        Some("--version") => {
            println!("NIRUDISPLAY {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help") => {
            usage();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("nirudisplay: unknown command: {other}");
            usage();
            ExitCode::from(2)
        }
    }
}

fn usage() {
    println!("NIRUDISPLAY {}", env!("CARGO_PKG_VERSION"));
    println!("Usage: nirudisplay [open]");
    println!("       nirudisplay status   print connected screens and profiles");
    println!("       nirudisplay list     print saved display profiles");
    println!("       nirudisplay --version | --help");
    println!("       open the window to move screens, choose modes/rates and preview layouts");
}

fn print_status() -> ExitCode {
    match backend::status_text() {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("nirudisplay: {err}");
            ExitCode::FAILURE
        }
    }
}

fn print_list() -> ExitCode {
    match backend::read_status() {
        Ok(status) => {
            for profile in status.profiles {
                println!("{}\t{}\toutputs", profile.name, profile.outputs.len());
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("nirudisplay: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run_window() -> ExitCode {
    let mut state = match window::State::load() {
        Ok(state) => state,
        Err(err) => {
            eprintln!("nirudisplay: {err}");
            return ExitCode::FAILURE;
        }
    };
    let mut app = match window::App::open() {
        Ok(app) => app,
        Err(err) => {
            eprintln!("nirudisplay: cannot open a window: {err}");
            return ExitCode::FAILURE;
        }
    };
    match window::run(&mut app, &mut state) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("nirudisplay: {err}");
            ExitCode::FAILURE
        }
    }
}
