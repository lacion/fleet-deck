//! Atlassian Document Format to Markdown (Jira Cloud REST v3 bodies).
//!
//! The converter keeps paragraphs, headings, lists, code, quotes, rules,
//! links, mentions, tables and images (`![](url)`). A node that it does not
//! know gives its text. It never writes raw HTML: a `<` in the document is
//! plain text for the Markdown renderer, which shows no raw HTML.

use std::collections::BTreeMap;

use serde_json::Value;

/// The URL of a media node, by the file name of its attachment.
pub type MediaUrls = BTreeMap<String, String>;

/// The most nesting levels that the converter follows.
const MAX_DEPTH: usize = 40;

/// The Markdown of an ADF document. `media` maps attachment file names to
/// the URLs that serve their content.
#[must_use]
pub fn to_markdown(doc: &Value, media: &MediaUrls) -> String {
    let mut out = String::new();
    block(doc, media, 0, &mut out);
    out.trim().to_owned()
}

/// An ADF document for plain text: one paragraph for each block that a blank
/// line ends, a hard break for each line inside a block.
#[must_use]
pub fn from_text(text: &str) -> Value {
    let mut paragraphs = Vec::new();
    for block in text.replace("\r\n", "\n").split("\n\n") {
        let mut content = Vec::new();
        for (i, line) in block.trim_matches('\n').split('\n').enumerate() {
            if i > 0 {
                content.push(serde_json::json!({"type": "hardBreak"}));
            }
            if !line.is_empty() {
                content.push(serde_json::json!({"type": "text", "text": line}));
            }
        }
        if !content.is_empty() {
            paragraphs.push(serde_json::json!({"type": "paragraph", "content": content}));
        }
    }
    serde_json::json!({"type": "doc", "version": 1, "content": paragraphs})
}

/// The URLs of the images in `doc`, in order.
#[must_use]
pub fn image_urls(doc: &Value, media: &MediaUrls) -> Vec<String> {
    let mut urls = Vec::new();
    collect_images(doc, media, 0, &mut urls);
    urls
}

fn collect_images(node: &Value, media: &MediaUrls, depth: usize, urls: &mut Vec<String>) {
    if depth > MAX_DEPTH {
        return;
    }
    if node["type"] == "media"
        && let Some(url) = media_url(node, media)
        && !urls.contains(&url)
    {
        urls.push(url);
    }
    for child in children(node) {
        collect_images(child, media, depth + 1, urls);
    }
}

fn children(node: &Value) -> impl Iterator<Item = &Value> {
    node["content"].as_array().into_iter().flatten()
}

fn media_url(node: &Value, media: &MediaUrls) -> Option<String> {
    let attrs = &node["attrs"];
    if attrs["type"] == "external" {
        return attrs["url"].as_str().map(str::to_owned);
    }
    attrs["alt"]
        .as_str()
        .and_then(|name| media.get(name))
        .cloned()
}

/// The plain text of a node and its children.
fn text_of(node: &Value, depth: usize) -> String {
    if depth > MAX_DEPTH {
        return String::new();
    }
    if let Some(t) = node["text"].as_str() {
        return t.to_owned();
    }
    if node["type"] == "hardBreak" {
        return "\n".to_owned();
    }
    if let Some(t) = node["attrs"]["text"].as_str() {
        return t.to_owned();
    }
    children(node).map(|c| text_of(c, depth + 1)).collect()
}

fn inline(node: &Value, media: &MediaUrls, depth: usize) -> String {
    if depth > MAX_DEPTH {
        return String::new();
    }
    match node["type"].as_str().unwrap_or_default() {
        "text" => marked(node),
        "hardBreak" => "  \n".to_owned(),
        "mention" => node["attrs"]["text"]
            .as_str()
            .map_or_else(|| "@user".to_owned(), str::to_owned),
        "emoji" => node["attrs"]["text"]
            .as_str()
            .or(node["attrs"]["shortName"].as_str())
            .unwrap_or_default()
            .to_owned(),
        "inlineCard" | "blockCard" | "embedCard" => {
            let url = node["attrs"]["url"].as_str().unwrap_or_default();
            if url.is_empty() {
                String::new()
            } else {
                format!("<{url}>")
            }
        }
        "media" => image(node, media),
        "date" => node["attrs"]["timestamp"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        "status" => format!("[{}]", node["attrs"]["text"].as_str().unwrap_or_default()),
        _ => children(node)
            .map(|c| inline(c, media, depth + 1))
            .collect(),
    }
}

fn image(node: &Value, media: &MediaUrls) -> String {
    let alt = node["attrs"]["alt"].as_str().unwrap_or_default();
    match media_url(node, media) {
        Some(url) => format!("![{}]({url})", alt.replace(['[', ']'], "")),
        None if alt.is_empty() => "[image]".to_owned(),
        None => format!("[image: {alt}]"),
    }
}

fn marked(node: &Value) -> String {
    let mut text = node["text"].as_str().unwrap_or_default().to_owned();
    let mut link = None;
    let mut code = false;
    for mark in node["marks"].as_array().into_iter().flatten() {
        match mark["type"].as_str().unwrap_or_default() {
            "code" => code = true,
            "link" => link = mark["attrs"]["href"].as_str().map(str::to_owned),
            _ => {}
        }
    }
    if code {
        let ticks = if text.contains('`') { "``" } else { "`" };
        text = format!("{ticks}{text}{ticks}");
    }
    for mark in node["marks"].as_array().into_iter().flatten() {
        text = match mark["type"].as_str().unwrap_or_default() {
            "strong" => format!("**{text}**"),
            "em" => format!("*{text}*"),
            "strike" => format!("~~{text}~~"),
            _ => text,
        };
    }
    match link {
        Some(href) => format!("[{text}]({href})"),
        None => text,
    }
}

fn inline_children(node: &Value, media: &MediaUrls, depth: usize) -> String {
    children(node)
        .map(|c| inline(c, media, depth + 1))
        .collect()
}

fn indent(text: &str, pad: &str) -> String {
    text.lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("{pad}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn block(node: &Value, media: &MediaUrls, depth: usize, out: &mut String) {
    if depth > MAX_DEPTH {
        return;
    }
    match node["type"].as_str().unwrap_or_default() {
        "doc" | "mediaSingle" | "mediaGroup" | "panel" | "expand" | "nestedExpand" => {
            for child in children(node) {
                block(child, media, depth + 1, out);
            }
        }
        "paragraph" => {
            let text = inline_children(node, media, depth);
            if !text.trim().is_empty() {
                out.push_str(&text);
                out.push_str("\n\n");
            }
        }
        "heading" => {
            let level = node["attrs"]["level"].as_u64().unwrap_or(1).clamp(1, 6);
            let hashes = "#".repeat(usize::try_from(level).unwrap_or(1));
            out.push_str(&format!(
                "{hashes} {}\n\n",
                inline_children(node, media, depth)
            ));
        }
        "bulletList" | "orderedList" => {
            let ordered = node["type"] == "orderedList";
            let start = node["attrs"]["order"].as_u64().unwrap_or(1);
            for (i, item) in children(node).enumerate() {
                let mut inner = String::new();
                for child in children(item) {
                    block(child, media, depth + 1, &mut inner);
                }
                let inner = inner.trim_end();
                let marker = if ordered {
                    format!("{}. ", start + u64::try_from(i).unwrap_or(0))
                } else {
                    "- ".to_owned()
                };
                let pad = " ".repeat(marker.len());
                let mut lines = inner.lines();
                out.push_str(&marker);
                out.push_str(lines.next().unwrap_or_default());
                out.push('\n');
                let rest: Vec<&str> = lines.collect();
                if !rest.is_empty() {
                    out.push_str(&indent(&rest.join("\n"), &pad));
                    out.push('\n');
                }
            }
            out.push('\n');
        }
        "codeBlock" => {
            let lang = node["attrs"]["language"].as_str().unwrap_or_default();
            let code = text_of(node, depth);
            // A fence longer than any run of backticks inside the code.
            let longest = code.split(|c| c != '`').map(str::len).max().unwrap_or(0);
            let fence = "`".repeat((longest + 1).max(3));
            out.push_str(&format!("{fence}{lang}\n{code}\n{fence}\n\n"));
        }
        "blockquote" => {
            let mut inner = String::new();
            for child in children(node) {
                block(child, media, depth + 1, &mut inner);
            }
            for line in inner.trim_end().lines() {
                out.push_str(&format!("> {line}\n"));
            }
            out.push('\n');
        }
        "rule" => out.push_str("---\n\n"),
        "table" => {
            for (i, row) in children(node).enumerate() {
                let cells: Vec<String> = children(row)
                    .map(|cell| {
                        let mut inner = String::new();
                        for child in children(cell) {
                            block(child, media, depth + 2, &mut inner);
                        }
                        inner.trim().replace('\n', " ").replace('|', "\\|")
                    })
                    .collect();
                out.push_str(&format!("| {} |\n", cells.join(" | ")));
                if i == 0 {
                    out.push_str(&format!("|{}\n", " --- |".repeat(cells.len())));
                }
            }
            out.push('\n');
        }
        "media" => {
            out.push_str(&image(node, media));
            out.push_str("\n\n");
        }
        "text" | "hardBreak" | "mention" | "emoji" | "inlineCard" | "status" | "date" => {
            out.push_str(&inline(node, media, depth));
            out.push_str("\n\n");
        }
        _ => {
            let text = text_of(node, depth);
            if !text.trim().is_empty() {
                out.push_str(text.trim());
                out.push_str("\n\n");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn md(doc: &Value) -> String {
        to_markdown(doc, &MediaUrls::new())
    }

    #[test]
    fn paragraphs_headings_marks_and_links_convert() {
        let doc = json!({"type":"doc","version":1,"content":[
            {"type":"heading","attrs":{"level":2},"content":[{"type":"text","text":"Plan"}]},
            {"type":"paragraph","content":[
                {"type":"text","text":"Fix "},
                {"type":"text","text":"login","marks":[{"type":"strong"}]},
                {"type":"text","text":" see "},
                {"type":"text","text":"docs","marks":[{"type":"link","attrs":{"href":"https://x.test/d"}}]},
                {"type":"text","text":" and "},
                {"type":"text","text":"run()","marks":[{"type":"code"}]},
                {"type":"hardBreak"},
                {"type":"mention","attrs":{"id":"1","text":"@ana"}}
            ]}
        ]});
        assert_eq!(
            md(&doc),
            "## Plan\n\nFix **login** see [docs](https://x.test/d) and `run()`  \n@ana"
        );
    }

    #[test]
    fn lists_nest_and_numbers_follow_the_start() {
        let doc = json!({"type":"doc","content":[
            {"type":"bulletList","content":[
                {"type":"listItem","content":[
                    {"type":"paragraph","content":[{"type":"text","text":"one"}]},
                    {"type":"bulletList","content":[{"type":"listItem","content":[
                        {"type":"paragraph","content":[{"type":"text","text":"inner"}]}]}]}
                ]},
                {"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"two"}]}]}
            ]},
            {"type":"orderedList","attrs":{"order":3},"content":[
                {"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"c"}]}]}
            ]}
        ]});
        assert_eq!(md(&doc), "- one\n  - inner\n- two\n\n3. c");
    }

    #[test]
    fn code_blocks_use_a_longer_fence_than_their_content() {
        let doc = json!({"type":"doc","content":[
            {"type":"codeBlock","attrs":{"language":"rust"},"content":[{"type":"text","text":"let a = \"```\";"}]}
        ]});
        assert_eq!(md(&doc), "````rust\nlet a = \"```\";\n````");
    }

    #[test]
    fn images_resolve_by_file_name_and_external_urls_pass() {
        let doc = json!({"type":"doc","content":[
            {"type":"mediaSingle","content":[{"type":"media","attrs":{"type":"file","id":"u","alt":"shot.png"}}]},
            {"type":"mediaSingle","content":[{"type":"media","attrs":{"type":"external","url":"https://img.test/a.png"}}]},
            {"type":"mediaSingle","content":[{"type":"media","attrs":{"type":"file","id":"v","alt":"gone.png"}}]}
        ]});
        let media = MediaUrls::from([(
            "shot.png".to_owned(),
            "https://acme.atlassian.net/rest/api/3/attachment/content/10".to_owned(),
        )]);
        assert_eq!(
            to_markdown(&doc, &media),
            "![shot.png](https://acme.atlassian.net/rest/api/3/attachment/content/10)\n\n![](https://img.test/a.png)\n\n[image: gone.png]"
        );
        assert_eq!(
            image_urls(&doc, &media),
            [
                "https://acme.atlassian.net/rest/api/3/attachment/content/10",
                "https://img.test/a.png"
            ]
        );
    }

    #[test]
    fn an_unknown_node_gives_its_text_and_html_stays_text() {
        let doc = json!({"type":"doc","content":[
            {"type":"fancyWidget","content":[{"type":"text","text":"inside"}]},
            {"type":"paragraph","content":[{"type":"text","text":"<script>alert(1)</script>"}]}
        ]});
        assert_eq!(md(&doc), "inside\n\n<script>alert(1)</script>");
    }

    #[test]
    fn plain_text_becomes_paragraphs_with_hard_breaks() {
        let doc = from_text("one\ntwo\n\nthree");
        assert_eq!(
            doc,
            json!({"type":"doc","version":1,"content":[
                {"type":"paragraph","content":[
                    {"type":"text","text":"one"},{"type":"hardBreak"},{"type":"text","text":"two"}]},
                {"type":"paragraph","content":[{"type":"text","text":"three"}]}
            ]})
        );
        assert_eq!(md(&doc), "one  \ntwo\n\nthree");
    }

    #[test]
    fn a_table_gets_a_header_separator() {
        let cell = |t: &str, kind: &str| json!({"type":kind,"content":[{"type":"paragraph","content":[{"type":"text","text":t}]}]});
        let doc = json!({"type":"doc","content":[{"type":"table","content":[
            {"type":"tableRow","content":[cell("a","tableHeader"),cell("b","tableHeader")]},
            {"type":"tableRow","content":[cell("1","tableCell"),cell("2","tableCell")]}
        ]}]});
        assert_eq!(md(&doc), "| a | b |\n| --- | --- |\n| 1 | 2 |");
    }

    #[test]
    fn a_deeply_nested_document_does_not_overflow() {
        let mut node = json!({"type":"text","text":"x"});
        for _ in 0..500 {
            node = json!({"type":"blockquote","content":[node]});
        }
        let doc = json!({"type":"doc","content":[node]});
        let _ = md(&doc);
    }
}
