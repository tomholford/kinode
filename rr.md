## Uqbar: Processes

### Overview

In our system, processes are the building blocks for peer-to-peer applications. The Uqbar kernel is a microkernel. It exlusively handles message-passing between `processes`, plus the startup and teardown of said processes. The following describes the message design as it relates to processes. Processes are spawned with a unique identifier, which can either be a string or an auto-generated UUID.

Processes are compiled to WASM. They can be started once and complete immediately, or run forever. They can spawn other processes to do things for them, and coordinate in arbitrarily complex ways by passing messages to one another.

Uqbar provides the 4 basic primitives needed for p2p applications. We've identified and built on these primitives in order to create robust abstractions, cutting away the usual boilerplate and complexity while preserving flexibility. These 4 basic primitives are:

- Networking: passing messages from peer to peer.

- Filesystem: storing data and persisting it forever.

- Global State: reading shared global state (blockchain) and composing actions with it (transactions).

and most importantly,
- Applications: writing and distributing software that runs on privately-held personal server nodes.

Making use of networking is as simple as passing messages to other nodes, as labeled by PQI (what is the PQI? need another article..) username. Using the filesystem can be done by explicit message-passing to the built-in module, or by persisting state in a process. Accessing global state in the form of the Ethereum blockchain is easier than ever before, with chain reads and writes taken care of by state-of-the-art built-in system runtime modules.

As stated above, applications are composed of processes, which hold state and pass messages:

### Process State

Uqbar processes can be stateless or stateful. Statefulness is trivial in the sense that a running process can declate variables and mutate them as desired, while it's still running. But nodes get turned off. The kernel handles booting processes back up that were running previously, but their state is not persisted by default.

Instead, processes elect to persist data when desired. This could be after every message ingested, every X minutes, after a certain specific event, or never. When data is persisted, the kernel saves it to our abstracted filesystem, which not only persists data on disk, but also across arbitrarily many encrypted remote backups as configured at the user-system-level.

If a process persists data, it's simply handed over in a special message at process-start by the kernel.

This design allows for ephemeral state, when desired for performance or pure expediency. It also allows for truly permanent data storage, encrypted across many remote backups, synchronized and safe.

### Requests and Responses

There are two kinds of messages: `requests` and `responses`.

When a request or response is received, it always comes with a source, which includes the name of the node plus the name/ID of the process on that node that produced it. Keep in mind that a process ID given by a remote node cannot be trusted to cohere to any particular logic, given that their kernel could label it as it pleases. Local messages can be trusted insofar as the local kernel code can be trusted.

Requests can be issued at any time by a running process. A request can *optionally* expect a response. If it does, the request will be retained by the kernel, along with an optional `context` object that can be created by the request's issuer. This request will be considered outstanding until the kernel receives a matching response from any process, at which point that response will be delivered to the requester alongside the optional context. Contexts saved by the kernel enable very straightforward, async-style code, avoiding scattered callbacks and lots of ephemeral top-level process state.

If a process receives a request, that doesn't mean it has to directly issue a response. It can issue request(s) that can inherit the context of the incipient request. Developers should keep in mind that dangling requests can occur if a request is received by a process and that process fails to either issue a response or issue a subsidiary request that ultimately produces a response. Dangling requests and their contexts will be thrown away by the kernel if enough of them build up from a single process. (XX this behavior could be system-level or configurable)

Messages, both requests and responses, can contain arbitrary data, which must be interpreted by the process that receives it. The structure of a message contains ample hints about how best to do this.

A message contains a JSON-string used for "IPC"-style typed messages. These are JSON-strings specifically to cross the WASM boundary and be language-agnostic. To achieve composability, a process should be very clear, in code and documentation, about what it expects in this field and how it gets parsed, usually into a language-level struct or object. In the future, the kernel should support even more explicit declaration of this interface, such that developers can assert correctness about structures at compile time.

A message also contains a payload, which is used for opaque or arbitrary bytes. The payload holds bytes alongside a `mime` field for explicit process-and-language-agnostic format declaration, if desired.

Lastly, it contains a `metadata` field to enable middleware processes and the like to manipulate the message without altering the content itself.

In order to allow middleware-style processes to flourish, without impacting performance, a message's payload is *not* automatically loaded into the WASM process when a message is first ingested. The process should look at the typed message and perhaps the source, then call `get_payload()` in order to bring the potentially very large block of data across the WASM boundary. In practice, processes can choose to always bring the payload in if they are dealing with small enough messages, and the standard process library has good affordances for this.

Processes can use exlusively one kind of message, or both.

An example of an IPC-style typed message without payload: a file-transfer app sends a message from a local process to a remote process that issues a "GetFile" command along with a file name.

An example of a payload-only message: a process receives HTTP GET data from the http_server module, and responds with a payload with the MIME type `text/html`. Both of these messages would have a payload that might be adjusted or metadata-tagged with middleware.

It is possible to use both the payload field and the IPC field of a message at the same time. This often happens if a message contains an instruction for a process ("use this payload to assemble a larger data structure") while also containing large amounts of opaque data stored as bytes ("a new chunk loaded in the game-world").

Messages that result in networking failures are returned to the process that created them, as an Error. There are two kinds of networking errors: Offline and Timeout. Offline means the remote target node cannot be reached. Timeout means that the target node is reachable, but the message was not sent within 5 seconds. (THIS NUMBER SUBJECT TO CHANGE, COULD BE UP TO 30)

A network error will give the process the original message along with any payload or context, so the process can handle re-sending, crashing, or otherwise dealing with the failure as it sees fit. If the error comes from a response, the process may send a response again: it will be directed towards the original outstanding request that the failed response was for.