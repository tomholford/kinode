use crate::types::*;
use crate::ws::*;
use ring::signature::Ed25519KeyPair;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async; // tungstenite::Result};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{self};
use tokio_tungstenite::MaybeTlsStream;
use url::Url;

/// we always have networking info in Identity
pub async fn ws_listener(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    peers: Peers,
    message_tx: MessageSender,
    _print_tx: PrintSender,
    tcp: TcpListener,
) {
    while let Ok((stream, _socket_addr)) = tcp.accept().await {
        let stream = accept_async(MaybeTlsStream::Plain(stream)).await;
        match stream {
            Ok(stream) => {
                let _closed = create_connection(
                    our.clone(),
                    keypair.clone(),
                    pki.clone(),
                    stream,
                    peers.clone(),
                    message_tx.clone(),
                )
                .await;
                // match closed {
                //     Ok(_) => {}
                //     Err(e) => {
                //         let _ = print_tx.send(format!("connection closed: {}", e)).await;
                //     }
                // }
            }
            Err(_) => {
                // let _ = print_tx.send(format!("connection failed: {}", e)).await;
            }
        }
    }
}

async fn create_connection(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    ws_stream: Sock,
    peers: Peers,
    message_tx: MessageSender,
) -> Result<(), String> {
    let (mut write_stream, mut read_stream) = ws_stream.split();
    // receive handshake, parse handshake
    let handshake: Handshake = get_handshake(&mut read_stream).await?;
    let their_id: Identity = match pki.get(&handshake.from) {
        Some(v) => v.clone(),
        None => return Err("peer not found in onchain pki".into()),
    };
    // verify handshake
    let (their_ephemeral_pk, nonce) =
        validate_handshake(&handshake, &their_id, handshake.nonce.clone())?;

    if handshake.routing_request || handshake.target == our.name {
        // create our handshake
        let (ephemeral_secret, our_handshake) = make_secret_and_handshake(
            &our,
            keypair.clone(),
            their_id.name.clone(),
            Some(nonce.clone()),
            false,
        )?;
        // send our handshake
        let _ = match write_stream
            .send(tungstenite::Message::Text(
                serde_json::to_string(&our_handshake)
                    .map_err(|_| "failed to serialize handshake")?,
            ))
            .await
        {
            Ok(_) => (),
            Err(e) => return Err(format!("failed to send handshake: {}", e)),
        };

        // if handshake.routing_request,
        // hold as a forwarding connection
        if handshake.routing_request {
            // save write stream in peer mapping and start holding connection
            peers.write().await.insert(
                their_id.name.clone(),
                Peer {
                    networking_address: their_id.address,
                    ephemeral_secret: ephemeral_secret.clone(),
                    their_ephemeral_pk: their_ephemeral_pk.clone(),
                    nonce: nonce.clone(),
                    router: Some(our.name.clone()),
                    direct_write_stream: Some(write_stream),
                },
            );

            tokio::spawn(handle_forwarding_connection(
                our.clone(),
                their_id.clone(),
                read_stream,
                ephemeral_secret.clone(),
                their_ephemeral_pk.clone(),
                nonce.clone(),
                message_tx.clone(),
                pki.clone(),
                peers.clone(),
            ));
        } else {
            // else,
            // if handshake.target is us,
            // hold as a direct connection
            // save write stream in peer mapping and start holding connection
            peers.write().await.insert(
                their_id.name.clone(),
                Peer {
                    networking_address: their_id.address,
                    ephemeral_secret: ephemeral_secret.clone(),
                    their_ephemeral_pk: their_ephemeral_pk.clone(),
                    nonce: nonce.clone(),
                    router: None,
                    direct_write_stream: Some(write_stream),
                },
            );

            tokio::spawn(connections::handle_direct_connection(
                their_id,
                read_stream,
                peers.clone(),
                ephemeral_secret.clone(),
                their_ephemeral_pk,
                nonce,
                message_tx.clone(),
            ));
        }
        return Ok(());
    } else {
        // handshake.target is not us.
        // try to make a matching pass-through
        match pki.get(&handshake.target) {
            Some(target_id) => {
                let from_1: Identity = their_id;
                let mut write_stream_1 = write_stream;
                let read_stream_1 = read_stream;
                if target_id.ws_routing.is_some() {
                    // try to connect directly to them and create a pass-through
                    let (ip, port) = target_id.ws_routing.as_ref().unwrap();
                    let ws_url: Url = Url::parse(&format!("ws://{}:{}/ws", ip, port)).unwrap();
                    match connect_async(ws_url).await {
                        Err(_) => {
                            return Err("error: failed to connect to router".into());
                        }
                        Ok((ws_stream_2, _response)) => {
                            let (mut write_stream_2, mut read_stream_2) = ws_stream_2.split();
                            // send handshake on behalf of initiator
                            let _ = match write_stream_2
                                .send(tungstenite::Message::Text(
                                    serde_json::to_string(&handshake)
                                        .map_err(|_| "failed to serialize handshake")?,
                                ))
                                .await
                            {
                                Ok(_) => (),
                                Err(e) => return Err(format!("failed to send handshake: {}", e)),
                            };

                            // get the other handshake
                            let their_handshake: Handshake =
                                get_handshake(&mut read_stream_2).await?;

                            // FORWARD it back to the initiator
                            let _ = match write_stream_1
                                .send(tungstenite::Message::Text(
                                    serde_json::to_string(&their_handshake)
                                        .map_err(|_| "failed to serialize handshake")?,
                                ))
                                .await
                            {
                                Ok(_) => (),
                                Err(e) => return Err(format!("failed to send handshake: {}", e)),
                            };

                            // finally, create the pass-through
                            tokio::spawn(handle_pass_through_connection(
                                from_1,
                                write_stream_1,
                                read_stream_1,
                                target_id.clone(),
                                write_stream_2,
                                read_stream_2,
                                peers.clone(),
                            ));
                            Ok(())
                        }
                    }
                } else if !target_id.allowed_routers.is_empty() {
                    // try to connect to one of their routers and create a pass-through
                    println!("XX");
                    return Ok(());
                } else {
                    return Err("target unreachable via pki".into());
                }
            }
            None => return Err("target not found in onchain pki".into()),
        }
    }
}
