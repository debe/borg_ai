//! `borg setup`'s questions: one line of stdin each, the default in brackets and taken on an empty line
//! or at end of input. Under `--yes` nothing is read: each question prints with its default as the answer.
//! Headings are bold only on a terminal and never with `NO_COLOR`. No prompt crate: four questions'
//! worth of line reading.

use std::io::{BufRead, IsTerminal, Write};

pub struct Prompt<'a> {
    yes: bool,
    color: bool,
    input: &'a mut dyn BufRead,
}

impl<'a> Prompt<'a> {
    /// `yes`: answer every question with its default.
    pub fn new(yes: bool, input: &'a mut dyn BufRead) -> Self {
        let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty());
        Prompt { yes, color, input }
    }

    #[must_use]
    pub fn yes(&self) -> bool {
        self.yes
    }

    /// A step's title.
    pub fn heading(&self, t: &str) {
        if self.color {
            println!("\n\x1b[1m{t}\x1b[0m");
        } else {
            println!("\n{t}");
        }
    }

    /// The answer to `q`, trimmed; `default` for an empty line, end of input or `--yes`.
    pub fn line(&mut self, q: &str, default: &str) -> String {
        if self.yes {
            println!("{q}: {default}");
            return default.to_string();
        }
        print!("{q} [{default}]: ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        match self.input.read_line(&mut answer) {
            Ok(n) if n > 0 && !answer.trim().is_empty() => answer.trim().to_string(),
            _ => default.to_string(),
        }
    }

    /// Yes or no; anything but y, yes, n or no is the default.
    pub fn yes_no(&mut self, q: &str, default: bool) -> bool {
        if self.yes {
            println!("{q}: {}", if default { "yes" } else { "no" });
            return default;
        }
        match self.line(q, if default { "Y/n" } else { "y/N" }).to_lowercase().as_str() {
            "y" | "yes" => true,
            "n" | "no" => false,
            _ => default,
        }
    }

    /// One of `options` by number (from 1); the default, `default`, for anything else.
    pub fn choose(&mut self, q: &str, options: &[&str], default: usize) -> usize {
        if self.yes {
            println!("{q}: {}", options.get(default).copied().unwrap_or_default());
            return default;
        }
        for (i, o) in options.iter().enumerate() {
            println!("  {}) {o}", i + 1);
        }
        let a = self.line(q, &(default + 1).to_string());
        a.parse::<usize>().ok().filter(|n| (1..=options.len()).contains(n)).map_or(default, |n| n - 1)
    }

    /// A list, words separated by spaces or commas; `-` for none.
    pub fn list(&mut self, q: &str, default: &[String]) -> Vec<String> {
        let shown = if default.is_empty() { "-".to_string() } else { default.join(" ") };
        split(&self.line(q, &shown))
    }
}

/// `a b,c` as `[a, b, c]`; `-` alone as none.
pub fn split(text: &str) -> Vec<String> {
    text.split([' ', ',']).filter(|w| !w.is_empty() && *w != "-").map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_and_defaults() {
        let mut input = std::io::Cursor::new("\nn\n2\n~/a, ~/b\n9\n");
        let mut p = Prompt::new(false, &mut input);
        assert!(p.yes_no("go?", true), "an empty line takes the default");
        assert!(!p.yes_no("go?", true));
        assert_eq!(p.choose("which?", &["one", "two"], 0), 1);
        assert_eq!(p.list("dirs?", &[]), vec!["~/a", "~/b"]);
        assert_eq!(p.choose("which?", &["one", "two"], 0), 0, "out of range: the default");
        assert_eq!(p.line("name?", "team"), "team", "end of input: the default");
    }

    #[test]
    fn yes_reads_nothing() {
        let mut input = std::io::Cursor::new("n\n");
        let mut p = Prompt::new(true, &mut input);
        assert!(p.yes_no("go?", true));
        assert!(!p.yes_no("import?", false), "the default, not the y of y/N");
        assert_eq!(p.list("dirs?", &["~/src".into()]), vec!["~/src"]);
        assert!(p.list("off?", &[]).is_empty());
    }
}
