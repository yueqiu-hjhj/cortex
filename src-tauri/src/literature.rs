//! Literature retrieval pipeline.
//!
//! Given a user query (optionally with a subject's topic keywords), produce a
//! unified, de-duplicated, re-ranked list of academic hits. The pipeline has
//! four stages:
//!
//! 1. Query rewrite — generate 2-3 search variants (original / expanded /
//!    scholarly) by asking the configured chat LLM, with a deterministic
//!    heuristic fallback when no LLM is available.
//! 2. Multi-source fan-out — each variant is run against several public
//!    scholarly sources in parallel: arXiv, Crossref, OpenAlex, Semantic
//!    Scholar, and (if configured) the user's SearXNG "science" engine.
//! 3. RRF fusion — Reciprocal Rank Fusion with k=60 combines the per-source
//!    ranks into a single score per unique paper (DOI/title dedup).
//! 4. Rule rerank — boosts for: has DOI, has abstract, recent year,
//!    citation count, subject-keyword overlap, academic host; penalties for
//!    SEO/ads domains and very old papers. Final score = RRF + bonuses,
//!    sorted descending.
//!
//! Every step is best-effort and degrades gracefully: a single source failure
//! does not abort the pipeline; a 0-LLM user still gets heuristic rewrites
//! and all sources attempted.

use crate::error::{Error, Result};
use serde::Serialize;

const ARXIV_BASE: &str = "https://export.arxiv.org/api/query";
const CROSSREF_BASE: &str = "https://api.crossref.org/works";
const OPENALEX_BASE: &str = "https://api.openalex.org/works";
const S2_BASE: &str = "https://api.semanticscholar.org/graph/v1/paper/search";
const POLITE_MAILTO: &str = "cortex@local";
const SOURCE_TIMEOUT_SECS: u64 = 10;
const RRF_K: f64 = 60.0;

#[derive(Debug, Clone)]
pub struct RawHit {
    pub source: String,
    pub rank: i64,
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<i64>,
    pub venue: Option<String>,
    pub doi: Option<String>,
    pub url: Option<String>,
    pub abstract_text: Option<String>,
    pub citation_count: Option<i64>,
    pub host: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiteratureHit {
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<i64>,
    pub venue: Option<String>,
    pub doi: Option<String>,
    pub url: Option<String>,
    pub abstract_text: Option<String>,
    pub citation_count: Option<i64>,
    pub source: String,
    pub sources: Vec<String>,
    pub rrf_score: f64,
    pub final_score: f64,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiteratureResult {
    pub query: String,
    pub rewritten: Vec<String>,
    pub hits: Vec<LiteratureHit>,
    pub sources_attempted: Vec<String>,
    pub sources_succeeded: Vec<String>,
    pub took_ms: i64,
}
// ---- query rewrite ----------------------------------------------------

    /// Produce 2-3 search variants for the user query. Tries the chat LLM first;
    /// falls back to a deterministic heuristic if the LLM is the offline stub or
    /// returns nothing usable.
    pub fn rewrite_query(
        llm: Option<&dyn crate::llm::Llm>,
        query: &str,
        subject_keywords: &[String],
    ) -> Vec<String> {
        if let Some(model) = llm {
            if let Ok(variants) = llm_rewrite(model, query, subject_keywords) {
                if !variants.is_empty() {
                    return variants;
                }
            }
        }
        heuristic_rewrite(query, subject_keywords)
    }

    fn llm_rewrite(
        model: &dyn crate::llm::Llm,
        query: &str,
        subject_keywords: &[String],
    ) -> Result<Vec<String>> {
        let kw_hint = if subject_keywords.is_empty() {
            String::new()
        } else {
            format!("\nDomain keywords (synonyms/OR-terms when relevant): {}.\n",
                subject_keywords.join(", "))
        };
        let sys = "You expand an academic search query. Output 2 or 3 distinct search \
variants, one per line, no numbering, no quotes, no commentary. Variants \
should target scholarly databases: include the original query, an expanded \
form with synonyms/related terms, and a precise form (quoted key terms or \
method/result keywords). Avoid generic words like 'literature' or 'paper'. \
Each variant must be <= 100 characters.";
        let user = format!("Query: {}{}\nVariants:", query, kw_hint);
        let raw = model.complete(sys, &user)?;
        let out: Vec<String> = raw
            .lines()
            .map(|l| l.trim().trim_matches('"').to_string())
            .filter(|l| !l.is_empty() && l.chars().count() <= 120 && l.chars().count() >= 2)
            .take(3)
            .collect();
        Ok(out)
    }

    fn heuristic_rewrite(query: &str, subject_keywords: &[String]) -> Vec<String> {
        let q = query.trim();
        let mut out: Vec<String> = Vec::new();
        if !q.is_empty() {
            out.push(q.to_string());
        }
        if !subject_keywords.is_empty() {
            let extra: Vec<String> = subject_keywords.iter()
                .take(3)
                .filter(|k| !q.to_lowercase().contains(&k.to_lowercase()))
                .cloned()
                .collect();
            if !extra.is_empty() {
                out.push(format!("{} {}", q, extra.join(" ")));
            }
        }
        let stops: &[&str] = &["what", "is", "are", "how", "why", "the", "a", "of", "to", "in"];
        let quoted: String = q.split_whitespace()
            .filter(|w| w.len() > 3 && !stops.contains(&w.to_lowercase().as_str()))
            .take(4)
            .map(|w| format!("\"{}\"", w))
            .collect::<Vec<_>>()
            .join(" ");
        let mut scholarly = if !quoted.is_empty() { quoted } else { q.to_string() };
        let low = q.to_lowercase();
        if !low.contains("survey") && !low.contains("review") && !low.contains("meta-analysis")
            && !low.contains("tutorial") {
            scholarly.push_str(" survey");
        }
        out.push(scholarly);
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        out.retain(|v| seen.insert(v.to_lowercase()));
        out
    }
// ---- shared HTTP helper ---------------------------------------------

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(SOURCE_TIMEOUT_SECS))
        .user_agent("Cortex/1.0 (mailto:cortex@local)")
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
}

/// Extract a host (no scheme/www./port) from a URL. Mirrors the helper in
/// commands.rs but lives here to avoid leaking that private symbol.
fn host_of(url: &str) -> Option<String> {
    let after = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let auth = after.split(['/', '?', '#']).next()?;
    let auth = auth.rsplit('@').next()?;
    let host = auth.split(':').next()?.trim_start_matches("www.");
    if host.is_empty() { None } else { Some(host.to_string()) }
}

fn first_author_year(s: &serde_json::Value) -> (Vec<String>, Option<i64>) {
    let authors: Vec<String> = s["author"].as_array()
        .map(|a| a.iter().filter_map(|x| {
            if let Some(n) = x["name"].as_str() { return Some(n.to_string()); }
            let g = x["given"].as_str()?;
            let f = x["family"].as_str()?;
            Some(format!("{} {}", g, f))
        }).collect())
        .unwrap_or_default();
    let year = s["published-print"]["date-parts"][0][0].as_i64()
        .or_else(|| s["published-online"]["date-parts"][0][0].as_i64())
        .or_else(|| s["created"]["date-parts"][0][0].as_i64());
    (authors, year)
}

// ---- arXiv (Atom XML) -----------------------------------------------

pub fn search_arxiv(query: &str) -> Vec<RawHit> {
    let client = http();
    let resp = match client.get(ARXIV_BASE)
        .query(&[("search_query", format!("all:\"{}\"", query)), ("max_results", "25".to_string())])
        .send()
    {
        Ok(r) if r.status().is_success() => r,
        _ => return Vec::new(),
    };
    let text = match resp.text() { Ok(t) => t, Err(_) => return Vec::new() };
    // Each <entry>...</entry> block. Cheap and good-enough for arXiv's well-formed Atom.
    let mut hits: Vec<RawHit> = Vec::new();
    let mut rank = 0i64;
    for entry in text.split("<entry>").skip(1) {
        let body = match entry.split("</entry>").next() { Some(b) => b, None => continue };
        rank += 1;
        let title = strip_tags(extract_between(body, "<title>", "</title>").unwrap_or_default().to_string());
        let summary = strip_tags(extract_between(body, "<summary>", "</summary>").unwrap_or_default().to_string());
        let id = extract_between(body, "<id>", "</id>").map(|s| s.to_string());
        let published = extract_between(body, "<published>", "</published>").unwrap_or_default();
        let year = published.get(..4).and_then(|y| y.parse::<i64>().ok());
        let authors: Vec<String> = body.split("<author>").skip(1)
            .filter_map(|a| a.split("</author>").next())
            .filter_map(|a| extract_between(a, "<name>", "</name>").map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .take(10)
            .collect();
        if title.is_empty() { continue; }
        let url = if id.as_ref().map(|s| s.is_empty()).unwrap_or(true) { None } else { id };
        let host = host_of(url.as_deref().unwrap_or("")).or_else(|| Some("arxiv.org".into()));
        hits.push(RawHit {
            source: "arxiv".into(), rank, title, authors, year,
            venue: Some("arXiv".into()),
            doi: None, url, abstract_text: Some(summary),
            citation_count: None, host,
        });
    }
    hits
}

fn extract_between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let end = s[start..].find(close)? + start;
    Some(&s[start..end])
}

fn strip_tags(s: String) -> String {
    // Collapse whitespace and remove any embedded tags.
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---- Crossref --------------------------------------------------------

pub fn search_crossref(query: &str) -> Vec<RawHit> {
    let client = http();
    let resp = match client.get(CROSSREF_BASE)
        .query(&[("query.bibliographic", query), ("rows", "25"), ("mailto", POLITE_MAILTO)])
        .send()
    {
        Ok(r) if r.status().is_success() => r,
        _ => return Vec::new(),
    };
    let json: serde_json::Value = match resp.json() { Ok(v) => v, Err(_) => return Vec::new() };
    let items = match json["message"]["items"].as_array() { Some(a) => a, None => return Vec::new() };
    let mut hits: Vec<RawHit> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let title_arr = it["title"].as_array();
        let title = title_arr.and_then(|a| a.first()).and_then(|v| v.as_str()).unwrap_or("").to_string();
        if title.is_empty() { continue; }
        let (authors, year) = first_author_year(it);
        let doi = it["DOI"].as_str().map(|s| s.to_string());
        let url = it["URL"].as_str().map(|s| s.to_string());
        let host = host_of(url.as_deref().unwrap_or("")).or_else(|| Some("doi.org".into()));
        let venue = it["container-title"].as_array()
            .and_then(|a| a.first()).and_then(|v| v.as_str()).map(|s| s.to_string());
        let citations = it["is-referenced-by-count"].as_i64();
        hits.push(RawHit {
            source: "crossref".into(), rank: (i as i64) + 1,
            title, authors, year, venue, doi, url,
            abstract_text: None, citation_count: citations,
            host,
        });
    }
    hits
}

// ---- OpenAlex --------------------------------------------------------

pub fn search_openalex(query: &str) -> Vec<RawHit> {
    let client = http();
    let resp = match client.get(OPENALEX_BASE)
        .query(&[("search", query), ("per-page", "25"), ("mailto", POLITE_MAILTO)])
        .send()
    {
        Ok(r) if r.status().is_success() => r,
        _ => return Vec::new(),
    };
    let json: serde_json::Value = match resp.json() { Ok(v) => v, Err(_) => return Vec::new() };
    let items = match json["results"].as_array() { Some(a) => a, None => return Vec::new() };
    let mut hits: Vec<RawHit> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let title = it["title"].as_str().unwrap_or("").to_string();
        if title.is_empty() { continue; }
        let authors: Vec<String> = it["authorships"].as_array()
            .map(|a| a.iter().filter_map(|x| x["author"]["display_name"].as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        let year = it["publication_year"].as_i64();
        let venue = it["primary_location"]["source"]["display_name"].as_str().map(|s| s.to_string());
        let doi = it["doi"].as_str().map(|s| s.to_string());
        let url = it["id"].as_str().map(|s| s.to_string());
        let host = host_of(url.as_deref().unwrap_or("")).or_else(|| Some("openalex.org".into()));
        let abstract_text = it["abstract_inverted_index"].as_object().map(|o| {
            let mut parts: Vec<(u64, &str)> = o.iter().flat_map(|(word, positions)| {
                positions.as_array().into_iter().flatten().filter_map(move |p| p.as_u64().map(|p| (p, word.as_str())))
            }).collect();
            parts.sort_by_key(|(p, _)| *p);
            parts.into_iter().map(|(_, word)| word).collect::<Vec<_>>().join(" ")
        }).filter(|s| !s.is_empty());
        let citations = it["cited_by_count"].as_i64();
        hits.push(RawHit {
            source: "openalex".into(), rank: (i as i64) + 1,
            title, authors, year, venue, doi, url, abstract_text, citation_count: citations,
            host,
        });
    }
    hits
}

// ---- Semantic Scholar ------------------------------------------------

pub fn search_semantic_scholar(query: &str) -> Vec<RawHit> {
    let client = http();
    let fields = "title,authors,year,venue,abstract,externalIds,url,citationCount";
    let resp = match client.get(S2_BASE)
        .query(&[("query", query), ("limit", "25"), ("fields", fields)])
        .send()
    {
        Ok(r) if r.status().is_success() => r,
        _ => return Vec::new(),
    };
    let json: serde_json::Value = match resp.json() { Ok(v) => v, Err(_) => return Vec::new() };
    let items = match json["data"].as_array() { Some(a) => a, None => return Vec::new() };
    let mut hits: Vec<RawHit> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let title = it["title"].as_str().unwrap_or("").to_string();
        if title.is_empty() { continue; }
        let authors: Vec<String> = it["authors"].as_array()
            .map(|a| a.iter().filter_map(|x| x["name"].as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        let year = it["year"].as_i64();
        let venue = it["venue"].as_str().map(|s| s.to_string()).filter(|s| !s.is_empty());
        let doi = it["externalIds"]["DOI"].as_str().map(|s| s.to_string());
        let url = it["url"].as_str().map(|s| s.to_string());
        let host = host_of(url.as_deref().unwrap_or("")).or_else(|| Some("semanticscholar.org".into()));
        let abstract_text = it["abstract"].as_str().map(|s| s.to_string()).filter(|s| !s.is_empty());
        let citations = it["citationCount"].as_i64();
        hits.push(RawHit {
            source: "semantic_scholar".into(), rank: (i as i64) + 1,
            title, authors, year, venue, doi, url, abstract_text, citation_count: citations,
            host,
        });
    }
    hits
}

// ---- SearXNG (science) — optional, requires configured searxng_url --

pub fn search_searxng_science(query: &str, searxng_base: Option<&str>) -> Vec<RawHit> {
    let Some(base) = searxng_base else { return Vec::new(); };
    let client = http();
    let resp = match client.get(format!("{}/search", base.trim_end_matches('/')))
        .query(&[("q", query), ("format", "json"), ("categories", "science"), ("pageno", "1")])
        .send()
    {
        Ok(r) if r.status().is_success() => r,
        _ => return Vec::new(),
    };
    let json: serde_json::Value = match resp.json() { Ok(v) => v, Err(_) => return Vec::new() };
    let items = match json["results"].as_array() { Some(a) => a, None => return Vec::new() };
    let mut hits: Vec<RawHit> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let title = it["title"].as_str().unwrap_or("").to_string();
        if title.is_empty() { continue; }
        let url = it["url"].as_str().map(|s| s.to_string());
        let host = host_of(url.as_deref().unwrap_or("")).or_else(|| Some("searxng".into()));
        let snippet = it["content"].as_str().map(|s| s.to_string()).filter(|s| !s.is_empty());
        // SearXNG science results often don't carry structured metadata; we
        // surface what we can and leave the rest None for the reranker.
        hits.push(RawHit {
            source: "searxng".into(), rank: (i as i64) + 1,
            title, authors: Vec::new(), year: None, venue: Some("web".into()),
            doi: None, url, abstract_text: snippet, citation_count: None,
            host,
        });
    }
    hits
}
// ---- fusion & rerank ------------------------------------------------

/// Normalize a title for dedup: lowercase, collapse whitespace, strip
/// non-alphanumeric noise. We do not decode Unicode; titles in CJK still
/// match each other (their normalized form is identical).
fn title_key(t: &str) -> String {
    t.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Reciprocal Rank Fusion. For each paper id (DOI preferred, else title key),
/// sum 1 / (RRF_K + rank) across all source lists. Returns a sorted vec.
fn doi_key(doi: &str) -> String {
    let d = doi.trim().to_lowercase();
    let d = d.strip_prefix("https://doi.org/").or_else(|| d.strip_prefix("http://doi.org/"))
        .or_else(|| d.strip_prefix("https://dx.doi.org/")).or_else(|| d.strip_prefix("doi:")).unwrap_or(&d);
    format!("doi:{}", d.trim())
}

fn rrf_fuse(lists: &[Vec<RawHit>]) -> Vec<(String, f64, Vec<String>, Vec<RawHit>)> {
    // id -> (rrf_score, sources_seen, member_hits)
    let mut acc: std::collections::HashMap<String, (f64, Vec<String>, Vec<RawHit>)>
        = std::collections::HashMap::new();
    let title_dois: std::collections::HashMap<String, String> = lists.iter().flatten()
        .filter_map(|h| h.doi.as_deref().map(|d| (title_key(&h.title), doi_key(d)))).collect();
    for list in lists {
        let mut seen = std::collections::HashSet::new();
        for h in list {
            let title = title_key(&h.title);
            let key = h.doi.as_deref().map(doi_key)
                .or_else(|| title_dois.get(&title).cloned())
                .unwrap_or_else(|| format!("t:{}", title));
            if !seen.insert(key.clone()) { continue; }
            let entry = acc.entry(key).or_insert((0.0, Vec::new(), Vec::new()));
            entry.0 += 1.0 / (RRF_K + h.rank as f64);
            if !entry.1.contains(&h.source) {
                entry.1.push(h.source.clone());
            }
            entry.2.push(h.clone());
        }
    }
    let mut out: Vec<(String, f64, Vec<String>, Vec<RawHit>)> = acc.into_iter()
        .map(|(k, (s, src, hits))| (k, s, src, hits))
        .collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Rule-based rerank: produce the final score and a human-readable trace.
fn current_year() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(1_730_000_000);
    // 1970 + seconds / (365.25 * 86400) -- good enough for recency scoring.
    1970 + (secs / 31_557_600)
}

fn rule_rerank(
    fused_hits: &[(String, f64, Vec<String>, Vec<RawHit>)],
    subject_keywords: &[String],
) -> Vec<LiteratureHit> {
    // Normalize RRF: divide by the max so the strongest hit reads as 1.0.
    let rrf_max = fused_hits.iter().map(|(_, s, _, _)| *s).fold(0.0_f64, f64::max);
    let norm = |s: f64| if rrf_max > 0.0 { s / rrf_max } else { 0.0 };
    let now_year = current_year();
    let kw_low: Vec<String> = subject_keywords.iter().map(|k| k.to_lowercase()).collect();

    let mut out: Vec<LiteratureHit> = Vec::with_capacity(fused_hits.len());
    for (_, rrf, sources, members) in fused_hits {
        // Pick the best member as the canonical representative: prefer one
        // with an abstract, then the most-cited, then the earliest.
        let rep = members.iter().max_by_key(|m| (
            (if m.abstract_text.is_some() { 1u8 } else { 0u8 }),
            m.citation_count.unwrap_or(0),
        )).cloned().unwrap_or_else(|| members[0].clone());

        let mut score = norm(*rrf);
        let mut reasons: Vec<String> = Vec::new();
        reasons.push(format!("RRF {:.2} (sources: {})", rrf, sources.join(",")));

        if rep.doi.is_some() {
            score += 0.20;
            reasons.push("+doi".into());
        }
        if rep.abstract_text.is_some() {
            score += 0.15;
            reasons.push("+abstract".into());
        }
        if let Some(c) = rep.citation_count {
            let bump = (c as f64 / 100.0).min(1.0) * 0.10;
            if bump > 0.0 {
                score += bump;
                reasons.push(format!("+cit:{}", c));
            }
        }
        if let Some(y) = rep.year {
            let age = (now_year - y).max(0) as f64;
            let recency = ((5.0 - age).max(0.0) / 5.0) * 0.10;
            if recency > 0.0 {
                score += recency;
                reasons.push(format!("+recent:{}", y));
            } else if age > 20.0 {
                score -= 0.10;
                reasons.push(format!("-old:{}", y));
            }
        }
        // Subject-keyword overlap against title + abstract.
        if !kw_low.is_empty() {
            let text = format!("{} {}", rep.title.to_lowercase(),
                rep.abstract_text.as_deref().unwrap_or("").to_lowercase());
            let hits_kw = kw_low.iter().filter(|k| text.contains(k.as_str())).count();
            if hits_kw > 0 {
                let bump = (hits_kw as f64 / kw_low.len() as f64).min(1.0) * 0.10;
                score += bump;
                reasons.push(format!("+kw:{}", hits_kw));
            }
        }
        // Host heuristic.
        let host = rep.host.as_deref().unwrap_or("").to_lowercase();
        if host.ends_with(".edu") || host.ends_with(".ac.cn") || host.ends_with(".ac.uk")
            || host.contains("scholar") || host.contains("semanticscholar")
            || host.contains("arxiv.org") || host.contains("doi.org")
            || host.contains("openalex")
        {
            score += 0.05;
            reasons.push("+academic".into());
        }
        if host.contains("click.") || host.contains("ads.") || host.contains("shop")
            || host.contains("spam")
        {
            score -= 0.10;
            reasons.push("-seo".into());
        }

        out.push(LiteratureHit {
            title: rep.title,
            authors: rep.authors,
            year: rep.year,
            venue: rep.venue,
            doi: rep.doi,
            url: rep.url,
            abstract_text: rep.abstract_text,
            citation_count: rep.citation_count,
            source: sources.first().cloned().unwrap_or_else(|| rep.source.clone()),
            sources: sources.clone(),
            rrf_score: norm(*rrf),
            final_score: score,
            reasons,
        });
    }
    out.sort_by(|a, b| b.final_score.partial_cmp(&a.final_score).unwrap_or(std::cmp::Ordering::Equal));
    out
}
// ---- public entrypoint ---------------------------------------------

/// Look up the topic keywords for a subject. Empty Vec if the subject
/// doesn't exist or has no topics; the pipeline still works, the reranker
/// just skips keyword overlap.
pub fn subject_keywords(conn: &rusqlite::Connection, subject_id: &str) -> Vec<String> {
    let mut st = match conn.prepare("SELECT name FROM topics WHERE subject_id = ?1 ORDER BY position, name") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    st.query_map([subject_id], |r| r.get::<_, String>(0))
        .map(|rows| rows.flatten().filter(|n| !n.is_empty()).collect())
        .unwrap_or_default()
}

/// Run the full pipeline. query is the user prompt; subject_id (if
/// given) supplies topic keywords for the rewrite + rerank stages.
/// limit caps the final hit list (default 30). Best-effort: a single
/// source failure does not abort, and a query that nothing can match
/// returns an empty hit list rather than an error.
pub fn literature_search(
    llm: Option<&dyn crate::llm::Llm>,
    query: &str,
    keywords: &[String],
    searxng_base: Option<&str>,
    limit: usize,
) -> Result<LiteratureResult> {
    let started = std::time::Instant::now();
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(Error::Other("empty query".into()));
    }

    let mut variants = vec![trimmed.to_string()];
    for variant in rewrite_query(llm, trimmed, keywords) {
        if !variants.iter().any(|v| v.eq_ignore_ascii_case(&variant)) { variants.push(variant); }
    }
    variants.truncate(3);
    if variants.is_empty() {
        return Err(Error::Other("no rewrite variants produced".into()));
    }

    // SearXNG base is optional; resolved through the homelab chain.

    let mut attempted: Vec<String> = Vec::new();
    let mut succeeded: Vec<String> = Vec::new();
    let mut all_lists: Vec<Vec<RawHit>> = Vec::new();
    for v in &variants {
        let lists = std::thread::scope(|scope| {
            let mut jobs = vec![
                ("arxiv", scope.spawn(|| search_arxiv(v))),
                ("crossref", scope.spawn(|| search_crossref(v))),
                ("openalex", scope.spawn(|| search_openalex(v))),
                ("semantic_scholar", scope.spawn(|| search_semantic_scholar(v))),
            ];
            if searxng_base.is_some() {
                jobs.push(("searxng", scope.spawn(|| search_searxng_science(v, searxng_base))));
            }
            jobs.into_iter().map(|(name, job)| (name, job.join().unwrap_or_default())).collect::<Vec<_>>()
        });
        for (name, list) in lists {
            if !attempted.contains(&name.to_string()) { attempted.push(name.to_string()); }
            if !list.is_empty() {
                if !succeeded.contains(&name.to_string()) { succeeded.push(name.to_string()); }
                all_lists.push(list);
            }
        }
    }

    let fused = rrf_fuse(&all_lists);
    let mut hits = rule_rerank(&fused, keywords);
    hits.truncate(limit.max(1));

    Ok(LiteratureResult {
        query: trimmed.to_string(),
        rewritten: variants,
        hits,
        sources_attempted: attempted,
        sources_succeeded: succeeded,
        took_ms: started.elapsed().as_millis() as i64,
    })
}



// ---- paper-text repair ------------------------------------------------

/// Light post-processing for PDF / OCR-extracted paper text. Cheap regex
/// passes that handle the most common artifacts so the reader view doesn't
/// show one-word-per-line walls of text. Anything more clever (column
/// detection, table extraction) would need a dedicated layout model; this is
/// the no-dependency baseline.
pub fn repair_paper_text(input: &str) -> String {
    let mut s = input.to_string();
    // 1. Merge hyphenated line breaks: "transfor-\nmation" -> "transformation".
    //    Hyphen at line end + lowercase next line char => join without hyphen.
    let re_hyphen = regex::Regex::new(r"([a-z])-\s*\n\s*([a-z])").unwrap();
    s = re_hyphen.replace_all(&s, |c: &regex::Captures| format!("{}{}", &c[1], &c[2])).into_owned();
    // 2. Soft line breaks inside a paragraph: a newline that is NOT followed
    //    by a blank line is usually a typesetting wrap, not a paragraph break.
    //    We replace those with a single space.
    let re_soft = regex::Regex::new(r"([^\n])\n([^\n])").unwrap();
    s = re_soft.replace_all(&s, |c: &regex::Captures| format!("{} {}", &c[1], &c[2])).into_owned();
    // 3. Collapse runs of 3+ blank lines down to 2.
    let re_blank = regex::Regex::new(r"\n{3,}").unwrap();
    s = re_blank.replace_all(&s, "\n\n").into_owned();
    // 4. Trim trailing whitespace per line.
    let re_trail = regex::Regex::new(r"[ \t]+$").unwrap();
    s = re_trail.replace_all(&s, "").into_owned();
    // 5. Collapse runs of 3+ spaces (introduced by the soft-break join).
    let re_space = regex::Regex::new(r" {3,}").unwrap();
    s = re_space.replace_all(&s, "  ").into_owned();
    s.trim().to_string()
}

// ---- math repair (2.6) --------------------------------------------------

/// A line that looks like (part of) an equation rather than prose. Deliberately
/// conservative: at least one math operator, and at most ONE long alphabetic
/// word (equations carry symbols and short identifiers; prose carries many
/// long words like "computed"/"learned"). False positives would wrap ordinary
/// text into $$…$$ and corrupt the reading view.
fn is_equation_line(line: &str) -> bool {
    let t = line.trim();
    if t.chars().count() < 3 || t.starts_with("$$") || t.starts_with("\\(") || t.starts_with("\\[") {
        return false;
    }
    if t.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
        return false;
    }
    let ops = t
        .chars()
        .filter(|c| {
            matches!(
                c,
                '=' | '+' | '-' | '−' | '×' | '÷' | '^' | '_' | '\\' | '≤' | '≥' | '≈' | '±'
                    | '∑' | '∫' | '∂' | '∈'
            )
        })
        .count();
    if ops == 0 {
        return false;
    }
    let long_words = t
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| w.chars().count() >= 4 && w.chars().all(|c| c.is_alphabetic()))
        .count();
    long_words <= 1
}

/// Make extracted-paper math renderable (2.6 公式修复):
///   1. `\[ … \]` display delimiters → `$$ … $$` (KaTeX-ready shape the
///      RichText renderer understands; pdftotext leaks these constantly).
///   2. Consecutive equation-like bare lines are wrapped into ONE `$$…$$`
///      display block with internal wraps folded — multi-line equations that
///      otherwise render as broken prose.
///   3. Stray Unicode math letters commonly produced by PDF extraction
///      (e.g. "�" ligature leftovers) are left alone — only structure is
///      repaired, content is never invented.
pub fn fix_paper_math(input: &str) -> String {
    // 1. display delimiters
    let mut s = input.replace("\\[", "$$").replace("\\]", "$$");
    // 2. group consecutive equation-like lines — but never touch lines inside
    //    an existing $$…$$ block (toggle on fence lines).
    let mut out: Vec<String> = Vec::new();
    let mut eqbuf: Vec<String> = Vec::new();
    let mut in_block = false;
    let flush = |buf: &mut Vec<String>, out: &mut Vec<String>| {
        if !buf.is_empty() {
            let joined = buf.join(" ");
            out.push(format!("$$\n{joined}\n$$"));
            buf.clear();
        }
    };
    for line in s.lines() {
        if line.trim() == "$$" {
            flush(&mut eqbuf, &mut out);
            in_block = !in_block;
            out.push(line.to_string());
        } else if !in_block && is_equation_line(line) {
            eqbuf.push(line.trim().to_string());
        } else {
            flush(&mut eqbuf, &mut out);
            out.push(line.to_string());
        }
    }
    flush(&mut eqbuf, &mut out);
    s = out.join("\n");
    s
}

// ---- LLM-driven annotation -------------------------------------------

/// Run a focused LLM call for selection-based reading assistance. mode is
/// one of: "translate:<lang>", "explain", "summarize", "keypoints",
/// "define". xcerpt is the selected text (capped at 3000 chars).
pub fn explain_text(
    model: &dyn crate::llm::Llm,
    excerpt: &str,
    mode: &str,
) -> Result<String> {
    let trimmed: String = excerpt.chars().take(3000).collect();
    let (sys, user) = match mode {
        m if m.starts_with("translate:") => {
            let target = m.trim_start_matches("translate:").trim();
            let target = if target.is_empty() { "Simplified Chinese" } else { target };
            let prompt = if target.eq_ignore_ascii_case("en") || target.eq_ignore_ascii_case("English") {
                "Translate the following passage into natural, fluent English. Preserve \
technical terms, citations, and inline math verbatim. Reply with ONLY \
the translation, no preamble.".to_string()
            } else {
                format!("Translate the following passage into natural, fluent {target}. Preserve \
technical terms, citations, and inline math verbatim. Reply with ONLY \
the translation, no preamble.")
            };
            (prompt, trimmed)
        }
        "explain" => (
            "You are a patient tutor. Explain the following passage in clear, plain \
language. Highlight the key idea, the supporting evidence, and any terms \
a non-specialist would not know. 2-3 short paragraphs, no preamble.".to_string(),
            trimmed,
        ),
        "summarize" => (
            "Summarize the following passage in ONE sentence (max 30 words). \
Keep the technical precision; drop hedging.".to_string(),
            trimmed,
        ),
        "keypoints" => (
            "Extract 3-5 key takeaways from the following passage. One bullet each, \
short, action-oriented. No preamble.".to_string(),
            trimmed,
        ),
        "define" => (
            "Define the most important term or concept in the following passage \
in 1-2 sentences, in glossary style. No preamble.".to_string(),
            trimmed,
        ),
        _ => return Err(Error::Other(format!(
            "unknown mode '{mode}'; expected translate:<lang>, explain, summarize, keypoints, or define"
        ))),
    };
    model.complete(&sys, &user)
}


// ---- save paper as a cortex source -----------------------------------

/// Split text into overlapping chunks for embedding. Aim for ~CHUNK_TARGET
/// chars with CHUNK_OVERLap of overlap on each side; respect paragraph
/// boundaries when a paragraph is available.
pub fn chunk_text(text: &str) -> Vec<String> {
    const CHUNK_TARGET: usize = 1500;
    const CHUNK_OVERLAP: usize = 200;
    let mut out: Vec<String> = Vec::new();
    let mut start = 0usize;
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    if len == 0 { return out; }
    while start < len {
        let mut end = (start + CHUNK_TARGET).min(len);
        // Try to back up to the nearest paragraph break before nd.
        if end < len {
            if let Some(rel) = chars[start..end].iter().rposition(|c| *c == '\n') {
                let abs = start + rel;
                if abs > start + CHUNK_TARGET / 2 { end = abs; }
            }
        }
        let chunk: String = chars[start..end].iter().collect();
        let chunk = chunk.trim().to_string();
        if !chunk.is_empty() { out.push(chunk); }
        if end >= len { break; }
        start = end.saturating_sub(CHUNK_OVERLAP);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_key_collapses_whitespace_and_case() {
        assert_eq!(title_key("Mitochondria  are  the"), title_key("mitochondria are the"));
        assert_eq!(title_key("A B C"), "a b c");
    }

    #[test]
    fn heuristic_rewrite_returns_original_expanded_and_scholarly() {
        let v = heuristic_rewrite("transformer attention", &["deep learning".into()]);
        assert!(v.len() >= 2, "want at least 2 variants, got {}: {:?}", v.len(), v);
        assert!(v[0].contains("transformer"));
        // scholarly variant should have at least one quoted term
        assert!(v.iter().any(|x| x.contains('"')));
    }

    #[test]
    fn heuristic_rewrite_skips_keywords_already_in_query() {
        let v = heuristic_rewrite("deep learning", &["deep learning".into()]);
        // 'deep learning' already in query, so the expanded variant is dropped (deduped).
        assert!(!v.iter().any(|x| x == "deep learning deep learning"));
    }

    #[test]
    fn rrf_fuse_aggregates_across_sources() {
        // X is shared between lists (same DOI -> same key); Y is single-source.
        let a = vec![
            RawHit { source: "arxiv".into(), rank: 1, title: "X".into(),
                authors: vec![], year: None, venue: None, doi: Some("10.1/xxx".into()), url: None,
                abstract_text: None, citation_count: None, host: None },
            RawHit { source: "arxiv".into(), rank: 2, title: "Y".into(),
                authors: vec![], year: None, venue: None, doi: None, url: None,
                abstract_text: None, citation_count: None, host: None },
        ];
        let b = vec![
            RawHit { source: "crossref".into(), rank: 1, title: "X".into(),
                authors: vec![], year: None, venue: None, doi: Some("10.1/xxx".into()), url: None,
                abstract_text: None, citation_count: None, host: None },
        ];
        let fused = rrf_fuse(&[a, b]);
        assert_eq!(fused.len(), 2, "X (deduped) and Y should both be present, got {:?}", fused.iter().map(|(k,_,_,_)| k.clone()).collect::<Vec<_>>());
        let x_score = fused.iter().find(|(k, _, _, _)| k == "doi:10.1/xxx").map(|(_, s, _, _)| *s).unwrap_or(0.0);
        let y_score = fused.iter().find(|(k, _, _, _)| k == "t:y").map(|(_, s, _, _)| *s).unwrap_or(0.0);
        assert!(x_score > y_score, "X (multi-source) should outrank Y: x={} y={}", x_score, y_score);
        // X must list both sources it appeared in.
        let x_sources = fused.iter().find(|(k, _, _, _)| k == "doi:10.1/xxx").map(|(_, _, src, _)| src.clone()).unwrap_or_default();
        assert!(x_sources.contains(&"arxiv".to_string()));
        assert!(x_sources.contains(&"crossref".to_string()));
    }

    #[test]
    fn rule_rerank_boosts_doi_and_abstract_and_recent() {
        // Build a fused vec with three hits that differ in DOI/abstract/year only.
        let make = |title: &str, doi: Option<&str>, abs: Option<&str>, year: Option<i64>| {
            let key = doi.map(|d| format!("doi:{}", d)).unwrap_or_else(|| format!("t:{}", title_key(title)));
            let rep = RawHit {
                source: "arxiv".into(), rank: 1, title: title.into(),
                authors: vec!["A".into()], year, venue: None,
                doi: doi.map(|s| s.into()), url: None,
                abstract_text: abs.map(|s| s.into()),
                citation_count: None, host: Some("arxiv.org".into()),
            };
            (key, 0.5, vec!["arxiv".into()], vec![rep])
        };
        let now = current_year();
        let fused = vec![
            make("no doi no abs 2000", None, None, Some(2000)),
            make("doi and abs", Some("10.1/x"), Some("hello deep"), Some(now)),
        ];
        let hits = rule_rerank(&fused, &["deep".into()]);
        // The doi+abstract+recent hit should rank first.
        assert!(hits[0].doi.is_some(), "doi+abstract+recent should win: {:?}", hits.iter().map(|h| (h.doi.is_some(), h.abstract_text.is_some(), h.year)).collect::<Vec<_>>());
        assert!(hits[0].reasons.iter().any(|r| r.starts_with("+doi")));
        assert!(hits[0].reasons.iter().any(|r| r.starts_with("+abstract")));
    }


    #[test]
    fn repair_merges_hyphenated_line_breaks() {
        let s = "transfor-
mation of atten-
tion is critical.";
        let r = repair_paper_text(s);
        assert!(r.contains("transformation"), "merged: {r:?}");
        assert!(r.contains("attention"), "merged: {r:?}");
    }

    #[test]
    fn repair_folds_soft_wraps() {
        let s = "This is a long
sentence that wraps across
lines but is one paragraph.";
        let r = repair_paper_text(s);
        // After repair, the soft newlines should be folded into spaces.
        assert!(r.contains("long sentence"), "soft wrap should fold: {r:?}");
        assert!(r.contains("across lines"), "soft wrap should fold: {r:?}");
        assert!(!r.contains("long\nsentence"), "newline should be gone: {r:?}");
    }

    #[test]
    fn repair_preserves_paragraph_breaks() {
        let s = "First paragraph here.

Second paragraph here.";
        let r = repair_paper_text(s);
        assert!(r.contains("

"), "paragraph break should survive: {r:?}");
    }

    #[test]
    fn math_wraps_equation_runs_and_converts_display_delims() {
        // multi-line bare equations get one $$ block; prose untouched
        let s = "The loss is computed as
L = Wx + b
y = softmax(z)
where W is learned.";
        let r = fix_paper_math(s);
        assert!(r.contains("$$
L = Wx + b y = softmax(z)
$$"), "eq block wrapped: {r:?}");
        assert!(r.starts_with("The loss is computed as
$$"), "prose stays prose: {r:?}");
        // \[ \] → $$
        let d = fix_paper_math("we obtain
\\[ E = mc^2 \\]
done.");
        assert!(d.contains("$$ E = mc^2 $$"), "display delims converted: {d:?}");
    }

    #[test]
    fn math_never_wraps_cjk_or_ordinary_text() {
        let s = "训练误差为 0.32，这是一个普通句子。";
        assert_eq!(fix_paper_math(s), s);
        let p = "This is plain prose with 12 results and no operators to speak of.";
        assert_eq!(fix_paper_math(p), p);
        // already-delimited lines pass through untouched
        let q = "$$
E = mc^2
$$";
        assert_eq!(fix_paper_math(q), q);
    }

    #[test]
    fn chunks_handle_unicode_and_paragraph_boundaries() {
        let text = "中文🧠 paragraph\n".repeat(400);
        let chunks = chunk_text(&text);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.chars().count() <= 1500));
        assert!(chunks.last().unwrap().ends_with("paragraph"));
    }

    #[test]
    fn rrf_matches_doi_urls_and_title_only_hits() {
        let hit = |doi: Option<&str>| RawHit { source: "test".into(), rank: 1,
            title: "Same Paper".into(), authors: vec![], year: None, venue: None,
            doi: doi.map(str::to_string), url: None, abstract_text: None,
            citation_count: None, host: None };
        let fused = rrf_fuse(&[vec![hit(None)], vec![hit(Some("10.1/abc"))], vec![hit(Some("https://doi.org/10.1/ABC"))]]);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].0, "doi:10.1/abc");
        assert_eq!(fused[0].3.len(), 3);
    }

    #[test]
    fn chunk_text_splits_long_input() {
        let text: String = "a".repeat(5000);
        let chunks = chunk_text(&text);
        assert!(chunks.len() >= 3);
        // Each chunk should be smaller than the input.
        assert!(chunks.iter().all(|c| c.len() <= 2000));
        // Overlap: last chars of chunk 0 should appear in chunk 1.
        let last_chars: String = chunks[0].chars().rev().take(50).collect::<String>().chars().rev().collect();
        assert!(chunks[1].starts_with(&last_chars) || chunks[1].contains(&last_chars),
            "expected overlap, got chunk0 tail {last_chars:?} and chunk1 head {:?}",
            chunks[1].chars().take(80).collect::<String>());
    }
}
