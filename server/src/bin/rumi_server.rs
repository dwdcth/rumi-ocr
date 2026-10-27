//! rumi-server 前台进程：读默认配置（engine/port/token），起 HTTP 服务直到 Ctrl-C。
//!
//! 用法：`rumi-server [--port N] [--config PATH] [--host ADDR]`
//! CLI 参数优先于配置文件；端口默认取 config.toml（无则 1224）。

use rumi_server::{ServerConfig, serve};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(msg) = run(args) {
        eprintln!("用法: rumi-server [--port N] [--config PATH] [--host ADDR]\n错误：{msg}");
        std::process::exit(2);
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let mut cfg = ServerConfig::default();
    // 先读配置文件取默认端口，CLI 再覆盖
    cfg.port = rumi_server::AppConfig::load(&cfg.config_path).port();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                cfg.port = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| "--port 需要一个整数".to_string())?;
                i += 2;
            }
            "--config" => {
                cfg.config_path = PathBuf::from(
                    args.get(i + 1).ok_or_else(|| "--config 需要一个路径".to_string())?,
                );
                i += 2;
            }
            "--host" => {
                cfg.host = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| "--host 需要一个 IP 地址".to_string())?;
                i += 2;
            }
            other => return Err(format!("未知参数 {other}")),
        }
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("构建 tokio runtime 失败：{e}"))?;
    runtime
        .block_on(async move {
            let (addr, handle) = serve(cfg).await?;
            println!("rumi-server 监听 http://{addr}（Ctrl-C 退出）");
            if let Err(e) = tokio::signal::ctrl_c().await {
                eprintln!("等待 Ctrl-C 失败：{e}");
            }
            handle.shutdown().await;
            println!("已退出");
            Ok::<(), std::io::Error>(())
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}
