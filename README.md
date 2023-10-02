# operationOJ
Last updated: 9/29/23
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
# OPTIONAL: --release flag
cargo +nightly build

# To build all of the apps
# OPTIONAL: --release flag
./build.sh --all

# Create the home directory for your node
# If you boot multiple nodes, make sure each has their own home directory.
mkdir home
```

### Boot

Before booting, compile all the apps with `./build.sh` (this may take some time). Then, booting your node takes one argument: the home directory where all your files will be stored. You can also use your own custom eth-rpc URL using: `--rpc` (NOTE: must begin with wss:// NOT https://). You can also include the `--release` if you want optimized performance.
```bash
./build --all
cargo +nightly run home
```

On boot you will be prompted to navigate to `localhost:8000`. Make sure your eth wallet is connected to the Sepolia test network. Login should be very straightforward, just submit the transactions and follow the flow.

### Development
Running `./build.sh` will automatically build any apps that have changes in git. Developing with `./build.sh && cargo +nightly run home` anytime you make a change to your app should be very fast. You can also manually recompile just your app with `./build-app chess` - if your app was named "chess" for example. Sometimes you may need to run `./build.sh --all` if something got out of whack.

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
