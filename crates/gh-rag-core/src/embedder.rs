//! Embedder trait:向量生产者的唯一抽象。
//!
//! 实现(fastembed 封装,内部即 ort + tokenizers):
//! - `FastEmbedder`:本地 bge-m3(int8),dense 输出 + L2 归一化。
//!   与 Python sentence-transformers(fp32)的对齐度由黄金测试度量。

use crate::Result;

/// 环境指纹:钉进 index.sqlite 的 manifest,防止向量空间混用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingFingerprint(pub String);

/// 嵌入文本组装:标题重复加权 + 正文截断。
/// 必须与 Python 版 `BgeM3Embedder.build_text` 逐字符一致(有单元测试)。
pub fn build_text(title: &str, body: &str, title_repeats: usize, body_max_chars: usize) -> String {
    let t = title.trim();
    let head = format!("{}\n", t).repeat(title_repeats);
    let body_prefix: String = body.chars().take(body_max_chars).collect();
    format!("{}{}", head, body_prefix)
}

pub trait Embedder {
    /// 批量嵌入(建库路径)。输出 float32 小端字节,1024 维,L2 归一化。
    fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<u8>>>;

    /// 单条查询嵌入(检索路径)。返回 1024 维 f32,L2 归一化。
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;

    /// 环境指纹(模型+实现+序列长度),写入 manifest。
    fn fingerprint(&self) -> EmbeddingFingerprint;
}

/// 嵌入文本中的图片降噪:`![alt](url)` → `[图片]`,裸图片 URL → `[图片]`。
/// URL 哈希字符对嵌入模型是纯噪声;原文保留在库/MCP 返回,仅嵌入侧剥离。
pub fn strip_image_links(body: &str) -> String {
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len());
    let mut i = 0usize;
    while i < chars.len() {
        // markdown 图片 ![...](...)
        if chars[i] == '!' && i + 1 < chars.len() && chars[i + 1] == '[' {
            if let Some(end) = md_image_end(&chars, i) {
                out.push_str("[图片]");
                i = end + 1;
                continue;
            }
        }
        // 裸图片 URL(http 开头且行内出现图片扩展名)
        if chars[i..].starts_with(&['h', 't', 't', 'p']) {
            let mut j = i;
            while j < chars.len() && !matches!(chars[j], ' ' | '\n' | '\r' | ')' | '"' | '\t') {
                j += 1;
            }
            let url: String = chars[i..j].iter().collect();
            let lower = url.to_lowercase();
            if [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"]
                .iter()
                .any(|ext| lower.contains(ext))
            {
                out.push_str("[图片]");
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn md_image_end(chars: &[char], start: usize) -> Option<usize> {
    let mut j = start + 2;
    while j < chars.len() && chars[j] != ']' {
        if chars[j] == '\n' {
            return None;
        }
        j += 1;
    }
    if j + 1 >= chars.len() || chars[j + 1] != '(' {
        return None;
    }
    let mut k = j + 2;
    while k < chars.len() && chars[k] != ')' {
        if chars[k] == '\n' {
            return None;
        }
        k += 1;
    }
    if k >= chars.len() {
        None
    } else {
        Some(k)
    }
}

pub fn build_text_with_comments(
    title: &str,
    body: &str,
    comments: &[(&str, &str)], // (author, body) 时间序
    title_repeats: usize,
    body_max_chars: usize,
    comment_quota: usize,
    per_comment_max: usize,
) -> String {
    let mut text = build_text(title, "", title_repeats, 0); // 标题段(自带换行)
    let clean_body = strip_image_links(body);
    text.push_str(&clean_body.chars().take(body_max_chars).collect::<String>());
    if comments.is_empty() {
        return text;
    }
    text.push_str("\n\n[讨论]\n");
    let mut used = 0usize;
    for (author, cbody) in comments {
        if used >= comment_quota {
            break;
        }
        let line: String = format!(
            "[- {}] {}\n",
            author,
            cbody.chars().take(per_comment_max).collect::<String>()
        );
        let line_chars = line.chars().count();
        if used + line_chars > comment_quota {
            let room = comment_quota.saturating_sub(used);
            text.push_str(&line.chars().take(room).collect::<String>());
            break;
        }
        text.push_str(&line);
        used += line_chars;
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_aggregate_within_quota() {
        let t = build_text_with_comments(
            "标题",
            "正文",
            &[("a", "评论一"), ("b", "评论二")],
            2,
            100,
            1000,
            500,
        );
        assert!(t.contains("标题\n标题\n"));
        assert!(t.contains("[讨论]"));
        assert!(t.contains("[- a] 评论一"));
        assert!(t.contains("[- b] 评论二"));
    }

    #[test]
    fn comment_quota_truncates_tail() {
        let t = build_text_with_comments(
            "t",
            "b",
            &[("a", "很长".repeat(300).as_str())],
            1,
            10,
            100,
            500,
        );
        assert!(t.chars().count() < 200, "配额 100 应截断超长评论");
    }

    #[test]
    fn image_urls_are_stripped_from_embed_text() {
        let body = "报错截图:\n![image](https://dl.dbxio.com/abc123.png)\n还有裸链 https://a.com/x.JPG 结束";
        let t = build_text_with_comments("t", body, &[], 1, 500, 100, 100);
        assert!(!t.contains("dl.dbxio.com"), "markdown 图片 URL 应剥离");
        assert!(!t.contains("a.com/x.JPG"), "裸图片 URL 应剥离");
        assert!(t.matches("[图片]").count() >= 1);
        assert!(t.contains("报错截图"));
    }

    #[test]
    fn no_comments_keeps_plain_form() {
        let t = build_text_with_comments("标题", "正文", &[], 2, 100, 100, 100);
        assert_eq!(t, "标题\n标题\n正文");
    }
}
