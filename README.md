# operationOJ

## Setup

### Building components

```bash
# Clone the repo.

git clone git@github.com:uqbar-dao/operationOJ.git

# Get some stuff so we can build wasm.

rustup target add wasm32-unknown-unknown
cargo install cargo-wasi
cargo install --git https://github.com/bytecodealliance/cargo-component --rev 84ad1dc

# Build the runtime, along with 3 booted-at-startup WASM modules: process-manager, terminal, and http-bindings
cargo build

# Create the home directory for your node
# If you boot multiple nodes, make sure each has their own home directory.
mkdir home
```

If desired, build additional components and copy them into your node's home directory like so. Available components to build:
- file-transfer
- hi-lus-lus
- poast
- sequencer

```bash
cd file-transfer
cargo component build --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/debug/file-transfer.wasm ../home/file-transfer.wasm
```
Replace `file-transfer` with the desired component.

### Boot

Boot takes 3 arguments: the desired process manager, the home directory, and the name of the node. You can just use `process_manager.wasm`, which will already be built. Use the home directory you created previously and select a name for the node. This name argument will soon be eliminated and replaced with the login page.
```bash
cargo run process_manager.wasm home your_name
```

Now that the node has started, look to the example usage section below to see what kind of commands are available.

### Terminal syntax

- CTRL+C to kill node
- CTRL+V to toggle verbose mode, which is on by default
- CTRL+D to toggle debug mode
- CTRL+S to step through events in debug mode

- `!message <name> <app> <json>`: send a card with a JSON value to another node or yourself
- more to come

## Example usage

### Using the file-transfer app

```bash
# Create tuna and dolph home directories, and populate them:
mkdir home
mkdir home/tuna
mkdir home/dolph
mkdir home/dolph/file_transfer
cp hi-lus-lus/target/wasm32-unknown-unknown/debug/hi_lus_lus.wasm home/tuna/
cp hi-lus-lus/target/wasm32-unknown-unknown/debug/hi_lus_lus.wasm home/dolph/
cp file-transfer/target/wasm32-unknown-unknown/debug/file_transfer.wasm home/tuna/
cp file-transfer/target/wasm32-unknown-unknown/debug/file_transfer.wasm home/dolph/
cp README.md home/dolph/file_transfer/

# Terminal A: add hi++ apps to process_manager
cargo r process_manager.wasm home/tuna tuna
!message tuna process_manager {"type": "Start", "process_name": "hi_lus_lus", "wasm_bytes_uri": "fs://hi_lus_lus.wasm"}
!message tuna process_manager {"type": "Start", "process_name": "file_transfer", "wasm_bytes_uri": "fs://file_transfer.wasm"}

# Terminal B: While A is still running add hi++ to process_manager
cargo r process_manager.wasm home/dolph dolph
!message dolph process_manager {"type": "Start", "process_name": "hi_lus_lus", "wasm_bytes_uri": "fs://hi_lus_lus.wasm"}
!message dolph process_manager {"type": "Start", "process_name": "file_transfer", "wasm_bytes_uri": "fs://file_transfer.wasm"}

# Terminal B: Send a message using hi++ from Terminal B to A:
!message dolph hi_lus_lus {"target": "tuna", "action": "send", "contents": "hello from dolph"}

# Terminal A: Send a message back from A to B using hi++:
!message tuna hi_lus_lus {"target": "dolph", "action": "send", "contents": "hello from tuna"}

# Terminal A: get a file from B using file_transfer:
!message tuna file_transfer {"type": "GetFile", "target_ship": "dolph", "uri_string": "fs://README.md", "chunk_size": 1024}
!message tuna file_transfer {"type": "DisplayOngoing"}

# Terminal A: Stopping a process means messages will no longer work:
!message tuna process_manager {"type": "Stop", "process_name": "hi_lus_lus"}
!message tuna hi_lus_lus {"target": "dolph", "action": "send", "contents": "hello from tuna"}

# Terminal A: However, restarting a process will reset its state and messages will work since the process is running again:
!message tuna process_manager {"type": "Start", "process_name": "hi_lus_lus", "wasm_bytes_uri": "fs://home/tuna/hi_lus_lus.wasm"}
!message tuna process_manager {"type": "Restart", "process_name": "hi_lus_lus"}
!message tuna hi_lus_lus {"target": "dolph", "action": "send", "contents": "hello from tuna"}

!message tuna process_manager {"type": "Restart", "process_name": "file_transfer"}
!message dolph process_manager {"type": "Restart", "process_name": "file_transfer"}
```

### Using `http-server` with an app

After booting poast using the commands above, run
Make sure to boot both the http-bindings app and poast
```bash
cargo r process_manager.wasm tuna
!message tuna process_manager {"type": "Start", "process_name": "poast", "wasm_bytes_uri": "fs://poast.wasm"}
```
Then try making a GET request in browser to `http://127.0.0.1:8080/poast` or make a POST request as below:
```bash
curl -X POST -H "Content-Type: application/json" -d '{"foo":"bar"}' http://127.0.0.1:8080/poast
```
And both should return responses.
