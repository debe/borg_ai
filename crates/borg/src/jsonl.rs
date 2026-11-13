//! JSONL logs: one record per line, read back skipping what does not parse.
//!
//! Also what a reader that wants a field or two of a line needs without building a
//! `serde_json::Value`: `valid` checks a line as `Value` would read it, `pick` reads the last value of
//! some keys as `Value::get` would see them. Both borrow from the line and allocate only to unescape a
//! string they return. `by_id` and `lead` are the first pass of a fold that keeps one line per id.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::marker::PhantomData;
use std::path::Path;

use serde::Deserialize;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

/// Every parsable line of a JSONL file, in file order (an incomplete or unknown line is skipped).
pub(crate) fn read_lines<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// `by_id`'s groups: an id, and each of its lines with the key `head` read from it.
pub(crate) type Groups<'a, K> = Vec<(Cow<'a, str>, Vec<(K, &'a str)>)>;

/// The lines of `text` that `head` reads an id and a key (a version, a line number) from, grouped by that
/// id: the groups in the order of their id's first line, each group's lines in file order. The first pass
/// of a fold that keeps one line per id: `head` reads a few fields of every line (`lead`), and only the
/// lines that win are then parsed whole. A `head` must take every line the whole parse takes, with the
/// same id: a line it takes that the whole parse refuses is the caller's to skip.
pub(crate) fn by_id<'a, K>(text: &'a str, mut head: impl FnMut(&'a str) -> Option<(Cow<'a, str>, K)>) -> Groups<'a, K> {
    let mut at: HashMap<Cow<'a, str>, usize> = HashMap::new();
    let mut out: Groups<'a, K> = Vec::new();
    for line in text.lines() {
        let Some((id, key)) = head(line) else { continue };
        match at.get(&id) {
            Some(&i) => out[i].1.push((key, line)),
            None => {
                at.insert(id.clone(), out.len());
                out.push((id, vec![(key, line)]));
            }
        }
    }
    out
}

/// Test support: every line of `files` in the store `BORG_BENCH_STORE` names (a local copy of a real
/// store, read only here and never copied into the repo), or none, with a note, when it is unset. The
/// oracle tests run old and new parsers over them.
#[cfg(test)]
pub(crate) fn store_lines(files: &[&str]) -> Vec<String> {
    let Some(dir) = std::env::var_os("BORG_BENCH_STORE") else {
        eprintln!("BORG_BENCH_STORE unset: only the edge lines are checked");
        return Vec::new();
    };
    let dir = std::path::PathBuf::from(dir);
    files.iter().flat_map(|f| std::fs::read_to_string(dir.join(f)).unwrap_or_default().lines().map(str::to_string).collect::<Vec<_>>()).collect()
}

/// Test support: `s` with every character written as a JSON `\u` escape (characters of the basic plane
/// only), for lines whose strings must be unescaped to be read.
#[cfg(test)]
pub(crate) fn escaped(s: &str) -> String {
    s.chars().map(|c| format!("{}u{:04x}", char::from(0x5c), u32::from(c))).collect()
}

/// Whether `line` is one JSON value, exactly as `serde_json::from_str::<Value>` accepts it, without
/// building it.
pub fn valid(line: &str) -> bool {
    serde_json::from_str::<Valid>(line).is_ok()
}

/// A JSON value read with every check `Value` makes, then dropped. Not `IgnoredAny`: that skips strings
/// and numbers unchecked, so it takes a lone surrogate escape or a number out of range that `Value`
/// refuses.
pub(crate) struct Valid;

impl<'de> Deserialize<'de> for Valid {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(ValidVisitor)
    }
}

struct ValidVisitor;

impl<'de> Visitor<'de> for ValidVisitor {
    type Value = Valid;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<Valid, E> {
        Ok(Valid)
    }

    fn visit_i64<E>(self, _: i64) -> Result<Valid, E> {
        Ok(Valid)
    }

    fn visit_u64<E>(self, _: u64) -> Result<Valid, E> {
        Ok(Valid)
    }

    fn visit_f64<E>(self, _: f64) -> Result<Valid, E> {
        Ok(Valid)
    }

    fn visit_str<E>(self, _: &str) -> Result<Valid, E> {
        Ok(Valid)
    }

    fn visit_unit<E>(self) -> Result<Valid, E> {
        Ok(Valid)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Valid, A::Error> {
        while seq.next_element::<Valid>()?.is_some() {}
        Ok(Valid)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Valid, A::Error> {
        while map.next_key::<Valid>()?.is_some() {
            map.next_value::<Valid>()?;
        }
        Ok(Valid)
    }
}

/// A string read with every check `String` makes, then dropped: a projection's stand-in for a string field
/// it must accept exactly as the whole parse does but never reads. Unescapes into serde_json's scratch
/// buffer, never a new one.
#[derive(Default)]
pub(crate) struct Str;

impl<'de> Deserialize<'de> for Str {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_str(StrVisitor)
    }
}

struct StrVisitor;

impl Visitor<'_> for StrVisitor {
    type Value = Str;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a string")
    }

    fn visit_str<E>(self, _: &str) -> Result<Str, E> {
        Ok(Str)
    }
}

/// An array whose every element reads as `T`, checked as `Vec<T>` checks it and kept nowhere (`Str`'s
/// counterpart for a list field).
pub(crate) struct Each<T>(PhantomData<T>);

impl<T> Default for Each<T> {
    fn default() -> Self {
        Each(PhantomData)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Each<T> {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_seq(EachVisitor(PhantomData))
    }
}

struct EachVisitor<T>(PhantomData<T>);

impl<'de, T: Deserialize<'de>> Visitor<'de> for EachVisitor<T> {
    type Value = Each<T>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a sequence")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Each<T>, A::Error> {
        while seq.next_element::<T>()?.is_some() {}
        Ok(Each(PhantomData))
    }
}

/// An object whose every key reads as `K` and value as `V`, checked as a `BTreeMap<K, V>` or `HashMap`
/// checks it and kept nowhere (`Str`'s counterpart for a map field).
pub(crate) struct Entries<K, V>(PhantomData<(K, V)>);

impl<K, V> Default for Entries<K, V> {
    fn default() -> Self {
        Entries(PhantomData)
    }
}

impl<'de, K: Deserialize<'de>, V: Deserialize<'de>> Deserialize<'de> for Entries<K, V> {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_map(EntriesVisitor(PhantomData))
    }
}

struct EntriesVisitor<K, V>(PhantomData<(K, V)>);

impl<'de, K: Deserialize<'de>, V: Deserialize<'de>> Visitor<'de> for EntriesVisitor<K, V> {
    type Value = Entries<K, V>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a map")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entries<K, V>, A::Error> {
        while map.next_key::<K>()?.is_some() {
            map.next_value::<V>()?;
        }
        Ok(Entries(PhantomData))
    }
}

/// A string borrowed from the line unless it holds an escape. `Cow<str>` under `#[serde(borrow)]` is that
/// for a field, but inside a `Vec` or a map serde's own `Cow` always copies: there, this.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Text<'a>(pub Cow<'a, str>);

impl std::ops::Deref for Text<'_> {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for Text<'a> {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_str(TextVisitor(PhantomData))
    }
}

struct TextVisitor<'a>(PhantomData<Text<'a>>);

impl<'de: 'a, 'a> Visitor<'de> for TextVisitor<'a> {
    type Value = Text<'a>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a string")
    }

    fn visit_borrowed_str<E>(self, s: &'de str) -> Result<Text<'a>, E> {
        Ok(Text(Cow::Borrowed(s)))
    }

    fn visit_str<E>(self, s: &str) -> Result<Text<'a>, E> {
        Ok(Text(Cow::Owned(s.to_owned())))
    }

    fn visit_string<E>(self, s: String) -> Result<Text<'a>, E> {
        Ok(Text(Cow::Owned(s)))
    }
}

/// One value `pick` read, as much of it as `Value::as_str`, `as_u64` and `as_i64` tell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Scalar<'a> {
    /// A string, borrowed unless it held an escape.
    Str(Cow<'a, str>),
    /// A non-negative integer.
    U64(u64),
    /// A negative integer.
    I64(i64),
    /// Absent, or anything else: a float, a bool, null, an array or an object.
    #[default]
    Other,
}

impl Scalar<'_> {
    /// As `Value::as_str`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Scalar::Str(s) => Some(s),
            _ => None,
        }
    }

    /// As `Value::as_u64`.
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Scalar::U64(n) => Some(n),
            _ => None,
        }
    }

    /// As `Value::as_i64`.
    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            Scalar::U64(n) => i64::try_from(n).ok(),
            Scalar::I64(n) => Some(n),
            _ => None,
        }
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for Scalar<'a> {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(ScalarVisitor(PhantomData))
    }
}

struct ScalarVisitor<'a>(PhantomData<Scalar<'a>>);

impl<'de: 'a, 'a> Visitor<'de> for ScalarVisitor<'a> {
    type Value = Scalar<'a>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<Scalar<'a>, E> {
        Ok(Scalar::Other)
    }

    fn visit_i64<E>(self, n: i64) -> Result<Scalar<'a>, E> {
        // serde_json visits a non-negative integer as u64; kept as `Value` would hold it all the same.
        Ok(u64::try_from(n).map_or(Scalar::I64(n), Scalar::U64))
    }

    fn visit_u64<E>(self, n: u64) -> Result<Scalar<'a>, E> {
        Ok(Scalar::U64(n))
    }

    fn visit_f64<E>(self, _: f64) -> Result<Scalar<'a>, E> {
        Ok(Scalar::Other)
    }

    fn visit_borrowed_str<E>(self, s: &'de str) -> Result<Scalar<'a>, E> {
        Ok(Scalar::Str(Cow::Borrowed(s)))
    }

    fn visit_str<E>(self, s: &str) -> Result<Scalar<'a>, E> {
        Ok(Scalar::Str(Cow::Owned(s.to_owned())))
    }

    fn visit_string<E>(self, s: String) -> Result<Scalar<'a>, E> {
        Ok(Scalar::Str(Cow::Owned(s)))
    }

    fn visit_unit<E>(self) -> Result<Scalar<'a>, E> {
        Ok(Scalar::Other)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Scalar<'a>, A::Error> {
        ValidVisitor.visit_seq(seq).map(|_| Scalar::Other)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Scalar<'a>, A::Error> {
        ValidVisitor.visit_map(map).map(|_| Scalar::Other)
    }
}

/// The values of `keys` in `line`, as `Value::get` gives them after `from_str::<Value>(line)`: `None`
/// when `Value` would not parse the line; the last value of a key the object repeats; `Scalar::Other`
/// for a key it lacks, and for every key of a line that is no object.
pub fn pick<'a, const N: usize>(line: &'a str, keys: [&str; N]) -> Option<[Scalar<'a>; N]> {
    let mut de = serde_json::Deserializer::from_str(line);
    let out = Pick { keys: &keys }.deserialize(&mut de).ok()?;
    de.end().ok()?;
    Some(out)
}

/// `pick`'s reader of one value.
struct Pick<'k, const N: usize> {
    keys: &'k [&'k str; N],
}

impl<'de, const N: usize> DeserializeSeed<'de> for Pick<'_, N> {
    type Value = [Scalar<'de>; N];

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de, const N: usize> Visitor<'de> for Pick<'_, N> {
    type Value = [Scalar<'de>; N];

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
        Ok(std::array::from_fn(|_| Scalar::Other))
    }

    fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
        Ok(std::array::from_fn(|_| Scalar::Other))
    }

    fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
        Ok(std::array::from_fn(|_| Scalar::Other))
    }

    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
        Ok(std::array::from_fn(|_| Scalar::Other))
    }

    fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
        Ok(std::array::from_fn(|_| Scalar::Other))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(std::array::from_fn(|_| Scalar::Other))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
        ValidVisitor.visit_seq(seq).map(|_| std::array::from_fn(|_| Scalar::Other))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut out: [Scalar<'de>; N] = std::array::from_fn(|_| Scalar::Other);
        while let Some(at) = map.next_key_seed(Key { keys: self.keys })? {
            match at {
                Some(i) => out[i] = map.next_value()?,
                None => {
                    map.next_value::<Valid>()?;
                }
            }
        }
        Ok(out)
    }
}

/// The first value of each of `keys` among the leading entries of the object `line`, read as `pick` reads
/// one, and nothing past them: a fold's first pass (`by_id`) over records serde wrote with these fields
/// first, whose cost is then the few leading bytes of each line, not the line. Not a check: a line `lead`
/// takes may be no JSON past what it read. That suits a head whose lines a typed parse decides whole
/// (serde derive): it refuses a repeated field, so for each line it takes, the first value is the only one.
/// None when `line` is no object, or one of `keys` is missing or behind something that is no JSON.
pub fn lead<'a, const N: usize>(line: &'a str, keys: [&str; N]) -> Option<[Scalar<'a>; N]> {
    let mut out: [Option<Scalar<'a>>; N] = std::array::from_fn(|_| None);
    let mut de = serde_json::Deserializer::from_str(line);
    // Unless the keys are all the object holds, serde_json finds it unfinished when `Lead` returns: the
    // error is the expected end of the read (and its one allocation), so only what was read counts.
    let _ = Lead { keys: &keys, out: &mut out }.deserialize(&mut de);
    out.iter().all(Option::is_some).then(|| out.map(Option::unwrap_or_default))
}

/// `lead`'s reader: the object's entries until each of `keys` has a value in `out`.
struct Lead<'k, 'o, 'de, const N: usize> {
    keys: &'k [&'k str; N],
    out: &'o mut [Option<Scalar<'de>>; N],
}

impl<'de, const N: usize> DeserializeSeed<'de> for Lead<'_, '_, 'de, N> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de, const N: usize> Visitor<'de> for Lead<'_, '_, 'de, N> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while self.out.iter().any(Option::is_none) {
            match map.next_key_seed(Key { keys: self.keys })? {
                Some(Some(i)) if self.out[i].is_none() => self.out[i] = Some(map.next_value()?),
                Some(_) => {
                    map.next_value::<de::IgnoredAny>()?;
                }
                None => break,
            }
        }
        Ok(())
    }
}

/// A key of an object `pick` or `lead` reads: which of its keys it is, if any.
struct Key<'k, const N: usize> {
    keys: &'k [&'k str; N],
}

impl<'de, const N: usize> DeserializeSeed<'de> for Key<'_, N> {
    type Value = Option<usize>;

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_str(self)
    }
}

impl<'de, const N: usize> Visitor<'de> for Key<'_, N> {
    type Value = Option<usize>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a key")
    }

    fn visit_str<E>(self, s: &str) -> Result<Option<usize>, E> {
        Ok(self.keys.iter().position(|k| *k == s))
    }
}

/// A map keyed by owned strings that `slot` fills, looked up by `&str`.
pub(crate) trait Slots<V> {
    fn has(&self, key: &str) -> bool;
    fn found(&mut self, key: &str) -> Option<&mut V>;
    fn put(&mut self, key: String, value: V) -> &mut V;
}

impl<V> Slots<V> for HashMap<String, V> {
    fn has(&self, key: &str) -> bool {
        self.contains_key(key)
    }
    fn found(&mut self, key: &str) -> Option<&mut V> {
        self.get_mut(key)
    }
    fn put(&mut self, key: String, value: V) -> &mut V {
        self.entry(key).or_insert(value)
    }
}

impl<V> Slots<V> for BTreeMap<String, V> {
    fn has(&self, key: &str) -> bool {
        self.contains_key(key)
    }
    fn found(&mut self, key: &str) -> Option<&mut V> {
        self.get_mut(key)
    }
    fn put(&mut self, key: String, value: V) -> &mut V {
        self.entry(key).or_insert(value)
    }
}

/// `map`'s value at `key`, `new()` inserted first if absent: `entry(key).or_insert_with(new)` without
/// copying `key` when it is there, which a fold over borrowed lines finds for most of its keys.
pub(crate) fn slot<'m, V>(map: &'m mut impl Slots<V>, key: &str, new: impl FnOnce() -> V) -> &'m mut V {
    if map.has(key) { map.found(key).expect("checked above") } else { map.put(key.to_owned(), new()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Lines at every edge `Value` decides: what `valid` and `pick` must decide as it does.
    fn edges() -> Vec<String> {
        [
            "",
            " ",
            "\t",
            "{}",
            "[]",
            "null",
            "1",
            "\"s\"",
            r#"{"id":"L1","version":2}"#,
            r#"{"id":"L1","id":"L2","version":2}"#,
            r#"{"id":"L1","version":2,"id":3}"#,
            r#"{"id":1,"version":"2"}"#,
            r#"{"id":"L1","version":5000000000}"#,
            r#"{"id":"L1","version":18446744073709551615}"#,
            r#"{"id":"L1","version":18446744073709551616}"#,
            r#"{"id":"L1","version":-1}"#,
            r#"{"id":"L1","version":2.0}"#,
            r#"{"id":"L1","version":1e400}"#,
            r#"{"id":"L1","version":2}"#,
            r#"{"id":"L1","version":2}"#,
            r#"{"id":"\ud800","version":2}"#,
            r#"{"x":"\ud800"}"#,
            r#"{"x":"\udc00\ud800"}"#,
            r#"{"x":"😀"}"#,
            "{\"x\":\"a\u{1}b\"}",
            r#"{"x":"\q"}"#,
            r#"{"id":"L1","version":2"#,
            r#"{"id":"L1","version":2}}"#,
            r#"{"id":"L1","version":2} "#,
            r#"{"id":"L1","version":2} x"#,
            r#"{"id":"L1",}"#,
            r#"{"nested":{"id":"no"},"id":"yes"}"#,
            r#"[{"id":"L1","version":1}]"#,
            r#"{"id":null,"version":true}"#,
            r#"{"ms":-9223372036854775808,"n":9223372036854775807,"m":9223372036854775808}"#,
            "01",
            "-0",
            "1.5e3",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain([format!("{}1{}", "[".repeat(127), "]".repeat(127)), format!("{}1{}", "[".repeat(128), "]".repeat(128))])
        .collect()
    }

    /// The store's logs the mesh and the readers parse.
    const LOGS: [&str; 9] = [
        "lessons.jsonl",
        "stats.jsonl",
        "outcomes.jsonl",
        "outcomes-team.jsonl",
        "messages.jsonl",
        "reasoning.jsonl",
        "episodes.jsonl",
        "cues.jsonl",
        "mesh/origins.jsonl",
    ];

    fn real_lines() -> Vec<String> {
        store_lines(&LOGS)
    }

    /// `valid` and `pick` decide every line as a full `Value` parse does: the same lines accepted, the
    /// same strings and numbers read.
    #[test]
    fn valid_and_pick_read_as_value_does() {
        let keys = ["id", "version", "ms", "n", "m", "x"];
        for line in edges().iter().chain(&real_lines()) {
            let v = serde_json::from_str::<Value>(line).ok();
            assert_eq!(valid(line), v.is_some(), "{line:?}");
            let got = pick(line, keys);
            assert_eq!(got.is_some(), v.is_some(), "{line:?}");
            if let (Some(v), Some(got)) = (v, got) {
                for (k, s) in keys.iter().zip(&got) {
                    let want = v.get(k);
                    assert_eq!(s.as_str(), want.and_then(Value::as_str), "{line:?} {k}");
                    assert_eq!(s.as_u64(), want.and_then(Value::as_u64), "{line:?} {k}");
                    assert_eq!(s.as_i64(), want.and_then(Value::as_i64), "{line:?} {k}");
                }
            }
        }
    }

    /// `lead` reads, from every line a typed parse of those fields takes, the values that parse reads, on
    /// the edges and every line of a real store; and it reads nothing past them.
    #[test]
    fn lead_reads_what_a_typed_parse_reads() {
        #[derive(Deserialize)]
        struct Versioned<'a> {
            #[serde(borrow)]
            id: Cow<'a, str>,
            version: u64,
        }
        #[derive(Deserialize)]
        struct Timed<'a> {
            ms: i64,
            #[serde(borrow)]
            kind: Cow<'a, str>,
        }
        let mut versioned = 0;
        for line in edges().iter().chain(&real_lines()) {
            if let Ok(h) = serde_json::from_str::<Versioned>(line) {
                let [id, v] = lead(line, ["id", "version"]).expect("a typed line leads");
                assert_eq!((id.as_str(), v.as_u64()), (Some(h.id.as_ref()), Some(h.version)), "{line:?}");
                versioned += 1;
            }
            if let Ok(h) = serde_json::from_str::<Timed>(line) {
                let [ms, kind] = lead(line, ["ms", "kind"]).expect("a typed line leads");
                assert_eq!((ms.as_i64(), kind.as_str()), (Some(h.ms), Some(h.kind.as_ref())), "{line:?}");
            }
        }
        assert!(versioned > 0, "the edges hold typed lines");
        let id = |line: &str| lead(line, ["id", "version"]).map(|[id, _]| id.as_str().map(str::to_string));
        assert_eq!(id(r#"{"id":"A","version":1,"rest":"#), Some(Some("A".into())), "nothing past the keys is read");
        assert_eq!(id(r#"{"id":"A","id":"B","version":1}"#), Some(Some("A".into())), "the first value");
        assert_eq!(id(r#"{"x":{"id":"B"},"id":"A","version":1}"#), Some(Some("A".into())), "a nested key is no key");
        assert_eq!(id(r#"{"id":"A","x":"#), None, "a key behind what is no JSON");
        assert_eq!(id(r#"{"id":"A"}"#), None, "a key missing");
        assert_eq!(id(r#"[{"id":"A","version":1}]"#), None, "no object");
    }

    /// `Text` borrows inside a `Vec`, where serde's `Cow` copies, and copies only what holds an escape;
    /// it takes the strings `String` takes.
    #[test]
    fn text_borrows_inside_a_vec() {
        let line = format!(r#"["a","{}"]"#, escaped("b"));
        let v: Vec<Text> = serde_json::from_str(&line).expect("strings");
        assert!(matches!(v[0].0, Cow::Borrowed("a")) && matches!(v[1].0, Cow::Owned(ref s) if s == "b"));
        let cow: Vec<Cow<str>> = serde_json::from_str(&line).expect("strings");
        assert!(matches!(cow[0], Cow::Owned(_)), "serde's own Cow copies");
        for line in edges().iter().chain(&real_lines()) {
            assert_eq!(serde_json::from_str::<Text>(line).ok().map(|t| t.to_string()), serde_json::from_str::<String>(line).ok(), "{line:?}");
        }
    }

    /// Why not `IgnoredAny`: it takes lines `Value` refuses.
    #[test]
    fn ignored_any_is_laxer_than_value() {
        for line in [r#"{"x":"\ud800"}"#, "1e400"] {
            assert!(serde_json::from_str::<de::IgnoredAny>(line).is_ok(), "{line}");
            assert!(serde_json::from_str::<Value>(line).is_err() && !valid(line), "{line}");
        }
    }

    /// A string without an escape is borrowed from the line: nothing is allocated to read it.
    #[test]
    fn pick_borrows_what_holds_no_escape() {
        let [id, q] = pick(r#"{"id":"L1","q":"a\nb"}"#, ["id", "q"]).expect("valid");
        assert!(matches!(id, Scalar::Str(Cow::Borrowed("L1"))));
        assert!(matches!(q, Scalar::Str(Cow::Owned(ref s)) if s == "a\nb"));
    }
}
