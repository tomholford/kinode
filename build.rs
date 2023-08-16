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

    // Tell Cargo that if the given file changes, to rerun this build script.
    const APPS: [&str; 10] = [
        "apps_home",
        "file_transfer",
        "file_transfer_one_off",
        "hi_lus_lus",
        "http_bindings",
        "http_proxy",
        "process_manager",
        "terminal",
        "sequencer",
        "sequentialize",
    ];
    for name in APPS {
        // only execute if one of the modules has source code changes
        println!("cargo:rerun-if-changed=modules/{}/src", name);
    }
    let pwd = std::env::current_dir().unwrap();
    let wit_file = fs::read_to_string("wit/process.wit").unwrap();
    for name in APPS {
        // copy in the wit file
        fs::write(
            format!("{}/modules/{}/wit", pwd.display(), name),
            wit_file.clone(),
        ).unwrap();
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
