//! Minimal npm-compatible SemVer parser and range matcher.
//! Replaces `node-semver` (and its 66-crate `miette`/`gimli`/`nom` tree).
use anyhow::{Result, bail, ensure};
use std::cmp::Ordering;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreId {
    Numeric(u64),
    Alpha(String),
}

impl Ord for PreId {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (PreId::Numeric(a), PreId::Numeric(b)) => a.cmp(b),
            (PreId::Numeric(_), PreId::Alpha(_)) => Ordering::Less,
            (PreId::Alpha(_), PreId::Numeric(_)) => Ordering::Greater,
            (PreId::Alpha(a), PreId::Alpha(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for PreId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<PreId>,
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Version {
    pub fn parse(input: &str) -> Result<Self> {
        let s = input.trim().trim_start_matches(['=', 'v', 'V']);
        let s = s.split('+').next().unwrap();
        let (core, pre_str) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut parts = core.split('.');
        let major = parse_num(parts.next().ok_or_else(|| anyhow::anyhow!("invalid semver"))?)?;
        let minor = parse_num(parts.next().ok_or_else(|| anyhow::anyhow!("invalid semver"))?)?;
        let patch = parse_num(parts.next().ok_or_else(|| anyhow::anyhow!("invalid semver"))?)?;
        ensure!(parts.next().is_none(), "invalid semver: {input}");
        let pre = match pre_str {
            Some(p) => parse_pre(p)?,
            None => Vec::new(),
        };
        Ok(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    fn zero_pre(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
            pre: vec![PreId::Numeric(0)],
        }
    }

    fn plain(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
            pre: Vec::new(),
        }
    }

    fn tuple(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }
}

fn parse_num(s: &str) -> Result<u64> {
    ensure!(
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()),
        "invalid numeric version component: {s}"
    );
    Ok(s.parse()?)
}

fn parse_pre(s: &str) -> Result<Vec<PreId>> {
    ensure!(!s.is_empty(), "empty prerelease");
    s.split('.')
        .map(|part| {
            ensure!(
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                "invalid prerelease identifier: {part}"
            );
            if part.bytes().all(|b| b.is_ascii_digit()) {
                Ok(PreId::Numeric(part.parse()?))
            } else {
                Ok(PreId::Alpha(part.to_owned()))
            }
        })
        .collect()
}

#[derive(Clone, Debug)]
enum Op {
    Eq,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Clone, Debug)]
struct Comparator {
    op: Op,
    version: Version,
}

impl Comparator {
    fn matches(&self, v: &Version) -> bool {
        match self.op {
            Op::Eq => v == &self.version,
            Op::Gt => v > &self.version,
            Op::Gte => v >= &self.version,
            Op::Lt => v < &self.version,
            Op::Lte => v <= &self.version,
        }
    }
}

#[derive(Clone, Debug)]
struct ComparatorSet(Vec<Comparator>);

impl ComparatorSet {
    fn satisfies(&self, v: &Version) -> bool {
        if !self.0.iter().all(|c| c.matches(v)) {
            return false;
        }
        if v.pre.is_empty() {
            return true;
        }
        // npm node-semver prerelease rule: a prerelease version matches a set
        // only if at least one comparator in the set has a prerelease tag on
        // the same [major, minor, patch] tuple.
        self.0
            .iter()
            .any(|c| !c.version.pre.is_empty() && c.version.tuple() == v.tuple())
    }

    fn bounds(&self) -> Option<(Bound, Bound)> {
        let mut lo = Bound::Unbounded;
        let mut hi = Bound::Unbounded;
        for c in &self.0 {
            match c.op {
                Op::Eq => {
                    lo = lo.max_lower(Bound::Included(c.version.clone()));
                    hi = hi.min_upper(Bound::Included(c.version.clone()));
                }
                Op::Gte => lo = lo.max_lower(Bound::Included(c.version.clone())),
                Op::Gt => lo = lo.max_lower(Bound::Excluded(c.version.clone())),
                Op::Lte => hi = hi.min_upper(Bound::Included(c.version.clone())),
                Op::Lt => hi = hi.min_upper(Bound::Excluded(c.version.clone())),
            }
        }
        if bounds_non_empty(&lo, &hi) {
            Some((lo, hi))
        } else {
            None
        }
    }
}

#[derive(Clone, Debug)]
enum Bound {
    Unbounded,
    Included(Version),
    Excluded(Version),
}

impl Bound {
    fn max_lower(self, other: Bound) -> Bound {
        match (&self, &other) {
            (Bound::Unbounded, _) => other,
            (_, Bound::Unbounded) => self,
            (Bound::Included(a), Bound::Included(b)) => {
                if a >= b { self } else { other }
            }
            (Bound::Excluded(a), Bound::Excluded(b)) => {
                if a >= b { self } else { other }
            }
            (Bound::Included(a), Bound::Excluded(b)) => {
                if a > b { self } else { other }
            }
            (Bound::Excluded(a), Bound::Included(b)) => {
                if a >= b { self } else { other }
            }
        }
    }

    fn min_upper(self, other: Bound) -> Bound {
        match (&self, &other) {
            (Bound::Unbounded, _) => other,
            (_, Bound::Unbounded) => self,
            (Bound::Included(a), Bound::Included(b)) => {
                if a <= b { self } else { other }
            }
            (Bound::Excluded(a), Bound::Excluded(b)) => {
                if a <= b { self } else { other }
            }
            (Bound::Included(a), Bound::Excluded(b)) => {
                if a < b { self } else { other }
            }
            (Bound::Excluded(a), Bound::Included(b)) => {
                if a <= b { self } else { other }
            }
        }
    }
}

fn bounds_non_empty(lo: &Bound, hi: &Bound) -> bool {
    match (lo, hi) {
        (Bound::Unbounded, _) | (_, Bound::Unbounded) => true,
        (Bound::Included(a), Bound::Included(b)) => a <= b,
        (Bound::Included(a), Bound::Excluded(b))
        | (Bound::Excluded(a), Bound::Included(b))
        | (Bound::Excluded(a), Bound::Excluded(b)) => a < b,
    }
}

#[derive(Clone, Debug)]
pub struct Range(Vec<ComparatorSet>);

impl Range {
    pub fn parse(input: &str) -> Result<Self> {
        let mut sets = Vec::new();
        for part in input.split("||") {
            sets.push(parse_set(part.trim())?);
        }
        Ok(Self(sets))
    }

    pub fn satisfies(&self, v: &Version) -> bool {
        self.0.iter().any(|set| set.satisfies(v))
    }

    pub fn allows_any(&self, other: &Range) -> bool {
        for a in &self.0 {
            let Some((alo, ahi)) = a.bounds() else {
                continue;
            };
            for b in &other.0 {
                let Some((blo, bhi)) = b.bounds() else {
                    continue;
                };
                let lo = alo.clone().max_lower(blo);
                let hi = ahi.clone().min_upper(bhi);
                if bounds_non_empty(&lo, &hi) {
                    return true;
                }
            }
        }
        false
    }
}

fn is_wild(s: &str) -> bool {
    matches!(s, "*" | "x" | "X")
}

struct Partial {
    major: Option<u64>,
    minor: Option<u64>,
    patch: Option<u64>,
    pre: Vec<PreId>,
}

fn parse_partial(input: &str) -> Result<Partial> {
    let s = input.trim().trim_start_matches(['v', 'V']);
    let s = s.split('+').next().unwrap();
    if s.is_empty() || is_wild(s) {
        return Ok(Partial {
            major: None,
            minor: None,
            patch: None,
            pre: Vec::new(),
        });
    }
    let (core, pre_str) = match s.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (s, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    ensure!(!parts.is_empty() && parts.len() <= 3, "invalid partial version: {input}");
    let parse_opt = |p: &str| -> Result<Option<u64>> {
        if is_wild(p) {
            Ok(None)
        } else {
            Ok(Some(parse_num(p)?))
        }
    };
    let major = parse_opt(parts[0])?;
    let minor = if parts.len() >= 2 { parse_opt(parts[1])? } else { None };
    let patch = if parts.len() >= 3 { parse_opt(parts[2])? } else { None };
    let pre = match pre_str {
        Some(p) if major.is_some() && minor.is_some() && patch.is_some() => parse_pre(p)?,
        Some(_) => bail!("prerelease on partial version: {input}"),
        None => Vec::new(),
    };
    Ok(Partial {
        major,
        minor,
        patch,
        pre,
    })
}

fn parse_set(input: &str) -> Result<ComparatorSet> {
    if input.is_empty() || is_wild(input) {
        return Ok(ComparatorSet(vec![Comparator {
            op: Op::Gte,
            version: Version::plain(0, 0, 0),
        }]));
    }
    if let Some((left, right)) = input.split_once(" - ") {
        let mut comps = Vec::new();
        let lo = parse_partial(left)?;
        if let Some(maj) = lo.major {
            comps.push(Comparator {
                op: Op::Gte,
                version: Version {
                    major: maj,
                    minor: lo.minor.unwrap_or(0),
                    patch: lo.patch.unwrap_or(0),
                    pre: lo.pre,
                },
            });
        }
        let hi = parse_partial(right)?;
        match (hi.major, hi.minor, hi.patch) {
            (None, _, _) => {}
            (Some(maj), None, _) => comps.push(Comparator {
                op: Op::Lt,
                version: Version::zero_pre(maj + 1, 0, 0),
            }),
            (Some(maj), Some(min), None) => comps.push(Comparator {
                op: Op::Lt,
                version: Version::zero_pre(maj, min + 1, 0),
            }),
            (Some(maj), Some(min), Some(pat)) => comps.push(Comparator {
                op: Op::Lte,
                version: Version {
                    major: maj,
                    minor: min,
                    patch: pat,
                    pre: hi.pre,
                },
            }),
        }
        return Ok(ComparatorSet(comps));
    }

    // Normalize whitespace after operators: ">= 1.0.0" -> ">=1.0.0"
    let raw_tokens: Vec<&str> = input.split_whitespace().collect();
    let mut tokens: Vec<String> = Vec::with_capacity(raw_tokens.len());
    let mut i = 0;
    while i < raw_tokens.len() {
        let tok = raw_tokens[i];
        if matches!(tok, "=" | ">" | ">=" | "<" | "<=" | "^" | "~" | "~>") && i + 1 < raw_tokens.len() {
            tokens.push(format!("{tok}{}", raw_tokens[i + 1]));
            i += 2;
        } else {
            tokens.push(tok.to_owned());
            i += 1;
        }
    }

    let mut comps = Vec::new();
    for tok in &tokens {
        parse_token(tok, &mut comps)?;
    }
    Ok(ComparatorSet(comps))
}

fn parse_token(tok: &str, out: &mut Vec<Comparator>) -> Result<()> {
    if let Some(rest) = tok.strip_prefix("^") {
        let p = parse_partial(rest)?;
        match (p.major, p.minor, p.patch) {
            (None, _, _) => out.push(Comparator {
                op: Op::Gte,
                version: Version::plain(0, 0, 0),
            }),
            (Some(0), None, _) => {
                out.push(Comparator { op: Op::Gte, version: Version::plain(0, 0, 0) });
                out.push(Comparator { op: Op::Lt, version: Version::zero_pre(1, 0, 0) });
            }
            (Some(0), Some(0), None) => {
                out.push(Comparator { op: Op::Gte, version: Version::plain(0, 0, 0) });
                out.push(Comparator { op: Op::Lt, version: Version::zero_pre(0, 1, 0) });
            }
            (Some(0), Some(0), Some(pat)) => {
                out.push(Comparator {
                    op: Op::Gte,
                    version: Version { major: 0, minor: 0, patch: pat, pre: p.pre },
                });
                out.push(Comparator { op: Op::Lt, version: Version::zero_pre(0, 0, pat + 1) });
            }
            (Some(0), Some(min), pat) => {
                out.push(Comparator {
                    op: Op::Gte,
                    version: Version { major: 0, minor: min, patch: pat.unwrap_or(0), pre: p.pre },
                });
                out.push(Comparator { op: Op::Lt, version: Version::zero_pre(0, min + 1, 0) });
            }
            (Some(maj), min, pat) => {
                out.push(Comparator {
                    op: Op::Gte,
                    version: Version {
                        major: maj,
                        minor: min.unwrap_or(0),
                        patch: pat.unwrap_or(0),
                        pre: p.pre,
                    },
                });
                out.push(Comparator { op: Op::Lt, version: Version::zero_pre(maj + 1, 0, 0) });
            }
        }
        return Ok(());
    }
    if let Some(rest) = tok.strip_prefix("~>").or_else(|| tok.strip_prefix('~')) {
        let p = parse_partial(rest)?;
        match (p.major, p.minor, p.patch) {
            (None, _, _) => out.push(Comparator {
                op: Op::Gte,
                version: Version::plain(0, 0, 0),
            }),
            (Some(maj), None, _) => {
                out.push(Comparator { op: Op::Gte, version: Version::plain(maj, 0, 0) });
                out.push(Comparator { op: Op::Lt, version: Version::zero_pre(maj + 1, 0, 0) });
            }
            (Some(maj), Some(min), pat) => {
                out.push(Comparator {
                    op: Op::Gte,
                    version: Version {
                        major: maj,
                        minor: min,
                        patch: pat.unwrap_or(0),
                        pre: p.pre,
                    },
                });
                out.push(Comparator { op: Op::Lt, version: Version::zero_pre(maj, min + 1, 0) });
            }
        }
        return Ok(());
    }
    let (op, rest) = if let Some(r) = tok.strip_prefix(">=") {
        (Some(Op::Gte), r)
    } else if let Some(r) = tok.strip_prefix("<=") {
        (Some(Op::Lte), r)
    } else if let Some(r) = tok.strip_prefix('>') {
        (Some(Op::Gt), r)
    } else if let Some(r) = tok.strip_prefix('<') {
        (Some(Op::Lt), r)
    } else if let Some(r) = tok.strip_prefix('=') {
        (Some(Op::Eq), r)
    } else {
        (None, tok)
    };
    let p = parse_partial(rest)?;
    match op {
        None | Some(Op::Eq) => match (p.major, p.minor, p.patch) {
            (None, _, _) => out.push(Comparator {
                op: Op::Gte,
                version: Version::plain(0, 0, 0),
            }),
            (Some(maj), None, _) => {
                out.push(Comparator { op: Op::Gte, version: Version::plain(maj, 0, 0) });
                out.push(Comparator { op: Op::Lt, version: Version::zero_pre(maj + 1, 0, 0) });
            }
            (Some(maj), Some(min), None) => {
                out.push(Comparator { op: Op::Gte, version: Version::plain(maj, min, 0) });
                out.push(Comparator { op: Op::Lt, version: Version::zero_pre(maj, min + 1, 0) });
            }
            (Some(maj), Some(min), Some(pat)) => out.push(Comparator {
                op: Op::Eq,
                version: Version {
                    major: maj,
                    minor: min,
                    patch: pat,
                    pre: p.pre,
                },
            }),
        },
        Some(Op::Gt) => match (p.major, p.minor, p.patch) {
            (None, _, _) => out.push(Comparator { op: Op::Lt, version: Version::zero_pre(0, 0, 0) }),
            (Some(maj), None, _) => out.push(Comparator { op: Op::Gte, version: Version::plain(maj + 1, 0, 0) }),
            (Some(maj), Some(min), None) => out.push(Comparator { op: Op::Gte, version: Version::plain(maj, min + 1, 0) }),
            (Some(maj), Some(min), Some(pat)) => out.push(Comparator {
                op: Op::Gt,
                version: Version { major: maj, minor: min, patch: pat, pre: p.pre },
            }),
        },
        Some(Op::Gte) => {
            let v = match (p.major, p.minor, p.patch) {
                (None, _, _) => Version::plain(0, 0, 0),
                (Some(maj), min, pat) => Version {
                    major: maj,
                    minor: min.unwrap_or(0),
                    patch: pat.unwrap_or(0),
                    pre: p.pre,
                },
            };
            out.push(Comparator { op: Op::Gte, version: v });
        }
        Some(Op::Lt) => match (p.major, p.minor, p.patch) {
            (None, _, _) => out.push(Comparator { op: Op::Lt, version: Version::zero_pre(0, 0, 0) }),
            (Some(maj), None, _) => out.push(Comparator { op: Op::Lt, version: Version::zero_pre(maj, 0, 0) }),
            (Some(maj), Some(min), None) => out.push(Comparator { op: Op::Lt, version: Version::zero_pre(maj, min, 0) }),
            (Some(maj), Some(min), Some(pat)) => out.push(Comparator {
                op: Op::Lt,
                version: Version { major: maj, minor: min, patch: pat, pre: p.pre },
            }),
        },
        Some(Op::Lte) => match (p.major, p.minor, p.patch) {
            (None, _, _) => out.push(Comparator { op: Op::Gte, version: Version::plain(0, 0, 0) }),
            (Some(maj), None, _) => out.push(Comparator { op: Op::Lt, version: Version::zero_pre(maj + 1, 0, 0) }),
            (Some(maj), Some(min), None) => out.push(Comparator { op: Op::Lt, version: Version::zero_pre(maj, min + 1, 0) }),
            (Some(maj), Some(min), Some(pat)) => out.push(Comparator {
                op: Op::Lte,
                version: Version { major: maj, minor: min, patch: pat, pre: p.pre },
            }),
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_ordering_and_ranges() {
        let v1 = Version::parse("1.0.0-alpha.1").unwrap();
        let v2 = Version::parse("1.0.0-alpha.2").unwrap();
        let v3 = Version::parse("1.0.0-beta").unwrap();
        let v4 = Version::parse("1.0.0").unwrap();
        assert!(v1 < v2 && v2 < v3 && v3 < v4);

        assert!(Range::parse("^1.2.3").unwrap().satisfies(&Version::parse("1.9.0").unwrap()));
        assert!(!Range::parse("^1.2.3").unwrap().satisfies(&Version::parse("2.0.0").unwrap()));
        assert!(Range::parse("^0.2.3").unwrap().satisfies(&Version::parse("0.2.9").unwrap()));
        assert!(!Range::parse("^0.2.3").unwrap().satisfies(&Version::parse("0.3.0").unwrap()));
        assert!(Range::parse("~1.2.3").unwrap().satisfies(&Version::parse("1.2.9").unwrap()));
        assert!(!Range::parse("~1.2.3").unwrap().satisfies(&Version::parse("1.3.0").unwrap()));
        assert!(Range::parse("1.x || >=2.5.0 <3.0.0").unwrap().satisfies(&Version::parse("2.7.0").unwrap()));
        assert!(Range::parse("1.0.0 - 2.0.0").unwrap().satisfies(&Version::parse("2.0.0").unwrap()));
        assert!(Range::parse("^1").unwrap().allows_any(&Range::parse("^1.2.0").unwrap()));
        assert!(!Range::parse("^1").unwrap().allows_any(&Range::parse("^2.0.0").unwrap()));
        assert!(Range::parse("^1.2.3-alpha.1").unwrap().satisfies(&Version::parse("1.2.3-alpha.2").unwrap()));
        assert!(!Range::parse("^1.2.3").unwrap().satisfies(&Version::parse("1.2.4-alpha.1").unwrap()));
    }
}
