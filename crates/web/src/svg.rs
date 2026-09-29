//! Local-only SVG subset: geometry, groups and text; no URL-bearing attributes,
//! CSS, definitions/references, animation, HTML, or external resources. Unsupported
//! input fails as a whole and remains available as exact source.
use crate::modal::Modal;
use yew::{prelude::*, virtual_dom::VTag};

const MAX_BYTES: usize = 128 * 1024;
const MAX_NODES: usize = 2048;
const MAX_DEPTH: usize = 32;
const MAX_ATTRIBUTE: usize = 16 * 1024;
const ATTRIBUTE_NAMES: &[&str] = &[
    "x",
    "y",
    "x1",
    "y1",
    "x2",
    "y2",
    "cx",
    "cy",
    "dx",
    "dy",
    "width",
    "height",
    "r",
    "rx",
    "ry",
    "stroke-width",
    "stroke-miterlimit",
    "font-size",
    "textLength",
    "opacity",
    "fill-opacity",
    "stroke-opacity",
    "fill",
    "stroke",
    "transform",
    "points",
    "d",
    "stroke-linecap",
    "stroke-linejoin",
    "fill-rule",
    "clip-rule",
    "stroke-dasharray",
    "stroke-dashoffset",
    "text-anchor",
    "dominant-baseline",
    "font-family",
    "font-weight",
    "font-style",
    "lengthAdjust",
    "viewBox",
    "preserveAspectRatio",
];
const SVG_NS: &str = "http://www.w3.org/2000/svg";

type SvgResult<T> = Result<T, String>;

fn numbers(value: &str) -> Option<Vec<f64>> {
    let result: Vec<_> = value
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
        .map(str::parse::<f64>)
        .collect::<Result<_, _>>()
        .ok()?;
    (!result.is_empty()
        && result
            .iter()
            .all(|n| n.is_finite() && n.abs() <= 1_000_000.0))
    .then_some(result)
}
fn number(value: &str) -> Option<f64> {
    let n = numbers(value)?;
    (n.len() == 1).then_some(n[0])
}
fn color(value: &str) -> bool {
    if let Some(hex) = value.strip_prefix('#') {
        return matches!(hex.len(), 3 | 4 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    matches!(
        value,
        "none"
            | "currentColor"
            | "transparent"
            | "black"
            | "white"
            | "gray"
            | "grey"
            | "red"
            | "green"
            | "blue"
            | "yellow"
            | "orange"
            | "purple"
            | "pink"
            | "cyan"
            | "magenta"
            | "teal"
            | "navy"
            | "lime"
            | "silver"
            | "maroon"
            | "olive"
            | "aqua"
            | "fuchsia"
    )
}
fn transform(mut value: &str) -> bool {
    let mut count = 0;
    while !value.trim().is_empty() {
        value = value.trim_start();
        let Some((name, rest)) = value.split_once('(') else {
            return false;
        };
        let Some((args, rest)) = rest.split_once(')') else {
            return false;
        };
        let Some(args) = numbers(args) else {
            return false;
        };
        if !match name.trim() {
            "matrix" => args.len() == 6,
            "translate" | "scale" => matches!(args.len(), 1 | 2),
            "rotate" => matches!(args.len(), 1 | 3),
            "skewX" | "skewY" => args.len() == 1,
            _ => false,
        } {
            return false;
        }
        count += 1;
        if count > 16 {
            return false;
        }
        value = rest.trim_start().strip_prefix(',').unwrap_or(rest);
    }
    count > 0
}
fn path(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() || c == b',' || b"MmZzLlHhVvCcSsQqTtAa".contains(&c) {
            i += 1;
            continue;
        }
        let start = i;
        if matches!(c, b'+' | b'-') {
            i += 1;
        }
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i < bytes.len() && matches!(bytes[i], b'e' | b'E') {
            i += 1;
            if i < bytes.len() && matches!(bytes[i], b'+' | b'-') {
                i += 1;
            }
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i == start || number(&value[start..i]).is_none() {
            return false;
        }
    }
    !value.is_empty()
}
fn attribute(name: &str, value: &str) -> bool {
    if value.len() > MAX_ATTRIBUTE {
        return false;
    }
    match name {
        "x" | "y" | "x1" | "y1" | "x2" | "y2" | "cx" | "cy" | "dx" | "dy" => {
            number(value).is_some()
        }
        "width" | "height" | "r" | "rx" | "ry" | "stroke-width" | "stroke-miterlimit"
        | "font-size" | "textLength" => number(value).is_some_and(|n| n >= 0.0),
        "opacity" | "fill-opacity" | "stroke-opacity" => {
            number(value).is_some_and(|n| (0.0..=1.0).contains(&n))
        }
        "fill" | "stroke" => color(value),
        "transform" => transform(value),
        "points" => numbers(value).is_some_and(|n| n.len() >= 4 && n.len() % 2 == 0),
        // Only geometry command letters and bounded finite numbers reach the DOM.
        "d" => path(value),
        "stroke-linecap" => matches!(value, "butt" | "round" | "square"),
        "stroke-linejoin" => matches!(value, "miter" | "round" | "bevel"),
        "fill-rule" | "clip-rule" => matches!(value, "nonzero" | "evenodd"),
        "stroke-dasharray" => {
            value == "none" || numbers(value).is_some_and(|n| n.iter().all(|v| *v >= 0.0))
        }
        "stroke-dashoffset" => number(value).is_some(),
        "text-anchor" => matches!(value, "start" | "middle" | "end"),
        "dominant-baseline" => matches!(
            value,
            "auto"
                | "middle"
                | "central"
                | "hanging"
                | "alphabetic"
                | "text-before-edge"
                | "text-after-edge"
        ),
        "font-family" => matches!(value, "sans-serif" | "serif" | "monospace"),
        "font-weight" => matches!(
            value,
            "normal"
                | "bold"
                | "100"
                | "200"
                | "300"
                | "400"
                | "500"
                | "600"
                | "700"
                | "800"
                | "900"
        ),
        "font-style" => matches!(value, "normal" | "italic" | "oblique"),
        "lengthAdjust" => matches!(value, "spacing" | "spacingAndGlyphs"),
        "preserveAspectRatio" => {
            let words: Vec<_> = value.split_ascii_whitespace().collect();
            matches!(words.as_slice(), ["none"])
                || words.len() <= 2
                    && words.first().is_some_and(|v| {
                        matches!(
                            *v,
                            "xMinYMin"
                                | "xMidYMin"
                                | "xMaxYMin"
                                | "xMinYMid"
                                | "xMidYMid"
                                | "xMaxYMid"
                                | "xMinYMax"
                                | "xMidYMax"
                                | "xMaxYMax"
                        )
                    })
                    && words.get(1).is_none_or(|v| matches!(*v, "meet" | "slice"))
        }
        "viewBox" => numbers(value).is_some_and(|n| n.len() == 4 && n[2] > 0.0 && n[3] > 0.0),
        _ => false,
    }
}

pub fn render(source: &str) -> SvgResult<Html> {
    if source.len() > MAX_BYTES {
        return Err("SVG exceeds the 128 KiB preview limit".into());
    }
    let doc = roxmltree::Document::parse_with_options(
        source,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_NODES as u32,
            ..Default::default()
        },
    )
    .map_err(|_| "SVG is invalid or exceeds the 2,048-node limit".to_owned())?;
    if doc.descendants().any(|n| n.is_pi()) {
        return Err("SVG processing instructions are unsupported".into());
    }
    let root = doc.root_element();
    if root.tag_name().name() != "svg" {
        return Err("Expected an SVG root element".into());
    }
    let view_box = match root.attribute("viewBox") {
        Some(v) if attribute("viewBox", v) => v.to_owned(),
        Some(_) => return Err("Invalid SVG viewBox".into()),
        None => {
            let dimensions = root
                .attribute("width")
                .and_then(number)
                .zip(root.attribute("height").and_then(number));
            let Some((w, h)) = dimensions.filter(|(w, h)| *w > 0.0 && *h > 0.0) else {
                return Err("SVG needs a viewBox or positive numeric width and height".into());
            };
            format!("0 0 {w} {h}")
        }
    };
    fn node(n: roxmltree::Node<'_, '_>, depth: usize) -> SvgResult<Html> {
        if depth > MAX_DEPTH {
            return Err("SVG exceeds the 32-level nesting limit".into());
        }
        if n.is_text() {
            return Ok(Html::from(n.text().unwrap_or_default().to_owned()));
        }
        if n.is_comment() {
            return Ok(Html::default());
        }
        if !n.is_element() {
            return Err("Unsupported SVG content".into());
        }
        let name = n.tag_name().name();
        if n.tag_name().namespace().is_some_and(|ns| ns != SVG_NS)
            || !matches!(
                name,
                "svg"
                    | "g"
                    | "rect"
                    | "circle"
                    | "ellipse"
                    | "line"
                    | "polyline"
                    | "polygon"
                    | "path"
                    | "text"
                    | "tspan"
                    | "title"
                    | "desc"
            )
            || name == "svg" && depth != 0
        {
            return Err(format!("Unsupported SVG element: {name}"));
        }
        let mut tag = VTag::new(name.to_owned());
        for a in n.attributes() {
            if a.namespace().is_some() || !attribute(a.name(), a.value()) {
                return Err(format!("Unsupported SVG attribute: {}", a.name()));
            }
            let key = ATTRIBUTE_NAMES
                .iter()
                .copied()
                .find(|key| *key == a.name())
                .ok_or_else(|| format!("Unsupported SVG attribute: {}", a.name()))?;
            tag.add_attribute(key, a.value().to_owned());
        }
        for child in n.children() {
            tag.add_child(node(child, depth + 1)?);
        }
        Ok(tag.into())
    }
    let mut tree = node(root, 0)?;
    if let yew::virtual_dom::VNode::VTag(tag) = &mut tree {
        let tag = std::rc::Rc::make_mut(tag);
        tag.add_attribute("viewBox", view_box);
        tag.add_attribute("width", "100%");
        tag.add_attribute("height", "100%");
        if root.attribute("preserveAspectRatio").is_none() {
            tag.add_attribute("preserveAspectRatio", "xMidYMid meet");
        }
        tag.add_attribute("role", "img");
        tag.add_attribute("aria-label", "SVG preview");
    }
    Ok(tree)
}

#[derive(Properties, PartialEq)]
pub struct Props {
    pub source: AttrValue,
}

#[function_component(SvgPreview)]
pub fn svg_preview(props: &Props) -> Html {
    let expanded = use_state(|| false);
    let parsed = use_memo(props.source.clone(), |source| render(source));
    let show = {
        let expanded = expanded.clone();
        Callback::from(move |_| expanded.set(true))
    };
    let close = {
        let expanded = expanded.clone();
        Callback::from(move |_| expanded.set(false))
    };
    html! {
        <div class="svg-widget">
            {match &*parsed {
                Ok(tree) => html! {
                    <>
                        <button type="button" class="svg-thumbnail" onclick={show} aria-label="Enlarge SVG preview" title="Enlarge SVG preview">{tree.clone()}</button>
                        {if *expanded { html! {
                            <Modal title="SVG preview" onclose={close} icon_close=true dismiss_outside=true>
                                <div class="svg-expanded">{tree.clone()}</div>
                            </Modal>
                        }} else { Html::default() }}
                    </>
                },
                Err(error) => html! { <p class="muted">{format!("SVG preview unavailable: {error}.")}</p> },
            }}
            <details class="svg-source"><summary>{"SVG source"}</summary><pre><code>{props.source.clone()}</code></pre></details>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_local_geometry_and_text() {
        assert!(render(r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100"><title>Example</title><g transform="translate(10, 5) rotate(3)"><rect width="80" height="40" fill="#ff8800"/><circle cx="120" cy="30" r="20" stroke="white"/><path d="M 0 0 L 10 20 Z"/><text x="20" y="80" font-family="sans-serif">Hello &amp; welcome</text></g></svg>"##).is_ok());
        assert!(render(r#"<svg width="100" height="50"><line x1="0" x2="50" /></svg>"#).is_ok());
    }
    #[test]
    fn unsupported_features_retain_fallback() {
        for source in [
            r#"<svg viewBox="0 0 10 10"><defs/></svg>"#,
            r#"<svg viewBox="0 0 10 10" style="fill:red"/>"#,
            r#"<svg viewBox="0 0 10 10"><image/></svg>"#,
            r#"<svg viewBox="0 0 10 10"><rect width="100%"/></svg>"#,
        ] {
            assert!(render(source).is_err());
        }
    }
    #[test]
    fn enforces_size_depth_nodes_and_finite_dimensions() {
        assert!(render(&" ".repeat(MAX_BYTES + 1)).is_err());
        assert!(
            render(&format!(
                "<svg viewBox=\"0 0 10 10\">{}{}</svg>",
                "<g>".repeat(34),
                "</g>".repeat(34)
            ))
            .is_err()
        );
        assert!(
            render(&format!(
                "<svg viewBox=\"0 0 10 10\">{}</svg>",
                "<g/>".repeat(MAX_NODES)
            ))
            .is_err()
        );
        for value in ["0 0 0 10", "0 0 NaN 10", "0 0 1e30 10"] {
            assert!(!attribute("viewBox", value));
        }
        assert!(path("M0-1.5L2e2,3z"));
        assert!(!path("M 0 0 L 1e30 10"));
        assert!(!path("M 0 0 L NaN 10"));
        assert!(!transform(&"scale(1) ".repeat(17)));
        assert!(!transform("scale(NaN)"));
        assert!(!transform("translate(1,2,3)"));
    }
}
