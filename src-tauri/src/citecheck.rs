//! Citation existence verification — the mechanical half of "trustworthy RAG".
//!
//! Prompting asks the model to cite only what it was given; prompting is not
//! enforcement. Two failure modes actually happen:
//!
//! 1. **Fabricated sources** — the answer contains ⟦Some Doc · p12⟧ where
//!    "Some Doc" was never in the retrieved set. The reader sees a legitimate-
//!    looking citation backed by nothing. Fix: strip any ⟦…⟧ token whose
//!    source name is not among the retrieved hits (web citations allowed only
//!    in web mode), and report how many were dropped.
//! 2. **Numbered ghosts** — the answer cites [7] when 5 chunks were retrieved.
//!    Fix: drop every [n] outside 1..=max, and return the indices that
//!    survived so the caller can build a citations array of what was ACTUALLY
//!    cited, not everything retrieved (知芽-style honesty).
//!
//! Zero-hit abstention rides on the same helpers: with an empty retrieved set
//! every ⟦…⟧/[n] token is by definition fabricated, so `max = 0` / an empty
//! name list strips them all — the caller marks the answer as ungrounded.

/// Extract the source-name part of a `⟦name · loc⟧` token body: everything
/// before the first `·`, trimmed. A token without a separator is all name.
fn token_name(body: &str) -> &str {
    body.split('·').next().unwrap_or(body).trim()
}

/// Strip ⟦…⟧ citation tokens whose source name is not in `valid_names`.
/// `allow_web` keeps `⟦web · host⟧` tokens (web mode). Returns the cleaned
/// text and the number of tokens dropped.
pub fn verify_named(text: &str, valid_names: &[String], allow_web: bool) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut dropped = 0usize;
    let mut rest = text;
    while let Some(start) = rest.find('⟦') {
        let Some(end_rel) = rest[start + '⟦'.len_utf8()..].find('⟧') else {
            // Unterminated token — keep the text as-is from here on.
            out.push_str(rest);
            rest = "";
            break;
        };
        let end = start + '⟦'.len_utf8() + end_rel;
        let body = &rest[start + '⟦'.len_utf8()..end];
        let name = token_name(body);
        let ok = valid_names.iter().any(|v| v == name)
            || (allow_web && name == "web");
        if ok {
            out.push_str(&rest[..end + '⟧'.len_utf8()]);
        } else {
            // Keep the text BEFORE the token, drop the token itself.
            out.push_str(&rest[..start]);
            dropped += 1;
        }
        rest = &rest[end + '⟧'.len_utf8()..];
    }
    out.push_str(rest);
    (out, dropped)
}

/// Strip `[n]` citation markers outside `1..=max`; return the indices that
/// survived (order of first appearance, deduped). `max = 0` strips everything.
pub fn verify_numbered(text: &str, max: usize) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut cited: Vec<usize> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        let Some(end_rel) = rest[start + 1..].find(']') else {
            out.push_str(rest);
            rest = "";
            break;
        };
        let end = start + 1 + end_rel;
        let body = &rest[start + 1..end];
        match body.trim().parse::<usize>() {
            // Valid index within the retrieved set: keep it, remember it.
            Ok(n) if n >= 1 && n <= max => {
                out.push_str(&rest[..=end]);
                if !cited.contains(&n) {
                    cited.push(n);
                }
            }
            // Out of range, zero, or not a number at all (markdown links,
            // footnote refs on arbitrary text): keep non-numeric bodies,
            // drop numeric ghosts — but keep the text before the marker.
            Ok(_) => out.push_str(&rest[..start]),
            Err(_) => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    (out, cited)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn fabricated_named_citations_are_stripped_real_ones_kept() {
        let text = "对角化见⟦特征值讲义.md · p3⟧，而⟦编造的书.pdf · 1⟧是假的。";
        let (out, dropped) = verify_named(text, &names(&["特征值讲义.md"]), false);
        assert_eq!(dropped, 1);
        assert!(out.contains("⟦特征值讲义.md · p3⟧"));
        assert!(!out.contains("编造"));
    }

    #[test]
    fn web_tokens_survive_only_in_web_mode() {
        let text = "外部说法⟦web · example.com⟧。";
        let (_out1, d1) = verify_named(text, &names(&[]), false);
        assert_eq!(d1, 1);
        let (out2, d2) = verify_named(text, &names(&[]), true);
        assert_eq!(d2, 0);
        assert!(out2.contains("⟦web · example.com⟧"));
    }

    #[test]
    fn max_zero_strips_everything_zero_hit_abstention() {
        let (out, _) = verify_named("回答⟦某来源 · p1⟧结束", &[], false);
        assert_eq!(out, "回答结束");
    }

    #[test]
    fn unterminated_bracket_is_kept_verbatim() {
        let text = "他说⟦没闭合的引用不算引用";
        let (out, d) = verify_named(text, &names(&[]), false);
        assert_eq!(d, 0);
        assert_eq!(out, text);
    }

    #[test]
    fn numbered_ghosts_are_dropped_and_cited_indices_returned() {
        let text = "如 [2] 与 [1] 所示，而 [7] 不存在，[0] 也无效，[链接] 不是引用。";
        let (out, cited) = verify_numbered(text, 5);
        assert_eq!(cited, vec![2, 1]);
        assert!(out.contains("[2]") && out.contains("[1]"));
        assert!(!out.contains("[7]") && !out.contains("[0]"));
        assert!(out.contains("[链接]"), "non-numeric bodies must survive");
    }

    #[test]
    fn cited_indices_are_deduped_in_first_appearance_order() {
        let (out, cited) = verify_numbered("[3] 然后 [1] 然后 [3]", 5);
        assert_eq!(cited, vec![3, 1]);
        assert_eq!(out.matches('[').count(), 3);
    }
}
