use futures_util::{ stream::FusedStream, SinkExt, StreamExt };
use serde_json::{ json, Value };
use tokio::{ net::TcpStream, select, sync::mpsc::{ self, UnboundedSender }, task::JoinHandle };
use tokio_tungstenite::{ accept_async, tungstenite::{ self, client, WebSocket }, WebSocketStream };
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio::sync::{ Mutex };
use std::{ collections::HashMap, sync::Arc };
use serde::{ Serialize, Deserialize };
use uuid::Uuid;

use crate::server::{ GameBegin, GameEnd };

use super::{ ClientErr, Score, ServerError, SessionErr };

type Socket = Arc<Mutex<WebSocketStream<TcpStream>>>;

struct Client {
    conn: Socket,
    score: i32, // java 只有 有符号整数，因此我们只用有符号整数就行了
    thread: JoinHandle<Result<(), ClientErr>>, // 线程句柄，返回 ()
}

/// 这里保存的是 session 的状态
struct Session {
    ssid: Uuid,
    clients: Arc<Mutex<(Client, Client)>>,
    thread: JoinHandle<Result<(), SessionErr>>,
}

impl Client {
    /// 主要是管道，专门用来接收的
    async fn handle_client(
        conn: Socket,
        score_tx: UnboundedSender<i32>,
        end_tx: UnboundedSender<i32>,
        disconnect_tx: UnboundedSender<bool>
    ) -> Result<(), ClientErr> {
        while let Some(msg) = conn.lock().await.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    let value: Value = serde_json::from_str(&text)?;

                    if value.get("end").is_some() {
                        let game_end: GameEnd = serde_json::from_value(value.clone())?;
                        dbg!(&game_end);
                        end_tx.send(game_end.score)?;
                        break;
                    } else if value.get("value").is_some() {
                        let score: Score = serde_json::from_value(value.clone())?;
                        score_tx.send(score.value)?;
                        // dbg!(&score);
                    } else {
                        /*else if value.get("begin").is_some() {
                        let game_begin: GameBegin = serde_json::from_value(value.clone())?;
                        dbg!(&game_begin);
                        // Do something with GameBegin if necessary
                    } */ dbg!("Unknown message type");
                        dbg!("unknown message type: ", &text);
                    }
                }
                Ok(Message::Close(frame)) => {
                    dbg!(frame);
                    disconnect_tx.send(true)?;
                    return Ok(()); // Should destroy all tx
                }
                Ok(_) => {}
                Err(e) => {
                    dbg!(e);
                    // 垃圾回收
                    return Err(ClientErr::RecvError);
                }
            }
        }
        Ok(())
    }
}

impl Session {
    /// 宣布游戏开始，并创建 session
    async fn broadcast_start(
        conn0: WebSocketStream<TcpStream>,
        conn1: WebSocketStream<TcpStream>
    ) -> Result<(Uuid, Session), ServerError> {
        let ssid = Uuid::new_v4();

        /* ---------- client0 ---------- */
        let conn0 = Arc::new(Mutex::new(conn0));

        let (score_tx0, score_rx0) = mpsc::unbounded_channel();
        let (end_tx0, end_rx0) = mpsc::unbounded_channel();
        let (diconnect_tx0, disconnect_rx0) = mpsc::unbounded_channel();

        let client0 = Client {
            conn: conn0.clone(),
            score: 0,
            thread: tokio::spawn(
                Client::handle_client(conn0.clone(), score_tx0, end_tx0, diconnect_tx0)
            ),
        };

        /* ---------- client1 ---------- */
        let conn1 = Arc::new(Mutex::new(conn1));

        let (score_tx1, score_rx1) = mpsc::unbounded_channel();
        let (end_tx1, end_rx1) = mpsc::unbounded_channel();
        let (diconnect_tx1, disconnect_rx1) = mpsc::unbounded_channel();

        let client1 = Client {
            conn: conn1.clone(),
            score: 0,
            thread: tokio::spawn(
                Client::handle_client(conn1.clone(), score_tx1, end_tx1, diconnect_tx1)
            ),
        };

        let clients = Arc::new(Mutex::new((client0, client1)));

        /* ---------- session 的创建 ---------- */

        let session_thread = {
            let clients = clients.clone();
            tokio::spawn(async move {
                Session::handle_session(
                    clients,
                    score_rx0,
                    score_rx1,
                    end_rx0,
                    end_rx1,
                    disconnect_rx0,
                    disconnect_rx1
                ).await
            })
        };

        let session = Session {
            ssid,
            clients: clients.clone(),
            thread: session_thread,
        };

        /* ---------- 宣布游戏开始 ---------- */

        {
            let msg = serde_json
                ::to_string(
                    &(GameBegin {
                        begin: "begin".to_string(),
                    })
                )
                .unwrap();
            let clients = clients.lock().await;
            clients.0.conn.lock().await.send(Message::Text(msg.clone())).await?;
            clients.1.conn.lock().await.send(Message::Text(msg.clone())).await?;
        }

        /* ---------- 返回 session ---------- */

        Ok((ssid, session))
    }

    /// session 线程函数
    async fn handle_session(
        clients: Arc<Mutex<(Client, Client)>>,
        mut score_rx0: mpsc::UnboundedReceiver<i32>,
        mut score_rx1: mpsc::UnboundedReceiver<i32>,
        mut end_rx0: mpsc::UnboundedReceiver<i32>,
        mut end_rx1: mpsc::UnboundedReceiver<i32>,
        mut disconnect_rx0: mpsc::UnboundedReceiver<bool>,
        mut disconnect_rx1: mpsc::UnboundedReceiver<bool>
    ) -> Result<(), SessionErr> {
        let mut gaming0 = true;
        let mut gaming1 = true;

        loop {
            select! {
            // Handle messages from gamer 0
            msg = score_rx0.recv() => {
                if let Some(score) = msg {
                    clients.lock().await.0.score = score;
                    let clients = clients.lock().await;
                    if let Err(e) = clients.1.conn
                        .lock().await
                        .send(
                            Message::Text(json!(Score { value: clients.0.score }).to_string())
                        ).await
                    {
                        if !matches!(e, tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) {
                            return Err(e.into());
                        }
                    };
                }
            },
            end = end_rx0.recv() => {
                if let Some(score) = end {
                    gaming0 = false;
                    clients.lock().await.0.score = score;
                }
            },
            disconnect = disconnect_rx0.recv() => {
                if let Some(true) = disconnect {
                    gaming0 = false;
                }
            },

            // Handle messages from gamer 1
            msg = score_rx1.recv() => {
                if let Some(score) = msg {
                    clients.lock().await.1.score = score;
                    let clients = clients.lock().await;
                        if let Err(e) = clients.0.conn
                        .lock().await
                        .send(
                            Message::Text(json!(Score { value: clients.0.score }).to_string())
                        ).await
                    {
                        if !matches!(e, tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) {
                            return Err(e.into());
                        }
                    };
                }
            },
            end = end_rx1.recv() => {
                if let Some(score) = end {
                    gaming1 = false;
                    clients.lock().await.1.score = score;
                }
            },
            disconnect = disconnect_rx1.recv() => {
                if let Some(true) = disconnect {
                    gaming1 = false;
                }
            },
        }

            // Check for game over
            if !gaming0 && !gaming1 {
                dbg!("game over");
                let clients = clients.lock().await;
                // Notify gamer 0
                if
                    let Err(e) = clients.0.conn.lock().await.send(
                        Message::Text(
                            json!(GameEnd {
                                end: "end".to_string(),
                                score: clients.1.score,
                            }).to_string()
                        )
                    ).await
                {
                    if
                        !matches!(
                            e,
                            tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed
                        )
                    {
                        return Err(e.into());
                    }
                }
                // Notify gamer 1
                if
                    let Err(e) = clients.1.conn.lock().await.send(
                        Message::Text(
                            json!(GameEnd {
                                end: "end".to_string(),
                                score: clients.0.score,
                            }).to_string()
                        )
                    ).await
                {
                    if
                        !matches!(
                            e,
                            tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed
                        )
                    {
                        return Err(e.into());
                    }
                }
                return Ok(());
            }
        }
    }
}

pub async fn run() -> Result<(), ServerError> {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:9999").await?;
    dbg!(&listener);

    let mut ez_buffer = Vec::new();

    while let Ok((stream, _)) = listener.accept().await {
        dbg!(&stream);
        let ws_stream = accept_async(stream).await.unwrap();
        ez_buffer.push(ws_stream);
        if ez_buffer.len() >= 2 {
            let (client1_ws, client2_ws) = (ez_buffer.pop().unwrap(), ez_buffer.pop().unwrap());
            if client1_ws.is_terminated() {
                ez_buffer.push(client2_ws);
                continue;
            }
            if client2_ws.is_terminated() {
                ez_buffer.push(client1_ws);
                continue;
            }
            let _ = Session::broadcast_start(client1_ws, client2_ws).await;
        }
    }
    Ok(())
}
