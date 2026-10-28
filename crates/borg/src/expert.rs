//! L2 of the expertise ladder: a version-keyed expert index of another repo (PLAN.md, "Expertise
//! and the queen").
//!
//! `borg debug expert <repo> --at <commit> [--version <v>] --q "<text>"` answers with at most 3 cited
//! claims from the expert repo *as it was at `<commit>`*: every file is read through `git ls-tree`
//! and `git show <commit>:<path>`, never the working tree, so nothing written later can leak in.
//! Git runs with the caller's GIT_DIR / GIT_WORK_TREE / index / object overrides removed and with
//! replace refs disabled, at the repo's top level. A ceiling (`BORG_EXPERT_CEILING`, set by the eval
//! to the expert commit) makes `--at` a hex hash that must be an ancestor of the ceiling.
//!
//! Sources, parsed deterministically (0 tokens):
//! - spec requirements: headings `#### CORE-073: Title` in `spec/**.md` / `specs/**.md`, with their text;
//! - release notes: `CHANGELOG.md` (or CHANGES / HISTORY / RELEASES) at the root, one claim per change;
//! - proof coverage: any `*proof-coverage.json` (`theorems[].{name, module, specIds[].{id, fragment}}`);
//! - docs: every heading of every tracked `README.md` and `docs/**.md`, with its first paragraph.
//!
//! Retrieval is BM25 over the claim text (the conscience's tokeniser). With `--version`:
//! - a claim that states "since vX" / "until vY" is kept only if the pinned version lies in that range;
//! - a requirement, or a theorem proving one, whose id the changelog first cites in a release newer
//!   than the pin (or only in an unreleased section) is dropped: it did not exist for the consumer;
//! - releases newer than the pinned one are surfaced as "newer release vY: <change>": one slot of
//!   the three is kept for the best-matching newer change, or, if none matches, for a line saying
//!   that newer releases exist.
//!
//! A version is `9.0.0`, `v9`, or labelled, `java 9.0.0` / `rust@11`. An unlabelled pin takes the
//! changelog's only line; when the changelog names several lines (flowlib: Java, TypeScript, Rust,
//! Python) the label is required. Sections headed "unreleased", or undated above dated releases,
//! are printed as "unreleased (in progress at <short>)", never as a newer release.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;

use crate::conscience::{bm25, tokens};

/// At most this many claims are printed.
pub const MAX_CLAIMS: usize = 3;
/// Characters of claim text per line; three lines with their citations stay near 200 tokens.
const CLAIM_CHARS: usize = 210;
/// Words of a claim's body that are indexed: a long requirement would otherwise match everything.
const INDEX_WORDS: usize = 160;
/// Environment variable naming the newest commit `--at` may resolve to.
pub const CEILING_ENV: &str = "BORG_EXPERT_CEILING";

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Spec,
    Release { newer: bool },
    Proof,
    Doc,
}

/// One citable statement of the expert repo.
#[derive(Debug, Clone)]
pub struct Claim {
    /// Path of the file it was read from, at the commit.
    pub source: String,
    /// Anchor inside the source: requirement id, release and change number, theorem, heading slug.
    pub id: String,
    /// The claim as printed (before truncation).
    pub text: String,
    /// What BM25 matches against.
    index: String,
    /// Requirement ids the claim is about (a spec's own id, a theorem's specIds).
    reqs: Vec<String>,
    pub kind: Kind,
    /// A change of a release that ships the pin's own artifact line above the pin.
    line_newer: bool,
}

impl Claim {
    /// `source#id`, as a rendered line cites it.
    pub fn key(&self) -> String {
        format!("{}#{}", self.source, self.id)
    }
}

/// The answer: the repo's name, the commit's short hash, and the chosen claims, best first.
#[derive(Debug)]
pub struct Answer {
    pub repo: String,
    pub short: String,
    pub claims: Vec<Claim>,
}

impl Answer {
    /// `[<repo>@<short> <source>#<id>] <claim>` lines.
    pub fn render(&self) -> String {
        self.claims.iter().map(|c| format!("[{}@{} {}#{}] {}\n", self.repo, self.short, c.source, c.id, clip(&c.text, CLAIM_CHARS))).collect()
    }
}

static HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9a-fA-F]{4,40}$").unwrap());

/// `borg debug expert <repo> --at <commit> [--version <v>] [--q <text>]` (the question on stdin without
/// `--q`).
pub fn main(repo: &str, rest: &[&str]) -> i32 {
    let flag = |f: &str| rest.iter().position(|a| *a == f).and_then(|i| rest.get(i + 1)).copied();
    let Some(at) = flag("--at") else {
        eprintln!("borg debug expert: --at <commit> is required");
        return 2;
    };
    let query = flag("--q").map_or_else(
        || {
            let mut q = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut q);
            q
        },
        str::to_string,
    );
    match ask(repo.as_ref(), at, flag("--version"), &query, std::env::var(CEILING_ENV).ok().filter(|c| !c.is_empty()).as_deref()) {
        Ok(answer) => {
            print!("{}", answer.render());
            0
        }
        Err(e) => {
            eprintln!("borg debug expert: {e}");
            1
        }
    }
}

/// Ask the expert repo at `at` (any commit-ish), for a consumer pinned at `version`. With a
/// `ceiling`, `at` must be a hex hash naming the ceiling or one of its ancestors.
pub fn ask(repo: &Path, at: &str, version: Option<&str>, query: &str, ceiling: Option<&str>) -> Result<Answer, String> {
    Ok(Indexed::open(repo, at, version, ceiling)?.answer(query))
}

/// The expert repo indexed once at a commit, for a consumer pinned at a version: ask it any number
/// of questions, or read its API vocabulary first (see [`plan_query`]).
#[derive(Debug)]
pub struct Indexed {
    pub repo: String,
    pub short: String,
    claims: Vec<Claim>,
    pinned: bool,
    /// The work tree's top level and the full commit: [`Indexed::api`] reads the code there.
    top: PathBuf,
    commit: String,
}

impl Indexed {
    /// See [`ask`].
    pub fn open(repo: &Path, at: &str, version: Option<&str>, ceiling: Option<&str>) -> Result<Indexed, String> {
        let top = toplevel(repo);
        let commit = resolve(&top, at)?;
        if let Some(ceiling) = ceiling {
            if !HEX.is_match(at) {
                return Err(format!("--at {at:?} must be a commit hash while a ceiling is set"));
            }
            let ceiling = resolve(&top, ceiling)?;
            if !is_ancestor(&top, &commit, &ceiling)? {
                return Err(format!("--at {at} is not at or before the ceiling {}", &ceiling[..ceiling.len().min(12)]));
            }
        }
        let short = git(&top, &["rev-parse", "--short", &commit])?.trim().to_string();
        let name = top.canonicalize().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_else(|| "repo".into());
        let pin = match version {
            Some(v) => Some(Pin::parse(v).ok_or_else(|| format!("cannot read version {v:?}"))?),
            None => None,
        };
        let claims = index(&top, &commit, pin.as_ref())?;
        Ok(Indexed { repo: name, short, claims, pinned: pin.is_some(), top, commit })
    }

    /// At most `MAX_CLAIMS` claims for `query`, best first (see [`select`]).
    pub fn answer(&self, query: &str) -> Answer {
        self.answer_except(query, &|_| false)
    }

    /// [`Indexed::answer`] without the claims whose [`Claim::key`] `told` names: what this drone
    /// was already told is never repeated.
    pub fn answer_except(&self, query: &str, told: &dyn Fn(&str) -> bool) -> Answer {
        Answer { repo: self.repo.clone(), short: self.short.clone(), claims: select(self.claims.clone(), self.pinned, query, told) }
    }

    /// The repo's API vocabulary: code identifiers its claims name (`PetriNet`, `deadlockFree`,
    /// `fire_transition`) that its own code at the commit declares, so a plan that uses the API
    /// without naming the repo still finds it. Prose about other code (`HashMap`, `created_at`,
    /// `CompletableFuture` in a changelog) is no API of the repo: an identifier counts only when the
    /// repo declares it and it is no well-known standard-library name. No code, no API.
    pub fn api(&self) -> Api {
        let mut named = BTreeSet::new();
        for c in &self.claims {
            for t in IDENT.find_iter(&c.text).chain(IDENT.find_iter(&c.index)) {
                if is_api(t.as_str()) && !STD_API.contains(&t.as_str()) {
                    named.insert(t.as_str().to_string());
                }
            }
        }
        declarations(&self.top, Some(&self.commit), &named)
    }
}

/// Identifiers a repo declares: types (`class PetriNet`, `struct Marking`, `interface TokenSink`)
/// and members (`fn fire_transition`, `def deadlock_free`, `public boolean deadlockFree(`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Api {
    pub types: BTreeSet<String>,
    pub members: BTreeSet<String>,
}

impl Api {
    /// Whether `sentence` uses this API: it names one of its types, or two distinct members (one
    /// member name alone, `getState` or `close_all`, is too likely the consumer's own).
    pub fn used_in(&self, sentence: &str) -> bool {
        let words: BTreeSet<&str> = IDENT.find_iter(sentence).map(|m| m.as_str()).collect();
        words.iter().any(|w| self.types.contains(*w)) || words.iter().filter(|w| self.members.contains(**w)).count() >= 2
    }

    /// Without the identifiers the consumer in `dir` declares itself: its own `PetriNet` is not the
    /// expert's. Only the identifiers `plan` uses are looked up, so this stays cheap.
    pub fn without_declared_in(mut self, dir: &Path, plan: &str) -> Api {
        let used: BTreeSet<String> =
            IDENT.find_iter(plan).map(|m| m.as_str().to_string()).filter(|w| self.types.contains(w) || self.members.contains(w)).collect();
        if used.is_empty() || dir.as_os_str().is_empty() {
            return self;
        }
        let own = declarations(&toplevel(dir), None, &used);
        let theirs = |t: &String| !own.types.contains(t) && !own.members.contains(t);
        self.types.retain(theirs);
        self.members.retain(theirs);
        self
    }
}

/// Source files whose declarations make up an API.
const CODE: [&str; 13] = ["*.java", "*.kt", "*.scala", "*.ts", "*.tsx", "*.js", "*.mjs", "*.rs", "*.py", "*.go", "*.cs", "*.swift", "*.lean"];

/// Every keyword `TYPE_DECL` and `MEMBER_DECL` start at (POSIX ERE, for git grep).
const DECL_WORDS: &str = "(class|interface|enum|record|struct|trait|object|type|protocol|structure|inductive|fn|fun|func|def|function|theorem|lemma|export|public|protected)[[:space:]]";

static TYPE_DECL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:class|interface|enum|record|struct|trait|object|type|protocol|structure|inductive)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap()
});
static MEMBER_DECL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\b(?:fn|fun|func|def|function|theorem|lemma)\s+([A-Za-z_][A-Za-z0-9_]*)",
        r"|\bexport\s+(?:const|let|var)\s+([A-Za-z_][A-Za-z0-9_]*)",
        r"|\b(?:public|protected)\b[^=;(){}]*?\b([A-Za-z_][A-Za-z0-9_]*)\s*\(",
    ))
    .unwrap()
});

/// Which of `names` the code of `top` declares: at `rev`, or in the tracked work tree when None.
/// Any git failure (not a repo, no match) declares nothing.
fn declarations(top: &Path, rev: Option<&str>, names: &BTreeSet<String>) -> Api {
    let mut api = Api::default();
    if names.is_empty() {
        return api;
    }
    // Lines that may declare something are read, then checked for a declaration of one of `names`.
    // A few names are looked up as words; many (a repo's whole vocabulary) by the declaring keyword,
    // because hundreds of word patterns make git grep take tens of seconds on a large repo.
    let mut args: Vec<&str> = vec!["grep", "-h", "-I"];
    if names.len() <= 16 {
        args.extend(["-w", "-F"]);
        for n in names {
            args.extend(["-e", n.as_str()]);
        }
    } else {
        args.extend(["-E", "-e", DECL_WORDS]);
    }
    args.extend(rev);
    args.push("--");
    args.extend(CODE);
    let Ok(out) = git(top, &args) else { return api };
    for line in out.lines() {
        for c in TYPE_DECL.captures_iter(line).filter(|c| names.contains(&c[1])) {
            api.types.insert(c[1].to_string());
        }
        for c in MEMBER_DECL.captures_iter(line) {
            if let Some(m) = c.get(1).or(c.get(2)).or(c.get(3)).filter(|m| names.contains(m.as_str())) {
                api.members.insert(m.as_str().to_string());
            }
        }
    }
    let types = api.types.clone();
    api.members.retain(|m| !types.contains(m));
    api
}

static IDENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z_][A-Za-z0-9_]*").unwrap());
/// Words with an inner capital that are prose, not API.
const NOT_API: [&str; 8] = ["JavaScript", "TypeScript", "GitHub", "GitLab", "macOS", "iOS", "README", "CHANGELOG"];

/// Standard-library and everyday names (JDK, Rust std, JS/TS, Python, schema columns) that a repo
/// may mention, wrap or even redeclare, but that never say a plan is about that repo.
const STD_API: &[&str] = &[
    // Java
    "HashMap",
    "LinkedHashMap",
    "TreeMap",
    "ConcurrentHashMap",
    "ConcurrentMap",
    "EnumMap",
    "IdentityHashMap",
    "WeakHashMap",
    "HashSet",
    "LinkedHashSet",
    "TreeSet",
    "EnumSet",
    "ArrayList",
    "LinkedList",
    "ArrayDeque",
    "PriorityQueue",
    "BlockingQueue",
    "LinkedBlockingQueue",
    "ArrayBlockingQueue",
    "ConcurrentLinkedQueue",
    "CopyOnWriteArrayList",
    "CompletableFuture",
    "CompletionStage",
    "ExecutorService",
    "ScheduledExecutorService",
    "ThreadPoolExecutor",
    "ForkJoinPool",
    "CountDownLatch",
    "CyclicBarrier",
    "AtomicInteger",
    "AtomicLong",
    "AtomicBoolean",
    "AtomicReference",
    "ReentrantLock",
    "ReadWriteLock",
    "ReentrantReadWriteLock",
    "ThreadLocal",
    "StringBuilder",
    "StringBuffer",
    "BigDecimal",
    "BigInteger",
    "LocalDate",
    "LocalDateTime",
    "LocalTime",
    "ZonedDateTime",
    "OffsetDateTime",
    "ZoneId",
    "ChronoUnit",
    "TimeUnit",
    "InputStream",
    "OutputStream",
    "ByteBuffer",
    "CharSequence",
    "IntStream",
    "LongStream",
    "AutoCloseable",
    "FunctionalInterface",
    "IllegalArgumentException",
    "IllegalStateException",
    "InterruptedException",
    "RuntimeException",
    "NullPointerException",
    "IOException",
    "UncheckedIOException",
    "TimeoutException",
    "ExecutionException",
    "CancellationException",
    "UnsupportedOperationException",
    "IndexOutOfBoundsException",
    "ClassCastException",
    "ConcurrentModificationException",
    "NoSuchElementException",
    "NumberFormatException",
    "StackOverflowError",
    "OutOfMemoryError",
    "SuppressWarnings",
    "toString",
    "hashCode",
    "compareTo",
    "getClass",
    "forEach",
    "valueOf",
    "orElse",
    "orElseThrow",
    "orElseGet",
    "isEmpty",
    "isPresent",
    "ifPresent",
    "ofNullable",
    "getMessage",
    "getName",
    "getValue",
    "getKey",
    "setName",
    "setValue",
    "thenApply",
    "thenCompose",
    "thenAccept",
    "supplyAsync",
    "runAsync",
    "parseInt",
    "toList",
    // Rust
    "BTreeMap",
    "BTreeSet",
    "VecDeque",
    "BinaryHeap",
    "IntoIterator",
    "FromIterator",
    "DoubleEndedIterator",
    "ExactSizeIterator",
    "AsRef",
    "AsMut",
    "ToString",
    "ToOwned",
    "PartialEq",
    "PartialOrd",
    "DefaultHasher",
    "JoinHandle",
    "RwLock",
    "OnceCell",
    "OnceLock",
    "LazyLock",
    "LazyCell",
    "RefCell",
    "PhantomData",
    "PathBuf",
    "OsString",
    "SystemTime",
    "TryFrom",
    "TryInto",
    "FromStr",
    "into_iter",
    "iter_mut",
    "unwrap_or",
    "unwrap_or_else",
    "unwrap_or_default",
    "as_str",
    "as_ref",
    "as_mut",
    "to_string",
    "to_owned",
    "to_vec",
    "from_str",
    "ok_or",
    "ok_or_else",
    "and_then",
    "map_err",
    "is_empty",
    "is_some",
    "is_none",
    "with_capacity",
    "read_to_string",
    "spawn_blocking",
    // JS / TS
    "setTimeout",
    "setInterval",
    "clearTimeout",
    "clearInterval",
    "addEventListener",
    "removeEventListener",
    "toFixed",
    "getElementById",
    "querySelector",
    "querySelectorAll",
    "AbortController",
    "AbortSignal",
    "ReadableStream",
    "WritableStream",
    "WeakMap",
    "WeakSet",
    "WeakRef",
    "ArrayBuffer",
    "Uint8Array",
    "EventEmitter",
    "EventTarget",
    "TypeError",
    "RangeError",
    "SyntaxError",
    "ReferenceError",
    "useState",
    "useEffect",
    "useMemo",
    "useCallback",
    "useRef",
    "toISOString",
    "localStorage",
    "sessionStorage",
    "innerHTML",
    "textContent",
    "readFileSync",
    "writeFileSync",
    // Python
    "__init__",
    "__name__",
    "__main__",
    "__repr__",
    "__str__",
    "__eq__",
    "__hash__",
    "__call__",
    "__enter__",
    "__exit__",
    "ValueError",
    "KeyError",
    "IndexError",
    "RuntimeError",
    "TypeVar",
    "NamedTuple",
    "OrderedDict",
    "defaultdict",
    // Columns and fields every schema has
    "created_at",
    "updated_at",
    "deleted_at",
    "user_id",
    "created_by",
    "updated_by",
    "createdAt",
    "updatedAt",
    "deletedAt",
    "userId",
    "timeoutMs",
    "timeout_ms",
];

/// An identifier that can only be code: camelCase / PascalCase with an inner capital, or snake_case.
fn is_api(t: &str) -> bool {
    let b = t.as_bytes();
    let inner_capital = b.windows(2).any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase());
    let snake = t.trim_matches('_').contains('_') && t.chars().any(|c| c.is_ascii_lowercase());
    t.len() >= 5 && (inner_capital || snake) && !NOT_API.contains(&t)
}

/// What to ask the expert about a plan: the plan's sentences that mention the dependency, by one of
/// `names` (case-insensitive, whole words; `org.flowlib` also matches `org.flowlib:flowlib`) or by
/// its `api` (case-sensitive; see [`Api::used_in`]). The query is those sentences plus the names,
/// never the whole plan: a long plan otherwise drowns the dependency in unrelated words (E1). None
/// when no sentence mentions it: the plan does not touch the dependency, so nothing is asked.
pub fn plan_query(plan: &str, names: &[&str], api: &Api) -> Option<(Vec<String>, String)> {
    let names: Vec<String> = names.iter().map(|n| n.trim().to_lowercase()).filter(|n| !n.is_empty()).collect();
    let mentions = |s: &str| {
        let low = s.to_lowercase();
        names.iter().any(|n| has_word(&low, n)) || api.used_in(s)
    };
    let picked: Vec<String> = sentences(plan).into_iter().filter(|s| mentions(s)).map(str::to_string).collect();
    if picked.is_empty() {
        return None;
    }
    let query = format!("{} {}", picked.join(" "), names.join(" "));
    Some((picked, query))
}

/// `needle` occurs in `hay` with no letter, digit or '_' right before or after it.
fn has_word(hay: &str, needle: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    hay.match_indices(needle).any(|(i, _)| !word(hay[..i].chars().next_back()) && !word(hay[i + needle.len()..].chars().next()))
}

/// A plan's sentences: its lines, split again after '.', '?' or '!' followed by a space once at
/// least three words precede it.
fn sentences(plan: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for line in plan.lines() {
        let b = line.as_bytes();
        let mut start = 0;
        for i in 0..b.len() {
            // "2. ", "e.g. " or "Fix it. " is no sentence of its own: it stays with what follows.
            if matches!(b[i], b'.' | b'?' | b'!') && b.get(i + 1) == Some(&b' ') && line[start..i].split_whitespace().count() >= 3 {
                out.push(line[start..=i].trim());
                start = i + 1;
            }
        }
        out.push(line[start..].trim());
    }
    out.retain(|s| !s.is_empty());
    out
}

/// The work tree's top level, so a subdirectory path reads the whole repo; a bare repo stays as is.
fn toplevel(repo: &Path) -> PathBuf {
    git(repo, &["rev-parse", "--show-toplevel"]).map(|t| PathBuf::from(t.trim())).unwrap_or_else(|_| repo.to_path_buf())
}

fn resolve(repo: &Path, rev: &str) -> Result<String, String> {
    Ok(git(repo, &["rev-parse", "--verify", "--end-of-options", &format!("{rev}^{{commit}}")])?.trim().to_string())
}

/// All claims of the repo at `commit`. The pin is resolved once against the changelog (its line
/// made explicit) and then used everywhere: release claims carry `newer` relative to it; spec, doc
/// and proof claims whose stated range excludes it, or whose requirement arrived later, are dropped.
pub fn index(repo: &Path, commit: &str, pin: Option<&Pin>) -> Result<Vec<Claim>, String> {
    let paths = git(repo, &["ls-tree", "-r", "--full-tree", "--name-only", commit])?;
    let short = git(repo, &["rev-parse", "--short", commit])?.trim().to_string();
    let show = |path: &str| git(repo, &["show", &format!("{commit}:{path}")]);
    let mut claims = Vec::new();
    let mut log: Option<(String, String)> = None;
    for path in paths.lines() {
        let lower = path.to_lowercase();
        let base = lower.rsplit('/').next().unwrap_or_default();
        let is_md = lower.ends_with(".md");
        if is_md && (lower.starts_with("spec/") || lower.starts_with("specs/")) {
            claims.extend(spec_claims(path, &show(path)?));
        } else if !lower.contains('/') && is_md && ["changelog", "changes", "history", "releases"].iter().any(|n| base.starts_with(n)) {
            if log.is_none() {
                log = Some((path.to_string(), show(path)?));
            }
        } else if base.ends_with("proof-coverage.json") {
            claims.extend(proof_claims(path, &show(path)?));
        } else if is_md && (base == "readme.md" || lower.starts_with("docs/")) {
            claims.extend(doc_claims(path, &show(path)?));
        }
    }
    let releases = log.as_ref().map(|(_, t)| parse_releases(t)).unwrap_or_default();
    let pin = pin.map(|p| resolve_pin(p, &releases)).transpose()?;
    let newer = newer_flags(&releases, pin.as_ref());
    if let Some(pin) = &pin {
        let late = late_reqs(&releases, &newer);
        claims.retain(|c| in_range(&c.index, pin) && !c.reqs.iter().any(|r| late.contains(r.as_str())));
    }
    if let Some((path, _)) = &log {
        claims.extend(release_claims(path, &releases, &newer, pin.as_ref(), &short));
    }
    Ok(claims)
}

/// A newer-release change takes the reserved slot only if it scores at least this share of the best
/// claim; a weaker one is noise, and the slot says instead that newer releases exist.
const NEWER_MIN_SHARE: f64 = 0.25;

/// BM25 top claims with score > 0, at most `MAX_CLAIMS`. With a pin:
/// - when releases of the pin's own line newer than the pin exist, the first claim always says so
///   ("N newer release(s) than the pinned version: ..."): for a consumer it is the most consequential
///   fact, whatever the query (E1 missed "Java 10.0.0 released" when asked with a plan);
/// - one of the remaining slots shows the best-matching newer change, if it scores at least
///   `NEWER_MIN_SHARE` of the best claim; with newer releases only of other lines and no matching
///   newer change, the last slot says that those exist.
///
/// Claims `told` names by key are left out, the newer-release line too: once told, never repeated.
fn select(claims: Vec<Claim>, pinned: bool, query: &str, told: &dyn Fn(&str) -> bool) -> Vec<Claim> {
    let docs: Vec<Vec<String>> = claims.iter().map(|c| tokens(&c.index)).collect();
    let mut ranked: Vec<(f64, usize)> =
        bm25(&docs, query).into_iter().enumerate().map(|(i, s)| (s, i)).filter(|&(s, i)| s > 0.0 && !told(&claims[i].key())).collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let newer = |i: usize| claims[i].kind == Kind::Release { newer: true };
    let lead = (pinned && claims.iter().any(|c| c.line_newer)).then(|| newer_summary(&claims, |c| c.line_newer)).filter(|c| !told(&c.key()));
    let room = MAX_CLAIMS - usize::from(lead.is_some());
    let mut chosen: Vec<usize> = ranked.iter().map(|r| r.1).take(room).collect();
    let mut tail = None;
    let floor = ranked.first().map_or(0.0, |r| r.0 * NEWER_MIN_SHARE);
    if pinned && !chosen.iter().any(|&i| newer(i)) {
        if let Some(&(_, i)) = ranked.iter().find(|r| newer(r.1) && r.0 >= floor) {
            chosen.truncate(room - 1);
            chosen.push(i);
        } else if lead.is_none() && claims.iter().any(newer_release_first) {
            let summary = newer_summary(&claims, |_| true);
            if !told(&summary.key()) {
                chosen.truncate(room - 1);
                tail = Some(summary);
            }
        }
    }
    lead.into_iter().chain(chosen.into_iter().map(|i| claims[i].clone())).chain(tail).collect()
}

fn newer_release_first(c: &Claim) -> bool {
    c.kind == Kind::Release { newer: true } && c.id.ends_with(":1")
}

/// "N newer release(s) than the pinned version: A (date); B (date)" over the newer releases `keep`
/// admits (at least one), cited to the newest of them.
fn newer_summary(claims: &[Claim], keep: impl Fn(&Claim) -> bool) -> Claim {
    let firsts: Vec<&Claim> = claims.iter().filter(|c| newer_release_first(c) && keep(c)).collect();
    let names: Vec<String> = firsts.iter().map(|c| c.text.split(':').next().unwrap_or_default().trim_start_matches("newer release ").to_string()).collect();
    let c = firsts[0];
    Claim {
        source: c.source.clone(),
        id: c.id.trim_end_matches(":1").to_string(),
        text: format!("{} newer release(s) than the pinned version: {}", names.len(), names.join("; ")),
        index: String::new(),
        reqs: Vec::new(),
        kind: c.kind.clone(),
        line_newer: false,
    }
}

/// Git variables that would point `-C <repo>` at another repository, index or object store.
const GIT_ENV: [&str; 7] =
    ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_NAMESPACE", "GIT_COMMON_DIR"];

fn git_cmd(repo: &Path) -> Command {
    let mut cmd = Command::new("git");
    for v in GIT_ENV {
        cmd.env_remove(v);
    }
    // refs/replace could swap an old commit's content for something written later.
    cmd.env("GIT_NO_REPLACE_OBJECTS", "1").arg("-C").arg(repo);
    cmd
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = git_cmd(repo).args(args).output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether `a` is `b` or an ancestor of it.
fn is_ancestor(repo: &Path, a: &str, b: &str) -> Result<bool, String> {
    let out = git_cmd(repo).args(["merge-base", "--is-ancestor", a, b]).output().map_err(|e| format!("git: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!("git merge-base --is-ancestor: {}", String::from_utf8_lossy(&out.stderr).trim())),
    }
}

// ---------------------------------------------------------------- versions

/// A version as numbers, compared with missing parts as 0 (`10` == `10.0.0`).
#[derive(Debug, Clone)]
pub struct Ver(Vec<u64>);

impl Ver {
    fn parse(s: &str) -> Option<Ver> {
        let v: Option<Vec<u64>> = s.trim_start_matches(['v', 'V']).split('.').map(|p| p.parse().ok()).collect();
        v.filter(|v| !v.is_empty()).map(Ver)
    }
}

impl PartialEq for Ver {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Ver {}
impl PartialOrd for Ver {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Ver {
    fn cmp(&self, o: &Self) -> Ordering {
        let n = self.0.len().max(o.0.len());
        (0..n).map(|i| self.0.get(i).unwrap_or(&0).cmp(o.0.get(i).unwrap_or(&0))).find(|c| c.is_ne()).unwrap_or(Ordering::Equal)
    }
}

/// The version the consumer pins, optionally labelled with the artifact line (`java`, `rust`).
#[derive(Debug, Clone)]
pub struct Pin {
    pub label: Option<String>,
    pub ver: Ver,
}

static PIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*(?:([a-z][a-z+#]*)[\s@:/=]+)?v?(\d+(?:\.\d+){0,3})\s*$").unwrap());

impl Pin {
    pub fn parse(s: &str) -> Option<Pin> {
        let c = PIN.captures(s)?;
        Some(Pin { label: c.get(1).map(|l| l.as_str().to_lowercase()), ver: Ver::parse(&c[2])? })
    }
}

/// Same artifact line: either side unlabelled, or equal labels.
fn same_line(label: Option<&str>, pin: &Pin) -> bool {
    match (label, &pin.label) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => true,
    }
}

static SINCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(since|as of|added in|introduced in|until|before|removed in)\s+(?:([a-z][a-z+#]*)\s+)?v?(\d+\.\d+(?:\.\d+)?)\b").unwrap()
});

/// Whether every "since vX" / "until vY" the text states admits the pinned version.
fn in_range(text: &str, pin: &Pin) -> bool {
    SINCE.captures_iter(text).all(|c| {
        let Some(v) = Ver::parse(&c[3]) else { return true };
        if !same_line(c.get(2).map(|m| m.as_str()), pin) {
            return true;
        }
        match c[1].to_lowercase().as_str() {
            "until" | "before" | "removed in" => pin.ver < v,
            _ => pin.ver >= v,
        }
    })
}

// ---------------------------------------------------------------- sources

static REQ: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#{2,6}\s+\[?([A-Z][A-Z0-9]*(?:-[A-Z0-9]+)*-\d+)\]?\s*[:.\u{2014}\u{2013}-]\s*(.+?)\s*$").unwrap());
static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(#{1,6})\s+(.+?)\s*#*\s*$").unwrap());

/// Requirement headings and their text up to the next heading or rule; the priority line is dropped.
fn spec_claims(path: &str, text: &str) -> Vec<Claim> {
    let mut out = Vec::new();
    let mut cur: Option<(String, String, Vec<&str>)> = None;
    let flush = |cur: &mut Option<(String, String, Vec<&str>)>, out: &mut Vec<Claim>| {
        if let Some((id, title, body)) = cur.take() {
            let body = body.join("\n");
            let first = paragraphs(&body).into_iter().find(|p| !p.starts_with("Priority:")).unwrap_or_default();
            out.push(Claim {
                source: path.into(),
                id: id.clone(),
                text: format!("{id} {title}: {first}"),
                index: format!("{id} {title} {title} {}", words(&plain(&body), INDEX_WORDS)),
                reqs: vec![id.clone()],
                kind: Kind::Spec,
                line_newer: false,
            });
        }
    };
    for line in text.lines() {
        if let Some(c) = REQ.captures(line) {
            flush(&mut cur, &mut out);
            cur = Some((c[1].to_string(), plain(&c[2]), Vec::new()));
        } else if line.starts_with('#') || line.trim() == "---" {
            flush(&mut cur, &mut out);
        } else if let Some((_, _, body)) = cur.as_mut() {
            body.push(line);
        }
    }
    flush(&mut cur, &mut out);
    out
}

static RELEASE_PART: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\b([A-Za-z][A-Za-z+#]*)\s+)?\[?v?(\d+\.\d+(?:\.\d+)?)\]?(?:\s*\(([A-Za-z][A-Za-z+#]*)\))?").unwrap());
static DATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d{4}-\d{2}-\d{2}").unwrap());
/// A requirement id cited in a change, `[CORE-002]` or `VER-024`.
static REQ_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[A-Z][A-Z0-9]*(?:-[A-Z0-9]+)*-\d+\b").unwrap());

#[derive(Debug)]
struct Release<'a> {
    /// (label, version) of each artifact released together, e.g. ("java", 10.0.0), ("rust", 11.0.0).
    parts: Vec<(Option<String>, Ver)>,
    title: String,
    date: Option<String>,
    /// Headed "unreleased", or undated above dated releases: not installable yet.
    unreleased: bool,
    body: Vec<&'a str>,
}

/// The `## <version>` sections of a changelog, newest first.
fn parse_releases(text: &str) -> Vec<Release<'_>> {
    let mut releases: Vec<Release> = Vec::new();
    let mut in_release = false;
    for line in text.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            let h = h.trim();
            let date = DATE.find(h).map(|m| m.as_str().to_string());
            let head = DATE.find(h).map_or(h, |m| &h[..m.start()]);
            let parts: Vec<(Option<String>, Ver)> = RELEASE_PART
                .captures_iter(head)
                .filter_map(|c| {
                    let label = c.get(1).or(c.get(3)).map(|l| l.as_str().to_lowercase()).filter(|l| !["v", "version", "unreleased"].contains(&l.as_str()));
                    Some((label, Ver::parse(&c[2])?))
                })
                .collect();
            in_release = !parts.is_empty();
            if in_release {
                let title = head.trim().trim_end_matches(['-', '\u{2014}', '\u{2013}', ' ']).trim_matches(['[', ']']).to_string();
                let unreleased = h.to_lowercase().contains("unreleased");
                releases.push(Release { parts, title, date, unreleased, body: Vec::new() });
            }
        } else if in_release && let Some(r) = releases.last_mut() {
            r.body.push(line);
        }
    }
    // Undated sections above the first dated release are work in progress.
    if let Some(dated) = releases.iter().position(|r| r.date.is_some()) {
        releases[..dated].iter_mut().for_each(|r| r.unreleased = true);
    }
    releases
}

/// Make the pin's artifact line explicit. An unlabelled pin takes the changelog's only line; when
/// the released sections name several lines (or labelled and unlabelled ones), the pin is ambiguous.
fn resolve_pin(pin: &Pin, releases: &[Release]) -> Result<Pin, String> {
    if pin.label.is_some() {
        return Ok(pin.clone());
    }
    let parts = releases.iter().filter(|r| !r.unreleased).flat_map(|r| &r.parts);
    let labels: BTreeSet<&str> = parts.clone().filter_map(|(l, _)| l.as_deref()).collect();
    let unlabelled = parts.clone().any(|(l, _)| l.is_none());
    match labels.len() {
        0 => Ok(pin.clone()),
        1 if !unlabelled => Ok(Pin { label: labels.first().map(|l| l.to_string()), ver: pin.ver.clone() }),
        _ => {
            let mut found: Vec<&str> = labels.into_iter().collect();
            if unlabelled {
                found.push("(unlabelled)");
            }
            Err(format!(
                "version {} names no artifact line, and the changelog has several: {}; pin as e.g. \"{} {}\"",
                ver_str(&pin.ver),
                found.join(", "),
                found[0],
                ver_str(&pin.ver)
            ))
        }
    }
}

/// One claim per change of each release (a top-level bullet or a paragraph), newest release first.
fn release_claims(path: &str, releases: &[Release], newer: &[bool], pin: Option<&Pin>, short: &str) -> Vec<Claim> {
    let mut out = Vec::new();
    for (r, &newer) in releases.iter().zip(newer) {
        let shown = match pin {
            Some(p) => r.parts.iter().find(|(l, _)| l.is_some() && same_line(l.as_deref(), p)).map(|(l, v)| (l.as_deref(), v)),
            None => None,
        };
        let shown = match shown.or_else(|| r.unreleased.then(|| r.parts.first().map(|(l, v)| (l.as_deref(), v))).flatten()) {
            Some((l, v)) => l.map_or_else(|| ver_str(v), |l| format!("{} {}", capitalise(l), ver_str(v))),
            None => r.title.clone(),
        };
        let date = r.date.as_ref().map(|d| format!(" ({d})")).unwrap_or_default();
        let line_newer = newer && pin.is_some_and(|p| r.parts.iter().any(|(l, v)| same_line(l.as_deref(), p) && v > &p.ver));
        let lead = if r.unreleased {
            format!("unreleased {shown} (in progress at {short})")
        } else if newer {
            format!("newer release {shown}{date}")
        } else {
            format!("release {shown}{date}")
        };
        // Anchored by the release's first artifact: `java-10.0.0:3` is the third change of that release.
        let anchor = r.parts.first().map_or_else(|| slug(&r.title), |(l, v)| slug(&format!("{} {}", l.as_deref().unwrap_or("v"), ver_str(v))));
        for (n, (section, change)) in changes(&r.body).into_iter().enumerate() {
            // "### Added" reads "added <change>"; a titled section ("### Added: snapshots") keeps its words.
            let what = match section.as_str() {
                "" => String::new(),
                s if !s.contains(' ') => format!("{} ", s.to_lowercase()),
                s => format!("{s}: "),
            };
            out.push(Claim {
                source: path.into(),
                id: format!("{anchor}:{}", n + 1),
                text: format!("{lead}: {what}{}", first_sentences(&change)),
                index: format!("{} {section} {}", r.title, words(&change, INDEX_WORDS)),
                reqs: Vec::new(),
                kind: Kind::Release { newer },
                line_newer,
            });
        }
    }
    out
}

/// Which releases are newer than the pin. A release that names the pin's line compares versions;
/// one that does not (e.g. a Rust-only release for a Java pin) is newer iff it is listed above the
/// newest release at or below the pin (changelogs list newest first). Unreleased sections are never
/// newer releases: nobody can install them.
fn newer_flags(releases: &[Release], pin: Option<&Pin>) -> Vec<bool> {
    let Some(pin) = pin else { return vec![false; releases.len()] };
    let cmp = |r: &Release| r.parts.iter().find(|(l, _)| same_line(l.as_deref(), pin)).map(|(_, v)| v.cmp(&pin.ver));
    let pinned = releases.iter().position(|r| !r.unreleased && cmp(r).is_some_and(|o| o.is_le())).unwrap_or(releases.len());
    releases.iter().enumerate().map(|(i, r)| !r.unreleased && cmp(r).map_or(i < pinned, |o| o.is_gt())).collect()
}

/// Requirement ids that arrived after the pin: the oldest release citing them is newer than the
/// pin, or only unreleased sections cite them. Ids the changelog never cites are not judged.
fn late_reqs<'a>(releases: &'a [Release], newer: &[bool]) -> BTreeSet<&'a str> {
    // Newest first, so the last write per id is its oldest citing release.
    let mut first: HashMap<&str, bool> = HashMap::new();
    for (r, &newer) in releases.iter().zip(newer) {
        for line in &r.body {
            for m in REQ_ID.find_iter(line) {
                first.insert(m.as_str(), newer || r.unreleased);
            }
        }
    }
    first.into_iter().filter(|&(_, late)| late).map(|(id, _)| id).collect()
}

/// A release body as (section, change): top-level bullets with their continuation lines, and loose
/// paragraphs. `### Added` etc. name the section; code blocks are skipped.
fn changes(body: &[&str]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut section = String::new();
    // The change being read, and whether it is a bullet (which survives blank lines) or a paragraph.
    let mut cur: Option<(String, bool)> = None;
    let mut fence = false;
    let mut blank = false;
    let push = |cur: &mut Option<(String, bool)>, out: &mut Vec<(String, String)>, section: &str| {
        if let Some(c) = cur.take().map(|c| plain(&c.0)).filter(|c| !c.is_empty()) {
            out.push((section.to_string(), c));
        }
    };
    for line in body {
        if line.trim_start().starts_with("```") {
            fence = !fence;
            continue;
        }
        if fence || line.trim() == "---" {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if let Some(h) = line.strip_prefix("###") {
            push(&mut cur, &mut out, &section);
            section = h.trim_start_matches('#').trim().to_string();
        } else if line.starts_with("- ") || line.starts_with("* ") {
            push(&mut cur, &mut out, &section);
            cur = Some((line[2..].to_string(), true));
        } else if line.trim().is_empty() {
            if cur.as_ref().is_some_and(|c| !c.1) {
                push(&mut cur, &mut out, &section);
            }
        } else if let Some(c) = cur.as_mut().filter(|c| indented && c.1 || !blank) {
            // An indented line continues a bullet; any line right after text continues it.
            c.0.push(' ');
            c.0.push_str(line.trim());
        } else {
            push(&mut cur, &mut out, &section);
            cur = Some((line.trim().to_string(), false));
        }
        blank = line.trim().is_empty();
    }
    push(&mut cur, &mut out, &section);
    out
}

/// One claim per theorem: what it proves for which requirement, under which fragment.
fn proof_claims(path: &str, text: &str) -> Vec<Claim> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let Some(theorems) = v["theorems"].as_array() else { return Vec::new() };
    theorems
        .iter()
        .filter_map(|t| {
            let name = t["name"].as_str()?;
            let module = t["module"].as_str().unwrap_or_default();
            let ids: Vec<String> = t["specIds"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| format!("{} ({})", s["id"].as_str().unwrap_or_default(), s["fragment"].as_str().unwrap_or_default()))
                .collect();
            Some(Claim {
                source: path.into(),
                id: name.into(),
                text: format!("Lean {name} ({module}) proves {}", ids.join("; ")),
                index: format!("proof proven lean theorem {name} {module} {}", ids.join(" ")),
                reqs: t["specIds"].as_array().into_iter().flatten().filter_map(|s| s["id"].as_str().map(String::from)).collect(),
                kind: Kind::Proof,
                line_newer: false,
            })
        })
        .collect()
}

/// Every heading with its first paragraph (code blocks skipped).
fn doc_claims(path: &str, text: &str) -> Vec<Claim> {
    let mut out = Vec::new();
    let mut cur: Option<(String, Vec<&str>)> = None;
    let mut fence = false;
    let flush = |cur: &mut Option<(String, Vec<&str>)>, out: &mut Vec<Claim>| {
        if let Some((title, body)) = cur.take() {
            let first = paragraphs(&body.join("\n")).into_iter().next().unwrap_or_default();
            let text = if first.is_empty() { title.clone() } else { format!("{title}: {first}") };
            out.push(Claim {
                source: path.into(),
                id: slug(&title),
                index: format!("{title} {title} {}", words(&first, INDEX_WORDS)),
                text,
                reqs: Vec::new(),
                kind: Kind::Doc,
                line_newer: false,
            });
        }
    };
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        if let Some(c) = HEADING.captures(line) {
            flush(&mut cur, &mut out);
            cur = Some((plain(&c[2]), Vec::new()));
        } else if let Some((_, body)) = cur.as_mut() {
            body.push(line);
        }
    }
    flush(&mut cur, &mut out);
    out
}

// ---------------------------------------------------------------- text

static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!?\[([^\]]*)\]\([^)]*\)").unwrap());
static HTML: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());

/// Markdown to plain text: links to their text, emphasis and tags removed, whitespace collapsed.
fn plain(s: &str) -> String {
    let s = LINK.replace_all(s, "$1");
    let s = HTML.replace_all(&s, "");
    s.replace("**", "").replace("__", "").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Paragraphs of a markdown body as plain text, skipping code blocks, tables and images. A paragraph
/// ending in ':' runs on into the next one, the list it introduces.
fn paragraphs(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut fence = false;
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            fence = !fence;
            continue;
        }
        if fence || t.starts_with('|') || t.starts_with("![") || t.starts_with("<!--") {
            continue;
        }
        if t.is_empty() {
            if !cur.is_empty() && !cur.ends_with(':') {
                out.push(plain(&cur));
                cur.clear();
            }
        } else {
            cur.push(' ');
            cur.push_str(t);
        }
    }
    if !cur.is_empty() {
        out.push(plain(&cur));
    }
    out.retain(|p| !p.is_empty());
    out
}

/// Leading sentences, enough to say what the change is (the printed line clips further).
fn first_sentences(s: &str) -> String {
    let mut out = String::new();
    for part in s.split_inclusive(". ") {
        out.push_str(part);
        if out.chars().count() >= CLAIM_CHARS {
            break;
        }
    }
    out.trim().to_string()
}

fn words(s: &str, n: usize) -> String {
    s.split_whitespace().take(n).collect::<Vec<_>>().join(" ")
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let cut: String = s.chars().take(n).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(a, _)| a);
    format!("{}…", cut.trim_end_matches([',', ';', ':']))
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() || c == '.' {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

fn ver_str(v: &Ver) -> String {
    v.0.iter().map(u64::to_string).collect::<Vec<_>>().join(".")
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn run(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    const SPEC1: &str = "# Core\n\n#### CORE-001: Token order\n\n**Priority:** MUST\n\nTokens in a place are consumed in FIFO order.\n\n---\n";
    const LOG1: &str = "# Changelog\n\n## Java 1.0.0 / Rust 1.0.0 — 2026-01-01\n\n### Added\n\n- **Executor.** Runs a net with FIFO token order.\n";

    /// Commit 1: CORE-001 and release 1.0.0. Commit 2: CORE-002 (snapshot restore, since 2.0) and
    /// release 2.0.0 that adds it. Then an uncommitted CORE-003 in the working tree.
    fn fixture() -> (Fixture, String, String) {
        let dir = std::env::temp_dir().join(format!("borg-expert-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("spec")).unwrap();
        let f = Fixture(dir.clone());
        run(&dir, &["init", "-q"]);
        std::fs::write(dir.join("spec/01-core.md"), SPEC1).unwrap();
        std::fs::write(dir.join("CHANGELOG.md"), LOG1).unwrap();
        std::fs::write(dir.join("README.md"), "# Demo\n\nA Petri net engine.\n\n## Install\n\nAdd the crate.\n").unwrap();
        run(&dir, &["add", "-A"]);
        run(&dir, &["commit", "-qm", "one"]);
        let c1 = git(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let spec2 = format!(
            "{SPEC1}\n#### CORE-002: Marking snapshot and restore\n\n**Priority:** MUST\n\nSince 2.0 a marking snapshot can be restored as the initial marking of a new run.\n"
        );
        std::fs::write(dir.join("spec/01-core.md"), spec2).unwrap();
        let log2 = LOG1.replace(
            "# Changelog\n\n",
            "# Changelog\n\n## Java 2.0.0 / Rust 2.1.0 — 2026-02-01\n\n### Added\n\n- **Snapshot restore ([CORE-002]).** A marking snapshot restores as the initial marking.\n\n## Rust 1.1.0 — 2026-01-15\n\n- Faster firing in Rust.\n\n",
        );
        std::fs::write(dir.join("CHANGELOG.md"), log2).unwrap();
        run(&dir, &["commit", "-qam", "two"]);
        let c2 = git(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let spec3 = std::fs::read_to_string(dir.join("spec/01-core.md")).unwrap() + "\n#### CORE-003: Snapshot export\n\nSnapshots export to JSON.\n";
        std::fs::write(dir.join("spec/01-core.md"), spec3).unwrap();
        (f, c1, c2)
    }

    fn ids(a: &Answer) -> Vec<String> {
        a.claims.iter().map(|c| format!("{}#{}", c.source, c.id)).collect()
    }

    #[test]
    fn a_claim_added_after_at_is_invisible() {
        let (f, c1, c2) = fixture();
        let q = "restore a marking snapshot as the initial marking";
        let early = ask(&f.0, &c1, None, q, None).unwrap();
        assert!(ids(&early).iter().all(|i| !i.contains("CORE-002") && !i.contains("2.0.0")), "{:?}", ids(&early));
        assert!(!early.render().to_lowercase().contains("snapshot"), "{}", early.render());
        let late = ask(&f.0, &c2, None, q, None).unwrap();
        assert!(ids(&late).contains(&"spec/01-core.md#CORE-002".to_string()), "{:?}", ids(&late));
        // The uncommitted working-tree requirement never shows, even at the latest commit.
        assert!(!late.render().contains("CORE-003"));
        assert!(ask(&f.0, "HEAD", None, "snapshot export JSON", None).unwrap().claims.iter().all(|c| c.id != "CORE-003"));
    }

    #[test]
    fn newer_releases_surface_and_since_filters() {
        let (f, _, c2) = fixture();
        let a = ask(&f.0, &c2, Some("java 1.0.0"), "restore a marking snapshot", None).unwrap();
        let out = a.render();
        // CORE-002 says "since 2.0": not in 1.0.0, so dropped; the 2.0.0 release that adds it surfaces.
        assert!(!out.contains("#CORE-002]"), "{out}");
        assert!(out.contains("newer release Java 2.0.0 (2026-02-01): added Snapshot restore ([CORE-002])"), "{out}");
        assert!(out.starts_with(&format!("[{}@{} CHANGELOG.md#", f.0.file_name().unwrap().to_string_lossy(), a.short)), "{out}");
        // Pinned at 2.0.0 the requirement is in range and the release is not newer.
        let out = ask(&f.0, &c2, Some("java 2.0.0"), "restore a marking snapshot", None).unwrap().render();
        assert!(out.contains("#CORE-002] CORE-002 Marking snapshot and restore: Since 2.0"), "{out}");
        assert!(!out.contains("newer release"), "{out}");
        // Newer releases of the pinned line always come first, whatever the query; a Rust-only
        // release is not a newer Java release.
        let out = ask(&f.0, &c2, Some("java 1.0.0"), "FIFO token order", None).unwrap().render();
        let first = out.lines().next().unwrap();
        assert!(first.ends_with("1 newer release(s) than the pinned version: Java 2.0.0 (2026-02-01)"), "{out}");
        assert!(first.contains("CHANGELOG.md#java-2.0.0]"), "{out}");
        assert!(out.lines().count() <= MAX_CLAIMS);
        // A Rust pin reads its own line; at the newest Java nothing is newer.
        let out = ask(&f.0, &c2, Some("rust 2.0.0"), "FIFO token order", None).unwrap().render();
        assert!(out.lines().next().unwrap().ends_with("1 newer release(s) than the pinned version: Rust 2.1.0 (2026-02-01)"), "{out}");
        let out = ask(&f.0, &c2, Some("java 2.0.0"), "FIFO token order", None).unwrap().render();
        assert!(!out.contains("newer release"), "{out}");
    }

    #[test]
    fn rust_only_release_is_newer_for_a_java_pin_by_position() {
        let (f, _, c2) = fixture();
        let claims = index(&f.0, &c2, Pin::parse("java 1.0.0").as_ref()).unwrap();
        let rust = claims.iter().find(|c| c.text.contains("Faster firing")).unwrap();
        assert_eq!(rust.kind, Kind::Release { newer: true });
        let claims = index(&f.0, &c2, Pin::parse("rust@2.1").as_ref()).unwrap();
        assert!(claims.iter().filter(|c| matches!(c.kind, Kind::Release { .. })).all(|c| c.kind == Kind::Release { newer: false }));
    }

    /// A repo with one commit of `files`, in a directory of its own.
    fn repo_with(name: &str, files: &[(&str, &str)]) -> (Fixture, String) {
        let dir = std::env::temp_dir().join(format!("borg-expert-{name}-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        let f = Fixture(dir.clone());
        for (path, text) in files {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        run(&dir, &["init", "-q"]);
        run(&dir, &["add", "-A"]);
        run(&dir, &["commit", "-qm", "one"]);
        let c = git(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        (f, c)
    }

    const VER_SPEC: &str = "# Verification\n\n#### VER-001: Deadlock check\n\nThe verifier proves deadlock freedom.\n\n#### VER-024: Stubborn set reduction\n\nPartial order reduction of the state space with stubborn sets.\n\n#### VER-030: Uncited bound\n\nPlace bounds are reported.\n";
    const VER_LOG: &str = "# Changelog\n\n## Java 10.0.0 / Rust 11.0.0 — 2026-10-08\n\n- **Stubborn sets ([VER-024]).** Partial order reduction.\n- VER-001 is faster.\n\n## Java 9.0.0 / Rust 10.0.0 — 2026-10-05\n\n- **Deadlock check (VER-001).** First release of the verifier.\n";
    const VER_PROOF: &str = r#"{"theorems":[{"name":"stubborn_exact","module":"L/S.lean","specIds":[{"id":"VER-024","fragment":"untimed"}]},{"name":"deadlock_sound","module":"L/D.lean","specIds":[{"id":"VER-001","fragment":"untimed"}]}]}"#;

    #[test]
    fn a_requirement_first_cited_in_a_newer_release_is_dropped() {
        let (f, c) = repo_with("late", &[("spec/07-verification.md", VER_SPEC), ("CHANGELOG.md", VER_LOG), ("lean/proof-coverage.json", VER_PROOF)]);
        let q = "stubborn set partial order reduction of the state space";
        let ids_of = |v: &str| {
            index(&f.0, &c, Pin::parse(v).as_ref()).unwrap().into_iter().filter(|c| !matches!(c.kind, Kind::Release { .. })).map(|c| c.id).collect::<Vec<_>>()
        };
        // Pinned at 9.0.0: VER-024 arrives in 10.0.0, though nothing in its text says "since".
        let at9 = ids_of("java 9.0.0");
        assert!(!at9.iter().any(|i| i == "VER-024" || i == "stubborn_exact"), "{at9:?}");
        // VER-001 was cited in 9.0.0 (and again later); VER-030 is never cited: both stay.
        assert!(["VER-001", "deadlock_sound", "VER-030"].iter().all(|i| at9.iter().any(|x| x == i)), "{at9:?}");
        let out = ask(&f.0, &c, Some("java 9.0.0"), q, None).unwrap().render();
        assert!(!out.contains("#VER-024]") && !out.contains("#stubborn_exact]"), "{out}");
        assert!(out.contains("newer release Java 10.0.0 (2026-10-08): Stubborn sets ([VER-024])"), "{out}");
        // The same line read through Rust, and a pin at 10.0.0, both see it.
        assert!(ids_of("rust 11.0.0").iter().any(|i| i == "VER-024"));
        let at10 = ids_of("java 10.0.0");
        assert!(at10.iter().any(|i| i == "VER-024") && at10.iter().any(|i| i == "stubborn_exact"), "{at10:?}");
        // Without a pin the repo at the commit is current: everything shows.
        assert!(ask(&f.0, &c, None, q, None).unwrap().render().contains("#VER-024]"));
    }

    /// A flowlib-shaped repo: four language lines, a Java 10.0.0 above the consumer's 9.0.0.
    const E1_LOG: &str = "# Changelog\n\n\
## Java 10.0.0 / Rust 11.0.0 — 2026-10-08\n\n### Changed\n\n\
- **Breaking:** `PetriNet.verify()` reports in-flight tokens separately (VER-004).\n\
- Timer wheel granularity is configurable per executor.\n\
- DBM float drift is bounded for long-running timed nets.\n\n\
## TypeScript 9.0.0 — 2026-10-06\n\n- Order of places in the debug UI is stable.\n\n\
## Java 9.0.0 / Rust 10.0.0 — 2026-10-05\n\n\
- `bounded(k)` is refused on a place the net writes (VER-006).\n\
- Executor shutdown drains in-flight transitions.\n\n\
## Java 8.0.0 — 2026-09-01\n\n- First Java release of the verifier.\n";
    const E1_SPEC: &str = "# Core\n\n#### CORE-010: Token order\n\nTokens in a place are consumed in FIFO order by the executor.\n\n\
#### CORE-020: Timer transitions\n\nA timed transition fires within its interval; the executor schedules timers on a wheel.\n\n\
#### VER-006: Bounded places\n\n`bounded(k)` holds only for places the net never writes; `deadlockFree` is checked on the state class graph.\n";
    /// E1's plan shape: a long plan about porting a service, one sentence on flowlib.
    const E1_PLAN: &str = "## Plan: port the order service to the new runtime\n\n\
1. Move the order state machine from the old scheduler into the new module, keeping the executor thread pool and the timer configuration.\n\
2. Replace the hand-written retry loop with a bounded queue; tokens of work flow through the order places in FIFO order.\n\
3. Model the order workflow as a net on flowlib 9.0.0 with `PetriNet.builder()`, and check it with `deadlockFree` before wiring it in.\n\
4. Keep the debug UI, the metrics and the timer wheel; run the integration tests and update the docs.\n";

    #[test]
    fn e1_a_plan_about_porting_on_java_9_surfaces_the_java_10_release() {
        let (f, c) = repo_with(
            "e1",
            &[("CHANGELOG.md", E1_LOG), ("spec/core.md", E1_SPEC), ("README.md", "# flowlib\n\nPetri nets for Java, TypeScript, Rust and Python.\n")],
        );
        let ix = Indexed::open(&f.0, &c, Some("java 9.0.0"), None).unwrap();
        let first = |a: &Answer| a.claims.first().map(|c| c.text.clone()).unwrap_or_default();
        // (a) Asked with the whole plan (what E1 did), the newer Java release still comes first.
        let whole = ix.answer(E1_PLAN);
        assert_eq!(first(&whole), "1 newer release(s) than the pinned version: Java 10.0.0 (2026-10-08)", "{}", whole.render());
        assert!(whole.render().lines().next().unwrap().contains("CHANGELOG.md#java-10.0.0]"), "{}", whole.render());
        // (b) The query is the plan's flowlib sentence plus the dependency's names, not the plan.
        let (sentences, query) = plan_query(E1_PLAN, &["flowlib", "org.flowlib"], &ix.api()).unwrap();
        assert_eq!(sentences.len(), 1, "{sentences:?}");
        assert!(sentences[0].contains("flowlib 9.0.0") && !query.contains("order service"), "{query}");
        let a = ix.answer(&query);
        assert_eq!(first(&a), "1 newer release(s) than the pinned version: Java 10.0.0 (2026-10-08)", "{}", a.render());
        assert!(a.claims.len() <= MAX_CLAIMS);
        // The focused query finds the breaking change to the API the plan uses.
        assert!(a.render().contains("PetriNet.verify()"), "{}", a.render());
    }

    /// E1's changelog plus prose about everyday code, and the Java code that declares the API.
    const API_LOG: &str = "\n## Java 8.1.0 — 2026-09-10\n\n- `fireAsync` returns a `CompletableFuture`; markings moved from `HashMap` to `LinkedHashMap`; \
events carry `created_at` and `timeoutMs`; `ConcurrentHashMap` and `ExecutorService` are no longer exposed; `getState` is gone.\n";
    const API_JAVA: &str = "package org.flowlib;\n\nimport java.util.HashMap;\n\n/** Unlike a HashMap, a PetriNet is immutable. */\n\
public final class PetriNet {\n  private final HashMap<String, Long> timeoutMs = new HashMap<>();\n\
  public static Builder builder() { return new Builder(); }\n  public boolean deadlockFree() { return true; }\n\
  public CompletableFuture<Marking> fireAsync(Transition t) { return null; }\n  public String getState() { return \"\"; }\n\
  @Override public String toString() { return \"net\"; }\n}\n";

    #[test]
    fn a_plan_mentions_a_dependency_by_name_or_api() {
        let log = format!("{E1_LOG}{API_LOG}");
        let (f, c) = repo_with("mention", &[("CHANGELOG.md", &log), ("spec/core.md", E1_SPEC), ("java/src/main/java/org/flowlib/PetriNet.java", API_JAVA)]);
        let api = Indexed::open(&f.0, &c, Some("java 9.0.0"), None).unwrap().api();
        assert!(api.types.contains("PetriNet") && api.members.contains("deadlockFree") && api.members.contains("fireAsync"), "{api:?}");
        // Named in the changelog but not declared by the repo, or a standard name: no API.
        for w in [
            "TypeScript",
            "Timer",
            "HashMap",
            "LinkedHashMap",
            "CompletableFuture",
            "ConcurrentHashMap",
            "ExecutorService",
            "created_at",
            "timeoutMs",
            "toString",
        ] {
            assert!(!api.types.contains(w) && !api.members.contains(w), "{w}: {api:?}");
        }
        let names = ["flowlib", "org.flowlib"];
        // Everyday Java in a project that depends on flowlib: never about flowlib.
        for plan in [
            "Replace the HashMap with a LinkedHashMap so iteration order is stable.",
            "Add a created_at column to the users table.",
            "Wrap the call in a CompletableFuture and submit it to the ExecutorService; pass timeoutMs through.",
            "Catch the InterruptedException and use a ConcurrentHashMap.",
        ] {
            assert_eq!(plan_query(plan, &names, &api), None, "{plan}");
        }
        // One member alone is too weak (the consumer's own getState); two, or a type, are the API.
        assert_eq!(plan_query("Call fireAsync on the order.", &names, &api), None);
        assert!(plan_query("Call fireAsync, then check deadlockFree.", &names, &api).is_some());
        // A consumer that declares the name itself means its own.
        let (own, _) = repo_with("consumer", &[("src/Net.java", "public class PetriNet { }\n")]);
        let plan = "Build the net with PetriNet.builder() first.";
        assert_eq!(plan_query(plan, &names, &api.clone().without_declared_in(&own.0.join("src"), plan)), None);
        let (other, _) = repo_with("consumer2", &[("src/App.java", "class App { PetriNet net = PetriNet.builder().build(); }\n")]);
        assert!(plan_query(plan, &names, &api.clone().without_declared_in(&other.0, plan)).is_some(), "using it is no declaring it");
        // By API only.
        let (s, _) = plan_query("Refactor the cache.\nBuild the net with PetriNet.builder() first.", &names, &api).unwrap();
        assert_eq!(s, ["Build the net with PetriNet.builder() first."]);
        // By coordinate; never by a word that merely contains the name.
        assert!(plan_query("Bump org.flowlib:flowlib to 10.", &names, &api).is_some());
        assert!(plan_query("Rename myflowlibx and the flowlibfication step.", &names, &api).is_none());
        assert!(plan_query("Fix the CSS of the login page. Then run the tests.", &names, &api).is_none());
    }

    #[test]
    fn declarations_read_the_same_by_word_or_by_keyword() {
        let (f, c) =
            repo_with("decl", &[("java/PetriNet.java", API_JAVA), ("rs/lib.rs", "pub struct Marking;\npub fn fire_transition() {}\n// uses created_at\n")]);
        let few: BTreeSet<String> = ["PetriNet", "deadlockFree", "fire_transition", "Marking", "HashMap", "created_at"].map(String::from).into();
        let mut many = few.clone();
        many.extend((0..20).map(|i| format!("unusedName{i}")));
        for names in [&few, &many] {
            let api = declarations(&f.0, Some(&c), names);
            assert_eq!(api.types, ["Marking", "PetriNet"].map(String::from).into(), "{} names", names.len());
            assert_eq!(api.members, ["deadlockFree", "fire_transition"].map(String::from).into(), "{} names", names.len());
        }
        assert_eq!(declarations(Path::new("/nonexistent"), None, &few), Api::default(), "no repo, no API");
    }

    #[test]
    fn unreleased_sections_are_never_newer_releases() {
        let log = "# Changelog\n\n## 2.0.0 (unreleased)\n\n- Modular composition of nets.\n\n## 1.8.5\n\n- Fix a timer.\n\n## 1.8.4\n\n- Fix a leak.\n";
        let (f, c) = repo_with("unrel", &[("CHANGELOG.md", log)]);
        let a = ask(&f.0, &c, Some("1.8.5"), "modular composition", None).unwrap();
        let out = a.render();
        assert!(out.contains(&format!("unreleased 2.0.0 (in progress at {}): Modular composition", a.short)), "{out}");
        assert!(!out.contains("newer release"), "{out}");
        // A labelled "— unreleased" heading, and an undated one above dated releases.
        let log = "# Changelog\n\n## Java 2.14.0\n\n- Draft work.\n\n## Java 2.13.0 — unreleased\n\n- `Builder.executor(ExecutorService)` renamed.\n\n## Java 2.12.0 — 2026-07-14\n\n- Executor tweaks.\n";
        let releases = parse_releases(log);
        assert_eq!(releases.iter().map(|r| r.unreleased).collect::<Vec<_>>(), [true, true, false]);
        assert!(releases[0].parts.iter().all(|(l, _)| l.as_deref() == Some("java")));
        let (f, c) = repo_with("unrel2", &[("CHANGELOG.md", log)]);
        let out = ask(&f.0, &c, Some("java 2.12.0"), "executor renamed", None).unwrap().render();
        assert!(out.contains("unreleased Java 2.13.0 (in progress at"), "{out}");
        assert!(!out.contains("newer release"), "{out}");
        // A `(unreleased)` suffix is no artifact line: an unlabelled pin still resolves.
        assert!(resolve_pin(&Pin::parse("1.0").unwrap(), &parse_releases("## 2.0.0 (unreleased)\n\n## 1.0.0 — 2026-01-01\n")).unwrap().label.is_none());
    }

    #[test]
    fn an_unlabelled_pin_needs_a_single_line() {
        let one = parse_releases("## Java 2.0.0 — 2026-02-01\n\n## Java 1.0.0 — 2026-01-01\n");
        assert_eq!(resolve_pin(&Pin::parse("1.0.0").unwrap(), &one).unwrap().label.as_deref(), Some("java"));
        // flowlib at 739e671 / d2db2e8: labelled Rust and Python sections over unlabelled ones.
        let mixed = parse_releases("## Unreleased\n\n## 2.8.0 (Python) — 2026-06-01\n\n## 3.0.0 (Rust) / 2.7.0 (Python) — 2026-06-01\n\n## 2.6.0\n");
        let e = resolve_pin(&Pin::parse("2.6.0").unwrap(), &mixed).unwrap_err();
        assert!(e.contains("python, rust, (unlabelled)"), "{e}");
        assert_eq!(resolve_pin(&Pin::parse("python 2.6.0").unwrap(), &mixed).unwrap().label.as_deref(), Some("python"));
        // The fixture's Java + Rust changelog: an unlabelled pin is an error, not a guess.
        let (f, _, c2) = fixture();
        let e = ask(&f.0, &c2, Some("1.0.0"), "snapshot", None).unwrap_err();
        assert!(e.contains("java, rust"), "{e}");
        // The resolved line reaches in_range: a single-line Java changelog ignores "since rust 2.0".
        let spec = "# S\n\n#### CORE-009: Rust-only thing\n\nAvailable since rust 2.0 everywhere.\n";
        let (f, c) = repo_with("label", &[("spec/s.md", spec), ("CHANGELOG.md", "## Java 1.0.0 — 2026-01-01\n\n- First.\n")]);
        assert!(index(&f.0, &c, Pin::parse("1.0").as_ref()).unwrap().iter().any(|c| c.id == "CORE-009"));
    }

    #[test]
    fn at_must_not_pass_the_ceiling() {
        let (f, c1, c2) = fixture();
        let q = "snapshot";
        assert!(ask(&f.0, &c1, None, q, Some(&c2)).is_ok());
        assert!(ask(&f.0, &c2[..10], None, q, Some(&c2)).is_ok());
        let e = ask(&f.0, &c2, None, q, Some(&c1)).unwrap_err();
        assert!(e.contains("not at or before the ceiling"), "{e}");
        // Symbolic refs are refused under a ceiling, even when they resolve below it.
        assert!(ask(&f.0, "HEAD~1", None, q, Some(&c2)).unwrap_err().contains("must be a commit hash"));
        assert!(ask(&f.0, "HEAD", None, q, None).is_ok());
    }

    #[test]
    fn a_subdirectory_reads_the_whole_repo() {
        let (f, _, c2) = fixture();
        let a = ask(&f.0.join("spec"), &c2, None, "restore a marking snapshot as the initial marking", None).unwrap();
        assert_eq!(a.repo, f.0.file_name().unwrap().to_string_lossy());
        assert!(ids(&a).contains(&"spec/01-core.md#CORE-002".to_string()), "{:?}", ids(&a));
    }

    #[test]
    fn git_ignores_the_callers_repository_variables() {
        let cmd = git_cmd(Path::new("."));
        let envs: Vec<_> = cmd.get_envs().collect();
        for v in GIT_ENV {
            assert!(envs.contains(&(std::ffi::OsStr::new(v), None)), "{v} not removed");
        }
        assert!(envs.contains(&(std::ffi::OsStr::new("GIT_NO_REPLACE_OBJECTS"), Some(std::ffi::OsStr::new("1")))));
    }

    #[test]
    fn versions_and_pins() {
        assert!(Ver::parse("10").unwrap() == Ver::parse("10.0.0").unwrap());
        assert!(Ver::parse("v9.1").unwrap() < Ver::parse("10.0.0").unwrap());
        let p = Pin::parse("Java 9.0.0").unwrap();
        assert_eq!((p.label.as_deref(), p.ver.0.clone()), (Some("java"), vec![9, 0, 0]));
        assert_eq!(Pin::parse("v5").unwrap().label, None);
        assert!(Pin::parse("latest").is_none());
        let p = Pin::parse("2.0").unwrap();
        assert!(in_range("works since v2.0 and until 3.0", &p));
        assert!(!in_range("added in 2.1", &p));
        assert!(in_range("added in rust 2.1", &Pin::parse("java 2.0").unwrap()));
    }

    #[test]
    fn docs_and_proofs_parse() {
        let d = doc_claims("README.md", "# Lib\n\nIntro text.\n\n```\n# not a heading\n```\n## Use it\n\nCall `run()`.\n");
        assert_eq!(d.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["lib", "use-it"]);
        assert_eq!(d[1].text, "Use it: Call `run()`.");
        let p = proof_claims(
            "lean/proof-coverage.json",
            r#"{"theorems":[{"name":"fifo_sound","module":"L/F.lean","specIds":[{"id":"CORE-001","fragment":"untimed"}]}]}"#,
        );
        assert_eq!(p[0].text, "Lean fifo_sound (L/F.lean) proves CORE-001 (untimed)");
    }
}
