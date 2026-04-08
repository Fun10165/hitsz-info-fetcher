use crate::models::{ExtractedLink, PageSnapshot};
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

#[cfg(test)]
mod tests {
    use super::extract_page_snapshot;
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
}
