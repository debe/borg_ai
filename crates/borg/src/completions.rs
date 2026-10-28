//! The command table of the borg CLI, and `borg completions <bash|zsh|fish>`: a shell completion script
//! for borg, printed to stdout.
//!
//! One table (`GROUPS`, plus `DEBUG` for `borg debug`) feeds `borg help`, the dispatch gate in main.rs
//! (`known`) and all three shells, so a new command needs one entry here; `HARNESSES` stands for the
//! harnesses' names (harness/mod.rs `ALL`). Paths complete as files where a command takes one (`why`).

/// In a subcommand list: every visible harness's name.
pub const HARNESSES: &str = "<harnesses>";

/// One command: its name, its help rows (the arguments after the name, what it does), and for the
/// shells its subcommands, its flags and whether it takes a path.
pub struct Command {
    pub name: &'static str,
    pub help: &'static [(&'static str, &'static str)],
    pub subs: &'static [&'static str],
    pub flags: &'static [&'static str],
    pub path: bool,
}

const fn cmd(name: &'static str, help: &'static [(&'static str, &'static str)], subs: &'static [&'static str], flags: &'static [&'static str]) -> Command {
    Command { name, help, subs, flags, path: false }
}

/// `borg help`: the commands grouped by what they are for.
pub const GROUPS: &[(&str, &[Command])] = &[
    (
        "Setup",
        &[
            cmd("setup", &[("[--yes] [--no-vectors]", "guided setup: collectives, team, vectors, wiring")], &[], crate::install::setup::FLAGS),
            cmd("install", &[("<HARNESS|all> [--apply]", "wire the hooks (a dry run without --apply)")], &[HARNESSES, "all"], &["--apply", "--home"]),
            cmd("uninstall", &[("<HARNESS|all> [--apply]", "unwire them")], &[HARNESSES, "all"], &["--apply", "--home"]),
            cmd("status", &[("", "each collective's lessons, outcomes and wiring")], &[], &["--home"]),
            cmd("doctor", &[("", "what is broken: config, hooks, adapters, the model CLI")], &[], &["--home"]),
            Command { path: true, ..cmd("collective", &[("[DIR]", "which collective a directory belongs to")], &[], &[]) },
            cmd("vectors", &[("[--fetch|--rebuild] [--store LESSONS]", "the vector model and index")], &[], &["--fetch", "--rebuild", "--store"]),
            cmd("completions", &[("<bash|zsh|fish>", "a shell completion script")], &["bash", "zsh", "fish"], &[]),
        ],
    ),
    (
        "Learning",
        &[
            cmd("learn", &[("", "turn waiting corrections into lessons now")], &[], &[]),
            cmd(
                "no",
                &[("[--session ID] <reason>", "say no to a drone's work: shown lessons lose credit, the reason becomes a lesson")],
                &[],
                crate::veto::FLAGS,
            ),
            cmd(
                "assimilate",
                &[
                    ("[--apply] [--since DATE] [--limit N]", "import corrections from past sessions"),
                    ("--from-dir DIR [--plan FILE] [--apply]", "import lessons from team documents"),
                ],
                &[],
                crate::assimilate::FLAGS,
            ),
            cmd("reach", &[("[--plan FILE] [--apply] [--jobs N]", "sort lessons into team, repo-only and personal")], &[], &["--plan", "--apply", "--jobs"]),
            cmd("facets", &[("[--apply] [--store FILE] [--jobs N]", "search facets for lessons")], &[], &["--apply", "--store", "--jobs"]),
        ],
    ),
    (
        "Decisions",
        &[
            cmd(
                "queen",
                &[("[show|ask|answer|skip|dismiss|dedupe]", "open decisions waiting for you")],
                &["show", "ask", "answer", "skip", "dismiss", "dedupe"],
                &["--json", "--repo", "--dir", "--collective", "--apply"],
            ),
            Command {
                path: true,
                ..cmd("why", &[("<path>[:<line>[-<line>]] | <commit>", "the reasoning behind code  [--full] [--json]")], &[], &["--full", "--json", "--dir"])
            },
            cmd("notes", &[("fetch [<commit>...]", "fetch the team's reasoning notes behind commits")], &["fetch"], &["--dir"]),
            cmd(
                "regress",
                &[("[--replay A..B] [--explain] [--json]", "which earlier decisions later commits fixed (dry run)")],
                &[],
                crate::notes::regress_cli::FLAGS,
            ),
            cmd(
                "review-fix",
                &[("[--since DATE] [--json]", "bugs workflow reviews found and fix steps fixed, before any commit")],
                &[],
                crate::review_fix::FLAGS,
            ),
            cmd("trust", &[("[--dir DIR]", "trust per engineer and area")], &[], &["--dir"]),
        ],
    ),
    (
        "Watching",
        &[
            cmd("top", &[("[--once] [--json] [--dir DIR]", "the drones running across the team, live (q quits)")], &[], &["--once", "--json", "--dir"]),
            cmd("web", &[("[--port PORT] [--dir DIR]", "a live page of knowledge and stats on 127.0.0.1")], &[], &["--port", "--dir"]),
        ],
    ),
    (
        "Team",
        &[
            cmd(
                "mesh",
                &[("<command>", "sync with team members (borg mesh --help)")],
                &["id", "init", "invite", "invites", "join", "revoke", "members", "peer", "serve", "sync", "fetch"],
                &["--collective", "--apply", "--paths", "--role", "--expires", "--uses", "--png", "--revoke"],
            ),
            cmd(
                "steer",
                &[("<drone|all> <text> | --pause | --resume", "a signed instruction to running drones")],
                &["all"],
                &["--pause", "--resume", "--veto"],
            ),
        ],
    ),
];

/// `borg debug <command>`: evaluation and debugging tools, out of the main help.
pub const DEBUG: &[Command] = &[
    Command { path: true, ..cmd("recall", &[("<lessons.jsonl> [before_ms]", "rank lessons for text on stdin")], &[], &[]) },
    Command { path: true, ..cmd("resolve", &[("<lessons.jsonl>", "the word rule's changed/ignored for {asked, before, edits, final} on stdin")], &[], &[]) },
    Command { path: true, ..cmd("prefilter", &[("<lessons.jsonl> [before_ms]", "the free gate's score for a turn on stdin")], &[], &[]) },
    Command { path: true, ..cmd("memory", &[("<lessons.jsonl> [--session ID]", "each lesson's activation and usefulness")], &[], &["--session"]) },
    Command {
        path: true,
        ..cmd("expert", &[("<repo> --at <commit> --q <text>", "at most 3 cited claims from an expert repo")], &[], &["--at", "--version", "--q"])
    },
    cmd("vectors", &[("--query [--store LESSONS]", "the vector shortlist for a context on stdin")], &[], &["--query", "--store"]),
    cmd("ope", &[("[--dir DIR | --file FILE] [--json]", "off-policy estimates of the belief's hand-set weights")], &[], &["--dir", "--file", "--json"]),
];

/// What hooks and drones run, never listed: dispatched, not shown.
pub const INTERNAL: &[&str] = &["hook", "check"];

/// Every command of the main help, in its order.
pub fn commands() -> impl Iterator<Item = &'static Command> {
    GROUPS.iter().flat_map(|(_, cmds)| cmds.iter())
}

/// Whether `name` is a top-level borg command (dispatched by main.rs): listed, `debug`, `help` or internal.
pub fn known(name: &str) -> bool {
    commands().any(|c| c.name == name) || INTERNAL.contains(&name) || matches!(name, "debug" | "help" | "-h" | "--help")
}

/// The completions' commands: the main help's, then `debug` with the debug commands as subcommands and
/// all their flags.
fn completed() -> Vec<(&'static str, Vec<&'static str>, Vec<&'static str>, bool)> {
    let mut out: Vec<_> = commands().map(|c| (c.name, subs_of(c.subs), c.flags.to_vec(), c.path)).collect();
    let mut flags: Vec<&str> = DEBUG.iter().flat_map(|c| c.flags.iter().copied()).collect();
    flags.sort_unstable();
    flags.dedup();
    out.push(("debug", DEBUG.iter().map(|c| c.name).chain(["help"]).collect(), flags, true));
    out
}

/// The debug commands that take subcommands of their own: (name, its subcommands and flags). Completed
/// by the third word, so such a command offers its own subcommands rather than the debug commands again.
fn nested() -> Vec<(&'static str, String)> {
    DEBUG.iter().filter(|c| !c.subs.is_empty()).map(|c| (c.name, [subs_of(c.subs).as_slice(), c.flags].concat().join(" "))).collect()
}

const USAGE: &str = "usage: borg completions <bash|zsh|fish>
  bash: borg completions bash > ~/.local/share/bash-completion/completions/borg
  zsh:  borg completions zsh > \"${fpath[1]}/_borg\"   (then restart the shell)
  fish: borg completions fish > ~/.config/fish/completions/borg.fish";

pub fn main(args: &[&str]) -> i32 {
    match args.first().copied() {
        Some("bash") => print!("{}", bash()),
        Some("zsh") => print!("{}", zsh()),
        Some("fish") => print!("{}", fish()),
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    }
    0
}

/// A command's subcommands, `HARNESSES` spelled out.
fn subs_of(subs: &[&'static str]) -> Vec<&'static str> {
    subs.iter().flat_map(|s| if *s == HARNESSES { crate::harness::visible().map(|h| h.name()).collect() } else { vec![*s] }).collect()
}

/// The shell's `case` lines: `line(command, its subcommands and flags, takes a path)` for each command.
fn cases(line: impl Fn(&str, &str, bool) -> String) -> String {
    completed().iter().map(|(cmd, subs, flags, path)| line(cmd, &[subs.as_slice(), flags].concat().join(" "), *path)).collect()
}

fn names() -> String {
    completed().iter().map(|c| c.0).collect::<Vec<_>>().join(" ")
}

pub fn bash() -> String {
    let cases = cases(|cmd, words, path| format!("        {cmd}) words=\"{words}\"; files=\"{}\" ;;\n", if path { " -f" } else { "" }));
    let nested: String =
        nested().iter().map(|(cmd, words)| format!("            {cmd}) COMPREPLY=($(compgen -W \"{words}\" -- \"$cur\")); return ;;\n")).collect();
    format!(
        r#"# bash completion for borg (borg completions bash)
_borg() {{
    local cur="${{COMP_WORDS[COMP_CWORD]}}" words="" files=""
    if [ "$COMP_CWORD" -eq 1 ]; then
        COMPREPLY=($(compgen -W "{names}" -- "$cur"))
        return
    fi
    if [ "$COMP_CWORD" -gt 2 ] && [ "${{COMP_WORDS[1]}}" = debug ]; then
        case "${{COMP_WORDS[2]}}" in
{nested}        esac
    fi
    case "${{COMP_WORDS[1]}}" in
{cases}    esac
    COMPREPLY=($(compgen -W "$words" -- "$cur"))
    if [ -n "$files" ] && [ "${{cur:0:1}}" != "-" ]; then
        COMPREPLY+=($(compgen -f -- "$cur"))
    fi
}}
complete -o default -F _borg borg
"#,
        names = names()
    )
}

pub fn zsh() -> String {
    let cases = cases(|cmd, words, path| format!("        {cmd}) compadd -- {words}{} ;;\n", if path { "; _files" } else { "" }));
    let nested: String = nested().iter().map(|(cmd, words)| format!("            {cmd}) compadd -- {words}; return ;;\n")).collect();
    format!(
        r#"#compdef borg
# zsh completion for borg (borg completions zsh)
_borg() {{
    if (( CURRENT == 2 )); then
        compadd -- {names}
        return
    fi
    if (( CURRENT > 3 )) && [[ ${{words[2]}} == debug ]]; then
        case "${{words[3]}}" in
{nested}        esac
    fi
    case "${{words[2]}}" in
{cases}    esac
}}
_borg "$@"
"#,
        names = names()
    )
}

pub fn fish() -> String {
    let mut out =
        format!("# fish completion for borg (borg completions fish)\ncomplete -c borg -f\ncomplete -c borg -n __fish_use_subcommand -a '{}'\n", names());
    // fish sees every word: `borg debug vectors` has also seen `vectors`, so a command a debug one shares a
    // name with completes only outside `debug`, and a nested debug command's subcommands only inside it.
    for (cmd, subs, flags, path) in completed() {
        let when = if DEBUG.iter().any(|d| d.name == cmd) {
            format!("__fish_seen_subcommand_from {cmd}; and not __fish_seen_subcommand_from debug")
        } else {
            format!("__fish_seen_subcommand_from {cmd}")
        };
        if !subs.is_empty() {
            out.push_str(&format!("complete -c borg -n '{when}' -a '{}'\n", subs.join(" ")));
        }
        for f in flags {
            out.push_str(&format!("complete -c borg -n '{when}' -l {}\n", f.trim_start_matches("--")));
        }
        if path {
            out.push_str(&format!("complete -c borg -n '{when}' -F\n"));
        }
    }
    for c in DEBUG.iter().filter(|c| !c.subs.is_empty()) {
        let subs = subs_of(c.subs).join(" ");
        out.push_str(&format!("complete -c borg -n '__fish_seen_subcommand_from debug; and __fish_seen_subcommand_from {}' -a '{subs}'\n", c.name));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shell_names_every_command() {
        for script in [bash(), zsh(), fish()] {
            for (cmd, ..) in completed() {
                assert!(script.contains(cmd), "{cmd} missing");
            }
        }
        assert!(bash().contains("queen) words=\"show ask answer"));
        assert!(zsh().starts_with("#compdef borg"));
        assert!(fish().contains("-n '__fish_seen_subcommand_from install' -l apply"));
        assert!(bash().contains("install) words=\"claude omp opencode codex all --apply"), "{}", bash());
        assert!(!bash().contains(" sim ") && !bash().contains(HARNESSES), "the simulated harness stays hidden");
    }

    #[test]
    fn a_command_shared_with_debug_completes_outside_debug_only() {
        let fish = fish();
        assert!(fish.contains("-n '__fish_seen_subcommand_from vectors; and not __fish_seen_subcommand_from debug' -l fetch"), "{fish}");
        assert!(fish.contains("-n '__fish_seen_subcommand_from debug' -a 'recall"), "{fish}");
    }
}
