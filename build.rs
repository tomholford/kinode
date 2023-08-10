use std::io;
use std::process::Command;

fn run_command(cmd: &mut Command) -> io::Result<()> {
    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::Other, "Command failed"))
    }
}

fn main() {
    // Tell Cargo that if the given file changes, to rerun this build script.
    const APPS: [&str; 9] = [
        "apps_home",
        "file_transfer",
        "file_transfer_one_off",
        "hi_lus_lus",
        "http_bindings",
        "http_proxy",
        "process_manager",
        "terminal",
        "sequencer",
    ];
    for name in APPS {
        // only execute if one of the modules has source code changes
        println!("cargo:rerun-if-changed=modules/{}/src", name);
    }
    let pwd = std::env::current_dir().unwrap();
    for name in APPS {
        // symlink the wit file
        let _ = run_command(Command::new("ln").args(&[
            "-s",
            &format!("{}/wit/process.wit", pwd.display()),
            &format!("{}/modules/{}/wit", pwd.display(), name),
        ]));
        // build the component
        run_command(Command::new("cargo").args(&[
            "component",
            "build",
            "--release",
            &format!("--manifest-path={}/modules/{}/Cargo.toml", pwd.display(), name),
            "--target",
            "wasm32-unknown-unknown",
        ]))
        .unwrap();
    }
    run_command(
        Command::new("cargo")
            .current_dir("boot_sequence") // Change to boot_sequence directory
            .arg("run"),
    )
    .unwrap();
}
