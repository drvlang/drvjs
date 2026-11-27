//! Minimal Nix ATerm derivation builder and protocol-1.21 Unix-socket daemon client.
use crate::hash::Sha256;
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    io::{BufReader, BufWriter, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    sync::{Arc, Mutex},
    thread,
};

const NIX_BASE32_CHARS: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";
const CLIENT_MAGIC: u64 = 0x6e69_7863;
const SERVER_MAGIC: u64 = 0x6478_696f;
const PROTOCOL_1_21: u64 = (1 << 8) | 21;
const STDERR_NEXT: u64 = 0x6f6c_6d67;
const STDERR_LAST: u64 = 0x616c_7473;
const STDERR_ERROR: u64 = 0x6378_7470;
const WOP_ADD_TEXT_TO_STORE: u64 = 8;
const WOP_QUERY_VALID_PATHS: u64 = 31;

pub fn store_dir() -> String {
    match std::env::var("NIX_STORE") {
        Ok(dir) if dir.starts_with('/') => {
            let trimmed = dir.trim_end_matches('/');
            if trimmed.is_empty() {
                "/".into()
            } else {
                trimmed.into()
            }
        }
        _ => "/nix/store".into(),
    }
}

pub fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

pub fn nix_base32(data: &[u8]) -> String {
    let out_len = (data.len() * 8 - 1) / 5 + 1;
    let mut out = vec![0u8; out_len];
    for (n, slot) in out.iter_mut().enumerate() {
        let b = (out_len - 1 - n) * 5;
        let i = b / 8;
        let j = b % 8;
        let mut c = data[i] >> j;
        if j > 0 && i + 1 < data.len() {
            c |= data[i + 1] << (8 - j);
        }
        *slot = NIX_BASE32_CHARS[(c & 0x1f) as usize];
    }
    String::from_utf8(out).unwrap()
}

fn compress_hash(hash: &[u8], size: usize) -> Vec<u8> {
    let mut out = vec![0u8; size];
    for (i, &b) in hash.iter().enumerate() {
        out[i % size] ^= b;
    }
    out
}

pub fn make_store_path(store_dir: &str, typ: &str, sha256_hex: &str, name: &str) -> String {
    let fingerprint = format!("{typ}:sha256:{sha256_hex}:{store_dir}:{name}");
    let sum = Sha256::digest(fingerprint.as_bytes());
    let hash = nix_base32(&compress_hash(&sum, 20));
    format!("{store_dir}/{hash}-{name}")
}

pub fn make_text_path(store_dir: &str, name: &str, text: &[u8], refs: &[String]) -> String {
    let sum = Sha256::digest(text);
    let sorted: BTreeSet<&str> = refs.iter().map(String::as_str).collect();
    let mut typ = String::from("text");
    for r in sorted {
        typ.push(':');
        typ.push_str(r);
    }
    make_store_path(store_dir, &typ, &hex_lower(&sum), name)
}

pub fn make_fixed_output_path(
    store_dir: &str,
    name: &str,
    recursive: bool,
    algo: &str,
    digest_hex: &str,
) -> String {
    if recursive && algo == "sha256" {
        return make_store_path(store_dir, "source", digest_hex, name);
    }
    let prefix = if recursive { "r:" } else { "" };
    let inner = Sha256::digest(format!("fixed:out:{prefix}{algo}:{digest_hex}:").as_bytes());
    make_store_path(store_dir, "output:out", &hex_lower(&inner), name)
}

pub fn hash_placeholder(output_name: &str) -> String {
    let sum = Sha256::digest(format!("nix-output:{output_name}").as_bytes());
    format!("/{}", nix_base32(&sum))
}

pub fn downstream_placeholder(drv_path: &str, output_name: &str) -> String {
    let base = Path::new(drv_path)
        .file_name()
        .and_then(|s| s.to_str())
        .expect("invalid drv path");
    let hash_part = &base[..32];
    let drv_name = base[33..].strip_suffix(".drv").expect("expected .drv");
    let out_path_name = if output_name == "out" {
        drv_name.to_owned()
    } else {
        format!("{drv_name}-{output_name}")
    };
    let sum = Sha256::digest(format!("nix-upstream-output:{hash_part}:{out_path_name}").as_bytes());
    format!("/{}", nix_base32(&sum))
}

#[derive(Clone, Debug)]
pub struct DerivationOutput {
    pub name: String,
    pub path: String,
    pub hash_algo: String,
    pub hash: String,
}

#[derive(Clone, Debug)]
pub struct Derivation {
    pub outputs: Vec<DerivationOutput>,
    pub input_drvs: BTreeMap<String, BTreeSet<String>>,
    pub input_srcs: BTreeSet<String>,
    pub platform: String,
    pub builder: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

impl Derivation {
    pub fn format(&self) -> String {
        let mut out = String::with_capacity(1024);
        out.push_str("Derive([");
        let mut outputs = self.outputs.clone();
        outputs.sort_by(|a, b| a.name.cmp(&b.name));
        for (i, o) in outputs.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('(');
            write_aterm_str(&mut out, &o.name);
            out.push(',');
            write_aterm_str(&mut out, &o.path);
            out.push(',');
            write_aterm_str(&mut out, &o.hash_algo);
            out.push(',');
            write_aterm_str(&mut out, &o.hash);
            out.push(')');
        }
        out.push_str("],[");
        for (i, (drv, outs)) in self.input_drvs.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('(');
            write_aterm_str(&mut out, drv);
            out.push_str(",[");
            for (j, o) in outs.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                write_aterm_str(&mut out, o);
            }
            out.push_str("])");
        }
        out.push_str("],[");
        for (i, src) in self.input_srcs.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            write_aterm_str(&mut out, src);
        }
        out.push_str("],");
        write_aterm_str(&mut out, &self.platform);
        out.push(',');
        write_aterm_str(&mut out, &self.builder);
        out.push_str(",[");
        for (i, arg) in self.args.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            write_aterm_str(&mut out, arg);
        }
        out.push_str("],[");
        for (i, (k, v)) in self.env.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('(');
            write_aterm_str(&mut out, k);
            out.push(',');
            write_aterm_str(&mut out, v);
            out.push(')');
        }
        out.push_str("])");
        out
    }

    pub fn references(&self) -> Vec<String> {
        let mut refs: BTreeSet<String> = self.input_srcs.clone();
        refs.extend(self.input_drvs.keys().cloned());
        refs.into_iter().collect()
    }

    pub fn path(&self, store_dir: &str, aterm: &[u8]) -> String {
        let name = format!("{}.drv", self.env["name"]);
        make_text_path(store_dir, &name, aterm, &self.references())
    }
}

fn write_aterm_str(out: &mut String, s: &str) {
    out.push('"');
    for b in s.bytes() {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            _ => out.push(b as char),
        }
    }
    out.push('"');
}

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
}

impl Client {
    pub fn open(remote: &str) -> Result<Self> {
        let socket = remote
            .strip_prefix("unix://")
            .filter(|p| p.starts_with('/') && !p.contains(['?', '%', '#']))
            .with_context(|| format!("expected plain Nix unix:// socket, got {remote:?}"))?;
        let stream =
            UnixStream::connect(socket).with_context(|| format!("connecting to {socket}"))?;
        let reader = BufReader::new(stream.try_clone()?);
        let writer = BufWriter::with_capacity(64 * 1024, stream);
        let mut client = Self { reader, writer };
        client.handshake()?;
        Ok(client)
    }

    fn write_u64(&mut self, n: u64) -> Result<()> {
        self.writer.write_all(&n.to_le_bytes())?;
        Ok(())
    }

    fn write_bytes(&mut self, b: &[u8]) -> Result<()> {
        self.write_u64(b.len() as u64)?;
        self.writer.write_all(b)?;
        let pad = (8 - (b.len() & 7)) & 7;
        if pad != 0 {
            self.writer.write_all(&[0u8; 8][..pad])?;
        }
        Ok(())
    }

    fn write_str(&mut self, s: &str) -> Result<()> {
        self.write_bytes(s.as_bytes())
    }

    fn read_u64(&mut self) -> Result<u64> {
        let mut buf = [0u8; 8];
        self.reader.read_exact(&mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn read_string(&mut self) -> Result<String> {
        let n = self.read_u64()?;
        ensure!(n <= (32 << 20), "Nix daemon string exceeds 32 MiB");
        let n = n as usize;
        let pad = (8 - (n & 7)) & 7;
        let mut buf = vec![0u8; n + pad];
        self.reader.read_exact(&mut buf)?;
        ensure!(
            buf[n..].iter().all(|&b| b == 0),
            "invalid Nix daemon string padding"
        );
        buf.truncate(n);
        Ok(String::from_utf8(buf)?)
    }

    fn process_stderr(&mut self) -> Result<()> {
        loop {
            match self.read_u64()? {
                STDERR_LAST => return Ok(()),
                STDERR_NEXT => {
                    let _ = self.read_string()?;
                }
                STDERR_ERROR => {
                    let msg = self.read_string()?;
                    let status = self.read_u64()?;
                    bail!("Nix daemon: {msg} (status {status})");
                }
                tag => bail!("unexpected Nix daemon message: {tag:#x}"),
            }
        }
    }

    fn handshake(&mut self) -> Result<()> {
        self.write_u64(CLIENT_MAGIC)?;
        self.write_u64(PROTOCOL_1_21)?;
        self.writer.flush()?;
        let magic = self.read_u64()?;
        ensure!(magic == SERVER_MAGIC, "invalid Nix daemon greeting: {magic:#x}");
        let server = self.read_u64()?;
        ensure!(
            (server >> 8) == 1 && server >= PROTOCOL_1_21,
            "unsupported Nix daemon protocol: {server:#x}"
        );
        self.write_u64(0)?; // obsolete CPU affinity
        self.write_u64(0)?; // obsolete reserve space
        self.writer.flush()?;
        self.process_stderr()
    }

    pub fn valid_paths(&mut self, paths: &[String]) -> Result<HashSet<String>> {
        if paths.is_empty() {
            return Ok(HashSet::new());
        }
        self.write_u64(WOP_QUERY_VALID_PATHS)?;
        self.write_u64(paths.len() as u64)?;
        let requested: HashSet<&str> = paths.iter().map(String::as_str).collect();
        for p in paths {
            self.write_str(p)?;
        }
        self.writer.flush()?;
        self.process_stderr()?;
        let count = self.read_u64()? as usize;
        ensure!(count <= requested.len(), "daemon returned too many valid paths");
        let mut valid = HashSet::with_capacity(count);
        for _ in 0..count {
            let p = self.read_string()?;
            ensure!(
                requested.contains(p.as_str()) && valid.insert(p.clone()),
                "unexpected valid path from Nix daemon: {p:?}"
            );
        }
        Ok(valid)
    }

    pub fn add_text(&mut self, name: &str, content: &[u8], refs: &[String]) -> Result<String> {
        let sorted: BTreeSet<&str> = refs.iter().map(String::as_str).collect();
        self.write_u64(WOP_ADD_TEXT_TO_STORE)?;
        self.write_str(name)?;
        self.write_bytes(content)?;
        self.write_u64(sorted.len() as u64)?;
        for r in sorted {
            self.write_str(r)?;
        }
        self.writer.flush()?;
        self.process_stderr()?;
        self.read_string()
    }
}

pub struct TextItem {
    pub store_path: String,
    pub name: String,
    pub content: Vec<u8>,
    pub refs: Vec<String>,
}

/// Query validity once for all items, then register missing items in
/// topological order over a bounded pool of Nix daemon connections.
pub fn register_text_graph(remote: &str, items: Vec<TextItem>) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let want: Vec<String> = items.iter().map(|it| it.store_path.clone()).collect();
    let mut first = Client::open(remote)?;
    let valid = first.valid_paths(&want)?;
    let missing: Vec<TextItem> = items
        .into_iter()
        .filter(|it| !valid.contains(&it.store_path))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }

    // Build dependency graph among missing items.
    let total = missing.len();
    let by_path: HashMap<&str, usize> = missing
        .iter()
        .enumerate()
        .map(|(i, it)| (it.store_path.as_str(), i))
        .collect();
    let mut in_degree = vec![0usize; total];
    let mut dependents = vec![Vec::new(); total];
    for (i, it) in missing.iter().enumerate() {
        let mut seen = HashSet::new();
        for r in &it.refs {
            if let Some(&dep) = by_path.get(r.as_str())
                && seen.insert(dep)
            {
                in_degree[i] += 1;
                dependents[dep].push(i);
            }
        }
    }

    // Topological check.
    let mut check_deg = in_degree.clone();
    let mut order = Vec::with_capacity(total);
    for (i, &deg) in check_deg.iter().enumerate() {
        if deg == 0 {
            order.push(i);
        }
    }
    let mut head = 0;
    while head < order.len() {
        let idx = order[head];
        head += 1;
        for &dep in &dependents[idx] {
            check_deg[dep] -= 1;
            if check_deg[dep] == 0 {
                order.push(dep);
            }
        }
    }
    ensure!(
        order.len() == total,
        "reference cycle among {} store items",
        total - order.len()
    );

    let max_workers = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(4)
        .min(total);
    if max_workers <= 1 || total <= 2 {
        for idx in order {
            let it = &missing[idx];
            let got = first.add_text(&it.name, &it.content, &it.refs)?;
            ensure!(
                got == it.store_path,
                "store path mismatch for {}: got {}, computed {}",
                it.name,
                got,
                it.store_path
            );
        }
        return Ok(());
    }

    let mut clients = Vec::with_capacity(max_workers);
    clients.push(first);
    for _ in 1..max_workers {
        clients.push(Client::open(remote)?);
    }

    struct State {
        ready: VecDeque<usize>,
        in_degree: Vec<usize>,
        remaining: usize,
        error: Option<anyhow::Error>,
    }

    let mut initial = VecDeque::new();
    for (i, &deg) in in_degree.iter().enumerate() {
        if deg == 0 {
            initial.push_back(i);
        }
    }
    let state = Arc::new((
        Mutex::new(State {
            ready: initial,
            in_degree,
            remaining: total,
            error: None,
        }),
        std::sync::Condvar::new(),
    ));

    let missing_ref = &missing;
    let dependents_ref = &dependents;
    thread::scope(|s| {
        for mut client in clients {
            let state = Arc::clone(&state);
            s.spawn(move || {
                loop {
                    let idx = {
                        let (lock, cvar) = &*state;
                        let mut st = lock.lock().unwrap();
                        loop {
                            if st.error.is_some() || st.remaining == 0 {
                                return;
                            }
                            if let Some(idx) = st.ready.pop_front() {
                                break idx;
                            }
                            st = cvar.wait(st).unwrap();
                        }
                    };
                    let it = &missing_ref[idx];
                    let res = client.add_text(&it.name, &it.content, &it.refs).and_then(|got| {
                        ensure!(
                            got == it.store_path,
                            "store path mismatch for {}: got {}, computed {}",
                            it.name,
                            got,
                            it.store_path
                        );
                        Ok(())
                    });
                    let (lock, cvar) = &*state;
                    let mut st = lock.lock().unwrap();
                    match res {
                        Ok(()) => {
                            st.remaining -= 1;
                            for &dep in &dependents_ref[idx] {
                                st.in_degree[dep] -= 1;
                                if st.in_degree[dep] == 0 {
                                    st.ready.push_back(dep);
                                }
                            }
                        }
                        Err(err) => {
                            if st.error.is_none() {
                                st.error = Some(err);
                            }
                        }
                    }
                    cvar.notify_all();
                }
            });
        }
    });

    let mut st = state.0.lock().unwrap();
    if let Some(err) = st.error.take() {
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_output_and_fetchurl_drv_paths_match_nix() {
        let store = "/nix/store";
        // Observed from nix-instantiate on <nix/fetchurl.nix> for is-number@6.0.0 (sha512)
        let sha512_hex = "5aed551de20b04af0a016254022499417f781a6384e39460ebfe77f1f2b08a5a14bb6d4a9dc1246063eaa1bda8499e66513ef7bcb1ab1d08cac3728c3f07c3ce";
        let out_path =
            make_fixed_output_path(store, "drvjs-is-number-6.0.0.tgz", false, "sha512", sha512_hex);
        assert_eq!(
            out_path,
            "/nix/store/mhq3kc7mc2kpq0rg7y0x9cb9rvwivgh8-drvjs-is-number-6.0.0.tgz"
        );
        let url = "https://registry.npmjs.org/is-number/-/is-number-6.0.0.tgz";
        let sri = "sha512-Wu1VHeILBK8KAWJUAiSZQX94GmOE45Rg6/538fKwiloUu21KncEkYGPqob2oSZ5mUT73vLGrHQjKw3KMPwfDzg==";
        let drv = Derivation {
            outputs: vec![DerivationOutput {
                name: "out".into(),
                path: out_path.clone(),
                hash_algo: "sha512".into(),
                hash: sha512_hex.into(),
            }],
            input_drvs: BTreeMap::new(),
            input_srcs: BTreeSet::new(),
            platform: "builtin".into(),
            builder: "builtin:fetchurl".into(),
            args: Vec::new(),
            env: BTreeMap::from([
                ("builder".into(), "builtin:fetchurl".into()),
                ("executable".into(), "".into()),
                (
                    "impureEnvVars".into(),
                    "http_proxy https_proxy ftp_proxy all_proxy no_proxy".into(),
                ),
                ("name".into(), "drvjs-is-number-6.0.0.tgz".into()),
                ("out".into(), out_path),
                ("outputHash".into(), sri.into()),
                ("outputHashAlgo".into(), "".into()),
                ("outputHashMode".into(), "flat".into()),
                ("preferLocalBuild".into(), "1".into()),
                ("system".into(), "builtin".into()),
                ("unpack".into(), "".into()),
                ("url".into(), url.into()),
                ("urls".into(), url.into()),
            ]),
        };
        let aterm = drv.format();
        assert_eq!(
            drv.path(store, aterm.as_bytes()),
            "/nix/store/kyc55xh0lwkbmirvlvamgixp13svlb72-drvjs-is-number-6.0.0.tgz.drv"
        );

        // Observed from nix-instantiate on <nix/fetchurl.nix> (sha256)
        let sha256_hex = "0000000000000000000000000000000000000000000000000000000000000000";
        let out2 = make_fixed_output_path(store, "drvjs-a.tgz", false, "sha256", sha256_hex);
        assert_eq!(out2, "/nix/store/16a0widx47h2446p3b1wppmp8l9zf6sj-drvjs-a.tgz");
    }
}
