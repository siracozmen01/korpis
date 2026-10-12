// SPDX-License-Identifier: AGPL-3.0-or-later

//! Derived configuration fields (§5.3 of docs/09-recipes.md): a small arithmetic language over
//! the intent's resources and its other configuration, e.g. `resources.memory * 0.8`.
//!
//! The control plane evaluates it at declaration, with the intent body it is validating, and
//! writes the result into the intent like any other value, so a node never evaluates anything and
//! an old intent keeps the value it was declared with.
//!
//! ```text
//! expr    := term (('+' | '-') term)*
//! term    := factor (('*' | '/') factor)*
//! factor  := number | quantity | path | '-' factor | '(' expr ')' | fn '(' expr ',' expr ')'
//! fn      := 'min' | 'max'
//! path    := 'resources.' ('cpu' | 'memory' | 'disk') ['.reservation' | '.limit']
//!          | 'resources.pids' | 'config.' key
//! ```

use crate::quantity::Quantity;

/// The longest expression accepted, in bytes.
pub const MAX_LEN: usize = 256;
/// The deepest nesting accepted.
const MAX_DEPTH: usize = 16;

/// A resource an expression can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    /// CPU, in millicores.
    Cpu,
    /// Memory, in bytes.
    Memory,
    /// Disk, in bytes.
    Disk,
    /// The process limit.
    Pids,
}

/// Which side of a resource's reservation and limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// The limit if one is set, otherwise the reservation.
    Effective,
    /// The reservation.
    Reservation,
    /// The limit.
    Limit,
}

/// A parsed expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// A literal.
    Number(f64),
    /// A resource of the intent.
    Resource(Resource, Part),
    /// Another configuration field, by key.
    Config(String),
    /// Negation.
    Neg(Box<Expr>),
    /// A binary operation.
    Binary(Op, Box<Expr>, Box<Expr>),
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `min(a, b)`
    Min,
    /// `max(a, b)`
    Max,
}

/// Why an expression is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeriveError {
    /// It does not parse.
    #[error("derived expression: {0}")]
    Parse(String),
    /// It parses and cannot be evaluated with these inputs.
    #[error("derived value: {0}")]
    Eval(String),
}

/// What an expression reads.
pub trait Inputs {
    /// A resource of the intent being declared.
    fn resource(&self, resource: Resource, part: Part) -> f64;
    /// Another field's resolved numeric value.
    fn config(&self, key: &str) -> Option<f64>;
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Ident(String),
    Sym(char),
}

fn parse_err(m: impl Into<String>) -> DeriveError {
    DeriveError::Parse(m.into())
}

fn number_token(word: &str) -> Result<f64, DeriveError> {
    if word.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return word.parse().map_err(|_| parse_err(format!("bad number {word:?}")));
    }
    Ok(Quantity::parse(word).map_err(|e| parse_err(e.to_string()))?.as_f64())
}

fn lex(src: &str) -> Result<Vec<Token>, DeriveError> {
    if src.len() > MAX_LEN {
        return Err(parse_err(format!("longer than {MAX_LEN} bytes")));
    }
    let mut tokens = Vec::new();
    let mut chars = src.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if "+-*/(),".contains(c) {
            tokens.push(Token::Sym(c));
            continue;
        }
        if !(c.is_ascii_alphanumeric() || c == '.' || c == '_') {
            return Err(parse_err(format!("unexpected {c:?}")));
        }
        let mut end = start + c.len_utf8();
        while let Some(&(i, n)) = chars.peek() {
            if !(n.is_ascii_alphanumeric() || n == '.' || n == '_') {
                break;
            }
            end = i + n.len_utf8();
            chars.next();
        }
        let word = src.get(start..end).ok_or_else(|| parse_err("bad slice"))?;
        tokens.push(if c.is_ascii_digit() || c == '.' {
            Token::Number(number_token(word)?)
        } else {
            Token::Ident(word.to_owned())
        });
    }
    Ok(tokens)
}

fn path(word: &str) -> Result<Expr, DeriveError> {
    if let Some(key) = word.strip_prefix("config.") {
        return Ok(Expr::Config(key.to_owned()));
    }
    let mut parts = word.split('.');
    let (Some("resources"), Some(r)) = (parts.next(), parts.next()) else {
        return Err(parse_err(format!("unknown name {word:?}")));
    };
    let resource = match r {
        "cpu" => Resource::Cpu,
        "memory" => Resource::Memory,
        "disk" => Resource::Disk,
        "pids" => Resource::Pids,
        _ => return Err(parse_err(format!("unknown resource {r:?}"))),
    };
    let part = match (parts.next(), parts.next(), resource) {
        (None, _, _) => Part::Effective,
        (Some("reservation"), None, r) if r != Resource::Pids => Part::Reservation,
        (Some("limit"), None, _) => Part::Limit,
        _ => return Err(parse_err(format!("unknown name {word:?}"))),
    };
    Ok(Expr::Resource(resource, part))
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.at).cloned();
        self.at += 1;
        t
    }

    fn expect(&mut self, c: char) -> Result<(), DeriveError> {
        match self.next() {
            Some(Token::Sym(s)) if s == c => Ok(()),
            _ => Err(parse_err(format!("expected {c:?}"))),
        }
    }

    fn binary(
        &mut self,
        ops: [(char, Op); 2],
        next: fn(&mut Self) -> Result<Expr, DeriveError>,
    ) -> Result<Expr, DeriveError> {
        let mut left = next(self)?;
        while let Some(op) =
            ops.iter().find_map(|(c, op)| (self.peek() == Some(&Token::Sym(*c))).then_some(*op))
        {
            self.at += 1;
            left = Expr::Binary(op, Box::new(left), Box::new(next(self)?));
        }
        Ok(left)
    }

    fn expr(&mut self) -> Result<Expr, DeriveError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(parse_err("nested too deeply"));
        }
        let e = self.binary([('+', Op::Add), ('-', Op::Sub)], Self::term);
        self.depth -= 1;
        e
    }

    fn term(&mut self) -> Result<Expr, DeriveError> {
        self.binary([('*', Op::Mul), ('/', Op::Div)], Self::factor)
    }

    fn factor(&mut self) -> Result<Expr, DeriveError> {
        match self.next() {
            Some(Token::Number(n)) => Ok(Expr::Number(n)),
            Some(Token::Sym('-')) => Ok(Expr::Neg(Box::new(self.factor()?))),
            Some(Token::Sym('(')) => {
                let e = self.expr()?;
                self.expect(')')?;
                Ok(e)
            }
            Some(Token::Ident(f)) if f == "min" || f == "max" => {
                self.expect('(')?;
                let a = self.expr()?;
                self.expect(',')?;
                let b = self.expr()?;
                self.expect(')')?;
                let op = if f == "min" { Op::Min } else { Op::Max };
                Ok(Expr::Binary(op, Box::new(a), Box::new(b)))
            }
            Some(Token::Ident(word)) => path(&word),
            _ => Err(parse_err("expected a number, a name, or '('")),
        }
    }
}

impl Expr {
    /// Parses an expression.
    pub fn parse(src: &str) -> Result<Self, DeriveError> {
        let mut p = Parser { tokens: lex(src)?, at: 0, depth: 0 };
        let e = p.expr()?;
        if p.at != p.tokens.len() {
            return Err(parse_err("unexpected input after the expression"));
        }
        Ok(e)
    }

    /// The configuration keys it reads.
    #[must_use]
    pub fn config_keys(&self) -> Vec<&str> {
        match self {
            Self::Config(k) => vec![k.as_str()],
            Self::Neg(e) => e.config_keys(),
            Self::Binary(_, a, b) => {
                let mut keys = a.config_keys();
                keys.extend(b.config_keys());
                keys
            }
            Self::Number(_) | Self::Resource(..) => Vec::new(),
        }
    }

    /// Evaluates it. The result is finite or an error.
    pub fn eval(&self, inputs: &impl Inputs) -> Result<f64, DeriveError> {
        let v = match self {
            Self::Number(n) => *n,
            Self::Resource(r, p) => inputs.resource(*r, *p),
            Self::Config(k) => inputs
                .config(k)
                .ok_or_else(|| DeriveError::Eval(format!("config.{k} has no numeric value")))?,
            Self::Neg(e) => -e.eval(inputs)?,
            Self::Binary(op, a, b) => {
                let (a, b) = (a.eval(inputs)?, b.eval(inputs)?);
                match op {
                    Op::Add => a + b,
                    Op::Sub => a - b,
                    Op::Mul => a * b,
                    Op::Div if b == 0.0 => {
                        return Err(DeriveError::Eval("division by zero".into()));
                    }
                    Op::Div => a / b,
                    Op::Min => a.min(b),
                    Op::Max => a.max(b),
                }
            }
        };
        if v.is_finite() { Ok(v) } else { Err(DeriveError::Eval("not a finite number".into())) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed;

    impl Inputs for Fixed {
        fn resource(&self, r: Resource, p: Part) -> f64 {
            match (r, p) {
                (Resource::Memory, Part::Reservation) => 1024.0,
                (Resource::Memory, _) => 2048.0,
                _ => 0.0,
            }
        }

        fn config(&self, key: &str) -> Option<f64> {
            (key == "players").then_some(20.0)
        }
    }

    fn eval(src: &str) -> Result<f64, DeriveError> {
        Expr::parse(src)?.eval(&Fixed)
    }

    #[test]
    fn arithmetic_over_resources_quantities_and_config() {
        assert_eq!(eval("resources.memory * 0.5"), Ok(1024.0));
        assert_eq!(eval("resources.memory.reservation + 1Ki"), Ok(2048.0));
        assert_eq!(eval("max(config.players * 2, 50) - (10 / 2)"), Ok(45.0));
        assert_eq!(eval("-1 + 2 * 3"), Ok(5.0));
        assert_eq!(eval("min(500m, 1)"), Ok(0.5));
    }

    #[test]
    fn malformed_and_unevaluable_expressions_are_refused() {
        for bad in ["", "resources.gpu", "resources.pids.reservation", "1 +", "(1", "2 3", "foo"] {
            assert!(matches!(eval(bad), Err(DeriveError::Parse(_))), "{bad:?}");
        }
        assert!(matches!(eval("1 / 0"), Err(DeriveError::Eval(_))));
        assert!(matches!(eval("config.missing"), Err(DeriveError::Eval(_))));
        assert!(Expr::parse(&"(".repeat(40)).is_err());
        assert!(Expr::parse(&"1+".repeat(200)).is_err());
    }
}
