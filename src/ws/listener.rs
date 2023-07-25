use crate::types::*;
use crate::ws::*;
use ring::signature::Ed25519KeyPair;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::{self};
use tokio_tungstenite::MaybeTlsStream;

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
    let pass_throughs: PassThroughs = Arc::new(RwLock::new(HashMap::new()));

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
                    pass_throughs.clone(),
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
    pass_throughs: PassThroughs,
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
                pass_throughs.clone(),
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
                if target_id.ws_routing.is_none()
                    && target_id.allowed_routers.contains(&our.name)
                    && peers.read().await.contains_key(&handshake.target)
                {
                    // ok, we can route to them!
                    let mut pt_writer = pass_throughs.write().await;
                    match pt_writer.get_mut(&target_id.name) {
                        None => return Err("target not routable".into()),
                        Some(map) => map.insert(their_id.name.clone(), write_stream),
                    };

                    // spawn a new one-way pass-through
                    tokio::spawn(one_way_pass_through_connection(
                        handshake.target.clone(),
                        their_id.name,
                        read_stream,
                        peers.clone(),
                        Some(handshake),
                    ));

                    Ok(())
                } else {
                    return Err("target not routable".into());
                }
            }
            None => return Err("target not found in onchain pki".into()),
        }
    }
}
