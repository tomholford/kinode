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
    const APPS: [&str; 7] = [
        "process_manager",
        "terminal",
        "http_bindings",
        "http_proxy",
        "file_transfer",
        "apps_home",
        "sequencer",
    ];
    let pwd = std::env::current_dir().unwrap();
    for name in APPS {
        println!("cargo:rerun-if-changed={}/src", name);
        run_command(Command::new("cargo").args(&[
            "component",
            "build",
            "--release",
            &format!("--manifest-path={}/{}/Cargo.toml", pwd.display(), name),
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
