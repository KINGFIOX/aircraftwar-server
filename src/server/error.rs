use thiserror::Error;

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("Failed to bind the address: {0}")] IOError(#[from] std::io::Error), // IO 错误，原因：绑定地址失败
    #[error("session: {0}")] SessionError(SessionErr), // 会话错误
    #[error("send error: {0}")] SendError(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("send error: {0}")] ClientErr(ClientErr),
}

#[derive(Error, Debug)]
pub enum SessionErr {
    #[error("too many clients in a session, expect 2")] TooManyClients,
    #[error("less than 2 clients in a session, expect 2")] LessThanTwoClients,
}

#[derive(Error, Debug)]
pub enum ClientErr {
    #[error("channel send error: {0}")] ChannelSendScoreError(
        #[from] tokio::sync::mpsc::error::SendError<i32>,
    ),
    #[error("channel send error: {0}")] ChannelSendNullError(
        #[from] tokio::sync::mpsc::error::SendError<()>,
    ),
}
