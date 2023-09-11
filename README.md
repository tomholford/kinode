# operationOJ

## Setup

### Building components

```bash
# Clone the repo.

git clone git@github.com:uqbar-dao/operationOJ.git

# Get some stuff so we can build wasm.

cargo install wasm-tools
rustup install nightly
rustup target add wasm32-wasi
rustup target add wasm32-wasi --toolchain nightly
cargo install cargo-wasi
cargo install --git https://github.com/bytecodealliance/cargo-component --locked cargo-component

# Build the runtime, along with a number of booted-at-startup WASM modules including terminal and key_value
cargo +nightly build --release

# Create the home directory for your node
# If you boot multiple nodes, make sure each has their own home directory.
mkdir home
```

### Boot

Boot takes 2 arguments: the home directory, and the URL of a "blockchain" RPC endpoint. Use the home directory you created previously and select a name for the node. For the second argument, use either a node that you're running locally, or this URL which I (@dr-frmr) will try to keep active 24/7:
```bash
cargo +nightly run --release home http://147.135.114.167:8083/blockchain.json
```
There is also a third optional argument `--bs boot_sequence.bin` if you want to add a custom boot sequence - see [here](./boot_sequence/README.md) for details on how to make a custom one.

Note that the `--release` flag is optional but should normally be included to enable better performance (while only adding a few seconds to build time).

If you want to set up a blockchain node locally, simply set the second argument to anything, as long as you put some string there it will default to the local `blockchain.json` in filesystem. NOTE: this "blockchain" node itself will not network properly yet, because it's not set up to "index" itself. :(

In order to make the "blockchain" node work such as the one I have at the above IP, you need to build `sequencer.wasm` and put it in the node's home directory, as shown in the example with `file-transfer` above. After doing so, use this command to start it up:
`!message our kernel {"type": "StartProcess", "name": "sequencer", "wasm_bytes_uri": "fs://sequencer.wasm", "on_panic": null}`

You will be prompted to navigate to `localhost:8000/register`. This should appear as a screen to input a username and password. After submitting these and signing the metamask prompt, your node should connect and insert itself to the chain. You can check by going to the URL endpoint served at either that IP or your local "chain" node.

Now that the node has started, look to the example usage section below to see what kind of commands are available.

## Terminal syntax

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
- `<name>` is either the name of a node or `our`, which will fill in the present node name
- more to come

## Example usage

TODO
