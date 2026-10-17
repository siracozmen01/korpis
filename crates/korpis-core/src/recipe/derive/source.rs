// SPDX-License-Identifier: AGPL-3.0-or-later

//! An expression written back out in the language it was parsed from.

use super::{Expr, Op, Part, Resource};

/// The expression written back out in the language it was parsed from, every operation in
/// parentheses and every quantity as the number it denotes, so it reads back as the same
/// expression: what a client shows for a derived field.
impl std::fmt::Display for Expr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(n) => write!(f, "{n}"),
            Self::Resource(r, part) => {
                let name = match r {
                    Resource::Cpu => "cpu",
                    Resource::Memory => "memory",
                    Resource::Disk => "disk",
                    Resource::Pids => "pids",
                };
                let suffix = match part {
                    Part::Effective => "",
                    Part::Reservation => ".reservation",
                    Part::Limit => ".limit",
                };
                write!(f, "resources.{name}{suffix}")
            }
            Self::Config(key) => write!(f, "config.{key}"),
            Self::Neg(e) => write!(f, "-({e})"),
            Self::Binary(Op::Min, a, b) => write!(f, "min({a}, {b})"),
            Self::Binary(Op::Max, a, b) => write!(f, "max({a}, {b})"),
            Self::Binary(op, a, b) => {
                let sign = match op {
                    Op::Add => '+',
                    Op::Sub => '-',
                    Op::Mul => '*',
                    _ => '/',
                };
                write!(f, "({a} {sign} {b})")
            }
        }
    }
}
