//! 链接加固（2026-11-03，外部咨询共识落地）：
//! 静态原生库（ct2rs/sentencepiece 的 protobuf-lite、oneDNN 等）的符号默认会
//! 导出到可执行文件的动态符号表；系统动态库（libonnxruntime）的内部调用因此
//! 可能被 ELF 全局符号插入劫持到 exe 内另一版本的实现上（实测 protobuf 互劫
//! 致模型加载 SIGSEGV，复盘见 docs/开发记录.md）。`--exclude-libs,ALL` 把静态
//! 库符号全部收敛为本地，两边各用各的。
//!
//! 为什么在 build.rs 而不只靠 .cargo/config.toml：cargo 的 rustflags 只取一个
//! 来源（环境变量 > config），任何 `RUSTFLAGS=...` 一设，config 里的参数就
//! 静默失效、崩溃复现且难排查；build script 发出的 link-arg 不受其影响。
//! config.toml 保留作双保险（链接器对重复参数幂等）。仅 Linux（GNU/LLD 支持）。
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg=-Wl,--exclude-libs,ALL");
    }
}
