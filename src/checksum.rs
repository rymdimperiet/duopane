//! "Checksum": the SHA-256 of a file, checked against a checksum list next to it
//! (`name.sha256`, `SHA256SUMS` and the like) when there is one. On a checksum
//! list itself, every file it names is checked.
//!
//! SHA-256 is written out here rather than pulled in as a crate: it's short, and
//! it's tested against the standard's own examples.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// What a check found, ready to show.
pub struct Report {
    pub title: String,
    pub text: String,
    /// The hash of a single file.
    #[cfg_attr(not(test), allow(dead_code))]
    pub hash: Option<String>,
}

/// Whether a file is a checksum list (rather than something to take the checksum of).
pub fn is_list(name: &str) -> bool {
    let n = name.to_lowercase();
    n.ends_with(".sha256") || n.ends_with(".sha256sum") || n.starts_with("sha256sums")
}

/// Takes the checksum of `path` (or checks the files a list names). Slow for big
/// files: run it off the UI thread. `cancel` stops it between blocks.
pub fn check(path: &Path, cancel: &Arc<AtomicBool>) -> Report {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let dir = path.parent().unwrap_or(Path::new("/"));
    if is_list(&name) {
        return check_list(path, &name, dir, cancel);
    }
    let hash = match sha256_file(path, cancel) {
        Ok(h) => h,
        Err(e) => return Report { title: "Checksum".into(), text: format!("Couldn't read {name}: {e}"), hash: None },
    };
    let mut text = format!("SHA-256 of {name}:\n\n{hash}");
    match expected_for(dir, &name) {
        Some((list, want)) if want == hash => text.push_str(&format!("\n\n✓ Matches the checksum in {list}.")),
        Some((list, want)) => text.push_str(&format!("\n\n✗ Doesn't match the checksum in {list}, which is:\n{want}\nThe file may be damaged or incomplete.")),
        None => {}
    }
    Report { title: "Checksum".into(), text, hash: Some(hash) }
}

fn check_list(path: &Path, name: &str, dir: &Path, cancel: &Arc<AtomicBool>) -> Report {
    let title = format!("Checking {name}");
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return Report { title, text: format!("Couldn't read {name}: {e}"), hash: None },
    };
    let mut entries = parse_list(&content);
    // "file.iso.sha256" holding just a hash is about "file.iso".
    if entries.is_empty() {
        if let (Some(h), Some(stem)) = (lone_hash(&content), Path::new(name).file_stem()) {
            entries.push((h, stem.to_string_lossy().into_owned()));
        }
    }
    if entries.is_empty() {
        return Report { title, text: format!("{name} has no SHA-256 checksums in it."), hash: None };
    }
    let (mut good, mut bad, mut missing) = (Vec::new(), Vec::new(), Vec::new());
    for (want, file) in entries {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let target: PathBuf = dir.join(&file);
        match sha256_file(&target, cancel) {
            Ok(h) if h == want => good.push(file),
            Ok(_) => bad.push(file),
            Err(_) => missing.push(file),
        }
    }
    let mut text = String::new();
    if !bad.is_empty() {
        text.push_str(&format!("✗ Don't match (damaged or incomplete):\n  {}\n\n", bad.join("\n  ")));
    }
    if !missing.is_empty() {
        text.push_str(&format!("Not found or unreadable:\n  {}\n\n", missing.join("\n  ")));
    }
    if !good.is_empty() {
        text.push_str(&format!("✓ Match:\n  {}", good.join("\n  ")));
    }
    Report { title, text: text.trim_end().to_string(), hash: None }
}

/// The checksum a list next to the file gives for it, and that list's name.
fn expected_for(dir: &Path, name: &str) -> Option<(String, String)> {
    let mut lists = vec![format!("{name}.sha256"), format!("{name}.sha256sum")];
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut more: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.to_lowercase().starts_with("sha256sums") || (is_list(n) && !lists.contains(n)))
            .collect();
        more.sort();
        lists.extend(more);
    }
    for list in lists {
        let Ok(meta) = std::fs::metadata(dir.join(&list)) else { continue };
        // A checksum list is small; anything big is something else.
        if meta.len() > 1 << 20 {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(dir.join(&list)) else { continue };
        if let Some((h, _)) = parse_list(&content).into_iter().find(|(_, f)| f == name) {
            return Some((list, h));
        }
        if list == format!("{name}.sha256") || list == format!("{name}.sha256sum") {
            if let Some(h) = lone_hash(&content) {
                return Some((list, h));
            }
        }
    }
    None
}

/// (hash, file name) pairs from `sha256sum` output ("hash  name", "hash *name")
/// or the BSD style ("SHA256 (name) = hash"). Hashes come back in lower case.
fn parse_list(content: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("SHA256 (") {
            if let Some((file, hash)) = rest.rsplit_once(") = ") {
                if is_hash(hash) {
                    out.push((hash.to_lowercase(), file.to_string()));
                }
            }
            continue;
        }
        if let Some((hash, file)) = line.split_once(char::is_whitespace) {
            let file = file.trim_start().trim_start_matches('*');
            if is_hash(hash) && !file.is_empty() {
                // sha256sum writes "./name" when given a path like that.
                out.push((hash.to_lowercase(), file.trim_start_matches("./").to_string()));
            }
        }
    }
    out
}

fn lone_hash(content: &str) -> Option<String> {
    let t = content.trim();
    is_hash(t).then(|| t.to_lowercase())
}

fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn sha256_file(path: &Path, cancel: &Arc<AtomicBool>) -> Result<String, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.hex())
}

/// SHA-256 (FIPS 180-4).
struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    used: usize,
    length: u64,
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
    0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
    0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Sha256 {
    fn new() -> Self {
        Sha256 {
            state: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            block: [0; 64],
            used: 0,
            length: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.length += data.len() as u64;
        while !data.is_empty() {
            let take = (64 - self.used).min(data.len());
            self.block[self.used..self.used + take].copy_from_slice(&data[..take]);
            self.used += take;
            data = &data[take..];
            if self.used == 64 {
                let block = self.block;
                self.compress(&block);
                self.used = 0;
            }
        }
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, c) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }

    fn hex(mut self) -> String {
        let bits = self.length * 8;
        self.update(&[0x80]);
        while self.used != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        self.state.iter().map(|v| format!("{v:08x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        h.hex()
    }

    #[test]
    fn known_hashes() {
        assert_eq!(sha(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            sha(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // Fed in uneven pieces: the same answer.
        let million = vec![b'a'; 1_000_000];
        let mut h = Sha256::new();
        for piece in million.chunks(777) {
            h.update(piece);
        }
        assert_eq!(h.hex(), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }

    #[test]
    fn checking_against_lists() {
        let dir = std::env::temp_dir().join(format!("duopane-checksum-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        std::fs::write(dir.join("a.iso"), "abc").unwrap();
        std::fs::write(dir.join("b.iso"), "abd").unwrap();
        std::fs::write(dir.join("SHA256SUMS"), format!("{abc}  a.iso\n{abc} *b.iso\n{abc}  ./gone.iso\n")).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));

        let r = check(&dir.join("a.iso"), &cancel);
        assert_eq!(r.hash.as_deref(), Some(abc));
        assert!(r.text.contains("✓ Matches the checksum in SHA256SUMS"), "{}", r.text);
        let r = check(&dir.join("b.iso"), &cancel);
        assert!(r.text.contains("✗ Doesn't match"), "{}", r.text);

        let r = check(&dir.join("SHA256SUMS"), &cancel);
        assert!(r.text.contains("Don't match (damaged or incomplete):\n  b.iso"), "{}", r.text);
        assert!(r.text.contains("Not found or unreadable:\n  gone.iso"), "{}", r.text);
        assert!(r.text.contains("✓ Match:\n  a.iso"), "{}", r.text);

        // A list holding just the hash, BSD style lines.
        std::fs::write(dir.join("c.txt"), "abc").unwrap();
        std::fs::write(dir.join("c.txt.sha256"), format!("{}\n", abc.to_uppercase())).unwrap();
        assert!(check(&dir.join("c.txt"), &cancel).text.contains("✓ Matches the checksum in c.txt.sha256"));
        assert_eq!(parse_list(&format!("SHA256 (x y.bin) = {abc}")), vec![(abc.to_string(), "x y.bin".to_string())]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
