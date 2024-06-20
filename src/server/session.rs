use futures_util::{ SinkExt, StreamExt };
use serde_json::Value;
use tokio::{ net::{ TcpStream }, sync::mpsc::{ self, UnboundedSender }, task::JoinHandle };
use tokio_tungstenite::{ accept_async, tungstenite::WebSocket, WebSocketStream };
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
        end_tx: UnboundedSender<()>
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
                        end_tx.send(())?;
                        dbg!(&game_end);
                        break;
                    } else if value.get("score").is_some() {
                        let score: Score = serde_json::from_value(value)?;
                        score_tx.send(score.score)?;
                        dbg!(&score);
                    } else {
                        dbg!("Unknown message type");
                    }
                }
                _ => {
                    continue;
                }
            }
        }
        Ok(())
    }
}

impl Session {
    /// 宣布游戏开始，并创建 session
    async fn broadcast_start(
        conn1: WebSocketStream<TcpStream>,
        conn2: WebSocketStream<TcpStream>
    ) -> Result<(Uuid, Session), ServerError> {
        let ssid = Uuid::new_v4();

        let (score_tx1, mut score_rx1) = mpsc::unbounded_channel();
        let (end_tx1, mut end_rx1) = mpsc::unbounded_channel();
        let (score_tx2, mut score_rx2) = mpsc::unbounded_channel();
        let (end_tx2, mut end_rx2) = mpsc::unbounded_channel();

        let conn1 = Arc::new(Mutex::new(conn1));
        let conn2 = Arc::new(Mutex::new(conn2));

        /* ---------- 创建 client ---------- */

        let client1 = Client {
            conn: conn1.clone(),
            score: 0,
            thread: tokio::spawn(Client::handle_client(conn1.clone(), score_tx1, end_tx1)),
        };

        let client2 = Client {
            conn: conn2.clone(),
            score: 0,
            thread: tokio::spawn(Client::handle_client(conn2.clone(), score_tx2, end_tx2)),
        };

        let clients = Arc::new(Mutex::new((client1, client2)));

        /* ---------- session 的创建 ---------- */

        let session_thread = {
            let clients = clients.clone();
            tokio::spawn(async move {
                Session::handle_session(clients, score_rx1, score_rx2, end_rx1, end_rx2).await
            })
        };

        let session = Session {
            ssid: ssid.clone(),
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
            dbg!(msg);
        }

        /* ---------- 返回 session ---------- */

        Ok((ssid, session))
    }

    /// session 线程函数
    async fn handle_session(
        clients: Arc<Mutex<(Client, Client)>>,
        mut score_rx0: mpsc::UnboundedReceiver<i32>,
        mut score_rx1: mpsc::UnboundedReceiver<i32>,
        mut end_rx0: mpsc::UnboundedReceiver<()>,
        mut end_rx1: mpsc::UnboundedReceiver<()>
    ) -> Result<(), SessionErr> {
        let mut cnt: u32 = 2;
        // polling
        loop {
            /* ---------- 分数 ---------- */
            match score_rx0.try_recv() {
                Ok(score) => {
                    let mut clients = clients.lock().await;
                    clients.0.score = score;
                    // 向客户端发送分数
                    let msg = serde_json::to_string(
                        &(Score {
                            score,
                        })
                    )?;
                    // 发给对方
                    clients.1.conn.lock().await.send(Message::Text(msg)).await?;
                }
                Err(mpsc::error::TryRecvError::Empty) => {/* 没有数据不是错误 */}
                Err(e) => {
                    return Err(e.into());
                }
            }

            match score_rx1.try_recv() {
                Ok(score) => {
                    let mut clients = clients.lock().await;
                    clients.1.score = score;
                    // 向客户端发送分数
                    let msg = serde_json::to_string(
                        &(Score {
                            score,
                        })
                    )?;
                    clients.0.conn.lock().await.send(Message::Text(msg)).await?;
                }
                Err(mpsc::error::TryRecvError::Empty) => {/* 没有数据不是错误 */}
                Err(e) => {
                    return Err(e.into());
                }
            }

            /* ---------- 结束标志 ---------- */

            match end_rx0.try_recv() {
                Ok(_) => {
                    cnt -= 1;
                }
                Err(mpsc::error::TryRecvError::Empty) => {/* 没有数据不是错误 */}
                Err(e /* 出现了其他错误 */) => {
                    return Err(e.into());
                }
            }

            match end_rx1.try_recv() {
                Ok(_) => {
                    cnt -= 1;
                }
                Err(mpsc::error::TryRecvError::Empty) => {/* 没有数据不是错误 */}
                Err(e /* 出现了其他错误 */) => {
                    return Err(e.into());
                }
            }

            /* ---------- 结束标志 ---------- */
            if cnt == 0 {
                // 正常的结束
                break;
            }
        }

        // 宣布游戏结束
        {
            let clients = clients.lock().await;
            let msg0 = serde_json::to_string(
                &(GameEnd {
                    end: "end".to_string(),
                    score: clients.0.score,
                })
            )?;
            clients.1.conn.lock().await.send(Message::Text(msg0.clone())).await?;
            let msg1 = serde_json::to_string(
                &(GameEnd {
                    end: "end".to_string(),
                    score: clients.0.score,
                })
            )?;
            clients.1.conn.lock().await.send(Message::Text(msg1.clone())).await?;
        }

        Ok(())
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
