//! plugins-worker —— Umi-OCR 插件协议兼容层（宿主侧，规范 §7）。
//!
//! spawn python worker（`pyshim/umi_worker.py`），按行 JSON 协议通信（规范 §3），
//! 并管理托管 venv（规范 §5，见 [`venv`]）。
//!
//! worker 脚本定位：环境变量 `RUMI_PYSHIM_DIR` 指向 pyshim 目录；未设置时按
//! 工作区布局取 `<crate>/../pyshim`（开发期）。打包分发时设置该变量即可。
//!
//! ```no_run
//! use serde_json::json;
//!
//! # async fn demo() -> plugins_worker::Result<()> {
//! let mut p = plugins_worker::PluginProcess::spawn(
//!     std::path::Path::new("python3"),
//!     std::path::Path::new("plugins/demo_AbaOCR"),
//!     "zh_CN",
//! )?;
//! let _info = p.info().await?;
//! p.init(json!({"api_key": "..."})).await?;
//! let start = p.start(json!({"language": "zh_CN"})).await?; // {"ok":bool,"error":str}
//! let res = p.run_path("/tmp/x.png").await?;                // {"code":100,"data":[..]}
//! # Ok(())
//! # }
//! ```

pub mod venv;

use serde::Serialize;
use serde_json::{Value, json};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

pub type Result<T> = std::result::Result<T, Error>;

/// 宿主侧错误。[`Error::WorkerDied`] 表示 worker 已退出（EOF），重启后须重走 init+start。
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Json(serde_json::Error),
    /// worker 进程死亡：stdout 读到 EOF，或写入时管道已断
    WorkerDied,
    /// worker 经协议回报的错误（ok:false，error 字段）
    Worker(String),
    /// worker 输出无法解析为协议消息
    Protocol(String),
    /// 进程 spawn 失败 / 参数不可用
    Spawn(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "IO 错误：{e}"),
            Error::Json(e) => write!(f, "JSON 错误：{e}"),
            Error::WorkerDied => write!(f, "worker 进程已退出（EOF）"),
            Error::Worker(msg) => write!(f, "插件 worker 报错：{msg}"),
            Error::Protocol(msg) => write!(f, "worker 协议错误：{msg}"),
            Error::Spawn(msg) => write!(f, "spawn 失败：{msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

/// info 返回的插件描述（规范 §7）。
#[derive(Debug, Clone, Serialize)]
pub struct PluginEntry {
    pub name: String,
    pub group: String,
    pub global_options: Value,
    pub local_options: Value,
}

impl PluginEntry {
    /// 从 info 协议结果构造；缺字段时取空值，不报错。
    pub fn from_info(v: &Value) -> Self {
        Self {
            name: v
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            group: v
                .get("group")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            global_options: v
                .get("global_options")
                .cloned()
                .unwrap_or(Value::Null),
            local_options: v.get("local_options").cloned().unwrap_or(Value::Null),
        }
    }
}

/// 插件识别结果统一中间格式（规范 §7：box 是 4 点 × xy）。
#[derive(Debug, Clone, Serialize)]
pub struct OcrLine {
    pub text: String,
    /// 四点 box：[[x, y]; 4]，插件未提供时为 None
    #[serde(rename = "box")]
    pub box_: Option<[[f32; 2]; 4]>,
    pub score: Option<f32>,
}

/// 把插件 run 返回的 `{"code":100,"data":[..]}` 转成统一 [`OcrLine`] 列表。
/// `code != 100` 时把 `data` 作为错误文本返回 [`Error::Worker`]。
pub fn plugin_result_to_lines(result: &Value) -> Result<Vec<OcrLine>> {
    let code = result.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 100 {
        let msg = result
            .get("data")
            .and_then(Value::as_str)
            .unwrap_or("插件返回非 100 code 且 data 不是文本");
        return Err(Error::Worker(format!("code={code}：{msg}")));
    }
    let mut lines = Vec::new();
    if let Some(items) = result.get("data").and_then(Value::as_array) {
        for it in items {
            let text = it
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let box_ = it.get("box").and_then(|pts| {
                let pts = pts.as_array()?;
                if pts.len() < 4 {
                    return None;
                }
                let mut b = [[0f32; 2]; 4];
                for (i, pt) in pts.iter().take(4).enumerate() {
                    let xy = pt.as_array()?;
                    b[i][0] = xy.first()?.as_f64()? as f32;
                    b[i][1] = xy.get(1)?.as_f64()? as f32;
                }
                Some(b)
            });
            let score = it.get("score").and_then(Value::as_f64).map(|s| s as f32);
            lines.push(OcrLine { text, box_, score });
        }
    }
    Ok(lines)
}

/// 标准字母表 base64 编码（JSON 无 bytes 类型，runBytes 走 base64，规范 §3）。
/// 不引入 base64 crate——本 crate 依赖收敛在 serde/serde_json/tokio。
pub fn base64_encode(data: &[u8]) -> String {
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TBL[(n >> 18) as usize & 63] as char);
        out.push(TBL[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TBL[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TBL[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 扫描插件目录：含 `__init__.py` 的一级子目录（规范 §7），按路径排序。
pub fn scan_plugins(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() && p.join("__init__.py").is_file() {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// python worker 子进程（async 接口；测试/同步场景用 [`BlockingPluginProcess`]）。
///
/// drop 时自动 kill worker（`kill_on_drop`），不留孤儿 python。
pub struct PluginProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

fn write_err_to_worker_died(e: io::Error) -> Error {
    if matches!(
        e.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::UnexpectedEof
    ) {
        Error::WorkerDied
    } else {
        Error::Io(e)
    }
}

impl PluginProcess {
    /// 启动 worker：`<python> umi_worker.py <plugin_dir> [--lang <lang>]`。
    /// `lang` 为空串时不传 `--lang`。须在 tokio runtime 上下文内调用。
    pub fn spawn(python: &Path, plugin_dir: &Path, lang: &str) -> Result<Self> {
        let script = worker_script()?;
        let plugin_dir = plugin_dir
            .canonicalize()
            .map_err(|e| Error::Spawn(format!("插件目录不可用 {}：{e}", plugin_dir.display())))?;
        let mut cmd = tokio::process::Command::new(python);
        cmd.arg(&script).arg(&plugin_dir);
        if !lang.is_empty() {
            cmd.arg("--lang").arg(lang);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // stderr 原样转发到宿主日志（规范 §3）：直接继承，
            // 避免管道积压把 worker 的插件 print 卡死
            .stderr(std::process::Stdio::inherit())
            .env("PYTHONIOENCODING", "utf-8")
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            Error::Spawn(format!("启动 python worker 失败（{}）：{e}", python.display()))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Spawn("worker stdin 管道缺失".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Spawn("worker stdout 管道缺失".into()))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    /// worker 子进程 pid（已退出时为 None），宿主可用于监控/强杀。
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// 强杀 worker。之后所有调用返回 [`Error::WorkerDied`]。
    pub async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }

    /// 一问一答：发一行请求，读一行响应。worker 侧 ok:false 映射为 [`Error::Worker`]。
    async fn call(&mut self, method: &str, extra: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut req = json!({"id": id, "method": method});
        if let (Some(obj), Value::Object(extra)) = (req.as_object_mut(), extra) {
            for (k, v) in extra {
                obj.insert(k, v);
            }
        }
        let mut line = req.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(write_err_to_worker_died)?;
        self.stdin.flush().await.map_err(write_err_to_worker_died)?;

        let mut buf = String::new();
        let n = self.stdout.read_line(&mut buf).await?;
        if n == 0 {
            // stdout EOF：worker 已死（规范 §3：宿主报 WorkerDied，重启后须重走 init+start）
            return Err(Error::WorkerDied);
        }
        let resp: Value = serde_json::from_str(buf.trim()).map_err(|e| {
            Error::Protocol(format!("worker 响应无法解析：{e}；原文：{}", buf.trim()))
        })?;
        match resp.get("ok") {
            Some(Value::Bool(true)) => Ok(resp.get("result").cloned().unwrap_or(Value::Null)),
            _ => {
                let msg = resp
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("worker 未回报错误信息")
                    .to_string();
                Err(Error::Worker(msg))
            }
        }
    }

    /// 插件描述：`{"name","group","global_options","local_options"}`
    pub async fn info(&mut self) -> Result<Value> {
        self.call("info", json!({})).await
    }

    /// 实例化 `Api(globalArgd)`。成功返回 `()`，Api 构造异常返回 [`Error::Worker`]。
    pub async fn init(&mut self, global: Value) -> Result<()> {
        self.call("init", json!({ "global": global })).await.map(|_| ())
    }

    /// 启动引擎。返回插件层结果 `{"ok":bool,"error":str}`（"" 成功 / "[Error] xxx" 失败）；
    /// Api.start 未捕获异常返回 [`Error::Worker`]。
    pub async fn start(&mut self, local: Value) -> Result<Value> {
        self.call("start", json!({ "local": local })).await
    }

    /// 路径识图，原样转发插件返回值 `{"code":100,"data":[..]}` / `{"code":102,"data":".."}`。
    pub async fn run_path(&mut self, img_path: &str) -> Result<Value> {
        self.call("runPath", json!({ "path": img_path })).await
    }

    /// 字节流识图：bytes 经 base64 装载（规范 §3）。
    pub async fn run_bytes(&mut self, image_bytes: &[u8]) -> Result<Value> {
        self.call("runBytes", json!({ "b64": base64_encode(image_bytes) }))
            .await
    }

    /// base64 字符串识图。
    pub async fn run_base64(&mut self, image_base64: &str) -> Result<Value> {
        self.call("runBase64", json!({ "b64": image_base64 })).await
    }

    /// 停止引擎。
    pub async fn stop(&mut self) -> Result<()> {
        self.call("stop", json!({})).await.map(|_| ())
    }
}

/// 定位 pyshim/umi_worker.py：`RUMI_PYSHIM_DIR` 优先，回退工作区布局（开发期）。
fn worker_script() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("RUMI_PYSHIM_DIR") {
        let p = PathBuf::from(dir).join("umi_worker.py");
        return if p.is_file() {
            Ok(p)
        } else {
            Err(Error::Spawn(format!(
                "RUMI_PYSHIM_DIR 下找不到 umi_worker.py：{}",
                p.display()
            )))
        };
    }
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../pyshim/umi_worker.py");
    if p.is_file() {
        return Ok(p);
    }
    Err(Error::Spawn(format!(
        "找不到 pyshim/umi_worker.py（打包分发时请设 RUMI_PYSHIM_DIR）：{}",
        p.display()
    )))
}

/// [`PluginProcess`] 的同步包装：内置 current_thread runtime，供测试与同步调用方使用。
/// 字段顺序保证先 drop 子进程（在 runtime 仍存活时被 kill_on_drop 回收）再 drop runtime。
pub struct BlockingPluginProcess {
    inner: PluginProcess,
    rt: tokio::runtime::Runtime,
}

impl BlockingPluginProcess {
    pub fn spawn(python: &Path, plugin_dir: &Path, lang: &str) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::Spawn(format!("构建 tokio runtime 失败：{e}")))?;
        let inner = rt.block_on(async { PluginProcess::spawn(python, plugin_dir, lang) })?;
        Ok(Self { inner, rt })
    }

    pub fn id(&self) -> Option<u32> {
        self.inner.id()
    }

    pub fn info(&mut self) -> Result<Value> {
        self.rt.block_on(self.inner.info())
    }

    pub fn init(&mut self, global: Value) -> Result<()> {
        self.rt.block_on(self.inner.init(global))
    }

    pub fn start(&mut self, local: Value) -> Result<Value> {
        self.rt.block_on(self.inner.start(local))
    }

    pub fn run_path(&mut self, img_path: &str) -> Result<Value> {
        self.rt.block_on(self.inner.run_path(img_path))
    }

    pub fn run_bytes(&mut self, image_bytes: &[u8]) -> Result<Value> {
        self.rt.block_on(self.inner.run_bytes(image_bytes))
    }

    pub fn run_base64(&mut self, image_base64: &str) -> Result<Value> {
        self.rt.block_on(self.inner.run_base64(image_base64))
    }

    pub fn stop(&mut self) -> Result<()> {
        self.rt.block_on(self.inner.stop())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn scan_plugins_finds_packages_only() {
        let root = std::env::temp_dir().join(format!("rumi-scan-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("good")).unwrap();
        fs::write(root.join("good/__init__.py"), "").unwrap();
        fs::create_dir_all(root.join("no_init")).unwrap(); // 无 __init__.py → 排除
        fs::write(root.join("afile"), "").unwrap(); // 普通文件 → 排除
        let found = scan_plugins(&root);
        assert_eq!(found, vec![root.join("good")]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn plugin_result_to_lines_maps_fields() {
        let v = json!({
            "code": 100,
            "data": [{"text": "阿巴阿巴阿巴", "box": [[0, 0], [200, 0], [200, 40], [0, 40]], "score": 1}]
        });
        let lines = plugin_result_to_lines(&v).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "阿巴阿巴阿巴");
        assert_eq!(
            lines[0].box_,
            Some([[0.0, 0.0], [200.0, 0.0], [200.0, 40.0], [0.0, 40.0]])
        );
        assert_eq!(lines[0].score, Some(1.0));

        // code != 100 → 错误文本
        let bad = json!({"code": 102, "data": "未知的语言"});
        match plugin_result_to_lines(&bad) {
            Err(Error::Worker(msg)) => assert!(msg.contains("未知的语言")),
            other => panic!("预期 Error::Worker，实际 {other:?}"),
        }
    }
}
