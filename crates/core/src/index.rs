//! 本地检索：私有文档用 BM25，不装模型（ADR-09）。
//!
//! 为什么不用向量：私有文档通常只有几十份，BM25 + 云端 rerank 已经够用；
//! 而端侧 embedding 会让安装包涨 25~30MB。**先量再买**。
//!
//! 中文分词用 bigram（无词典、无依赖），配合 BM25 对短文档够用。

use std::collections::HashMap;

/// 一条检索结果。
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub id: String,
    pub score: f32,
}

/// 检索接口：以后要换向量实现，只需实现这个 trait（ADR-09 的"可插拔"）。
pub trait LocalIndex {
    fn add(&mut self, id: &str, text: &str);
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn search(&self, query: &str, k: usize) -> Vec<SearchHit>;
}

/// 分词：CJK 取 bigram（单字成词时保留单字），ASCII 取小写单词。
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut cjk: Vec<char> = Vec::new();

    fn flush_word(word: &mut String, out: &mut Vec<String>) {
        if !word.is_empty() {
            out.push(std::mem::take(word));
        }
    }
    fn flush_cjk(cjk: &mut Vec<char>, out: &mut Vec<String>) {
        match cjk.len() {
            0 => {}
            1 => out.push(cjk[0].to_string()),
            _ => {
                for w in cjk.windows(2) {
                    out.push(w.iter().collect());
                }
            }
        }
        cjk.clear();
    }

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            flush_cjk(&mut cjk, &mut out);
            word.push(ch.to_ascii_lowercase());
        } else if is_cjk(ch) {
            flush_word(&mut word, &mut out);
            cjk.push(ch);
        } else {
            flush_word(&mut word, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_word(&mut word, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF)
}

#[derive(Debug, Clone)]
struct Doc {
    id: String,
    tf: HashMap<String, u32>,
    len: u32,
}

/// BM25-lite 实现。
#[derive(Debug, Clone)]
pub struct Bm25Index {
    docs: Vec<Doc>,
    df: HashMap<String, u32>,
    avg_len: f32,
    k1: f32,
    b: f32,
}

impl Default for Bm25Index {
    fn default() -> Self {
        Self::new()
    }
}

impl Bm25Index {
    pub fn new() -> Self {
        Self {
            docs: Vec::new(),
            df: HashMap::new(),
            avg_len: 0.0,
            k1: 1.2,
            b: 0.75,
        }
    }

    fn recompute_avg(&mut self) {
        if self.docs.is_empty() {
            self.avg_len = 0.0;
        } else {
            let total: u32 = self.docs.iter().map(|d| d.len).sum();
            self.avg_len = total as f32 / self.docs.len() as f32;
        }
    }

    fn idf(&self, term: &str, n: u32) -> f32 {
        let df = *self.df.get(term).unwrap_or(&0) as f32;
        let n = n as f32;
        (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
    }
}

impl LocalIndex for Bm25Index {
    fn add(&mut self, id: &str, text: &str) {
        if let Some(existing) = self.docs.iter().position(|d| d.id == id) {
            // 覆盖：先撤销旧的 df 统计
            for term in self.docs[existing].tf.keys() {
                if let Some(v) = self.df.get_mut(term) {
                    *v = v.saturating_sub(1);
                }
            }
            self.docs.remove(existing);
        }

        let tokens = tokenize(text);
        let mut tf: HashMap<String, u32> = HashMap::new();
        for t in &tokens {
            *tf.entry(t.clone()).or_insert(0) += 1;
        }
        for term in tf.keys() {
            *self.df.entry(term.clone()).or_insert(0) += 1;
        }
        self.docs.push(Doc {
            id: id.to_owned(),
            tf,
            len: tokens.len() as u32,
        });
        self.recompute_avg();
    }

    fn len(&self) -> usize {
        self.docs.len()
    }

    fn search(&self, query: &str, k: usize) -> Vec<SearchHit> {
        if self.docs.is_empty() || k == 0 {
            return vec![];
        }
        let terms = tokenize(query);
        if terms.is_empty() {
            return vec![];
        }
        let n = self.docs.len() as u32;
        let mut hits: Vec<SearchHit> = self
            .docs
            .iter()
            .filter_map(|doc| {
                let mut score = 0.0_f32;
                for term in &terms {
                    let tf = *doc.tf.get(term).unwrap_or(&0) as f32;
                    if tf == 0.0 {
                        continue;
                    }
                    let denom = tf
                        + self.k1
                            * (1.0 - self.b + self.b * doc.len as f32 / self.avg_len.max(1.0));
                    score += self.idf(term, n) * (tf * (self.k1 + 1.0)) / denom;
                }
                (score > 0.0).then(|| SearchHit {
                    id: doc.id.clone(),
                    score,
                })
            })
            .collect();

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(k);
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Bm25Index {
        let mut idx = Bm25Index::new();
        idx.add("d1", "下午第3节有高等数学，东校区1教305，张老师");
        idx.add("d2", "图书馆开放时间：周一至周日 8:00-22:00");
        idx.add("d3", "培养方案要求：高等数学为必修课，6 学分");
        idx
    }

    #[test]
    fn cjk_bigram_tokenizer() {
        assert_eq!(tokenize("高等数学"), vec!["高等", "等数", "数学"]);
        assert_eq!(tokenize("高"), vec!["高"]);
        // 中英混排
        assert_eq!(tokenize("GPA 计算"), vec!["gpa", "计算"]);
    }

    #[test]
    fn search_ranks_relevant_docs() {
        let idx = seeded();
        let hits = idx.search("高等数学", 3);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.id != "d2"));
        // d1 与 d3 都命中；同分时只要求都在结果里
        let ids: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        assert!(ids.contains(&"d1") && ids.contains(&"d3"));
        // 不相关查询不应命中
        assert!(idx.search("量子力学", 3).is_empty());
    }

    #[test]
    fn re_adding_same_id_replaces() {
        let mut idx = Bm25Index::new();
        idx.add("a", "苹果");
        idx.add("a", "香蕉");
        assert_eq!(idx.len(), 1);
        assert!(idx.search("苹果", 5).is_empty());
        assert_eq!(idx.search("香蕉", 5).len(), 1);
    }

    #[test]
    fn empty_cases() {
        let idx = Bm25Index::new();
        assert!(idx.is_empty());
        assert!(idx.search("随便", 3).is_empty());
        let idx = seeded();
        assert!(idx.search("", 3).is_empty());
        assert!(idx.search("数学", 0).is_empty());
    }
}
