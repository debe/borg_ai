//! Unigram (SentencePiece) tokenization for a Model2Vec model, built for a short-lived process.
//!
//! `tokenizers::Tokenizer::from_file` on potion-multilingual-128M (500k pieces) builds a trie of one hash
//! map per node: about 0.8 s to load and 0.3 s to free, in every hook process. This keeps the pieces in
//! one string with a hash index (tens of ms) and runs the same Viterbi search as `tokenizers`
//! (`Unigram::encode_optimized`: best total score, the shortest piece first on ties, unknown characters
//! scored `min_score - 10` and fused into one unknown token). Normalisation and pre-tokenisation are
//! the model's own (`tokenizers` normalizer and pre-tokenizer from the same file). The offline check
//! compares the token ids with `tokenizers` on every eval text (internal evaluation "vectors", not published).

use std::borrow::Cow;
use std::collections::HashMap;

use serde::Deserialize;
use tokenizers::{Normalizer, PreTokenizer};
use tokenizers::{NormalizerWrapper, OffsetReferential, OffsetType, PreTokenizedString, PreTokenizerWrapper};

/// `tokenizers`' penalty for an unknown character (`K_UNK_PENALTY`). Protocol: must equal the
/// library's, or this tokenizer splits text differently from the one the model was trained with.
const UNK_PENALTY: f64 = 10.0;

pub struct Unigram {
    normalizer: Option<NormalizerWrapper>,
    pre_tokenizer: Option<PreTokenizerWrapper>,
    /// Every piece, concatenated; `spans[id]` is its byte range.
    arena: String,
    spans: Vec<(usize, usize)>,
    scores: Vec<f64>,
    /// FNV-1a of a piece's bytes -> its id; a second piece with the same hash goes to `collided`.
    by_hash: HashMap<u64, u32>,
    collided: HashMap<String, u32>,
    /// The longest piece (bytes) starting with a pair of characters; a single character's key has '\0'.
    longest: HashMap<(char, char), usize>,
    unk: u32,
    min_score: f64,
}

#[derive(Deserialize)]
struct File<'a> {
    normalizer: Option<NormalizerWrapper>,
    pre_tokenizer: Option<PreTokenizerWrapper>,
    #[serde(borrow)]
    model: Model<'a>,
}

#[derive(Deserialize)]
struct Model<'a> {
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
    unk_id: Option<usize>,
    #[serde(borrow)]
    vocab: Vec<(Cow<'a, str>, f64)>,
    #[serde(default)]
    byte_fallback: bool,
}

/// The FNV-1a 64-bit offset basis. Protocol: the hash's published constant.
const FNV: u64 = 0xcbf29ce484222325;

fn fnv(h: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

impl Unigram {
    /// From a `tokenizer.json`; None when its model is not a Unigram this handles (another model type,
    /// byte fallback, no unknown token): the caller then uses `tokenizers` itself.
    pub fn parse(json: &str) -> Option<Unigram> {
        let f: File = serde_json::from_str(json).ok()?;
        let m = f.model;
        if m.kind != "Unigram" || m.byte_fallback || m.vocab.is_empty() {
            return None;
        }
        let unk = u32::try_from(m.unk_id?).ok().filter(|&u| (u as usize) < m.vocab.len())?;
        let mut u = Unigram {
            normalizer: f.normalizer,
            pre_tokenizer: f.pre_tokenizer,
            arena: String::with_capacity(m.vocab.iter().map(|(p, _)| p.len()).sum()),
            spans: Vec::with_capacity(m.vocab.len()),
            scores: Vec::with_capacity(m.vocab.len()),
            by_hash: HashMap::with_capacity(m.vocab.len()),
            collided: HashMap::new(),
            longest: HashMap::new(),
            unk,
            min_score: f64::INFINITY,
        };
        for (id, (piece, score)) in m.vocab.iter().enumerate() {
            let start = u.arena.len();
            u.arena.push_str(piece);
            u.spans.push((start, u.arena.len()));
            u.scores.push(*score);
            u.min_score = u.min_score.min(*score);
            // As `tokenizers`: a repeated piece maps to its last id.
            let h = fnv(FNV, piece.as_bytes());
            match u.by_hash.get(&h).copied() {
                Some(other) if u.piece(other) != piece.as_ref() => {
                    u.collided.insert(piece.to_string(), id as u32);
                }
                _ => {
                    u.by_hash.insert(h, id as u32);
                }
            }
            let mut cs = piece.chars();
            if let Some(a) = cs.next() {
                let e = u.longest.entry((a, cs.next().unwrap_or('\0'))).or_default();
                *e = (*e).max(piece.len());
            }
        }
        Some(u)
    }

    fn piece(&self, id: u32) -> &str {
        let (a, b) = self.spans[id as usize];
        &self.arena[a..b]
    }

    fn id(&self, h: u64, s: &str) -> Option<u32> {
        match self.by_hash.get(&h) {
            Some(&id) if self.piece(id) == s => Some(id),
            _ => self.collided.get(s).copied(),
        }
    }

    /// Token ids of `text` as `tokenizers` would give them (`encode_fast`, no special tokens).
    pub fn ids(&self, text: &str) -> Option<Vec<u32>> {
        let mut pre = PreTokenizedString::from(text);
        if let Some(n) = &self.normalizer {
            pre.normalize(|s| n.normalize(s)).ok()?;
        }
        if let Some(p) = &self.pre_tokenizer {
            p.pre_tokenize(&mut pre).ok()?;
        }
        let mut out = Vec::new();
        for (s, _, _) in pre.get_splits(OffsetReferential::Original, OffsetType::Byte) {
            self.viterbi(s, &mut out);
        }
        Some(out)
    }

    fn viterbi(&self, s: &str, out: &mut Vec<u32>) {
        if s.is_empty() {
            return;
        }
        // best[end] = (score of the best path ending at byte `end`, its last token's start, its id)
        let mut best: Vec<(f64, Option<usize>, u32)> = vec![(0.0, None, 0); s.len() + 1];
        let unk_score = self.min_score - UNK_PENALTY;
        let mut start = 0;
        while start < s.len() {
            let base = best[start].0;
            let first = s[start..].chars().next().map_or(1, char::len_utf8);
            let second = s[start + first..].chars().next().unwrap_or('\0');
            let c0 = s[start..].chars().next().unwrap_or('\0');
            let longest = self.longest.get(&(c0, '\0')).copied().unwrap_or(0).max(self.longest.get(&(c0, second)).copied().unwrap_or(0));
            let mut single = false;
            let mut h = FNV;
            let mut end = start;
            // every prefix that ends on a character boundary, shortest first (the trie's order)
            for (i, c) in s[start..].char_indices() {
                let next = start + i + c.len_utf8();
                if next - start > longest {
                    break;
                }
                h = fnv(h, &s.as_bytes()[end..next]);
                end = next;
                if let Some(id) = self.id(h, &s[start..end]) {
                    let score = base + self.scores[id as usize];
                    let t = &mut best[end];
                    if t.1.is_none() || score > t.0 {
                        *t = (score, Some(start), id);
                    }
                    single |= end - start == first;
                }
            }
            if !single {
                let score = base + unk_score;
                let t = &mut best[start + first];
                if t.1.is_none() || score > t.0 {
                    *t = (score, Some(start), self.unk);
                }
            }
            start += first;
        }
        let mut ids = Vec::new();
        let mut end = s.len();
        while end > 0 {
            let (_, Some(from), id) = best[end] else { break };
            // consecutive unknown characters are one unknown token
            if !(id == self.unk && ids.last() == Some(&self.unk)) {
                ids.push(id);
            }
            end = from;
        }
        out.extend(ids.into_iter().rev());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> String {
        serde_json::json!({
            "normalizer": null,
            "pre_tokenizer": {"type": "Metaspace", "replacement": "\u{2581}", "prepend_scheme": "always", "split": false},
            "model": {"type": "Unigram", "unk_id": 0, "byte_fallback": false, "vocab": [
                ["<unk>", 0.0], ["\u{2581}", -2.0], ["a", -3.0], ["b", -3.0], ["ab", -2.5], ["\u{2581}ab", -1.0], ["c", -3.0], ["c", -3.5]
            ]}
        })
        .to_string()
    }

    #[test]
    fn best_path_and_fused_unknowns_match_tokenizers() {
        let json = tiny();
        let ours = Unigram::parse(&json).unwrap();
        let theirs: tokenizers::Tokenizer = json.parse().unwrap();
        for text in ["ab", "abab", "ab xyz ab", "c", "zzab", "a b ab"] {
            let want = theirs.encode_fast(text, false).unwrap().get_ids().to_vec();
            assert_eq!(ours.ids(text).unwrap(), want, "{text}");
        }
    }

    #[test]
    fn other_models_are_left_to_tokenizers() {
        let json = tiny().replace("\"Unigram\"", "\"BPE\"");
        assert!(Unigram::parse(&json).is_none());
    }
}
