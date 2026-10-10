mod bluez;
mod cli;
mod config;
mod daemon;
mod gui;
mod profile;
mod sound;
mod udev;

fn main() -> std::process::ExitCode {
    cli::main()
}
