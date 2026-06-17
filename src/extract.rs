use crate::models::{ExtractedLink, NoticeItem, PageSnapshot};
use scraper::{Html, Selector};
use url::Url;

pub fn extract_page_snapshot(final_url: &Url, html: &str) -> PageSnapshot {
    let document = Html::parse_document(html);
    let title_selector = Selector::parse("title").expect("valid selector");
    let anchor_selector = Selector::parse("a[href]").expect("valid selector");

    let title = document
        .select(&title_selector)
        .next()
        .map(|node| node.text().collect::<String>().trim().to_owned())
        .filter(|value| !value.is_empty());

    let mut links = Vec::new();
    for node in document.select(&anchor_selector) {
        let text = node.text().collect::<String>();
        let title = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() {
            continue;
        }

        let Some(raw_href) = node.value().attr("href") else {
            continue;
        };

        let Ok(url) = final_url.join(raw_href) else {
            continue;
        };

        links.push(ExtractedLink {
            title,
            url: url.to_string(),
        });
    }

    PageSnapshot {
        final_url: final_url.to_string(),
        title,
        links,
        html: html.to_owned(),
    }
}

/// Extracts structured notice items from the info portal list page.
/// Each notice is an `<li>` inside `.Newslist ul` containing:
///   `<span>部门  日期</span>【分类】<a href="...">标题</a>`
pub fn extract_notice_items(html: &str, base_url: &Url) -> Vec<NoticeItem> {
    let document = Html::parse_document(html);
    let li_selector = Selector::parse(".Newslist ul li").expect("valid selector");
    let span_selector = Selector::parse("span").expect("valid selector");
    let a_selector = Selector::parse("a[href]").expect("valid selector");

    let mut notices = Vec::new();
    for li in document.select(&li_selector) {
        // Span: "部门   YYYY-MM-DD"
        let span_text = li
            .select(&span_selector)
            .next()
            .map(|s| s.text().collect::<String>())
            .unwrap_or_default();
        let (department, date) = split_span(&span_text);

        // <a>: title + url
        let (title, url) = li
            .select(&a_selector)
            .next()
            .and_then(|a| {
                let title = a.text().collect::<String>().trim().to_owned();
                let href = a.value().attr("href")?;
                let url = base_url.join(href).ok()?.to_string();
                Some((title, url))
            })
            .unwrap_or_default();

        // Category: text between </span> and <a>, like "【工作通知】"
        let category = li
            .text()
            .collect::<String>()
            .trim()
            .to_owned();
        let category = extract_bracket_tag(&category, &span_text, &title);

        if !date.is_empty() && !title.is_empty() {
            notices.push(NoticeItem {
                title,
                url,
                date,
                department,
                category,
            });
        }
    }
    notices
}

/// Returns the URL of the "next page" link, if present.
pub fn find_next_page_url(html: &str, base_url: &Url) -> Option<Url> {
    let document = Html::parse_document(html);
    let a_selector = Selector::parse("a[href]").expect("valid selector");
    for a in document.select(&a_selector) {
        let text = a.text().collect::<String>();
        if text.contains("下页") || text.contains("下一页") {
            return a.value().attr("href").and_then(|h| base_url.join(h).ok());
        }
    }
    None
}

/// Splits a span like "人力资源处   2026-06-03" into (department, date).
fn split_span(raw: &str) -> (String, String) {
    let raw = raw.trim();
    // Date is the last 10 chars: YYYY-MM-DD
    if raw.len() >= 10 {
        let maybe_date = &raw[raw.len() - 10..];
        if maybe_date.chars().filter(|&c| c == '-').count() == 2
            && maybe_date.chars().all(|c| c.is_ascii_digit() || c == '-')
        {
            let dept = raw[..raw.len() - 10].trim().to_owned();
            return (dept, maybe_date.to_owned());
        }
    }
    (raw.to_owned(), String::new())
}

/// Extracts a 【…】bracket tag from the full LI text, excluding the span and title.
fn extract_bracket_tag(full_text: &str, span: &str, title: &str) -> String {
    let rest = full_text
        .strip_prefix(span)
        .unwrap_or(full_text)
        .trim();
    let rest = rest.strip_suffix(title).unwrap_or(rest).trim();
    let start = rest.find('【').unwrap_or(rest.len());
    let end = rest[start..].find('】').map(|i| start + i + 3).unwrap_or(rest.len());
    rest[start..end.min(rest.len())].trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    #[test]
    fn extracts_title_and_absolute_links() {
        let url = Url::parse("https://info.hitsz.edu.cn/list").expect("valid url");
        let html = r#"
            <html>
              <head><title>通知公告</title></head>
              <body>
                <a href="/item/1"> 第一条通知 </a>
                <a href="https://example.com/b">第二条</a>
              </body>
            </html>
        "#;

        let snapshot = extract_page_snapshot(&url, html);
        assert_eq!(snapshot.title.as_deref(), Some("通知公告"));
        assert_eq!(snapshot.links.len(), 2);
        assert_eq!(snapshot.links[0].url, "https://info.hitsz.edu.cn/item/1");
        assert_eq!(snapshot.links[0].title, "第一条通知");
    }

    #[test]
    fn extracts_notice_items_from_list_page() {
        let url = Url::parse("http://info.hitsz.edu.cn/list.jsp?wbtreeid=1053").expect("valid url");
        let html = r#"
            <html><body>
            <div class="Newslist"><ul>
              <li><span>人力资源处&nbsp;&nbsp;&nbsp;2026-06-16</span>【工作通知】<a href="content.jsp?wbnewsid=9219">关于启动项目通知</a></li>
              <li><span>教务部&nbsp;&nbsp;&nbsp;2026-06-15</span>【公告公示】<a href="content.jsp?wbnewsid=9218">考试安排通知</a></li>
            </ul></div>
            </body></html>
        "#;

        let notices = extract_notice_items(html, &url);
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0].title, "关于启动项目通知");
        assert_eq!(notices[0].date, "2026-06-16");
        assert_eq!(notices[0].department, "人力资源处");
        assert_eq!(notices[0].category, "【工作通知】");
        assert!(notices[0].url.contains("wbnewsid=9219"));
        assert_eq!(notices[1].date, "2026-06-15");
    }

    #[test]
    fn finds_next_page_url() {
        let url = Url::parse("http://info.hitsz.edu.cn/list.jsp?wbtreeid=1053").expect("valid url");
        let html = r#"<a href="?totalpage=295&PAGENUM=2&wbtreeid=1053">下页</a>"#;

        let next = find_next_page_url(html, &url);
        assert!(next.is_some());
        assert!(next.unwrap().as_str().contains("PAGENUM=2"));
    }

    #[test]
    fn splits_span_correctly() {
        assert_eq!(split_span("人力资源处   2026-06-16"), ("人力资源处".into(), "2026-06-16".into()));
        assert_eq!(split_span("教务部2026-06-15"), ("教务部".into(), "2026-06-15".into()));
        assert_eq!(split_span("no date here"), ("no date here".into(), "".into()));
    }
}
