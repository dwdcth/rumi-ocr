//! protobuf 链接守卫（2026-11-04，外部咨询共识的测试形态落地）。
//!
//! 背景：ort-sys（静态，内嵌 protobuf 3.14）与 sentencepiece-sys（静态，内嵌
//! protobuf-lite 3.21）曾同链互劫持（SIGSEGV，复盘见 docs/开发记录.md）。现行
//! 防线 = 分词器纯 Rust 化（sentencepiece-sys 依赖树清零）+ ort-sys 动态链
//! 系统 onnxruntime + `--exclude-libs,ALL`（translate/build.rs 下发）。
//! 本守卫钉住终态：**两个 exe 的全符号表（含本地符号）不允许出现任何
//! `google::protobuf` 符号**——将来任何依赖变更引入第二份 protobuf 都会在
//! 测试里响亮失败，而不是在运行期"看起来无关"的路径上 SIGSEGV。
//!
//! 运行前提（宽松 skip，不打网不阻塞）：target/debug 下存在 rumi-ui 与
//! rumi-server 二进制（cargo test 单跑 translate 时可能未构建 ui/server——
//! skip 并打印原因；`cargo test --workspace` 或 build 过即覆盖）。
//! 手动复验命令（CI 不做，见仓库决策）：
//! `nm target/debug/rumi-server | c++filt | grep -c 'google::protobuf'`

use std::path::PathBuf;
use std::process::Command;

/// 从本测试的 manifest（<root>/translate）推 workspace target 目录。
/// 注意集成测试的 CARGO_MANIFEST_DIR 是 **crate 根**（translate/），
/// 一次 parent 即 workspace root。
fn target_dir() -> PathBuf {
    if let Ok(t) = std::env::var("CARGO_TARGET_DIR") {
        return PathBuf::from(t);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent() // translate/ → workspace root
        .expect("workspace root 推导失败")
        .join("target")
        .join("debug")
}

/// 全符号表（含本地符号）里 google::protobuf 的符号数。
/// 走 shell 管道（nm | c++filt | grep -c）：nm 对 debug 二进制的输出有几十 MB，
/// Rust 侧手工接管管道易在写端阻塞死锁（曾踩：写完才读，缓冲 64KB 打满即挂），
/// 交给 shell 并发消费最省心。工具缺失/失败 → None（调用方按 skip 处理）。
fn protobuf_symbol_count(bin: &std::path::Path) -> Option<usize> {
    let script = format!(
        "nm {} | c++filt | grep -c 'google::protobuf' || true",
        shell_escape(bin)
    );
    let out = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .ok()
}

/// 路径转义进 shell 单引号串（路径含空格/中文时安全）。
fn shell_escape(p: &std::path::Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "'\\''"))
}

#[test]
fn protobuf_link_guard() {
    let dir = target_dir();
    let mut checked = 0;
    let mut skipped: Vec<&str> = Vec::new();
    for name in ["rumi-ui", "rumi-server"] {
        let bin = dir.join(name);
        if !bin.is_file() {
            skipped.push(name);
            continue;
        }
        match protobuf_symbol_count(&bin) {
            Some(0) => {
                checked += 1;
            }
            Some(n) => panic!(
                "{name} 全符号表出现 {n} 个 google::protobuf 符号——依赖变更引入了\
                第二份 protobuf（双静态库互劫持会 SIGSEGV，见 docs/开发记录.md）。\
                排查：cargo tree -i sentencepiece-sys / 检查 ort-sys 是否退回静态链接"
            ),
            None => skipped.push(name),
        }
    }
    if checked == 0 {
        eprintln!(
            "SKIP：{} 未构建或 nm/c++filt 不可用（cargo test --workspace 前先 \
            cargo build -p rumi-ui -p rumi-server 可获得覆盖）",
            skipped.join("、")
        );
        return;
    }
    if !skipped.is_empty() {
        eprintln!("部分覆盖：{} 未构建，已跳过", skipped.join("、"));
    }
}
