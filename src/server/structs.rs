use serde::{ Deserialize, Serialize };

/// 从 Server 发出去是 "begin"
/// Client 在没有收到 "begin"
#[derive(Serialize, Deserialize, Debug)]
pub struct GameBegin {
    pub begin: String,
}

/// 来自于 Client 只能是 "end"
/// 从 Server 发出去是 "wait" 或者是 "end"
#[derive(Serialize, Deserialize, Debug)]
pub struct GameEnd {
    pub end: String,
    pub score: i32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Score {
    pub value: i32,
}
