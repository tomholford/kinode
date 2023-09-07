use std::{io, fs};
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
    if std::env::var("SKIP_BUILD_SCRIPT").is_ok() {
        println!("Skipping build script");
        return;
    }

    let pwd = std::env::current_dir().unwrap();

    // Build wasm32-wasi apps.
    const WASI_APPS: [&str; 3] = [
        "terminal",
        "sequentialize",
        "key_value",
    ];
    for name in WASI_APPS {
        // only execute if one of the modules has source code changes
        println!("cargo:rerun-if-changed=modules/{}/src", name);
    }
    for name in WASI_APPS {
        // copy in the wit files
        run_command(Command::new("cp").args(&[
            "-r",
            "wit",
            &format!("{}/modules/{}", pwd.display(), name),
        ]))
            .unwrap();

        fs::create_dir_all(&format!("{}/modules/{}/target/wasm32-unknown-unknown/release", pwd.display(), name))
            .unwrap();

        // build the module
        run_command(Command::new("cargo").args(&[
            "build",
            "--release",
            "--no-default-features",
            &format!("--manifest-path={}/modules/{}/Cargo.toml", pwd.display(), name),
            "--target",
            "wasm32-wasi",
        ]))
            .unwrap();

        //  adapt module to component with adaptor
        run_command(Command::new("wasm-tools").args(&[
            "component",
            "new",
            &format!("{}/modules/{}/target/wasm32-wasi/release/{}.wasm", pwd.display(), name, name),
            "-o",
            &format!("{}/modules/{}/target/wasm32-wasi/release/{}_adapted.wasm", pwd.display(), name, name),
            "--adapt",
            &format!("{}/wasi_snapshot_preview1.wasm", pwd.display()),
        ]))
            .unwrap();

        //  put wit into component & place where boot sequence expects to find it
        run_command(Command::new("wasm-tools").args(&[
            "component",
            "embed",
            "wit",
            "--world",
            "uq-process",
            &format!("{}/modules/{}/target/wasm32-wasi/release/{}_adapted.wasm", pwd.display(), name, name),
            "-o",
            &format!("{}/modules/{}/target/wasm32-unknown-unknown/release/{}.wasm", pwd.display(), name, name),
        ]))
            .unwrap();
    }
}
