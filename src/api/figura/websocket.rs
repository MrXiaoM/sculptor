use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::Response,
};
use dashmap::DashMap;
use tracing::{debug, error, info, trace, warn};
use tokio::sync::{
    broadcast::{self, Receiver},
    mpsc, Notify,
};
use uuid::Uuid;

use crate::AppState;
use super::types::{C2SMessage, S2CMessage};

pub async fn handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

#[derive(Debug, Clone)]
struct WSUser {
    username: String,
    uuid: Uuid,
}

trait ExtWSUser {
    fn name(&self) -> String;
}

impl ExtWSUser for Option<WSUser> {
    fn name(&self) -> String {
        if let Some(user) = self {
            format!(" ({})", user.username)
        } else {
            String::new()
        }
    }
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    debug!("[WebSocket] New unknown connection!");
    let mut owner: Option<WSUser> = None; // Information about user
    let cutoff: DashMap<Uuid, Arc<Notify>> = DashMap::new(); // Отключение подписки
    let (mtx, mut mrx) = mpsc::channel(64); // multiple tx and single receive
    let mut bctx: Option<broadcast::Sender<Vec<u8>>> = None; // broadcast tx send
    loop {
        tokio::select! {
            // Main loop what receving messages from WebSocket
            Some(msg) = socket.recv() => {
                trace!("[WebSocket{}] Raw: {msg:?}", owner.name());
                let mut msg = if let Ok(msg) = msg {
                    if let Message::Close(_) = msg {
                        info!("[WebSocket{}] Connection successfully closed!", owner.name());
                        break;
                    }
                    msg
                } else {
                    debug!("[WebSocket{}] Receive error! Connection terminated!", owner.name());
                    break;
                };
                // Checking ban list
                if let Some(ref user) = owner {
                    if state.user_manager.is_banned(&user.uuid) {
                        warn!("[WebSocket] Detected banned user with active WebSocket! Sending close with Banned code.");
                        let _ = socket.send(Message::Binary(S2CMessage::Toast(2, "You're banned!", None).to_vec())).await; // option слищком жирный Some("Reason: Lorum Ipsum interсно сколько влезет~~~ 0w0.")
                        tokio::time::sleep(std::time::Duration::from_secs(6)).await;
                        debug!("{:?}", socket.send(Message::Close(Some(axum::extract::ws::CloseFrame { code: 4001, reason: "You're banned!".into() }))).await);
                        continue;
                    }
                }
                // Next is the code for processing msg
                let msg_vec = msg.clone().into_data();
                let msg_array = msg_vec.as_slice();
                
                if msg_array.len() == 0 { tracing::debug!("[WebSocket{}] Deprecated len 0 msg", owner.name()); continue; };

                let newmsg = match C2SMessage::try_from(msg_array) {
                    Ok(data) => data,
                    Err(e) => {
                        error!("[WebSocket{}] This message is not from Figura! {}", owner.name(), e.to_string());
                        debug!("[WebSocket{}] Broken data: {}", owner.name(), hex::encode(msg_vec));
                        continue;
                        // break;
                    },
                };

                debug!("[WebSocket{}] MSG: {:?}, HEX: {}", owner.name(), newmsg, hex::encode(newmsg.to_vec()));

                match newmsg {
                    C2SMessage::Token(token) => {
                        trace!("[WebSocket{}] C2S : Token", owner.name());
                        let token = String::from_utf8(token.to_vec()).unwrap();
                        match state.user_manager.get(&token) { // The principle is simple: if there is no token in authenticated, then it's "dirty hacker" :D
                            Some(t) => {
                                //username = t.username.clone();
                                owner = Some(WSUser { username: t.username.clone(), uuid: t.uuid });
                                state.session.insert(t.uuid, mtx.clone());
                                msg = Message::Binary(S2CMessage::Auth.to_vec());
                                match state.broadcasts.get(&t.uuid) {
                                    Some(tx) => {
                                        bctx = Some(tx.to_owned());
                                    },
                                    None => {
                                        let (tx, _rx) = broadcast::channel(64);
                                        state.broadcasts.insert(t.uuid, tx.clone());
                                        bctx = Some(tx.to_owned());
                                    },
                                };
                            },
                            None => {
                                warn!("[WebSocket] Authentication error! Sending close with Re-auth code.");
                                debug!("[WebSocket] Tried to log in with {token}"); // Tried to log in with token: {token}
                                debug!("{:?}", socket.send(Message::Close(Some(axum::extract::ws::CloseFrame { code: 4000, reason: "Re-auth".into() }))).await);
                                continue;
                            },
                        };
                    },
                    C2SMessage::Ping(_, _, _) => {
                        trace!("[WebSocket{}] C2S : Ping", owner.name());
                        let data = into_s2c_ping(msg_vec, owner.clone().unwrap().uuid);
                        match bctx.clone().unwrap().send(data) {
                            Ok(_) => (),
                            Err(_) => debug!("[WebSocket{}] Failed to send Ping! Maybe there's no one to send", owner.name()),
                        };
                        continue;
                    },
                    // Subscribing
                    C2SMessage::Sub(uuid) => { // TODO: Eliminate the possibility of using SUB without authentication
                        trace!("[WebSocket{}] C2S : Sub", owner.name());
                        // Ignoring self Sub
                        if uuid == owner.clone().unwrap().uuid {
                            continue;
                        };

                        let rx = match state.broadcasts.get(&uuid) { // Get sender
                            Some(rx) => rx.to_owned().subscribe(), // Subscribe on sender to get receiver
                            None => {
                                warn!("[WebSocket{}] Attention! The required UUID for subscription was not found!", owner.name());
                                let (tx, rx) = broadcast::channel(64); // Pre creating broadcast for future
                                state.broadcasts.insert(uuid, tx); // Inserting into dashmap
                                rx
                            },
                        };

                        let shutdown = Arc::new(Notify::new()); // Creating new shutdown <Notify>
                        tokio::spawn(subscribe(mtx.clone(), rx, shutdown.clone())); // <For send pings to >
                        cutoff.insert(uuid, shutdown); 
                        continue;
                    },
                    // Unsubscribing
                    C2SMessage::Unsub(uuid) => {
                        trace!("[WebSocket{}] C2S : Unsub", owner.name());
                        // Ignoring self Unsub
                        if uuid == owner.clone().unwrap().uuid {
                            continue;
                        };

                        let shutdown = cutoff.remove(&uuid).unwrap().1; // Getting <Notify> from list // FIXME: UNWRAP PANIC! NONE VALUE
                        shutdown.notify_one(); // Shutdown <subscribe> function
                        continue;
                    },
                }

                // Sending message
                debug!("[WebSocket{}] Answering: {msg:?}", owner.name());
                if socket.send(msg).await.is_err() {
                    warn!("[WebSocket{}] Send error! Connection terminated!", owner.name());
                    break;
                }
            }
            msg = mrx.recv() => {
                match socket.send(Message::Binary(msg.clone().unwrap())).await {
                    Ok(_) => {
                        debug!("[WebSocketSubscribe{}] Answering: {}", owner.name(), hex::encode(msg.unwrap()));
                    }
                    Err(_) => {
                        warn!("[WebSocketSubscriber{}] Send error! Connection terminated!", owner.name());
                        break;
                    }
                }
            }
        }
    }
    // Closing connection
    if let Some(u) = owner {
        debug!("[WebSocket ({})] Removing session data", u.username);
        state.session.remove(&u.uuid); // FIXME: Temporary solution
        // state.broadcasts.remove(&u.uuid); // NOTE: Create broadcasts manager ??
        state.user_manager.remove(&u.uuid);
    } else {
        debug!("[WebSocket] Nothing to remove");
    }
}

async fn subscribe(
    socket: mpsc::Sender<Vec<u8>>,
    mut rx: Receiver<Vec<u8>>,
    shutdown: Arc<Notify>,
) {
    loop {
        tokio::select! {
            _ = shutdown.notified() => {
                debug!("SUB successfully closed!");
                return;
            }
            msg = rx.recv() => {
                let msg = msg.ok();

                if let Some(msg) = msg {
                    if socket.send(msg.clone()).await.is_err() {
                        debug!("Forced shutdown SUB! Client died?");
                        return;
                    };
                } else {
                    debug!("Forced shutdown SUB! Source died?");
                    return;
                }
            }
        }
    }
}

fn into_s2c_ping(buf: Vec<u8>, uuid: Uuid) -> Vec<u8> {
    use std::iter::once;
    once(1)
        .chain(uuid.into_bytes().iter().copied())
        .chain(buf.as_slice()[1..].iter().copied())
        .collect()
}
