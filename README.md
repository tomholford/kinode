# operationOJ

## Setup

### Building components

```bash
# Clone the repo.

git clone git@github.com:uqbar-dao/operationOJ.git

# Get some stuff so we can build wasm.

rustup target add wasm32-unknown-unknown
cargo install cargo-wasi
cargo install --git https://github.com/bytecodealliance/cargo-component --locked

# Build the components.

cd http-server
cargo component build --target wasm32-unknown-unknown
cd ..
cd poast
cargo component build --target wasm32-unknown-unknown
cd ..
cd hi-lus-lus
cargo component build --target wasm32-unknown-unknown
cd ..
```

### Terminal

- look at `blockchain.json` to see what identities are available to you
- `squid` is running on the uqbar devnet server, `loach` is what i use for local testing
- pick a name, or add your own
- run `cargo r <yourname>` to start the server

## Current commands

- `!card <name> <json>`: send a card with a JSON value to another server or yourself
- `!quit`, `!exit`: kill the server

## Example usage

```bash
# Create tuna and dolph home directories, and populate them:
mkdir home
mkdir home/tuna
mkdir home/dolph
cp hi-lus-lus/target/wasm32-unknown-unknown/debug/hi_lus_lus.wasm home/tuna
cp hi-lus-lus/target/wasm32-unknown-unknown/debug/hi_lus_lus.wasm home/dolph

# Terminal A: add hi++ apps to process_manager
cargo r process_manager.wasm home/tuna tuna
!message tuna process_manager {"type": "Start", "process_name": "hi_lus_lus", "wasm_bytes_uri": "fs://home/tuna/hi_lus_lus.wasm"}

# Terminal B: While A is still running add hi++ to process_manager
cargo r process_manager.wasm home/dolph dolph
!message dolph process_manager {"type": "Start", "process_name": "hi_lus_lus", "wasm_bytes_uri": "fs://home/dolph/hi_lus_lus.wasm"}

# Terminal B: Send a message using hi++ from Terminal B to A:
!message dolph hi_lus_lus {"target": "tuna", "action": "send", "contents": "hello from dolph"}

# Terminal A: Send a message back from A to B using hi++:
!message tuna hi_lus_lus {"target": "dolph", "action": "send", "contents": "hello from tuna"}

# Terminal A: Stopping a process means messages will no longer work:
!message tuna process_manager {"type": "Stop", "process_name": "hi_lus_lus"}
!message tuna hi_lus_lus {"target": "dolph", "action": "send", "contents": "hello from tuna"}

# Terminal A: However, restarting a process will reset its state and messages will work since the process is running again:
!message tuna process_manager {"type": "Start", "process_name": "hi_lus_lus", "wasm_bytes_uri": "fs://home/tuna/hi_lus_lus.wasm"}
!message tuna process_manager {"type": "Restart", "process_name": "hi_lus_lus"}
!message tuna hi_lus_lus {"target": "dolph", "action": "send", "contents": "hello from tuna"}
```
