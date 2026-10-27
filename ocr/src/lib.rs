//! rumi-ocr：oar-ocr 的薄封装，提供统一中间格式 [`OcrLine`]。
//!
//! 设计（见 ../docs/m1-brief.md）：
//! - 引擎（ONNX session）进程内全局只建一次：[`OcrEngine::global()`]；
//!   首次 [`OcrEngine::warmup()`] / [`OcrEngine::recognize()`] 触发构建，
//!   含 ModelScope 模型下载（首次约 2.4s，之后命中 `~/.oar` 缓存）。
//! - 所有调用都会阻塞，务必在后台线程执行（ui crate 负责接线）。
//! - 输入统一是 `image::RgbaImage`（K2：不依赖 capture crate，capture 侧经
//!   `Frame::to_rgba_image` / `into_rgba_image` 交接），RGBA→RGB 在内存中完成，
//!   不落临时文件；oar-ocr 的 `predict` 本就接受 `image::RgbImage`。
//!
//! 附带纯逻辑模块（无引擎依赖，可独立测试）：
//! - [`postprocess`]：识别后处理——排版解析（按行/合并段落/自然段落）与忽略规则；
//! - [`qr`]：文本 → 二维码 PNG（与 [`scan_qr`] 组成生成/识别闭环）。
//!
//! 公式模式：[`OcrEngine::recognize_structured`] 走 oar-ocr 结构化管线
//! （PP-DocLayout-S 版面 + PP-OCRv6 文字 + PP-FormulaNet-S），公式区域
//! 识别为 LaTeX 直接进 [`OcrLine::text`]；示例见 examples/formula.rs。

use image::{DynamicImage, RgbImage, RgbaImage};
use oar_ocr::domain::structure::StructureResult;
use oar_ocr::prelude::*;
use oar_ocr::processors::BoundingBox;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

pub mod postprocess;
pub mod qr;
pub mod pdf_route;
pub mod dual_pdf;

/// 单行识别结果（统一中间格式，M2 的插件输出将来也转这个）。
#[derive(Debug, Clone, PartialEq)]
pub struct OcrLine {
    /// 识别出的文本
    pub text: String,
    /// 置信度 0..1
    pub score: f32,
    /// 四点框，原始输入图像坐标，顺序：左上→右上→右下→左下
    pub r#box: [[f32; 2]; 4],
}

/// OCR 错误
#[derive(Debug)]
pub enum OcrError {
    /// oar-ocr 报错（引擎构建 / 模型下载 / 推理）
    Oar(String),
    /// 引擎锁被毒化（另一线程识别时 panic）
    Poisoned,
    /// 文件 / 子进程 IO 错误
    Io(std::io::Error),
    /// 图像解码失败（png/jpg/webp）
    Image(String),
    /// PDF 分页失败（pdftoppm 子进程）
    Pdf(String),
    /// 二维码生成失败（文本过长 / scale 非法）
    Qr(String),
}

impl fmt::Display for OcrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OcrError::Oar(m) => write!(f, "oar-ocr: {m}"),
            OcrError::Poisoned => write!(f, "OCR 引擎锁被毒化"),
            OcrError::Io(e) => write!(f, "IO 错误：{e}"),
            OcrError::Image(m) => write!(f, "图像解码失败：{m}"),
            OcrError::Pdf(m) => write!(f, "PDF 处理失败：{m}"),
            OcrError::Qr(m) => write!(f, "二维码生成失败：{m}"),
        }
    }
}
impl std::error::Error for OcrError {}

impl From<OCRError> for OcrError {
    fn from(e: OCRError) -> Self {
        OcrError::Oar(e.to_string())
    }
}

impl From<std::io::Error> for OcrError {
    fn from(e: std::io::Error) -> Self {
        OcrError::Io(e)
    }
}

impl From<image::ImageError> for OcrError {
    fn from(e: image::ImageError) -> Self {
        OcrError::Image(e.to_string())
    }
}

/// OCR 引擎：oar-ocr 的懒加载单例包装。
///
/// `Mutex<Option<...>>` 而非裸 OnceLock：懒构建时可能失败（模型下载断网），
/// 且顺带串行化对同一 ONNX session 的 `predict` 调用。
///
/// 两条独立模型栈，各自懒构建、互不复用：
/// - `inner`：PP-OCR 文字管线（[`OcrEngine::recognize`]）；
/// - `structured`：PP-DocLayout 版面 + 文字 + PP-FormulaNet 公式管线
///   （[`OcrEngine::recognize_structured`]，模型更重，按需才下载）。
pub struct OcrEngine {
    inner: Mutex<Option<OAROCR>>,
    structured: Mutex<Option<OARStructure>>,
}

impl OcrEngine {
    /// 全局唯一实例（不阻塞、不构建；真正构建发生在首次 warmup/recognize）。
    pub fn global() -> &'static OcrEngine {
        static ENGINE: OnceLock<OcrEngine> = OnceLock::new();
        ENGINE.get_or_init(|| OcrEngine {
            inner: Mutex::new(None),
            structured: Mutex::new(None),
        })
    }

    /// 预热：构建 ONNX session（首次含模型下载，约 2.4s）。幂等、可重试。
    pub fn warmup(&self) -> Result<(), OcrError> {
        self.lock_engine().map(|_| ())
    }

    /// 识别一帧，返回逐行结果（按检测顺序）。
    ///
    /// 阻塞（首次调用含引擎构建与模型下载）；无文本时返回空 Vec。
    pub fn recognize(&self, img: &RgbaImage) -> Result<Vec<OcrLine>, OcrError> {
        Ok(self
            .recognize_many(std::slice::from_ref(img))?
            .into_iter()
            .next()
            .unwrap_or_default())
    }

    /// 批量识别（M3 §3）：oar-ocr 的 predict 本就接受批量输入，一次引擎锁
    /// 完成 N 帧，避免逐帧锁竞争。返回顺序与输入顺序一一对应。
    ///
    /// 阻塞（首次调用含引擎构建与模型下载）。
    pub fn recognize_many(&self, imgs: &[RgbaImage]) -> Result<Vec<Vec<OcrLine>>, OcrError> {
        if imgs.is_empty() {
            return Ok(Vec::new());
        }
        let guard = self.lock_engine()?;
        let engine = guard.as_ref().expect("lock_engine 保证已初始化");
        let rgb: Vec<RgbImage> = imgs.iter().map(rgba_to_rgb).collect();
        let results = engine.predict(rgb)?;
        if results.len() != imgs.len() {
            return Err(OcrError::Oar(format!(
                "predict 返回 {} 组结果，期望 {} 组",
                results.len(),
                imgs.len()
            )));
        }
        Ok(results
            .into_iter()
            .map(|r| {
                r.text_regions
                    .iter()
                    .filter_map(|tr| {
                        let (text, score) = tr.text_with_confidence()?;
                        Some(OcrLine {
                            text: text.to_string(),
                            score,
                            r#box: quad_of(&tr.bounding_box),
                        })
                    })
                    .collect()
            })
            .collect())
    }

    /// 取引擎锁；未构建则当场构建（模型下载发生在这里）。
    fn lock_engine(&self) -> Result<MutexGuard<'_, Option<OAROCR>>, OcrError> {
        let mut guard = self.inner.lock().map_err(|_| OcrError::Poisoned)?;
        if guard.is_none() {
            // 探针同款：pp-ocrv6 small（~/.oar 缓存对应的就是 small 三件套）
            *guard = Some(OAROCRBuilder::pp_ocrv6(PpOcrV6Size::Small).build()?);
        }
        Ok(guard)
    }

    /// 预热公式引擎：构建结构化管线（首次含重模型下载，见
    /// [`OcrEngine::recognize_structured`]）。幂等、可重试。
    pub fn warmup_structured(&self) -> Result<(), OcrError> {
        self.lock_structured().map(|_| ())
    }

    /// 结构化识别（公式模式）：一帧 → 带 LaTeX 的行。
    ///
    /// 管线：PP-DocLayout-S 版面检测 → 文字区域走 PP-OCRv6 small，
    /// 版面判为公式（formula/formula_number）的裁块送 PP-FormulaNet-S
    /// 识别成 LaTeX。返回的行里：公式行 `text` 是 LaTeX、`score` 是公式
    /// 识别置信度、`box` 是版面输出框；普通文字行同 [`OcrEngine::recognize`]。
    ///
    /// 阻塞（首次调用含引擎构建与模型下载：PP-DocLayout-S 约 5 MiB、
    /// PP-FormulaNet-S 约 221 MiB + 分词器 2 MiB，缓存在 `~/.oar`）。
    pub fn recognize_structured(&self, img: &RgbaImage) -> Result<Vec<OcrLine>, OcrError> {
        let rgb = rgba_to_rgb(img);
        let guard = self.lock_structured()?;
        let engine = guard.as_ref().expect("lock_structured 保证已初始化");
        let result = engine.predict_image(rgb)?;
        Ok(structure_lines(&result))
    }

    /// 取公式引擎锁；未构建则当场构建（重模型下载发生在这里）。
    fn lock_structured(&self) -> Result<MutexGuard<'_, Option<OARStructure>>, OcrError> {
        let mut guard = self.structured.lock().map_err(|_| OcrError::Poisoned)?;
        if guard.is_none() {
            // 最小结构化管线：版面 + 文字 + 公式，不带表格四件套
            // （pp_structurev3() 预置的表格模型合计 500+ MiB，本场景用不上）。
            // 模型名都是 auto-download 注册表条目（ModelScope → ~/.oar）。
            *guard = Some(
                OARStructureBuilder::new("pp-doclayout-s.onnx")
                    .layout_model_name("PP-DocLayout-S")
                    .with_ocr(
                        // 与文字管线同款 small，命中既有 ~/.oar 缓存
                        "pp-ocrv6_small_det.onnx",
                        "pp-ocrv6_small_rec.onnx",
                        "ppocrv6_dict.txt",
                    )
                    .with_formula_recognition(
                        "pp-formulanet-s.onnx",
                        "pp-formulanet-tokenizer.json",
                        "pp_formulanet",
                    )
                    .build()?,
            );
        }
        Ok(guard)
    }
}

/// [`StructureResult`] → 行列表（[`OcrEngine::recognize_structured`] 的纯转换层）。
///
/// - 公式：`StructureResult.formulas`（LaTeX + 真实置信度 + 输出框）；
/// - 文字：`text_regions` 里非公式区域——管线会把公式结果以 `label = "formula"`
///   注入 text_regions（置信度硬编码 1.0），这里跳过以免与公式行重复；
/// - 合并后按框顶 y 排序（单栏截图/文档的简单阅读序）。
fn structure_lines(result: &StructureResult) -> Vec<OcrLine> {
    let mut lines: Vec<OcrLine> = result
        .formulas
        .iter()
        .map(|f| OcrLine {
            text: f.latex.clone(),
            score: f.confidence,
            r#box: quad_of(&f.bbox),
        })
        .collect();
    if let Some(regions) = &result.text_regions {
        for tr in regions {
            if tr.label.as_deref() == Some("formula") {
                continue;
            }
            let Some((text, score)) = tr.text_with_confidence() else {
                continue;
            };
            lines.push(OcrLine {
                text: text.to_string(),
                score,
                r#box: quad_of(&tr.bounding_box),
            });
        }
    }
    lines.sort_by(|a, b| a.r#box[0][1].total_cmp(&b.r#box[0][1]));
    lines
}

/// RGBA 图像 → `RgbImage`（oar-ocr predict 的输入类型；alpha 直接丢弃，不预乘）。
fn rgba_to_rgb(img: &RgbaImage) -> RgbImage {
    DynamicImage::ImageRgba8(img.clone()).into_rgb8()
}

/// oar `BoundingBox.points`（Vec<Point>）→ 固定四点框。
///
/// 检测输出通常是 4 点多边形（PaddleOCR det polys）；
/// 不足 4 点时按最小外接矩形补齐，空框返回全 0。
fn quad_of(bb: &BoundingBox) -> [[f32; 2]; 4] {
    if bb.points.len() >= 4 {
        [
            [bb.points[0].x, bb.points[0].y],
            [bb.points[1].x, bb.points[1].y],
            [bb.points[2].x, bb.points[2].y],
            [bb.points[3].x, bb.points[3].y],
        ]
    } else if bb.points.is_empty() {
        [[0.0, 0.0]; 4]
    } else {
        let (x0, y0, x1, y1) = (bb.x_min(), bb.y_min(), bb.x_max(), bb.y_max());
        [[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
    }
}

// ---------------------------------------------------------------------------
// M3 §3/§4：文件输入（图片 / PDF）与二维码
// ---------------------------------------------------------------------------

/// 图片文件 → RGBA 图像（png/jpg/webp，image crate 解码）。
pub fn load_frame(path: &Path) -> Result<RgbaImage, OcrError> {
    let img = image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()
        .map_err(|e| OcrError::Image(format!("{}：{e}", path.display())))?;
    Ok(img.into_rgba8())
}

/// 内存图像字节（png/jpg/webp）→ RGBA 图像。HTTP base64 输入用，不落临时文件。
pub fn load_frame_from_bytes(bytes: &[u8]) -> Result<RgbaImage, OcrError> {
    let img = image::load_from_memory(bytes).map_err(|e| OcrError::Image(e.to_string()))?;
    Ok(img.into_rgba8())
}

/// 默认渲染 DPI（200：poppler 默认 150 对小字偏糊，300 体积翻倍收益有限）。
const PDF_DEFAULT_DPI: u32 = 200;

/// PDF → 按页 RGBA 图像列表（M3 §3）。
///
/// 选型说明：oar-ocr **库 crate 没有 PDF API**（其 git 仓里 PDF 分页只存在于
/// `oar-ocr-cli` 二进制 crate，经 hayro 渲染，无法作为库依赖复用），故按任务书
/// 兜底方案走 poppler `pdftoppm` 子进程按页转 PNG，再经 [`load_frame`] 装载。
/// 本机 poppler 26.10 可用；分发环境缺 pdftoppm 时返回 [`OcrError::Pdf`]。
pub fn load_pdf(path: &Path) -> Result<Vec<RgbaImage>, OcrError> {
    load_pdf_dpi(path, PDF_DEFAULT_DPI)
}

/// [`load_pdf`] 的显式 DPI 版本（渲染精度换内存/耗时的调节口）。
pub fn load_pdf_dpi(path: &Path, dpi: u32) -> Result<Vec<RgbaImage>, OcrError> {
    if !path.is_file() {
        return Err(OcrError::Pdf(format!("文件不存在：{}", path.display())));
    }
    // 临时目录：系统临时区 + 进程号 + 纳秒，避免并发/重入互踩
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("rumi-pdf-{}-{}", std::process::id(), nanos));
    std::fs::create_dir_all(&dir)?;
    let result = pdf_pages_via_pdftoppm(path, dpi, None, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// [`load_pdf`] 的页范围版本（1 起始、双端含）：PDF 智能路由只渲染扫描页区间用。
///
/// DPI 固定取 [`PDF_DEFAULT_DPI`]（与全量渲染一致）；`first > last` 或越界返回空/报错
/// 由 pdftoppm 语义兜底（越界页不产出，产出为空时报 [`OcrError::Pdf`]）。
pub fn load_pdf_pages(path: &Path, first: u32, last: u32) -> Result<Vec<RgbaImage>, OcrError> {
    if !path.is_file() {
        return Err(OcrError::Pdf(format!("文件不存在：{}", path.display())));
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("rumi-pdf-{}-{}", std::process::id(), nanos));
    std::fs::create_dir_all(&dir)?;
    let result = pdf_pages_via_pdftoppm(path, PDF_DEFAULT_DPI, Some((first, last)), &dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// `pdftoppm` 按页转 PNG 并装载。`range = Some((first, last))` 时经 `-f/-l`
/// 只渲染该闭区间（1 起始），`None` 渲染全文档。
fn pdf_pages_via_pdftoppm(
    path: &Path,
    dpi: u32,
    range: Option<(u32, u32)>,
    dir: &Path,
) -> Result<Vec<RgbaImage>, OcrError> {
    let prefix = dir.join("page");
    let mut cmd = Command::new("pdftoppm");
    cmd.arg("-png").arg("-r").arg(dpi.to_string());
    if let Some((first, last)) = range {
        cmd.arg("-f").arg(first.to_string());
        cmd.arg("-l").arg(last.to_string());
    }
    let out = cmd
        .arg(path)
        .arg(&prefix)
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                OcrError::Pdf("找不到 pdftoppm（需安装 poppler-utils）".into())
            }
            _ => OcrError::Io(e),
        })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(OcrError::Pdf(format!("pdftoppm 失败：{}", stderr.trim())));
    }
    // pdftoppm 命名 page-N.png（总页数 ≥10 时补零 page-01.png），
    // 按解析出的页码数字排序，保证页序与原 PDF 一致
    let mut pages: Vec<(u32, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if let Some(n) = page_number_of(&p) {
            pages.push((n, p));
        }
    }
    pages.sort_by_key(|(n, _)| *n);
    if pages.is_empty() {
        return Err(OcrError::Pdf("pdftoppm 未产出任何页面图像".into()));
    }
    pages.into_iter().map(|(_, p)| load_frame(&p)).collect()
}

/// 从 `page-12.png` 这类文件名解析页码（无页码/非 png 返回 None）。
fn page_number_of(p: &Path) -> Option<u32> {
    let name = p.file_name()?.to_str()?;
    let stem = name.strip_suffix(".png")?;
    stem.rsplit('-').next()?.parse().ok()
}

/// 扫描图像中的二维码，返回全部解码文本（M3 §4）。
///
/// 选型：`rqrr`（纯 Rust、维护中）；`bardecoder` 已多年未更新故不选。
/// RGBA → luma 按 ITU-R BT.709 加权（image crate 行为，忽略 alpha），
/// 透明底 QR 需宿主先合成白底。解码失败的 grid 忽略，返回空。
pub fn scan_qr(img: &RgbaImage) -> Vec<String> {
    let luma = DynamicImage::ImageRgba8(img.clone()).into_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(luma);
    prepared
        .detect_grids()
        .iter()
        .filter_map(|g| g.decode().ok().map(|(_, text)| text))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ab_glyph::FontArc;
    use imageproc::drawing::draw_text_mut;
    use std::path::{Path, PathBuf};

    /// 找一个能渲染中文的字体（Noto Sans CJK 优先，回退任意 ttf/ttc/otf）。
    fn find_font() -> Option<PathBuf> {
        let roots = ["/usr/share/fonts", "/usr/local/share/fonts"];
        let prefer = [
            "NotoSansCJK-Regular",
            "NotoSansCJK-Bold",
            "NotoSansCJK",
            "DejaVuSans.ttf",
        ];
        for root in roots {
            let mut all: Vec<PathBuf> = Vec::new();
            collect_fonts(Path::new(root), &mut all);
            for key in prefer {
                if let Some(p) = all.iter().find(|p| p.to_string_lossy().contains(key)) {
                    return Some(p.clone());
                }
            }
            if let Some(p) = all
                .iter()
                .find(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("ttf" | "ttc" | "otf")))
            {
                return Some(p.clone());
            }
        }
        None
    }

    fn collect_fonts(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_fonts(&p, out);
            } else {
                out.push(p);
            }
        }
    }

    /// 白底黑字合成图：每行一条文本，返回 RGBA 图像。
    /// px 尺寸给大些（64px），OCR 检测模型对小字不友好。
    fn text_img(lines: &[&str]) -> RgbaImage {
        let font_path = find_font().expect("系统缺少可用字体（ttf/ttc/otf）");
        let bytes = std::fs::read(&font_path).expect("读取字体失败");
        // ttc 集合需要带索引加载
        let font = ab_glyph_from_bytes(bytes);

        let (w, line_h) = (1200u32, 110u32);
        let mut img = RgbaImage::from_pixel(w, line_h * lines.len() as u32 + 40, image::Rgba([255, 255, 255, 255]));
        for (i, text) in lines.iter().enumerate() {
            draw_text_mut(
                &mut img,
                image::Rgba([0, 0, 0, 255]),
                20,
                20 + line_h as i32 * i as i32,
                72.0,
                &font,
                text,
            );
        }
        img
    }

    /// ttc 字体集合按索引 0 加载，普通 ttf/otf 直接加载。
    fn ab_glyph_from_bytes(bytes: Vec<u8>) -> FontArc {
        if bytes.starts_with(b"ttcf") {
            let fv = ab_glyph::FontVec::try_from_vec_and_index(bytes, 0).expect("ttc 加载失败");
            FontArc::from(fv)
        } else {
            FontArc::try_from_vec(bytes).expect("字体加载失败")
        }
    }

    #[test]
    fn rgba_to_rgb_preserves_dimensions_and_color() {
        // 2x2：左上红、右上绿、左下蓝、右下不透明度干扰
        let img = RgbaImage::from_raw(
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, //
                0, 0, 255, 255, 1, 2, 3, 4,
            ],
        )
        .expect("缓冲长度正确");
        let rgb = rgba_to_rgb(&img);
        assert_eq!(rgb.dimensions(), (2, 2));
        assert_eq!(rgb.get_pixel(0, 0).0, [255, 0, 0]);
        assert_eq!(rgb.get_pixel(1, 0).0, [0, 255, 0]);
        assert_eq!(rgb.get_pixel(0, 1).0, [0, 0, 255]);
    }

    #[test]
    fn quad_of_falls_back_to_bbox_for_short_polygons() {
        let empty = BoundingBox::new(vec![]);
        assert_eq!(quad_of(&empty), [[0.0, 0.0]; 4]);

        let rect = BoundingBox::from_coords(1.0, 2.0, 5.0, 8.0);
        assert_eq!(
            quad_of(&rect),
            [[1.0, 2.0], [5.0, 2.0], [5.0, 8.0], [1.0, 8.0]]
        );
    }

    #[test]
    fn page_number_parses_pdftoppm_names() {
        fn mk(s: &str) -> &Path {
            Path::new(s)
        }
        assert_eq!(page_number_of(mk("page-1.png")), Some(1));
        assert_eq!(page_number_of(mk("page-12.png")), Some(12));
        // 总页数 ≥10 时 poppler 补零
        assert_eq!(page_number_of(mk("page-007.png")), Some(7));
        assert_eq!(page_number_of(mk("page.txt")), None);
        assert_eq!(page_number_of(mk("unrelated.png")), None);
    }

    /// 端到端：合成图 → OcrEngine::recognize → 非空且含期望文本。
    /// 首跑会下载模型（~2.4s，之后走 ~/.oar 缓存）。
    #[test]
    fn recognize_synthetic_text() {
        let img = text_img(&["你好世界", "Rumi OCR 123"]);
        let engine = OcrEngine::global();

        let t0 = std::time::Instant::now();
        engine.warmup().expect("引擎构建失败");
        eprintln!("warmup: {:?}", t0.elapsed());

        let t1 = std::time::Instant::now();
        let lines = engine.recognize(&img).expect("识别失败");
        eprintln!("recognize: {:?}，共 {} 行", t1.elapsed(), lines.len());
        for l in &lines {
            eprintln!("  [{:.3}] {} @{:?}", l.score, l.text, l.r#box[0]);
        }

        assert!(!lines.is_empty(), "识别结果为空");
        let joined: String = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
        assert!(
            joined.contains("123") || joined.contains("Rumi"),
            "未识别出期望文本，实际输出: {joined:?}"
        );
        // 坐标必须落在图像内
        let (w, h) = img.dimensions();
        for l in &lines {
            for pt in l.r#box {
                assert!(pt[0] >= 0.0 && pt[0] <= w as f32);
                assert!(pt[1] >= 0.0 && pt[1] <= h as f32);
            }
        }
    }

    /// 公式行合成辅助：指定框、LaTeX、置信度的 FormulaResult。
    fn formula(y: f32, latex: &str, score: f32) -> oar_ocr::domain::structure::FormulaResult {
        oar_ocr::domain::structure::FormulaResult::new(
            BoundingBox::from_coords(10.0, y, 200.0, y + 30.0),
            latex,
            score,
        )
    }

    /// 文字区合成辅助：一行文本 + 可选 formula 标记。
    fn region(y: f32, text: &str, label: Option<&str>) -> TextRegion {
        let mut tr = TextRegion::new(BoundingBox::from_coords(10.0, y, 200.0, y + 30.0));
        tr.text = Some(text.into());
        tr.confidence = Some(0.95);
        tr.label = label.map(Into::into);
        tr
    }

    /// 纯转换层：公式行（真实置信度）+ 文字行合并、跳过管线注入的
    /// formula 重复区、按框顶 y 排序。
    #[test]
    fn structure_lines_merges_and_sorts() {
        let mut result = StructureResult::new("t", 0);
        result.formulas = vec![formula(50.0, "x ^ {2}", 0.9)];
        result.text_regions = Some(vec![
            region(10.0, "标题", None),
            // 管线把公式注入 text_regions（label=formula、置信度 1.0）→ 必须跳过
            region(52.0, "x ^ {2}", Some("formula")),
            region(100.0, "正文", None),
        ]);

        let lines = structure_lines(&result);
        assert_eq!(
            lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            ["标题", "x ^ {2}", "正文"],
            "应按 y 排序合并公式与文字行"
        );
        let f = &lines[1];
        assert!((f.score - 0.9).abs() < 1e-6, "公式行用公式识别真实置信度");
        assert_eq!(f.r#box[0], [10.0, 50.0]);
    }

    /// 无公式、无文字 → 空行表（不报错）。
    #[test]
    fn structure_lines_empty_result() {
        let lines = structure_lines(&StructureResult::new("t", 0));
        assert!(lines.is_empty());
    }

    /// 端到端（真推理，默认忽略）：合成公式图 → recognize_structured。
    /// 首跑下载 PP-DocLayout-S + PP-FormulaNet-S（约 230 MiB，缓存 ~/.oar）。
    /// 手动跑：cargo test -p rumi-ocr recognize_structured_synthetic -- --ignored --nocapture
    /// 版面把整图判成 formula 时 text 是 LaTeX；判成 text 时是逐字 OCR——
    /// 两者都算管线通，故只断言非空且框在图内。
    #[test]
    #[ignore = "真推理：首次下载约 230 MiB 模型（~/.oar 缓存），手动执行"]
    fn recognize_structured_synthetic_formula() {
        let img = text_img(&["x^2 + y^2 = r^2"]);
        let engine = OcrEngine::global();

        let t0 = std::time::Instant::now();
        engine.warmup_structured().expect("公式引擎构建失败");
        eprintln!("构建公式引擎: {:?}", t0.elapsed());

        let lines = engine.recognize_structured(&img).expect("识别失败");
        eprintln!("共 {} 行：", lines.len());
        for l in &lines {
            eprintln!("  [{:.3}] {} @{:?}", l.score, l.text, l.r#box[0]);
        }
        assert!(!lines.is_empty(), "识别结果为空");
        let (w, h) = img.dimensions();
        for l in &lines {
            for pt in l.r#box {
                assert!(pt[0] >= 0.0 && pt[0] <= w as f32);
                assert!(pt[1] >= 0.0 && pt[1] <= h as f32);
            }
        }
    }

    /// 端到端（真推理，默认忽略）：两段文字图 → recognize → NaturalPara 分两段。
    /// text_img 行距 110 / 字号 72：段内间隙 ≈ 0.47×行高（同段），
    /// 空一行隔段间隙 ≈ 1.9×行高（断段）。守卫"中心距"旧 bug——
    /// 误用中心距时每行各自成段，groups 数 == 行数，本断言失败。
    /// 手动跑：cargo test -p rumi-ocr natural_para_real_engine -- --ignored --nocapture
    #[test]
    #[ignore = "真推理：依赖已缓存模型（~/.oar），手动执行"]
    fn natural_para_real_engine_two_paragraphs() {
        use crate::postprocess::{postprocess, Layout, PostRules};
        let img = text_img(&[
            "春眠不觉晓",
            "处处闻啼鸟",
            "", // 空行 = 段间距
            "夜来风雨声",
            "花落知多少",
        ]);
        let lines = OcrEngine::global().recognize(&img).expect("识别失败");
        eprintln!("共 {} 行：", lines.len());
        for l in &lines {
            eprintln!("  [{:.3}] {} @{:?}", l.score, l.text, l.r#box[0]);
        }
        assert!(lines.len() >= 3, "识别行数过少: {}", lines.len());
        let p = postprocess(&lines, Layout::NaturalPara, &PostRules::default());
        eprintln!("NaturalPara {} 段: {:?}", p.groups.len(), p.texts);
        assert_eq!(p.groups.len(), 2, "两段文字应恰好分两段（旧中心距 bug 会逐行成段）");
    }
}
