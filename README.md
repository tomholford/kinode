# operationOJ

# demo-0.1 branch

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

Boot takes 3 arguments: the home directory, and the URL of a "blockchain" RPC endpoint. Use the home directory you created previously and select a name for the node. For the third argument, use either a node that you're running locally, or this URL which I (@dr-frmr) will try to keep active 24/7:
```bash
cargo run home http://147.135.114.167:8083/blockchain.json
```

If you want to set up a blockchain node locally, simply set this third argument to anything, as long as you put some string there it will default to the local `blockchain.json` in filesystem. NOTE: this "blockchain" node itself will not network properly yet, because it's not set up to "index" itself. :(

In order to make the "blockchain" node work such as the one I have at the above IP, you need to build `sequencer.wasm` and put it in the node's home directory, as shown in the example with `file-transfer` above. After doing so, use this command to start it up, replacing your_name as necessary:
`!message <<your_name>> process_manager {"type": "Start", "process_name": "sequencer", "wasm_bytes_uri": "fs://sequencer.wasm"}`

You will be prompted to navigate to `localhost:8000/register`. This should appear as a screen to input a username and password. After submitting these and signing the metamask prompt, your node should connect and insert itself to the chain. You can check by going to the URL endpoint served at either that IP or your local "chain" node.

Now that the node has started, look to the example usage section below to see what kind of commands are available.

### Terminal syntax

- CTRL+C or CTRL+D to shutdown node
- CTRL+V to toggle verbose mode, which is on by default
- CTRL+J to toggle debug mode
- CTRL+S to step through events in debug mode

- CTRL+A to jump to beginning of input
- CTRL+E to jump to end of input
- UpArrow/DownArrow or CTRL+P/CTRL+N to move up and down through command history
- CTRL+R to search history, CTRL+R again to toggle through search results, CTRL+G to cancel search

- `!message <name> <app> <json>`: send a card with a JSON value to another node or yourself. <name> can be `our`, which will be interpreted as our node's username.
- `!hi <name> <string>`: send a text message to another node's command line.
- more to come

## Example usage

### Using the file-transfer app

```bash
# Create tuna and dolph home directories, and populate them:
mkdir home
mkdir home/tuna
mkdir home/dolph
mkdir home/dolph/file_transfer
mkdir home/dolph/file_transfer_one_off
cp hi-lus-lus/target/wasm32-unknown-unknown/debug/hi_lus_lus.wasm home/tuna/
cp hi-lus-lus/target/wasm32-unknown-unknown/debug/hi_lus_lus.wasm home/dolph/
cp file-transfer/target/wasm32-unknown-unknown/debug/file_transfer.wasm home/tuna/
cp file-transfer/target/wasm32-unknown-unknown/debug/file_transfer.wasm home/dolph/
cp file-transfer-one-off/target/wasm32-unknown-unknown/debug/file_transfer_one_off.wasm home/tuna/
cp file-transfer-one-off/target/wasm32-unknown-unknown/debug/file_transfer_one_off.wasm home/dolph/
cp README.md home/dolph/file_transfer/
cp README.md home/dolph/file_transfer_one_off/

# For releases:
cp hi-lus-lus/target/wasm32-unknown-unknown/release/hi_lus_lus.wasm home/tuna/
cp hi-lus-lus/target/wasm32-unknown-unknown/release/hi_lus_lus.wasm home/dolph/
cp file-transfer/target/wasm32-unknown-unknown/release/file_transfer.wasm home/tuna/
cp file-transfer/target/wasm32-unknown-unknown/release/file_transfer.wasm home/dolph/
cp file-transfer-one-off/target/wasm32-unknown-unknown/release/file_transfer_one_off.wasm home/tuna/
cp file-transfer-one-off/target/wasm32-unknown-unknown/release/file_transfer_one_off.wasm home/dolph/

# Terminal A: add hi++ apps to process_manager
cargo r home/tuna tuna
!message tuna process_manager {"type": "Start", "process_name": "hi_lus_lus", "wasm_bytes_uri": "fs://hi_lus_lus.wasm"}
!message tuna process_manager {"type": "Start", "process_name": "file_transfer", "wasm_bytes_uri": "fs://file_transfer.wasm"}
!message tuna process_manager {"type": "Start", "process_name": "file_transfer_one_off", "wasm_bytes_uri": "fs://file_transfer_one_off.wasm"}

# Terminal B: While A is still running add hi++ to process_manager
cargo r home/dolph dolph
!message dolph process_manager {"type": "Start", "process_name": "hi_lus_lus", "wasm_bytes_uri": "fs://hi_lus_lus.wasm"}
!message dolph process_manager {"type": "Start", "process_name": "file_transfer", "wasm_bytes_uri": "fs://file_transfer.wasm"}
!message dolph process_manager {"type": "Start", "process_name": "file_transfer_one_off", "wasm_bytes_uri": "fs://file_transfer_one_off.wasm"}

# Terminal B: Send a message using hi++ from Terminal B to A:
!message dolph hi_lus_lus {"target": "tuna", "action": "send", "contents": "hello from dolph"}

# Terminal A: Send a message back from A to B using hi++:
!message tuna hi_lus_lus {"target": "dolph", "action": "send", "contents": "hello from tuna"}

# Terminal A: get a file from B using file_transfer:
!message tuna file_transfer {"type": "GetFile", "target_ship": "dolph", "uri_string": "fs://README.md", "chunk_size": 1024}
!message tuna file_transfer_one_off {"type": "GetFile", "target_ship": "dolph", "uri_string": "fs://README.md", "chunk_size": 1024}
!message tuna file_transfer {"type": "DisplayOngoing"}
!message tuna file_transfer {"type": "ReadDir", "target_node": "dolph", "uri_string": "fs://."}

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
