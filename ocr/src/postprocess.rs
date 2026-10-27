//! 识别后处理（P2 #11 排版解析 + #12 忽略规则）：纯函数，无引擎依赖。
//!
//! 对齐 Umi-OCR 的"排版解析"三种输出策略：
//! - [`Layout::ByLine`]：逐行输出（默认，等于不合并）；
//! - [`Layout::MergePara`]：全部行合并成一个段落；
//! - [`Layout::NaturalPara`]：按行间垂直间距聚类分段（间距 > 行高 × 0.8 视为分段），
//!   段内合并规则与 MergePara 相同。
//!
//! 合并拼接规则：两段文本相连时，若前段末字符与后段首字符**都是 CJK**
//! （统一表意文字 / 假名 / 谚文 / 中文标点 / 全角形式）则直接相连，
//! 否则插入一个空格（拉丁词间需要分隔，如"你好 World"）。
//!
//! 忽略规则：[`PostRules::ignore_regex`] 中任一正则对整行文本命中
//! （`is_match` 搜索语义，可用 `^…$` 锚定全行）的行被剔除并计入
//! [`Processed::ignored`]；编译失败的正则跳过并记入 [`Processed::bad_regex`]。

use crate::OcrLine;
use regex::Regex;

/// 排版解析策略（Umi"文本后处理"的三种输出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    /// 逐行输出（默认）
    #[default]
    ByLine,
    /// 全部行合并成一个段落
    MergePara,
    /// 按行垂直间距聚类分段
    NaturalPara,
}

/// 后处理规则（忽略规则）。
#[derive(Debug, Clone, Default)]
pub struct PostRules {
    /// 忽略正则列表：任一正则整行命中的行被剔除。
    /// 编译失败项被忽略，并在 [`Processed::bad_regex`] 上报原文。
    pub ignore_regex: Vec<String>,
}

/// 后处理结果。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Processed {
    /// 输出文本：ByLine 时与保留行一一对应；两种合并模式下每段一条。
    pub texts: Vec<String>,
    /// 被忽略规则剔除的行数
    pub ignored: usize,
    /// 编译失败、被跳过的正则原文（与 [`PostRules::ignore_regex`] 顺序无关）
    pub bad_regex: Vec<String>,
    /// 每条输出对应的**输入** `lines` 下标（保持输入顺序；过滤后下标会错位，
    /// 故用原始下标，供调用方回填 box/score 而不必复制分组逻辑）：
    /// ByLine 时一一对应，合并模式下是段内成员的原始下标。
    pub groups: Vec<Vec<usize>>,
}

/// 识别后处理：先按忽略规则过滤行，再按排版策略合并/分段。
pub fn postprocess(lines: &[OcrLine], layout: Layout, rules: &PostRules) -> Processed {
    let (regexes, bad_regex) = compile_rules(rules);
    let mut ignored = 0usize;
    // 过滤后仍保留 (原始下标, 行) 对：分组要映射回原始下标
    let kept: Vec<(usize, &OcrLine)> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            let hit = regexes.iter().any(|re| re.is_match(&l.text));
            if hit {
                ignored += 1;
            }
            !hit
        })
        .collect();

    let groups: Vec<Vec<usize>> = match layout {
        Layout::ByLine => kept.iter().map(|(i, _)| vec![*i]).collect(),
        Layout::MergePara => {
            if kept.is_empty() {
                Vec::new()
            } else {
                vec![kept.iter().map(|(i, _)| *i).collect()]
            }
        }
        Layout::NaturalPara => {
            let mut gs: Vec<Vec<usize>> = Vec::new();
            for (k, (idx, line)) in kept.iter().enumerate() {
                let same_para = k > 0 && !paragraph_break(kept[k - 1].1, line);
                if same_para {
                    gs.last_mut().expect("k > 0 时首段已建").push(*idx);
                } else {
                    gs.push(vec![*idx]);
                }
            }
            gs
        }
    };

    let texts: Vec<String> = groups
        .iter()
        .map(|g| join_paragraph(g.iter().map(|&i| lines[i].text.as_str())))
        .collect();

    Processed { texts, ignored, bad_regex, groups }
}

/// 编译忽略正则：失败项与其余正则分离（跳过 + 上报，不整体失败）。
fn compile_rules(rules: &PostRules) -> (Vec<Regex>, Vec<String>) {
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    for pat in &rules.ignore_regex {
        match Regex::new(pat) {
            Ok(re) => ok.push(re),
            Err(_) => bad.push(pat.clone()),
        }
    }
    (ok, bad)
}

/// 相邻两行是否应分段：**真实行间隙**（上一行最低点 → 下一行最高点）
/// > 行高 × 0.8。行高取两行 y 向跨度的较大者（对轻微倾斜 / 不均匀框更稳）。
///
/// 陷阱（曾实现错）：不能用行**中心距**——正常行距下中心距 ≈ 1.2~1.5×行高，
/// 恒大于 0.8 阈值，会把同段每一行都断成单段（自然分段退化成按行输出）。
/// 行框重叠（间隙 ≤ 0）视为同段。
fn paragraph_break(prev: &OcrLine, next: &OcrLine) -> bool {
    let (top0, bot0) = line_y_extent(&prev.r#box);
    let (top1, bot1) = line_y_extent(&next.r#box);
    let h = (bot0 - top0).max(bot1 - top1);
    (top1 - bot0).max(0.0) > h * 0.8
}

/// 四点框的 y 范围（最高点 / 最低点）。
fn line_y_extent(b: &[[f32; 2]; 4]) -> (f32, f32) {
    let ys = [b[0][1], b[1][1], b[2][1], b[3][1]];
    let min = ys.iter().cloned().fold(f32::INFINITY, f32::min);
    let max = ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    (min, max)
}

/// 段内拼接：边界两侧都是 CJK 时直接相连，否则补一个空格；空段跳过。
fn join_paragraph<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    let mut s = String::new();
    for part in parts {
        if part.is_empty() {
            continue;
        }
        if !s.is_empty() {
            let prev_cjk = s.chars().next_back().is_some_and(is_cjk);
            let next_cjk = part.chars().next().is_some_and(is_cjk);
            if !(prev_cjk && next_cjk) {
                s.push(' ');
            }
        }
        s.push_str(part);
    }
    s
}

/// 是否 CJK 字符：统一表意文字（含扩展/兼容区）、日文假名、韩文谚文、
/// 中文标点与全角形式。
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F     // CJK 符号与标点（，。、《》）
        | 0x3040..=0x30FF   // 平假名 / 片假名
        | 0x3400..=0x4DBF   // 扩展 A
        | 0x4E00..=0x9FFF   // CJK 统一表意文字
        | 0xAC00..=0xD7AF   // 谚文音节
        | 0xF900..=0xFAFF   // 兼容表意文字
        | 0xFF00..=0xFFEF   // 全角形式（！（）ａｂｃ）
        | 0x20000..=0x2A6DF // 扩展 B
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合成行：矩形框，左上角 (x, y)，高 h，宽固定 100。
    fn line(text: &str, y: f32) -> OcrLine {
        line_h(text, 0.0, y, 30.0)
    }

    fn line_h(text: &str, x: f32, y: f32, h: f32) -> OcrLine {
        OcrLine {
            text: text.into(),
            score: 1.0,
            r#box: [[x, y], [x + 100.0, y], [x + 100.0, y + h], [x, y + h]],
        }
    }

    fn no_rules() -> PostRules {
        PostRules::default()
    }

    #[test]
    fn by_line_outputs_one_text_per_line() {
        let lines = [line("你好", 0.0), line("World", 20.0), line("继续", 40.0)];
        let p = postprocess(&lines, Layout::ByLine, &no_rules());
        assert_eq!(p.texts, vec!["你好", "World", "继续"]);
        assert_eq!(p.groups, vec![vec![0], vec![1], vec![2]]);
        assert_eq!((p.ignored, p.bad_regex.len()), (0, 0));
    }

    #[test]
    fn merge_para_joins_cjk_lines_without_space() {
        let lines = [line("你好", 0.0), line("世界", 20.0)];
        let p = postprocess(&lines, Layout::MergePara, &no_rules());
        assert_eq!(p.texts, vec!["你好世界"]);
        assert_eq!(p.groups, vec![vec![0, 1]]);
    }

    #[test]
    fn merge_para_inserts_space_at_cjk_latin_boundary() {
        let p = postprocess(&[line("你好", 0.0), line("World", 20.0)], Layout::MergePara, &no_rules());
        assert_eq!(p.texts, vec!["你好 World"]);
        let p = postprocess(&[line("Hello", 0.0), line("世界", 20.0)], Layout::MergePara, &no_rules());
        assert_eq!(p.texts, vec!["Hello 世界"]);
        // 两个拉丁行也要空格
        let p = postprocess(&[line("Rumi", 0.0), line("OCR", 20.0)], Layout::MergePara, &no_rules());
        assert_eq!(p.texts, vec!["Rumi OCR"]);
    }

    #[test]
    fn natural_para_clusters_by_vertical_gap() {
        // 真实几何：行高 30、行距 40（间隙 10 ≤ 阈值 24 → 同段）；
        // 段间：上行底 110 → 下行顶 170（间隙 60 > 24 → 断段）。
        // 注意行距 40 > 行高 30：若误用"中心距"判定（旧 bug），40 > 24 会把
        // 同段也逐行断开——本用例即该回归的守卫。
        let lines = [
            line("你好", 0.0),    // 占 0..30
            line("世界", 40.0),   // 占 40..70，间隙 10
            line("继续", 80.0),   // 占 80..110，间隙 10
            line("abc", 170.0),   // 间隙 60 → 新段
            line("def", 210.0),   // 间隙 10
        ];
        let p = postprocess(&lines, Layout::NaturalPara, &no_rules());
        assert_eq!(p.groups, vec![vec![0, 1, 2], vec![3, 4]]);
        assert_eq!(p.texts, vec!["你好世界继续", "abc def"]);
    }

    #[test]
    fn natural_para_gap_exactly_at_threshold_stays_merged() {
        // 行高 40 → 阈值 32；间隙恰为 32（上行底 40 → 下行顶 72），不大于 → 仍是一段
        let lines = [line_h("上", 0.0, 0.0, 40.0), line_h("海", 0.0, 72.0, 40.0)];
        let p = postprocess(&lines, Layout::NaturalPara, &no_rules());
        assert_eq!(p.groups, vec![vec![0, 1]]);
        assert_eq!(p.texts, vec!["上海"]);
    }

    #[test]
    fn natural_para_overlapping_boxes_stay_merged() {
        // 行框轻微重叠（下行顶 25 < 上行底 30）：间隙 ≤ 0 视为同段
        let lines = [line_h("甲", 0.0, 0.0, 30.0), line_h("乙", 0.0, 25.0, 30.0)];
        let p = postprocess(&lines, Layout::NaturalPara, &no_rules());
        assert_eq!(p.groups, vec![vec![0, 1]]);
        assert_eq!(p.texts, vec!["甲乙"]);
    }

    #[test]
    fn natural_para_zero_boxes_stay_one_paragraph() {
        // 插件路径可能给全零占位框：间距 0、行高 0 → 0 > 0 不成立 → 全并一段
        let zero = OcrLine { text: "a".into(), score: 1.0, r#box: [[0.0; 2]; 4] };
        let two = [zero, OcrLine { text: "b".into(), score: 1.0, r#box: [[0.0; 2]; 4] }];
        let p = postprocess(&two, Layout::NaturalPara, &no_rules());
        assert_eq!(p.texts, vec!["a b"]);
    }

    #[test]
    fn ignore_regex_drops_matched_lines_and_counts() {
        let lines = [line("keep me", 0.0), line("SECRET line", 20.0), line("ok", 40.0)];
        let rules = PostRules { ignore_regex: vec!["SECRET".into()] };
        let p = postprocess(&lines, Layout::ByLine, &rules);
        assert_eq!(p.texts, vec!["keep me", "ok"]);
        assert_eq!(p.groups, vec![vec![0], vec![2]]);
        assert_eq!(p.ignored, 1);
        assert!(p.bad_regex.is_empty());
    }

    #[test]
    fn bad_regex_reported_and_good_ones_still_applied() {
        let lines = [line("one", 0.0), line("SECRET", 20.0)];
        let rules = PostRules { ignore_regex: vec!["([unclosed".into(), "SECRET".into()] };
        let p = postprocess(&lines, Layout::ByLine, &rules);
        assert_eq!(p.bad_regex, vec!["([unclosed"]);
        assert_eq!(p.texts, vec!["one"]);
        assert_eq!(p.ignored, 1);
    }

    #[test]
    fn ignore_runs_before_layout_merge() {
        // 过滤先行：被剔除的行不参与合并，groups 记录的是原始下标
        let lines = [line("删掉", 0.0), line("下一行", 20.0)];
        let rules = PostRules { ignore_regex: vec!["^删".into()] };
        let p = postprocess(&lines, Layout::MergePara, &rules);
        assert_eq!(p.texts, vec!["下一行"]);
        assert_eq!(p.groups, vec![vec![1]]);
        assert_eq!(p.ignored, 1);
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let p = postprocess(&[], Layout::MergePara, &no_rules());
        assert_eq!(p, Processed::default());
    }
}
