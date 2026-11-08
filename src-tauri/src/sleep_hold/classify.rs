//! Alarms design 10: does a shell command wait in the foreground? A small
//! shell lexer and parser find the words in command position; only those
//! count, so `echo "sleep 5"` or a heredoc body never holds.
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitKind {
    Sleep,
    WaitBuiltin,
    PollLoop,
    GhWatch,
    Watch,
    Follow,
    Timeout,
    ClusterWait,
    Interpreter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Wait {
    pub kind: WaitKind,
    /// How long it waits, when the command says so literally. `None` is
    /// unbounded or unknown, and never counts as short.
    pub seconds: Option<u32>,
}

/// `bash -c`, `eval` and `$(...)` nest; past this depth the rest is ignored.
const MAX_DEPTH: u8 = 8;

pub fn classify(command: &str, background: bool) -> Option<Wait> {
    if background {
        return None;
    }
    text_wait(command, 0)
}

fn text_wait(text: &str, depth: u8) -> Option<Wait> {
    if depth > MAX_DEPTH {
        return None;
    }
    let jobs = Parser::new(Lexer::new(text).tokens()).program();
    jobs_wait(&jobs, depth)
}

const fn wait(kind: WaitKind, seconds: Option<u32>) -> Wait {
    Wait { kind, seconds }
}

/// Two waits run one after the other: the first names the kind, and the
/// time is the sum when both are known.
fn then(first: Option<Wait>, next: Option<Wait>) -> Option<Wait> {
    match (first, next) {
        (Some(first), Some(next)) => Some(Wait {
            kind: first.kind,
            seconds: first
                .seconds
                .zip(next.seconds)
                .map(|(a, b)| a.saturating_add(b)),
        }),
        (first, next) => first.or(next),
    }
}

// ---- lexer ----

#[derive(Debug, Clone, Default)]
struct Word {
    /// The word with quotes removed; a substitution stays as `$(…)`.
    text: String,
    /// Any quote or escape, so `"done"` is not the reserved word.
    quoted: bool,
    /// Commands inside `$(...)`, backticks and `<(...)`, which run too.
    subs: Vec<String>,
}

#[derive(Debug, Clone)]
enum Tok {
    Word(Word),
    Heredoc(String),
    Op(Op),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// `;`, newline, `;;`.
    Sep,
    /// `&&`, `||`, `|`, `|&`: the next command belongs to the same job.
    Join,
    /// A trailing `&`: the job runs in the background.
    Amp,
    Open,
    Close,
}

struct Lexer {
    chars: Vec<char>,
    at: usize,
    toks: Vec<Tok>,
    /// Heredocs whose body starts after the next newline: token index,
    /// delimiter, and whether leading tabs are stripped (`<<-`).
    pending: Vec<(usize, String, bool)>,
}

impl Lexer {
    fn new(text: &str) -> Self {
        Self {
            chars: text.chars().collect(),
            at: 0,
            toks: Vec::new(),
            pending: Vec::new(),
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += 1;
        Some(c)
    }

    fn tokens(mut self) -> Vec<Tok> {
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' | '\r' => self.at += 1,
                '\\' if self.peek_at(1) == Some('\n') => self.at += 2,
                '\n' => {
                    self.at += 1;
                    self.toks.push(Tok::Op(Op::Sep));
                    self.read_heredocs();
                }
                '#' => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.at += 1;
                    }
                }
                ';' | '&' | '|' | '(' | ')' | '<' | '>' => self.operator(c),
                _ => self.word(),
            }
        }
        self.toks
    }

    fn push(&mut self, op: Op, width: usize) {
        self.at += width;
        self.toks.push(Tok::Op(op));
    }

    fn operator(&mut self, c: char) {
        let next = self.peek_at(1);
        match (c, next) {
            ('<' | '>', Some('(')) => {
                self.at += 2;
                let inner = self.balanced();
                self.toks.push(Tok::Word(Word {
                    text: format!("{c}(…)"),
                    quoted: false,
                    subs: vec![inner],
                }));
            }
            ('<', Some('<')) => self.heredoc(),
            ('<' | '>', _) | ('&', Some('>')) => self.redirect(),
            ('&', Some('&')) | ('|', Some('|' | '&')) => self.push(Op::Join, 2),
            ('|', _) => self.push(Op::Join, 1),
            ('&', _) => self.push(Op::Amp, 1),
            (';', Some(';' | '&')) => self.push(Op::Sep, 2),
            (';', _) => self.push(Op::Sep, 1),
            ('(', _) => self.push(Op::Open, 1),
            _ => self.push(Op::Close, 1),
        }
    }

    /// A redirection and its target are not words of the command.
    fn redirect(&mut self) {
        while matches!(self.peek(), Some('<' | '>' | '&' | '|')) {
            self.at += 1;
        }
        self.target();
    }

    /// The word after a redirection operator, quotes removed.
    fn target(&mut self) -> String {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.at += 1;
        }
        match self.peek() {
            None | Some('\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>') => String::new(),
            Some(_) => self.read_word().text,
        }
    }

    fn heredoc(&mut self) {
        self.at += 2;
        if self.peek() == Some('<') {
            self.at += 1;
            self.target();
            return;
        }
        let strip_tabs = self.peek() == Some('-');
        if strip_tabs {
            self.at += 1;
        }
        let delimiter = self.target();
        self.pending.push((self.toks.len(), delimiter, strip_tabs));
        self.toks.push(Tok::Heredoc(String::new()));
    }

    fn read_heredocs(&mut self) {
        for (index, delimiter, strip_tabs) in std::mem::take(&mut self.pending) {
            let mut body = String::new();
            while self.at < self.chars.len() {
                let end = self.chars[self.at..]
                    .iter()
                    .position(|&c| c == '\n')
                    .map_or(self.chars.len(), |offset| self.at + offset);
                let line: String = self.chars[self.at..end].iter().collect();
                self.at = (end + 1).min(self.chars.len());
                let line = if strip_tabs {
                    line.trim_start_matches('\t').to_string()
                } else {
                    line
                };
                if line == delimiter {
                    break;
                }
                body.push_str(&line);
                body.push('\n');
            }
            self.toks[index] = Tok::Heredoc(body);
        }
    }

    fn word(&mut self) {
        let word = self.read_word();
        // `2>&1`: the digits name a file descriptor, not an argument.
        let fd = !word.quoted
            && !word.text.is_empty()
            && word.text.chars().all(|c| c.is_ascii_digit())
            && matches!(self.peek(), Some('<' | '>'));
        if !fd {
            self.toks.push(Tok::Word(word));
        }
    }

    fn read_word(&mut self) -> Word {
        let mut word = Word::default();
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' | '\r' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>' => break,
                '\\' => {
                    self.at += 1;
                    word.quoted = true;
                    if let Some(next) = self.bump().filter(|&next| next != '\n') {
                        word.text.push(next);
                    }
                }
                '\'' => {
                    self.at += 1;
                    word.quoted = true;
                    self.single_quoted(&mut word.text);
                }
                '"' => {
                    self.at += 1;
                    word.quoted = true;
                    self.double_quoted(&mut word);
                }
                '$' => self.dollar(&mut word),
                '`' => self.backtick(&mut word),
                _ => {
                    self.at += 1;
                    word.text.push(c);
                }
            }
        }
        word
    }

    fn single_quoted(&mut self, text: &mut String) {
        while let Some(c) = self.bump() {
            if c == '\'' {
                return;
            }
            text.push(c);
        }
    }

    fn double_quoted(&mut self, word: &mut Word) {
        while let Some(c) = self.peek() {
            match c {
                '"' => {
                    self.at += 1;
                    return;
                }
                '\\' => {
                    self.at += 1;
                    match self.bump() {
                        Some('\n') | None => {}
                        Some(next @ ('"' | '\\' | '$' | '`')) => word.text.push(next),
                        Some(next) => {
                            word.text.push('\\');
                            word.text.push(next);
                        }
                    }
                }
                '$' => self.dollar(word),
                '`' => self.backtick(word),
                _ => {
                    self.at += 1;
                    word.text.push(c);
                }
            }
        }
    }

    fn dollar(&mut self, word: &mut Word) {
        match self.peek_at(1) {
            Some('(') => {
                self.at += 2;
                let inner = self.balanced();
                // `$((1 + 2))` is arithmetic, not a command.
                if !(inner.starts_with('(') && inner.ends_with(')')) {
                    word.subs.push(inner);
                }
                word.text.push_str("$(…)");
            }
            Some('\'') => {
                self.at += 2;
                word.quoted = true;
                self.single_quoted(&mut word.text);
            }
            _ => {
                self.at += 1;
                word.text.push('$');
            }
        }
    }

    fn backtick(&mut self, word: &mut Word) {
        self.at += 1;
        let mut inner = String::new();
        while let Some(c) = self.bump() {
            match c {
                '`' => break,
                '\\' => inner.extend(self.bump()),
                _ => inner.push(c),
            }
        }
        word.subs.push(inner);
        word.text.push_str("`…`");
    }

    /// The text up to the `)` that closes an already-read `(`.
    fn balanced(&mut self) -> String {
        let start = self.at;
        let mut depth = 1;
        while let Some(c) = self.bump() {
            match c {
                '\\' => {
                    self.bump();
                }
                '\'' => self.single_quoted(&mut String::new()),
                '"' => self.double_quoted(&mut Word::default()),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return self.chars[start..self.at - 1].iter().collect();
                    }
                }
                _ => {}
            }
        }
        self.chars[start..].iter().collect()
    }
}

// ---- parser ----

/// Commands joined by `&&`, `||` or `|`. A trailing `&` puts all of them in
/// the background, as bash does.
#[derive(Debug)]
struct Job {
    cmds: Vec<Cmd>,
    background: bool,
}

#[derive(Debug)]
enum Cmd {
    Simple {
        words: Vec<Word>,
        heredocs: Vec<String>,
    },
    /// A subshell, a `{ }` block, or every branch of an `if`.
    Group(Vec<Job>),
    /// A `while`/`until`/`for` loop: condition and body, and the `for`
    /// iteration count when its list is literal.
    Loop {
        body: Vec<Job>,
        iterations: Option<u32>,
    },
    /// A function definition, `case`, or a stray token: never runs a wait
    /// this classifier can see.
    Skip,
}

struct Parser {
    toks: Vec<Tok>,
    at: usize,
}

impl Parser {
    fn new(toks: Vec<Tok>) -> Self {
        Self { toks, at: 0 }
    }

    fn program(mut self) -> Vec<Job> {
        let mut jobs = Vec::new();
        while self.at < self.toks.len() {
            jobs.extend(self.list(&[]));
            // A stray `)` ended the list; step over it.
            self.at += 1;
        }
        jobs
    }

    fn reserved(&self, at: usize) -> Option<&str> {
        match self.toks.get(at) {
            Some(Tok::Word(word)) if !word.quoted => Some(&word.text),
            _ => None,
        }
    }

    fn op(&self) -> Option<Op> {
        match self.toks.get(self.at) {
            Some(Tok::Op(op)) => Some(*op),
            _ => None,
        }
    }

    fn skip_separators(&mut self) {
        while self.op() == Some(Op::Sep) {
            self.at += 1;
        }
    }

    fn ends(&self, stops: &[&str]) -> bool {
        self.at >= self.toks.len()
            || self.op() == Some(Op::Close)
            || self
                .reserved(self.at)
                .is_some_and(|word| stops.contains(&word))
    }

    fn eat(&mut self, word: &str) {
        if self.reserved(self.at) == Some(word) {
            self.at += 1;
        }
    }

    fn list(&mut self, stops: &[&str]) -> Vec<Job> {
        let mut jobs = Vec::new();
        loop {
            self.skip_separators();
            if self.ends(stops) {
                return jobs;
            }
            jobs.push(self.job(stops));
        }
    }

    fn job(&mut self, stops: &[&str]) -> Job {
        let mut cmds = vec![self.command()];
        while self.op() == Some(Op::Join) {
            self.at += 1;
            self.skip_separators();
            if self.ends(stops) {
                break;
            }
            cmds.push(self.command());
        }
        let background = self.op() == Some(Op::Amp);
        if background {
            self.at += 1;
        }
        Job { cmds, background }
    }

    fn command(&mut self) -> Cmd {
        match self.toks.get(self.at) {
            None => Cmd::Skip,
            Some(Tok::Op(Op::Open)) => {
                self.at += 1;
                let jobs = self.list(&[]);
                if self.op() == Some(Op::Close) {
                    self.at += 1;
                }
                Cmd::Group(jobs)
            }
            Some(Tok::Op(_)) => {
                self.at += 1;
                Cmd::Skip
            }
            Some(_) => self.compound(),
        }
    }

    fn compound(&mut self) -> Cmd {
        let Some(word) = self.reserved(self.at) else {
            return self.simple();
        };
        match word {
            "{" => {
                self.at += 1;
                let jobs = self.list(&["}"]);
                self.eat("}");
                Cmd::Group(jobs)
            }
            "while" | "until" => self.while_loop(),
            "for" | "select" => self.for_loop(),
            "if" => self.if_block(),
            "case" => self.skip_through("esac"),
            "function" => {
                self.at += 2;
                self.function_body()
            }
            "!" => {
                self.at += 1;
                self.command()
            }
            "then" | "elif" | "else" | "fi" | "do" | "done" | "esac" | "}" => {
                self.at += 1;
                Cmd::Skip
            }
            _ => self.simple(),
        }
    }

    fn while_loop(&mut self) -> Cmd {
        self.at += 1;
        let mut body = self.list(&["do"]);
        self.eat("do");
        body.extend(self.list(&["done"]));
        self.eat("done");
        Cmd::Loop {
            body,
            iterations: None,
        }
    }

    fn for_loop(&mut self) -> Cmd {
        self.at += 2;
        let mut items = Vec::new();
        let listed = self.reserved(self.at) == Some("in");
        if listed {
            self.at += 1;
            while let Some(Tok::Word(word)) = self.toks.get(self.at) {
                if !word.quoted && word.text == "do" {
                    break;
                }
                items.push(word.clone());
                self.at += 1;
            }
        }
        while self.at < self.toks.len() && self.reserved(self.at) != Some("do") {
            self.at += 1;
        }
        self.eat("do");
        let body = self.list(&["done"]);
        self.eat("done");
        Cmd::Loop {
            body,
            iterations: if listed { iterations(&items) } else { None },
        }
    }

    fn if_block(&mut self) -> Cmd {
        self.at += 1;
        let mut jobs = Vec::new();
        loop {
            jobs.extend(self.list(&["then", "elif", "else", "fi"]));
            match self.reserved(self.at) {
                Some("fi") => {
                    self.at += 1;
                    return Cmd::Group(jobs);
                }
                Some("then" | "elif" | "else") => self.at += 1,
                _ => return Cmd::Group(jobs),
            }
        }
    }

    fn skip_through(&mut self, end: &str) -> Cmd {
        while self.at < self.toks.len() {
            self.at += 1;
            if self.reserved(self.at - 1) == Some(end) {
                break;
            }
        }
        Cmd::Skip
    }

    /// A function's body only defines it; nothing runs.
    fn function_body(&mut self) -> Cmd {
        if self.op() == Some(Op::Open) {
            self.at += 2;
        }
        self.skip_separators();
        self.command();
        Cmd::Skip
    }

    fn simple(&mut self) -> Cmd {
        let defines = matches!(
            (self.toks.get(self.at + 1), self.toks.get(self.at + 2)),
            (Some(Tok::Op(Op::Open)), Some(Tok::Op(Op::Close)))
        );
        if defines {
            self.at += 1;
            return self.function_body();
        }
        let mut words = Vec::new();
        let mut heredocs = Vec::new();
        while let Some(tok) = self.toks.get(self.at) {
            match tok {
                Tok::Word(word) => words.push(word.clone()),
                Tok::Heredoc(body) => heredocs.push(body.clone()),
                Tok::Op(_) => break,
            }
            self.at += 1;
        }
        Cmd::Simple { words, heredocs }
    }
}

/// How many times a `for` list runs, when every item is literal.
fn iterations(items: &[Word]) -> Option<u32> {
    items.iter().try_fold(0u32, |sum, item| {
        Some(sum.saturating_add(item_count(item)?))
    })
}

fn item_count(item: &Word) -> Option<u32> {
    if let [sub] = item.subs.as_slice() {
        return (item.text == "$(…)").then(|| seq_count(sub)).flatten();
    }
    if !item.subs.is_empty() || item.text.contains(['$', '*', '?', '[']) {
        return None;
    }
    if !item.quoted && item.text.starts_with('{') && item.text.ends_with('}') {
        let (low, high) = item.text[1..item.text.len() - 1].split_once("..")?;
        let (low, high): (i64, i64) = (low.parse().ok()?, high.parse().ok()?);
        return u32::try_from(low.abs_diff(high).saturating_add(1)).ok();
    }
    Some(1)
}

fn seq_count(sub: &str) -> Option<u32> {
    let words: Vec<&str> = sub.split_whitespace().collect();
    let ["seq", numbers @ ..] = words.as_slice() else {
        return None;
    };
    let numbers: Vec<i64> = numbers
        .iter()
        .map(|n| n.parse().ok())
        .collect::<Option<_>>()?;
    let count = match numbers.as_slice() {
        [last] => *last,
        [first, last] => last.checked_sub(*first)?.checked_add(1)?,
        [first, step, last] => last
            .checked_sub(*first)?
            .checked_div(*step)?
            .checked_add(1)?,
        _ => return None,
    };
    u32::try_from(count.max(0)).ok()
}

// ---- classifier ----

fn jobs_wait(jobs: &[Job], depth: u8) -> Option<Wait> {
    jobs.iter()
        .filter(|job| !job.background)
        .flat_map(|job| &job.cmds)
        .map(|cmd| cmd_wait(cmd, depth))
        .fold(None, then)
}

fn cmd_wait(cmd: &Cmd, depth: u8) -> Option<Wait> {
    match cmd {
        Cmd::Simple { words, heredocs } => {
            let text: Vec<&str> = words.iter().map(|word| word.text.as_str()).collect();
            let stdin = heredocs.last().map(String::as_str);
            words
                .iter()
                .flat_map(|word| &word.subs)
                .map(|sub| text_wait(sub, depth + 1))
                .fold(words_wait(&text, stdin, depth), then)
        }
        Cmd::Group(jobs) => jobs_wait(jobs, depth),
        Cmd::Loop { body, iterations } => jobs_wait(body, depth).map(|inner| {
            let seconds = iterations
                .zip(inner.seconds)
                .map(|(n, each)| n.saturating_mul(each));
            wait(WaitKind::PollLoop, seconds)
        }),
        Cmd::Skip => None,
    }
}

/// One simple command's words, quotes removed. `stdin` is its heredoc.
fn words_wait(words: &[&str], stdin: Option<&str>, depth: u8) -> Option<Wait> {
    let words = after_prefixes(words)?;
    let (&head, args) = words.split_first()?;
    let name = head.rsplit('/').next().unwrap_or(head);
    match name {
        "sleep" | "gsleep" => sleep_wait(args),
        "wait" => Some(wait(WaitKind::WaitBuiltin, None)),
        "gh" => gh_wait(args),
        "watch" => Some(wait(WaitKind::Watch, None)),
        "tail" => (short_flag(args, 'f') || short_flag(args, 'F') || long_flag(args, "--follow"))
            .then_some(wait(WaitKind::Follow, None)),
        "fswatch" => (!short_flag(args, '1') && !long_flag(args, "--one-event"))
            .then_some(wait(WaitKind::Watch, None)),
        "inotifywait" => (short_flag(args, 'm') || long_flag(args, "--monitor"))
            .then_some(wait(WaitKind::Watch, None)),
        "timeout" | "gtimeout" => timeout_wait(args, stdin, depth),
        "kubectl" => kubectl_wait(args),
        "docker" | "podman" => docker_wait(args),
        "wait-on" | "wait-for-it" | "wait-for-it.sh" | "wait-for" => {
            Some(wait(WaitKind::ClusterWait, None))
        }
        "npx" | "bunx" | "pnpx" => words_wait(skip_flags(args, &["-p", "--package"]), stdin, depth),
        "pnpm" | "yarn" => match args {
            ["dlx" | "exec", rest @ ..] => words_wait(skip_flags(rest, &[]), stdin, depth),
            _ => None,
        },
        "bash" | "sh" | "zsh" | "dash" | "ksh" => shell_wait(args, stdin, depth),
        "eval" => text_wait(&args.join(" "), depth + 1),
        _ => interpreter_wait(name, args, stdin),
    }
}

/// The command after assignments and wrappers that run their argument in
/// the foreground (`env`, `time`, `nohup`, `sudo` ...). `None` when a
/// wrapper detaches it (`setsid`) or only looks it up (`command -v`).
fn after_prefixes<'a>(mut words: &'a [&'a str]) -> Option<&'a [&'a str]> {
    loop {
        let (&head, rest) = words.split_first()?;
        if is_assignment(head) {
            words = rest;
            continue;
        }
        words = match head.rsplit('/').next().unwrap_or(head) {
            "setsid" | "disown" => return None,
            "command" if matches!(rest.first(), Some(&("-v" | "-V"))) => return None,
            "time" | "nohup" | "builtin" | "command" => skip_flags(rest, &[]),
            "exec" => skip_flags(rest, &["-a"]),
            "nice" => skip_flags(rest, &["-n", "--adjustment"]),
            "env" => skip_flags(rest, &["-u", "--unset", "-C", "--chdir"]),
            "sudo" => skip_flags(
                rest,
                &["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U", "-T"],
            ),
            "caffeinate" => skip_flags(rest, &["-t", "-w"]),
            "stdbuf" => skip_flags(rest, &["-i", "-o", "-e"]),
            _ => return Some(words),
        };
    }
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The words after the leading flags; `valued` flags take the next word.
fn skip_flags<'a>(mut words: &'a [&'a str], valued: &[&str]) -> &'a [&'a str] {
    while let Some((&first, rest)) = words.split_first() {
        if first == "--" {
            return rest;
        }
        if !first.starts_with('-') || first == "-" {
            break;
        }
        words = if valued.contains(&first) {
            rest.get(1..).unwrap_or_default()
        } else {
            rest
        };
    }
    words
}

/// The arguments that are not flags or flag values.
fn positionals<'a>(args: &[&'a str], valued: &[&str]) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut args = args.iter();
    while let Some(&arg) = args.next() {
        if valued.contains(&arg) {
            args.next();
        } else if !arg.starts_with('-') {
            found.push(arg);
        }
    }
    found
}

fn short_flag(args: &[&str], flag: char) -> bool {
    args.iter().any(|arg| {
        arg.len() > 1 && arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains(flag)
    })
}

fn long_flag(args: &[&str], flag: &str) -> bool {
    args.iter().any(|arg| {
        arg.strip_prefix(flag)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('='))
    })
}

fn flag_value<'a>(args: &[&'a str], flag: &str) -> Option<&'a str> {
    args.iter()
        .enumerate()
        .find_map(|(i, arg)| match arg.strip_prefix(flag) {
            Some("") => args.get(i + 1).copied(),
            Some(rest) => rest.strip_prefix('='),
            None => None,
        })
}

/// `5`, `1.5`, `2m`, `1h`, `1d` in whole seconds, rounded up. `None` for
/// `infinity`, `$N`, or anything else not literal.
fn parse_duration(text: &str) -> Option<u32> {
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let value: f64 = number.parse().ok()?;
    let scale = match unit {
        "" | "s" => 1.0,
        "m" => 60.0,
        "h" => 3600.0,
        "d" => 86400.0,
        _ => return None,
    };
    let seconds = (value * scale).ceil();
    // `as` saturates at u32::MAX.
    seconds.is_finite().then_some(seconds as u32)
}

fn sleep_wait(args: &[&str]) -> Option<Wait> {
    if args.is_empty() {
        return None;
    }
    let seconds = args.iter().try_fold(0u32, |sum, arg| {
        Some(sum.saturating_add(parse_duration(arg)?))
    });
    Some(wait(WaitKind::Sleep, seconds))
}

fn gh_wait(args: &[&str]) -> Option<Wait> {
    match args {
        ["pr", "checks", rest @ ..] if long_flag(rest, "--watch") => {}
        ["run", "watch", ..] => {}
        _ => return None,
    }
    Some(wait(WaitKind::GhWatch, None))
}

/// `timeout N <cmd>` holds only when `<cmd>` waits; then it waits at most N.
fn timeout_wait(args: &[&str], stdin: Option<&str>, depth: u8) -> Option<Wait> {
    let rest = skip_flags(args, &["-s", "--signal", "-k", "--kill-after"]);
    let (&limit, command) = rest.split_first()?;
    let inner = words_wait(command, stdin, depth)?;
    let seconds = match (parse_duration(limit).filter(|&n| n > 0), inner.seconds) {
        (Some(limit), Some(inner)) => Some(limit.min(inner)),
        (limit, inner) => limit.or(inner),
    };
    Some(wait(WaitKind::Timeout, seconds))
}

fn kubectl_wait(args: &[&str]) -> Option<Wait> {
    let valued = [
        "-n",
        "--namespace",
        "--context",
        "--kubeconfig",
        "--cluster",
        "--user",
        "-l",
        "--selector",
        "--for",
        "--timeout",
    ];
    let waits = match positionals(args, &valued).as_slice() {
        ["wait", ..] => true,
        ["rollout", "status", ..] => !args
            .iter()
            .any(|arg| matches!(*arg, "--watch=false" | "-w=false")),
        _ => false,
    };
    let seconds = flag_value(args, "--timeout").and_then(parse_duration);
    waits.then_some(wait(WaitKind::ClusterWait, seconds))
}

fn docker_wait(args: &[&str]) -> Option<Wait> {
    let valued = [
        "-H",
        "--host",
        "--context",
        "--config",
        "-c",
        "--log-level",
        "-f",
        "--file",
        "-p",
        "--project-name",
    ];
    matches!(
        positionals(args, &valued).as_slice(),
        ["wait", ..] | ["compose", "wait", ..]
    )
    .then_some(wait(WaitKind::ClusterWait, None))
}

/// `bash -c '...'` runs its string; `bash` with no script runs its heredoc.
fn shell_wait(args: &[&str], stdin: Option<&str>, depth: u8) -> Option<Wait> {
    let mut args = args.iter();
    while let Some(&arg) = args.next() {
        if matches!(arg, "-o" | "+o") {
            args.next();
        } else if arg.starts_with('-') && !arg.starts_with("--") && arg.contains('c') {
            return text_wait(args.next()?, depth + 1);
        } else if !arg.starts_with(['-', '+']) {
            return None;
        }
    }
    text_wait(stdin?, depth + 1)
}

#[derive(Clone, Copy)]
enum Lang {
    Python,
    Js,
    Perl,
    Ruby,
    AppleScript,
}

fn lang(name: &str) -> Option<Lang> {
    match name {
        "node" | "bun" | "deno" => Some(Lang::Js),
        "perl" => Some(Lang::Perl),
        "ruby" => Some(Lang::Ruby),
        "osascript" => Some(Lang::AppleScript),
        _ if name == "python" || name.starts_with("python2") || name.starts_with("python3") => {
            Some(Lang::Python)
        }
        _ => None,
    }
}

/// A one-liner (`python -c`, `node -e`, `perl -e`, `osascript -e`) or a
/// script on stdin (`python3 - <<EOF`) that sleeps.
fn interpreter_wait(name: &str, args: &[&str], stdin: Option<&str>) -> Option<Wait> {
    let lang = lang(name)?;
    let flags: &[&str] = match lang {
        Lang::Python => &["-c"],
        Lang::Js => &["-e", "--eval", "-p", "--print", "eval"],
        Lang::Perl => &["-e", "-E"],
        Lang::Ruby | Lang::AppleScript => &["-e"],
    };
    let code: Vec<&str> = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| {
            flags.contains(arg)
                || (matches!(lang, Lang::Perl | Lang::Ruby) && is_cluster_ending(arg, 'e'))
        })
        .filter_map(|(i, _)| args.get(i + 1).copied())
        .collect();
    let code = if code.is_empty() {
        stdin
            .filter(|_| args.iter().all(|arg| arg.starts_with('-')))?
            .to_string()
    } else {
        code.join("\n")
    };
    code_wait(lang, &code)
}

fn is_cluster_ending(arg: &str, flag: char) -> bool {
    arg.len() > 2 && arg.starts_with('-') && !arg.starts_with("--") && arg.ends_with(flag)
}

fn code_wait(lang: Lang, code: &str) -> Option<Wait> {
    let calls: Vec<&str> = match lang {
        Lang::Python => called(code, "sleep", |next| next == '('),
        Lang::Js => {
            let timers = ["setTimeout", "setInterval"]
                .iter()
                .any(|timer| code.contains(timer));
            let sleeps = !called(code, "sleep", |next| next == '(').is_empty();
            return (timers || sleeps).then_some(wait(WaitKind::Interpreter, None));
        }
        Lang::Perl | Lang::Ruby => called(code, "sleep", |next| {
            next.is_ascii_digit() || matches!(next, '(' | '$' | ';')
        }),
        Lang::AppleScript => called(code, "delay", |next| next.is_ascii_digit() || next == '('),
    };
    if calls.is_empty() {
        return None;
    }
    let looped = ["for ", "while ", "loop", "each", "repeat", "times"]
        .iter()
        .any(|word| code.contains(word));
    let seconds = match calls.as_slice() {
        [args] if !looped => leading_number(args),
        _ => None,
    };
    Some(wait(WaitKind::Interpreter, seconds))
}

/// The text after each standalone `name` whose next non-space character
/// passes `next`. `time.sleep(` counts; `sleepy(` and `"sleep"` do not.
fn called<'a>(code: &'a str, name: &str, next: impl Fn(char) -> bool) -> Vec<&'a str> {
    code.match_indices(name)
        .filter(|(at, _)| {
            !code[..*at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
        .map(|(at, _)| code[at + name.len()..].trim_start())
        .filter(|rest| rest.chars().next().is_none_or(&next))
        .collect()
}

fn leading_number(args: &str) -> Option<u32> {
    let args = args.trim_start_matches(['(', ' ']);
    let end = args
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(args.len());
    parse_duration(&args[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use WaitKind::*;

    type Row = (&'static str, WaitKind, Option<u32>);

    /// Design 10 "Holds", one row per pattern.
    const HOLDS: &[Row] = &[
        ("sleep 30", Sleep, Some(30)),
        ("sleep 1m", Sleep, Some(60)),
        ("sleep 2h", Sleep, Some(7200)),
        ("sleep 1d", Sleep, Some(86400)),
        ("sleep 0.5", Sleep, Some(1)),
        ("sleep 5 10", Sleep, Some(15)),
        ("sleep $DELAY", Sleep, None),
        ("sleep infinity", Sleep, None),
        ("gsleep 3", Sleep, Some(3)),
        ("/bin/sleep 4", Sleep, Some(4)),
        ("cargo build && sleep 30", Sleep, Some(30)),
        ("sleep 5; echo done", Sleep, Some(5)),
        ("false || sleep 7", Sleep, Some(7)),
        ("sleep 3 | cat", Sleep, Some(3)),
        ("sleep 2 && sleep 3", Sleep, Some(5)),
        ("wait", WaitBuiltin, None),
        ("wait $pid", WaitBuiltin, None),
        ("wait -n", WaitBuiltin, None),
        ("cargo build & wait", WaitBuiltin, None),
        (
            "while true; do curl -s localhost:3000 && break; sleep 2; done",
            PollLoop,
            None,
        ),
        ("until gh pr checks 9; do sleep 30; done", PollLoop, None),
        (
            "while read -r l; do sleep 1; done < list.txt",
            PollLoop,
            None,
        ),
        ("for i in 1 2 3; do sleep 2; done", PollLoop, Some(6)),
        (
            "for i in {1..10}; do curl x; sleep 3; done",
            PollLoop,
            Some(30),
        ),
        ("for i in $(seq 1 5); do sleep 4; done", PollLoop, Some(20)),
        (
            "for i in $(seq 9223372036854775807 -9223372036854775808); do sleep 4; done",
            PollLoop,
            None,
        ),
        ("gh pr checks 912 --watch", GhWatch, None),
        ("gh pr checks --watch --fail-fast", GhWatch, None),
        ("gh run watch 123", GhWatch, None),
        ("watch -n 5 kubectl get pods", Watch, None),
        ("tail -f app.log", Follow, None),
        ("tail -F app.log", Follow, None),
        ("tail --follow=name app.log", Follow, None),
        ("tail -n 50 -f app.log | grep -m1 ready", Follow, None),
        ("fswatch src", Watch, None),
        ("inotifywait -m -r src", Watch, None),
        ("timeout 60 sleep 600", Timeout, Some(60)),
        ("timeout 600 sleep 5", Timeout, Some(5)),
        ("gtimeout -s KILL 300 gh run watch 5", Timeout, Some(300)),
        ("timeout 5m tail -f log", Timeout, Some(300)),
        (
            "kubectl wait --for=condition=ready pod/x --timeout=60s",
            ClusterWait,
            Some(60),
        ),
        (
            "kubectl -n prod rollout status deploy/api",
            ClusterWait,
            None,
        ),
        ("docker wait ctr", ClusterWait, None),
        ("wait-on http://localhost:3000", ClusterWait, None),
        ("wait-for-it db:5432 -t 30", ClusterWait, None),
        ("npx wait-on tcp:5432", ClusterWait, None),
        (
            "python3 -c 'import time; time.sleep(5)'",
            Interpreter,
            Some(5),
        ),
        (
            "python -c \"from time import sleep\nfor i in range(9): sleep(1)\"",
            Interpreter,
            None,
        ),
        ("node -e 'setTimeout(() => {}, 30000)'", Interpreter, None),
        ("perl -e 'sleep 10'", Interpreter, Some(10)),
        ("osascript -e 'delay 3'", Interpreter, Some(3)),
    ];

    /// Holds this row adds beyond design 10 (listed in state/P1.md).
    const ADDED_HOLDS: &[Row] = &[
        ("FOO=1 sleep 3", Sleep, Some(3)),
        ("env -i PATH=/bin sleep 3", Sleep, Some(3)),
        ("time sleep 3", Sleep, Some(3)),
        ("nohup sleep 3", Sleep, Some(3)),
        ("exec sleep 3", Sleep, Some(3)),
        ("command sleep 3", Sleep, Some(3)),
        ("nice -n 10 sleep 3", Sleep, Some(3)),
        ("sudo -u me sleep 3", Sleep, Some(3)),
        ("sleep 3 2>/dev/null", Sleep, Some(3)),
        ("bash -c 'sleep 8'", Sleep, Some(8)),
        ("sh -lc \"cd x && sleep 8\"", Sleep, Some(8)),
        ("eval 'sleep 8'", Sleep, Some(8)),
        ("( sleep 4 )", Sleep, Some(4)),
        ("{ sleep 4; }", Sleep, Some(4)),
        ("if true; then sleep 4; fi", Sleep, Some(4)),
        ("! sleep 4", Sleep, Some(4)),
        ("echo $(sleep 4)", Sleep, Some(4)),
        ("x=\"$(sleep 4)\"", Sleep, Some(4)),
        ("echo `sleep 4`", Sleep, Some(4)),
        ("bash <<'EOF'\nsleep 4\nEOF", Sleep, Some(4)),
        ("cat <<EOF > f\nsleep 9\nEOF\nsleep 2", Sleep, Some(2)),
        ("docker compose wait db", ClusterWait, None),
        ("npx -y wait-for-it db:5432", ClusterWait, None),
        (
            "python3 - <<'EOF'\nimport time\ntime.sleep(4)\nEOF",
            Interpreter,
            Some(4),
        ),
        ("ruby -e 'sleep 2'", Interpreter, Some(2)),
        ("perl -le 'sleep 2'", Interpreter, Some(2)),
    ];

    /// Design 10 "False-positive guards", plus commands that look close.
    const GUARDS: &[&str] = &[
        "sleep 30 &",
        "sleep 30 > /dev/null 2>&1 &",
        "nohup sleep 30 > log 2>&1 &",
        "setsid sleep 30",
        "sleep 30 & disown",
        "( sleep 30; echo hi ) &",
        "{ sleep 30; } &",
        "cargo build && sleep 30 &",
        "while true; do sleep 5; done &",
        "hub job start -- sleep 600",
        "monocode app sessions.start --json '{}'",
        "echo sleep 30",
        "echo 'sleep 30'",
        "printf '%s' \"sleep 30\"",
        "git commit -m \"sleep 30\"",
        "git commit -m 'wait for CI'",
        "cat <<EOF\nsleep 30\nEOF",
        "cat <<-EOF\n\tsleep 30\n\tEOF",
        "cargo test --sleep-ms 30",
        "sleepwalk 30",
        "grep -r sleep src",
        "# sleep 30",
        "echo hi # sleep 30",
        "echo '$(sleep 30)'",
        "echo a\\;sleep 2",
        "timeout 600 cargo test",
        "timeout 5 make",
        "cargo build",
        "npm run dev",
        "gh pr checks 912",
        "gh run view 123",
        "tail -n 20 app.log",
        "fswatch -1 src",
        "inotifywait src",
        "kubectl get pods",
        "kubectl rollout status deploy/api --watch=false",
        "docker ps",
        "python3 -c 'print(1)'",
        "python3 script.py",
        "node -e 'console.log(1)'",
        "perl -e 'print \"sleep\"'",
        "command -v sleep",
        "for f in *.rs; do echo $f; done",
        "while read l; do echo $l; done < f",
        "f() { sleep 30; }",
        "function f { sleep 30; }",
        "sleep",
        "bash script.sh",
        "echo done; echo do",
        "x=$((1+2)); echo $x",
    ];

    fn check(rows: &[Row]) {
        for &(command, kind, seconds) in rows {
            assert_eq!(
                classify(command, false),
                Some(Wait { kind, seconds }),
                "{command:?}"
            );
        }
    }

    #[test]
    fn every_design_hold_pattern_holds_with_its_kind_and_seconds() {
        // Defect: a foreground wait from design 10 runs without asking, or
        // reports the wrong length to the short-sleep skip.
        check(HOLDS);
    }

    #[test]
    fn wrappers_nesting_and_substitutions_still_hold() {
        // Defect: a wait hidden behind env/sudo/bash -c/$(...) or a heredoc
        // script runs without asking.
        check(ADDED_HOLDS);
    }

    #[test]
    fn guards_never_hold() {
        // Defect: a backgrounded command, a quoted or heredoc `sleep`, or
        // bounded work is held and blocks the agent for nothing.
        for command in GUARDS {
            assert_eq!(classify(command, false), None, "{command:?}");
        }
    }

    #[test]
    fn the_background_flag_never_holds() {
        // Defect: Claude run_in_background or omp async is ignored and the
        // background start is held.
        for &(command, ..) in HOLDS {
            assert_eq!(classify(command, true), None, "{command:?}");
        }
    }

    #[test]
    fn lexing_respects_quotes_separators_and_continuations() {
        // Defect: a `;` inside quotes splits the command, or a line
        // continuation hides the next command.
        check(&[
            ("echo \"a;b\" ; sleep 2", Sleep, Some(2)),
            ("echo 'it''s'; sleep 2", Sleep, Some(2)),
            ("ls \\\n && sleep 2", Sleep, Some(2)),
            ("x=$((1+2)); sleep 2", Sleep, Some(2)),
            ("echo \"$(echo ')')\"; sleep 2", Sleep, Some(2)),
            ("cmd &>/dev/null; sleep 2", Sleep, Some(2)),
        ]);
    }

    #[test]
    fn deep_nesting_stops_without_overflowing() {
        // Defect: a hostile chain of substitutions recurses until the stack
        // overflows, and a crash in the hook fails open on Codex.
        let command = format!("{}sleep 1{}", "echo $(".repeat(200), ")".repeat(200));
        assert_eq!(classify(&command, false), None);
    }
}
