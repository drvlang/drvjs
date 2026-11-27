//! Minimal RFC 3986 HTTP(S)/file URL parser and relative resolver.
//! Replaces the `url` crate (and its 35-crate `idna`/`icu_*` transitive tree).
use anyhow::{Result, bail, ensure};
use std::{fmt, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    scheme: String,
    username: String,
    password: Option<String>,
    host: String,
    port: Option<u16>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

impl Url {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        let Some((scheme_raw, rest)) = input.split_once("://") else {
            bail!("URL missing scheme://: {input}");
        };
        ensure!(
            !scheme_raw.is_empty()
                && scheme_raw.as_bytes()[0].is_ascii_alphabetic()
                && scheme_raw
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')),
            "invalid URL scheme: {scheme_raw}"
        );
        let scheme = scheme_raw.to_ascii_lowercase();

        let (rest, fragment) = match rest.split_once('#') {
            Some((before, frag)) => (before, Some(frag.to_owned())),
            None => (rest, None),
        };
        let (rest, query) = match rest.split_once('?') {
            Some((before, q)) => (before, Some(q.to_owned())),
            None => (rest, None),
        };
        let (authority, path_raw) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };

        let (userinfo, hostport) = match authority.rsplit_once('@') {
            Some((ui, hp)) => (Some(ui), hp),
            None => (None, authority),
        };
        let (username, password) = match userinfo {
            Some(ui) => match ui.split_once(':') {
                Some((u, p)) => (u.to_owned(), Some(p.to_owned())),
                None => (ui.to_owned(), None),
            },
            None => (String::new(), None),
        };

        let (host, port) = if scheme == "file" && hostport.is_empty() {
            (String::new(), None)
        } else if let Some(bracketed) = hostport.strip_prefix('[') {
            let Some(end) = bracketed.find(']') else {
                bail!("unterminated IPv6 host in URL: {input}");
            };
            let host = format!("[{}]", bracketed[..end].to_ascii_lowercase());
            let after = &bracketed[end + 1..];
            let port = if let Some(p) = after.strip_prefix(':') {
                if p.is_empty() {
                    None
                } else {
                    Some(p.parse::<u16>().map_err(|_| anyhow::anyhow!("invalid URL port: {p}"))?)
                }
            } else {
                ensure!(after.is_empty(), "invalid host/port in URL: {input}");
                None
            };
            (host, port)
        } else {
            let (h, p) = match hostport.rsplit_once(':') {
                Some((h, p)) => {
                    let port = if p.is_empty() {
                        None
                    } else {
                        Some(p.parse::<u16>().map_err(|_| anyhow::anyhow!("invalid URL port: {p}"))?)
                    };
                    (h, port)
                }
                None => (hostport, None),
            };
            ensure!(
                !h.is_empty()
                    && !h.bytes().any(|b| b <= 0x20 || matches!(b, b'\\' | b'/' | b'?' | b'#')),
                "invalid URL host: {input}"
            );
            (h.to_ascii_lowercase(), p)
        };

        let default_port = match scheme.as_str() {
            "http" => Some(80),
            "https" => Some(443),
            _ => None,
        };
        let port = if port == default_port { None } else { port };
        let path = normalize_path(path_raw);

        Ok(Self {
            scheme,
            username,
            password,
            host,
            port,
            path,
            query,
            fragment,
        })
    }

    pub fn from_file_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        ensure!(path.is_absolute(), "file URL requires an absolute path");
        let bytes = path.as_os_str().as_encoded_bytes();
        let mut encoded = String::with_capacity(bytes.len());
        for &b in bytes {
            if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~') {
                encoded.push(b as char);
            } else {
                encoded.push_str(&format!("%{b:02X}"));
            }
        }
        Ok(Self {
            scheme: "file".into(),
            username: String::new(),
            password: None,
            host: String::new(),
            port: None,
            path: encoded,
            query: None,
            fragment: None,
        })
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    pub fn host_str(&self) -> Option<&str> {
        (!self.host.is_empty()).then_some(self.host.as_str())
    }

    pub fn authority(&self) -> String {
        let mut out = String::new();
        if !self.username.is_empty() || self.password.is_some() {
            out.push_str(&self.username);
            if let Some(p) = &self.password {
                out.push(':');
                out.push_str(p);
            }
            out.push('@');
        }
        out.push_str(&self.host);
        if let Some(port) = self.port {
            out.push(':');
            out.push_str(&port.to_string());
        }
        out
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn path_segments(&self) -> Option<std::str::Split<'_, char>> {
        self.path.strip_prefix('/').map(|rest| rest.split('/'))
    }

    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    pub fn join(&self, input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            return Ok(self.clone());
        }
        if has_scheme(input) {
            return Self::parse(input);
        }
        if let Some(rest) = input.strip_prefix("//") {
            return Self::parse(&format!("{}:{}", self.scheme, format!("//{rest}")));
        }
        let (rest, fragment) = match input.split_once('#') {
            Some((before, frag)) => (before, Some(frag.to_owned())),
            None => (input, None),
        };
        let (rel_path, query) = match rest.split_once('?') {
            Some((before, q)) => (before, Some(q.to_owned())),
            None => (rest, None),
        };
        let (path, query) = if rel_path.is_empty() {
            (self.path.clone(), query.or_else(|| self.query.clone()))
        } else if rel_path.starts_with('/') {
            (normalize_path(rel_path), query)
        } else {
            let base = match self.path.rfind('/') {
                Some(i) => &self.path[..=i],
                None => "/",
            };
            (normalize_path(&format!("{base}{rel_path}")), query)
        };
        Ok(Self {
            scheme: self.scheme.clone(),
            username: self.username.clone(),
            password: self.password.clone(),
            host: self.host.clone(),
            port: self.port,
            path,
            query,
            fragment,
        })
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}{}", self.scheme, self.authority(), self.path)?;
        if let Some(q) = &self.query {
            write!(f, "?{q}")?;
        }
        if let Some(frag) = &self.fragment {
            write!(f, "#{frag}")?;
        }
        Ok(())
    }
}

fn has_scheme(s: &str) -> bool {
    let Some((scheme, _)) = s.split_once("://") else {
        return false;
    };
    !scheme.is_empty()
        && scheme.as_bytes()[0].is_ascii_alphabetic()
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

fn normalize_path(path: &str) -> String {
    let trailing_slash = path.ends_with('/') || path.ends_with("/.") || path.ends_with("/..");
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    if trailing_slash && !out.ends_with('/') {
        out.push('/');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_normalizes_and_joins_urls() {
        let base = Url::parse("https://Registry.NPMJS.org:443/sub/dir/").unwrap();
        assert_eq!(base.to_string(), "https://registry.npmjs.org/sub/dir/");
        assert_eq!(base.authority(), "registry.npmjs.org");
        assert_eq!(
            base.join("../pkg/-/pkg-1.0.0.tgz").unwrap().to_string(),
            "https://registry.npmjs.org/sub/pkg/-/pkg-1.0.0.tgz"
        );
        assert_eq!(
            base.join("https://other.example/a.tgz").unwrap().to_string(),
            "https://other.example/a.tgz"
        );
        let loopback = Url::parse("http://[::1]:8080/a/b?x=1#top").unwrap();
        assert_eq!(loopback.host_str(), Some("[::1]"));
        assert_eq!(loopback.authority(), "[::1]:8080");
        assert_eq!(loopback.query(), Some("x=1"));
        assert_eq!(loopback.fragment(), Some("top"));
        let creds = Url::parse("https://user:pass@example.org/a").unwrap();
        assert_eq!(creds.username(), "user");
        assert_eq!(creds.password(), Some("pass"));
        assert_eq!(
            Url::from_file_path("/tmp/a b/drvjs").unwrap().to_string(),
            "file:///tmp/a%20b/drvjs"
        );
    }
}
