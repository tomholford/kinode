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

- `!message <name> <app> <json>`: send a card with a JSON value to another server or yourself
- `!quit`, `!exit`: kill the server

## Example usage
```bash
# Terminal A: add some test apps to process_manager and run a simple test

cargo r process_manager.wasm tuna
!message tuna process_manager {"type": "Start", "process_name": "http_bindings", "wasm_bytes_uri": "fs://http_bindings.wasm"}
!message tuna process_manager {"type": "Start", "process_name": "poast", "wasm_bytes_uri": "fs://poast.wasm"}
!message tuna process_manager {"type": "Start", "process_name": "hi_lus_lus", "wasm_bytes_uri": "fs://hi_lus_lus.wasm"}

# Terminal B: While A is still running, run the same poast command remotely, then add hi++ to process_manager
cargo r process_manager.wasm dolph
!message dolph process_manager {"type": "Start", "process_name": "hi_lus_lus", "wasm_bytes_uri": "fs://hi_lus_lus.wasm"}

# Terminal B: Send a message using hi++ from Terminal B to A:
!message dolph hi_lus_lus {"target": "tuna", "action": "send", "contents": "hello from dolph"}

# Terminal A: Send a message back from A to B using hi++:
!message tuna hi_lus_lus {"target": "dolph", "action": "send", "contents": "hello from tuna"}

# Terminal A: Stopping a process means messages will no longer work:
!message tuna process_manager {"type": "Stop", "process_name": "poast"}
!message tuna poast "hello from tuna terminal"

# Terminal A: However, restarting a process will reset its state and messages will work since the process is running again:
!message tuna process_manager {"type": "Start", "process_name": "poast", "wasm_bytes_uri": "fs://poast.wasm"}
!message tuna process_manager {"type": "Restart", "process_name": "poast"}
!message tuna poast "hello from tuna terminal"
```

## Using `http-server` with an app
After booting poast using the commands above, run
Make sure to boot both the http-bindings app and poast
```bash
cargo r process_manager.wasm tuna
!message tuna process_manager {"type": "Start", "process_name": "http_bindings", "wasm_bytes_uri": "fs://http_bindings.wasm"}
!message tuna process_manager {"type": "Start", "process_name": "poast", "wasm_bytes_uri": "fs://poast.wasm"}
```
Then try making a GET request in browser to `http://127.0.0.1:8080/poast` or make a POST request as below:
```bash
curl -X POST -H "Content-Type: application/json" -d '{"foo":"bar"}' http://127.0.0.1:8080/poast
```
And both should return responses.