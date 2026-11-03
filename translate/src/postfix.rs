//! zh 译文的保守标点润色（to == "zh" 时由调用方启用，纯函数可单测）。
//! 文章实测痛点：SentencePiece 空格标记 "▁" 泄漏、半角句点/逗号、
//! "N: M" 冒号插空格、标点前空格。

/// ASCII 字母或数字（R1 的「前后均为字母数字」豁免判据）。
fn is_alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

fn is_digit(c: char) -> bool {
    c.is_ascii_digit()
}

/// R4 适用：这些标点之前的空格删除。
fn is_space_eating_punct(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '，' | '、' | '：' | '；' | '）' | '】' | '》' | '"')
}

/// R5 适用：句末终结符（后跟 CJK 字符时删其前空格）。
fn is_sentence_end(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '!' | '?' | '…')
}

/// R5 适用：CJK 表意字符（基本区 + 扩展A + 兼容区 + 扩展B）。
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF      // CJK 扩展 A
        | 0x4E00..=0x9FFF    // CJK 基本区
        | 0xF900..=0xFAFF    // 兼容表意区
        | 0x20000..=0x2A6DF  // CJK 扩展 B
    )
}

/// 对 zh 译文做保守标点润色，规则按序：
/// - R0：SentencePiece 空格标记 "▁"（U+2581）还原为空格并去首尾（实测译文开头必泄漏）；
/// - R1：'.'→'。'，数字/字母数字上下文豁免（"8080."、"v1.2.3" 不转）；
/// - R2：','→'，'，数字豁免（"1,000" 不转）；
/// - R3：数字冒号 "N: M" 去空格（"8: 15"→"8:15"）；
/// - R4：`。！？，、：；）】》"` 前空格删除；
/// - R5：句末标点与后随 CJK 字符间的空格删除；
/// 其余空格一律保留（拉丁词间、中西文之间不动）。
pub fn polish_zh(text: &str) -> String {
    // R0：▁ → 空格再去首尾；后续规则的字符上下文都基于处理后的文本
    let s = text.replace('\u{2581}', " ");
    let s = s.trim();
    let chars: Vec<char> = s.chars().collect();

    // R1/R2：半角 '.' ',' → 全角，按原字符串的相邻字符判豁免
    let mut out: String = chars
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let prev = if i > 0 { Some(chars[i - 1]) } else { None };
            let next = chars.get(i + 1).copied();
            match c {
                '.' => {
                    // 豁免判据（规格 §4 修正版）：前后均为字母数字（"v1.2"）、
                    // 或任一侧是数字（"8080."、"x.2"）→ 不转
                    let both_alnum =
                        prev.is_some_and(is_alnum) && next.is_some_and(is_alnum);
                    let digit_touch =
                        prev.is_some_and(is_digit) || next.is_some_and(is_digit);
                    if both_alnum || digit_touch { '.' } else { '。' }
                }
                ',' => {
                    // "1,000" 数字保护
                    let digit_touch =
                        prev.is_some_and(is_digit) || next.is_some_and(is_digit);
                    if digit_touch { ',' } else { '，' }
                }
                _ => c,
            }
        })
        .collect();

    out = fix_digit_colon_space(&out); // R3
    out = drop_space_before_punct(&out); // R4
    drop_space_before_cjk(&out) // R5
}

/// R3：前一字符与空格后一字符均为数字的 ": " 去空格（"8: 15"→"8:15"，"127.0.0.1: 8080"）。
fn fix_digit_colon_space(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ':'
            && i > 0
            && is_digit(chars[i - 1])
            && chars.get(i + 1) == Some(&' ')
            && chars.get(i + 2).copied().is_some_and(is_digit)
        {
            out.push(':'); // 冒号后空格丢弃
            i += 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// R4：目标标点之前紧邻的连续空格删除（"秒 。"→"秒。"）。
fn drop_space_before_punct(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ' ' {
            let mut j = i;
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            if j < chars.len() && is_space_eating_punct(chars[j]) {
                i = j; // 整段空格丢弃
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// R5：句末标点与其后 CJK 字符之间的空格删除（仅 CJK，"。 Next" 不动）。
fn drop_space_before_cjk(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ' ' && i > 0 && is_sentence_end(chars[i - 1]) {
            let mut j = i;
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            if j < chars.len() && is_cjk(chars[j]) {
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}
