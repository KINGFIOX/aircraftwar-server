use clap::{ Parser, Subcommand };

#[derive(Parser, Debug)]
#[command(version, author, about, long_about = None)]
pub struct Cli {
    #[clap(subcommand)]
    pub subcmd: SubCommand,
}

// Subcommand 太神奇了
#[derive(Subcommand, Debug)]
pub enum SubCommand {
    /// 启动服务器
    Serve,
    /// 生成配置文件模板
    Gen,
}

#[derive(Parser, Debug)]
pub struct ServeConfig {
    /// 指定服务器的IP地址
    #[clap(long, default_value = "127.0.0.1")]
    pub ip: String,
    /// 指定服务器的端口
    #[clap(long, default_value = "8080")]
    pub port: u16,
}
