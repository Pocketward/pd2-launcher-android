//! Native PD2 Launcher entry point.

#![forbid(unsafe_code)]

use std::{
    ffi::{OsStr, OsString},
    process::ExitCode,
};

const PACKAGE_IDENTITY_PROBE: &str = "--verify-package-identity";

fn main() -> ExitCode {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.first().map(OsString::as_os_str) == Some(OsStr::new(PACKAGE_IDENTITY_PROBE)) {
        if arguments.len() != 1 {
            eprintln!("Unverified");
            return ExitCode::from(2);
        }
        return match pd2_launcher::package_identity_probe() {
            Some(identity) => {
                println!("{identity}");
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("Unverified");
                ExitCode::FAILURE
            }
        };
    }

    match pd2_launcher::run_with_arguments(arguments) {
        Ok(()) => ExitCode::from(pd2_launcher::exit_status()),
        Err(error) => {
            eprintln!("{error}\n\nRun pd2-launcher --help for usage.");
            ExitCode::from(2)
        }
    }
}
