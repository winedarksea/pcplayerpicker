use leptos::prelude::*;

pub fn use_page_meta(title: &'static str, description: &'static str) {
    Effect::new(move |_| {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };

        document.set_title(title);
        set_meta_content(&document, r#"meta[name="description"]"#, description);
        set_meta_content(&document, r#"meta[property="og:title"]"#, title);
        set_meta_content(&document, r#"meta[property="og:description"]"#, description);
        set_meta_content(&document, r#"meta[name="twitter:title"]"#, title);
        set_meta_content(
            &document,
            r#"meta[name="twitter:description"]"#,
            description,
        );
    });
}

fn set_meta_content(document: &web_sys::Document, selector: &str, content: &str) {
    if let Ok(Some(element)) = document.query_selector(selector) {
        let _ = element.set_attribute("content", content);
    }
}
