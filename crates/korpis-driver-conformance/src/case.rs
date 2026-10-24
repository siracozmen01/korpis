// SPDX-License-Identifier: Apache-2.0

//! What a case ends in, and the report of a run: a driver conforms when no case failed (§8 of
//! docs/04-runtimes.md). A case the driver's declared capabilities put out of its reach is not
//! applicable, and says which capability; it never passes for a driver that lacks it.

use std::fmt;

/// How a case ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The driver did what the specification says.
    Pass,
    /// It did not: what was seen.
    Fail(String),
    /// The driver declares nothing the case tests: what it does not declare.
    NotApplicable(String),
}

impl Verdict {
    /// A failure saying `what`.
    #[must_use]
    pub fn fail(what: impl fmt::Display) -> Self {
        Self::Fail(what.to_string())
    }

    /// Pass when `held`, otherwise a failure saying `what`.
    #[must_use]
    pub fn check(held: bool, what: impl fmt::Display) -> Self {
        if held { Self::Pass } else { Self::fail(what) }
    }
}

/// One case's verdict, with where the specification says what it tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The case, by name.
    pub case: &'static str,
    /// Where in the specification the behaviour it tests is stated.
    pub section: &'static str,
    /// How it ended.
    pub verdict: Verdict,
}

/// A run of the suite against one driver.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The driver, by the name it describes itself with.
    pub driver: String,
    /// Every case, in the order it ran.
    pub outcomes: Vec<Outcome>,
    /// What §8 of docs/04-runtimes.md requires that the suite does not test yet, each with
    /// where it is stated: a driver's conformance says nothing of it.
    pub untested: Vec<(&'static str, &'static str)>,
}

impl Report {
    /// Records how `case`, testing what `section` says, ended.
    pub fn add(&mut self, (case, section): (&'static str, &'static str), verdict: Verdict) {
        self.outcomes.push(Outcome { case, section, verdict });
    }

    /// Whether the driver conforms: no case failed.
    #[must_use]
    pub fn conforms(&self) -> bool {
        !self.outcomes.iter().any(|o| matches!(o.verdict, Verdict::Fail(_)))
    }

    /// The cases that failed.
    pub fn failures(&self) -> impl Iterator<Item = &Outcome> {
        self.outcomes.iter().filter(|o| matches!(o.verdict, Verdict::Fail(_)))
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "driver {}", self.driver)?;
        for o in &self.outcomes {
            let (mark, why) = match &o.verdict {
                Verdict::Pass => ("pass", ""),
                Verdict::Fail(why) => ("FAIL", why.as_str()),
                Verdict::NotApplicable(why) => ("n/a ", why.as_str()),
            };
            writeln!(f, "  {mark}  {:<52} {}  {why}", o.case, o.section)?;
        }
        for (requirement, section) in &self.untested {
            writeln!(f, "  ----  {requirement:<52} {section}  not tested yet")?;
        }
        let verdict = match (self.conforms(), self.untested.len()) {
            (false, _) => "does not conform".to_owned(),
            (true, 0) => "conforms".to_owned(),
            (true, 1) => "conforms to every case built, and 1 requirement is not tested".to_owned(),
            (true, n) => {
                format!("conforms to every case built, and {n} requirements are not tested")
            }
        };
        write!(f, "{} {verdict}", self.driver)
    }
}
