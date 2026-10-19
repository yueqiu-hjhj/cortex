//! Built-in research-source connectors — arXiv & PubMed as first-class citizens
//! (roadmap 2.3). These are deliberately NOT manifest plugins: both APIs speak
//! XML (the D5 manifest primitives are JSON-dot-path based), and PubMed needs
//! the esearch → efetch two-step that a single declarative fetch can't express.
//! They reuse everything downstream of D5, though: the same `add_source`
//! pipeline (parse → chunk → embed) and the same subject-scoped origin dedupe,
//! so re-fetching a query never duplicates, and ingested papers land in the
//! normal reading pipeline (状态机/摘录/PRISMA).
//!
//! Auth: neither source needs a key. NCBI allows 3 req/s anonymous; an optional
//! `ncbi_api_key` setting raises that, and is appended when present.
//!
//! Ingest shape: one TEXT source per paper — title + metadata lines + abstract
//! — with `url` pinned to the canonical landing page (which is also the dedupe
//! origin). arXiv optionally follows up with the PDF as a SECOND source so the
//! full text goes through the PDF pipeline (chunked + embedded).

use crate::error::{Error, Result};
use serde::Serialize;
use std::collections::HashSet;
use tauri::Manager;

const PUBMED_BASE: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils";

#[derive(Debug, Clone, Serialize)]
pub struct ResearchConnector {
    pub id: String,
    pub name: String,
    pub desc: String,
    pub hint: String,
    pub supports_pdf: bool,
}

pub fn list() -> Vec<ResearchConnector> {
    vec![
        ResearchConnector {
            id: "arxiv".into(),
            name: "arXiv".into(),
            desc: "arXiv 预印本检索：标题/摘要/作者入库，可同时下载 PDF 全文。".into(),
            hint: "支持 arXiv 检索语法，如 all:transformer AND cat:cs.CL".into(),
            supports_pdf: true,
        },
        ResearchConnector {
            id: "pubmed".into(),
            name: "PubMed".into(),
            desc: "PubMed 生物医学文献检索：标题/摘要/作者/DOI 入库。".into(),
            hint: "支持 PubMed 高级检索语法，如 CRISPR[Title] AND 2023[PDAT]".into(),
            supports_pdf: false,
        },
    ]
}

fn http_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .user_agent("cortex-base/1.0 (research connector; +https://github.com/PndaMan/cortex)")
        .build()
        .map_err(|e| Error::Other(format!("HTTP client: {e}")))
}

// ---- arXiv -----------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ArxivEntry {
    pub id: String, // "2401.12345" — no version suffix
    pub title: String,
    pub summary: String,
    pub authors: Vec<String>,
    pub year: Option<i64>,
    pub abs_url: String,
    pub pdf_url: Option<String>,
}

/// "http://arxiv.org/abs/2401.12345v2" → ("2401.12345", "https://arxiv.org/abs/2401.12345")
pub fn arxiv_id_from_entry_url(raw: &str) -> (String, String) {
    let tail = raw
        .rsplit("/abs/")
        .next()
        .unwrap_or(raw)
        .trim()
        .trim_end_matches('/');
    // drop version suffix: 2401.12345v2 → 2401.12345. The split point is the
    // LAST 'v' whose remainder is all digits (new-style ids also appear as
    // cs/0112017v1; the base itself may contain letters).
    let id = match tail.rfind('v') {
        Some(vpos)
            if vpos > 0
                && !tail[vpos + 1..].is_empty()
                && tail[vpos + 1..].bytes().all(|b| b.is_ascii_digit()) =>
        {
            tail[..vpos].to_string()
        }
        _ => tail.to_string(),
    };
    (id.to_string(), format!("https://arxiv.org/abs/{id}"))
}

fn node_text(n: roxmltree::Node<'_, '_>) -> String {
    n.text().unwrap_or("").trim().to_string()
}

/// Parse the arXiv Atom feed. Namespace-agnostic on purpose: arXiv mixes the
/// Atom namespace with its own `arxiv:` extension namespace.
pub fn parse_arxiv_feed(xml: &str) -> Result<Vec<ArxivEntry>> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| Error::Other(format!("arXiv 返回的 XML 解析失败: {e}")))?;
    let mut out = Vec::new();
    for entry in doc.descendants().filter(|n| n.tag_name().name() == "entry") {
        let raw_id = entry
            .children()
            .find(|n| n.tag_name().name() == "id")
            .map(node_text)
            .unwrap_or_default();
        let title = entry
            .children()
            .find(|n| n.tag_name().name() == "title")
            .map(node_text)
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "); // arXiv titles wrap lines hard; re-join to one line
        if raw_id.is_empty() || title.is_empty() {
            continue;
        }
        let (id, abs_url) = arxiv_id_from_entry_url(&raw_id);
        let summary = entry
            .children()
            .find(|n| n.tag_name().name() == "summary")
            .map(node_text)
            .unwrap_or_default();
        let authors = entry
            .children()
            .filter(|n| n.tag_name().name() == "author")
            .filter_map(|a| {
                a.children()
                    .find(|n| n.tag_name().name() == "name")
                    .map(node_text)
            })
            .filter(|s| !s.is_empty())
            .collect();
        let year = entry
            .children()
            .find(|n| n.tag_name().name() == "published")
            .map(node_text)
            .and_then(|p| p.get(0..4)?.parse::<i64>().ok());
        let pdf_url = entry
            .children()
            .find(|n| {
                n.tag_name().name() == "link"
                    && n.attribute("type") == Some("application/pdf")
            })
            .and_then(|l| l.attribute("href"))
            .map(|h| h.to_string());
        out.push(ArxivEntry {
            id,
            title,
            summary,
            authors,
            year,
            abs_url,
            pdf_url,
        });
    }
    Ok(out)
}

pub fn fetch_arxiv(query: &str, max: usize) -> Result<Vec<ArxivEntry>> {
    let resp = http_client()?
        .get("https://export.arxiv.org/api/query")
        .query(&[
            ("search_query", query),
            ("start", "0"),
            ("max_results", &max.min(50).to_string()),
            ("sortBy", "relevance"),
            ("sortOrder", "descending"),
        ])
        .send()
        .map_err(|e| Error::Other(format!("arXiv 请求失败: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!(
            "arXiv 返回 {} —— 检索式可能不合法或触发了限流",
            resp.status()
        )));
    }
    let xml = resp.text().map_err(|e| Error::Other(format!("arXiv 响应读取失败: {e}")))?;
    parse_arxiv_feed(&xml)
}

// ---- PubMed ----------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PubmedEntry {
    pub pmid: String,
    pub title: String,
    pub abstract_text: Option<String>,
    pub authors: Vec<String>,
    pub year: Option<i64>,
    pub venue: Option<String>,
    pub doi: Option<String>,
    pub url: String,
}

/// Flatten a mixed-content node ("Attention Is <i>All</i> You Need") to text.
fn deep_text(n: roxmltree::Node<'_, '_>) -> String {
    let mut s = String::new();
    for d in n.descendants() {
        if d.is_text() {
            s.push_str(d.text().unwrap_or(""));
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn child<'a, 'input>(
    n: roxmltree::Node<'a, 'input>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    n.children().find(|c| c.tag_name().name() == name)
}

pub fn parse_pubmed_set(xml: &str) -> Result<Vec<PubmedEntry>> {
    let doc = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: true, // PubmedArticleSet ships a <!DOCTYPE ...> header
            ..Default::default()
        },
    )
    .map_err(|e| Error::Other(format!("PubMed 返回的 XML 解析失败: {e}")))?;
    let mut out = Vec::new();
    for article in doc
        .descendants()
        .filter(|n| n.tag_name().name() == "PubmedArticle")
    {
        let citation = match child(article, "MedlineCitation") {
            Some(c) => c,
            None => continue,
        };
        let pmid = child(citation, "PMID").map(node_text).unwrap_or_default();
        let art = match child(citation, "Article") {
            Some(a) => a,
            None => continue,
        };
        let title = child(art, "ArticleTitle").map(deep_text).unwrap_or_default();
        if pmid.is_empty() || title.is_empty() {
            continue;
        }
        let abstract_text = child(art, "Abstract").map(|abs| {
            abs.children()
                .filter(|n| n.tag_name().name() == "AbstractText")
                .map(|t| {
                    let label = t.attribute("Label").unwrap_or("");
                    let body = deep_text(t);
                    if label.is_empty() {
                        body
                    } else {
                        format!("{label}: {body}")
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        });
        let authors = child(art, "AuthorList")
            .map(|list| {
                list.children()
                    .filter(|n| n.tag_name().name() == "Author")
                    .filter_map(|a| {
                        if let Some(col) = child(a, "CollectiveName") {
                            return Some(deep_text(col));
                        }
                        let last = child(a, "LastName").map(node_text)?;
                        let initials = child(a, "Initials").map(node_text).unwrap_or_default();
                        Some(format!("{last} {initials}").trim().to_string())
                    })
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let year = child(art, "Journal")
            .and_then(|j| child(j, "JournalIssue"))
            .and_then(|i| child(i, "PubDate"))
            .and_then(|pd| {
                if let Some(y) = child(pd, "Year").map(node_text) {
                    y.parse::<i64>().ok()
                } else {
                    child(pd, "MedlineDate").map(node_text).and_then(|md| {
                        md.as_bytes()
                            .windows(4)
                            .position(|w| w.iter().all(|b| b.is_ascii_digit()))
                            .and_then(|p| md[p..p + 4].parse::<i64>().ok())
                    })
                }
            });
        let venue = child(art, "Journal")
            .and_then(|j| child(j, "ISOAbbreviation"))
            .map(node_text)
            .filter(|s| !s.is_empty());
        let doi = article
            .descendants()
            .find(|n| {
                n.tag_name().name() == "ArticleId" && n.attribute("IdType") == Some("doi")
            })
            .map(node_text)
            .filter(|s| !s.is_empty());
        out.push(PubmedEntry {
            url: format!("https://pubmed.ncbi.nlm.nih.gov/{pmid}/"),
            pmid,
            title,
            abstract_text: abstract_text.filter(|s| !s.is_empty()),
            authors,
            year,
            venue,
            doi,
        });
    }
    Ok(out)
}

/// esearch (JSON, paginated by retmax) then efetch (XML with abstracts). Two
/// requests per query — anonymous NCBI allows 3/s, so no throttling needed at
/// these limits.
pub fn fetch_pubmed(query: &str, max: usize, api_key: Option<&str>) -> Result<Vec<PubmedEntry>> {
    let client = http_client()?;
    let mut search_req = client
        .get(format!("{PUBMED_BASE}/esearch.fcgi"))
        .query(&[
            ("db", "pubmed"),
            ("term", query),
            ("retmax", max.min(50).to_string().as_str()),
            ("retmode", "json"),
            ("sort", "relevance"),
        ]);
    if let Some(k) = api_key {
        search_req = search_req.query(&[("api_key", k)]);
    }
    let resp = search_req
        .send()
        .map_err(|e| Error::Other(format!("PubMed esearch 请求失败: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!(
            "PubMed esearch 返回 {} —— 检索式可能不合法或触发了限流",
            resp.status()
        )));
    }
    let json: serde_json::Value = resp
        .json()
        .map_err(|e| Error::Other(format!("PubMed esearch 响应解析失败: {e}")))?;
    let result = &json["esearchresult"];
    if result.get("error").is_some() {
        return Err(Error::Other(format!(
            "PubMed 检索失败: {}",
            result["error"].as_str().unwrap_or("unknown")
        )));
    }
    let ids: Vec<String> = result["idlist"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let id_list = ids.join(",");
    let mut fetch_req = client
        .get(format!("{PUBMED_BASE}/efetch.fcgi"))
        .query(&[("db", "pubmed"), ("id", id_list.as_str()), ("retmode", "xml")]);
    if let Some(k) = api_key {
        fetch_req = fetch_req.query(&[("api_key", k)]);
    }
    let resp = fetch_req
        .send()
        .map_err(|e| Error::Other(format!("PubMed efetch 请求失败: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!(
            "PubMed efetch 返回 {} —— 稍后重试或配置 ncbi_api_key",
            resp.status()
        )));
    }
    let xml = resp.text().map_err(|e| Error::Other(format!("PubMed 响应读取失败: {e}")))?;
    parse_pubmed_set(&xml)
}

// ---- ingest ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ResearchFetchSummary {
    pub fetched: usize,
    pub imported: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
    pub titles_sample: Vec<String>,
}

/// The TEXT source body. One stable shape for both connectors so downstream
/// (chunking/embedding/citation) sees identical structure.
pub fn body_text(title: &str, authors: &[String], year: Option<i64>, venue: Option<&str>, doi: Option<&str>, url: &str, abstract_text: Option<&str>, extra: Option<&str>) -> String {
    let mut s = String::new();
    s.push_str(title.trim());
    s.push_str("\n\n");
    if !authors.is_empty() {
        s.push_str(&format!("Authors: {}\n", authors.join("; ")));
    }
    let mut meta: Vec<String> = Vec::new();
    if let Some(y) = year {
        meta.push(format!("Year: {y}"));
    }
    if let Some(v) = venue.map(str::trim).filter(|v| !v.is_empty()) {
        meta.push(format!("Venue: {v}"));
    }
    if let Some(d) = doi.map(str::trim).filter(|d| !d.is_empty()) {
        meta.push(format!("DOI: {d}"));
    }
    if !meta.is_empty() {
        s.push_str(&meta.join(" · "));
        s.push('\n');
    }
    s.push_str(&format!("URL: {url}\n"));
    if let Some(e) = extra.map(str::trim).filter(|e| !e.is_empty()) {
        s.push_str(&format!("{e}\n"));
    }
    if let Some(a) = abstract_text.map(str::trim).filter(|a| !a.is_empty()) {
        s.push_str("\nAbstract:\n");
        s.push_str(a);
        s.push('\n');
    }
    s
}

fn origin_exists(c: &rusqlite::Connection, subject_id: &str, origin: &str) -> Result<bool> {
    let hit: Option<i64> = c
        .query_row(
            "SELECT 1 FROM sources WHERE subject_id=?1 AND origin=?2 LIMIT 1",
            rusqlite::params![subject_id, origin],
            |r| r.get(0),
        )
        .optional()?;
    Ok(hit.is_some())
}

use rusqlite::OptionalExtension;

/// Fetch from one built-in connector and push every hit through add_source.
/// HTTP happens on the blocking pool; ingest is async because add_source is.
pub async fn fetch_and_ingest(
    app: tauri::AppHandle,
    connector_id: &str,
    query: &str,
    max: usize,
    subject_id: String,
    topic_id: Option<String>,
    with_pdf: bool,
) -> Result<ResearchFetchSummary> {
    let q = query.trim().to_string();
    if q.is_empty() {
        return Err(Error::Other("检索式不能为空".into()));
    }
    let cid = connector_id.to_string();
    let http_app = app.clone();

    // HTTP first (blocking pool), no DB lock held.
    let fetched = tauri::async_runtime::spawn_blocking(move || -> Result<Vec<Item>> {
        match cid.as_str() {
            "arxiv" => Ok(fetch_arxiv(&q, max)?
                    .into_iter()
                    .map(|e| Item {
                        id: e.id.clone(),
                        title: e.title.clone(),
                        url: e.abs_url.clone(),
                        text: body_text(
                            &e.title,
                            &e.authors,
                            e.year,
                            Some("arXiv"),
                            None,
                            &e.abs_url,
                            Some(&e.summary),
                            e.pdf_url.as_deref().map(|p| format!("PDF: {p}")).as_deref(),
                        ),
                        pdf: e.pdf_url,
                    })
                    .collect()),
            "pubmed" => {
                let key = {
                    let state = http_app.state::<crate::db::AppState>();
                    let c = state.db.lock().unwrap();
                    crate::repo::get_setting(&c, "ncbi_api_key")?
                };
                Ok(fetch_pubmed(&q, max, key.as_deref())?
                    .into_iter()
                    .map(|e| Item {
                        id: e.pmid.clone(),
                        title: e.title.clone(),
                        url: e.url.clone(),
                        text: body_text(
                            &e.title,
                            &e.authors,
                            e.year,
                            e.venue.as_deref(),
                            e.doi.as_deref(),
                            &e.url,
                            e.abstract_text.as_deref(),
                            None,
                        ),
                        pdf: None,
                    })
                    .collect())
            }
            other => Err(Error::Other(format!("未知内置连接器: {other}"))),
        }
    })
    .await
    .map_err(|e| Error::Other(format!("background task failed: {e}")))??;

    let mut summary = ResearchFetchSummary {
        fetched: fetched.len(),
        imported: 0,
        skipped: 0,
        errors: Vec::new(),
        titles_sample: Vec::new(),
    };
    let mut seen: HashSet<String> = HashSet::new();
    let tag = format!("research:{connector_id}");
    for item in fetched {
        if !seen.insert(item.url.clone()) {
            summary.skipped += 1;
            continue;
        }
        let already = {
            let state = app.state::<crate::db::AppState>();
            let c = state.db.lock().unwrap();
            origin_exists(&c, &subject_id, &item.url)
        };
        match already {
            Ok(true) => {
                summary.skipped += 1;
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                if summary.errors.len() < 5 {
                    summary.errors.push(e.to_string());
                }
                continue;
            }
        }
        let input = crate::models::AddSourceInput {
            subject_id: subject_id.clone(),
            topic_id: topic_id.clone(),
            name: Some(item.title.clone()),
            kind: Some("txt".into()), // pin txt: `url` stays metadata / dedupe origin
            text: Some(item.text.clone()),
            path: None,
            url: Some(item.url.clone()),
            tags: vec![tag.clone()],
        };
        match crate::commands::add_source(app.clone(), input).await {
            Ok(_) => {
                summary.imported += 1;
                if summary.titles_sample.len() < 8 {
                    summary.titles_sample.push(item.title.clone());
                }
                // Optional PDF full text as its own source (arXiv only).
                if with_pdf {
                    if let Some(pdf) = &item.pdf {
                        let already_pdf = {
                            let state = app.state::<crate::db::AppState>();
                            let c = state.db.lock().unwrap();
                            origin_exists(&c, &subject_id, pdf)
                        };
                        if matches!(already_pdf, Ok(false)) {
                            let pdf_input = crate::models::AddSourceInput {
                                subject_id: subject_id.clone(),
                                topic_id: topic_id.clone(),
                                name: Some(format!("{}（PDF 全文）", item.title)),
                                kind: None, // let detect_kind route the pdf pipeline
                                text: None,
                                path: None,
                                url: Some(pdf.clone()),
                                tags: vec![tag.clone()],
                            };
                            if let Err(e) = crate::commands::add_source(app.clone(), pdf_input).await {
                                if summary.errors.len() < 5 {
                                    summary.errors.push(format!("PDF {}: {e}", item.id));
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                if summary.errors.len() < 5 {
                    summary.errors.push(format!("{}: {e}", item.id));
                }
            }
        }
    }
    Ok(summary)
}

/// Normalized ingest item — same shape for both APIs.
struct Item {
    id: String,
    title: String,
    url: String,
    text: String,
    pdf: Option<String>,
}

// ---- Tauri command surface -------------------------------------------------

#[tauri::command]
pub fn research_connector_list() -> Vec<ResearchConnector> {
    list()
}

/// Long-running: network fetch + N ingest pipeline runs.
#[tauri::command]
pub async fn research_connector_fetch(
    app: tauri::AppHandle,
    connector_id: String,
    query: String,
    max: usize,
    subject_id: String,
    topic_id: Option<String>,
    with_pdf: bool,
) -> Result<ResearchFetchSummary> {
    fetch_and_ingest(app, &connector_id, &query, max, subject_id, topic_id, with_pdf).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const ATOM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <link href="http://arxiv.org/api/query" rel="self" type="application/atom+xml"/>
  <title>ArXiv Query</title>
  <entry>
    <id>http://arxiv.org/abs/1706.03762v7</id>
    <updated>2023-08-03T02:13:07Z</updated>
    <published>2017-06-12T17:57:34Z</published>
    <title>Attention Is
       All You Need</title>
    <summary>  The dominant sequence transduction models are based on
  recurrent networks. We propose the Transformer.
  </summary>
    <author><name>Ashish Vaswani</name></author>
    <author><name>Noam Shazeer</name></author>
    <link href="http://arxiv.org/abs/1706.03762v7" rel="alternate" type="text/html"/>
    <link href="http://arxiv.org/pdf/1706.03762v7" rel="related" type="application/pdf"/>
    <category term="cs.CL" scheme="http://arxiv.org/schemas/atom"/>
  </entry>
  <entry>
    <id>http://arxiv.org/abs/2401.99999</id>
    <published>2024-01-30T00:00:00Z</published>
    <title>Second Paper</title>
    <summary>s2</summary>
  </entry>
</feed>"#;

    const PUBMED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE PubmedArticleSet PUBLIC "-//NLM//DTD PubMedArticle, 1st January 2024//EN" "https://dtd.nlm.nih.gov/ncbi/pubmed/out/pubmed_240101.dtd">
<PubmedArticleSet>
  <PubmedArticle>
    <MedlineCitation Status="In-Data-Review" Owner="NLM">
      <PMID Version="1">30763937</PMID>
      <Article PubModel="Print-Electronic">
        <Journal>
          <ISSN>1097-4199</ISSN>
          <JournalIssue CitedMedium="Internet">
            <Volume>101</Volume>
            <PubDate>
              <Year>2019</Year>
              <Month>Jan</Month>
            </PubDate>
          </JournalIssue>
          <ISOAbbreviation>Neuron</ISOAbbreviation>
          <Title>Neuron.</Title>
        </Journal>
        <ArticleTitle>Attention Is <i>All</i> You Need: a neuroscience view.</ArticleTitle>
        <Abstract>
          <AbstractText Label="BACKGROUND">Transformers replaced RNNs.</AbstractText>
          <AbstractText Label="METHODS">We survey the evidence.</AbstractText>
        </Abstract>
        <AuthorList CompleteYN="Y">
          <Author><LastName>Vaswani</LastName><ForeName>Ashish</ForeName><Initials>A</Initials></Author>
          <Author><CollectiveName>DeepMind Team</CollectiveName></Author>
        </AuthorList>
      </Article>
    </MedlineCitation>
    <PubmedData>
      <ArticleIdList>
        <ArticleId IdType="pubmed">30763937</ArticleId>
        <ArticleId IdType="doi">10.1016/j.neuron.2018.12.003</ArticleId>
      </ArticleIdList>
    </PubmedData>
  </PubmedArticle>
  <PubmedArticle>
    <MedlineCitation Status="Publisher" Owner="NLM">
      <PMID Version="1">99999999</PMID>
      <Article>
        <Journal>
          <JournalIssue><PubDate><MedlineDate>2023 Jan-Feb</MedlineDate></PubDate></JournalIssue>
          <ISOAbbreviation>J Neurophysiol</ISOAbbreviation>
        </Journal>
        <ArticleTitle>Medline-date only paper</ArticleTitle>
      </Article>
    </MedlineCitation>
  </PubmedArticle>
</PubmedArticleSet>"#;

    #[test]
    fn atom_entries_normalize_ids_titles_and_pdf() {
        let entries = parse_arxiv_feed(ATOM).unwrap();
        assert_eq!(entries.len(), 2);
        let e = &entries[0];
        assert_eq!(e.id, "1706.03762");
        assert_eq!(e.abs_url, "https://arxiv.org/abs/1706.03762");
        assert_eq!(e.pdf_url.as_deref(), Some("http://arxiv.org/pdf/1706.03762v7"));
        assert_eq!(e.title, "Attention Is All You Need"); // re-wrapped to one line
        assert_eq!(e.year, Some(2017));
        assert_eq!(e.authors, vec!["Ashish Vaswani", "Noam Shazeer"]);
        assert!(e.summary.starts_with("The dominant"));
        assert_eq!(entries[1].pdf_url, None);
    }

    #[test]
    fn pubmed_parse_covers_markup_abstract_labels_and_medlinedate() {
        let entries = parse_pubmed_set(PUBMED).unwrap();
        assert_eq!(entries.len(), 2);
        let e = &entries[0];
        assert_eq!(e.pmid, "30763937");
        assert_eq!(e.title, "Attention Is All You Need: a neuroscience view.");
        assert_eq!(e.doi.as_deref(), Some("10.1016/j.neuron.2018.12.003"));
        assert_eq!(e.year, Some(2019));
        assert_eq!(e.venue.as_deref(), Some("Neuron"));
        assert_eq!(
            e.abstract_text.as_deref(),
            Some("BACKGROUND: Transformers replaced RNNs. METHODS: We survey the evidence.")
        );
        assert_eq!(e.authors, vec!["Vaswani A", "DeepMind Team"]);
        assert_eq!(e.url, "https://pubmed.ncbi.nlm.nih.gov/30763937/");
        // second paper: MedlineDate fallback year, no abstract/authors/doi
        let e2 = &entries[1];
        assert_eq!(e2.year, Some(2023));
        assert_eq!(e2.abstract_text, None);
        assert_eq!(e2.doi, None);
    }

    #[test]
    fn body_text_has_stable_shape() {
        let b = body_text(
            "T",
            &["A B".into(), "C D".into()],
            Some(2024),
            Some("NeurIPS"),
            Some("10.1/x"),
            "https://x",
            Some("Abs."),
            Some("PDF: https://pdf"),
        );
        assert_eq!(
            b,
            "T\n\nAuthors: A B; C D\nYear: 2024 · Venue: NeurIPS · DOI: 10.1/x\nURL: https://x\nPDF: https://pdf\n\nAbstract:\nAbs.\n"
        );
        // minimal shape: no authors/meta/abstract/extra
        let b2 = body_text("T2", &[], None, None, None, "https://y", None, None);
        assert_eq!(b2, "T2\n\nURL: https://y\n");
    }

    #[test]
    fn list_describes_both_connectors() {
        let l = list();
        assert_eq!(l.len(), 2);
        assert!(l.iter().any(|c| c.id == "arxiv" && c.supports_pdf));
        assert!(l.iter().any(|c| c.id == "pubmed" && !c.supports_pdf));
    }
}
