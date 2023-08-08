use std::process::Command;
use std::io;

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
    let pwd = std::env::current_dir().unwrap();
    run_command(
        Command::new("cargo")
            .args(&["component", "build", &format!("--manifest-path={}/process-manager/Cargo.toml", pwd.display()), "--target", "wasm32-unknown-unknown"])
    ).unwrap();
    run_command(
        Command::new("cargo")
            .args(&["component", "build", &format!("--manifest-path={}/terminal/Cargo.toml", pwd.display()), "--target", "wasm32-unknown-unknown"])
    ).unwrap();
    run_command(
        Command::new("cargo")
            .args(&["component", "build", &format!("--manifest-path={}/http-bindings/Cargo.toml", pwd.display()), "--target", "wasm32-unknown-unknown"])
    ).unwrap();
    run_command(
        Command::new("cargo")
            .args(&["component", "build", &format!("--manifest-path={}/http-proxy/Cargo.toml", pwd.display()), "--target", "wasm32-unknown-unknown"])
    ).unwrap();
    run_command(
        Command::new("cargo")
            .args(&["component", "build", &format!("--manifest-path={}/file-transfer/Cargo.toml", pwd.display()), "--target", "wasm32-unknown-unknown"])
    ).unwrap();
    run_command(
      Command::new("cargo")
            .args(&["component", "build", &format!("--manifest-path={}/apps-home/Cargo.toml", pwd.display()), "--target", "wasm32-unknown-unknown"])
    ).unwrap();
}
