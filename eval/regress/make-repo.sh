#!/bin/bash
# A scripted repo shaped like borg in daily use, for reports/regress.md: three engineers, 14 drone
# sessions over 31 days, a borg note on every commit a drone landed, and the traps a detector meets
# (reflow, move, revert, typo fix, lock file, big rename, same session, young origin, a hand-made
# commit without a note). Usage: make-repo.sh <dir>; the repo is <dir>/app. Deterministic: same
# commit ids on every run. Ground truth: truth.tsv.
set -euo pipefail
edit() { local e=$1; shift; for f in "$@"; do sed -i.bak "$e" "$f" && rm "$f.bak"; done; }
R=${1:?target dir}
rm -rf "$R"; mkdir -p "$R/app" "$R/store" "$R/state"
cd "$R/app"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
T0=1760000000; DAY=86400; HOUR=3600
git init -q -b main
cat > "$R/collectives.json" <<EOF
{"default":"off","collectives":[{"name":"work","paths":["$R/app"],"share":"team"}]}
EOF

# commit <offset-s> <who> <session|-> <subject> <task> <proposal>
commit() {
  local at=$((T0 + $1)) who=$2 s=$3 msg=$4 task=$5 prop=$6
  export GIT_AUTHOR_DATE="@$at +0000" GIT_COMMITTER_DATE="@$at +0000"
  git add -A
  git -c user.name="$who" -c user.email="$(echo "$who" | tr A-Z a-z)@example.com" commit -q -m "$msg"
  if [ "$s" != - ]; then
    git notes --ref=refs/notes/borg add -f -m "borg reasoning (session $s, collective work)

Task:
$task

Last proposal:
$prop
" HEAD
  fi
  echo "$(git rev-parse --short=7 HEAD) $msg" >> "$R/commits.txt"
}

mkdir -p src/models
cat > src/cache.rs <<'EOF'
pub struct Cache { map: HashMap<String, (u64, Instant)> }
impl Cache {
fn key(sym:&str)->String{sym.to_string()}
pub fn get(&self,sym:&str)->Option<u64>{self.map.get(&key(sym)).map(|v|v.0)}
pub fn put(&mut self,sym:&str,p:u64){self.map.insert(key(sym),(p,Instant::now()));}
}
EOF
cat > src/http.rs <<'EOF'
pub fn fetch(url: &str) -> Result<String> {
    let body = retry(3, || get(url))?;
    Ok(body)
}
EOF
cat > src/config.rs <<'EOF'
pub struct Config { pub port: u16, pub timeout_ms: u64 }
pub fn load(env: &Env) -> Config {
    Config {
        port: 8080,
        timeout_ms: 5000,
    }
}
EOF
printf '# ledger\n\nPrices per symbol, cached in front of the upstream.\nRun: `ledger serve`.\n' > README.md
printf '[package]\nname = "ledger"\nversion = "0.1.0"\n\n[dependencies]\nserde = "1.0.100"\n' > Cargo.toml
printf '# lock\nserde 1.0.100\n' > Cargo.lock
for i in $(seq -w 1 45); do printf 'pub struct Price%s { pub v: u64 }\n' "$i" > "src/models/m$i.rs"; done
commit 0 Ann s-01 "Price cache in front of the upstream lookup" "Add a cache in front of the price lookup" "Key the cache by symbol; entries are kept until restart."

# C2: retries, no backoff.
edit 's/retry(3, || get(url))/retry(3, || get(url))   \/\/ immediate retry/' src/http.rs
commit $((1*DAY)) Bo s-02 "Retry failed upstream calls" "Retry failed upstream calls" "Retry three times, immediately."

# C3: the timeout read from the environment, in the wrong unit.
edit 's/timeout_ms: 5000/timeout_ms: env.secs("TIMEOUT") * 100/' src/config.rs
commit $((2*DAY)) Ann s-03 "Configurable upstream timeout" "Make the upstream timeout configurable" "Read TIMEOUT in seconds."
# C4: same session, touches C3's line again.
edit 's/env.secs("TIMEOUT") \* 100/env.secs("UPSTREAM_TIMEOUT") * 100/' src/config.rs
commit $((2*DAY + 3*HOUR)) Ann s-03 "Rename TIMEOUT to UPSTREAM_TIMEOUT" "Make the upstream timeout configurable" "Rename the variable."
# C4b: a commit made by hand, no borg note: the default port.
edit 's/port: 8080/port: 80/' src/config.rs
commit $((3*DAY)) Cy - "Default port 80" "" ""

# C5: rustfmt over cache.rs: a reflow, plus the trailing comma rustfmt adds.
cat > src/cache.rs <<'EOF'
pub struct Cache {
    map: HashMap<String, (u64, Instant)>,
}
impl Cache {
    fn key(sym: &str) -> String { sym.to_string() }
    pub fn get(&self, sym: &str) -> Option<u64> { self.map.get(&key(sym)).map(|v| v.0) }
    pub fn put(&mut self, sym: &str, p: u64) { self.map.insert(key(sym), (p, Instant::now())); }
}
EOF
commit $((6*DAY)) Bo s-04 "style: cargo fmt" "Format the crate" "Run cargo fmt."

# C6: TRUE fix of C1's key.
edit 's/fn key(sym: &str) -> String { sym.to_string() }/fn key(sym: \&str, ccy: \&str) -> String { format!("{sym}:{ccy}") }/' src/cache.rs
commit $((8*DAY)) Ann s-05 "fix: cache keys ignore the currency" "EUR and USD prices of the same symbol overwrite each other in the cache" "Key by symbol and currency."

# C7: move http.rs, no content change.
mkdir -p src/net; git mv src/http.rs src/net/http.rs
commit $((9*DAY)) Bo s-06 "Move the HTTP helpers to src/net" "Group the network code" "git mv only."

# C8: TRUE fix of C3 (unit), said in the task, not the subject.
edit 's/env.secs("UPSTREAM_TIMEOUT") \* 100/env.secs("UPSTREAM_TIMEOUT") * 1000/' src/config.rs
commit $((12*DAY)) Ann s-07 "Timeouts are read in seconds" "A timeout of 30 gives 3 seconds and calls fail under load" "Multiply by 1000."

# C9: a README edit to C1's line, no fault.
edit 's/Prices per symbol, cached/Prices per symbol and currency, cached/' README.md
commit $((13*DAY)) Bo s-08 "Document the currency in the README" "Say in the README that prices are per currency" "One line."
# C9b: log every cache miss.
perl -pi -e 's{^(    pub fn get)}{    // log every miss\n$1}' src/cache.rs
commit $((14*DAY)) Bo s-08 "Log every cache miss" "Log cache misses" "A log line in get."
# C10: TRUE: revert of C9b (it flooded the logs); written by hand to set the date and note.
GIT_AUTHOR_DATE="@$((T0 + 17*DAY)) +0000" GIT_COMMITTER_DATE="@$((T0 + 17*DAY)) +0000" \
  git -c user.name=Ann -c user.email=ann@example.com revert --no-edit HEAD >/dev/null
git notes --ref=refs/notes/borg add -f -m "borg reasoning (session s-09, collective work)

Task:
The miss log fills the disk on the staging box

Last proposal:
Revert it.
" HEAD
echo "$(git rev-parse --short=7 HEAD) $(git log -1 --format=%s)" >> "$R/commits.txt"

# C11: a typo fix on C1's README line: fix-shaped, no decision at fault.
edit 's/Run: `ledger serve`./Run: `ledger serve`, then open :80./' README.md
commit $((20*DAY)) Ann s-09 "Fix typo in the README" "The README misses the port" "Add it."

# C12: TRUE fix of C2 (backoff), across the move.
edit 's|retry(3, \|\| get(url))   // immediate retry|retry_backoff(3, Duration::from_millis(200), \|\| get(url))|' src/net/http.rs
commit $((22*DAY)) Bo s-10 "fix: back off between retries" "The upstream rate-limits us after a failed call; three instant retries make it worse" "Exponential backoff from 200 ms."
# C13: dependency bump (lock file dropped, Cargo.toml line weak).
edit 's/1.0.100/1.0.200/' Cargo.toml Cargo.lock
commit $((23*DAY)) Bo s-10 "Bump serde" "Update dependencies" "serde 1.0.200."

# C14: rename across 45 files: too large.
for f in src/models/m*.rs; do edit 's/Price/Quote/' "$f"; done
commit $((25*DAY)) Ann s-11 "Rename Price to Quote" "Rename Price to Quote everywhere" "Mechanical rename."

# C15: TRUE fix of C4b, which has no note: out of reach.
edit 's/port: 80,/port: 8080,/; s/port: 80 }/port: 8080 }/' src/config.rs
commit $((27*DAY)) Bo s-12 "fix: default port back to 8080" "Port 80 needs root, the service does not start" "8080 again."

# C16: refactor over C6's line, no fault.
edit 's/fn key(sym: &str, ccy: &str) -> String { format!("{sym}:{ccy}") }/fn key(sym: \&str, ccy: \&str) -> String { join_key(sym, ccy) }/' src/cache.rs
commit $((28*DAY)) Bo s-12 "Extract the key format" "Share the key format with the exporter" "A join_key helper."

# C17: TRUE fix of C1 (no expiry): get is C1's line, reformatted by C5.
edit 's/    pub fn get(&self, sym: &str) -> Option<u64> { self.map.get(&key(sym)).map(|v| v.0) }/    pub fn get(\&self, sym: \&str) -> Option<u64> { self.fresh(\&key(sym)).map(|v| v.0) }/' src/cache.rs
commit $((30*DAY)) Ann s-13 "fix: cache entries never expire" "Prices are stale for hours after the upstream changes them" "A 60 s TTL checked on get."
# C18: young: a day later, another session fixes C17's line.
edit 's/self.fresh(&key(sym))/self.fresh(\&key(sym), self.ttl)/' src/cache.rs
commit $((31*DAY)) Bo s-14 "fix: the TTL ignores the config" "The TTL is hard-coded, the config value is ignored" "Pass self.ttl."
