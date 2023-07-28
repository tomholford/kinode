use std::process::Command;

fn main() {
    // Tell Cargo that if the given file changes, to rerun this build script.
    println!("cargo:rerun-if-changed=process-manager/src");
    println!("cargo:rerun-if-changed=terminal/src");
    println!("cargo:rerun-if-changed=http-bindings/src");
    let pwd = std::env::current_dir().unwrap();
    Command::new("cargo")
        .args(&["component", "build", &format!("--manifest-path={}/process-manager/Cargo.toml", pwd.display()), "--target", "wasm32-unknown-unknown"])
        .status()
        .unwrap();
    Command::new("cargo")
        .args(&["component", "build", &format!("--manifest-path={}/terminal/Cargo.toml", pwd.display()), "--target", "wasm32-unknown-unknown"])
        .status()
        .unwrap();
    Command::new("cargo")
        .args(&["component", "build", &format!("--manifest-path={}/http-bindings/Cargo.toml", pwd.display()), "--target", "wasm32-unknown-unknown"])
        .status()
        .unwrap();
}
