use futures_util::{ SinkExt, StreamExt };
use tokio::{ net::{ TcpStream }, sync::mpsc::{ self, UnboundedSender }, task::JoinHandle };
use tokio_tungstenite::{ accept_async, tungstenite::WebSocket, WebSocketStream };
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio::sync::{ Mutex };
use std::{ sync::Arc };
use serde::{ Serialize, Deserialize };
use uuid::Uuid;

use super::{ ClientErr, ServerError };

type Conn = Arc<Mutex<WebSocketStream<TcpStream>>>;

struct Client {
    conn: Conn,
    score: i32, // java 只有 有符号整数，因此我们只用有符号整数就行了
    thread: JoinHandle<Result<(), ClientErr>>, // 线程句柄，返回 ()
}

/// 这里保存的是 session 的状态
struct Session {
    ssid: Uuid,
    clients: Arc<Mutex<(Client, Client)>>,
    thread: JoinHandle<()>,
}

// /// 这个应该需要一个 Arc Mutex
// type Sessions = Arc<Mutex<HashMap<Uuid, Session>>>;

// impl Session {
//     /// 获取到了 session 一个 client 的分数
//     async fn collect_scores(&self) {
//         unimplemented!()
//     }

//     /// 给 client 看对手的分数
//     async fn refresh_scores(&self) {
//         // let mut clients = self.clients.lock().await;
//         // let scores = self.scores.lock().await;
//         // if let (Some(ref mut client1), Some(ref mut client2)) = *clients {
//         //     if let (Some(score1), Some(score2)) = *scores {
//         //         let msg1 = serde_json::to_string(&score2).unwrap();
//         //         let msg2 = serde_json::to_string(&score1).unwrap();
//         //         client1.send(Message::Text(msg1)).await.unwrap();
//         //         client2.send(Message::Text(msg2)).await.unwrap();
//         //     }
//         // }
//         unimplemented!()
//     }

//     /// 跑 session
//     async fn run(&self) {
//         // 宣布开始游戏

//         // 收集分数

//         // 刷新分数

//         // 结束游戏
//         unimplemented!()
//     }
// }

#[derive(Serialize, Deserialize)]
struct GameBegin {
    /// 从 Server 发出去是 "begin"
    /// Client 在没有收到 "begin"
    message: String,
}

#[derive(Serialize, Deserialize)]
struct GameEnd {
    /// 来自于 Client 只能是 "end"
    /// 从 Server 发出去是 "wait" 或者是 "end"
    message: String,
}

#[derive(Serialize, Deserialize)]
struct Score {
    /// 来自于 Client
    message: i32,
}

impl Client {
    async fn handle_client(
        conn: Conn,
        score_tx: UnboundedSender<i32>,
        end_tx: UnboundedSender<()>
    ) -> Result<(), ClientErr> {
        while let Some(msg) = conn.lock().await.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                    if value["message_type"] == "score" {
                        let score: i32 = value["message"].as_i64().unwrap() as i32;
                        score_tx.send(score)?;
                    } else if value["message_type"] == "end" {
                        end_tx.send(())?;
                        break;
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
                loop {
                    tokio::select! {
                    Some(score) = score_rx1.recv() => {
                        let mut clients = clients.lock().await;
                        // 我们按照游戏周期来发送分数，而不是将分数加和
                        clients.0.score = score;
                    },
                    Some(score) = score_rx2.recv() => {
                        let mut clients = clients.lock().await;
                        clients.1.score = score;
                    },
                    Some(_) = end_rx1.recv() => {
                        let clients = clients.lock().await;
                        if end_rx2.try_recv().is_ok() {
                            // Announce end of the game
                            let msg = serde_json::to_string(&GameEnd {
                                message: "end".to_string(),
                            }).unwrap();
                            clients.0.conn.lock().await.send(Message::Text(msg.clone())).await.unwrap();
                            clients.1.conn.lock().await.send(Message::Text(msg.clone())).await.unwrap();
                            break;
                        } else {
                            let msg = serde_json::to_string(&GameEnd {
                                message: "wait".to_string(),
                            }).unwrap();
                            clients.0.conn.lock().await.send(Message::Text(msg.clone())).await.unwrap();
                        }
                    },
                    Some(_) = end_rx2.recv() => {
                        let clients = clients.lock().await;
                        if end_rx1.try_recv().is_ok() {
                            // Announce end of the game
                            let msg = serde_json::to_string(&GameEnd {
                                message: "end".to_string(),
                            }).unwrap();
                            clients.0.conn.lock().await.send(Message::Text(msg.clone())).await.unwrap();
                            clients.1.conn.lock().await.send(Message::Text(msg.clone())).await.unwrap();
                            break;
                        } else {
                            let msg = serde_json::to_string(&GameEnd {
                                message: "wait".to_string(),
                            }).unwrap();
                            clients.1.conn.lock().await.send(Message::Text(msg.clone())).await.unwrap();
                        }
                    },
                }
                }

                let clients = clients.lock().await;
                let final_scores = format!(
                    "Client 1 score: {}, Client 2 score: {}",
                    clients.0.score,
                    clients.1.score
                );
                println!("{}", final_scores);
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
                        message: "begin".to_string(),
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
}

pub async fn run() -> Result<(), ServerError> {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:9999").await?;

    let mut buffer = Vec::new();
    while let Ok((stream, _)) = listener.accept().await {
        let ws_stream = accept_async(stream).await.unwrap();
        buffer.push(ws_stream);
        if buffer.len() >= 2 {
            let (client1_ws, client2_ws) = (buffer.remove(0), buffer.remove(0));

            let _ = Session::broadcast_start(client1_ws, client2_ws).await;

            // let session_arc = Arc::new(session);
            // let session_clone = session_arc.clone();
            // let handle = tokio::spawn(async move {
            //     session_clone.run().await;
            // });

            // session_arc.thread = handle;

            // sessions.lock().await.insert(ssid, session_arc);
        }
    }
    Ok(())
}
