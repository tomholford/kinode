# operationOJ

setup:
- look at `blockchain.json` to see what identities are available to you
- `squid` is running on the uqbar devnet server, `loach` is what i use for local testing
- pick a name, or add your own
- run `cargo r <yourname>` to start the server

current commands available:
- `!card <name> <json>`: send a card with a JSON value to another server or yourself
- `!quit`, `!exit`: kill the server

use `!card <yourname> "ShowPeers"` to ask the kernel to display your current peer-set
