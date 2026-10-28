"""敏感信息打码 —— 基于 OcrManager 识别结果对指定区域做马赛克/模糊/纯色遮盖。

数据来源全部为 IPC 调用返回值 (ocrResult 文本框 + qrcode 角点), 无需任何逆向模型。

用法:
    from wechat_ocr.ocr_manager import OcrManager
    from wechat_ocr.mosaic import apply_mosaic
    from PIL import Image

    results = {}   # ocr 回调拿到的结果 dict
    img = apply_mosaic("screenshot.png", results,
                       patterns="default",   # 内置 手机号/身份证/邮箱 正则
                       qrcode=True,          # 同时打掉二维码
                       style="pixel")        # pixel | blur | solid
    img.save("masked.png")

patterns 说明:
  - "default": 使用内置正则 (手机号 / 身份证 / 邮箱)
  - list[str|Pattern]: 自定义正则列表 (命中即打码整行文本框)
  - None: 不按文本打码, 仅打二维码区域
"""
from __future__ import annotations

import re
from PIL import Image, ImageFilter

__all__ = ["DEFAULT_PATTERNS", "apply_mosaic", "mosaic_file"]

DEFAULT_PATTERNS = {
    "phone": re.compile(r"1[3-9]\d{9}"),
    "id_card": re.compile(r"\d{17}[\dXx]"),
    "email": re.compile(r"[\w.+-]+@[\w-]+\.[\w.]+"),
}


def _normalize_patterns(patterns):
    if patterns is None:
        return []
    if patterns == "default":
        return list(DEFAULT_PATTERNS.values())
    out = []
    for p in patterns:
        out.append(re.compile(p) if isinstance(p, str) else p)
    return out


def _collect_regions(results, patterns, include_qrcode, pad):
    """从识别结果里收集需要打码的像素区域 (left, top, right, bottom)。"""
    regions = []

    for item in results.get("ocrResult", []):
        text = item.get("text", "") or ""
        if patterns and any(p.search(text) for p in patterns):
            loc = item["location"]
            regions.append((loc["left"], loc["top"], loc["right"], loc["bottom"]))

    if include_qrcode:
        for qr in results.get("qrcode", []):
            c = qr.get("corners", {})
            xs, ys = [], []
            for key in ("topleft", "topright", "bottomright", "bottomleft"):
                pt = c.get(key, {})
                xs.append(pt.get("x", 0))
                ys.append(pt.get("y", 0))
            regions.append((min(xs), min(ys), max(xs), max(ys)))

    # 外扩 pad 像素, 并裁剪到合法矩形
    out = []
    for l, t, r, b in regions:
        out.append((max(0, int(l) - pad), max(0, int(t) - pad),
                    int(r) + pad, int(b) + pad))
    return out


def _pixelate(region: Image.Image, block: int) -> Image.Image:
    w, h = region.size
    small = region.resize((max(1, w // block), max(1, h // block)), Image.NEAREST)
    return small.resize((w, h), Image.NEAREST)


def _mosaic_region(img: Image.Image, box, style: str, block: int, color):
    l, t, r, b = box
    r = min(r, img.width)
    b = min(b, img.height)
    if r - l <= 1 or b - t <= 1:
        return
    region = img.crop((l, t, r, b))
    if style == "pixel":
        masked = _pixelate(region, block)
    elif style == "blur":
        masked = region.filter(ImageFilter.GaussianBlur(radius=block))
    elif style == "solid":
        masked = Image.new(region.mode, region.size, color or (0, 0, 0))
    else:
        raise ValueError(f"未知打码样式: {style!r} (可选 pixel / blur / solid)")
    img.paste(masked, (l, t))


def apply_mosaic(img, results: dict, patterns="default", qrcode: bool = True,
                 style: str = "pixel", block: int = 12, pad: int = 2,
                 color=None) -> Image.Image:
    """按识别结果对图片打码, 返回新的 PIL Image。

    参数:
        img:       图片路径或 PIL.Image
        results:   OcrManager 回调返回的结果 dict
        patterns:  "default" / 正则列表 / None (见模块文档)
        qrcode:    是否打掉二维码区域
        style:     "pixel" 马赛克 | "blur" 高斯模糊 | "solid" 纯色
        block:     马赛克块大小 / 模糊半径 (像素)
        pad:       区域外扩像素, 避免边缘残留
        color:     style="solid" 时的填充色, 默认黑色
    """
    if isinstance(img, str):
        img = Image.open(img)
    img = img.convert("RGB") if img.mode not in ("RGB", "RGBA") else img.copy()

    regs = _collect_regions(results, _normalize_patterns(patterns), qrcode, pad)
    for box in regs:
        _mosaic_region(img, box, style, block, color)
    return img


def mosaic_file(src: str, dst: str, results: dict, **kwargs) -> Image.Image:
    """便捷函数: 读入图片打码后保存到 dst, 返回处理后的 Image。"""
    out = apply_mosaic(src, results, **kwargs)
    out.save(dst)
    return out
