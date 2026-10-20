//! A small Groovy-ish front end for Jenkinsfiles: a lexer, a generic "DSL call" parser (`name args { block }`)
//! and a mini expression evaluator for conditions and GString interpolation.

use regex::Regex;

#[derive(Debug, Clone, PartialEq)]
pub enum TK {
    Ident,
    Str,
    Num,
    Punct,
}

#[derive(Debug, Clone)]
pub struct Tok {
    pub kind: TK,
    pub text: String,
    /// Double-quoted (GString) string: `$x` / `${x}` are interpolated at runtime.
    pub interp: bool,
    /// A newline sits between this token and the previous one.
    pub nl: bool,
    pub start: usize,
    pub end: usize,
}

/// Placeholder for an escaped `\$` inside a GString.
pub const ESC_DOLLAR: char = '\u{1}';

pub fn lex(src: &str) -> Vec<Tok> {
    let c: Vec<(usize, char)> = src.char_indices().collect();
    let n = c.len();
    let at = |i: usize| c.get(i).map(|x| x.1);
    let byte = |i: usize| c.get(i).map(|x| x.0).unwrap_or(src.len());
    let mut i = 0;
    let mut nl = false;
    let mut out: Vec<Tok> = vec![];
    if src.starts_with("#!") {
        while i < n && c[i].1 != '\n' {
            i += 1;
        }
    }
    while i < n {
        let ch = c[i].1;
        if ch == '\n' {
            nl = true;
            i += 1;
            continue;
        }
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        if ch == '/' && at(i + 1) == Some('/') {
            while i < n && c[i].1 != '\n' {
                i += 1;
            }
            continue;
        }
        if ch == '/' && at(i + 1) == Some('*') {
            i += 2;
            while i + 1 < n && !(c[i].1 == '*' && c[i + 1].1 == '/') {
                if c[i].1 == '\n' {
                    nl = true;
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        let start = byte(i);
        // Slashy string (regex literal): only where an operand is expected.
        if ch == '/' && at(i + 1).map(|x| x != ' ' && x != '=').unwrap_or(false) {
            let operand_expected = match out.last() {
                None => true,
                Some(t) => t.kind == TK::Punct && !matches!(t.text.as_str(), ")" | "]" | "}"),
            };
            if operand_expected {
                let mut j = i + 1;
                let mut v = String::new();
                while j < n && c[j].1 != '/' && c[j].1 != '\n' {
                    if c[j].1 == '\\' && at(j + 1) == Some('/') {
                        v.push('/');
                        j += 2;
                        continue;
                    }
                    v.push(c[j].1);
                    j += 1;
                }
                if at(j) == Some('/') {
                    i = j + 1;
                    out.push(Tok { kind: TK::Str, text: v, interp: false, nl, start, end: byte(i) });
                    nl = false;
                    continue;
                }
            }
        }
        if ch == '\'' || ch == '"' {
            let triple = at(i + 1) == Some(ch) && at(i + 2) == Some(ch);
            i += if triple { 3 } else { 1 };
            let mut v = String::new();
            loop {
                if i >= n {
                    break;
                }
                let d = c[i].1;
                if triple {
                    if d == ch && at(i + 1) == Some(ch) && at(i + 2) == Some(ch) {
                        i += 3;
                        break;
                    }
                } else if d == ch {
                    i += 1;
                    break;
                }
                if d == '\\' && i + 1 < n {
                    let e = c[i + 1].1;
                    i += 2;
                    match e {
                        'n' => v.push('\n'),
                        't' => v.push('\t'),
                        'r' => {}
                        '\n' => {}
                        '$' if ch == '"' => v.push(ESC_DOLLAR),
                        '\\' => v.push('\\'),
                        '\'' => v.push('\''),
                        '"' => v.push('"'),
                        other => {
                            v.push('\\');
                            v.push(other);
                        }
                    }
                    continue;
                }
                v.push(d);
                i += 1;
            }
            if triple && v.starts_with('\n') {
                v.remove(0);
            }
            out.push(Tok { kind: TK::Str, text: v, interp: ch == '"', nl, start, end: byte(i) });
            nl = false;
            continue;
        }
        if ch.is_ascii_digit() {
            let st = i;
            while i < n && (c[i].1.is_ascii_alphanumeric() || (c[i].1 == '.' && at(i + 1).map(|x| x.is_ascii_digit()).unwrap_or(false))) {
                i += 1;
            }
            out.push(Tok { kind: TK::Num, text: src[byte(st)..byte(i)].to_string(), interp: false, nl, start, end: byte(i) });
            nl = false;
            continue;
        }
        if ch.is_alphabetic() || ch == '_' || ch == '$' {
            let st = i;
            while i < n && (c[i].1.is_alphanumeric() || c[i].1 == '_' || c[i].1 == '$') {
                i += 1;
            }
            out.push(Tok { kind: TK::Ident, text: src[byte(st)..byte(i)].to_string(), interp: false, nl, start, end: byte(i) });
            nl = false;
            continue;
        }
        // Punctuation, longest operator first.
        let three: String = c[i..(i + 3).min(n)].iter().map(|x| x.1).collect();
        let two: String = c[i..(i + 2).min(n)].iter().map(|x| x.1).collect();
        let op = if three == "==~" || three == "?.=" {
            three
        } else if matches!(two.as_str(), "==" | "!=" | "&&" | "||" | "<=" | ">=" | "=~" | "?:" | "->" | "+=" | "-=" | "++" | "--" | "?.") {
            two
        } else {
            ch.to_string()
        };
        i += op.chars().count();
        out.push(Tok { kind: TK::Punct, text: op, interp: false, nl, start, end: byte(i) });
        nl = false;
    }
    out
}

// ============================== DSL tree ==============================

#[derive(Debug, Clone)]
pub enum Val {
    Str { v: String, interp: bool },
    Num(String),
    /// Anything else, kept as source text and evaluated later.
    Expr(String),
    /// `[a, b]` or `[k: v]`
    List(Vec<Arg>),
    /// `{ ... }` closure used as a value (`parallel a: { ... }`).
    Block(Vec<Node>),
}

#[derive(Debug, Clone)]
pub struct Arg {
    pub name: Option<String>,
    pub val: Val,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub args: Vec<Arg>,
    pub block: Option<Vec<Node>>,
    pub line: usize,
    /// Source text of the whole statement.
    pub raw: String,
}

impl Node {
    pub fn arg(&self, name: &str) -> Option<&Val> {
        self.args.iter().find(|a| a.name.as_deref() == Some(name)).map(|a| &a.val)
    }
    /// First positional argument, or the named one.
    pub fn first(&self, name: &str) -> Option<&Val> {
        self.arg(name).or_else(|| self.args.iter().find(|a| a.name.is_none()).map(|a| &a.val))
    }
}

pub struct Parser<'a> {
    src: &'a str,
    t: Vec<Tok>,
    i: usize,
}

fn is_infix(t: &str) -> bool {
    matches!(t, "+" | "-" | "*" | "/" | "%" | "&&" | "||" | "?" | ":" | "." | "==" | "!=" | "<" | ">" | "<=" | ">=" | "=~" | "==~" | "?:" | "?." | "in" | "instanceof" | "as")
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a str) -> Parser<'a> {
        Parser { src, t: lex(src), i: 0 }
    }

    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }

    fn is(&self, p: &str) -> bool {
        matches!(self.peek(), Some(t) if t.kind == TK::Punct && t.text == p)
    }

    fn line_of(&self, t: &Tok) -> usize {
        self.src[..t.start.min(self.src.len())].matches('\n').count() + 1
    }

    pub fn parse_all(&mut self) -> Vec<Node> {
        self.stmts(false)
    }

    fn stmts(&mut self, closer: bool) -> Vec<Node> {
        let mut out = vec![];
        while let Some(t) = self.peek() {
            if t.kind == TK::Punct && t.text == "}" {
                if closer {
                    self.i += 1;
                    return out;
                }
                self.i += 1;
                continue;
            }
            if t.kind == TK::Punct && t.text == ";" {
                self.i += 1;
                continue;
            }
            let before = self.i;
            let start = t.start;
            if let Some(mut n) = self.stmt() {
                if self.i > before {
                    let end = self.t[self.i - 1].end;
                    n.raw = self.src[start..end.max(start)].to_string();
                }
                out.push(n);
            }
            if self.i == before {
                self.i += 1;
            }
        }
        out
    }

    fn block(&mut self) -> Vec<Node> {
        // current token is `{`
        self.i += 1;
        self.stmts(true)
    }

    fn stmt(&mut self) -> Option<Node> {
        let first = self.peek()?.clone();
        let line = self.line_of(&first);
        if first.kind != TK::Ident {
            // Not a call: keep the rest of the line as raw text so nothing is silently dropped.
            let start = first.start;
            let mut end = first.end;
            self.i += 1;
            while let Some(t) = self.peek() {
                if t.nl || (t.kind == TK::Punct && t.text == "}") {
                    break;
                }
                end = t.end;
                self.i += 1;
            }
            return Some(Node { name: "?".into(), args: vec![Arg { name: None, val: Val::Expr(self.src[start..end].to_string()) }], block: None, line, raw: String::new() });
        }
        self.i += 1;
        let mut name = first.text.clone();
        // `else if`
        if name == "else" && matches!(self.peek(), Some(t) if t.kind == TK::Ident && t.text == "if" && !t.nl) {
            self.i += 1;
            name = "else if".into();
        }
        // dotted chains: env.FOO, docker.image(...).inside
        loop {
            if self.is(".") && matches!(self.t.get(self.i + 1), Some(t) if t.kind == TK::Ident) && !self.peek().unwrap().nl {
                name.push('.');
                name.push_str(&self.t[self.i + 1].text.clone());
                self.i += 2;
                continue;
            }
            break;
        }
        // assignment
        if self.is("=") && !self.peek().unwrap().nl {
            self.i += 1;
            let v = self.value(false);
            return Some(Node { name: "=".into(), args: vec![Arg { name: None, val: Val::Str { v: name, interp: false } }, Arg { name: None, val: v }], block: None, line, raw: String::new() });
        }
        let mut args = vec![];
        let mut block = None;
        if self.is("(") && !self.peek().unwrap().nl {
            self.i += 1;
            args = self.args(true);
            // chained calls: foo(a).bar(b) { }
            while self.is(".") && matches!(self.t.get(self.i + 1), Some(t) if t.kind == TK::Ident) && !self.peek().unwrap().nl {
                name.push('.');
                name.push_str(&self.t[self.i + 1].text.clone());
                self.i += 2;
                if self.is("(") {
                    self.i += 1;
                    let more = self.args(true);
                    args.extend(more);
                }
            }
            if self.is("{") && !self.peek().unwrap().nl {
                block = Some(self.block());
            }
        } else if self.is("{") && !self.peek().unwrap().nl {
            block = Some(self.block());
        } else if matches!(self.peek(), Some(t) if !t.nl && (matches!(t.kind, TK::Str | TK::Num | TK::Ident) || (t.kind == TK::Punct && (t.text == "[" || t.text == "-" || t.text == "!")))) {
            args = self.args(false);
            if self.is("{") && !self.peek().unwrap().nl {
                block = Some(self.block());
            }
        } else if self.is("{") {
            block = Some(self.block());
        }
        // `else {` / `try {` / `finally {` etc. directly on the next line after `}`
        Some(Node { name, args, block, line, raw: String::new() })
    }

    fn args(&mut self, parens: bool) -> Vec<Arg> {
        let mut out = vec![];
        loop {
            if parens && self.is(")") {
                self.i += 1;
                break;
            }
            let Some(t) = self.peek().cloned() else { break };
            if !parens && (t.nl && !out.is_empty() && false) {
                break;
            }
            if !parens && t.kind == TK::Punct && matches!(t.text.as_str(), "}" | ";" | "{") {
                break;
            }
            // named argument: ident ':'  (not `?:`)
            let named = t.kind == TK::Ident && matches!(self.t.get(self.i + 1), Some(n) if n.kind == TK::Punct && n.text == ":");
            let name = if named {
                self.i += 2;
                Some(t.text.clone())
            } else if t.kind == TK::Str && matches!(self.t.get(self.i + 1), Some(n) if n.kind == TK::Punct && n.text == ":") {
                // 'quoted key': value (inside maps)
                self.i += 2;
                Some(t.text.clone())
            } else {
                None
            };
            let v = self.value(parens);
            out.push(Arg { name, val: v });
            if self.is(",") {
                self.i += 1;
                continue;
            }
            if parens {
                if self.is(")") {
                    self.i += 1;
                }
                break;
            }
            break;
        }
        out
    }

    fn value(&mut self, in_parens: bool) -> Val {
        if self.is("{") {
            return Val::Block(self.block());
        }
        if self.is("[") && self.looks_like_literal_list() {
            self.i += 1;
            let items = self.list_items();
            // Allow `['a','b'].join(',')` style postfix by falling through only for plain lists.
            return Val::List(items);
        }
        // Gather tokens up to a terminator at depth 0.
        let mut depth = 0i32;
        let first = self.i;
        let mut prev_infix = false;
        while let Some(t) = self.peek() {
            let is_p = t.kind == TK::Punct;
            if depth == 0 {
                if is_p && matches!(t.text.as_str(), "," | ")" | "]" | "}" | ";") {
                    break;
                }
                if is_p && t.text == "{" && !in_parens {
                    break;
                }
                if self.i > first && t.nl && !prev_infix && !(is_p && is_infix(&t.text)) {
                    break;
                }
            }
            if is_p {
                match t.text.as_str() {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => depth -= 1,
                    _ => {}
                }
            }
            prev_infix = is_p && is_infix(&t.text) || (t.kind == TK::Ident && matches!(t.text.as_str(), "in" | "instanceof" | "as"));
            self.i += 1;
        }
        let toks = &self.t[first..self.i];
        match toks {
            [] => Val::Expr(String::new()),
            [only] if only.kind == TK::Str => Val::Str { v: only.text.clone(), interp: only.interp },
            [only] if only.kind == TK::Num => Val::Num(only.text.clone()),
            _ => Val::Expr(self.src[toks[0].start..toks[toks.len() - 1].end].to_string()),
        }
    }

    /// `[` followed by something that is a list/map literal, not an index (we only get here at value start).
    fn looks_like_literal_list(&self) -> bool {
        true
    }

    fn list_items(&mut self) -> Vec<Arg> {
        let mut out = vec![];
        // `[:]`
        if self.is(":") {
            self.i += 1;
        }
        loop {
            if self.is("]") {
                self.i += 1;
                break;
            }
            let Some(t) = self.peek().cloned() else { break };
            let keyed = matches!(t.kind, TK::Ident | TK::Str) && matches!(self.t.get(self.i + 1), Some(n) if n.kind == TK::Punct && n.text == ":");
            let name = if keyed {
                self.i += 2;
                Some(t.text.clone())
            } else {
                None
            };
            let v = self.value(true);
            out.push(Arg { name, val: v });
            if self.is(",") {
                self.i += 1;
            } else if !self.is("]") {
                // Garbage: bail out to the closing bracket.
                while let Some(t) = self.peek().cloned() {
                    self.i += 1;
                    if t.kind == TK::Punct && t.text == "]" {
                        break;
                    }
                }
                break;
            }
        }
        out
    }
}

pub fn parse(src: &str) -> Vec<Node> {
    Parser::new(src).parse_all()
}

// ============================== evaluation ==============================

#[derive(Debug, Clone, PartialEq)]
pub enum GV {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    List(Vec<GV>),
}

impl GV {
    pub fn truthy(&self) -> bool {
        match self {
            GV::Null => false,
            GV::Bool(b) => *b,
            GV::Num(n) => *n != 0.0,
            GV::Str(s) => !s.is_empty(),
            GV::List(l) => !l.is_empty(),
        }
    }
    pub fn text(&self) -> String {
        match self {
            GV::Null => "null".into(),
            GV::Bool(b) => b.to_string(),
            GV::Num(n) => {
                if n.fract() == 0.0 {
                    format!("{}", *n as i64)
                } else {
                    n.to_string()
                }
            }
            GV::Str(s) => s.clone(),
            GV::List(l) => format!("[{}]", l.iter().map(|x| x.text()).collect::<Vec<_>>().join(", ")),
        }
    }
    fn num(&self) -> f64 {
        match self {
            GV::Num(n) => *n,
            GV::Str(s) => s.trim().parse().unwrap_or(f64::NAN),
            GV::Bool(b) => *b as i32 as f64,
            _ => f64::NAN,
        }
    }
}

pub trait Scope {
    /// Value of `env.X`, `params.X`, `BUILD_NUMBER`, `currentBuild.number` ... None = unknown.
    fn var(&self, name: &str) -> Option<String>;
    /// Same lookup, but with a non-string type (e.g. boolean parameters).
    fn typed(&self, _name: &str) -> Option<GV> {
        None
    }
    /// Built-in predicates used by translated `when {}` conditions (`__branch`, `__tag`, `__changeset`, ...).
    fn call(&self, name: &str, args: &[GV]) -> Option<GV>;
}

struct E<'a> {
    t: Vec<Tok>,
    i: usize,
    sc: &'a dyn Scope,
}

fn glob_match(pat: &str, text: &str) -> bool {
    let mut re = String::from("^");
    for ch in pat.chars() {
        match ch {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    Regex::new(&re).map(|r| r.is_match(text)).unwrap_or(false)
}

impl<'a> E<'a> {
    fn is(&self, p: &str) -> bool {
        matches!(self.t.get(self.i), Some(t) if t.kind == TK::Punct && t.text == p)
    }
    fn is_word(&self, w: &str) -> bool {
        matches!(self.t.get(self.i), Some(t) if t.kind == TK::Ident && t.text == w)
    }
    fn ternary(&mut self) -> Result<GV, String> {
        let c = self.or()?;
        if self.is("?") {
            self.i += 1;
            let a = self.ternary()?;
            if !self.is(":") {
                return Err("expected `:` in ternary".into());
            }
            self.i += 1;
            let b = self.ternary()?;
            return Ok(if c.truthy() { a } else { b });
        }
        if self.is("?:") {
            self.i += 1;
            let b = self.ternary()?;
            return Ok(if c.truthy() { c } else { b });
        }
        Ok(c)
    }
    fn or(&mut self) -> Result<GV, String> {
        let mut l = self.and()?;
        while self.is("||") {
            self.i += 1;
            let r = self.and()?;
            l = GV::Bool(l.truthy() || r.truthy());
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<GV, String> {
        let mut l = self.rel()?;
        while self.is("&&") {
            self.i += 1;
            let r = self.rel()?;
            l = GV::Bool(l.truthy() && r.truthy());
        }
        Ok(l)
    }
    fn rel(&mut self) -> Result<GV, String> {
        let mut l = self.add()?;
        loop {
            let op = match self.t.get(self.i) {
                Some(t) if t.kind == TK::Punct && matches!(t.text.as_str(), "==" | "!=" | "<" | ">" | "<=" | ">=" | "=~" | "==~") => t.text.clone(),
                Some(t) if t.kind == TK::Ident && t.text == "in" => "in".into(),
                _ => return Ok(l),
            };
            self.i += 1;
            let r = self.add()?;
            l = match op.as_str() {
                "==" => GV::Bool(eq(&l, &r)),
                "!=" => GV::Bool(!eq(&l, &r)),
                "<" => GV::Bool(l.num() < r.num()),
                ">" => GV::Bool(l.num() > r.num()),
                "<=" => GV::Bool(l.num() <= r.num()),
                ">=" => GV::Bool(l.num() >= r.num()),
                "in" => GV::Bool(matches!(&r, GV::List(items) if items.iter().any(|x| eq(x, &l)))),
                _ => {
                    let re = Regex::new(&if op == "==~" { format!("^(?:{})$", r.text()) } else { r.text() }).map_err(|e| e.to_string())?;
                    GV::Bool(re.is_match(&l.text()))
                }
            };
        }
    }
    fn add(&mut self) -> Result<GV, String> {
        let mut l = self.unary()?;
        loop {
            if self.is("+") {
                self.i += 1;
                let r = self.unary()?;
                l = match (&l, &r) {
                    (GV::Num(a), GV::Num(b)) => GV::Num(a + b),
                    (GV::List(a), GV::List(b)) => GV::List(a.iter().chain(b.iter()).cloned().collect()),
                    _ => GV::Str(format!("{}{}", if matches!(l, GV::Null) { "null".into() } else { l.text() }, r.text())),
                };
            } else if self.is("-") {
                self.i += 1;
                let r = self.unary()?;
                l = GV::Num(l.num() - r.num());
            } else {
                return Ok(l);
            }
        }
    }
    fn unary(&mut self) -> Result<GV, String> {
        if self.is("!") {
            self.i += 1;
            let v = self.unary()?;
            return Ok(GV::Bool(!v.truthy()));
        }
        if self.is("-") {
            self.i += 1;
            let v = self.unary()?;
            return Ok(GV::Num(-v.num()));
        }
        self.postfix()
    }
    fn call_args(&mut self) -> Result<Vec<GV>, String> {
        // current token is `(`
        self.i += 1;
        let mut out = vec![];
        while !self.is(")") {
            if self.i >= self.t.len() {
                return Err("expected `)`".into());
            }
            // named args (`expected: 1, actual: x`) are positional here
            if matches!(self.t.get(self.i), Some(t) if t.kind == TK::Ident) && matches!(self.t.get(self.i + 1), Some(t) if t.kind == TK::Punct && t.text == ":") {
                self.i += 2;
            }
            out.push(self.ternary()?);
            if self.is(",") {
                self.i += 1;
            }
        }
        self.i += 1;
        Ok(out)
    }
    fn postfix(&mut self) -> Result<GV, String> {
        let mut v = self.primary()?;
        loop {
            if (self.is(".") || self.is("?.")) && matches!(self.t.get(self.i + 1), Some(t) if t.kind == TK::Ident) {
                let m = self.t[self.i + 1].text.clone();
                self.i += 2;
                if self.is("(") {
                    let args = self.call_args()?;
                    v = method(&v, &m, &args)?;
                } else {
                    v = match (&v, m.as_str()) {
                        (GV::Str(s), "length") => GV::Num(s.chars().count() as f64),
                        (GV::List(l), "size") => GV::Num(l.len() as f64),
                        _ => GV::Null,
                    };
                }
            } else if self.is("[") {
                self.i += 1;
                let idx = self.ternary()?;
                if !self.is("]") {
                    return Err("expected `]`".into());
                }
                self.i += 1;
                v = match (&v, &idx) {
                    (GV::List(l), i) => l.get(i.num() as usize).cloned().unwrap_or(GV::Null),
                    _ => GV::Null,
                };
            } else {
                return Ok(v);
            }
        }
    }
    fn primary(&mut self) -> Result<GV, String> {
        let t = self.t.get(self.i).cloned().ok_or("unexpected end of expression")?;
        self.i += 1;
        match t.kind {
            TK::Str => Ok(GV::Str(if t.interp { interpolate(&t.text, self.sc) } else { t.text })),
            TK::Num => Ok(GV::Num(t.text.parse().unwrap_or(0.0))),
            TK::Punct if t.text == "(" => {
                let v = self.ternary()?;
                if !self.is(")") {
                    return Err("expected `)`".into());
                }
                self.i += 1;
                Ok(v)
            }
            TK::Punct if t.text == "[" => {
                let mut items = vec![];
                while !self.is("]") {
                    if self.i >= self.t.len() {
                        return Err("expected `]`".into());
                    }
                    items.push(self.ternary()?);
                    if self.is(",") {
                        self.i += 1;
                    }
                }
                self.i += 1;
                Ok(GV::List(items))
            }
            TK::Ident => match t.text.as_str() {
                "true" => Ok(GV::Bool(true)),
                "false" => Ok(GV::Bool(false)),
                "null" => Ok(GV::Null),
                "return" => self.ternary(),
                _ => {
                    // dotted path
                    let mut path = t.text.clone();
                    // function call?
                    if self.is("(") {
                        let args = self.call_args()?;
                        return self.sc.call(&path, &args).ok_or_else(|| format!("unsupported function `{path}`"));
                    }
                    let mut last_good = (path.clone(), self.i);
                    loop {
                        if self.is(".") && matches!(self.t.get(self.i + 1), Some(n) if n.kind == TK::Ident) {
                            let seg = self.t[self.i + 1].text.clone();
                            // A segment followed by `(` is a method on the value resolved so far.
                            if matches!(self.t.get(self.i + 2), Some(n) if n.kind == TK::Punct && n.text == "(") {
                                break;
                            }
                            path.push('.');
                            path.push_str(&seg);
                            self.i += 2;
                            if self.sc.var(&path).is_some() {
                                last_good = (path.clone(), self.i);
                            }
                            continue;
                        }
                        break;
                    }
                    // Prefer the longest path that resolves (so `env.FOO.trim()` finds `env.FOO`).
                    let resolved = if self.sc.var(&path).is_some() { (path.clone(), self.i) } else { last_good };
                    self.i = resolved.1;
                    Ok(self.sc.typed(&resolved.0).or_else(|| self.sc.var(&resolved.0).map(GV::Str)).unwrap_or(GV::Null))
                }
            },
            _ => Err(format!("unexpected `{}`", t.text)),
        }
    }
}

fn eq(a: &GV, b: &GV) -> bool {
    match (a, b) {
        (GV::Num(x), GV::Str(s)) | (GV::Str(s), GV::Num(x)) => s.trim().parse::<f64>().map(|n| n == *x).unwrap_or(false),
        (GV::Bool(x), GV::Str(s)) | (GV::Str(s), GV::Bool(x)) => s == &x.to_string(),
        _ => a == b,
    }
}

fn method(v: &GV, m: &str, a: &[GV]) -> Result<GV, String> {
    let s = v.text();
    let arg = |i: usize| a.get(i).map(|x| x.text()).unwrap_or_default();
    Ok(match m {
        "trim" => GV::Str(s.trim().to_string()),
        "toString" | "toLowerCase" | "toUpperCase" => GV::Str(match m {
            "toLowerCase" => s.to_lowercase(),
            "toUpperCase" => s.to_uppercase(),
            _ => s,
        }),
        "equals" => GV::Bool(s == arg(0)),
        "equalsIgnoreCase" => GV::Bool(s.eq_ignore_ascii_case(&arg(0))),
        "contains" => match v {
            GV::List(l) => GV::Bool(l.iter().any(|x| eq(x, a.first().unwrap_or(&GV::Null)))),
            _ => GV::Bool(s.contains(&arg(0))),
        },
        "startsWith" => GV::Bool(s.starts_with(&arg(0))),
        "endsWith" => GV::Bool(s.ends_with(&arg(0))),
        "isEmpty" => GV::Bool(s.is_empty()),
        "isInteger" | "isNumber" => GV::Bool(s.trim().parse::<f64>().is_ok()),
        "toInteger" => GV::Num(s.trim().parse::<f64>().unwrap_or(0.0)),
        "matches" => GV::Bool(Regex::new(&format!("^(?:{})$", arg(0))).map(|r| r.is_match(&s)).unwrap_or(false)),
        "replace" => GV::Str(s.replace(&arg(0), &arg(1))),
        "split" => GV::List(s.split(&arg(0)).map(|x| GV::Str(x.to_string())).collect()),
        "join" => GV::Str(match v {
            GV::List(l) => l.iter().map(|x| x.text()).collect::<Vec<_>>().join(&arg(0)),
            _ => s,
        }),
        "size" | "length" => GV::Num(match v {
            GV::List(l) => l.len(),
            _ => s.chars().count(),
        } as f64),
        other => return Err(format!("unsupported method `.{other}()`")),
    })
}

pub fn eval(expr: &str, sc: &dyn Scope) -> Result<GV, String> {
    let t = lex(expr);
    if t.is_empty() {
        return Ok(GV::Null);
    }
    let mut e = E { t, i: 0, sc };
    let v = e.ternary()?;
    if e.i < e.t.len() {
        return Err(format!("cannot evaluate `{expr}` (stopped at `{}`)", e.t[e.i].text));
    }
    Ok(v)
}

/// Expand `$name`, `$a.b` and `${expr}` in a GString. Unknown names are left as written (a shell variable).
pub fn interpolate(s: &str, sc: &dyn Scope) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] == ESC_DOLLAR {
            out.push('$');
            i += 1;
            continue;
        }
        if c[i] != '$' {
            out.push(c[i]);
            i += 1;
            continue;
        }
        if c.get(i + 1) == Some(&'{') {
            let mut depth = 1;
            let mut j = i + 2;
            while j < c.len() && depth > 0 {
                match c[j] {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            if depth == 0 {
                let inner: String = c[i + 2..j - 1].iter().collect();
                match eval(&inner, sc) {
                    Ok(GV::Null) => out.push_str("null"),
                    Ok(v) => out.push_str(&v.text()),
                    Err(_) => {
                        // Not a Groovy expression: leave it for the shell.
                        out.push_str(&c[i..j].iter().collect::<String>());
                    }
                }
                i = j;
                continue;
            }
        }
        if c.get(i + 1).map(|x| x.is_alphabetic() || *x == '_').unwrap_or(false) {
            let mut j = i + 1;
            while j < c.len() && (c[j].is_alphanumeric() || c[j] == '_') {
                j += 1;
            }
            // `$a.b.c` consumes dotted segments when the whole path resolves.
            let mut best_end = j;
            let mut path: String = c[i + 1..j].iter().collect();
            let mut k = j;
            while k + 1 < c.len() && c[k] == '.' && (c[k + 1].is_alphabetic() || c[k + 1] == '_') {
                let mut e = k + 1;
                while e < c.len() && (c[e].is_alphanumeric() || c[e] == '_') {
                    e += 1;
                }
                let cand: String = c[i + 1..e].iter().collect();
                if sc.var(&cand).is_some() {
                    best_end = e;
                    path = cand;
                }
                k = e;
            }
            match sc.var(&path) {
                Some(v) => {
                    out.push_str(&v);
                    i = best_end;
                }
                None => {
                    out.push('$');
                    i += 1;
                }
            }
            continue;
        }
        out.push('$');
        i += 1;
    }
    out
}

pub fn glob(pat: &str, text: &str) -> bool {
    glob_match(pat, text)
}

