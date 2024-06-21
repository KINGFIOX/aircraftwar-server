use futures_util::{ SinkExt, StreamExt };
use serde_json::Value;
use tokio::{ net::{ TcpStream }, sync::mpsc::{ self, UnboundedSender }, task::JoinHandle };
use tokio_tungstenite::{ accept_async, tungstenite::{ self, client, WebSocket }, WebSocketStream };
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio::sync::{ Mutex };
use std::{ collections::HashMap, sync::Arc };
use serde::{ Serialize, Deserialize };
use uuid::Uuid;

use super::{ ClientErr, ServerError, SessionErr };

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

/// 从 Server 发出去是 "begin"
/// Client 在没有收到 "begin"
#[derive(Serialize, Deserialize, Debug)]
struct GameBegin {
    begin: String,
}

/// 来自于 Client 只能是 "end"
/// 从 Server 发出去是 "wait" 或者是 "end"
#[derive(Serialize, Deserialize, Debug)]
struct GameEnd {
    end: String,
    score: i32,
}

#[derive(Serialize, Deserialize, Debug)]
struct Score {
    score: i32,
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

                    if value.get("begin").is_some() {
                        let game_begin: GameBegin = serde_json::from_value(value)?;
                        dbg!(&game_begin);
                        // 处理 game_begin 消息
                    } else if value.get("end").is_some() {
                        let game_end: GameEnd = serde_json::from_value(value)?;
                        dbg!(&game_end);
                        end_tx.send(game_end.score)?;
                        // break;
                    } else if value.get("score").is_some() {
                        let score: Score = serde_json::from_value(value)?;
                        score_tx.send(score.score)?;
                        // dbg!(&score);
                    } else {
                        dbg!("Unknown message type");
                    }
                }
                Ok(Message::Close(frame)) => {
                    dbg!(frame);
                    disconnect_tx.send(true)?;
                    return Ok(()); // 应该是会销毁所有的 tx
                }
                Ok(_) => {}
                Err(e) => {
                    dbg!(e);
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
        let mut gaming0: bool = true;

        let mut gaming1: bool = true;

        // polling
        loop {
            /* ---------- gamer 0 ---------- */
            match score_rx0.try_recv() {
                Ok(score) => {
                    clients.lock().await.0.score = score;
                }
                Err(e) => {
                    match e {
                        mpsc::error::TryRecvError::Disconnected => {
                            gaming0 = false; /* 上面断开的时候减过了 */
                        }
                        mpsc::error::TryRecvError::Empty => {/* 没有数据不是错误 */}
                    }
                }
            }

            match disconnect_rx0.try_recv() {
                Ok(true) => {
                    gaming0 = false;
                }
                Ok(false) => {/*  */}
                Err(e) => {
                    match e {
                        mpsc::error::TryRecvError::Disconnected => {/* 上面断开的时候减过了 */}
                        mpsc::error::TryRecvError::Empty => {/* 没有数据不是错误 */}
                    }
                }
            }

            match end_rx0.try_recv() {
                Ok(score) => {
                    gaming0 = false;
                    dbg!(&score);
                    clients.lock().await.0.score = score;
                }
                Err(e) => {
                    match e {
                        mpsc::error::TryRecvError::Disconnected => {/* 上面断开的时候减过了 */}
                        mpsc::error::TryRecvError::Empty => {/* 没有数据不是错误 */}
                    }
                }
            }

            /* ---------- gamer 1 ---------- */

            match score_rx1.try_recv() {
                Ok(score) => {
                    clients.lock().await.1.score = score;
                }
                Err(e) => {
                    match e {
                        mpsc::error::TryRecvError::Disconnected => {
                            gaming1 = false;
                            /* 上面断开的时候减过了 */
                        }
                        mpsc::error::TryRecvError::Empty => {/* 没有数据不是错误 */}
                    }
                }
            }

            match disconnect_rx1.try_recv() {
                Ok(true) => {
                    gaming1 = false;
                }
                Ok(false) => {/*  */}
                Err(e) => {
                    match e {
                        mpsc::error::TryRecvError::Disconnected => {/* 上面断开的时候减过了 */}
                        mpsc::error::TryRecvError::Empty => {/* 没有数据不是错误 */}
                    }
                }
            }

            match end_rx1.try_recv() {
                Ok(score) => {
                    gaming1 = false;
                    dbg!(&score);
                    clients.lock().await.1.score = score;
                }
                Err(e) => {
                    match e {
                        mpsc::error::TryRecvError::Disconnected => {/* 上面断开的时候减过了 */}
                        mpsc::error::TryRecvError::Empty => {/* 没有数据不是错误 */}
                    }
                }
            }

            /* 发送分数给双方 */
            {
                let clients = clients.lock().await;
                if
                    let Err(e) = clients.0.conn.lock().await.send(
                        Message::Text(
                            serde_json::to_string(
                                &(Score {
                                    score: clients.1.score,
                                })
                            )?
                        )
                    ).await
                {
                    if
                        let
                        | tungstenite::Error::ConnectionClosed
                        | tungstenite::Error::AlreadyClosed = e
                    {
                    } else {
                        return Err(e.into());
                    }
                }

                if
                    let Err(e) = clients.1.conn.lock().await.send(
                        Message::Text(
                            serde_json::to_string(
                                &(Score {
                                    score: clients.0.score,
                                })
                            )?
                        )
                    ).await
                {
                    if
                        let
                        | tungstenite::Error::ConnectionClosed
                        | tungstenite::Error::AlreadyClosed = e
                    {
                    } else {
                        return Err(e.into());
                    }
                };
            }

            /* ---------- 结束标志 ---------- */

            if !gaming0 && !gaming1 {
                dbg!("game over");

                /* ---------- 0 号玩家 ---------- */
                {
                    let clients = clients.lock().await;
                    if
                        let Err(e) = clients.0.conn.lock().await.send(
                            Message::Text(
                                serde_json::to_string(
                                    &(GameEnd {
                                        end: "end".to_string(),
                                        score: clients.1.score, // 传对方的分数
                                    })
                                )?
                            )
                        ).await
                    {
                        if
                            let
                            | tungstenite::Error::ConnectionClosed
                            | tungstenite::Error::AlreadyClosed = e
                        {
                        } else {
                            return Err(e.into());
                        }
                    };
                }

                /* ---------- 1 号玩家 ---------- */
                {
                    let clients = clients.lock().await;
                    if
                        let Err(e) = clients.1.conn.lock().await.send(
                            Message::Text(
                                serde_json::to_string(
                                    &(GameEnd {
                                        end: "end".to_string(),
                                        score: clients.0.score,
                                    })
                                )?
                            )
                        ).await
                    {
                        if
                            let
                            | tungstenite::Error::ConnectionClosed
                            | tungstenite::Error::AlreadyClosed = e
                        {
                        } else {
                            return Err(e.into());
                        }
                    };
                }
                return Ok(());
            }
        }
    }
}

pub async fn run() -> Result<(), ServerError> {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:9999").await?;
    dbg!(&listener);

    let mut buffer = Vec::new();
    while let Ok((stream, _)) = listener.accept().await {
        dbg!(&stream);
        let ws_stream = accept_async(stream).await.unwrap();
        buffer.push(ws_stream);
        if buffer.len() >= 2 {
            let (client1_ws, client2_ws) = (buffer.remove(0), buffer.remove(0));

            let _ = Session::broadcast_start(client1_ws, client2_ws).await;
        }
    }
    Ok(())
}
