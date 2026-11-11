//! Source tracking: URLs cited or opened by agents, and numbered citations in the final report.

use super::store::Source;
use serde_json::Value;

/// Canonical form used to dedupe: http(s) only, no fragment, no `utm_*` query pairs.
pub fn normalize(raw: &str) -> Option<String> {
    let mut u = url::Url::parse(raw).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    u.set_fragment(None);
    let kept: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| !k.starts_with("utm_"))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        u.set_query(None);
    } else if kept.len() != u.query_pairs().count() {
        u.query_pairs_mut().clear().extend_pairs(kept);
    }
    Some(u.to_string())
}

/// `url_citation` annotations on output messages.
pub fn annotations(items: &[Value]) -> Vec<(String, String)> {
    let mut out = vec![];
    for it in items.iter().filter(|i| i["type"] == "message") {
        let Some(parts) = it["content"].as_array() else { continue };
        for p in parts.iter().filter(|p| p["type"] == "output_text") {
            let Some(anns) = p["annotations"].as_array() else { continue };
            for a in anns.iter().filter(|a| a["type"] == "url_citation") {
                if let Some(u) = a["url"].as_str() {
                    out.push((u.to_string(), a["title"].as_str().unwrap_or("").to_string()));
                }
            }
        }
    }
    out
}

/// Pages opened by `web_search` and URLs passed to `fetch_url`.
pub fn opened(items: &[Value]) -> Vec<(String, String)> {
    let mut out = vec![];
    for it in items {
        match it["type"].as_str() {
            Some("web_search_call") if it["action"]["type"] != "search" => {
                if let Some(u) = it["action"]["url"].as_str() {
                    out.push((u.to_string(), String::new()));
                }
            }
            Some("function_call") if it["name"] == "fetch_url" => {
                let args: Value = serde_json::from_str(it["arguments"].as_str().unwrap_or("")).unwrap_or(Value::Null);
                if let Some(u) = args["url"].as_str() {
                    out.push((u.to_string(), String::new()));
                }
            }
            _ => {}
        }
    }
    out
}

/// Markdown links and bare URLs in model text.
pub fn links(text: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let mut pos = 0;
    while let Some(rel) = text[pos..].find("http") {
        let start = pos + rel;
        let rest = &text[start..];
        if !(rest.starts_with("http://") || rest.starts_with("https://")) {
            pos = start + 4;
            continue;
        }
        let mut depth = 0usize;
        let mut end = rest.len();
        for (i, c) in rest.char_indices() {
            match c {
                '(' => depth += 1,
                ')' if depth == 0 => {
                    end = i;
                    break;
                }
                ')' => depth -= 1,
                c if c.is_whitespace() || "<>\"'`|]".contains(c) => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
        let url = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
        let prefix = &text[..start];
        let title = prefix
            .strip_suffix("](")
            .and_then(|before| {
                let line = &before[before.rfind('\n').map_or(0, |p| p + 1)..];
                line.rfind('[').map(|o| line[o + 1..].to_string())
            })
            .unwrap_or_default();
        out.push((url.to_string(), title));
        pos = start + end.max(1);
    }
    out
}

/// Add sightings to `sources`. `agent` is the researcher index; `None` is the synthesizer (cited in the final report).
pub fn merge(sources: &mut Vec<Source>, found: Vec<(String, String)>, agent: Option<usize>) {
    for (raw, title) in found {
        let Some(url) = normalize(&raw) else { continue };
        let i = match sources.iter().position(|s| s.url == url) {
            Some(i) => i,
            None => {
                sources.push(Source { url, title: String::new(), agents: vec![], in_final: false });
                sources.len() - 1
            }
        };
        let s = &mut sources[i];
        if s.title.is_empty() && !title.trim().is_empty() {
            s.title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        }
        match agent {
            Some(a) => {
                if let Err(at) = s.agents.binary_search(&a) {
                    s.agents.insert(at, a);
                }
            }
            None => s.in_final = true,
        }
    }
}

/// Numbers cited as `[3]`, `[1, 4]`, `[2–5]`; sorted and unique.
pub fn cited_numbers(text: &str) -> Vec<usize> {
    let mut out = vec![];
    for (i, _) in text.match_indices('[') {
        let rest = &text[i + 1..];
        let Some((j, _)) = rest.char_indices().take_while(|(j, _)| *j <= 24).find(|(_, c)| *c == ']') else {
            continue;
        };
        let inner = &rest[..j];
        if !inner.chars().any(|c| c.is_ascii_digit())
            || !inner.chars().all(|c| c.is_ascii_digit() || ", -–".contains(c))
        {
            continue;
        }
        for part in inner.split(',') {
            let part = part.trim();
            if let Some((a, b)) = part.split_once(['-', '–']) {
                if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>())
                    && a <= b
                    && b - a <= 50
                {
                    out.extend(a..=b);
                }
            } else if let Ok(n) = part.parse::<usize>() {
                out.push(n);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Flag sources the text cites by number.
pub fn mark_cited(sources: &mut [Source], text: &str) {
    for n in cited_numbers(text) {
        if (1..=sources.len()).contains(&n) {
            sources[n - 1].in_final = true;
        }
    }
}

/// One `- [n] title — url` line per kept source. Numbers are global, so filtered lists have gaps.
pub fn render_list(sources: &[Source], keep: impl Fn(&Source) -> bool) -> String {
    let mut out = String::new();
    for (i, s) in sources.iter().enumerate().filter(|(_, s)| keep(s)) {
        if s.title.is_empty() {
            out.push_str(&format!("- [{}] {}\n", i + 1, s.url));
        } else {
            out.push_str(&format!("- [{}] {} — {}\n", i + 1, s.title, s.url));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn src(url: &str) -> Source {
        Source { url: url.into(), title: String::new(), agents: vec![], in_final: false }
    }

    #[test]
    fn normalize_strips_tracking() {
        assert_eq!(normalize("https://a.com/x?utm_source=openai#frag").as_deref(), Some("https://a.com/x"));
        assert_eq!(normalize("https://a.com/x?id=2&utm_medium=y").as_deref(), Some("https://a.com/x?id=2"));
        assert_eq!(normalize("ftp://a.com"), None);
    }

    #[test]
    fn links_finds_markdown_and_bare() {
        assert_eq!(
            links("see [Foo Bar](https://a.com/p) and https://b.org/q."),
            vec![("https://a.com/p".into(), "Foo Bar".into()), ("https://b.org/q".into(), String::new())]
        );
        assert_eq!(
            links("[W](https://en.wikipedia.org/wiki/Foo_(bar)) next"),
            vec![("https://en.wikipedia.org/wiki/Foo_(bar)".into(), "W".into())]
        );
    }

    #[test]
    fn merge_dedupes_and_tracks_agents() {
        let mut s = vec![];
        merge(&mut s, vec![("https://a.com/x?utm_source=openai".into(), String::new())], Some(2));
        merge(&mut s, vec![("https://a.com/x".into(), " The\n Title ".into())], Some(0));
        merge(&mut s, vec![("https://a.com/x".into(), "Other".into())], Some(2));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].agents, vec![0, 2]);
        assert_eq!(s[0].title, "The Title");
        assert!(!s[0].in_final);
        merge(&mut s, vec![("https://a.com/x".into(), String::new())], None);
        assert!(s[0].in_final);
    }

    #[test]
    fn cited_numbers_and_mark() {
        assert_eq!(cited_numbers("a [1] b [2, 4] c [5–6] d [x] e [99]"), vec![1, 2, 4, 5, 6, 99]);
        assert_eq!(cited_numbers("[1][2]"), vec![1, 2]);
        let mut s = vec![src("https://a.com/"), src("https://b.com/"), src("https://c.com/")];
        mark_cited(&mut s, "a [1] b [2, 4] c [99]");
        assert_eq!(s.iter().map(|x| x.in_final).collect::<Vec<_>>(), vec![true, true, false]);
    }

    #[test]
    fn extracts_from_output_items() {
        let items = vec![
            json!({"type":"web_search_call","action":{"type":"open_page","url":"https://open.example/p"}}),
            json!({"type":"web_search_call","action":{"type":"search","query":"q"}}),
            json!({"type":"function_call","name":"fetch_url","arguments":"{\"url\":\"https://f.example/\"}"}),
            json!({"type":"message","content":[{"type":"output_text","text":"x","annotations":[
                {"type":"url_citation","url":"https://a.example/doc","title":"Doc"}]}]}),
        ];
        assert_eq!(annotations(&items), vec![("https://a.example/doc".into(), "Doc".into())]);
        assert_eq!(
            opened(&items),
            vec![("https://open.example/p".into(), String::new()), ("https://f.example/".into(), String::new())]
        );
    }

    #[test]
    fn render_list_keeps_global_numbers() {
        let mut s = vec![src("https://a.com/"), src("https://b.com/")];
        s[1].title = "B".into();
        assert_eq!(render_list(&s, |x| x.url.contains("b.com")), "- [2] B — https://b.com/\n");
        assert_eq!(render_list(&s, |_| true), "- [1] https://a.com/\n- [2] B — https://b.com/\n");
    }
}
