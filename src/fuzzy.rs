//! A small fuzzy matcher: every character of the query must appear in order.
//! Matches at word starts and consecutive runs score higher.

/// Returns `None` when `query` doesn't match `text`, otherwise a score (higher is better).
pub fn score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(lower)
        .collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.chars().collect();
    let mut qi = 0;
    let mut total = 0;
    let mut last: Option<usize> = None;
    let mut first: Option<usize> = None;
    for (i, &c) in t.iter().enumerate() {
        if qi >= q.len() {
            break;
        }
        if lower(c) == q[qi] {
            let mut s = 1;
            if let Some(l) = last {
                if l + 1 == i {
                    s += 6;
                }
            }
            let word_start = i == 0
                || matches!(t[i - 1], '/' | '\\' | ' ' | '_' | '-' | '.')
                || (c.is_uppercase() && t[i - 1].is_lowercase());
            if word_start {
                s += 8;
            }
            total += s;
            first.get_or_insert(i);
            last = Some(i);
            qi += 1;
        }
    }
    if qi < q.len() {
        return None;
    }
    // Prefer short texts and early matches.
    Some(total * 10 - t.len() as i32 - first.unwrap_or(0) as i32)
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

#[cfg(test)]
mod tests {
    use super::score;

    #[test]
    fn matches_in_order() {
        assert!(score("dl", "Downloads").is_some());
        assert!(score("ld", "Downloads").is_some());
        assert!(score("xyz", "Downloads").is_none());
    }

    #[test]
    fn prefers_word_starts() {
        let a = score("doc", "Documents").unwrap();
        let b = score("doc", "my_old_cache").unwrap_or(i32::MIN);
        assert!(a > b);
    }
}
