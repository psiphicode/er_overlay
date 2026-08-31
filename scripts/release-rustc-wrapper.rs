use std::env;
use std::ffi::OsString;
use std::process::{self, Command};

const REMAP_VARIABLES: [&str; 2] = [
    "ER_OVERLAY_RELEASE_REMAP_BACKSLASH",
    "ER_OVERLAY_RELEASE_REMAP_FORWARD",
];

fn required_environment_variable(name: &str) -> OsString {
    env::var_os(name).unwrap_or_else(|| {
        eprintln!("release rustc wrapper is missing {name}");
        process::exit(1);
    })
}

fn main() {
    let mut arguments = env::args_os();
    let _wrapper = arguments.next();
    let rustc = arguments.next().unwrap_or_else(|| {
        eprintln!("release rustc wrapper did not receive a compiler path");
        process::exit(1);
    });

    let inner_wrapper =
        env::var_os("ER_OVERLAY_RELEASE_INNER_RUSTC_WRAPPER").filter(|value| !value.is_empty());
    let mut command = if let Some(inner_wrapper) = inner_wrapper {
        let mut command = Command::new(inner_wrapper);
        command.arg(rustc);
        command
    } else {
        Command::new(rustc)
    };
    command.args(arguments);

    for variable in REMAP_VARIABLES {
        let mut flag = OsString::from("--remap-path-prefix=");
        flag.push(required_environment_variable(variable));
        flag.push("=/redacted/user");
        command.arg(flag);
    }

    match command.status() {
        Ok(status) => process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("release rustc wrapper could not start the compiler: {error}");
            process::exit(1);
        }
    }
}
