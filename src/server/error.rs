use thiserror::Error;

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("Failed to bind the address: {0}")] IOError(#[from] std::io::Error), // IO 错误，原因：绑定地址失败
    #[error("send error: {0}")] SendError(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("session: {0}")] SessionError(SessionErr), // 会话错误
    #[error("client error: {0}")] ClientErr(ClientErr),
}

#[derive(Error, Debug)]
pub enum SessionErr {
    #[error("send error: {0}")] SendError(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("serde error: {0}")] SerdeError(#[from] serde_json::Error),
    #[error("channel error: {0}")] ChannelError(#[from] tokio::sync::mpsc::error::TryRecvError),
}

#[derive(Error, Debug)]
pub enum ClientErr {
    #[error("channel send error: {0}")] ChannelSendScoreError(
        #[from] tokio::sync::mpsc::error::SendError<i32>,
    ),
    #[error("channel send error: {0}")] ChannelSendNullError(
        #[from] tokio::sync::mpsc::error::SendError<()>,
    ),
    #[error("serde error: {0}")] SerdeError(#[from] serde_json::Error),
}
