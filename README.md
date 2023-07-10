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
# Terminal A
cargo r tuna
!card tuna {"from": "earth", "to": "poast", "data": "hello from tuna terminal"}

# Terminal B while A is still running
cargo r dolph
!card tuna {"from": "earth", "to": "poast", "data": "hello from dolph terminal"}
```
