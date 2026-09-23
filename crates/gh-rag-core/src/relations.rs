//! M2.5 relations:issue/PR 正文与标题的 fixes/closes/refs 提及解析与入库。
//!
//! 解析规则(AGENTS M2.5 拍板):
//! - 提及形态:`#123`(目标仓 = 当前仓)、`owner/repo#123`(跨仓)、
//!   markdown 链接文本形态 `[#123](…)`(等价 `#123`)。
//! - kind 判定:同句(换行/中英句读切分)出现修复类关键词(fix/fixes/fixed/
//!   resolve(s|d)/修复/解决)→ fixes;关闭类(close(s|d)/关闭)→ closes;
//!   否则 refs。references/参考类关键词不升级,即 refs(默认)。
//! - 自引用(目标 = 自身)排除;目标编号本库不存在也照存(检索时再交叉)。

/// 关联类型:fixes / closes / refs。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    Fixes,
    Closes,
    Refs,
}

impl RelationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RelationKind::Fixes => "fixes",
            RelationKind::Closes => "closes",
            RelationKind::Refs => "refs",
        }
    }
}

/// 一条关联:从 (repo, number) 指向 target_repo#target_number。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub kind: RelationKind,
    pub target_repo: String,
    pub target_number: i64,
}

/// 解析 title+body 中的 issue/PR 提及。`number` 为自身编号(排除自引用)。
pub fn parse_mentions(repo: &str, number: i64, title: &str, body: &str) -> Vec<Relation> {
    let _ = (repo, number, title, body);
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(v: &[Relation]) -> Vec<(&str, &str, i64)> {
        v.iter()
            .map(|r| (r.kind.as_str(), r.target_repo.as_str(), r.target_number))
            .collect()
    }

    #[test]
    fn no_mention_returns_empty() {
        assert!(parse_mentions("o/r", 1, "纯标题", "没有提及的正文。").is_empty());
        assert!(parse_mentions("o/r", 1, "", "").is_empty());
        // 非提及的 # 片段(url 锚点、纯 #)不算
        assert!(parse_mentions("o/r", 1, "t", "见 https://example.com/x#anchor 说明").is_empty());
    }

    #[test]
    fn plain_mention_is_refs() {
        let r = parse_mentions("o/r", 1, "标题", "相关 #12");
        assert_eq!(kinds(&r), [("refs", "o/r", 12)]);
    }

    #[test]
    fn english_fix_keywords() {
        for kw in ["fixes", "fix", "resolves", "fixed"] {
            let r = parse_mentions("o/r", 1, "", &format!("{kw} #3"));
            assert_eq!(kinds(&r), [("fixes", "o/r", 3)], "keyword={kw}");
        }
    }

    #[test]
    fn chinese_fix_keywords() {
        for kw in ["修复", "解决"] {
            let r = parse_mentions("o/r", 1, "", &format!("{kw} #3"));
            assert_eq!(kinds(&r), [("fixes", "o/r", 3)], "keyword={kw}");
        }
        let r = parse_mentions("o/r", 1, "", "关闭 #5");
        assert_eq!(kinds(&r), [("closes", "o/r", 5)]);
    }

    #[test]
    fn close_keywords_map_to_closes() {
        for kw in ["closes", "close", "closed"] {
            let r = parse_mentions("o/r", 1, "", &format!("{kw} #7"));
            assert_eq!(kinds(&r), [("closes", "o/r", 7)], "keyword={kw}");
        }
    }

    #[test]
    fn cross_repo_full_form() {
        let r = parse_mentions("o/r", 1, "", "fixes other/repo#9");
        assert_eq!(kinds(&r), [("fixes", "other/repo", 9)]);
        // 中文关键词跨仓同样生效
        let r = parse_mentions("o/r", 1, "", "修复 other/repo#9 的问题");
        assert_eq!(kinds(&r), [("fixes", "other/repo", 9)]);
    }

    #[test]
    fn markdown_link_form() {
        let r = parse_mentions("o/r", 1, "", "见 [#4](https://github.com/o/r/issues/4)");
        assert_eq!(kinds(&r), [("refs", "o/r", 4)]);
        let r = parse_mentions(
            "o/r",
            1,
            "",
            "fixes [x/repo#8](https://github.com/x/repo/issues/8)",
        );
        assert_eq!(kinds(&r), [("fixes", "x/repo", 8)]);
    }

    #[test]
    fn same_sentence_multi_mentions_share_kind() {
        let r = parse_mentions("o/r", 10, "", "fixes #3 and #4");
        assert_eq!(kinds(&r), [("fixes", "o/r", 3), ("fixes", "o/r", 4)]);
        // 不同句各自判定:第一句 refs、第二句 fixes
        let r = parse_mentions("o/r", 10, "", "相关 #1\nfixes #2");
        assert_eq!(kinds(&r), [("refs", "o/r", 1), ("fixes", "o/r", 2)]);
    }

    #[test]
    fn self_reference_excluded() {
        let r = parse_mentions("o/r", 3, "t", "fixes #3, refs #4");
        assert_eq!(kinds(&r), [("refs", "o/r", 4)]);
        // 跨仓同号不是自引用
        let r = parse_mentions("o/r", 3, "t", "fixes other/r#3");
        assert_eq!(kinds(&r), [("fixes", "other/r", 3)]);
    }

    #[test]
    fn title_counts_too_and_duplicates_dedup() {
        let r = parse_mentions("o/r", 1, "fix #2", "正文再次 fix #2");
        assert_eq!(kinds(&r), [("fixes", "o/r", 2)]);
    }

    #[test]
    fn mention_inside_word_not_matched() {
        // 字母数字紧贴 # 前不是提及(如 base64、版本号)
        assert!(parse_mentions("o/r", 1, "t", "abc#12 不算").is_empty());
        assert!(parse_mentions("o/r", 1, "t", "v1#12 不算").is_empty());
    }
}
