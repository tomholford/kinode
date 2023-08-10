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
    println!("cargo:rerun-if-changed=process-manager/src");
    println!("cargo:rerun-if-changed=terminal/src");
    println!("cargo:rerun-if-changed=http-bindings/src");
    println!("cargo:rerun-if-changed=http-proxy/src");
    println!("cargo:rerun-if-changed=file-transfer/src");
    println!("cargo:rerun-if-changed=apps-home/src");
    println!("cargo:rerun-if-changed=sequencer/src");
    let pwd = std::env::current_dir().unwrap();
    const APPS: [&str; 7] = [
        "process-manager",
        "terminal",
        "http-bindings",
        "http-proxy",
        "file-transfer",
        "apps-home",
        "sequencer",
    ];
    for name in APPS {
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
