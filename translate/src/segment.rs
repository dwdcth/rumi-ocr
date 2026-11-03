//! 规则分句：不引入 Stanza（其初始化需联网，见 docs/translate-design.md 选型）。
//!
//! 规则（按序应用，规格 §3 写死）：
//! 1. 先按 '\n' 切行（行是第一层单元，行结构由调用方另行保留）；
//! 2. 行内按终结符切：终结符留在前段，连续终结符并入前段；
//! 3. 超过 [`MAX_SEG_CHARS`] 的段按次级标点就近二分，递归直到 ≤300 或无标点可切；
//! 4. 不产生空段；整行空白产出单个空段（由调用方按行结构处理）。

/// 单段最大字符数（chars 口径，非字节）。
const MAX_SEG_CHARS: usize = 300;
/// 长段切分点搜索下界：切点必须落在超过 150 字符处，保证前段不会过碎。
const MIN_SPLIT_POS: usize = 150;

/// 句末终结符。规格 §3 列表未含 ASCII '.'，但写死用例 "Set X. Then start it!"
/// 要求按 '.' 断句，以用例为准将其计入。
fn is_terminal(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '.' | '!' | '?' | '…' | '；' | ';')
}

/// ASCII '.' 是否真的构成断句点：下一字符是字母/数字时不算——
/// 否则 "127.0.0.1"、"app.log"、"v1.2.3" 会被切碎，模型逐段翻译必然改坏
///（微信实测文章的已知痛点，见 docs/translate-design.md）。". Next"、"8080. Set"
/// 等下一字符是空格/EOL 的正常句尾不受影响。
fn dot_is_boundary(next: Option<char>) -> bool {
    !matches!(next, Some(c) if c.is_ascii_alphanumeric())
}

/// 次级标点（长段二分用），标点本身留在前段。
fn is_secondary(c: char) -> bool {
    matches!(c, '，' | ',' | '、' | ':' | '：')
}

/// 把文本切成可独立翻译的段（多行文本按行顺序拍平）。
pub fn segment(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for segs in segment_lines(text) {
        out.extend(segs);
    }
    out
}

/// 便捷封装：多行文本 → Vec<Vec<String>>（外层=行，内层=该行的段）。
/// 空/纯空白行产出 `vec![String::new()]`，保留行结构供调用方处理。
pub fn segment_lines(text: &str) -> Vec<Vec<String>> {
    text.split('\n').map(segment_line).collect()
}

/// 单行内分句。
fn segment_line(line: &str) -> Vec<String> {
    // 整行空白：按规格 §3 修正，产出单个空串段
    if line.trim().is_empty() {
        return vec![String::new()];
    }
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut out: Vec<String> = Vec::new();
    let mut start = 0usize; // 当前段起始字节偏移
    let mut i = 0usize;
    while i < chars.len() {
        if is_terminal(chars[i].1) {
            // 连续终结符（"！！""？！"）并入前段，边界落在终结符串之后；
            // ASCII '.' 需通过 dot_is_boundary（下一字符非字母数字才算句界）
            let mut j = i + 1;
            while j < chars.len() && is_terminal(chars[j].1) {
                j += 1;
            }
            let next = chars.get(j).map(|&(_, c)| c);
            if chars[i].1 == '.' && !dot_is_boundary(next) {
                i += 1;
                continue;
            }
            let end = if j < chars.len() { chars[j].0 } else { line.len() };
            out.push(line[start..end].to_string());
            start = end;
            i = j;
        } else {
            i += 1;
        }
    }
    if start < line.len() {
        out.push(line[start..].to_string());
    }
    // 超长段再按次级标点二分
    out.into_iter().flat_map(|s| split_long(&s)).collect()
}

/// 段长 >300 时二分：在 >150 字符处找最近的次级标点，标点留前段，两侧递归。
/// 切点上限 299 保证前段 ≤300——否则前段递归会再次命中同一切点而死循环；
/// 搜索范围内无标点即规格的「无标点可切」，超长段原样保留（交给模型上下文）。
fn split_long(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= MAX_SEG_CHARS {
        return vec![s.to_string()];
    }
    let cut = (MIN_SPLIT_POS + 1..=MAX_SEG_CHARS - 1).find(|&i| is_secondary(chars[i]));
    match cut {
        None => vec![s.to_string()],
        Some(i) => {
            let mut out = split_long(&chars[..=i].iter().collect::<String>());
            out.extend(split_long(&chars[i + 1..].iter().collect::<String>()));
            out
        }
    }
}
