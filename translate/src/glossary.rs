//! 术语表：设置文本域 → 结构化条目 + 应用策略。
//!
//! v1 范围（glossary 规格 §0 写死）：文本域存储 + 单元文本**精确匹配**替换
//! + OpenAI 兼容引擎的 system prompt 注入（见 [`crate::docmt`] 判定链与
//! `online::openai`）；术语自动提取、模糊替换、翻译页文本框应用 = v2 不做。
//!
//! 容错口径：设置文本域是用户**编辑中态**，解析失败的行（无 `=`、两侧
//! trim 后为空）一律静默跳过——不该让一次翻译把应用炸掉。

/// 单条术语：原文（精确匹配键）→ 固定译文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlossaryTerm {
    pub src: String,
    pub dst: String,
}

/// 术语表（解析产物；空表 = 全部单元照常走翻译链路）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Glossary {
    pub terms: Vec<GlossaryTerm>,
}

impl Glossary {
    /// 解析设置文本域（"原文 = 译文" 行式；# 注释/空行/无 = 的行跳过）。
    ///
    /// 行规则（规格 §1 写死）：`=` 两侧空格容忍；`#` 开头行（trim 后）=
    /// 注释；空行忽略；`=` 取**第一个**（译文里再出现 `=` 属译文内容）；
    /// 无 `=` 或任一侧 trim 后为空的行视为解析失败，静默跳过。
    pub fn parse(text: &str) -> Self {
        let terms = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .map(|(src, dst)| GlossaryTerm {
                src: src.trim().to_string(),
                dst: dst.trim().to_string(),
            })
            .filter(|t| !t.src.is_empty() && !t.dst.is_empty())
            .collect();
        Self { terms }
    }

    /// 单元文本精确匹配某术语（trim 相等）→ 直接用术语译文（跳过引擎）。
    ///
    /// 大小写敏感（保守：缩写类术语如 "API"/"Api" 语义可能不同，不折叠）；
    /// 整单元匹配、不做子串/模糊替换（v2）；同原文多条时取先解析到的
    /// （find 语义，与文本域从上到下的直觉一致）。
    pub fn exact_match(&self, unit_text: &str) -> Option<&str> {
        let key = unit_text.trim();
        self.terms
            .iter()
            .find(|t| t.src.trim() == key)
            .map(|t| t.dst.as_str())
    }

    /// 非空且有条目。
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// LLM system prompt 注入段（openai 引擎用）：
    /// "\n\n术语表（翻译时必须遵循以下固定译法）：\n- src → dst\n…"
    /// 空表返回空串（调用方按 is_empty 守卫，双重安全）。
    pub fn prompt_block(&self) -> String {
        if self.terms.is_empty() {
            return String::new();
        }
        let mut out = String::from("\n\n术语表（翻译时必须遵循以下固定译法）：\n");
        for t in &self.terms {
            out.push_str(&format!("- {} → {}\n", t.src, t.dst));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// parse 基本形态（规格 §6 的冒烟锚，详测在 tests/glossary_tests.rs）。
    #[test]
    fn parse_and_match_smoke() {
        let g = Glossary::parse("thread pool = 线程池\n# 注释\n\nbad line");
        assert_eq!(g.terms.len(), 1);
        assert_eq!(g.exact_match("thread pool"), Some("线程池"));
        assert!(g.exact_match("执行绪").is_none());
    }
}
