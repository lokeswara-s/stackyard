//! Expression languages: GitHub `${{ }}` expressions and GitLab `rules:if` expressions.

use regex::Regex;
use serde_json::{json, Value};

// ============================== GitHub ==============================

pub struct GhCtx<'a> {
    pub root: &'a Value,
    /// success() / failure() / cancelled() answers.
    pub ok: bool,
    pub cancelled: bool,
    pub hash_files: &'a dyn Fn(&[String]) -> String,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Str(String),
    Num(f64),
    Ident(String),
    Op(&'static str),
    Dot,
    LBracket,
    RBracket,
    LParen,
    RParen,
    Comma,
    Star,
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = vec![];
    while i < c.len() {
        let ch = c[i];
        match ch {
            ' ' | '\t' | '\n' | '\r' => i += 1,
            '\'' => {
                let mut v = String::new();
                i += 1;
                loop {
                    if i >= c.len() {
                        return Err("unterminated string".into());
                    }
                    if c[i] == '\'' {
                        if i + 1 < c.len() && c[i + 1] == '\'' {
                            v.push('\'');
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    v.push(c[i]);
                    i += 1;
                }
                out.push(Tok::Str(v));
            }
            '0'..='9' | '-' if ch != '-' || (i + 1 < c.len() && c[i + 1].is_ascii_digit()) => {
                let st = i;
                i += 1;
                while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.' || c[i] == 'e' || c[i] == 'x' || c[i].is_ascii_hexdigit()) {
                    i += 1;
                }
                let t: String = c[st..i].iter().collect();
                let n = if let Some(h) = t.strip_prefix("0x") { i64::from_str_radix(h, 16).map(|v| v as f64).map_err(|e| e.to_string())? } else { t.parse::<f64>().map_err(|_| format!("bad number `{t}`"))? };
                out.push(Tok::Num(n));
            }
            '.' => {
                out.push(Tok::Dot);
                i += 1;
            }
            '[' => {
                out.push(Tok::LBracket);
                i += 1;
            }
            ']' => {
                out.push(Tok::RBracket);
                i += 1;
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            '*' => {
                out.push(Tok::Star);
                i += 1;
            }
            '!' | '=' | '<' | '>' | '&' | '|' => {
                let two: String = c[i..(i + 2).min(c.len())].iter().collect();
                let op: &'static str = match two.as_str() {
                    "!=" => "!=",
                    "==" => "==",
                    "<=" => "<=",
                    ">=" => ">=",
                    "&&" => "&&",
                    "||" => "||",
                    _ => match ch {
                        '!' => "!",
                        '<' => "<",
                        '>' => ">",
                        _ => return Err(format!("unexpected `{ch}`")),
                    },
                };
                i += op.len();
                out.push(Tok::Op(op));
            }
            _ if ch.is_alphabetic() || ch == '_' => {
                let st = i;
                while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '-') {
                    i += 1;
                }
                out.push(Tok::Ident(c[st..i].iter().collect()));
            }
            _ => return Err(format!("unexpected `{ch}` in expression")),
        }
    }
    Ok(out)
}

pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

pub fn to_str(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
            _ => n.to_string(),
        },
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

fn to_num(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => *b as i32 as f64,
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        _ => f64::NAN,
    }
}

fn loose_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) => x.to_lowercase() == y.to_lowercase(),
        (Value::Null, Value::Null) => true,
        (Value::Object(_) | Value::Array(_), _) | (_, Value::Object(_) | Value::Array(_)) => a == b,
        _ => {
            let (x, y) = (to_num(a), to_num(b));
            x == y
        }
    }
}

struct P<'a, 'b> {
    t: Vec<Tok>,
    i: usize,
    cx: &'a GhCtx<'b>,
}

impl<'a, 'b> P<'a, 'b> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.t.get(self.i).cloned();
        self.i += 1;
        t
    }
    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if *o == op)
    }
    fn or(&mut self) -> Result<Value, String> {
        let mut l = self.and()?;
        while self.is_op("||") {
            self.i += 1;
            let r = self.and()?;
            l = if truthy(&l) { l } else { r };
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Value, String> {
        let mut l = self.eq()?;
        while self.is_op("&&") {
            self.i += 1;
            let r = self.eq()?;
            l = if truthy(&l) { r } else { l };
        }
        Ok(l)
    }
    fn eq(&mut self) -> Result<Value, String> {
        let mut l = self.cmp()?;
        loop {
            if self.is_op("==") {
                self.i += 1;
                let r = self.cmp()?;
                l = Value::Bool(loose_eq(&l, &r));
            } else if self.is_op("!=") {
                self.i += 1;
                let r = self.cmp()?;
                l = Value::Bool(!loose_eq(&l, &r));
            } else {
                return Ok(l);
            }
        }
    }
    fn cmp(&mut self) -> Result<Value, String> {
        let mut l = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op(o)) if matches!(*o, "<" | "<=" | ">" | ">=") => *o,
                _ => return Ok(l),
            };
            self.i += 1;
            let r = self.unary()?;
            let res = match (&l, &r) {
                (Value::String(a), Value::String(b)) => {
                    let (a, b) = (a.to_lowercase(), b.to_lowercase());
                    match op {
                        "<" => a < b,
                        "<=" => a <= b,
                        ">" => a > b,
                        _ => a >= b,
                    }
                }
                _ => {
                    let (a, b) = (to_num(&l), to_num(&r));
                    match op {
                        "<" => a < b,
                        "<=" => a <= b,
                        ">" => a > b,
                        _ => a >= b,
                    }
                }
            };
            l = Value::Bool(res);
        }
    }
    fn unary(&mut self) -> Result<Value, String> {
        if self.is_op("!") {
            self.i += 1;
            let v = self.unary()?;
            return Ok(Value::Bool(!truthy(&v)));
        }
        self.postfix()
    }
    fn postfix(&mut self) -> Result<Value, String> {
        let mut v = self.primary()?;
        loop {
            match self.peek() {
                Some(Tok::Dot) => {
                    self.i += 1;
                    match self.next() {
                        Some(Tok::Ident(n)) => v = prop(&v, &n),
                        Some(Tok::Star) => v = star(&v),
                        _ => return Err("expected a property name after `.`".into()),
                    }
                }
                Some(Tok::LBracket) => {
                    self.i += 1;
                    if matches!(self.peek(), Some(Tok::Star)) {
                        self.i += 1;
                        v = star(&v);
                    } else {
                        let k = self.or()?;
                        v = match (&v, &k) {
                            (Value::Array(a), k) => a.get(to_num(k) as usize).cloned().unwrap_or(Value::Null),
                            (_, k) => prop(&v, &to_str(k)),
                        };
                    }
                    if self.next() != Some(Tok::RBracket) {
                        return Err("expected `]`".into());
                    }
                }
                _ => return Ok(v),
            }
        }
    }
    fn primary(&mut self) -> Result<Value, String> {
        match self.next() {
            Some(Tok::Str(s)) => Ok(Value::String(s)),
            Some(Tok::Num(n)) => Ok(json!(n)),
            Some(Tok::LParen) => {
                let v = self.or()?;
                if self.next() != Some(Tok::RParen) {
                    return Err("expected `)`".into());
                }
                Ok(v)
            }
            Some(Tok::Ident(name)) => {
                if matches!(self.peek(), Some(Tok::LParen)) {
                    self.i += 1;
                    let mut args = vec![];
                    if !matches!(self.peek(), Some(Tok::RParen)) {
                        loop {
                            args.push(self.or()?);
                            match self.next() {
                                Some(Tok::Comma) => continue,
                                Some(Tok::RParen) => break,
                                _ => return Err("expected `,` or `)`".into()),
                            }
                        }
                    } else {
                        self.i += 1;
                    }
                    return self.call(&name, args);
                }
                Ok(match name.as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    "null" => Value::Null,
                    "NaN" => Value::Null,
                    _ => prop(self.cx.root, &name),
                })
            }
            other => Err(format!("unexpected token {other:?}")),
        }
    }
    fn call(&self, name: &str, a: Vec<Value>) -> Result<Value, String> {
        let s = |i: usize| a.get(i).map(to_str).unwrap_or_default();
        Ok(match name.to_lowercase().as_str() {
            "success" => Value::Bool(self.cx.ok && !self.cx.cancelled),
            "failure" => Value::Bool(!self.cx.ok && !self.cx.cancelled),
            "always" => Value::Bool(true),
            "cancelled" => Value::Bool(self.cx.cancelled),
            "contains" => match a.first() {
                Some(Value::Array(items)) => Value::Bool(items.iter().any(|x| loose_eq(x, a.get(1).unwrap_or(&Value::Null)))),
                _ => Value::Bool(s(0).to_lowercase().contains(&s(1).to_lowercase())),
            },
            "startswith" => Value::Bool(s(0).to_lowercase().starts_with(&s(1).to_lowercase())),
            "endswith" => Value::Bool(s(0).to_lowercase().ends_with(&s(1).to_lowercase())),
            "format" => {
                let mut out = s(0);
                for (i, v) in a.iter().enumerate().skip(1) {
                    out = out.replace(&format!("{{{}}}", i - 1), &to_str(v));
                }
                Value::String(out.replace("{{", "{").replace("}}", "}"))
            }
            "join" => match a.first() {
                Some(Value::Array(items)) => Value::String(items.iter().map(to_str).collect::<Vec<_>>().join(&if a.len() > 1 { s(1) } else { ",".into() })),
                Some(v) => Value::String(to_str(v)),
                None => Value::String(String::new()),
            },
            "tojson" => Value::String(serde_json::to_string_pretty(a.first().unwrap_or(&Value::Null)).unwrap_or_default()),
            "fromjson" => serde_json::from_str(&s(0)).map_err(|e| format!("fromJSON: {e}"))?,
            "hashfiles" => Value::String((self.cx.hash_files)(&a.iter().map(to_str).collect::<Vec<_>>())),
            other => return Err(format!("unknown function `{other}`")),
        })
    }
}

fn prop(v: &Value, name: &str) -> Value {
    match v {
        Value::Object(m) => m
            .get(name)
            .or_else(|| m.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v))
            .cloned()
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn star(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Array(m.values().cloned().collect()),
        Value::Array(a) => Value::Array(a.clone()),
        _ => Value::Array(vec![]),
    }
}

pub fn gh_eval(expr: &str, cx: &GhCtx) -> Result<Value, String> {
    let t = lex(expr)?;
    if t.is_empty() {
        return Ok(Value::Null);
    }
    let mut p = P { t, i: 0, cx };
    let v = p.or()?;
    if p.i < p.t.len() {
        return Err(format!("unexpected token {:?}", p.t[p.i]));
    }
    Ok(v)
}

/// Replace every `${{ expr }}` in `s`.
pub fn gh_interpolate(s: &str, cx: &GhCtx) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(st) = rest.find("${{") {
        out.push_str(&rest[..st]);
        let after = &rest[st + 3..];
        // Find the closing `}}` outside string literals.
        let mut in_str = false;
        let mut end = None;
        let b: Vec<char> = after.chars().collect();
        let mut i = 0;
        let mut byte = 0;
        while i < b.len() {
            if b[i] == '\'' {
                in_str = !in_str;
            }
            if !in_str && b[i] == '}' && b.get(i + 1) == Some(&'}') {
                end = Some(byte);
                break;
            }
            byte += b[i].len_utf8();
            i += 1;
        }
        let end = end.ok_or("unterminated `${{`")?;
        let v = gh_eval(after[..end].trim(), cx).map_err(|e| format!("in `${{{{ {} }}}}`: {e}", after[..end].trim()))?;
        out.push_str(&to_str(&v));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `if:` conditions may omit the `${{ }}` wrapper. A bare condition is true only if it evaluates truthy.
pub fn gh_condition(cond: &str, cx: &GhCtx) -> Result<bool, String> {
    let v = gh_condition_raw(cond, cx)?;
    // Without a status function GitHub evaluates `success() && (<condition>)`.
    let has_status = ["success(", "failure(", "always(", "cancelled("].iter().any(|f| cond.contains(f));
    Ok(v && (has_status || cx.ok))
}

fn gh_condition_raw(cond: &str, cx: &GhCtx) -> Result<bool, String> {
    let c = cond.trim();
    let inner = if c.starts_with("${{") && c.ends_with("}}") && c.matches("${{").count() == 1 { c[3..c.len() - 2].trim() } else { c };
    if !c.starts_with("${{") && c.contains("${{") {
        let s = gh_interpolate(c, cx)?;
        return Ok(truthy(&Value::String(s)));
    }
    Ok(truthy(&gh_eval(inner, cx)?))
}

// ============================== GitLab ==============================

#[derive(Debug, Clone, PartialEq)]
enum GTok {
    Var(String),
    Str(String),
    Re(String, String),
    Null,
    Op(&'static str),
    LParen,
    RParen,
}

fn glex(s: &str) -> Result<Vec<GTok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = vec![];
    while i < c.len() {
        let ch = c[i];
        match ch {
            ' ' | '\t' | '\n' => i += 1,
            '(' => {
                out.push(GTok::LParen);
                i += 1;
            }
            ')' => {
                out.push(GTok::RParen);
                i += 1;
            }
            '$' => {
                i += 1;
                let braced = c.get(i) == Some(&'{');
                if braced {
                    i += 1;
                }
                let st = i;
                while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                    i += 1;
                }
                out.push(GTok::Var(c[st..i].iter().collect()));
                if braced && c.get(i) == Some(&'}') {
                    i += 1;
                }
            }
            '"' | '\'' => {
                let q = ch;
                i += 1;
                let mut v = String::new();
                while i < c.len() && c[i] != q {
                    if c[i] == '\\' && i + 1 < c.len() {
                        i += 1;
                    }
                    v.push(c[i]);
                    i += 1;
                }
                i += 1;
                out.push(GTok::Str(v));
            }
            '/' => {
                i += 1;
                let mut v = String::new();
                while i < c.len() && c[i] != '/' {
                    if c[i] == '\\' && c.get(i + 1) == Some(&'/') {
                        v.push('/');
                        i += 2;
                        continue;
                    }
                    if c[i] == '\\' && i + 1 < c.len() {
                        v.push(c[i]);
                        i += 1;
                    }
                    v.push(c[i]);
                    i += 1;
                }
                i += 1;
                let st = i;
                while i < c.len() && matches!(c[i], 'i' | 'm' | 's' | 'x' | 'u') {
                    i += 1;
                }
                out.push(GTok::Re(v, c[st..i].iter().collect()));
            }
            '=' | '!' | '&' | '|' => {
                let two: String = c[i..(i + 2).min(c.len())].iter().collect();
                let op: &'static str = match two.as_str() {
                    "==" => "==",
                    "!=" => "!=",
                    "=~" => "=~",
                    "!~" => "!~",
                    "&&" => "&&",
                    "||" => "||",
                    _ => return Err(format!("unexpected `{ch}` in rule expression")),
                };
                i += 2;
                out.push(GTok::Op(op));
            }
            _ if ch.is_alphabetic() => {
                let st = i;
                while i < c.len() && c[i].is_alphanumeric() {
                    i += 1;
                }
                let w: String = c[st..i].iter().collect();
                if w == "null" {
                    out.push(GTok::Null);
                } else {
                    return Err(format!("unexpected word `{w}`"));
                }
            }
            _ => return Err(format!("unexpected `{ch}` in rule expression")),
        }
    }
    Ok(out)
}

#[derive(Clone, Debug)]
enum GVal {
    Null,
    Str(String),
    Re(String, String),
}

struct GP<'a> {
    t: Vec<GTok>,
    i: usize,
    vars: &'a dyn Fn(&str) -> Option<String>,
}

impl<'a> GP<'a> {
    fn operand(&mut self) -> Result<GVal, String> {
        match self.t.get(self.i).cloned() {
            Some(GTok::Var(n)) => {
                self.i += 1;
                Ok((self.vars)(&n).map(GVal::Str).unwrap_or(GVal::Null))
            }
            Some(GTok::Str(s)) => {
                self.i += 1;
                Ok(GVal::Str(s))
            }
            Some(GTok::Re(r, f)) => {
                self.i += 1;
                Ok(GVal::Re(r, f))
            }
            Some(GTok::Null) => {
                self.i += 1;
                Ok(GVal::Null)
            }
            other => Err(format!("unexpected {other:?}")),
        }
    }
    fn or(&mut self) -> Result<bool, String> {
        let mut l = self.and()?;
        while matches!(self.t.get(self.i), Some(GTok::Op("||"))) {
            self.i += 1;
            let r = self.and()?;
            l = l || r;
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<bool, String> {
        let mut l = self.cmp()?;
        while matches!(self.t.get(self.i), Some(GTok::Op("&&"))) {
            self.i += 1;
            let r = self.cmp()?;
            l = l && r;
        }
        Ok(l)
    }
    fn cmp(&mut self) -> Result<bool, String> {
        if matches!(self.t.get(self.i), Some(GTok::LParen)) {
            self.i += 1;
            let v = self.or()?;
            if self.t.get(self.i) != Some(&GTok::RParen) {
                return Err("expected `)`".into());
            }
            self.i += 1;
            return Ok(v);
        }
        let l = self.operand()?;
        let op = match self.t.get(self.i) {
            Some(GTok::Op(o)) if matches!(*o, "==" | "!=" | "=~" | "!~") => *o,
            _ => return Ok(matches!(&l, GVal::Str(s) if !s.is_empty())),
        };
        self.i += 1;
        let r = self.operand()?;
        match op {
            "==" | "!=" => {
                let eq = match (&l, &r) {
                    (GVal::Null, GVal::Null) => true,
                    (GVal::Str(a), GVal::Str(b)) => a == b,
                    _ => false,
                };
                Ok(if op == "==" { eq } else { !eq })
            }
            _ => {
                let (re, flags) = match &r {
                    GVal::Re(r, f) => (r.clone(), f.clone()),
                    GVal::Str(s) if s.starts_with('/') && s.len() > 1 => {
                        let end = s.rfind('/').unwrap_or(0);
                        (s[1..end].to_string(), s[end + 1..].to_string())
                    }
                    _ => return Err("the right side of =~ must be a /regex/".into()),
                };
                let pre: String = flags.chars().filter(|c| matches!(c, 'i' | 'm' | 's')).collect();
                let src = if pre.is_empty() { re } else { format!("(?{pre}){re}") };
                let rx = Regex::new(&src).map_err(|e| format!("bad regex /{src}/: {e}"))?;
                let m = matches!(&l, GVal::Str(s) if rx.is_match(s));
                Ok(if op == "=~" { m } else { !m })
            }
        }
    }
}

pub fn gl_eval(expr: &str, vars: &dyn Fn(&str) -> Option<String>) -> Result<bool, String> {
    let t = glex(expr)?;
    let mut p = GP { t, i: 0, vars };
    let v = p.or()?;
    if p.i < p.t.len() {
        return Err(format!("unexpected {:?}", p.t[p.i]));
    }
    Ok(v)
}
