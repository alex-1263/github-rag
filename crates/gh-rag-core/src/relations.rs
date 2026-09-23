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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    let mut out: Vec<Relation> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for sentence in sentences(title, body) {
        let kind = sentence_kind(&sentence);
        for (raw_repo, n) in mentions(&sentence) {
            let target_repo = if raw_repo.is_empty() {
                repo.to_string()
            } else {
                raw_repo
            };
            // 自引用排除;跨仓同号不算
            if n == number && target_repo == repo {
                continue;
            }
            let rel = Relation {
                kind,
                target_repo,
                target_number: n,
            };
            if seen.insert((rel.kind, rel.target_repo.clone(), rel.target_number)) {
                out.push(rel);
            }
        }
    }
    out
}

/// 切句:换行 + 中英常见句读。关键词判定以句为单位(同句提及共享 kind)。
fn sentences(title: &str, body: &str) -> Vec<String> {
    let joined = format!("{title}\n{body}");
    joined
        .split([
            '\n', '\r', '。', '!', '?', '.', '!', '?', ';', ';', ',', ',',
        ])
        .map(str::to_string)
        .collect()
}

/// 句内 kind:修复类 → Fixes;关闭类 → Closes;否则 Refs(references/参考不升级)。
fn sentence_kind(sentence: &str) -> RelationKind {
    let s = sentence.to_ascii_lowercase();
    const FIX_WORDS: [&str; 7] = [
        "fix", "fixes", "fixed", "resolve", "resolves", "修复", "解决",
    ];
    const CLOSE_WORDS: [&str; 4] = ["close", "closes", "closed", "关闭"];
    if FIX_WORDS.iter().any(|w| contains_word(&s, w)) {
        return RelationKind::Fixes;
    }
    if CLOSE_WORDS.iter().any(|w| contains_word(&s, w)) {
        return RelationKind::Closes;
    }
    RelationKind::Refs
}

/// 词边界包含:英文关键词要求两侧非 ASCII 字母数字(如 `fixed:` 命中,`prefixfix` 不命中);
/// 中文关键词直接包含。
fn contains_word(hay: &str, word: &str) -> bool {
    let bytes = hay.as_bytes();
    let wb = word.as_bytes();
    let is_alnum = |b: u8| b.is_ascii_alphanumeric();
    let latin = wb[0].is_ascii_alphabetic();
    let mut from = 0;
    while let Some(pos) = hay[from..].find(word) {
        let start = from + pos;
        let end = start + wb.len();
        let left_ok = !latin || start == 0 || !is_alnum(bytes[start - 1]);
        let right_ok = !latin || end == bytes.len() || !is_alnum(bytes[end]);
        if left_ok && right_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'
}

/// 提取句内全部提及:`#123`(当前仓,空 target_repo 由调用方填)、
/// `owner/repo#123`(跨仓)。markdown 链接文本形态 `[#123](…)` 由 `#123` 本身覆盖。
fn mentions(sentence: &str) -> Vec<(String, i64)> {
    let bytes = sentence.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'#' {
            i += 1;
            continue;
        }
        let ds = i + 1;
        let de = ds
            + bytes[ds..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
        if de == ds {
            i += 1;
            continue;
        }
        // `#123abc`:编号后紧跟词字符 → 非干净提及
        if de < bytes.len() && is_word(bytes[de]) {
            i = de;
            continue;
        }
        // 跨仓形态:紧邻 '#' 是 '/',且能拆出 owner/repo 两段
        let cross = split_owner_repo(sentence, i);
        if cross.is_none() && i > 0 && is_word(bytes[i - 1]) {
            // 无跨仓前缀且 '#' 前是词字符(abc#12、url 锚点 x#anchor)→ 排除
            i = de;
            continue;
        }
        let n: i64 = sentence[ds..de].parse().unwrap_or(0);
        out.push((cross.unwrap_or_default(), n));
        i = de;
    }
    out
}

/// `hash_idx` 指向 '#';若其前是 `owner/repo#` 形态则返回 "owner/repo"。
fn split_owner_repo(sentence: &str, hash_idx: usize) -> Option<String> {
    let bytes = sentence.as_bytes();
    if hash_idx < 3 {
        return None;
    }
    // repo 段:紧邻 '#' 向左收词字符
    let mut re = hash_idx - 1;
    if !is_word(bytes[re]) {
        return None;
    }
    while re > 0 && is_word(bytes[re - 1]) {
        re -= 1;
    }
    if re < 2 {
        return None; // 无 '/' + owner 段
    }
    let slash = re - 1;
    if bytes[slash] != b'/' {
        return None;
    }
    let mut os = slash;
    while os > 0 && is_word(bytes[os - 1]) {
        os -= 1;
    }
    if os == slash {
        return None; // 空 owner 段
    }
    Some(format!(
        "{}/{}",
        &sentence[os..slash],
        &sentence[re..hash_idx]
    ))
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
