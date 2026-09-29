//! Render a small, safe DOM vocabulary; never insert model-supplied HTML.
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag};
use wasm_bindgen::prelude::*;
use yew::{prelude::*, virtual_dom::VTag};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = demodexRich, js_name = diagram)]
    fn mount_diagram(element: web_sys::Element, source: &str) -> js_sys::Function;
    #[wasm_bindgen(js_namespace = demodexRich, js_name = copy, catch)]
    async fn copy_raw(source: &str) -> Result<JsValue, JsValue>;
}

#[derive(Properties, PartialEq)]
pub struct Props {
    pub source: AttrValue,
    #[prop_or_default]
    pub item_id: String,
    #[prop_or_default]
    pub file_revision: u64,
}

#[function_component(Message)]
pub fn message(props: &Props) -> Html {
    let formatted = use_state(|| true);
    let copied = use_state(|| false);
    let copy_error = use_state(|| false);
    {
        let copied = copied.clone();
        let copy_error = copy_error.clone();
        use_effect_with(props.source.clone(), move |_| {
            copied.set(false);
            copy_error.set(false);
        });
    }
    let source = props.source.clone();
    let selected = use_state(|| None::<demodex_protocol::MessageLink>);
    let open = { let selected = selected.clone(); Callback::from(move |link| selected.set(Some(link))) };
    let close = { let selected = selected.clone(); Callback::from(move |_| selected.set(None)) };
    let links = use_memo(source.clone(), |source| demodex_protocol::links::extract(source));
    let tree = use_memo((source.clone(), open.clone()), |(source,open)| render_with_links(source, open));
    let toggle = {
        let formatted = formatted.clone();
        Callback::from(move |event: web_sys::Event| {
            formatted.set(
                event
                    .target_unchecked_into::<web_sys::HtmlInputElement>()
                    .checked(),
            )
        })
    };
    let copy = {
        let copied = copied.clone();
        let copy_error = copy_error.clone();
        Callback::from(move |_| {
            let source = source.clone();
            let copied = copied.clone();
            let copy_error = copy_error.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let ok = copy_raw(&source).await.is_ok();
                copied.set(ok);
                copy_error.set(!ok);
            });
        })
    };
    html! {<div class="rich-message">
        {if *formatted && tree.1 {html!{<div class="markdown">{tree.0.clone()}</div>}}else{html!{<pre>{props.source.clone()}</pre>}}}
        {if tree.1 {html!{<div class="message-format-controls"><label><input type="checkbox" checked={*formatted} onchange={toggle}/>{"Format Markdown"}</label><button type="button" onclick={copy}>{if *copied {"Copied raw"} else {"Copy raw"}}</button>{if *copy_error {html!{<span role="status">{"Copy failed"}</span>}}else{Html::default()}}</div>}}else{Html::default()}}
        {if *formatted && !links.is_empty() {html!{<div class="message-destinations">{for links.iter().enumerate().map(|(index,link)| {
            let chosen=link.clone(); let open=open.clone();
            html!{<div class="message-destination"><button type="button" class="message-link" onclick={Callback::from(move |_|open.emit(chosen.clone()))}>{format!("{} {} {}",crate::links::icon(&link.kind),index+1,link.title)}</button><div class="link-destination">{&link.destination}</div></div>}
        })}</div>}}else{Html::default()}}
        {if let Some(link)=(*selected).clone() {html!{<crate::links::LinkPopup {link} item={props.item_id.clone()} revision={props.file_revision} onclose={close}/>}}else{Html::default()}}
    </div>}
}

#[function_component(Diagram)]
fn diagram(props: &Props) -> Html {
    let node = use_node_ref();
    {
        let node = node.clone();
        use_effect_with(props.source.clone(), move |source| {
            let cleanup = node
                .cast::<web_sys::Element>()
                .map(|el| mount_diagram(el, source));
            move || {
                if let Some(cleanup) = cleanup {
                    let _ = cleanup.call0(&JsValue::NULL);
                }
            }
        });
    }
    html! {<div class="mermaid-diagram"><div ref={node}/><details><summary>{"Diagram source"}</summary><pre><code>{props.source.clone()}</code></pre></details></div>}
}

fn element(name: &str, children: Vec<Html>) -> Html {
    let mut tag = VTag::new(name.to_owned());
    tag.add_children(children);
    tag.into()
}

// Normalize the common TeX delimiters outside inline/fenced code. Markdown's
// own parser remains responsible for code, escaped dollars and table structure.
fn math_delimiters(source: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut excluded = Vec::new();
    let mut code_start = None;
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => code_start = Some(range.start),
            Event::End(pulldown_cmark::TagEnd::CodeBlock) => {
                if let Some(start) = code_start.take() {
                    excluded.push(start..range.end);
                }
            }
            Event::Code(_) => excluded.push(range),
            _ => {}
        }
    }
    let mut output = String::new();
    let mut i = 0;
    let mut excluded_index = 0;
    while i < source.len() {
        while excluded
            .get(excluded_index)
            .is_some_and(|range| range.end <= i)
        {
            excluded_index += 1;
        }
        if let Some(range) = excluded
            .get(excluded_index)
            .filter(|r| r.start <= i && i < r.end)
        {
            output.push_str(&source[i..range.end]);
            i = range.end;
            continue;
        }
        let rest = &source[i..];
        if rest.starts_with("\\\\") {
            output.push_str("\\\\");
            i += 2;
            continue;
        }
        let pair = if rest.starts_with("\\(") {
            Some(("\\)", "$"))
        } else if rest.starts_with("\\[") {
            Some(("\\]", "$$"))
        } else {
            None
        };
        // A math opener cannot consume inline or fenced code while searching
        // for its closer, including a closer written inside a code example.
        let text_end = excluded
            .get(excluded_index)
            .map_or(source.len(), |range| range.start);
        if let Some((close, delimiter)) = pair
            && let Some(end) = source[i + 2..text_end].find(close)
        {
            output.push_str(delimiter);
            output.push_str(&rest[2..2 + end]);
            output.push_str(delimiter);
            i += end + 4;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        output.push(ch);
        i += ch.len_utf8();
    }
    output
}

#[cfg(test)]
fn render(source: &str) -> (Html, bool) {
    render_with_links(source, &Callback::noop())
}

fn render_with_links(source: &str, open: &Callback<demodex_protocol::MessageLink>) -> (Html, bool) {
    let normalized = math_delimiters(source);
    let mut events = Parser::new_ext(
        &normalized,
        Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_MATH,
    )
    .peekable();
    let mut changed = false;
    let links = demodex_protocol::links::extract(source);
    let nodes = nodes(&mut events, &mut changed, false, &[], 0, &links, open);
    (html! {<>{for nodes}</>}, changed)
}

fn nodes<'a>(
    events: &mut std::iter::Peekable<impl Iterator<Item = Event<'a>>>,
    changed: &mut bool,
    table_head: bool,
    table_alignments: &[Alignment],
    depth: usize,
    links: &[demodex_protocol::MessageLink],
    open: &Callback<demodex_protocol::MessageLink>,
) -> Vec<Html> {
    let mut result = Vec::new();
    let mut column = 0;
    while let Some(event) = events.next() {
        match event {
            Event::End(_) => break,
            Event::Start(tag) => {
                // Bound recursive browser rendering even for pathological Markdown.
                if depth > 128 {
                    let mut level = 1;
                    while level > 0 {
                        match events.next() {
                            Some(Event::Start(_)) => level += 1,
                            Some(Event::End(_)) => level -= 1,
                            Some(Event::Text(t) | Event::Code(t)) => {
                                result.push(html! {<>{t.to_string()}</>})
                            }
                            None => break,
                            _ => {}
                        }
                    }
                    continue;
                }
                if let Tag::CodeBlock(kind) = tag {
                    *changed = true;
                    let language = match kind {
                        CodeBlockKind::Fenced(s) => {
                            s.split_whitespace().next().unwrap_or("").to_owned()
                        }
                        _ => String::new(),
                    };
                    let mut code = String::new();
                    for event in events.by_ref() {
                        match event {
                            Event::End(_) => break,
                            Event::Text(t) | Event::Code(t) => code.push_str(&t),
                            _ => {}
                        }
                    }
                    result.push(if language == "mermaid" {
                        html! {<Diagram source={code}/>}
                    } else if language == "svg" {
                        html! {<crate::svg::SvgPreview source={code}/>}
                    } else {
                        html! {<pre class="code-block"><code>{code}</code></pre>}
                    });
                    continue;
                }
                if !matches!(tag, Tag::Paragraph) {
                    *changed = true;
                }
                let children = nodes(
                    events,
                    changed,
                    table_head || matches!(tag, Tag::TableHead),
                    if let Tag::Table(alignments) = &tag {
                        alignments
                    } else {
                        table_alignments
                    },
                    depth + 1,
                    links, open,
                );
                let child = match tag {
                    Tag::Paragraph => element("p", children),
                    Tag::Heading { level, .. } => element(&level.to_string(), children),
                    Tag::BlockQuote(_) => element("blockquote", children),
                    Tag::List(None) => element("ul", children),
                    Tag::List(Some(start)) => {
                        let mut el = VTag::new("ol");
                        el.add_attribute("start", start.to_string());
                        el.add_children(children);
                        el.into()
                    }
                    Tag::Item => element("li", children),
                    Tag::Emphasis => element("em", children),
                    Tag::Strong => element("strong", children),
                    Tag::Strikethrough => element("del", children),
                    Tag::Table(_) => {
                        let mut children = children.into_iter();
                        let head = children.next().unwrap_or_default();
                        html! {<div class="table-scroll"><table>{head}<tbody>{for children}</tbody></table></div>}
                    }
                    Tag::TableHead => element("thead", vec![element("tr", children)]),
                    Tag::TableRow => element("tr", children),
                    Tag::TableCell => {
                        let mut cell = VTag::new(if table_head { "th" } else { "td" });
                        let class = match table_alignments.get(column) {
                            Some(Alignment::Center) => "align-center",
                            Some(Alignment::Right) => "align-right",
                            _ => "align-left",
                        };
                        column += 1;
                        cell.add_attribute("class", class);
                        cell.add_children(children);
                        cell.into()
                    }
                    // Content cannot supply navigation targets or fetched resources.
                    // Original destinations remain available only as inert raw text.
                    Tag::Link { dest_url, .. } => {
                        if let Some((index, link)) = links.iter().enumerate().find(|(_,link)| link.destination == dest_url.as_ref()) {
                            let chosen=link.clone(); let open=open.clone();
                            html!{<button type="button" class="message-link markdown-link" onclick={Callback::from(move |_|open.emit(chosen.clone()))}><span aria-hidden="true">{format!("{} {} ",crate::links::icon(&link.kind),index+1)}</span>{for children}</button>}
                        } else { html!{<span class="markdown-link">{for children}</span>} }
                    },
                    Tag::Image { .. } => {
                        html! {<span class="markdown-image">{"[Image omitted: "}{for children}{"]"}</span>}
                    }
                    _ => html! {<>{for children}</>},
                };
                result.push(child);
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                result.push(html! {<>{text.to_string()}</>})
            }
            Event::Code(code) => {
                *changed = true;
                result.push(html! {<code>{code.to_string()}</code>});
            }
            Event::InlineMath(source) => {
                *changed = true;
                result.push(math(&source, false));
            }
            Event::DisplayMath(source) => {
                *changed = true;
                result.push(math(&source, true));
            }
            Event::HardBreak => {
                *changed = true;
                result.push(html! {<br/>});
            }
            Event::SoftBreak => result.push(html! {"\n"}),
            Event::Rule => {
                *changed = true;
                result.push(html! {<hr/>});
            }
            Event::TaskListMarker(checked) => {
                *changed = true;
                result.push(html!{<input type="checkbox" checked={checked} disabled=true aria-label={if checked{"Completed"}else{"Not completed"}}/>});
            }
            Event::FootnoteReference(label) => result.push(html! {<>{label.to_string()}</>}),
        }
    }
    result
}

fn math(source: &str, display: bool) -> Html {
    let rendered = (|| {
        if source.len() > 16_384 || source.bytes().filter(|b| *b == b'\\').count() > 512 {
            return None;
        }
        let mut nesting = 0usize;
        for ch in source.chars() {
            if ch == '{' {
                nesting += 1;
                if nesting > 64 {
                    return None;
                }
            } else if ch == '}' {
                nesting = nesting.saturating_sub(1);
            }
        }

        let converter = math_core::LatexToMathML::new(math_core::MathCoreConfig::default()).ok()?;
        let math = converter
            .convert_with_local_state(
                source,
                if display {
                    math_core::MathDisplay::Block
                } else {
                    math_core::MathDisplay::Inline
                },
            )
            .ok()?;
        let doc = roxmltree::Document::parse(&math.mathml).ok()?;
        math_node(doc.root_element(), 0)
    })();
    html! {<span class={if display{"math-display"}else{"math-inline"}} title={source.to_owned()}>{rendered.unwrap_or_else(||html!{<code class="math-fallback">{if display{format!("$${source}$$")}else{format!("${source}$")}}</code>})}</span>}
}

fn math_node(node: roxmltree::Node<'_, '_>, depth: usize) -> Option<Html> {
    if depth > 128 {
        return None;
    }
    if node.is_text() {
        return Some(html! {<>{node.text().unwrap_or("").to_owned()}</>});
    }
    let name = node.tag_name().name();
    if !matches!(
        name,
        "math"
            | "mrow"
            | "mi"
            | "mn"
            | "mo"
            | "mtext"
            | "mspace"
            | "mfrac"
            | "msqrt"
            | "mroot"
            | "msub"
            | "msup"
            | "msubsup"
            | "munder"
            | "mover"
            | "munderover"
            | "mtable"
            | "mtr"
            | "mtd"
            | "mstyle"
            | "mpadded"
            | "mphantom"
            | "menclose"
            | "mmultiscripts"
            | "mprescripts"
            | "none"
            | "semantics"
            | "annotation"
    ) {
        return None;
    }
    let mut tag = VTag::new(name.to_owned());
    for attr in node.attributes() {
        let key = match attr.name() {
            "display" => "display",
            "mathvariant" => "mathvariant",
            "stretchy" => "stretchy",
            "fence" => "fence",
            "separator" => "separator",
            "symmetric" => "symmetric",
            "largeop" => "largeop",
            "movablelimits" => "movablelimits",
            "accent" => "accent",
            "accentunder" => "accentunder",
            "columnalign" => "columnalign",
            "rowalign" => "rowalign",
            "columnspacing" => "columnspacing",
            "rowspacing" => "rowspacing",
            "columnspan" => "columnspan",
            "rowspan" => "rowspan",
            "linethickness" => "linethickness",
            "width" => "width",
            "height" => "height",
            "depth" => "depth",
            "lspace" => "lspace",
            "rspace" => "rspace",
            "minsize" => "minsize",
            "maxsize" => "maxsize",
            "displaystyle" => "displaystyle",
            "scriptlevel" => "scriptlevel",
            "notation" => "notation",
            "encoding" => "encoding",
            _ => continue,
        };
        tag.add_attribute(key, attr.value().to_owned());
    }
    for child in node.children().filter(|c| c.is_element() || c.is_text()) {
        tag.add_child(math_node(child, depth + 1)?);
    }
    Some(tag.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn assert_no_content_urls(node: &Html) {
        match node {
            yew::virtual_dom::VNode::VTag(tag) => {
                assert!(!matches!(
                    tag.tag(),
                    "a" | "img" | "script" | "iframe" | "link" | "image"
                ));
                assert!(tag.attributes.iter().all(|(key, _)| !matches!(
                    key,
                    "href" | "src" | "srcset" | "style" | "action"
                )));
                if let Some(children) = tag.children() {
                    assert_no_content_urls(children);
                }
            }
            yew::virtual_dom::VNode::VList(list) => {
                for child in list.iter() {
                    assert_no_content_urls(child);
                }
            }
            _ => {}
        }
    }
    #[test]
    fn all_links_and_images_are_inert_regardless_of_destination() {
        let source = "[Web](https://example.org) [Mail](mailto:person@example.org) [Local](/path) <https://example.org> ![Picture](https://example.org/a.png) <img src=\"https://example.org/raw.png\">";
        let (tree, changed) = render(source);
        assert!(changed);
        assert_no_content_urls(&tree);
    }
    #[test]
    fn delimiters_leave_code_and_unfinished_math_intact() {
        assert_eq!(
            math_delimiters(r"\(x^2\) and `\(raw\)`"),
            r"$x^2$ and `\(raw\)`"
        );
        assert_eq!(
            math_delimiters("```tex\n\\[x\\]\n```"),
            "```tex\n\\[x\\]\n```"
        );
        assert_eq!(math_delimiters(r"\(unfinished"), r"\(unfinished");
        assert_eq!(math_delimiters(r"\(unfinished `\)`"), r"\(unfinished `\)`");
        assert_eq!(
            math_delimiters("\\(unfinished\n\n```tex\n\\)\n```"),
            "\\(unfinished\n\n```tex\n\\)\n```"
        );
    }
    #[test]
    fn formatting_is_only_offered_for_transformed_messages() {
        assert!(!render("Plain text\nwith two lines").1);
        for text in [
            "**bold**",
            "| a | b |\n| --- | --- |\n| 1 | 2 |",
            "$x^2$",
            "```mermaid\ngraph LR\nA-->B\n```",
        ] {
            assert!(render(text).1);
        }
    }
    #[test]
    fn math_tree_rejects_active_content() {
        let doc = roxmltree::Document::parse("<math><script>bad</script></math>").unwrap();
        assert!(math_node(doc.root_element(), 0).is_none());
        let doc =
            roxmltree::Document::parse("<math><mfrac><mi>x</mi><mn>2</mn></mfrac></math>").unwrap();
        assert!(math_node(doc.root_element(), 0).is_some());
        let doc = roxmltree::Document::parse(r#"<math href="https://example.org"><mi style="background:url(https://example.org/math)">x</mi></math>"#).unwrap();
        assert_no_content_urls(&math_node(doc.root_element(), 0).unwrap());
    }
}
