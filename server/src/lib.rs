//! rumi-server —— M3 §5：Umi 兼容 HTTP API（axum 0.8，默认 127.0.0.1:1224）。
//!
//! - [`config`]：`~/.config/rumi/config.toml` 读取与热加载（每次请求现读现解析）
//! - [`dispatch`]：引擎选择分发（native 走 rumi_ocr，plugin:名 走 plugins-worker）
//! - [`http`]：路由 / 响应格式 / 生命周期（serve + shutdown 句柄）
//!
//! ```no_run
//! # async fn demo() -> std::io::Result<()> {
//! let (addr, handle) = rumi_server::serve(Default::default()).await?;
//! println!("listening on {addr}");
//! handle.shutdown().await; // 优雅退出
//! # Ok(())
//! # }
//! ```

pub mod config;
pub mod dispatch;
pub mod http;

pub use config::{AppConfig, EngineChoice};
pub use dispatch::{OcrInput, Recognizer, UmiLine};
pub use http::{ServerConfig, ServerHandle, build_router, serve};
