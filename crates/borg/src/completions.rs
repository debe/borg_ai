//! `borg completions <bash|zsh|fish>`: a shell completion script for borg, printed to stdout.
//!
//! One table (`COMMANDS`) of commands, their subcommands and flags feeds all three shells, so a new
//! command needs one line here. Paths complete as files where a command takes one (`why`, `memory`).

/// (command, subcommands, flags, takes a path)
const COMMANDS: &[(&str, &[&str], &[&str], bool)] = &[
    ("status", &[], &[], false),
    ("doctor", &[], &[], false),
    ("collective", &[], &[], true),
    ("install", &["claude", "omp", "opencode", "all"], &["--apply", "--home"], false),
    ("uninstall", &["claude", "omp", "opencode", "all"], &["--apply", "--home"], false),
    ("learn", &[], &["--session"], false),
    (
        "assimilate",
        &[],
        &[
            "--apply",
            "--since",
            "--limit",
            "--claude",
            "--omp",
            "--from-dir",
            "--plan",
            "--for",
            "--collective",
            "--knowledge",
            "--retros",
            "--families",
            "--decisions",
        ],
        false,
    ),
    ("reach", &[], &["--plan", "--apply", "--jobs"], false),
    ("queen", &["show", "ask", "answer", "skip", "dismiss", "next", "dedupe", "notify"], &["--json", "--repo", "--dir", "--collective", "--apply"], false),
    ("top", &[], &["--watch", "--json", "--dir"], false),
    ("steer", &["all"], &["--pause", "--resume"], false),
    ("why", &[], &["--full", "--json"], true),
    ("web", &[], &["--port", "--dir"], false),
    ("trust", &[], &["--dir"], false),
    ("notes", &["attach", "fetch"], &[], false),
    (
        "mesh",
        &["id", "init", "invite", "invites", "join", "revoke", "members", "peer", "serve", "sync", "fetch"],
        &["--collective", "--apply", "--role", "--expires", "--uses", "--png", "--revoke"],
        false,
    ),
    ("vectors", &[], &["--fetch", "--rebuild", "--query"], false),
    ("memory", &[], &["--session"], true),
    ("facets", &[], &["--apply", "--store", "--jobs"], false),
    ("expert", &[], &["--at", "--version", "--q"], true),
    ("recall", &[], &[], true),
    ("completions", &["bash", "zsh", "fish"], &[], false),
];

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

fn names() -> String {
    COMMANDS.iter().map(|c| c.0).collect::<Vec<_>>().join(" ")
}

pub fn bash() -> String {
    let mut cases = String::new();
    for (cmd, subs, flags, path) in COMMANDS {
        let words = subs.iter().chain(flags.iter()).copied().collect::<Vec<_>>().join(" ");
        let files = if *path { " -f" } else { "" };
        cases.push_str(&format!("        {cmd}) words=\"{words}\"; files=\"{files}\" ;;\n"));
    }
    format!(
        r#"# bash completion for borg (borg completions bash)
_borg() {{
    local cur="${{COMP_WORDS[COMP_CWORD]}}" words="" files=""
    if [ "$COMP_CWORD" -eq 1 ]; then
        COMPREPLY=($(compgen -W "{names}" -- "$cur"))
        return
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
    let mut cases = String::new();
    for (cmd, subs, flags, path) in COMMANDS {
        let words = subs.iter().chain(flags.iter()).copied().collect::<Vec<_>>().join(" ");
        let files = if *path { "; _files" } else { "" };
        cases.push_str(&format!("        {cmd}) compadd -- {words}{files} ;;\n"));
    }
    format!(
        r#"#compdef borg
# zsh completion for borg (borg completions zsh)
_borg() {{
    if (( CURRENT == 2 )); then
        compadd -- {names}
        return
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
    let mut out = String::from("# fish completion for borg (borg completions fish)\ncomplete -c borg -f\n");
    out.push_str(&format!("complete -c borg -n __fish_use_subcommand -a '{}'\n", names()));
    for (cmd, subs, flags, path) in COMMANDS {
        let when = format!("__fish_seen_subcommand_from {cmd}");
        if !subs.is_empty() {
            out.push_str(&format!("complete -c borg -n '{when}' -a '{}'\n", subs.join(" ")));
        }
        for f in *flags {
            out.push_str(&format!("complete -c borg -n '{when}' -l {}\n", f.trim_start_matches("--")));
        }
        if *path {
            out.push_str(&format!("complete -c borg -n '{when}' -F\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shell_names_every_command() {
        for script in [bash(), zsh(), fish()] {
            for (cmd, ..) in COMMANDS {
                assert!(script.contains(cmd), "{cmd} missing");
            }
        }
        assert!(bash().contains("queen) words=\"show ask answer"));
        assert!(zsh().starts_with("#compdef borg"));
        assert!(fish().contains("-n '__fish_seen_subcommand_from install' -l apply"));
    }
}
