//! glossary 纯逻辑单测（glossary 规格 §6，不打网不跑模型）：parse 行式容错
//! （正常/注释/空行/无 =/多空格）、exact_match（trim 相等、大小写敏感、
//! 整单元匹配）、prompt_block 注入段格式、is_empty 空表语义。
//! docmt 侧链路集成（from==to 直通 + 精确命中）见 docmt_tests.rs。

use rumi_translate::glossary::{Glossary, GlossaryTerm};

/// parse 正常形态："原文 = 译文" 逐行成对，两侧 trim。
#[test]
fn parse_normal_pairs_trimmed() {
    let g = Glossary::parse("thread pool = 线程池\nneural network=神经网络");
    assert_eq!(g.terms.len(), 2);
    assert_eq!(
        g.terms[0],
        GlossaryTerm { src: "thread pool".into(), dst: "线程池".into() }
    );
    assert_eq!(
        g.terms[1],
        GlossaryTerm { src: "neural network".into(), dst: "神经网络".into() }
    );
}

/// `=` 两侧多空格容忍（规格 §1）。
#[test]
fn parse_tolerates_spaces_around_equals() {
    let g = Glossary::parse("  thread pool   =   线程池  ");
    assert_eq!(
        g.terms,
        vec![GlossaryTerm { src: "thread pool".into(), dst: "线程池".into() }]
    );
}

/// `#` 开头行（含缩进注释）= 注释、空行忽略（规格 §1）。
#[test]
fn parse_skips_comments_and_blank_lines() {
    let g = Glossary::parse("# 注释行\n\n   \nthread pool = 线程池\n  # 缩进注释");
    assert_eq!(g.terms.len(), 1, "注释与空行应全部跳过：{:?}", g.terms);
    assert_eq!(g.terms[0].src, "thread pool");
    assert_eq!(g.terms[0].dst, "线程池");
}

/// 解析失败的行静默跳过（规格 §1：编辑中态不该炸应用）：无 `=`、
/// `=` 任一侧 trim 后为空。
#[test]
fn parse_skips_unparsable_lines_silently() {
    let g = Glossary::parse("没有等号的行\n= 只有译文\n只有原文 =\n=\nok = 好的");
    assert_eq!(
        g.terms,
        vec![GlossaryTerm { src: "ok".into(), dst: "好的".into() }],
        "无 = 或侧空的行应静默跳过：{:?}",
        g.terms
    );
}

/// `=` 取第一个：译文里再出现 `=` 属译文内容。
#[test]
fn parse_uses_first_equals_as_separator() {
    let g = Glossary::parse("a=b=c");
    assert_eq!(g.terms[0].src, "a");
    assert_eq!(g.terms[0].dst, "b=c");
}

/// 空文本域/纯空白/纯注释 → 空表；Default 恒空。
#[test]
fn empty_text_parses_to_empty_glossary() {
    assert!(Glossary::parse("").is_empty());
    assert!(Glossary::parse("   \n\n").is_empty());
    assert!(Glossary::parse("# 只有注释\n").is_empty());
    assert!(Glossary::default().is_empty());
    assert!(Glossary::default().terms.is_empty());
}

/// exact_match 命中：trim 相等即返回固定译文（单元带排版空白也命中）。
#[test]
fn exact_match_hit_returns_dst() {
    let g = Glossary::parse("thread pool = 线程池");
    assert_eq!(g.exact_match("thread pool"), Some("线程池"));
    assert_eq!(g.exact_match("  thread pool  "), Some("线程池"), "trim 相等应命中");
}

/// exact_match 边界：大小写敏感；子串不算命中（精确匹配，模糊替换 = v2）。
#[test]
fn exact_match_case_sensitive_and_whole_unit_only() {
    let g = Glossary::parse("Thread Pool = 线程池");
    assert_eq!(g.exact_match("thread pool"), None, "大小写不同不应命中");
    assert_eq!(
        g.exact_match("the thread pool rocks"),
        None,
        "子串不是整单元匹配，不应命中"
    );
}

/// exact_match：空表恒 None；同原文多条取先解析到的（find 语义）。
#[test]
fn exact_match_empty_none_and_first_term_wins() {
    assert_eq!(Glossary::default().exact_match("anything"), None);
    let dup = Glossary::parse("api = 接口\napi = 应用程序接口");
    assert_eq!(dup.exact_match("api"), Some("接口"), "同 src 应取先解析到的");
}

/// prompt_block 格式（规格 §2 写死）：\n\n 头 + 逐条 "- src → dst\n"。
#[test]
fn prompt_block_format_matches_spec() {
    let g = Glossary::parse("thread pool = 线程池\napi = 接口");
    assert_eq!(
        g.prompt_block(),
        "\n\n术语表（翻译时必须遵循以下固定译法）：\n- thread pool → 线程池\n- api → 接口\n"
    );
}

/// prompt_block：空表返回空串（调用方拼不拼都安全）。
#[test]
fn prompt_block_empty_glossary_is_empty_string() {
    assert_eq!(Glossary::default().prompt_block(), "");
    assert_eq!(Glossary::parse("# 全是注释").prompt_block(), "");
}
